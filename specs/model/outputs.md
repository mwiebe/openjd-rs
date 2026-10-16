# Output Values (CONTROL_FLOW)

[README](README.md) · Output Values

The `openjd_model::outputs` module turns the `openjd_output_var` directives that one
Action prints into each Task's output values (RFC 0010 §Output emission). It is the
model's half of output collection: it parses a directive, checks it against the Step's
declarations, converts its JSON to a typed `ExprValue`, maps a `PATH` value back to the
submission path context, checks constraints, applies the collect mode, and counts the
size. It does no I/O, so a session runtime, the CLI, `openjd-for-js`, and a scheduler
that re-validates values can all share it.

The caller (the session runtime, in a later change) owns everything that touches the
outside world:

* recognizing a line that begins with `openjd_output_var:` in an Action's stdout or
  stderr, deciding whether the Action may print one (RFC invalid-directive item 8), and
  rejecting a line that is too long or not valid UTF-8 (items 7 and 9, for the line);
* stripping the line terminator and the `openjd_output_var:` prefix;
* opening and reading an `@` file under §File values (a regular file inside the Session
  Working Directory, read as the Action's user, at most
  `remaining_bytes_for(..) + 1` bytes);
* canceling the Action as soon as a directive is invalid, and reporting the error.

Source: `crates/openjd-model/src/outputs/` (`mod.rs`, `directive.rs`, `convert.rs`,
`collector.rs`). Tests: `tests/integration/test_control_flow_output_directive.rs` and
`test_control_flow_output_collector.rs`.

## Directive grammar

`parse_output_directive(payload)` parses the text after `openjd_output_var:`:

```bnf
<Directive>    ::= <WS>* <Identifier> <TaskSelector>? "=" <WS>* <Value> <WS>*
<TaskSelector> ::= "[" <Int> "]"
<Value>        ::= <JSON> | "@" <FilePath>
<WS>           ::= " " | "\t"
```

* Leading spaces and tabs are skipped, so `openjd_output_var:N=1` and
  `openjd_output_var: N=1` are the same directive.
* `<Identifier>` is `[A-Za-z_][A-Za-z0-9_]*` (ASCII). The parser does not limit its
  length; a name the Step does not declare fails in the collector.
* `<TaskSelector>` is `[`, an optional `-`, one or more ASCII digits, `]`, with no
  whitespace. It is parsed as an `i64`, so `[012]` is 12 and `[-0]` is 0. A selector
  outside the `i64` range is a grammar error: it cannot name a `CHUNK[INT]` value.
* `=` follows the name or selector directly. Whitespace after the name or selector is an
  error with its own message (`whitespace is not allowed after 'N'`, `... after 'N[1]'`),
  and any other character is reported as found (`expected '=' after 'N', found 'é'`).
  Spaces and tabs after `=` are skipped (`N= 1`, `N= @f`).
* A value that begins with `@` is a file path: the rest of the line, with trailing spaces
  and tabs removed (leading ones are kept, since they follow the `@`). It must not be
  empty. Anything else is the JSON text with surrounding spaces and tabs removed; it must
  not be empty, and it is not parsed here.

The result is `OutputDirective::Inline { name, task, json }` or
`OutputDirective::File { name, task, path }`, borrowing from the payload. Grammar errors
have kind `OutputErrorKind::Grammar` and the message
`Invalid openjd_output_var directive: <problem>, in '<payload>'`.

## The collector

`OutputCollector::new(declared, max_bytes, path_rules, worker_format, chunk)` collects
the directives of one Action:

| Argument | Meaning |
|---|---|
| `declared` | The Step's `job::Step::output_variables` (`step.output_variables.as_deref().unwrap_or_default()`). A repeated name keeps its first declaration (validation rejects repeats). |
| `max_bytes` | The per-Task size limit. RFC 0010 §Limits item 2 requires at least 1 MiB, `MIN_OUTPUT_BYTES_LIMIT`; the collector takes any value, so tests can use small ones. |
| `path_rules` | The Session's full path mapping rules, in the order it applies them (`Session::path_mapping_rules()`), unfiltered, as `unapply_rules` requires. |
| `worker_format` | The worker host's path format: the format a reported `PATH` value is parsed in. |
| `chunk` | The values of the Step's `CHUNK[INT]` parameter in the chunk being run, in iteration order; `None` for a Step without one. |

