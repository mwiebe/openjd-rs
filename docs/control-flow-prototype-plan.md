# CONTROL_FLOW prototype plan for openjd-rs

Status: PRs 1–7 are implemented on `control-flow-prototype` (at `74e5c98`). PR 7a and the loop PRs 8–11 are
planned. Target branch: `control-flow-prototype`.

This plan maps the draft `CONTROL_FLOW` extension onto the openjd-rs crates. Its inputs are on branch
`rfc-control-flow` of openjd-specifications, at commit `c3f583b`:

* three draft RFCs that together define the one extension `CONTROL_FLOW` (called "the RFCs" below):
  * `rfcs/0011-control-flow.md`, the core: the `Steps.*` scope, progressive step input binding, `enableIf`,
    reductions, and job outputs;
  * `rfcs/0012-step-output-variables.md`, the producer side: `outputVariables`, the `openjd_output_var` directive,
    collect modes, `PATH` values, limits, and retries and re-runs;
  * `rfcs/0013-step-loops.md`, multi-Step loops: the `<StepLoop>` entry, loop variables, `while` and `until`,
    `maxIterations`, the `Loop.*` scope, and loop outputs;
* `wiki/2023-09-Template-Schemas.md` (§3.8 for loops) and `wiki/How-Jobs-Are-Run.md` (its Step Loops section);
* `docs/control-flow-investigation/04-design-space.md`, the v1 scope;
* `docs/control-flow-investigation/03-runtime-semantics.md`, the runtime notes. Its §8 maps emission onto the
  `action_filter.rs` stdout pipeline. An earlier RFC draft replaced that with a session-supplied output file (04 T1),
  and the current draft returns to a stdout directive, `openjd_output_var:`, with an `@<file>` form, `collect` modes,
  and metrics as output variables (04 §8, RFC 0012 Design Choices 1–3). The earlier `619d910` made step input
  binding progressive (RFC 0011 §Step input binding, RFC 0011 Design Choice 10; 04 §10), and `d19cd9f` renumbered the
  single RFC to 0011. The split into three RFCs, and RFC 0013, came after `d19cd9f`.