`new` returns `Result`, where the plan's signature returned `Self`, for two refusals:

* A chunk that repeats a value (`OutputErrorKind::Selector`). The RFC requires distinct
  values in a Step with outputs (§Chunked Steps item 1), so that a value names one Task;
  binding rejects duplicates, and the collector refuses them rather than merging two
  Tasks.
* A declaration with `collect: APPEND` on a type that is not a list
  (`OutputErrorKind::Declaration`). Template validation rejects it, but
  `job::OutputVariable`'s fields are public, and the collector needs an element type.

A runtime calls, for each directive:

1. `check_name(name, task)`: RFC §Processing a directive steps 1–2 (below), before an `@`
   file is opened.
2. For an `@` directive, `remaining_bytes_for(name, task)`, to bound the read.
3. `accept(name, task, bytes, from_file)`: steps 4–6. It repeats `check_name` first, so it
   is safe on its own. It returns the accepted value for the runtime to report as it
   arrives: the value under `ONCE` and `LAST`, the new element under `APPEND`.

and when the Action exits successfully, `finish()`. Every method that fails leaves the
collector's state as it was; the runtime fails the Action anyway.

Each Task (each value of `chunk`, or the single Task `None`) has its own values,
collect-mode state, and size, as §Chunked Steps item 2 requires.

### Name and selector checks

In order:

1. The name must be declared (`Undeclared`), matched case-sensitively. The message lists
   the declared names.
2. In a chunked Step a selector is required, in any other Step it is forbidden, and it
   must name a value of `chunk` (`Selector`). The check is against the chunk the runtime
   was given, not the parameter's whole range. The message lists up to 10 chunk values.
3. A `collect: ONCE` output must not have a value yet for that Task (`Repeated`).

### Converting a value

For `from_file`, the bytes are the `@` file's whole content: a UTF-8 byte order mark is
removed, the rest must be valid UTF-8 and contain no NUL (`FileContent`), and it may span
lines. An inline value's bytes are its JSON text; non-UTF-8 bytes there are a `Grammar`
error (`Invalid openjd_output_var directive: the value of 'N' is not valid UTF-8`), and
surrounding spaces and tabs are removed. The text must be exactly one JSON value
(`Json`); `serde_json` rejects `NaN`, `Infinity`, `-Infinity`, and trailing text. A number
too large to represent as a float (`1e400`) is valid JSON under RFC 8259 but, as the RFC's
type conversion rules say, does not convert. So validity is checked first without
converting numbers (`serde::de::IgnoredAny`), so `1e400 garbage` is a `Json` error for
its trailing text. Valid JSON can still fail to become a `serde_json::Value` in three
ways, each reported with its line and column: a number too large for a float
(`Conversion`, "has a number too large to represent as a float"); nesting of 128 levels
or more, `serde_json`'s recursion limit (`Conversion`, "nests deeper than any output
type": the deepest output type nests two levels); and a lone UTF-16 surrogate escape such
as `"\udc00"`, which RFC 8259 §8.2 allows in the grammar but which is not a Unicode
string (`Json`, with `serde_json`'s message).

Conversion (`Conversion` errors) follows §Processing a directive, "Type conversion":

1. `null` and objects are rejected at any depth, before anything else, with the position
   (`item[1][0]`).
2. Each JSON value gets its natural EXPR type: a number spelled without `.`, `e`, or `E`
   that fits in an `i64` is an `int`; any other number is a `float`; strings are
   `string`, `true`/`false` are `bool`.
3. The target type is the declared type, or under `APPEND` its element type. A list
   target requires an array, and each element is converted to the element type
   recursively, so the array is a list literal typed by its target (EXPR §1.2.6):
   `LIST[STRING]` accepts `[1, "a"]` as `["1", "a"]`. A scalar is converted with
   `ExprValue::coerce`, EXPR's implicit coercion. An array against a scalar target, and
   an array nested deeper than the target, fail there (`list[int]` does not coerce to
   `int`), which covers "nesting deeper than the target type".
4. The list is built with `ExprValue::make_list` and the element type as its hint, so an
   empty list has the declared type.

This deliberately does not reuse `json_to_expr_value` (which builds lists untyped, so
`[1, "a"]` fails) or `coerce_json_to_job_parameter_type` (whose job-parameter rules
accept `1` as a `BOOL`). The plan suggested moving `json_to_expr_value`'s scalar arms to
a shared module; they are not reused because the number arm must see the number's
spelling (below).

**Numbers keep their written digits.** `serde_json` is built without
`arbitrary_precision`, so a parsed number has lost its spelling. The collector recovers
each number's text from the JSON text, in document order (`convert::number_spellings`:
outside a string, `-` or a digit begins a number), and pairs it with the parsed values in
the same order. So a `float` keeps its spelling, as a `FLOAT` job parameter value does:
`X=1.50` is the float 1.5 displayed `1.50`, `S=1.50` is the string `"1.50"`, and
`S=12345678901234567890` is the string `"12345678901234567890"` (the float keeps the
digits). `-0` is the `int` 0. This answers plan ambiguity A6 for the prototype as "keep
the digits", without a workspace-wide `serde_json` feature.