Every `§` reference names the RFC that holds the section, in the same sentence, as in "RFC 0012 §Processing a
directive". `EXPR §1.2.3` is a section of the Expression Language, and `Template Schemas §3.4.1.1.1` a section of the
template schemas wiki page.
[Citation map](#citation-map-from-the-single-rfc) lists where each section of the single RFC moved.

The goal is a prototype that runs the six sample templates of RFC 0011 and RFC 0012 (five Basic Examples of RFC 0011,
and RFC 0012's task metrics example), and the three loop examples of RFC 0013, end to end with `openjd run`. The
prototype also tests whether the RFCs can be implemented as written, and the questions it raises are listed in
[RFC ambiguities surfaced](#rfc-ambiguities-surfaced).

Every PR below goes through the `implement-change` skill. Each PR updates the crate specs in `specs/` and
`public-api.md` in the same change, as `AGENTS.md` requires.

## Summary

* **openjd-model** does most of the work. It gets the schema fields (including `collect` and `unit` on outputs), the
  extension gate, the `Steps.*` scope in template validation, an output directive parser and collector that sessions
  and schedulers can share, and a progressive binding API. With that API, `create_job` evaluates what it can of each
  Step and leaves the rest pending, and a caller advances a pending Step as upstream values arrive: its Tasks and its
  host requirements become known separately, the first error fails it, and it is bound when every group has
  evaluated. It also gets
  `jobOutputDefinitions` and a function that evaluates one job output, and `PATH` output types with the
  `RawOutput` and `TaskRawParam` symbols. For loops (RFC 0013) it gets the `<StepLoop>` schema and its validation,
  a `job::Loop` bound like a Step, and functions that start an iteration, bind its body Step instances with the
  existing binding core, and evaluate the iteration boundary ([Loops](#loops-rfc-0013)).
* **openjd-expr** needs no new types or functions in the language. `ExprValue::Unresolved` already makes a value
  "unresolved until run time", and `accessed_symbols` already finds the references. Its `path_mapping` module gains a
  reverse-mapping function for `PATH` outputs (E1). A per-segment static-type API for the `null` warning is deferred
  along with the warning (see A1).
* **openjd-sessions** recognizes `openjd_output_var:` in the action filter, reads `@` files as the job user as each
  directive arrives, feeds each value to the model's collector, reports it through the action callback, and returns
  the collected values in `ActionResult`, with `PATH` values mapped back to the submission path context. Its four
  silent line-truncation sites must start marking truncation. Loops need one small change: the Session maps a
  `PATH`-typed `RawLoop.<V>` value forward to `Loop.<V>`, as it does a `RawOutput` (L7).
* **openjd-cli** `openjd run` already runs Steps in topological order in one Session. It becomes a scheduler
  simulator: after each Step finishes it advances the binding of every pending Step, reports what each has become
  known, fails a Step as soon as one of its groups fails, skips disabled Steps and Environments, and keeps outputs in
  memory. It prints each Task's outputs with their units, and each job output as soon as it is evaluated.
  `openjd summary` reports what is known of each pending Step, with provisional Task counts, and lists the job
  outputs. For loops, `openjd run` runs iterations one after another and names each body Step instance
  `<Loop>[<N>].<Step>` (L8).
* **openjd-snapshots** needs no change. It is the job-attachments crate and has no dependency on the model. "Snapshot
  test" in this plan means a golden-file test of CLI output.

## Current state, for reference

Each of these was checked against the code at `be266ed`, before the prototype, or by running the release CLI.

| Fact | Where |
|---|---|
| Extensions are a closed enum; the CLI enables all of `ModelExtension::ALL` by default | `crates/openjd-model/src/types.rs:356`, `crates/openjd-cli/src/common.rs:168` |
| `CONTROL_FLOW` is rejected today: `Unsupported extension names: CONTROL_FLOW` | `openjd check` probe |
| `WRAP_ACTIONS` is the model for an extension that requires `EXPR` | `crates/openjd-model/src/template/validate_v2023_09/wrap_actions.rs:86` |
| Validation is a fixed sequence of passes 5–10 | `crates/openjd-model/src/template/validate_v2023_09/mod.rs:180` |
| Validation symbol tables seed `ExprValue::unresolved(T)` per scope | `format_strings.rs:40`, `:75`, `:112`, `:169` |
| `create_job` instantiates every Step eagerly, in template order | `crates/openjd-model/src/job/create_job/mod.rs:265` |
| `instantiate_step` evaluates step `let`, host requirements, and ranges, then serializes a filtered symbol table into `job::Step.resolved_symtab` | `crates/openjd-model/src/job/create_job/instantiate.rs:37` |
| Computed list ranges are capped at 1024 elements; `IntRangeExpr` is not | `crates/openjd-model/src/job/create_job/ranges.rs:336`, `:475`, `:556`; `validate_v2023_09/mod.rs:41` |
| An expression range that evaluates to `[]` is already accepted with `EXPR` alone (0 Tasks); an empty range associated with a non-empty one is already an error | CLI probe: `range: "{{Param.L}}"` with `L=[]` and `range: "{{[]}}"` run 0 chunks; `(A, B)` gives "all members must have the same number of values, got 0 and 2" |
| `"1-{{N}}"` with `N=0` is rejected: `Descending range 1-0 requires a negative step` | `crates/openjd-expr/src/range_expr.rs:829`, CLI probe |
| `CallerLimits::max_task_count` is a total over all Steps, checked once in `create_job` | `crates/openjd-model/src/job/create_job/mod.rs:287` |
| Forward path mapping lives in `openjd-expr` (`PathMappingRule::apply_with_format`, `apply_rules_with_format`); the Session sorts its rules by decreasing `source_path` length and re-maps `PATH` `Param.*` values in `build_symbol_table` | `crates/openjd-expr/src/path_mapping.rs:55`, `:209`; `crates/openjd-sessions/src/session.rs:681`, `:2481` |
| Per-Task iteration with chunking disabled already exists | `StepParameterSpaceIterator::new_with_chunk_override`, `crates/openjd-model/src/job/step_param_space.rs:1298` |
| JSON to typed job-parameter values, with element-wise coercion | `coerce_json_to_job_parameter_type`, `crates/openjd-model/src/job/create_job/parameters.rs:1325` |
| Job-parameter constraint checks (`minValue`, `maxLength`, `item`, ...) | `JobParameterDefinition::check_constraints`, `crates/openjd-model/src/template/parameters.rs:496` |
| Symbol references of a format string or expression | `FormatString::accessed_symbols` (`crates/openjd-expr/src/format_string.rs:578`), `ParsedExpression::accessed_symbols` (`crates/openjd-expr/src/eval/parse.rs:35`) |
| `run_task` takes a `job::StepScript`, not a `job::Step`, and returns `ActionResult { state, exit_code, stdout }` | `crates/openjd-sessions/src/session.rs:1758`, `crates/openjd-sessions/src/action.rs:74` |
| Per-action environment variables are computed in `run_task` | `crates/openjd-sessions/src/session.rs:1785`, `evaluate_env_vars` at `:2393` |
| Directives are recognized by `parse_directive` (prefix `openjd_`, then `<kind>: `) and turned into `ActionMessage`s by `process_line`, on the line-reader side; the Session applies them in `apply_message`. A malformed `openjd_env` sends `CancelMarkFailed` | `crates/openjd-sessions/src/action_filter.rs:45`, `crates/openjd-sessions/src/subprocess.rs:1219`, `crates/openjd-sessions/src/session.rs:2286` |
| A recognized directive line is logged by default; `echo_openjd_directives` (default `true`) hides it when `false` | `crates/openjd-sessions/src/action_filter.rs:211`, the `SessionConfig` doc comment at `crates/openjd-sessions/src/session.rs:141`, and the CLI's setting at `crates/openjd-cli/src/run/execution.rs:289` |
| Lines are silently truncated at 64 KiB in four places: `truncate_line` on the direct path, and in the helper `LineFramer::accumulate` (which drops the rest of an over-long line before it is decoded), `decode_and_truncate`, and `fit_out_payload` (which can also cut a shorter line that grows when escaped) | `crates/openjd-sessions/src/subprocess.rs:62`, `crates/openjd-sessions/src/helper/src/framer.rs:195`, `:89`, `:116` |
| Invalid UTF-8 on stdout is decoded with backslash escapes, not rejected | `decode_backslashreplace`, `crates/openjd-sessions/src/subprocess.rs:87` |
| Under cross-user execution the embedded helper runs the action as the job user and streams `Response::Out` lines; its commands are `Run`, `Cancel`, and `Shutdown` | `crates/openjd-sessions/src/helper/src/protocol.rs:49`, `:69` |
| `openjd run` runs every Step in topological order in one Session, and stops running Tasks after the first failure | `crates/openjd-cli/src/run/execution.rs:202`, `:341`, `:378` |
| No crate and no OpenJD spec has a validation-warning channel | `grep -ri warn` over `openjd-model/src` and `openjd-cli/src`, and `grep -i warning` over the 2023-09 schema and EXPR wiki pages, find nothing |
| `repr_sh` has no signature for nested lists (`list[list[T]]`); `repr_py`, `repr_json`, `string`, and `flatten` do | `openjd check` probes |

## openjd-model

### M1. Extension and schema

* Add `ModelExtension::ControlFlow` (`"CONTROL_FLOW"`) to the enum, `ALL`, `as_str`, and `FromStr`
  (`types.rs:356`). Because the CLI takes `ALL` as its default, this also turns it on in `openjd check`, `run`, and
  `summary`.
* `template::JobTemplate` gets `job_output_definitions: Option<Vec<JobOutputDefinition>>`. A `JobOutputDefinition`
  reuses the `OutputVariableDefinition` wrapper, minus `collect` and plus `value: FormatString`. `job::Job` gets
  `outputs: Option<Vec<job::JobOutput>>`, each a `job::OutputVariable`, the unevaluated `value`, and
  `evaluated: Option<JobOutputValue>`, where `enum JobOutputValue { Value(ExprValue), Null }` and `None` means
  pending, with the same serde defaults as the other new fields. `create_job` fills `evaluated` for the outputs it evaluates; callers fill the rest
  from `evaluate_job_output`.
* `template::StepTemplate` (`template/step.rs:233`) gets
  `enable_if: Option<BoolOrFormatString>` and `output_variables: Option<Vec<OutputVariableDefinition>>`.
  `template::Environment` (`template/environment.rs:17`) gets `enable_if`.
* `OutputVariableDefinition`: reuse the job-parameter machinery instead of a parallel set of structs. Deserialize the
  same discriminated union as `JobParameterDefinition`, reject `default`, `userInterface`, and (for `PATH` and
  `LIST[PATH]`) `dataFlow` in validation, keep `objectType` without checking it, and wrap the result in a newtype.
  `collect` (an `OutputCollectMode` enum, `ONCE` by default) and `unit` (an `Option<String>`) are not job-parameter
  fields, so the wrapper takes them out of the mapping before it
  deserializes the union, which would otherwise reject them as unknown keys. The wrapper exposes `name`,
  `job_param_type`, `collect`, `unit`, and `check_constraints`. Three cautions:
  * RFC 0012 says that only declared constraint properties apply (§`<OutputVariableDefinition>` item 6). Any default
    that a job-parameter check implies when a property is omitted must be turned off for outputs. Test that a
    `RANGE_EXPR` output longer than 1024 characters is accepted when it declares no `maxLength`.
  * Error paths must say `outputVariables[i]`, not `parameterDefinitions[i]`.
  * Under `collect: APPEND` the constraints split: `item` applies to each element as it arrives, `maxLength` to the
    growing list, and `minLength` only when the action exits. `check_constraints` on a whole list can't express that,
    so the wrapper also exposes `check_element` and `check_length(len, at_exit: bool)`.
* `job::Step` (`job/mod.rs:105`) gets `output_variables: Option<Vec<job::OutputVariable>>`,
  `binding: StepBinding`, and `path_inputs: Vec<job::PathInput>` (M4.5). `job::Step` derives `PartialEq`, `Hash`, `Serialize`, and `Deserialize`
  (`job/mod.rs:103`), and `JobParameterDefinition` implements none of the first three, so `job::OutputVariable`
  cannot hold the M1 wrapper. It is its own struct: a name, a `JobParameterType`, the `collect` mode, the `unit`, and
  an `OutputConstraints` value
  that holds the declared constraint properties in hashable, serializable form (`Float64` for floats).
  `JobParameterType` derives `Serialize` but not `Deserialize` (`types.rs:175`), so it needs `Deserialize` added,
  which is a public API change recorded in `public-api.md`. It has a
  `check_constraints(&ExprValue)` that applies the same rules as the job-parameter checks, and it is built from the
  template definition in `create_job`. `job::Environment` (`job/mod.rs:147`) gets
  `enabled: bool`. New fields use `skip_serializing_if` so that the serialized form of existing jobs doesn't change
  (`tests/integration/test_job_step_serde.rs`). They also need `#[serde(default)]`, with `Bound` as the default
  binding and `true` as the default for `enabled`. Without it, a job serialized before the change, or one whose
  fields were skipped, fails to deserialize with `missing field`.

### M2. Validation

Add a pass 11, `validate_v2023_09/control_flow.rs`, following the `wrap_actions.rs` pattern. It checks the static
rules of RFC 0011 §Validation and RFC 0012 §Validation ("At template validation"):

1. Without the extension, `outputVariables` and `enableIf` are errors at their own paths. A `Steps.*` reference
   already fails as an unknown symbol, because nothing seeds it.
2. With `CONTROL_FLOW`, `EXPR` is required (reuse `check_expr_prerequisite`). `CONTROL_FLOW` in an Environment Template
   is an error, and so is `enableIf` in one.
3. `outputVariables`: 1–32 elements, unique names, an allowed type, valid constraints, `collect: APPEND` only with a
   `LIST[...]` type, a `unit` of 1–32 characters with no Unicode Cc characters, and no `LIST[LIST[INT]]` on a Step
   with a `parameterSpace`. Outputs on a Step with a `CHUNK[INT]` parameter are allowed (M3), and a literal-list
   `CHUNK[INT]` range in such a Step must have distinct values; a computed one is checked when it is materialized
   (M4 item 6).

The scope rules go into pass 8 (`format_strings.rs`), because that pass already builds one symbol table per field
family:

* Add a helper `add_steps_scope(symtab, jt, step_index, ScopeKind)`. For each direct whole-Step dependency `P` of the
  Step whose name is an `<Identifier>`, it seeds `Steps.P.Output.<V>` and `Steps.P.TaskParam.<N>` as
  `ExprValue::unresolved(T)`, typed by the table in RFC 0011 §The `Steps.*` scope, and their `RawOutput` and
  `TaskRawParam` twins with the same types. A producer with `enableIf` gets `T?`, and a producer with a
  `parameterSpace` gets `list[T]`. `Output.<V>` and `TaskParam.<N>` whose type is or contains `path` are seeded only
  in host scopes; the `Raw` forms are seeded everywhere. Call the helper from the Step's `enableIf`, `let`,
  `hostRequirements`, `range`, step-environment, and script symbol tables. Never call it for the job name,
  `jobEnvironments`, `combination`, or Environment Templates.
* An unknown symbol under the `Steps.` prefix gets a specific message instead of EXPR's generic one. Scan
  `accessed_symbols()` for the `Steps.` prefix and compare against the template, with messages for each case: no such
  Step; a Step name that is not an `<Identifier>`; the Step is not a direct dependency ("add `dependsOn: P`"); a
  self-reference; no such output; no such Task parameter; a `PATH`-typed `Output` or `TaskParam` in a
  TEMPLATE-scope field ("use `RawOutput`" or "use `TaskRawParam`"). Because the
  check is a static scan, it also catches a reference inside a branch that is never evaluated.
* `jobOutputDefinitions`: 1–50 elements, unique names, types and constraints as for outputs, except that
  `LIST[LIST[INT]]` is allowed (there is no `parameterSpace` to nest it in). Each `value` is checked
  against a symbol table with `Param.*` (minus `PATH` types), `RawParam.*`, `Job.Name`, and the `Steps.*` symbols of
  every Step with an `<Identifier>` name (with `add_steps_scope` in a mode that skips the direct-dependency rule and
  seeds `PATH`-typed values only in their `Raw` forms), with the target type made optional.
* `enableIf` is validated with target type `bool`, not `bool?`. Its symbol table has no `let` names, no `Task.*`, and
  no `Session.*`.
* The step-graph cycle check (`structure.rs:1118`) is unchanged. Dependency names are already checked at
  `structure.rs:217`.

RFC 0011's validation warning for `null` in a discarding position (RFC 0011 §Modifications to the Expression Language,
and §Validation item 7) is deferred in the prototype. openjd-rs has no warning channel, and adding one means a public
API decision in `decode_job_template` and output from `openjd check` (ambiguity A1). The prototype records the gap in
`specs/model/validation.md`.

### M3. Output directive parser and collector

Add `openjd_model::outputs`. It does no I/O, so sessions, the CLI, `openjd-for-js`, and a scheduler that re-validates
can all share it.

```rust
/// The payload after `openjd_output_var:`.
pub enum OutputDirective<'a> {
    Inline { name: &'a str, task: Option<i64>, json: &'a str },
    File { name: &'a str, task: Option<i64>, path: &'a str },
}
pub fn parse_output_directive(payload: &str) -> Result<OutputDirective<'_>, OutputError>;

/// Collects the directives of one Action: one Task, or every Task of a chunk.
pub struct OutputCollector { /* declared outputs, values so far, retained size, limit */ }
impl OutputCollector {
    pub fn new(
        declared: &[job::OutputVariable],
        max_bytes: usize,
        path_rules: &[PathMappingRule],   // the Session's rules, for PATH values
        worker_format: PathFormat,
        chunk: Option<Vec<i64>>,          // the chunk's CHUNK[INT] values (copied), or None if unchunked
    ) -> Self;
    /// Steps 1-2 of RFC 0012 §Processing a directive: name declared, selector valid, ONCE not repeated for that Task.
    /// Called before an `@` file is read.
    pub fn check_name(&self, name: &str, task: Option<i64>) -> Result<(), OutputError>;
    /// Bytes an `@` file for `name` may have, not counting the LAST value it would replace.
    pub fn remaining_bytes_for(&self, name: &str, task: Option<i64>) -> usize;
    /// Steps 4-6: convert the JSON text (inline, or a file's content), check constraints, apply the collect mode,
    /// update the retained size. Returns the accepted value, for the caller to report.
    pub fn accept(&mut self, name: &str, task: Option<i64>, json: &[u8], from_file: bool)
        -> Result<ExprValue, OutputError>;
    /// At exit: ONCE and LAST outputs present, APPEND minLength met, for every Task of the chunk.
    /// Keyed by CHUNK[INT] value (`None` for an unchunked Step).
    pub fn finish(self) -> Result<IndexMap<Option<i64>, IndexMap<String, ExprValue>>, OutputError>;
}
```

* The grammar is RFC 0012 §The `openjd_output_var` directive: leading spaces or tabs, an `<Identifier>`, an optional
  `[<Int>]` task selector (an optional `-` then digits, parsed as `i64`, no whitespace), `=` with nothing before it,
  optional spaces or tabs, then either `@` and a non-empty path (trailing spaces and tabs removed) or one JSON value
  with optional trailing spaces or tabs. The caller strips the line terminator. File content may start with a BOM and
  may span lines, and must be valid UTF-8 without NUL; `accept` checks that when `from_file` is set.
* `NaN`, `Infinity`, and numbers that overflow `f64` are invalid. `serde_json` rejects all three without
  `arbitrary_precision`; pin each with a test.
* Collect modes: `ONCE` rejects a second value (in `check_name`); `LAST` replaces the value and subtracts the
  replaced value's size; `APPEND` converts against the element type and pushes. Every `LAST` value must pass the
  constraints, not only the last.
* Size accounting follows RFC 0012 §Limits item 2: for each retained value, the inline JSON's bytes without surrounding
  whitespace, or the whole content of its `@` file.
* Messages name the variable and the problem, and never quote an `@` file's content. They may quote an inline
  value, which the Action printed itself; RFC 0012 asks for the offending text.
* Type conversion: type each JSON value against the target type (the declared type, or the element type under
  `APPEND`), then coerce with EXPR's implicit coercion, `ExprValue::coerce` (`value.rs:904`), as RFC 0012 §Processing
  a directive (Type conversion) requires. It infers an array "as a list literal with the target type" (EXPR §1.2.6),
  so arrays are converted element by element toward the target element type. `json_to_expr_value`
  (`parameters.rs:1381`) can't be reused as is: it builds lists with a `NULLTYPE` hint (`parameters.rs:1406`), so the
  RFC 0012 row `LIST[STRING] L=[1, "a"]` fails with `Invalid list value: make_list expected int element, got string`.
  Reuse its scalar arms. It is `pub(super)`, so move it to a module that both `create_job` and `outputs` can use.
  Keeping a number's written spelling (A6) is not free: the workspace's `serde_json` is built without
  `arbitrary_precision`, so `1.50` is already `1.5` after parsing. If A6 is answered "keep the digits", either enable
  `arbitrary_precision` (a workspace-wide change, through feature unification) or lex the number tokens separately. Do
  not reuse `coerce_json_to_job_parameter_type` either: it follows job-parameter rules, which are more lenient (for
  example `1` is accepted as a `BOOL` at `parameters.rs:691`). Ambiguity A7 asks RFC 0012 which of the two it means.
  Before converting, reject `null` and objects at any depth, and nesting deeper than the target type. Each row of RFC
  0012's conversion table, including its three `APPEND` rows, becomes a unit test.
* `PATH` values: after conversion, `accept` maps each `PATH` value or `LIST[PATH]` element back with
  `unapply_rules` (E1); a `Relative` or `NoRoundTrip` error makes the directive invalid (RFC 0012 invalid-directive item
  10). The stored `ExprValue::Path` carries the matching rule's `source_path_format` (the value's own format when
  the Session has no rules). Constraints and the size accounting then use the stored form, and so does the value
  `accept` returns. For a
  `PATH` value the size counted is the larger of the reported bytes and the stored value's JSON serialization.
* Chunked Steps: with `chunk` set, every call must pass `Some(task)` naming a value in `chunk`, and with `chunk`
  `None`, `None`; anything else is invalid (RFC 0012 invalid-directive item 11). Values, collect-mode state, and the
  size limit are kept per Task. `finish` fails if any Task lacks a required value, which fails the whole chunk.
* Constraints: `job::OutputVariable::check_constraints`, `check_element`, and `check_length` from M1.

### M4. Job creation and binding

This is the core of the prototype. Step input binding is progressive (RFC 0011 §Step input binding): a Step's
materialization expressions fall into groups, *enableIf* is decided first, and each other group is evaluated as soon
as its own inputs are concrete. A `PENDING_INPUTS` Step therefore gains facts ("Tasks known", "host requirements
known") one at a time, without a new state for each. A `job::Step` records this in its binding:

```rust
pub enum StepBinding {
    /// Every group has evaluated: the Step is READY and its Tasks may be dispatched (today's only state).
    Bound,
    /// Not bound yet. What is known so far, per group.
    PendingInputs(PendingBinding),
    /// enableIf was false (at job creation, or later). The parameter space is empty: zero Tasks.
    Skipped,
}

pub struct PendingBinding {
    pub enablement: GroupState<()>,                               // Known(()) once enableIf is true or absent
    pub tasks: GroupState<Option<job::StepParameterSpace>>,       // Known(None): no parameterSpace, one Task
    pub host_requirements: GroupState<Option<job::HostRequirements>>,
    pub remaining: GroupState<()>,                                // Step Environment and script fields
}

pub enum GroupState<T> {
    /// Not evaluated. `on` is the group's own inputs that are not concrete yet (`Steps.*` symbols, sorted), not
    /// counting the enablement group's inputs, which RFC 0011 also counts as inputs of every other group; it is
    /// empty for a group that waits only for enableIf. Every call to `advance_step_binding` recomputes it against
    /// its `inputs`, also while enablement is still `Waiting`, so a report always shows what is still missing.
    Waiting { on: Vec<String> },
    /// Evaluated successfully, with its result.
    Known(T),
}
```

A pending Step keeps `parameter_space` and `host_requirements` as `None`, so a caller that reads those fields
today never sees a partial Step. The materialized parameter space and the evaluated host requirements wait in
`PendingBinding` and move into the Step's own fields when it is bound. Its script and Environments are kept as they
are, because they are carried forward as format strings anyway. `PendingBinding` and `GroupState` derive what
`job::Step` derives (M1); their payloads already do, because they are `job::Step` field types. The prototype has
no Task graph group: `TASK_DEPENDENCIES` is rejected as unsupported (see the scene-graph golden tests; the
verbatim sample fails earlier, on its unknown `dependsOnSubspace` field).

1. **Group inputs.** Each group's inputs come from static analysis, not from evaluating with `Unresolved` values. The
   RFC 0011's rule is about what a field *references*, and a reference inside a branch that is never taken still counts.
   For each group, take the union of `accessed_symbols()` over its fields (RFC 0011 §Materialization groups):
   * enablement: the Step's `enableIf`;
   * Task enumeration: its Task parameter `range`s and `chunks` fields;
   * host selection: its `hostRequirements` fields;
   * remaining: the TEMPLATE-scope fields of its step Environments (including their `enableIf`), and the `timeout`
     and `cancelation` fields of its script.

   Expand each step-`let` name a group reads to the symbols that binding reads, transitively. The remaining group
   also uses every step `let`, so its set includes the symbols of all of them. A group's inputs are the `Steps.P.*`
   symbols in its set. A Step is bound at Job creation if `enableIf` is decided `true` (or is absent) there and
   every group's inputs are concrete then; otherwise it is `Skipped` or `PendingInputs`:
   * `Steps.P.Output.*` is concrete at creation only when `P` is statically `Skipped`, as `null`. For example, with
     `EncodeVideo=false`, `Publish` in the render sample binds at creation.
   * `Steps.P.TaskParam.*` is concrete at creation when `P`'s Tasks are known (`P` is `Bound`, or `PendingInputs` with
     `tasks: Known`) or `P` is statically `Skipped`.

   `create_job` loops over Steps in template order (`create_job/mod.rs:265`), and a Step may appear before a Step it
   depends on (the existing `reverse_dependency_order.yaml` CLI test template). So, under `CONTROL_FLOW`, `create_job`
   needs to instantiate Steps in dependency order, building `StepInputs` from the Steps it has already instantiated, while it still stores
   the Steps in template order. `StepDependencyGraph` is built from a `job::Job`, so it can't supply that order before
   the Job exists. Instead, factor the Kahn's-algorithm pass out of `detect_dependency_cycles`
   (`validate_v2023_09/structure.rs:1074`) so that it returns the order of the template's Steps. As written, that pass
   counts in-degree on each *dependency* (`structure.rs:1091`–`:1097`), so it yields Steps that nothing depends on
   first, and it seeds its queue from `HashMap` iteration. The factored version must therefore reverse the edges
   and break ties by template index, so the order is deterministic. Errors that `instantiate_step` collects into
   `check_errors` then arrive in dependency order, so sort them back into template order before reporting, so that
   messages follow the template as they do without `CONTROL_FLOW`.
2. **Job creation.** `create_job` calls the same internal core as later binding (item 3) for each Step, with the
   inputs that are concrete at creation, so every group whose inputs are concrete is evaluated then (RFC 0011 §When
   groups are evaluated, a MUST):
   * An `enableIf` whose inputs are concrete at creation is evaluated: one that reads no `Steps.*`, or only symbols
     concrete then (item 1), such as the `null` outputs of a producer skipped at creation. Because Steps are
     instantiated in dependency order, such a producer is already decided. If it is false, the Step becomes `Skipped`
     and nothing else of it is evaluated. That includes the step `let` evaluation and carried-forward checks that `instantiate_step`
     runs today (`instantiate.rs:62`–`:149`), so a skipped Step's `let` may assume the Step runs. Environment
     `enableIf` in a Job Environment works the same way and sets `job::Environment.enabled`.
   * An `enableIf` whose inputs are not concrete at creation leaves the Step `PendingInputs` with
     `enablement: Waiting`, and no other group of it is evaluated at creation, even one whose inputs are all static.
   * Otherwise, every group whose inputs are concrete is evaluated. A failure in any of them fails `create_job`, as
     an invalid parameter does, whether the Step would then be bound or stay pending. So a pending Step's host
     requirements or range that read no `Steps.*` are checked at submission.
   * Error reporting keeps today's two kinds. A step `let` error or a carried-forward check error (the script and
     every Environment) is collected across Steps into `check_errors` (`create_job/mod.rs:265`–`:276`), so
     `violations_across_steps_and_environments_are_reported_together` keeps its six errors. A resolution error in
     host requirements or the parameter space returns at once through `?` (`instantiate.rs:159`–`:169`) and aborts
     `create_job` alone, as `resolution_failure_aborts_alone_ahead_of_check_failures` pins.
     * Without `CONTROL_FLOW`, the core keeps exactly today's behavior: Steps are instantiated in template order (no
       Step can read `Steps.*`, so dependency order is not needed; item 1), and a `let` failure is collected and that
       Step's carried-forward checks still run, so `failed_step_level_let_still_checks_later_steps`
       (`test_job_creation_resolved_values.rs:974`) keeps its three errors. A test with a bad host requirement in a
       Step that comes before its dependency, whose range is bad, pins that the first Step's error is reported.
     * Under `CONTROL_FLOW`, the core stops a Step at its first failing group. A `let` error is collected into
       `check_errors`, whichever group's evaluation raised it, and that Step's later groups are not evaluated; a
       failing range or host requirement aborts `create_job` with its error, as today; a failing remaining group adds
       its errors to `check_errors`. Each `BindingError` from Job creation maps onto one of these two kinds by the
       error it holds: a `let` error or a remaining-group check error is collected; an `enableIf` failure (including
       `null`), a range or host requirement error, a duplicate `CHUNK[INT]` value, and a Task-count limit abort
       `create_job`. `BindingError` is `{ group: MaterializationGroup, errors: Vec<ModelError> }`, where
       `MaterializationGroup` is `Enablement`, `TaskEnumeration`, `HostSelection`, or `Remaining`.
   * RFC 0011's group order (enablement, Task enumeration, host selection, remaining) applies only under
     `CONTROL_FLOW`. Without it, the core keeps today's order (the carried-forward checks at `instantiate.rs:62`–`:149`,
     then host requirements before the parameter space at `:159`–`:169`), so a template that doesn't list
     `CONTROL_FLOW` reports the same first error as today.