**EXPR coercion, not job-parameter coercion** (plan ambiguity A7). `B=1` is invalid for
`BOOL` (`Cannot coerce int to bool`), while `B="yes"` is `true` and `N="12"` is 12, as
EXPR's string conversions allow.

### `PATH` values

A `PATH` value, each element of a `LIST[PATH]` value, and each `PATH` element under
`APPEND` is mapped back with
`openjd_expr::path_mapping::unapply_rules_with_source_format(path_rules, value,
worker_format)` (RFC §Path values; `specs/expr/path-mapping.md` § Reverse mapping). A
`Relative` or `NoRoundTrip` result is a `Path` error naming the path. The stored
`ExprValue::Path` gets the format the function returns: the matching rule's
`source_path_format`, `Uri` for a URI, or `worker_format` when there are no rules. A
`LIST[PATH]` value takes its first element's format (an `ExprValue::ListPath` has one
format; each element's text is kept as stored), or `worker_format` when empty. Elements
mapped back through rules of different source formats therefore share one format, and
an element taken out of the list has the list's format; forward mapping works on the
text and does not read the format, so this matters only to EXPR path operations on the
stored (`RawOutput`) value, which later PRs expose. Constraints, the size, and the
returned value all use the stored form. A `Path` error names where the path is
(`value`, `element`, `item[1]`).

### Constraints

Under `ONCE` and `LAST`, `job::OutputVariable::check_constraints` checks every declared
property against each reported value, so every `LAST` value must satisfy them, not only
the last. Under `APPEND`, `check_element` checks the new element against `item`, and
`check_length(len, false)` checks `maxLength` against the list's new length; `minLength`
is checked by `finish` (`check_length(len, true)`). Only declared properties apply
(`job-types.md` § CONTROL_FLOW fields).

A `RANGE_EXPR` value reported as a JSON string is checked in its written spelling, as a
`RANGE_EXPR` job parameter value is, and stored as the `ExprValue::RangeExpr`, whose
canonical spelling may differ: `"1-10:2"` is 6 characters and displays as `1-9:2`, and
`"3,1"` displays as `1,3`. This differs from the RFC table, whose `RANGE_EXPR` row shows
the result `1-10:2`; `ExprValue::RangeExpr` keeps no spelling, as for a `RANGE_EXPR` job
parameter, so the prototype records the divergence here rather than changing the
type.

### Size