3. **Binding API.**

   ```rust
   pub fn advance_step_binding(
       job_template: &JobTemplate,
       job_parameter_values: &JobParameterValues,
       ctx: &ValidationContext,
       job: &job::Job,
       step_index: usize,
       inputs: &StepInputs,
       committed_task_count: u64,   // Tasks of other Steps whose Tasks are known, for max_task_count (item 7)
   ) -> Result<job::Step, BindingError>;   // PendingInputs (possibly with more known), Bound, or Skipped
   ```

   `advance_step_binding` evaluates every group of a `PendingInputs` Step that has not been evaluated and whose inputs
   are now concrete in `inputs`, in RFC 0011's order: enablement first, and nothing else until it is `true`; then Task
   enumeration (materialize the parameter space, check distinct `CHUNK[INT]` values (item 6) and the Task-count
   limits (item 7)); then host selection; then the remaining group (Step Environment `enableIf`, which sets
   `job::Environment.enabled`, and the carried-forward checks of the script and Environments). It stops at the first
   failing group and returns `Err`: the Step is `FAILED` at once, whatever its other groups are waiting for.
   `BindingError` names the group and holds that group's `ModelError`s, each naming the field and the expression. When every group is
   `Known`, it moves the parameter space and host requirements into the Step's fields, builds `resolved_symtab`, and
   returns a `Bound` Step. Called with no new inputs, it returns the Step unchanged, so a caller can call it after
   every change without tracking which groups are evaluable. Called on a `Bound` or `Skipped` Step, it returns the
   Step unchanged.

   It refactors `instantiate_step` (`instantiate.rs:37`) into an internal core that serves both `create_job`, which
   has no `job::Job` yet (item 1), and later binding, for which `advance_step_binding` is the public wrapper over a
   `job::Job`. The core takes the template-level inputs, the Step being built, and the `StepInputs`. The Step's
   symbol table is the job symbol table plus the concrete `Steps.*` symbols from `inputs`. The
   other inputs `instantiate_step` takes today (the job symbol table, `has_expr`, `EffectiveLimits`, `EvalBudgets`)
   are rebuilt from `ctx`, the parameter values, and `job.name`, as `create_job` builds them. Step `let` bindings
   are evaluated on demand, only the ones the group being evaluated uses, in declaration order. The prototype
   re-evaluates them in each call rather than caching their values in `PendingBinding`; that is observably the same
   as RFC 0011's "evaluated once", because a binding whose inputs are concrete always gives the same value, and its
   first failure fails the Step. Binding stays atomic for dispatch because the function returns a new Step and the
   caller swaps it in; a caller dispatches Tasks only of a `Bound` Step.

   Job creation runs its own format-string checks, separate from pass 8. `instantiate_step` re-checks every Step's
   script and Environments (`instantiate.rs:114`, `:140`) against tables from `build_task_check_symtab`
   (`instantiate.rs:277`) and `build_env_check_symtab` (`:359`). Those tables must seed each `Steps.*` symbol too, as
   concrete when it is known and unresolved otherwise. Without that, every script that reads `Steps.*` fails
   `create_job` with `Undefined variable`. Seed the `Steps.*` placeholders in both builders, for the Step's own
   direct dependencies. Do not put them in `add_unresolved_session_symbols` (`instantiate.rs:224`): it takes no Step,
   so it can't know which producers are visible. Do not put unresolved placeholders in the Step symbol table either,
   because that table becomes `resolved_symtab` through `filter_symtab_for_step`. Only concrete values go there,
   when the Step is bound. This seeding lands in PR 2, together with the validation rules, so that a template that
   passes `openjd check` also passes `create_job` (see PR and commit order).

   Two read-only helpers serve reporting (C1, C2) and schedulers: `job::Step::tasks_known() -> Option<u64>`, the Task
   count once Tasks are known (`Some(0)` for `Skipped`), and `job::Step::binding_report()`, which lists, for
   enablement, Tasks, host requirements, and the remaining group, whether each is known and what it waits for (the
   `Steps.*` symbols that are not concrete, or the undecided `enableIf`). RFC 0011 §Implementation strategy recommends
   the first three in a report; the remaining group's waiting set says why a Step whose Tasks and host requirements
   are known is still pending (C2).
4. **Assembling inputs.** Schedulers and the CLI should not have to reimplement the shape rules, so the model provides
   them:

   ```rust
   pub enum ProducerState<'a> {
       /// Tasks known (Bound, or PendingInputs with `tasks: Known`) but not finished:
       /// contributes TaskParam only (Output stays unresolved).
       TasksKnown,
       Succeeded { task_outputs: &'a [IndexMap<String, ExprValue>] }, // indexed by iteration order
       Skipped,
   }
   impl StepInputs {
       pub fn add_producer(&mut self, producer: &job::Step, state: ProducerState) -> Result<(), ModelError>;
   }
   ```

   `add_producer` makes `Output.<V>` a scalar when the producer has no parameter space, and a list in iteration order
   when it has one. It fills `TaskParam.<N>` from `StepParameterSpaceIterator::new_with_chunk_override(ps, Some(1))`,
   one element per Task even for a `CHUNK[INT]` parameter. That iterator yields a one-value `RangeExpr` for a
   `CHUNK[INT]` parameter, which `add_producer` converts to its `int`, as RFC 0011 types it. It fills `RawOutput` and
   `TaskRawParam` with the same values (the stored, submission-context form), and for a type that is or contains `path`
   leaves `Output` and `TaskParam` to be derived on the worker (item 5 and S4). A skipped producer sets every symbol to
   `null`, including `Output` and `TaskParam` of path types, so nothing is left to derive; M4.5 then copies those
   `null`s directly, and they are not added to `path_inputs`. A `TasksKnown` producer contributes `TaskParam` only,
   read from its own `parameter_space` when it is `Bound` and from `PendingBinding.tasks` when it is pending, so a
   consumer can know its Tasks before its producer is bound (RFC 0011 §The `Steps.*` scope, Concreteness). The list
   type hint follows the declared type, so `[]` becomes a typed empty list.
5. **Host-scope `Steps.*` in Steps bound at creation.** A Step such as `FramesToVideo` reads `Steps.*` only in its
   script, so it binds at job creation but needs the values before its Tasks run. Add
   `pub fn attach_step_inputs(step: &job::Step, inputs: &StepInputs) -> Result<job::Step, ModelError>`. It merges the
   concrete `Steps.*` values that the Step reads into `resolved_symtab`. It has to find those symbols itself, because
   `filter_symtab_for_step` (`instantiate.rs:909`) runs only inside `instantiate_step`, where the symbol table has no
   `Steps.*` entries to keep (`copy_symbol_value` skips a missing symbol, `format_string.rs:612`). Reuse
   `collect_all_accessed_symbols` (`instantiate.rs:1014`) and copy the matching values from `inputs`. For a
   path-typed `Steps.P.Output.X` or `TaskParam.N` that the Step reads, `inputs` holds only the `Raw` twin, so copy
   that instead, as `include_raw_param_fallbacks` (`instantiate.rs:993`) does for `PATH` `Param.*`. Every path-typed
   `Raw` value it stores, whether the Step reads it directly or through this fallback, is stored as a `string` (or
   list of strings) and recorded in `job::Step::path_inputs` (`PathInput { symbol: String, ty: openjd_expr::ExprType }`, where `ty`
   is `path`, `list[path]`, or `list[list[path]]` for a multi-Task producer's `LIST[PATH]` output), which S4 uses.
   Every bound Step that reads `Steps.*` in host-scope fields goes through `attach_step_inputs`, including one bound
   by `advance_step_binding`, which doesn't put path-typed `Steps.*` values into `resolved_symtab` itself. Expose
   the list of producers whose values a Step needs before dispatch as `job::Step::input_producers()`.
6. **Range rules under `CONTROL_FLOW`.**
   * An empty list from a format-string range is already accepted (see the table above). Add tests for it under
     `CONTROL_FLOW`. Whether it stays accepted *without* `CONTROL_FLOW` is ambiguity A2.
   * A list-literal range whose evaluated items flatten to nothing needs new code, in two parts. First, `INT` and
     `FLOAT` list literals must flatten list-valued items as EXPR §1.3.2 requires; today only `STRING` and `PATH` do
     (A14). Second, the empty result must be accepted. RFC 0011 allows a list literal to evaluate to empty (§Empty
     parameter spaces), but openjd-rs errors today, with `EXPR` alone: a `STRING` `range: ["{{Param.L}}"]` with
     `L=[]` fails with `Task parameter 'X' range has no elements after resolution` (`ranges.rs:528`). Under
     `CONTROL_FLOW`, accept the empty result for all four types.
   * Lift the 1024-element cap (`ranges.rs:336`, `:475`, `:556`) for format-string ranges when `CONTROL_FLOW` is
     enabled. List literals stay capped as written. This depends on how A3 is answered, so the cap is decided in one
     function that the answer can change.
   * In a Step that declares `outputVariables`, a `CHUNK[INT]` range must have distinct values once evaluated. Check
     it in the Task enumeration group of the shared core (M4.3), which runs at creation for a range whose inputs are
     concrete then (today `range: "{{[1,1,2]}}"` creates three Tasks) and in `advance_step_binding` otherwise. A
     duplicate fails Job creation or the Step, with a message naming the value.
   * Range strings are unchanged. `"1-0"` stays an error in openjd-rs until A4 is settled.
7. **Task-count limits.** Keep `max_task_count` as the job total. The limits are checked in the Task enumeration
   group, when a Step's Tasks become known (RFC 0011 §Limits item 1), not when it is bound. In `create_job`, count
   only Steps whose Tasks are known at creation. In `advance_step_binding`, `committed_task_count` carries the Tasks
   of the other Steps whose Tasks are already known, and the Task enumeration that would exceed the limit fails the
   Step at once, naming it. Also add an optional `CallerLimits::max_step_task_count`, the per-Step limit from RFC 0011
   §Limits item 1. See A5.
8. **`StepDependencyGraph`** (`job/step_dependency_graph.rs`) is unchanged. The step graph is fixed.
9. **Job outputs.** `create_job` evaluates every job output whose `value` reads no `Steps.*` symbol that is not
   concrete yet, including a `TaskParam` of a Step whose Tasks are known at creation though it is still pending, and
   fails Job creation if one fails, as an invalid parameter does. An evaluated job output stays committed even if a
   Step it reads later fails (RFC 0011 §Job outputs item 4). The rest are evaluated later
   through:

   ```rust
   pub fn job_output_inputs(job: &job::Job, output_index: usize) -> Vec<String>; // Steps.* symbols it waits on
   pub fn evaluate_job_output(
       job_template: &JobTemplate,
       job_parameter_values: &JobParameterValues,
       ctx: &ValidationContext,
       job: &job::Job,
       output_index: usize,
       inputs: &StepInputs,      // built with add_producer for every Step the value reads
   ) -> Result<Option<ExprValue>, ModelError>;   // Ok(None) for a null result; Err fails the Job
   ```

   It evaluates `value` with the job symbol table plus the `Steps.*` symbols from `inputs`, converts to the declared
   type, and checks the constraints (skipped for `null`). `StepInputs` here may hold producers that are not
   dependencies of anything, so `add_producer` doesn't check that. The CLI and a scheduler call
   `evaluate_job_output` whenever a symbol it waits on becomes concrete: when a Step it reads `Output` or
   `RawOutput` of succeeds or is skipped, and when a Step it reads `TaskParam` or `TaskRawParam` of has its Tasks
   become known or is skipped. Deciding when to call it is the caller's
   job, which keeps the scheduling policy (fail the Job, mark stale on re-run) out of the model.

### M5. Retries and re-runs

`openjd run` has no retries or re-runs: a failed Task fails the run, and no Task runs twice. So the prototype
implements none of the re-run rules, and the model API needs nothing for them. The rules constrain a scheduler, and
the API leaves room for one:

* RFC 0012 §Retries and re-runs opens with a general rule: a re-run of a Task that has already succeeded leaves the
  Task `SUCCEEDED` for every purpose while the re-run is pending or running and after it fails or is rejected: for
  `dependsOn`, for completeness (RFC 0011 §Reductions item 3), for Step and Job success, and for loop iteration
  completion. Its rule 4 says such a failed or rejected re-run commits nothing, and the Task keeps its earlier
  succeeded state and all of its committed values. RFC 0011 §Step states adds that a re-run never moves a
  `SUCCEEDED` Step out of `SUCCEEDED`. In the model, a Step's state and its Tasks' values are the caller's records
  (`StepInputs` is built from them), so a scheduler meets these rules by keeping its records unchanged until a re-run
  commits.
* Rules 1–3 depend on whether an output has been **consumed**: a consumer that references it has evaluated a group
  whose inputs include it, or has started a Task. `job::Step::input_symbols()` lists the `Steps.*` symbols a Step's
  script and Step Environments read, and the ones its pending groups still wait on, and `job_output_inputs` (M4.9)
  those of a job output. The model doesn't expose the full input set of a group it has already evaluated (M4.1
  computes it internally), which a scheduler that allows re-runs would also need. RFC 0013 adds a loop's own
  expressions as consumers, and L6 exposes their inputs.
* RFC 0011 §Job outputs item 5 re-evaluates a job output after a re-run commits, and the new value replaces the
  old one. `evaluate_job_output` can be called again with the new inputs. If that re-evaluation fails, the output
  keeps its earlier value and is marked stale, which is the caller's job.

### Model specs to update

`specs/model/template-types.md`, `validation.md`, `job-creation.md`, `job-types.md`, `parameter-space.md`,
`step-dependencies.md` (skipped and zero-Task dependencies), `public-api.md` (including `ChunkTaskIndex`), and a new
`job-outputs.md`. Also update
`specs/architecture.md` with the progressive binding and job output evaluation stages; `job-creation.md` gets the
materialization groups, the group inputs, and the evaluation order of `advance_step_binding`.

## openjd-expr

No new types, functions, or operators, which matches RFC 0011 §Modifications to the Expression Language.

* "Unresolved until run time" is `ExprValue::Unresolved(T)` (`value.rs:789`), and its coercion is type-level
  (`value.rs:904`). A `T?` placeholder is accepted where `T` is required and checked later (04 T3). That is RFC 0011's
  stated behavior (Design Choice 5), and the prototype adds a test that pins it for a `Steps.*` symbol.
* The `Steps` namespace is just symbol-table paths (`Steps.P.Output.V`). `SymbolTable` is hierarchical, and
  `copy_symbol_value` (`format_string.rs:595`) stops at the deepest stored value. So when `filter_symtab_for_step`
  copies the symbols a Step reads, a reference such as `Steps.P.Output.Name.upper()` keeps `Steps.P.Output.Name`.
* No reduction helpers are needed. The RFCs' samples use `sum`, `min`, `len`, `range`, slicing, comprehensions, and
  `flatten`, which all exist. `argmin` and `argmax` are RFC 0006 future work.
* Deferred: `FormatString::segment_static_types(...)`, which returns each interpolation's static type and whether it
  is the whole field. The M2 warning needs it, and it ships with the warning.

The expr specs need no change for the language unless the warning lands.

### E1. Reverse path mapping

`PATH` outputs are stored in the submission path context (RFC 0012 §Path values), so `openjd_expr::path_mapping`
(`crates/openjd-expr/src/path_mapping.rs`), which has the forward direction (`PathMappingRule::apply_with_format`,
`apply_rules_with_format` at `:209`), gets the reverse:

```rust
/// Map a path reported on the worker back to the submission path context.
pub fn unapply_rules(
    rules: &[PathMappingRule],   // the Session's rules, sorted as for apply_rules
    path: &str,
    worker_format: PathFormat,
) -> Result<String, UnmapError>;
pub enum UnmapError { Relative, NoRoundTrip }
```

It implements RFC 0012's reference inverse: a URI is returned unchanged; a relative path is `Relative`; an empty rule
list returns the absolute path unchanged; otherwise it normalizes the path, collects the rules whose `destination_path`
matches on a component boundary (case-insensitively for Windows paths), builds each candidate from the remaining
components, tries them by decreasing `destination_path` length and then in the order the caller listed the rules, and
returns the first that maps back. A candidate is the remaining components joined with the separator of the rule's
`source_path_format` (`/` for `URI`), joined to `source_path` with one more separator unless `source_path` already ends
with one, or `source_path` itself when no components remain. The Session passes its rules sorted by `source_path` length
(`session.rs:682`), so "listed order" there is that order; RFC 0012's tie-break is only a SHOULD, and the round-trip
check keeps the result correct either way.

Two details of the existing forward code matter:
* `apply_rules_with_format` returns its input unchanged when no rule matches, and `apply_with_format` doesn't
  normalize (a `destination_path` with a trailing separator gives `/mnt/p//x`). So the round trip uses a forward
  helper that reports whether a rule matched, and compares the normalized forward result with the normalized reported
  path under the worker format's rules (case-insensitive with both separators on Windows), not as strings.
* `is_rooted` (`path_mapping.rs:182`) does not treat a Windows drive path such as `C:\x` as rooted, so the `Relative`
  check needs its own absoluteness test (a drive letter or UNC prefix on Windows, a leading `/` on POSIX).

It lives beside the forward code and reuses `split_path_parts` (`:174`). Its
tests include RFC 0012's nested-rule example (`/mnt/p/x/f` is `NoRoundTrip`), a root `source_path` such as `P:\`, a
URI-source rule, and the conversion table row (`/mnt/proj/shot01/out` → `P:\shot01\out`).

`specs/expr/path-mapping.md` gains the reverse-mapping rules beside the forward ones, and `specs/expr/public-api.md`
lists `unapply_rules` and `UnmapError`.

## openjd-sessions

### S1. API

* `run_task` (`session.rs:1758`) takes a `StepScript` and a `resolved_symtab`, not a `job::Step`, so the new inputs
  arrive in a `RunTaskOptions` struct, added as one new parameter beside the existing ones: `output_variables: Option<&[openjd_model::job::OutputVariable]>`, the Step's
  output declarations, and `path_inputs: &[openjd_model::job::PathInput]`, the list M4.5 records (S4 uses it).
  The crate is pre-1.0, so this change is allowed.
* The Session must know whether the Job lists `CONTROL_FLOW`, and for each Environment it enters whether a Job
  Template or an Environment Template defines it. Both decide whether a directive fails its Action or is ordinary
  output. For the first, `openjd run` already passes the Job Template's own profile (`execution.rs:128`), which
  carries its extensions, but the profile is documented as selecting functions and redaction, and a caller may pass a
  wider one. Add an explicit `SessionConfig` field so the rule doesn't depend on how a caller built the profile. For
  the second, `enter_environment` (`session.rs:1133`) takes a bare `&Environment`; add a parameter for its origin to
  it, to `enter_environment_with_output` (`:1147`), which does the work, and to `exit_environment` (`:1479`).
* `ActionMessage` (`action.rs:39`) gets `OutputVar { name: String, value: ExprValue }`, and `SessionConfig` gets an
  optional output callback, `Fn(&session_id, task: Option<i64>, &name, &ExprValue)`, called once per accepted value,
  so a caller can forward each value as it arrives (RFC 0012 §Committing values). Putting the values on `ActionStatus`
  instead would re-clone a growing `APPEND` list on every status callback, because `action_status()` builds a new
  struct each time (`session.rs:915`–`:925`).
* `ActionResult` (`action.rs:74`) gets `outputs: Option<IndexMap<Option<i64>, IndexMap<String, ExprValue>>>`, keyed by
  the Task's `CHUNK[INT]` value (`None` for an unchunked Step). It is keyed by value, not position, because a chunk's
  `range_expr` is sorted and deduplicated while per-Task iteration keeps a list range's written order (for
  `range: [5, 1, 3, 2]`, chunks `{1, 5}` and `{2, 3}` against iteration order 5, 1, 3, 2). The value is `Some` only when
  the Step declares outputs and the Action succeeded. `ActionMessage::OutputVar` gets `task: Option<i64>`.
* `run_task` treats the Step as chunked when an entry of `task_parameter_values` has
  `param_type == TaskParameterType::ChunkInt`, takes that entry's values as the chunk, and passes them to the
  collector; otherwise it passes `None`. The value may be an `ExprValue::RangeExpr` (as `openjd run` passes) or an
  `ExprValue::String` such as `"1-10"`, which the session accepts today (`session.rs:2609`, `:2636`; test
  `task_chunk_int_stays_string`). A string is parsed as a range expression; a parse failure fails the Action with
  "CHUNK[INT] parameter '<name>' value '<value>' is not a range expression".
* `run_task` builds the `OutputCollector` with the Session's sorted path mapping rules (`session.rs:681`) and
  `PathFormat::host()`, so `PATH` values are mapped back before they are reported or returned.
* `SessionLimits` (`limits.rs:23`) gets `max_task_output_bytes: Option<usize>`, with a default of at least 1 MiB and
  a `From<&CallerLimits>` mapping. The 64 KiB line limit stays as it is, which meets RFC 0012's floor (§Limits item 1).

### S2. Recognizing the directive

* The directive needs its own recognizer. `parse_directive` (`action_filter.rs:45`) matches only
  `openjd_<kind>: ` with a non-empty payload (`:47`–`:52`), but RFC 0012 recognizes any line that begins with
  `openjd_output_var:`, with optional whitespace after the colon. So `openjd_output_var:N=1` is a valid directive and
  `openjd_output_var: ` with nothing after it is an invalid one; neither may fall through as ordinary output. Check
  the `openjd_output_var:` prefix before `parse_directive`, and test both lines.
* The filter is built with a treatment for the current Action: `Allowed` (the Action may report outputs),
  `Forbidden` (a Job Template Action that may not), or `Ignored` (ordinary output: no `CONTROL_FLOW`, or an
  Environment Template Action). `run_task` uses `Allowed` only when `output_variables` is `Some`. Under
  WRAP_ACTIONS, the treatment follows the Action the hook runs in place of, which `run_task`, `enter_environment`,
  and `exit_environment` already know.
* With `Ignored`, the line falls through as today. With `Forbidden`, the filter returns a cancel callback, as a
  malformed `openjd_env` does, and the Action fails with a message that names the directive.
* With `Allowed`, `process_line` (`subprocess.rs:1219`) hands the payload to an `OutputCollector` (M3) owned by the
  line reader for this Action. Processing is synchronous in the reader, so an `@` file is read before any later line
  is processed, as RFC 0012 requires. An invalid directive cancels the Action at once and fails it, as RFC 0012
  requires, through `CancelMarkFailed`, which calls `cancel_action(None, true)` (`session.rs:2323`), so the Action's
  configured cancelation applies. That path needs one change:
  `process_line` sends a fixed `"Action canceled due to malformed command"` as the `CancelMarkFailed` message
  (`subprocess.rs:1279`–`:1282`), which overwrites any earlier `fail_message` when the Session applies it
  (`session.rs:2322`), and RFC 0012 requires the failure to name the output, the problem, and the offending text.
  Pass the collector's message through as the `fail_message`.
* Truncation: `truncate_line` (`subprocess.rs:62`), and in the helper `LineFramer::accumulate` (`framer.rs:195`),
  `decode_and_truncate` (`framer.rs:89`), and `fit_out_payload` (`framer.rs:116`), cut long lines silently today.
  `accumulate` is the one that cuts an ordinary over-long line on the cross-user path; the other two helper sites
  then see an already-capped line. Each must report that it truncated (for the
  helper, a flag in `Response::Out`, which is a protocol change on both sides of the helper boundary). A truncated
  line that starts with `openjd_output_var:` is invalid, and the discarded remainder is never treated as a line.
* Encoding: a line that `decode_backslashreplace` had to escape is not valid UTF-8, so a directive on it is invalid.
  The reader must carry a "lossy" flag with the decoded line to tell. On the cross-user path the helper decodes the
  line (`framer.rs:48`, `:89`), so this flag also crosses the helper protocol, beside the truncation flag.
* Line ends: the Unix helper sends `line.trim_end()` (`helper/src/runner.rs:61`–`:63`), which strips all trailing
  Unicode whitespace, while the Windows helper (`runner_win.rs:191`–`:197`) and the direct path
  (`subprocess.rs:1121`–`:1126`) strip only the line terminator. RFC 0012's grammar allows
  only spaces and tabs around the value, so a directive followed by, say, a form feed would be valid on one path and
  invalid on the other. Make the Unix helper strip only the line terminator, and let the parser apply the grammar.
* Logging: a valid directive line is treated like the other directives. It is logged by default and hidden when
  `echo_openjd_directives` is `false` (`action_filter.rs:211`). An invalid or `Forbidden` directive must always stay
  in the log, with ` -- ERROR: <message>` appended, as the error branches for `openjd_env` already do
  (`action_filter.rs:166`–`:175` and `:193`–`:204`, returning `pass_through = true`). It must not reach the shared `:211` path, which
  would hide it when echo is off.

### S3. Reading `@` files

* Resolve a relative path against the Session working directory. An absolute path is used as written, which in
  practice means it must name something inside the working directory; to use a beneath-the-root resolver with it,
  first make it relative to the working directory, and treat a path outside it as invalid.
* Symbolic links are followed, as RFC 0012 §File values requires ("after following symbolic links"): a link that stays
  inside the working directory is valid. What must be inside is the file actually opened. On Linux, open with
  `openat2(RESOLVE_BENEATH)` from a handle to the working directory, which follows links but refuses any that leave
  it, and fall back to opening the path and checking `/proc/self/fd/<n>`. On macOS, open and check `F_GETPATH`; on
  Windows, `GetFinalPathNameByHandleW`. Open without blocking (`O_NONBLOCK` on Unix), because opening a FIFO for
  reading otherwise blocks until a writer appears, which would stall the line reader and, on the helper path, cancel
  handling. Then require a regular file with `fstat`, and read at most `collector.remaining_bytes_for(name, task) + 1`
  bytes.
* Same-user Sessions read the file directly; that meets RFC 0012's access rule (§File values item 3).
* Cross-user Sessions must read it as the job user. The parent must not read the file itself, even with an owner or
  link-count check, because RFC 0012 requires no more access than the job user. PR 5 has the helper, which runs as
  the job user, resolve `@` directives as it reads the Action's output, so RFC 0012's ordering falls out of the
  stream:
  * `RunCommand` (`protocol.rs:19`) gets an optional `output_files: { max_bytes }`. The parent sets it only for an
    `Allowed` Action; otherwise the helper leaves `openjd_output_var:` lines alone, so `Ignored` lines stay ordinary
    output and `Forbidden` lines reach the parent unchanged and fail there. The root for path resolution is
    `RunCommand.cwd`, which is the Session working directory (`session.rs:2055`, `:2127`).
  * The helper sub-crate depends only on `serde`, `serde_json`, `nix`, and `windows` (`helper/Cargo.bundled.toml`), so
    it can't use the model's parser. It gets a minimal recognizer for the `@` form only, which must accept exactly the
    lines the M3 parser parses as `File` and extract the same name, selector, and path: the `openjd_output_var:` prefix,
    optional spaces or tabs, an `<Identifier>`, an optional `[<Int>]` selector, `=`, optional spaces or tabs, `@`, and a
    non-empty path with trailing spaces and tabs removed. Anything else goes to the parent as an ordinary `Out` line for
    the model parser. A line the helper truncated or had to decode lossily is not resolved; it is forwarded as an `Out`
    line with its S2 flags, and the parent rejects it. The file operations need new `nix` features (`fs`) and `windows`
    features (`Win32_Storage_FileSystem`), which go through the helper's own `deny.toml` and gates.
  * For a `@` directive, the helper reads the file as above, up to `max_bytes + 1`, and sends a new
    `Response::OutputFile { line, content | error }` in place of the `Out` line. The parent reads newline-delimited
    JSON with a 128 KiB line cap (`cross_user_helper.rs:23`, checked at `:119`–`:128`), and content up to 1 MiB
    doesn't fit one line, so the frame is a header followed by chunks of JSON-escaped content, each sized by its
    encoded length as `fit_out_payload` does.
  * The helper doesn't know the declared outputs or the values so far, so it reads before the parent's checks. To
    keep RFC 0012's outcome, it reports a read failure in the frame rather than failing the line itself, and the
    parent applies `check_name` first, then the read result, then the remaining-size check. This differs from
    RFC 0012's order in two ways that don't change the outcome: the helper may read a file whose directive is then
    rejected, and it reads up to the whole per-Task limit rather than what remains.
  * The parent re-parses `OutputFile.line` with the M3 parser and treats a disagreement with the helper's name,
    selector, or path as invalid. An `@` directive on the cross-user path that arrives as a plain `Out` line, because
    the helper's recognizer didn't match it, is invalid too. The parent never reads the file.
  * A helper command to read a file on request, answered while a `Run` is in progress, was considered. Its reply
    would arrive on the stream the parent's `select!` loop (`cross_user_helper.rs:620`) reads, interleaved with later
    `Out` lines, which the parent would have to hold back to keep the ordering.
* On the direct path, file reads are blocking I/O inside the async line reader. They are bounded by the size limit;
  use `spawn_blocking` and await it before the next line, so ordering is kept.
* The runtime never modifies or deletes the file.

### S4. At exit, and `Steps.*` values at Task run time

* If the Action did not succeed, or a directive was invalid, return `outputs: None`. Values already reported through
  the callback are discarded by the caller.
* Otherwise call `collector.finish()`. An error fails the Task with its message; success returns the values.
* `Steps.*` values arrive in `resolved_symtab` (M4 item 5), with path-typed values only in their `RawOutput` and
  `TaskRawParam` forms. These must stay in the stored (submission) form until they are mapped, but
  `build_symbol_table` deserializes the base with `to_symtab(PathFormat::host())` (`session.rs:2473`), which on a
  Windows worker rewrites `/` to `\` in every `path`-typed entry, so a POSIX submission path would no longer match a
  POSIX rule. `RawParam.*` avoids this by being stored as `string` (`create_job/parameters.rs:1429`–`:1447`). The
  same applies here: `attach_step_inputs` stores path-typed `Raw` values as `string` (or `list[string]`,
  `list[list[string]]`), and lists the symbols in `job::Step::path_inputs` with their declared types.
  `build_symbol_table` then gets a second loop beside the `Param.*` one (`:2480`–`:2525`): for each entry of
  `path_inputs` it reads the stored strings, sets `RawOutput.<V>` or `TaskRawParam.<N>` to `path` values in the
  stored form (`ExprValue::new_path` with the stored value's own format: Windows for a drive path or one starting
  with `\\`, URI for a URI, POSIX otherwise; not the host format; `new_path` normalizes separators within that
  format, so `P:/x` reads back as `P:\x`, which is the same Windows path), and
  sets `Output.<V>` or `TaskParam.<N>` to the forward-mapped `path` values, element by element.

### Sessions specs to update

`specs/sessions/action-filter.md` and `action-messages.md` (the new directive and its three treatments),
`subprocess.md` and `embedded-cross-user-helper.md` (truncation flags, the `@` file read), `session.md` (collection,
reverse mapping of `PATH` outputs, forward mapping of path-typed `Steps.*`, and the new configuration), and
`public-api.md`.

## openjd-cli

### C1. `openjd run` as a scheduler simulator

Today `run_workload` (`execution.rs:341`) iterates `steps_to_run` in topological order and calls `execute_step`. The
prototype keeps that loop and adds a small in-memory scheduler state:

```rust
struct StepRecord { state: SimStepState, task_outputs: Vec<IndexMap<String, ExprValue>> }
enum SimStepState { Pending, Bound, Skipped, Succeeded, Failed, Blocked }
```

The record's `job::Step` lives in the simulator's copy of the `job::Job`; while the record is `Pending`, that Step's
`binding` is `PendingInputs` with its `PendingBinding`, so the simulator always knows which groups have evaluated. A step **advance** is: build `StepInputs` from the records of the Step's direct dependencies with
`StepInputs::add_producer` (`Succeeded`, `Skipped`, or `TasksKnown` for a dependency whose Tasks are known), call
`advance_step_binding`, and swap in the result. For each fact the call adds, print one line, such as
`Step 'StylizeFrame': Tasks known (1440 tasks)` or `Step 'C': host requirements known`; a Step that becomes `Bound`
prints `Step 'X' is ready`; a `Skipped` result prints `Step 'X' skipped (enableIf is false)`. On error, print the
failure (the group, field, and expression), mark the Step `Failed`, and set `ctx.session_failed`, so the run stops
before it starts any more Tasks, as it does after a failed Task. This is how the CLI shows RFC 0011's early failure: a
Step whose parameter space fails fails as soon as the producer it reads finishes, before a later producer that only
its script reads has run.

Before the loop, print the job outputs that `create_job` already evaluated and, for each pending Step, what is
known of it (the `binding_report()` of M4.3). Then, for each Step in topological order:

1. If the Step is already `Failed`, skip it. If a dependency is `Failed` or `Blocked`, mark the Step `Blocked` and
   print why. The current stop-on-first-failure behavior (`ctx.session_failed`) already prevents it from running.
2. If it is still `Pending`, advance it. Every one of its direct dependencies is terminal by now, so every input is
   concrete and the result is `Bound`, `Skipped`, or an error. A `Pending` result here is a bug in the simulator
   or the model, and is reported as an internal error naming the groups still waiting.
3. If it is `Skipped`, it has zero Tasks; record it and continue at item 6.
4. Otherwise call `attach_step_inputs`, enter only the enabled step Environments, and run the Tasks as today. Record
   `ActionResult.outputs` for each Task index, using the model's `ChunkTaskIndex` for a chunked Step.

   * Build: from the per-Task iterator, map each key (the values of the Step's other Task parameters, `CHUNK[INT]`
     value) to its iteration indices, in order. A chunk runs with the other parameters fixed, so each `CHUNK[INT]`
     value recurs once per combination of them, and a non-chunk parameter may repeat a value (`STRING [a, a]`), so a
     key can have several indices.
   * Assign: for each chunk, in the chunked iterator's order, and each `CHUNK[INT]` value in it, take the next unused
     index for its key, and store that Task's map there. Repeated keys belong to Tasks with identical parameters, so
     which one gets which index is not observable.

   Print the outputs after each Action with their units (`Output mpjpe_mm = 52.6 mm`). Values reported while a Task
   runs can be printed as they arrive, marked as not committed.
5. A Step with zero Tasks prints `Step 'X' has 0 tasks` and succeeds.
6. After each Step succeeds or is skipped, advance every other `Pending` Step, in topological order, repeating until a
   pass changes nothing (a Step whose Tasks become known can make its own consumers' `TaskParam` inputs concrete).
   This is the simulator's form of RFC 0011's SHOULD (Design Choice 10) to evaluate each group as soon as its inputs
   are concrete.
7. After each Step's Tasks become known, or it succeeds or is skipped, call `evaluate_job_output` for every job
   output that is still pending and whose inputs are now all concrete (`job_output_inputs`), store the result, and
   print it once, as `Job output VideoCount = 12`. An output evaluated from a Step's `TaskParam` before that Step ran
   stays committed if the Step then fails. An error prints the output, expression, and message, marks the run
   failed, and stops running Tasks, as a failed Task does. At the end, list the job outputs that were never evaluated, naming the Step
   each waits on. With `--step` alone, those are the outputs that read Steps not run. The run is reported as succeeded
   only after every job output whose inputs are concrete has been evaluated. The CLI imposes no size limit on job
   outputs, which RFC 0011's 1 MiB minimum allows (§Job outputs item 7).

Job Environments with `enabled == false` are not entered (the loop at `execution.rs:360`).

Options:

* `--step X` without `--run-dependencies`, for a Step that reads `Steps.*`, needs the upstream values. Add
  `--step-inputs file://inputs.yaml | '<json>'`, a map of `Steps.P.Output.V` and `Steps.P.TaskParam.N` to values,
  checked against the declared types with the M3 coercion. Without it, the error names the missing producers. This
  option is what the proxy testing of the RFC drafts did by hand.
* `--maximum-tasks` or `--tasks` on a producer leaves its outputs incomplete. Its consumers become `Blocked` with
  "inputs from Step 'P' are incomplete because not all of its Tasks ran". This follows RFC 0011's completeness rule
  (§Reductions item 3).