The size counted for a value (§Limits item 2) is its reported bytes: the inline JSON text
without the spaces and tabs around it, or the whole content of its `@` file, byte order
mark and whitespace included. For a `PATH` or `LIST[PATH]` value, or a `PATH` element, it
is the larger of that and the length of the JSON serialization of its stored form. A
Task's size is the sum over what it holds: the `ONCE` value, the current `LAST` value (a
replaced value stops counting), and every `APPEND` element. A directive that would take
the Task over `max_bytes` is invalid (`Size`).

`remaining_bytes_for(name, task)` is `max_bytes` less the Task's size, not counting the
current value of a `LAST` output that the file's value would replace. A runtime reads at
most that many bytes plus one, and a file of more is over the limit. Because a `PATH`
value's size can grow with its stored form, `accept` checks the limit again.

### Collect modes and `finish`

| Mode | `accept` | `finish` |
|---|---|---|
| `ONCE` | Stores the value; a second one fails `check_name` | Fails (`Missing`) without a value |
| `LAST` | Replaces the value and its size | Fails (`Missing`) without a value |
| `APPEND` | Appends one element (a JSON array is one element of a `LIST[LIST[INT]]`) | The elements in arrival order, `[]` with none; fails if shorter than `minLength` |

`finish` returns `IndexMap<Option<i64>, TaskOutputs>`: one entry per Task, keyed by its
`CHUNK[INT]` value in the order of `chunk` (iteration order, whatever order the
directives arrived in), or by `None` for an unchunked Step. `TaskOutputs` is an
`IndexMap<String, ExprValue>` in declaration order. The first Task with a missing value
or a short `APPEND` list fails the whole call, so one Task fails the whole chunk
(§Chunked Steps item 3).

## Errors

`OutputError` carries an `OutputErrorKind` and a message; `Display` is the message.
Grammar errors begin `Invalid openjd_output_var directive:`. The two refusals of `new`
describe the chunk or the declaration. Every other message begins
`Output variable '<name>'`, or `Output variable '<name>[<task>]'` with a selector, and
names the problem and, where it has one, the position in the value (`item[1][0]`).

Messages quote an inline value's text and the converted value or element that is
invalid: a `PATH` with no submission-context form, a string not in `allowedValues`. The
RFC asks for "the offending text". They never quote an `@` file's text as such: a JSON
error for a file gives only `serde_json`'s line and column, and a conversion error only
the element's type. Quoted text is cut to 200 characters with `...`, and control
characters in it are escaped (`\n`, `\u{1b}`, `\0`), so a message stays on one line and
cannot drive a terminal; `job::OutputVariable`'s constraint messages quote strings the same
way. The EXPR coercion message a conversion error appends (which repeats the value) is cut
and escaped the same way, and so is an output name in a message, which only a hand-built
declaration can make unusual. A list with no type (an empty JSON array, which EXPR types
`list[nulltype]`) is described as an `empty list`.
Constraint messages come from `job::OutputVariable` and name the value as job parameter
constraint messages do. The plan said messages never quote an `@` file's content; quoting
the converted invalid value follows the RFC instead. The runtime adds the file path and
Action context.

| Kind | Cause |
|---|---|
| `Grammar` | The payload does not match the grammar, or an inline value is not UTF-8 |
| `Undeclared` | The Step does not declare the name |
| `Selector` | Selector missing, forbidden, or not in the chunk; or a chunk repeats a value |
| `Repeated` | A second `ONCE` value |
| `Json` | Not exactly one JSON value, or a string with a lone surrogate escape |
| `FileContent` | An `@` file is not UTF-8 or contains NUL |
| `Conversion` | `null`, an object, too deep (for the target, or 128 levels), a number too large for a float, or no conversion to the target type |
| `Path` | A relative `PATH` value, or one with no submission-context form |
| `Constraint` | A declared constraint property is violated |
| `Size` | The Task's values would exceed `max_bytes` |
| `Missing` | At exit, a `ONCE`/`LAST` value is missing or an `APPEND` list is too short |
| `Declaration` | `new` was given `collect: APPEND` on a non-list type |

## Python parity

openjd-model-for-python does not implement `CONTROL_FLOW` yet, so there is no Python
counterpart. The module is shaped (a pure parser and a collector with no I/O) so a Python
port can mirror it.