* `--output json|yaml`: `RunResult` (`run/result.rs:7`) gets a `steps` array of
  `{name, state, task_count, outputs: [ {<var>: <value>} ... ]}` and a `job_outputs` map of name to value (`null`
  for no value) plus a list of the ones not evaluated.
* No path mapping rules (the default) means `PATH` outputs are stored as reported; `--path-mapping-rules` exercises
  reverse mapping.

### C2. `openjd summary` and `openjd check`

* `summary` (`summary.rs:113`) prints what `create_job` knows of each pending Step, from `binding_report()`, which
  follows RFC 0011 §Implementation strategy: one line each for whether it runs, its Tasks, and its host requirements,
  either known or with the outputs it waits for. For example, `StylizeFrame` shows `Tasks: determined at run time from
  Steps.VideoToFrames.Output.FrameCount`, and a Step whose range reads no `Steps.*` but whose step `let`, read only by
  its script, reads an upstream output (the A11 case) shows `Tasks: 240 (known; Step pending inputs for
  Steps.P.Output.X)`. A Step with a dynamic `enableIf` shows `Enabled: determined at run time from ...` and its Tasks
  as waiting for it. The total becomes `Total tasks: N + (determined at run time)`, where `N` counts every Step whose
  Tasks are known, pending or not. A statically skipped Step is shown as `SKIPPED` with 0 Tasks.
* `summary` lists the job outputs, with their types and units, and which Steps each waits on. This is the CLI's
  form of RFC 0011's SHOULD that a pending job output be shown with what it waits for (§Job outputs item 2).
* `check` needs no change beyond M2.

### CLI specs to update

`specs/cli/run.md`, `summary.md`, and `output-formatting.md`, plus the CLI help text in `src/help.rs` (Q8).

## Loops (RFC 0013)

RFC 0013 adds loops to `CONTROL_FLOW`. A loop is a `<StepLoop>` entry in the template's `steps`, whose body of
ordinary Steps runs once per iteration, one iteration after another. The plan follows RFC 0013's own structure. The
loop is a node of the step graph, bound like a Step (RFC 0013 §Loop binding). Each iteration creates one instance of
every body Step, and an instance is a Step for every rule of RFC 0011 and RFC 0012 (RFC 0013 §Iterations), so the
existing binding core, sessions, and simulator path run it. The iteration boundary is the one new evaluation point
(RFC 0013 §The iteration boundary). The model provides the three functions that RFC 0013 §Implementation Impact
names: one binds a loop, one instantiates an iteration's body, and one evaluates a boundary. The CLI simulator drives
them.

Code references in this section are to `control-flow-prototype` at `74e5c98`. The prototype rejects
`TASK_DEPENDENCIES` as unsupported (see the scene-graph golden tests), so the `TASK_DEPENDENCIES` rules of RFC 0013
§Composition with other extensions stay unimplemented with the rest of that extension. A body Step may be chunked,
and RFC 0012's chunked-Step rules apply to each instance unchanged.

### L1. Schema

* `template::JobTemplate.steps` (`template/job_template.rs:29`) becomes `Vec<StepEntry>`, with
  `enum StepEntry { Step(StepTemplate), Loop(StepLoop) }`. Its `Deserialize` chooses `Loop` for a mapping that has a
  `loop` key and `Step` for any other, as RFC 0013 §Job Template says. `#[serde(untagged)]` is not used, because its
  error for a malformed entry names neither shape. `StepLoop` uses `deny_unknown_fields`, as `StepTemplate` does
  (`template/step.rs:233`), so a loop with `script` or `parameterSpace` fails as an unknown field of a loop
  (`3.8--loop-with-script`, `3.8--loop-with-parameter-space`).
* Without `CONTROL_FLOW`, an entry with `loop` still parses as a `StepLoop`. Pass 11 then reports it at
  `steps[i] -> loop` as needing `CONTROL_FLOW`, as it does for `outputVariables` (`1.1--loop-without-extension`).
* About 50 places iterate `jt.steps`: the validation passes, `create_job`, and the binding core
  (`job/create_job/binding.rs:305` indexes `cx.jt.steps[step_index]`). So that each doesn't re-derive the shape,
  `JobTemplate` gets helpers. `step_templates()` yields every `StepTemplate`, top-level or in a body, with its error
  path (`steps[i]` or `steps[i] -> loop -> steps[j]`) and its loop, if any. `top_level_steps()` yields the
  top-level `StepTemplate`s alone, in template order. A template without loops behaves exactly as today. This is a
  public API change, recorded in `public-api.md`.
* `StepLoop` has `name`, `description`, `enable_if: Option<BoolOrFormatString>`, `dependencies`, the
  `<LoopDefinition>` (serde name `loop`), and `output_variables: Option<Vec<LoopOutputVariableDefinition>>`.
  `LoopDefinition` has `variables: Option<Vec<LoopVariableDefinition>>`, `while_` and `until`
  (`Option<BoolOrFormatString>`), `max_iterations: IntOrFormatString` (`template/task_parameters.rs:286`), and the
  body, `steps`. The body is parsed so that an entry with a `loop` key fails with a message that says loops do not
  nest (`3.8.1--nested-loop`), not as an unknown field of a `<StepTemplate>`.
* `LoopVariableDefinition` and `LoopOutputVariableDefinition` reuse the job-parameter machinery as
  `OutputVariableDefinition` and `JobOutputDefinition` do (`template/output_variables.rs:73`, `:309`), with two new
  `DefinitionKind`s (`:100`). A loop variable takes `initial` and `next` out of the mapping, and has no `collect` or
  `unit`. A loop output takes `value` and `collect` (`LAST` or `APPEND`, required), and `unit`. Both allow
  `LIST[LIST[INT]]` and reject `default`, `userInterface`, and `dataFlow` as outputs do. Their constraints become the
  same `OutputConstraints`, so the `check_constraints`, `check_element`, and `check_length` of M1 apply.

### L2. Validation

Each rule of RFC 0013 §Validation, "At template validation", and where it goes. The structural rules go into
`structure.rs` and pass 11, and the scope rules into pass 8 and its `Steps.*` checker (`steps_scope.rs`), as for
RFC 0011 (M2).

1. A `<StepLoop>` has `name` and `loop` and only its own properties, and its `loop` has `steps`, `maxIterations`, and
   exactly one of `while` and `until`. Messages name the field (`3.8.1--missing-max-iterations`,
   `3.8.1--both-while-and-until`, `3.8.1--neither-while-nor-until`).
2. A body is a non-empty list with no `<StepLoop>` in it (`3.8.1--empty-body`, `3.8.1--nested-loop`).
3. The names of every top-level Step, loop, and body Step are distinct. This replaces the check of the top-level list
   alone (`structure.rs:168`) (`3.8--duplicate-body-and-top-level-name`, `3.8--duplicate-name-across-loops`,
   `3.8--loop-name-same-as-body-step`).
4. A top-level Step or loop depends only on top-level entries, and a body Step only on Steps of its own body
   (`3.8--top-level-depends-on-body-step`, `3.8.1--body-depends-on-outside-step`). The cycle check runs on the
   top-level graph and on each body's graph (`3.8.1--body-dependency-cycle`), through the Kahn's-algorithm pass
   factored out in M4.1 (`structure.rs:1130`), which gains a variant over a body.
5. `variables`: 1 to 32 elements, unique `<Identifier>` names, none of them `Iteration`, an `<OutputVariableType>`
   each, and constraint properties valid for the type (`3.8.2--duplicate-variable-names`,
   `3.8.2--variable-named-iteration`).
6. Loop `outputVariables`: 1 to 32 elements with unique names; `collect` present, `LAST` or `APPEND`, and `APPEND`
   only with a `LIST[...]` type; `value` present; types, constraints, and `unit` as for RFC 0012's `outputVariables`
   (`3.8.3--*`, `3.8--loop-output-duplicate-names`).
7. `maxIterations`: a literal from 1 to 1,000, or a format string of type `int` that reads only `Param.*` (not of a
   `PATH` or `LIST[PATH]` type), `RawParam.*`, and `Job.Name` (`3.8.1--max-iterations-*`). A literal out of range is
   a template error; a computed one is checked at loop binding (L3).
8. `Loop.*` and `RawLoop.*`: pass 8 seeds them as `ExprValue::unresolved(T)` only where RFC 0013 §The `Loop.*` scope
   allows. For loop `L`, that is every format string of a body Step of `L` except `combination`, and `L`'s `while`,
   `until`, `next`, and loop output `value`s; not `L`'s `enableIf`, `maxIterations`, or `initial`, nor anywhere
   else. `Loop.<V>` whose type is or contains `path` is seeded only in host scopes; `RawLoop.<V>` everywhere it is
   available. `Loop.Iteration` is `int`. Unknown symbols under `Loop.` and `RawLoop.` get specific messages, as
   `Steps.` does (M2): outside any loop, a variable of another loop, no such variable, and a path-typed `Loop.<V>`
   in a TEMPLATE-scope field ("use `RawLoop`") (`3.8.2--initial-reads-loop`,
   `3.8.2--path-variable-mapped-in-template-scope`, `3.8.4--loop-symbol-in-*`, `3.8.4--loop-variable-of-other-loop`).
9. `Steps.*` in and around a loop, from RFC 0013 §`Steps.*` in and around a loop. `add_steps_scope` (M2) takes the
   field's place in the template:
   * a body Step of `L` sees the body Steps it lists in its `dependencies` and the entries `L` lists in its own;
   * `L`'s `enableIf`, `while`, and each `initial` see `L`'s dependencies;
   * `L`'s `until`, `next`, and loop output `value`s see `L`'s dependencies and every body Step of `L`;
   * a top-level Step, another loop or its body, or a job output never sees a body Step, and gets a message that
     names the loop and suggests a loop output (`3.8.4--outer-step-reads-body-step`,
     `3.8.4--job-output-reads-body-step`);
   * `Steps.<L>.Output.<V>` and `RawOutput.<V>` name a declared loop output, and `Steps.<L>.TaskParam.*` and
     `TaskRawParam.*` are errors that say a loop has no Task parameters (`3.8.4--loop-taskparam`,
     `3.8.4--loop-undeclared-output`).

   A loop's symbols are typed by RFC 0013's table: a `LAST` output is `T` for an `until` loop, and `T?` for a
   `while` loop or a loop with `enableIf`; an `APPEND` output is `list[E]`, or `list[E]?` for a loop with
   `enableIf`. A loop has no Tasks, so its outputs are never wrapped in a per-Task list.
10. Every expression type-checks with `Loop.*` and `RawLoop.*` unresolved: `while` and `until` are `bool` (not
    `bool?`), each `initial` and `next` has its variable's type, and each loop output `value` its output's type, or
    under `APPEND` its element type (`3.8.1--until-not-bool`, `3.8.2--next-type-mismatch`,
    `3.8.3--append-value-is-element`).

The step-count caller limit (`structure.rs:28`–`:39`) counts every top-level Step, loop, and body Step once (RFC 0013
§Limits item 3; A18). The warning of RFC 0011 §Validation item 7 for a `while` loop's `LAST` output is deferred with
the rest of that warning (A1). The `create_job` check-table seeding of M4.3 also seeds `Loop.*` and `RawLoop.*` for
body Steps, so a template that passes `openjd check` also passes each instance's carried-forward checks (L4).

### L3. Job creation and the job model

A `job::Job` represents a loop as its own entry, beside its Steps:

* `job::Job.steps` (`job/mod.rs:92`) keeps one `job::Step` per top-level Step, and only those. `job::Job` gets
  `loops: Vec<job::Loop>`, one per loop in template order, with `skip_serializing_if` and `#[serde(default)]` so that
  the serialized form of a Job without loops doesn't change. Each `job::Loop` records its position among the
  template's `steps` entries, so that a Job alone can list its entries in template order.
* The alternative, one enum for every entry of `job::Job.steps`, would change every caller that reads
  `job.steps[i]` as a Step with a script, including `openjd summary` and the simulator, for Jobs without loops too.
  With separate lists, code that knows only Steps is unchanged for those Jobs.
* `job::Loop` has the loop's `name`, `description`, `dependencies`, its `variables` and `output_variables`
  declarations (name, type, `collect`, `unit`, `OutputConstraints`, as `job::OutputVariable` holds them, M1), the
  names of its body Steps in body order, and `binding: LoopBinding`. The body Step templates and the loop's format
  strings stay in the `JobTemplate`, which the loop functions take, as `advance_step_binding` does.
* `LoopBinding` mirrors `StepBinding`: `Bound(BoundLoop)`, `PendingInputs(PendingLoopBinding)`, or `Skipped`.
  `PendingLoopBinding` has a `GroupState` (M4) for each of the loop's groups: enablement, bound, and start.
  `BoundLoop` holds `max_iterations: u32`, each variable's `initial` value, and, for a `while` loop, whether the
  first `while` is `true`. A bound loop whose first `while` is `false` has zero iterations.
* `StepDependencyGraph` (`job/step_dependency_graph.rs`) includes loops as nodes. `step_dependency_order` (M4.1)
  orders the top-level entries, so `create_job` instantiates Steps and loops in dependency order.
* `create_job` binds a loop through the same core as a Step, with RFC 0013 §Loop binding's groups in order:
  enablement (`enableIf`), bound (`maxIterations`, which must be 1 to 1,000), and start (each `initial` in
  declaration order, then the first `while` with `Loop.Iteration` 0 and the initial values). `MaterializationGroup`
  (`job/binding.rs:32`) gains `LoopBound` and `LoopStart` so that a `BindingError` names the group. Each `initial`
  value is converted and checked as a job output's value is (RFC 0013 §`<LoopVariableDefinition>`), through the
  conversion `evaluate_job_output` uses (`job/create_job/job_outputs.rs:145`, made shared). The 1 MiB total of RFC
  0013 §Limits item 4 is checked after the `initial` values (L5).
* Group inputs come from symbol collection, as in M4.1: the enablement group reads `enableIf`, the bound group reads
  no `Steps.*` (validation guarantees it), and the start group reads the `initial` expressions and `while`. A group
  whose inputs are concrete at Job creation, in a loop whose `enableIf` is `true` or absent then, is evaluated then,
  and its failure fails `create_job`, as RFC 0011 requires for a Step and RFC 0013 §Loop binding for a loop.
  Otherwise the loop is `PendingInputs`, and `advance_loop_binding` evaluates the rest:

  ```rust
  pub fn advance_loop_binding(
      job_template: &JobTemplate,
      job_parameter_values: &JobParameterValues,
      ctx: &ValidationContext,
      job: &job::Job,
      loop_index: usize,           // into job.loops
      inputs: &StepInputs,         // the loop's dependencies
  ) -> Result<job::Loop, BindingError>;   // PendingInputs, Bound, or Skipped
  ```

* Nothing of a body Step is evaluated at Job creation (RFC 0013 §Template processing stages), including the
  carried-forward checks that `create_job` runs over every other Step. Those checks evaluate the script and
  Environments with concrete `Param.*` values (`job/create_job/instantiate.rs:272`, `:318`), so they are evaluation,
  and RFC 0013 Open Question 6 confirms that such an error is found when iteration 0 starts. A body Step's Tasks are
  not counted in `max_task_count` at creation.
* `job::Loop::tasks_known()` is `None` until the loop ends (RFC 0011 §Implementation strategy), and `Some(0)` for a
  skipped loop. `job::Loop::binding_report()` reports enablement, the bound, and the start group as M4.3's report
  does for a Step's groups.

### L4. Iterations

A body Step instance is a `job::Step`, created and bound by the same core as a top-level Step, with the loop's
values for the iteration as one more layer of its symbol table. The caller keeps the loop's progress, as the
simulator keeps a `StepRecord`:

```rust
/// A running loop's state between iterations, kept by the caller.
pub struct LoopProgress {
    pub iteration: u32,                                  // the iteration about to start or in progress
    pub values: IndexMap<String, ExprValue>,             // Loop.<V> for `iteration`, in the stored form
    pub outputs: IndexMap<String, LoopOutputSoFar>,      // per loop output: the LAST value or the APPEND list
}
impl LoopProgress { pub fn new(job_loop: &job::Loop) -> Option<Self>; }   // None unless the loop is Bound

/// The body Step instances of one iteration, in body order.
pub struct LoopIteration { pub loop_name: String, pub index: u32, pub steps: Vec<job::Step> }

pub fn start_iteration(
    job_template: &JobTemplate, job_parameter_values: &JobParameterValues, ctx: &ValidationContext,
    job: &job::Job, loop_index: usize, progress: &LoopProgress,
    inputs: &StepInputs,           // the loop's dependencies
    committed_task_count: u64,
) -> Result<LoopIteration, BindingError>;

pub fn advance_body_step_binding(
    job_template: &JobTemplate, job_parameter_values: &JobParameterValues, ctx: &ValidationContext,
    job: &job::Job, loop_index: usize, progress: &LoopProgress, iteration: &LoopIteration,
    body_index: usize,
    inputs: &StepInputs,           // the loop's dependencies and the instance's body dependencies
    committed_task_count: u64,
) -> Result<job::Step, BindingError>;
```

* `start_iteration` creates an instance of every body Step for `progress.iteration`, as `create_job` creates a
  top-level Step: each instance's groups whose inputs are concrete at the start of the iteration are evaluated then
  (RFC 0013 §Iterations item 3, a MUST), with every `Loop.*` symbol concrete. That includes the instance's
  carried-forward checks, which `create_job` skipped (L3). An instance whose groups all evaluate is `Bound`; one whose
  `enableIf` is false is `Skipped`; any other is `PendingInputs`, waiting for body Steps it depends on. An error fails
  the instance and so the loop, finally (RFC 0013 §Failure).
* `advance_body_step_binding` is `advance_step_binding` for an instance, and the caller swaps the result into
  `iteration.steps`. Both are wrappers over the core of M4.3, which is generalized to take the `StepTemplate` and an
  extra symbol layer instead of a top-level index. An instance is named by its body Step's name, which is unique in
  the template, so `StepInputs::add_producer` and `attach_step_inputs` work on instances unchanged, and
  `Steps.<Body>.*` within the iteration needs no new symbol form.
* `Loop.<V>`, `RawLoop.<V>`, and `Loop.Iteration` are concrete in the instance's symbol table, so its
  `resolved_symtab` carries the ones its script and Step Environments read, as M4.5 does for `Steps.*`. A path-typed
  variable is stored once, as `RawLoop.<V>`, as a `string` (or list of strings) in the stored form, and recorded in
  `job::Step::path_inputs` with symbol `RawLoop.<V>`; the Session derives `Loop.<V>` from it (L7). `Step.Name` in an
  instance is the body Step's name (A20).
* Task-count limits apply per instance as each instance's Tasks become known: `max_step_task_count` on its own Tasks,
  and `max_task_count` with `committed_task_count`, which counts the Tasks of every Step and earlier instance whose
  Tasks are known (A19). An instance that exceeds one fails, which fails the loop finally (RFC 0013 §Limits item 2).
  RFC 0013's optional per-loop limits are not added.

### L5. The iteration boundary

```rust
pub fn evaluate_iteration_boundary(
    job_template: &JobTemplate, job_parameter_values: &JobParameterValues, ctx: &ValidationContext,
    job: &job::Job, loop_index: usize, progress: &LoopProgress,
    inputs: &StepInputs,           // the loop's dependencies and every instance of the iteration
) -> Result<BoundaryOutcome, LoopError>;

pub enum BoundaryOutcome {
    Continue(LoopProgress),        // iteration + 1, its variable values, the outputs so far
    Succeeded(LoopResult),
}
pub struct LoopResult { pub iterations: u32, pub outputs: IndexMap<String, ExprValue> }

/// For a bound loop whose first `while` is false, once its dependencies are satisfied.
pub fn end_without_iterations(job: &job::Job, loop_index: usize) -> Result<LoopResult, LoopError>;
```

The caller calls `evaluate_iteration_boundary` when every instance of the iteration is `SUCCEEDED` or `SKIPPED`
(RFC 0013 §Iterations item 4). It evaluates, in RFC 0013's order, with `Loop.*` holding iteration `N`'s values:

1. Each loop output's `value`, converted to its type (or element type under `APPEND`) and checked: under `LAST`
   every declared constraint; under `APPEND` `item` on the new element and `maxLength` on the grown list. It becomes
   iteration `N`'s value. Then the 1 MiB check.
2. `until`, if present. If it is `true`, the loop ends: check each `APPEND` output's `minLength` and return
   `Succeeded` with `N + 1` iterations. No `next` is evaluated.
3. Every variable's `next`, each reading iteration `N`'s values, never another variable's new value. Each result is
   converted and checked against its variable's constraints. Then the 1 MiB check, on the new values (A21).
4. `while`, if present, with the new values and `Loop.Iteration` `N + 1`. If it is `false`, the loop ends as in
   step 2.
5. The bound: if `N + 1` equals `maxIterations`, the loop fails with a message that names `maxIterations` and its
   value. Otherwise `Continue`.

The first error stops the evaluation and fails the loop, with a message that names the field, the expression, the
iteration, and the problem. That covers a `fail(...)` call, a `null` result, a value that doesn't convert, and a
violated constraint. `LoopError` carries the loop's name, the iteration, the field (`value` of an output, `until`,
`next` of a variable, `while`, `maxIterations`, or `minLength` of an output), and the `ModelError`s.

`end_without_iterations` checks each `APPEND` output's `minLength` against `[]`, and gives `null` for each `LAST`
output and `[]` for each `APPEND` output (RFC 0013 §Loop outputs). This check is not a materialization group and is
never part of `create_job` (RFC 0013 §Loop binding). A skipped loop's outputs are all `null`, with no `minLength`
check.

The 1 MiB total of RFC 0013 §Limits item 4 is checked at three points: after the `initial` values (at loop binding,
or at Job creation if the start group is evaluated then, where exceeding it fails `create_job`), after boundary step
1, and after step 3. It counts the JSON serialization of each current value in its stored form (A21). The limit is
`CallerLimits::max_loop_value_bytes: Option<usize>`, where `None` means no limit. The CLI sets none, as it sets no
limit on job outputs, and the model's unit tests exercise all three points with a small limit.

A loop's outputs reach its consumers like a Step's. `StepInputs` gains
`add_loop_producer(&mut self, producer: &job::Loop, state: LoopProducerState)`, with
`LoopProducerState::{Succeeded { outputs: &IndexMap<String, ExprValue> }, Skipped}`. It sets `Output.<V>` and
`RawOutput.<V>` to the value itself (a loop has no per-Task list), leaves a path-typed `Output.<V>` to be derived on
the worker as for a Step (M4.4), and sets every symbol to `null` for a skipped loop. `job::Step::input_producers()`
and `job_output_inputs` already work by name, so a Step or job output that reads a loop needs nothing more.

### L6. Failure, recovery, and consumption

RFC 0013 §Failure makes every loop failure final except one: a failure of the Tasks of a body Step instance of the
iteration in progress, which a scheduler may recover by retrying or requeuing those Tasks. The model never sees a
Task attempt, so every error its loop functions return is final: a loop group that fails (`BindingError`), an
instance group that fails (`BindingError` from `start_iteration` or `advance_body_step_binding`), a Task-count limit,
an error at the boundary or a violated `minLength` (`LoopError`), and reaching the bound. The specs and doc comments
say so. A caller that sees a body Task fail decides whether it is recoverable; the CLI has no retries, so for it
every loop failure is final (L8).

A re-run of a body Task that has already succeeded leaves its instance `SUCCEEDED` and never changes the loop's
state, variables, or outputs (RFC 0013 §Retries and re-runs, RFC 0012 §Retries and re-runs). The model keeps no
Task state, so this needs nothing from it (M5). For consumption, RFC 0013 §Retries and re-runs counts the loop's
`while`, `until`, `next`, `initial`, and loop output `value`s as consumers of every `Steps.*` output they reference,
once evaluated. `job::Loop::expression_inputs(job_template, field)` returns the `Steps.*` symbols that one of those
fields references, found by symbol collection, so that a scheduler that allows re-runs can mark them consumed when
it evaluates the field. The CLI doesn't call it.

### L7. openjd-sessions

A body Step instance is a `job::Step`, so `run_task`, `enter_environment`, and the directive pipeline run it
unchanged. Its Step Environments are entered and exited around its Tasks as any Step's are (RFC 0013 §Sessions).

One change is needed. `path_inputs` (S4) may now name `RawLoop.<V>`, and `forward_twin` (`session.rs:3175`) knows
only `Steps.<Step>.RawOutput.<V>` and `Steps.<Step>.TaskRawParam.<N>`, so it rejects the input. It gains the mapping
`RawLoop.<V>` to `Loop.<V>`, and its error text names the new form. `build_symbol_table` then sets `RawLoop.<V>` in
the stored form and `Loop.<V>` forward-mapped, element by element, as it does for `RawOutput`. This lands in PR 9,
beside the model code that records the input, so that the contract changes in one PR. It comes with a sessions
test for a `PATH` and a `LIST[PATH]` loop variable under path mapping rules, and updates `specs/sessions/session.md`
and the `build_symbol_table` doc comment in `specs/sessions/public-api.md` (`:293`–`:304`), which list the
`Steps.P.RawOutput.V` and `Steps.P.TaskRawParam.N` forms.

### L8. The CLI simulator

`openjd run` treats a loop as a node of the step graph and runs its iterations in place, in the loop over the
top-level entries in dependency order (C1):

* **Records.** A `LoopRecord` beside the `StepRecord`s (`run/simulator.rs:79`) holds the loop's `SimStepState`
  (with `Running` added, as `RUNNING`), its `LoopProgress`, and one record per iteration: its index, its variable
  values, a `StepRecord` per body Step instance, and the loop output values evaluated at its boundary.
* **Binding.** A pending loop is advanced with `advance_loop_binding` whenever a dependency's state changes, as a
  pending Step is (C1 item 6), and reports what it gains: `Loop 'Refine': maxIterations 6`, `Loop 'Refine' is
  ready`, or `Loop 'Refine' skipped (enableIf is false)`.
* **Running.** When its dependencies are satisfied, a bound loop with zero iterations calls `end_without_iterations`.
  Any other loop prints `Loop 'Refine' iteration 0: Samples = 64, Frames = [1, 2, 3]` and calls `start_iteration`.
  Its instances then run in body dependency order through the existing per-Step path: advance, `attach_step_inputs`,
  enabled Step Environments, Tasks, outputs, and the fixpoint of C1 item 6 within the iteration. When every instance
  has succeeded or been skipped, the simulator calls `evaluate_iteration_boundary`, prints the iteration's loop
  output values, and prints either the next iteration's values or the end: `Loop 'Refine' succeeded after 3
  iterations (until is true)`. A succeeded loop is added to `inputs` with `add_loop_producer`, its dependents are
  advanced, and job outputs that read it are evaluated (C1 item 7).
* **Naming.** A body Step instance is `<Loop>[<N>].<Step>` in every log line (`Step 'Refine[1].RenderPass': Tasks
  known (3 tasks)`), in `--output json|yaml`, and in error messages.
* **Failure.** A failed body Task fails its instance and the loop, sets `ctx.session_failed`, and blocks the loop's
  dependents, as a failed Step does. The CLI has no retries, so the failure is final. A binding, limit, or boundary
  error is reported with the group or field, the expression, and the iteration, and fails the loop the same way.
* **Session.** Every Task still runs in the one Session `openjd run` opens, with its Job Environments entered once
  (RFC 0013 §Sessions allows this).

Flags, from RFC 0013 §Implementation Impact:

* `--step L` naming a loop runs the whole loop. `--step` naming a body Step is an error that names its loop.
* `--tasks` and `--task-param` are errors whenever the selected Step is a loop, whether `--step` names it or it is
  the Job's only top-level entry (`run/execution.rs:165` selects that one).
* An explicitly given `--maximum-tasks` is an error with `--step L` for a loop. Today it is an `i64` with
  `default_value = "-1"` (`run/mod.rs:294`), so it becomes `Option<i64>` (or reads clap's `value_source`) to tell an
  explicit value from the default.
* In every other run that includes a loop, `--maximum-tasks` applies to each body Step instance, as to any Step. An
  instance that didn't run all its Tasks is incomplete. Only the instances of its iteration that read its outputs
  are blocked, with C1's message. A failure of another instance still fails the loop. The iteration never completes,
  so the boundary is not evaluated, the loop is `BLOCKED` "by incomplete body Step instance 'Refine[0].RenderPass'",
  and its dependents are blocked behind it.
* `--step-inputs` with `--step L` supplies the outputs of `L`'s dependencies that the loop and its body read. For a
  Step that depends on a loop, it supplies each loop output it reads as one value of the output's declared type (the
  whole list under `APPEND`), not one value per Task.

Reporting:

* `--output json|yaml`: the `steps` array (`run/result.rs:23`) lists a loop's record,
  `{name, state, iterations, outputs: {<var>: <value>}}`, followed by one record per instance in run order, with
  `name` `<Loop>[<N>].<Step>`, `loop`, `iteration`, and the usual `state`, `task_count`, and `outputs`.
* `openjd summary` shows a loop with its condition (`while` or `until`), its bound (the value, or what it is
  computed from), its variables with their types, its loop outputs with type, `collect`, and unit, and its body
  Steps, whose Tasks are determined at run time in each iteration. A loop's Tasks count as determined at run time in
  the total.

### L9. Specs to update

`specs/model/template-types.md` (`StepEntry`, `StepLoop` and its definitions), `validation.md` (each L2 rule),
`job-creation.md` (loop binding, iterations, the boundary), `job-types.md` (`job::Loop`, `LoopBinding`),
`step-dependencies.md` (loops as nodes, body graphs), `job-outputs.md` (loop outputs as inputs), `public-api.md`,
and `specs/architecture.md` with the iteration start and iteration boundary stages. `specs/sessions/session.md` for
L7, and `specs/cli/run.md`, `summary.md`, `output-formatting.md`, and `src/help.rs` for L8.

## Other crates

* **openjd-snapshots**: no change (see the summary).
* **openjd-for-js**: it builds on `openjd-model`, so validation picks up `CONTROL_FLOW` automatically. Rebuild and run
  its vitest suite in the PRs that touch the model. JS bindings for `advance_step_binding` are out of scope for the
  prototype.

## PR and commit order

Each PR is one `implement-change` loop on a topic branch from `control-flow-prototype`. The lead merges it.

| # | PR | Depends on | Contents |
|---|---|---|---|
| 1 | `feat(model): CONTROL_FLOW extension, outputVariables and enableIf schema` | — | M1 except `jobOutputDefinitions`, and M2 items 1–3 (gating, the EXPR prerequisite, structural rules) |
| 2 | `feat(model): Steps.* scope in template validation` | 1 | the M2 scope rules and their messages except the job output scope, `enableIf` typing, and the `create_job` check-table seeding from M4.3. Until PR 4, `openjd run` of a template that reads `Steps.*` fails when a Task starts, because the value is unresolved; this is acceptable on the prototype branch |
| 3a | `feat(expr): reverse path mapping` | — | E1 |
| 3 | `feat(model): output directive parser and collector` | 1, 3a | M3 |
| 4 | `feat(model): progressive step input binding` | 2, 3 | M4: `StepBinding` with `PendingBinding`, group inputs, Job-creation evaluation of every evaluable group, `advance_step_binding`, `BindingError`, `binding_report`, `tasks_known`, `StepInputs`, `attach_step_inputs`, range rules (including the `INT`/`FLOAT` list-item flattening of A14), task-count limits |
| 5 | `feat(sessions): openjd_output_var directive` | 3, 4 | S1–S4, including the helper protocol and truncation changes, so the helper sub-crate's gates and the cross-user tests apply |
| 4a | `feat(model): job outputs` | 2, 4 | the M1 `jobOutputDefinitions` schema, the M2 job output scope, and M4 item 9 |
| 6 | `feat(cli): simulate progressive step input binding in openjd run` | 4, 4a, 5 | C1 (advancing pending Steps after each Step, early failure, per-group reports), C2 |
| 7 | `test(cli): golden tests for the RFC sample templates` | 6 | the sample tests below |
| 7a | `docs: re-cite CONTROL_FLOW references to RFCs 0011, 0012, and 0013` | 7 | every RFC citation in code, doc comments, messages, help text, `specs/`, golden templates, and `DEVELOPMENT.md`, re-cited with the [citation map](#citation-map-from-the-single-rfc); any non-loop behavior change from the split-era spec edits (M5); the non-loop conformance slice |
| 8 | `feat(model): loop schema and template validation` | 7a | L1, L2: `StepEntry`, `StepLoop`, `LoopDefinition`, `LoopVariableDefinition`, `LoopOutputVariableDefinition`, the `JobTemplate` helpers, every template-validation rule of RFC 0013 §Validation, and job outputs that read loop outputs. No `job::` instantiation beyond what the types need. The loop `job_templates` conformance tests |
| 9 | `feat(model): loop instantiation, iterations, and the iteration boundary` | 8 | L3–L6 and the L7 sessions change: `job::Loop`, `LoopBinding`, loop binding in `create_job` and `advance_loop_binding`, `LoopProgress`, `start_iteration`, `advance_body_step_binding`, `evaluate_iteration_boundary`, `end_without_iterations`, `LoopError`, `add_loop_producer`, `expression_inputs`, `max_loop_value_bytes`, and `forward_twin` for `RawLoop.<V>` |
| 10 | `feat(cli): run loops in the openjd run simulator` | 9 | L8: iterations, `<Loop>[<N>].<Step>` naming, the flag rules, `--step-inputs` for loops, `--output json\|yaml`, `openjd summary`; the whole conformance set |
| 11 | `test(cli): golden tests for the RFC 0013 examples` | 10 | the loop golden tests below, a check that each RFC 0011 and RFC 0012 golden template still matches its RFC text, and the final conformance run |

PR 3 can proceed in parallel with 2. PR 5's directive work (S1–S3) can start after 3, but its S4 part needs
`job::Step::path_inputs` from PR 4. PR 7 can start early as a list of failing tests, and the tests turn
on as PR 6 lands. The loop PRs run one after another. PR 9 also changes openjd-sessions, in `session.rs` only, so
the sessions gates apply to it; the helper and the cross-user code are unchanged.

## Test strategy

The tests follow the repo's standard. Validation failures assert the path and the full message, and expression
failures assert the message, the expression, and the caret (`AGENTS.md`, Test Quality Standard).

**openjd-model** (`tests/integration/test_control_flow_*.rs`):

* Gating: each new field without the extension, `CONTROL_FLOW` without `EXPR`, and `CONTROL_FLOW` in an Environment
  Template.
* Structure: each `outputVariables` rule, including `LIST[LIST[INT]]` on a multi-Task Step, outputs accepted on a
  `CHUNK[INT]` Step, the 32-item cap, `PATH` and `LIST[PATH]` accepted with `objectType` and rejected with `dataFlow`,
  `collect: APPEND` on a scalar type, and `unit` length and control characters.
* Scope: one test per `Steps.*` error message (M2), one per forbidden field family, a reference in a branch that is
  never taken, and a reference through a `let`.
* Types: scalar, list, and optional shapes, checked as `ExprType` on the seeded symbol, and `enableIf` given a `bool?`
  expression.
* Parser and collector: every row of RFC 0012's conversion table, each "invalid if" rule of RFC 0012 §Processing a
  directive that the model owns, both forms of the directive, leading whitespace, whitespace before `=` rejected,
  whitespace after `=` accepted (`N= 1`, `N= @f`), an empty `@` path, `NaN` and `1e400`, file content with a BOM and
  line breaks; `ONCE` repeated and missing, `LAST` replacing a value and its size, `LAST` with an invalid intermediate
  value, `APPEND` with zero and several values, `item` and `maxLength` checked per element and `minLength` at exit, a
  list value under `APPEND` becoming one element, and the size limit for inline and file values. Chunked: a selector
  required, forbidden, and outside the chunk (each invalid), `[012]` matching 12, `ONCE` per Task, the size limit per
  Task, a missing value for one Task failing `finish`, a chunk of one Task still requiring the selector, a selector
  that overflows `i64` (a grammar error), and a `range: [5, 1, 3, 2]` chunked producer whose `Steps.P.Output` and
  `TaskParam` lists stay aligned in iteration order. Binding: duplicate `CHUNK[INT]` values in a Step with outputs
  fail (a literal list at validation, a computed range at Job creation and at binding). A chunked producer with a
  second Task parameter (`CHUNK[INT]` `[5, 1, 3, 2]` × `STRING` `[a, b]`, and × `STRING` `[a, a]`) keeps its outputs
  aligned with iteration order, through `ChunkTaskIndex`.
* Binding: group inputs directly and through a `let` (including a `let` read only by the script, which puts only
  the remaining group behind its producer); a consumer of a statically skipped producer, and a
  consumer of `TaskParam` only, both bound at creation, including when the consumer comes before the producer in the
  template; a static skip that never evaluates the Step's `let`; a dynamic skip; `fail(...)` at binding; an empty
  product space; empty with associated ranges; non-empty `INT` and `FLOAT` list-literal ranges with a list-valued item
  (`["{{Param.L}}"]`); a list-literal range that flattens to nothing; the 1024 cap lifted;
  `max_task_count` across Steps as their Tasks become known; and `TaskParam` alignment and iteration order under a
  `combination: "A * B"` with `B` varying fastest.
* Progressive binding (`test_control_flow_progressive.rs`), each asserting the `PendingBinding` after every
  `advance_step_binding` call: a Step whose range reads `P` and whose host requirements read `Q` gets Tasks known
  after `P` alone and host requirements known after `Q` alone, and the same with the order reversed; it is `Bound`
  only after both and the remaining group; a dynamic `enableIf` keeps every group `Waiting` (with each group's own
  `on` set) even when their inputs are concrete, and a range that reads no `Steps.*` and would `fail(...)` (guarded by
  a `Param.*`, because a bare `{{fail('x')}}` range is rejected at validation) is not evaluated at creation; with
  `enableIf` false the Step is `Skipped` though its range would fail; with `enableIf` true, a failing range returns
  `Err` naming the group, field, and expression while the remaining group still waits; a failing range that reads no
  `Steps.*` in a pending Step aborts `create_job` with its error, and a failing carried-forward check in a pending
  Step's remaining group is reported together with another Step's errors; a `CONTROL_FLOW` variant of
  `failed_step_level_let_still_checks_later_steps`, in which the `let` read by the range fails, reports the `let`
  error and other Steps' errors but not the same Step's step-environment error; an `enableIf` that reads only a
  producer skipped at creation is decided at creation (a `null` there fails `create_job`); without `CONTROL_FLOW`, a
  template with both a bad host requirement and a bad range still reports the host requirement; a call with no new
  inputs returns the Step unchanged; a consumer's Tasks become known from the `TaskParam` of a producer that is still
  pending with Tasks known; the Task-count limit fails the Step when its Tasks become known; and a `let` used only by
  the remaining group is not evaluated when the Task enumeration group is.
* Job creation: a script that reads `Steps.*` passes `create_job`'s own carried-forward checks.
* `PATH` outputs: `PATH` and `LIST[PATH]` declarations; `Output` of a `PATH` producer in a TEMPLATE-scope field is
  rejected with the `RawOutput` hint, and `RawOutput` there is accepted; the collector maps a reported path back,
  rejects a relative one and one with no round trip, and checks constraints on the stored form.
* Job outputs: each schema rule; a `value` reading a Step that isn't a dependency of anything is accepted; `Param` of
  a `PATH` type rejected and `RawParam` accepted; a path-typed `Steps.P.Output.X` rejected with the `RawOutput`
  hint and `RawOutput` accepted; evaluation at creation, and a failure there failing `create_job`;
  an output reading only `TaskParam` of a Step whose Tasks are known at creation (evaluated at creation, including
  for a Step that is still pending), and one reading a
  statically skipped Step (evaluated at creation, `null`); `evaluate_job_output` for a later Step, a dynamically
  skipped Step (`null`), a zero-Task Step with `min([])` (error), a constraint violation, and a `null` result with a
  constraint (not checked). CLI: a run with a failing job output fails, and a run's success is reported after its
  last evaluable job output.

**openjd-sessions** (`tests/integration/test_control_flow.rs`):

* Values are returned on success and reported through the callback as they arrive, on stdout and on stderr.
  Nothing is returned on failure, cancellation, or timeout, or when the action exits 0 after an invalid directive.
* On a Windows worker with a POSIX submission path and a POSIX rule (or simulated with `to_symtab(Windows)` and
  `PathFormat::Windows`), a consumer's `RawOutput` keeps `/` and its `Output` is mapped. A script that reads only
  `Steps.P.RawOutput.X.name` gets a `path`, not a `string`.
* With path mapping rules, a `PATH` output is returned in the submission form; a consumer's script sees
  `Steps.P.Output.X` forward-mapped and `Steps.P.RawOutput.X` unmapped; a `LIST[PATH]` `TaskParam` of a `PATH`
  parameter is mapped element by element.
* Every collector failure surfaces as a failed Task with its message.
* Logging: with `echo_openjd_directives = false`, a valid directive is hidden and an invalid one stays in the log with
  its error; with the default, both appear.
* Cancel at once: an invalid directive followed by a long `sleep` cancels the Action well before the sleep ends,
  through its configured cancelation, with a message that names the output. A test that only checks the final state
  would also pass with fail-at-exit, so it must check the timing.
* Recognition: `openjd_output_var:N=1` with no space is a directive, and `openjd_output_var: ` with nothing after it
  fails the Task.
* `@` files: relative and absolute paths; a file written and closed before the directive; a symlink that stays
  inside the working directory succeeds; a file outside the working directory, a symlink out of it, a directory, and
  a missing file all fail; a file over the size limit fails without
  being read in full.
* The three treatments: a directive in `onRun` of a producer; in a Job Template `onEnter` and in the `onRun` of a Step
  without `outputVariables` (both fail); in an Environment Template action and in a Session without `CONTROL_FLOW`
  (ordinary output).
* A directive line over 64 KiB is invalid, on the direct path and through the helper, including a line under 64 KiB
  that grows past it when escaped.
* A directive line that is not valid UTF-8 is invalid, on both paths; a directive followed by a form feed is treated
  the same on both paths; an `@` file that is not UTF-8 or contains NUL fails the Task.
* An `@` FIFO fails the Task without hanging the reader, on both paths.
* Cross-user: an `@` file between 128 KiB and 1 MiB succeeds; an `@` directive in an Environment Template action is
  passed through unchanged and its file is not read; a directive in a Job Template `onEnter` cancels and fails the
  Action; an undeclared name with a missing `@` file reports the undeclared name.
* Chunked sessions: `run_task` passes the chunk values for a `CHUNK[INT]` Step and `None` otherwise; the output
  callback and `ActionResult.outputs` carry each Task's value; a chunk given as `ExprValue::String("10-12")` works,
  and an unparsable one fails the Action; cross-user parity for `N[3]=@f`.
* Cross-user parity: `@` directives with a space and with a tab after the colon, with `N= @f`, and with trailing
  spaces after the path give the same result as on the direct path. Run with the RFC 0011 samples' own form
  (`openjd_output_var: Keys=@keys.json`).
* WRAP_ACTIONS environment hooks: a directive from `onWrapEnvEnter` or `onWrapEnvExit` that wraps a Job Template
  Environment fails the Action, and one that wraps an Environment Template Environment is ordinary output.
* `LAST` with no directive fails the Task at exit.
* WRAP_ACTIONS: directives from `onWrapTaskRun` and from the wrapped process count for the Task, including when the
  wrapping Environment comes from an Environment Template.
* Cross-user (Docker, `scripts/run_cross_user_tests.sh`, and the Windows job): an `@` file that the job user can't
  read, including through a hard link into the working directory, fails the Task.

**openjd-cli golden tests**, one per sample of RFC 0011 and RFC 0012:

* Location: `crates/openjd-cli/tests/templates/control_flow/<sample>.yaml`, with one `<case>.expected.txt` per
  `openjd` command beside it (`<sample>.<variant>` for a variant).
* Each sample is the RFC YAML, verbatim where it is short enough to run. Its application commands (`ffmpeg`,
  `boto3`, `bake-node`, ...) are deterministic Python stubs that print a marker and write fixed outputs, found
  first on `PATH` and `PYTHONPATH`. The existing Python shim in `cli_tests.rs` supplies an interpreter.
* The test runs `openjd run`, normalizes timestamps and temporary paths, and compares the result with the expected
  file. `OPENJD_UPDATE_GOLDEN=1` rewrites the file. The helper is a few dozen lines in the test file, so the
  workspace gets no new dev-dependency such as `insta` (that would need `cargo deny`).
* Variants:
  * Video frame count: `FrameCount=3`, which gives 3 Tasks and a scalar `FrameRate` in a host-scope field.
  * Chunked render: RFC 0012's chunked `RenderTimeS[frame]` example (§Chunked Steps) with stub commands,
    `defaultTaskCount: 3`, and a variant that omits one frame's directive and fails its chunk.
  * Task metrics: a 2×1×1×1 space whose stub prints `frames_done` twice (`LAST`), three `chunk_error_mm` values
    (`APPEND`), and the three `ONCE` metrics, so the output shows each Task's values with their units. A second
    variant prints `mpjpe_mm` twice and fails the Task.
  * S3 fan-out: `Keys=[]`, which gives a zero-Task `Transcode` and a manifest with `totalBytes 0`, and a variant with
    two keys.
  * Sweep: a 2×1×1×2 space for a short runtime, run once with a winner and once taking the `fail(...)` path. The
    output shows `VideoCount`, `BestSingleTaskErrorMm`, and `ResultDirs` appearing after `Discover`, `Sweep`, and
    `RunWinner`, and a variant with path mapping rules shows `ResultDirs` in the submission form.
  * Scene graph: `openjd check` rejects `TASK_DEPENDENCIES` as unsupported. The verbatim sample fails earlier,
    on its unknown `dependsOnSubspace` field, so the extension check runs on a copy without that field. A variant without `dependsOnSubspace` runs
    the computed fan-out.
  * Render and QC: `EncodeVideo=false` (static skip, so `Publish` prints `frames only`), noise above and below the
    threshold (dynamic skip), and `BadFrames=[]` (zero Tasks).
  * Plus a `--step` run with `--step-inputs`, and a `--maximum-tasks` run that blocks the consumer.
  * Progressive binding (not an RFC sample, `control_flow/progressive.yaml`): a Step `C` whose range reads
    `Steps.P.Output.N` and whose host requirements read `Q`, where `Q` depends on `P`. The `openjd run` output shows
    `Tasks known` for `C` after `P` and `ready` after `Q`, and `openjd summary` shows `C`'s Tasks as determined at
    run time from `Steps.P.Output.N` and its host requirements as waiting for `Q`. A second template, whose range
    reads only a job parameter and whose step `let`, read only by its script, reads `Q`, shows in `openjd summary`
    a known Task count for a pending Step. Variants: the range `fail(...)`s (guarded by `P`'s value), which shows
    `C` failing after `P` and `Q` never running; and `C`'s `enableIf` reads `Q` and is false, which shows the
    failing range never evaluated and `C` skipped.

**Loops** (PRs 8–11), with the same assertion standard:

* Loop validation (`openjd-model`, `tests/integration/test_control_flow_loops_validation.rs`): one test per rule of
  L2 asserting path and message, including each `Loop.*` and `Steps.*` message; the seeded types of a loop's
  symbols for `until`, `while`, and `enableIf` loops, checked as `ExprType`; a template without loops whose errors
  and serialized `job::Job` are unchanged; and the step-count limit counting body Steps (A18).
* Loop binding and iterations (`tests/integration/test_control_flow_loops.rs`), one test per case:
  * `create_job`: a loop bound at creation; one pending on a dependency's output; a static skip that evaluates
    nothing else; `maxIterations` from a parameter, and 0 and 1,001 from a parameter failing `create_job`; an
    `initial` constraint violation and a first `while` that is `null`, each failing `create_job`; a body Step whose
    script would fail its carried-forward check, which `create_job` doesn't report (L3).
  * `start_iteration`: `Loop.*` concrete in every instance; an instance's range from a loop variable evaluated at
    iteration start; a group error failing the iteration; an instance pending on a body dependency and then bound by
    `advance_body_step_binding`; a path-typed variable recorded in `path_inputs` as `RawLoop.<V>`; a Task-count limit
    crossed by an instance (A19).
  * `evaluate_iteration_boundary`, mirroring `jobs/3.8.5--*`: `until` true skipping `next` (a `next` that would fail
    is not evaluated); `next` reading the old values (a swap); `while` reading the new values; the bound failing a
    loop that would continue, and not one whose condition ends it in the last allowed iteration; a `next` constraint
    violation at the final boundary of a `while` loop; `fail(...)` in `next` and in a loop output `value`; `LAST`
    replaced each iteration and checked each time; `APPEND` order, `item`, `maxLength`, and `minLength` at the end;
    a skipped body instance whose `null` makes a loop output `value` fail; zero iterations giving `null` and `[]`,
    and an `APPEND` `minLength` failing them; each error's message naming the field, expression, and iteration.
  * The 1 MiB check at each of its three points, with a small `max_loop_value_bytes`, including at `create_job`.
  * `add_loop_producer`: a consumer's range over an `APPEND` output, a `PATH` loop output read as `RawOutput`, and a
    job output from a loop output, including a skipped loop's `null`.
* Sessions: a `PATH` and a `LIST[PATH]` loop variable, under path mapping rules, give `RawLoop.<V>` in the stored form
  and `Loop.<V>` mapped (L7).
* Simulator (`run/simulator.rs` unit tests): instance naming; a body failure blocking the loop's dependents; an
  incomplete instance blocking only its readers, then the loop; `--step-inputs` for `--step L` and for a Step after a
  loop.
* CLI golden tests, under `control_flow/`, one per RFC 0013 example: render, check, and re-render until clean (a
  two-Step body with a fan-out); the iterative solver (a carried `FLOAT`); and the paginated listing (a `STRING`
  token and an `APPEND` `LIST[PATH]` that a Step after the loop fans out over). Variants: a zero-iteration `while`,
  `maxIterations` exceeded, a body Step failure, an error in `next`, a skipped loop, a job output from a loop output,
  `--step <loop>` with `--step-inputs`, each flag rejection of L8, and `openjd summary` for a loop.

**Conformance**: openjd-specifications at the target commit has a `conformance-tests/2023-09/CONTROL_FLOW/` set, in
the same format as `EXPR/jobs/*.test.yaml`, including the `7.4--*` progressive-binding tests and the loop tests
(`job_templates/1.1--loop-*`, `job_templates/3.8*`, and `jobs/3.8.5--*`). Each file's header cites the RFC (0011, 0012,
or 0013) that holds its rule. Its README marks the set unverified, because it was written before any implementation,
and none of the loop tests has been run. Run it against the prototype from PR 6 on, as the `AGENTS.md` conformance
slice does for the other extensions: from the openjd-specifications checkout, with this build's `openjd` first on
`PATH`, `uv run --with pyyaml python3.12 conformance-tests/run_openjd_cli_tests.py '2023-09/CONTROL_FLOW/*'`. PR 7a
runs the non-loop slice, PR 8 the loop `job_templates`, and PRs 10 and 11 the whole set. Raise any disagreement
between a test and an RFC on the RFC rather than changing the prototype to match the test.

**Python parity (Q7)**: not applicable until openjd-model-for-python implements the extension. The M3 parser and the
binding API are shaped so that a Python port can mirror them.

## RFC ambiguities surfaced

These are the questions the RFCs need to answer before an implementation can be called conformant. The prototype
picks a behavior for each and records it in the specs. A1–A17 were raised against the single RFC; each now names the
RFC that holds its text at `c3f583b`. A9, A10, and A12 were about the session-supplied output file, and the RFC
revision at `5a86c21` that replaced it with the directive settled them. A15 and A16 were settled in `1e89748` and
`61f27ee`, A11 in `a876d88`, and A17 in `66b6215`. A18 on are about loops (RFC 0013). Resolved items are kept, struck
through, so the numbering stays stable.

* **A1. The validation warning has no channel.** RFC 0011 §Modifications to the Expression Language says that
  template validation "MUST emit a warning" (RFC 0011 §Validation item 7). At `c3f583b` the warning also covers a
  `while` loop's `LAST` outputs, which are typed `T?` (RFC 0013 §`Steps.*` in and around a loop). No OpenJD
  specification, the conformance runner, nor openjd-rs has a warning concept. RFC 0011 should define where warnings
  appear (the decode result, `openjd check` output) and how they are tested, or change MUST to SHOULD. Prototype:
  deferred, for loop outputs too.
* **A2. Empty computed ranges are already accepted without `CONTROL_FLOW`.** RFC 0011 §Empty parameter spaces calls
  an empty list from a format-string range a relaxation that applies under `CONTROL_FLOW`. openjd-rs already accepts
  it with `EXPR` alone and runs zero Tasks. The EXPR specification should say whether that case is an error. If it
  is, openjd-rs has a bug, and fixing it breaks templates that run today. If it is not, the "relaxed rule" isn't new.
* **A3. Technical Requirement 1 conflicts with two relaxations.** RFC 0011 Technical Requirement 1 (TR1) says a
  template that lists `CONTROL_FLOW` but doesn't use a feature behaves exactly as today. Yet listing it lifts the
  1024-element cap on computed ranges (RFC 0011 §Range length), and, per RFC 0011's framing, it allows empty computed
  ranges. RFC 0011 §Backward compatibility counts only "one existing rule" as relaxed, but the range-length change is
  a second one. RFC 0011 Open Question 6 asks the related question; keying both relaxations on ranges that read
  `Steps.*` would satisfy TR1.
* **A4. Editorial: the Video example's `"1-0"` note.** RFC 0011 already records that openjd-rs rejects `"1-0"`
  (RFC 0011 §Empty parameter spaces, "some implementations reject it instead", and RFC 0011 Open Question 7). Its
  Video example's note says only that `"1-0"` is `[1]` under Template Schemas §3.4.1.1.1. It should mention the
  divergence too, because openjd-rs fails with `Descending range 1-0 requires a negative step`, and `minValue: 1` is
  the only guard that works everywhere.
* **A5. Task-count limits.** RFC 0011 §Limits item 1 allows a per-Step limit checked when the Step's Tasks become
  known. openjd-rs and services already have a per-Job total (`CallerLimits::max_task_count`) that is checked at
  creation. RFC 0011 should say whether a Job-total limit applies cumulatively as each Step's Tasks become known, so
  that the Task enumeration that crosses it fails the Step, and that Steps whose Tasks are not known yet count as
  zero. A19 asks the same of loops.
* **A6. JSON numbers coerced to `STRING`.** RFC 0012's conversion rule (§Processing a directive, Type conversion)
  makes any number outside `i64`, or with a fraction or exponent, a `float` before it is coerced.
  `S=12345678901234567890` then becomes `"1.2345678901234567e+19"` under EXPR coercion (checked with
  `ExprValue::coerce`), while the job-parameter path keeps the digits. RFC 0012 doesn't say whether `S=1.50` gives
  `"1.50"` or `"1.5"`. Specify whether a `STRING` output keeps the written digits (openjd-rs `Float64` can carry an
  original spelling, but M3 notes the parsing cost), and whether an out-of-range number such as `1e400` is an error.
* **A7. Which coercion, and how lenient (RFC 0012 Open Question 7).** RFC 0012 makes two statements that don't agree
  in openjd-rs. It converts with EXPR's implicit coercion (EXPR §1.2.3), and it also says a `Steps.*` value behaves
  exactly like a `Param.*` value of the same type. But job-parameter values go through a separate, more lenient
  conversion: `B=1` is a valid `BOOL` job parameter (`parameters.rs:691`, and `openjd run -p '{"B": 1}'` accepts it),
  while EXPR coercion rejects it (`Cannot coerce int to bool`). Both paths accept `N="12"` for `INT` and `B="yes"`,
  `"on"`, or `"1"` for `BOOL`. RFC 0012's table shows none of these cases. RFC 0012 should name one conversion and
  add these rows (or make output conversion stricter). The prototype uses EXPR coercion, which is RFC 0012's
  normative text.
* **A8. `enableIf` that is not a single whole-field expression.** RFC 0011 doesn't say how `enableIf: "yes"` (no
  braces) or `enableIf: "{{Param.X}} "` (text around the expression) resolves. EXPR's string-to-bool coercion would
  accept the first and reject `"true "`. Require a YAML `bool` or a single whole-field expression, or state the
  coercion. RFC 0013 gives a loop's `while` and `until` the same `<bool> | <FormatString>` form, so the answer applies
  to them too.
* ~~**A9. `Task.OutputFile` in TEMPLATE-scope action fields.**~~ Resolved: `Task.OutputFile` no longer exists.
* ~~**A10. The output file's integrity.**~~ Resolved: there is no session-supplied file. For `@` files, RFC 0012
  §File values now requires the opened file to be a regular file inside the Session working directory, checked on
  the handle, and read with no more access than the Action's user.
* ~~**A11. Step `let` forces `PENDING_INPUTS`.**~~ Resolved in `a876d88`: with progressive binding, a step `let`
  belongs to the groups that use it, and the remaining group uses every one, so a `let` that only the script reads
  holds back only the remaining group. Such a Step is still `PENDING_INPUTS` until its producer finishes (every
  `let` is evaluated before dispatch, as today), but its Tasks and host requirements are known at creation, so
  `openjd summary`'s Task count no longer changes.
* ~~**A12. Editorial: `<BlankLine>`.**~~ Resolved: the line grammar that used it is gone.
* **A13. Nested-list fan-in and `repr_sh`.** A per-Task `LIST[T]` output from a multi-Task producer becomes
  `list[list[T]]` (RFC 0011 §Reductions item 4), and `repr_sh` has no signature for nested lists, while `repr_py`,
  `repr_json`, and `flatten` do. A short note in RFC 0011 §Reductions ("`flatten` first for shell use") would spare
  authors a validation error.
* **A14. Flattening of list-valued items in `INT` and `FLOAT` range lists.** RFC 0011 §Empty parameter spaces says
  that after a list literal's items are evaluated, "EXPR flattens a list-valued item", which matches the Expression
  Language's list item rule (EXPR §1.3.2: target type `T? | list[T]`, and a list result "is flattened inline").
  openjd-rs does this only for `STRING` and `PATH` ranges (`ranges.rs:500`–`:522`). An `INT` list literal accepts
  only integers: `range: ["{{Param.L}}"]` with a `LIST[INT]` parameter fails `openjd check` with
  `Cannot parse '{{Param.L}}' as integer`. A `FLOAT` list literal parses each item's text as one float
  (`ranges.rs:407`), so with a `LIST[FLOAT]` parameter it passes `check` and then fails `openjd run` with
  `Cannot parse '[1.5, 2.5]' as float`. This is mainly an openjd-rs gap against the EXPR spec (Q5), to be fixed before
  M4.6. RFC 0011 should still say explicitly that range list items follow EXPR §1.3.2, because the EXPR §1.3.12 table
  types the `INT` range as `list[int | FormatString] | RangeString` and doesn't say that an item may be a list.
* ~~**A15. Is a directive line logged?**~~ Resolved in `1e89748`, refined in `61f27ee` (now RFC 0012 §The
  `openjd_output_var` directive): like the other messages, the line appears in the log by default; a runtime option
  to hide recognized messages may hide a valid directive, but an invalid one always stays in the log with its error.
  openjd-rs's `echo_openjd_directives` gives the first part, and S2 adds the second.
* ~~**A16. Cancel at once, or fail at exit?**~~ Resolved in `1e89748` (now RFC 0012 §Processing a directive): an
  invalid directive MUST cancel and fail its Action at once.
* ~~**A17. `TaskParam` read from a producer that then fails.**~~ Resolved in `66b6215`: a job output is evaluated
  as soon as every symbol it reads is concrete, including a `TaskParam` that is concrete because `P`'s Tasks are
  known before `P` is bound, and once evaluated it stays committed even if `P` later fails; the Job then fails as it
  otherwise would (RFC 0011 §Job outputs item 4). Which job outputs of a failed Job have been evaluated depends on
  evaluation timing and is not specified (RFC 0011 Design Choice 10). Prototype: keep the value (M4 item 9, C1
  item 7).
* **A18. Does a loop count toward a limit on the number of Steps?** RFC 0013 §Limits item 3 says a body Step counts
  once as a Step of the template wherever Steps are counted, and that its instances do not. It doesn't say whether
  the loop entry itself counts. openjd-rs checks a caller limit on `jt.steps.len()`
  (`validate_v2023_09/structure.rs:33`), which with loops would count a loop and none of its body Steps. Prototype:
  count every top-level Step, every loop, and every body Step once.
* **A19. Job-total Task limits across iterations.** RFC 0013 §Limits item 2 applies RFC 0011's per-Step limit to each
  body Step instance and lets a scheduler limit a loop's total Tasks or its number of instances, and says these
  failures are final. It doesn't say how a Job-total limit such as `CallerLimits::max_task_count` (A5) treats a
  loop, whose Tasks are not known at Job creation. Prototype: each instance's Tasks are added to the committed count
  when its Tasks become known, and an instance that would cross the limit fails, which fails the loop finally, as RFC
  0013 §Limits item 2 says of a loop-total limit.
* **A20. `Step.Name` in a body Step instance.** RFC 0013 says what `Step.Name` is in a loop's `enableIf` (the loop's
  name), and names a body Step instance `<Loop>[<N>].<Step>` only "in this RFC". It doesn't say what `Step.Name` is
  in a body Step's fields: the body Step's name, or something that includes the iteration. Prototype: the body Step's
  own name, the same in every iteration, as `Steps.<Name>` uses; a script that needs the iteration reads
  `Loop.Iteration`.
* **A21. What the 1 MiB loop total counts.** RFC 0013 §Limits item 4 counts "loop variable and loop output values
  ... at any moment" and checks the total after boundary step 3 (`next`). It doesn't say whether the values of
  iteration `N`, which `next` replaces, still count then. (`Loop.Iteration` is not a declared loop variable, so it
  doesn't count.) Prototype: after step 3 the total is the new variable values plus every loop output value so far
  (the current `LAST` value and every `APPEND` element), each measured as the JSON serialization of its stored form.

## Citation map from the single RFC

Where each part of the single `rfcs/0011-control-flow.md` at `d19cd9f` is at `c3f583b`. RFC 0011 kept its number, so
a citation of "RFC 0011" written before the split may point at RFC 0011, RFC 0012, or nothing. PR 7a uses this map;
check each citation against the RFC text rather than replacing blindly.

**Sections.**

| At `d19cd9f` | At `c3f583b` |
|---|---|
| Overview, Key concepts, Evaluation stages, Step states | RFC 0011, same names |
| Basic Examples: Video frame count, S3 prefix fan-out, Parameter sweep, Scene-graph DAG, Render and quality check | RFC 0011 §Basic Examples |
| Basic Examples: Task metrics from a parameter sweep | RFC 0012 §Basic Examples |
| Motivation, Use cases, Backward compatibility, Technical Requirements | RFC 0011 (RFC 0012 and RFC 0013 add their own) |
| Extension name and dependencies | RFC 0011 |
| Terminology | RFC 0011, except *output directive*, *collect mode*, and *submission path context*, which moved to RFC 0012 §Terminology; RFC 0013 adds loop terms |
| Schema: Job Template, `<StepTemplate>` (`enableIf`), `<JobOutputDefinition>`, `<StepDependency>`, Task parameter ranges, Empty parameter spaces, Range length, `<Environment>` | RFC 0011, same names |
| Schema: `<StepTemplate>` (`outputVariables`), `<OutputVariableDefinition>` | RFC 0012 §`<StepTemplate>`, §`<OutputVariableDefinition>` |
| Format strings, The `Steps.*` scope, Template processing stages | RFC 0011 (RFC 0013 §`Steps.*` in and around a loop modifies the scope) |
| Output emission: The `openjd_output_var` directive, File values, Collect modes, Chunked Steps, Processing a directive (with Type conversion), Path values, Committing values | RFC 0012, same names |
| Limits | items 1–3: RFC 0012 §Limits items 1–3; item 4: RFC 0011 §Limits item 1; item 5: RFC 0011 §Limits item 2 |
| Retries and re-runs | RFC 0012 §Retries and re-runs, which gained a general rule, extended rule 4 (a rejected re-run, and the Task keeps its state and values), and added a note on chunks that mix succeeded and new Tasks (M5) |
| Modifications to the Expression Language, Step input binding (Materialization expressions, Materialization groups, How a Step is bound, When groups are evaluated, Implementation strategy), Conditional execution: `enableIf`, Reductions, Job outputs | RFC 0011, same names |
| Modifications to How Jobs Are Run: Step states, Job output evaluation | RFC 0011 (Step states gained a re-run line, M5) |
| Modifications to How Jobs Are Run: Path mapping of output values, Stdout messages | RFC 0012, same names |
| Composition with `TASK_DEPENDENCIES` | RFC 0011 |
| Validation | see below |
| Prior Art, Implementation Impact, Rejected Ideas, Open Questions, Future Work | split by topic over the three RFCs |
| Future Work: Loop Steps | replaced by RFC 0013 |

**Design Choices.**

| At `d19cd9f` | At `c3f583b` |
|---|---|
| 1. Variable data flow is part of CONTROL_FLOW | RFC 0011 Design Choice 1 |
| 2. Outputs are reported with a stdout directive | RFC 0012 Design Choice 1 |
| 3. Repeated reports follow a declared collect mode | RFC 0012 Design Choice 2 |
| 4. Task metrics are output variables | RFC 0012 Design Choice 3 |
| 5. A new `Steps` root | RFC 0011 Design Choice 2 |
| 6. Only direct dependents may read a Step's outputs | RFC 0011 Design Choice 3 |
| 7. Output shape is determined by the template | RFC 0011 Design Choice 4 |
| 8. A skipped Step satisfies ordering and yields `null` data | RFC 0011 Design Choice 5 |
| 9. An empty computed parameter space is a zero-Task success | RFC 0011 Design Choice 6 |
| 10. Reductions are ordinary EXPR over ordered lists | RFC 0011 Design Choice 7 |
| 11. Limits are enforced at the producer | RFC 0012 Design Choice 4 |
| 12. Task-task dependencies are a separate extension | RFC 0011 Design Choice 8 |
| 13. Job outputs are expressions over Step outputs | RFC 0011 Design Choice 9 |
| 14. `PATH` values are stored in the submission path context | RFC 0012 Design Choice 5 |
| 15. Step input binding is progressive | RFC 0011 Design Choice 10 |
| 16. Design tenets | RFC 0011 Design Choice 11 |

**Open Questions.**

| At `d19cd9f` | At `c3f583b` |
|---|---|
| 1 Emission channel, 7 Emission rules, 8 Chunked producers, 9 Limits | RFC 0012 Open Questions 1, 2, 3, 4 |
| 13 Re-runs, 15 `@` file encoding, 16 Coercion strictness, 17 Number of outputs | RFC 0012 Open Questions 5, 6, 7, 8 |
| 19 Collect modes, 20 Uncommitted values, 21 Unmapped paths | RFC 0012 Open Questions 9, 10, 11 |
| 2 Namespace, 3 Direct dependents, 4 Output shape, 5 Correlating, 6 Fields | RFC 0011 Open Questions 1, 2, 3, 4, 5 |
| 10 Range length, 11 Empty parameter spaces, 12 `enableIf`, 14 `TASK_DEPENDENCIES` | RFC 0011 Open Questions 6, 7, 8, 9 |
| 18 Standalone StepTemplates, 22 Job outputs, 23 Progressive binding | RFC 0011 Open Questions 10, 11, 12 |

**Validation**, "At template validation".

| At `d19cd9f` | At `c3f583b` |
|---|---|
| 1 (`EXPR` prerequisite) | RFC 0011 item 1 |
| 2 (*outputVariables*) | RFC 0012 item 1 |
| 3–10 (`Steps.*` names, direct dependency, fields, *enableIf*, type checks, the warning, acyclic graph, *jobOutputDefinitions*) | RFC 0011 items 2–9 |

The other stages: "At step input binding" stays in RFC 0011 except the distinct `CHUNK[INT]` values, which moved to
RFC 0012 §Validation, "At step input binding". "At job output evaluation" is RFC 0011's; "As Actions run" and "On
re-run" are RFC 0012's. RFC 0013 §Validation adds the loop rules (L2) and the run-time loop checks (L3–L5).

Unchanged: Technical Requirements 1–12 (RFC 0011), `<OutputVariableDefinition>` items (RFC 0012), the invalid-directive
items 1–11 of Processing a directive (RFC 0012), Job outputs items 1–8 and Reductions items 1–5 (RFC 0011).
