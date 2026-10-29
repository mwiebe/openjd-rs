# Template Types

The `template` module contains types deserialized directly from YAML/JSON templates. These are
"unresolved" — `FormatString` fields have not been evaluated, parameter values have not been
substituted, and syntax sugar (like `SimpleAction`) has not been expanded.

All types use `#[serde(rename_all = "camelCase", deny_unknown_fields)]` for strict deserialization.
Section references (§) refer to the
[2023-09 Template Schemas](https://github.com/OpenJobDescription/openjd-specifications/wiki/2023-09-Template-Schemas).

## Constrained String Types (§7)

Three string types enforce spec constraints at deserialization time via custom `Deserialize` impls:

| Type | Pattern | Length | Usage |
|------|---------|--------|-------|
| `Identifier` | `[A-Za-z_][A-Za-z0-9_]*` | 1–512 | Parameter names, embedded file names |
| `Description` | Unicode except Cc control chars (allows `\n`, `\r`, `\t`) | 0–2048 | Description fields |
| `ExtensionName` | `[A-Z_0-9]{3,128}` | 3–128 | Extension names in `extensions` list |

These types implement `Deserialize`, `Serialize`, and `Display`. Validation happens during
deserialization — invalid values produce serde errors before the validation pipeline runs.

## Root Templates

### JobTemplate (§1.1)

```rust
pub struct JobTemplate {
    pub specification_version: String,
    pub schema: Option<String>,                              // $schema
    pub extensions: Option<Vec<ExtensionName>>,
    pub name: FormatString,
    pub description: Option<Description>,
    pub parameter_definitions: Option<Vec<JobParameterDefinition>>,
    pub job_environments: Option<Vec<Environment>>,
    pub steps: Vec<StepTemplate>,
}
```

Helper: `parameter_definitions_list()` returns `&[JobParameterDefinition]`, defaulting to
an empty slice when `parameter_definitions` is `None`.

### EnvironmentTemplate (§1.2)

```rust
pub struct EnvironmentTemplate {
    pub specification_version: String,
    pub schema: Option<String>,                              // $schema, ignored (§1.2 item 2)
    pub extensions: Option<Vec<ExtensionName>>,
    pub parameter_definitions: Option<Vec<JobParameterDefinition>>,
    pub environment: Environment,
}
```

## StepTemplate (§3)

> **Note:** Step names are plain `String`, not `Identifier` or `FormatString`.
> They accept any Unicode except Cc control characters — unlike parameter names
> and environment names which are constrained to `[A-Za-z_][A-Za-z0-9_]*` via
> the `Identifier` type. This is per the OpenJD specification §3.1 `<StepName>`.

```rust
pub struct StepTemplate {
    pub name: FormatString,
    pub description: Option<Description>,
    pub let_bindings: Option<Vec<String>>,           // "let" field in YAML
    pub dependencies: Option<Vec<StepDependency>>,
    pub step_environments: Option<Vec<Environment>>,
    pub host_requirements: Option<HostRequirements>,
    pub parameter_space: Option<StepParameterSpaceDefinition>,
    pub script: Option<StepScript>,
    // SimpleAction syntax sugar (FEATURE_BUNDLE_1)
    pub bash: Option<SimpleAction>,
    pub python: Option<SimpleAction>,
    pub cmd: Option<SimpleAction>,
    pub powershell: Option<SimpleAction>,
    pub node: Option<SimpleAction>,
}
```

### SimpleAction (FEATURE_BUNDLE_1)

Syntax sugar that expands into a `StepScript` with an embedded file and `onRun` action
(Template Schemas §8). A step must have either `script` or exactly one simple action
field — never both.

```rust
pub struct SimpleAction {
    pub let_bindings: Option<Vec<String>>,
    /// `<DataString>`, `@fmtstring[host]` — parsed at deserialization, so
    /// malformed `{{ ... }}` fails at parse time like any other format string.
    pub script: FormatString,
    pub args: Option<Vec<FormatString>>,
    pub timeout: Option<FormatString>,
    pub cancelation: Option<CancelationMode>,
}

/// Which interpreter key the SimpleAction was written under.
pub enum SimpleActionKind { Python, Bash, Cmd, Powershell, Node }

impl SimpleActionKind {
    pub const ALL: [SimpleActionKind; 5];          // the order resolve_syntax_sugar considers them
    pub fn field_name(self) -> &'static str;       // "bash", … — also the desugared `command`
    pub fn file_extension(self) -> &'static str;   // ".sh", ".py", ".bat", ".ps1", ".js"
    pub fn arg_prefix(self) -> &'static [&'static str]; // ["/C"] for cmd, ["-File"] for powershell, else []
    pub fn synthetic_arg_count(self) -> usize;     // arg_prefix().len() + 1 (the Task.File reference)
    pub fn desugar(self, step_name: &str, sa: &SimpleAction) -> StepScript;
    pub fn remap_desugared_path(self, rel: &[PathElement]) -> Vec<PathElement>;
}

impl StepTemplate {
    pub fn simple_actions(&self) -> impl Iterator<Item = (SimpleActionKind, &SimpleAction)>;
    pub fn simple_action(&self, kind: SimpleActionKind) -> Option<&SimpleAction>;
    /// `script.clone()` if present, else the first SimpleAction desugared; `None` if neither.
    pub fn resolve_syntax_sugar(&self) -> Option<StepScript>;
}
```

`desugar` builds the generated embedded file's name from the step name
(every character outside `[A-Za-z0-9]` → `_`, as in `openjd-model-for-python`;
at most 200 chars; `_`-prefixed if it would start with a digit; suffixed
`_script` — always a valid identifier, so the generated reference always
parses), sets `command` to the interpreter name,
and produces `args = [<arg_prefix>..., "{{Task.File.<name>}}", <authored args>...]`.

`remap_desugared_path` maps a validation-error path on the desugared
`StepScript` (relative to its `script` node) back onto the sugar field the
author wrote, so a diagnostic never names a node that does not exist in the
template — see `specs/model/validation.md` § Reporting on desugared forms
for the table. Validation (passes 6 and 8) and job creation all validate the
desugared form and report through this remap.

### StepDependency (§3.2)

```rust
pub struct StepDependency {
    pub depends_on: Option<String>,                          // dependsOn
    pub step: Option<String>,                                // ENV_PARAMS; synonym of dependsOn
    pub environment: Option<String>,                         // ENV_PARAMS
    pub required: Option<bool>,                              // ENV_PARAMS; absent = true
    pub enable_if: Option<ExprField>,                        // ENV_PARAMS; @expr bool; absent = enabled
    pub parameters: Option<EnvironmentParameterValueMap>,    // ENV_PARAMS
}

pub enum DependencyTarget<'a> {
    Step(&'a str, StepKey),   // the key as written, for messages
    Environment(&'a str),
}

pub enum StepKey {
    DependsOn,  // dependsOn
    Step,       // step (ENV_PARAMS)
}
```

An entry is `dependsOn: <StepName>`, or with ENV_PARAMS its synonym `step: <StepName>` (§3.2
item 1), or with ENV_PARAMS `environment: <EnvironmentName>`
with optional `required`, `enableIf` (§3.2 item 5), and `parameters`. Every key is optional in
the struct, so an entry
with more than one form or none decodes and pass 6 reports it at the entry's path (§3.2
constraint 1). `target()` returns the form, or `None` for such an entry (`is_well_formed()`
is `target().is_some()`). A `step` and a `dependsOn` entry are both `DependencyTarget::Step`,
differing only in the `StepKey`, which no rule reads: `step()` returns the Step name for
either, so self-dependency, unknown-Step, duplicate, cycle, and job-creation checks treat them
alike. `environment()` reads the other form; `is_required()` applies
the default; `literal_enabled()` is `Some(true)` for an absent `enableIf` or the boolean
scalar `true`, `Some(false)` for `false`, and `None` for any other value, an expression
included, even a constant one such as `"true"`, whose value job creation decides (§3.2
checked-at paragraph). `enableIf` is an expression field of type `bool`; see
[Expression fields](#expression-fields-75). `on_step(name)` and `on_environment(name)` build an entry, and
`DependencyTarget`'s `Display` writes it as a template does (`dependsOn: X`, `step: X`,
`environment: X`); `StepKey::as_str()` is `dependsOn` or `step`. These names follow the `service:` form on the `service-extension`
branch (`step()`, `service()`, `on_service()`, `is_well_formed()`), so adding that key later
is one field, one constructor, and one `DependencyTarget` variant.

The differences from that branch, for a later merge to reconcile:

- The struct derives `PartialEq` and `Eq` but not `Hash`, because a literal value is kept as
  a `serde_json::Value`, which has no `Hash` (nor has `ExprField`, which holds an `f64`). `service-extension` puts entries in a
  `HashSet<&StepDependency>`; that set must be keyed on `target()` instead.
- `service-extension` also has `StepDependency::describe()` (the entry as written, for
  messages, including a malformed one) and free functions over a dependency list
  (`listed_step_names`, `listed_service_names`, `lists_service`). This branch's messages do
  not need them, so they are not added here.

Job creation converts only the Step entries (`dependsOn` and `step`) to
`job::StepDependency`, whose one key is `dependsOn`, so a `step: X` entry is `dependsOn: X` in
the Job; an `environment:` entry does not delay scheduling (§3.2 item 2), so
it is not part of the Step dependency graph, and a list left empty by the filter becomes
`None`.

### Expression fields (§7.5)

```rust
pub enum ExprField {
    Bool(bool),          // a boolean scalar, used as written
    Int(i64),            // an integer scalar
    Float(f64),          // a number with a fraction or exponent
    Expression(String),  // a string: one expression, without {{ }}
    Null,                // a present null (`null`, `~`, or an empty key)
}
```

An expression field (`@expr <type>`) holds one Expression Language expression written
without `{{ }}`, or a boolean, integer, or float scalar used as written. `ExprField` is the
value as written; it knows nothing of the field's target type, which each field that uses it
supplies to the checker, so a later field (a Step `enableIf`, RFC 0011's loop fields) reuses
both unchanged. The fields that use it today are an `environment:` entry's `enableIf` and an
Environment's own `enableIf` (below), both of type `bool`.

Decoding is by hand and accepts every scalar, so that template validation, which knows the
target type, reports a scalar that does not fit (`enableIf: 1`) at the field's path with the
§7.5 rule, rather than serde reporting it without one. A string is always `Expression`, never
read as a scalar: `"true"` is the expression `true`. A list or a map is a decode error that
names the field ("enableIf must be a boolean, integer, float, or string (an expression field,
Template Schemas §7.5)"), as is an integer above `i64::MAX` that fits `u64` ("enableIf: the
integer N is out of range for an expression field …"). A larger integer, or one below
`i64::MIN`, arrives from JSON number parsing as a float and decodes as `Float`. Decode errors
carry no path, which is why the message names the field. A present `null` is an error, not an
omitted field (§7.5), so an optional `enableIf` is declared `#[serde(default, deserialize_with =
"expr_field::deserialize_enable_if")]`: an absent key is `None`, a present `null` is
`Some(ExprField::Null)`, and template validation rejects it at the field's path. (A later
expression field adds a `deserialize_with` that names it; `ExprField`'s own `Deserialize`,
which names no field, says "an expression field must be …" and, for a `u64`, "the integer N is
out of range for an expression field …". A plain `Option<ExprField>` reads a present `null` as
`None`, so every optional expression field needs such a `deserialize_with`.) `PartialEq` and
`Eq` are written by hand, comparing a float's bits (a decoded value is never NaN; `-0.0` and
`0.0` are unequal); `as_bool()` returns the boolean scalar, `None` otherwise.

The crate-private checker in `template/expr_field.rs` applies §7.5 for a target type `G`,
given the field's scope (a symbol table) and an `ExprFieldEval` (profile, library, budgets;
paths are always POSIX, since an expression field is evaluated in template scope, not a host
context: Template Schemas §3.2 item 5 and §4 item 6, spec `5535230`):

- `validate` (template validation, rules 1, 2, 4): a scalar must fit `G` (only `int` widens to
  `float`), a `null` is an error, and an expression must parse under the document's profile,
  evaluate against the scope (unknown names are errors), and have a static type that fits
  `G`. A parse error of a value that contains `{{` adds "An expression field takes an
  expression without '{{ }}' (Template Schemas §7.5)." after the message, before the
  expression and caret lines (". " before it, or " " after a summary that ends in `.`, `?`,
  or `!`). A float scalar is quoted in Rust's shortest round-trip form, which keeps a whole
  number's fraction (`3.0`; `1e3` is quoted `1000.0`), and a non-finite one (only a template
  built in code can hold one) is an error.
- `check_static_type` is the fit rule: `unresolved[U]` is read as `U`; where `G` admits
  `nulltype` (`nulltype`, `any`, or a choice with one) the type must satisfy `G`; otherwise
  `nulltype` is an error and `nulltype` members of a union are dropped (`U?` is accepted
  when `U` fits). Fitting is `ExprType::satisfies` (Expression Language §1.2.3 step 1), or
  `satisfies` after every `int` becomes `float`, element-wise.
- `resolve` (the field's stage) evaluates and returns the value: a `null` result is an error
  unless `G` admits `nulltype`, even on an optional field; an `int` result is widened to
  `float` where only that fits; any other result that does not fit is an error.
- `resolve_progressive` is `resolve` with a scope in which some symbols are bound to
  `unresolved[T]` (Expression Language progressive evaluation): a result that depends on one
  is `Ok(None)`, not checked further (template validation has checked its static type), and a
  known result is checked as `resolve` checks it. An Environment's own `enableIf` uses it at
  job creation, with the Step's missing or unconvertible values unknown (Template Schemas
  §3.2.2).

The checker lives in openjd-model because it is template-field policy built on
openjd-expr's public type API (`satisfies`, `code`, `params`, `ExprValue::coerce`);
openjd-expr is unchanged.

### Environment parameters (§4.5, ENV_PARAMS)

```rust
pub struct EnvironmentParameterDefinition {
    pub definition: JobParameterDefinition,      // its `default` is always None
    pub default: Option<EnvironmentParameterValue>,
}

pub enum EnvironmentParameterValue {
    FormatString(FormatString),                  // a string containing `{{`
    Literal(serde_json::Value),                  // anything else, as written
}

pub struct EnvironmentParameterValueMap(pub BTreeMap<String, EnvironmentParameterValue>);
```

An `<EnvironmentParameterDefinition>` has the shape of a `<JobParameterDefinition>` of the
same type, but its `default` is a format string resolved at job creation, which the Job
Parameter types' typed `default` fields cannot hold. The deserializer removes `default`,
decodes the rest as a `JobParameterDefinition`, and keeps `default` as an
`EnvironmentParameterValue`. An explicit `default: null` is no default, as it is for a Job
Parameter. Reusing the Job Parameter definition keeps `allowedValues`,
`minLength`, `item`, and the other constraints, their decoders, and `check_constraints`
in one place, and with them every field of that shape, which §4.5 permits: any type the
document allows, `RANGE_EXPR` included, and `userInterface`, `objectType`, and `dataFlow`,
with their decoders and the definition checks of `validate_definition`. Those three are
informational; nothing reads them after validation.

The same `EnvironmentParameterValue` holds a `default` and an `environment:` entry value,
since both are `@fmtstring` and the spec splits them the same way. A literal's validity
depends on the parameter's type, which is not known at decode, so it is kept as written and
checked in pass 11 (or, for an external Environment, at job creation against the attached
Environment Templates). The `parameters` map
is a `BTreeMap` keyed by plain strings: the workspace's `serde_json` has no
`preserve_order` feature, so a template map reaches the decoder sorted by key anyway, and a
plain string key lets pass 11 report a key that is not an `<Identifier>` at its own path.

## Environment (§4)

```rust
pub struct Environment {
    pub name: String,
    pub description: Option<Description>,
    pub env_parameter_definitions: Option<Vec<EnvironmentParameterDefinition>>,  // ENV_PARAMS; envParameterDefinitions
    pub activation: Option<EnvironmentActivation>,                              // ENV_PARAMS
    pub host_requirements: Option<HostRequirements>,                            // ENV_PARAMS
    pub enable_if: Option<ExprField>,                                           // ENV_PARAMS; @expr bool; absent = enabled
    pub script: Option<EnvironmentScript>,
    pub variables: Option<HashMap<String, FormatString>>,
}

pub enum EnvironmentActivation {  // serde: SCREAMING_SNAKE_CASE
    Always,     // ALWAYS, the default
    OnRequest,  // ON_REQUEST
}
```

`activation()` applies the default (§4 item 4: absent means `ALWAYS`). An unrecognized value,
including one in another case (`always`) or a value from before the rename (`ALL`,
`ONLY_REQUESTED`), is a decode error. Pass 11 rejects the field without
ENV_PARAMS and on a `stepEnvironments` entry. `EnvironmentActivation::as_str` gives the value as written.

`host_requirements` (§4 item 5) is the Step's [`HostRequirements`](#host-requirements-33)
type, with the same per-source checks (pass 6) and its own format-string scope (pass 8; see
[validation.md](validation.md#environment-hostrequirements-env_params)). Pass 11 rejects it
without ENV_PARAMS and on a `stepEnvironments` entry. Job creation resolves it once per
entering Step and combines it with the Step's own (see
[job-creation.md](job-creation.md#combined-host-requirements-env_params)).

`enable_if` (§4 item 6) is an [expression field](#expression-fields-75) of type `bool`,
decoded as an entry's `enableIf` is, with a present `null` kept as `ExprField::Null`. It
decides whether a Step's Sessions enter the Environment, and unlike `activation`,
`hostRequirements`, and `envParameterDefinitions` it is permitted on every position, a
`stepEnvironments` entry included. Pass 11 requires ENV_PARAMS in the document (which
requires EXPR; see [validation.md](validation.md#entry-points)), and pass 8 checks it in its
scope (see [validation.md](validation.md#environment-enableif-env_params)). `literal_enabled()` is
`Some(true)` for an absent `enableIf` or the boolean scalar `true`, `Some(false)` for the
scalar `false`, and `None` for anything else, a constant expression such as `"true"` included:
what template validation may count as entered or not (§4 item 6 tail; §7.5 item 1). Job
creation resolves it once per Step whose Sessions would otherwise enter the Environment (see
[job-creation.md](job-creation.md#environment-enableif-env_params)); no job type records it,
since a disabled Environment is simply left out of the Step's sets.

### EnvironmentScript (§4.1)

```rust
pub struct EnvironmentScript {
    pub let_bindings: Option<Vec<String>>,
    pub actions: EnvironmentActions,
    pub embedded_files: Option<Vec<EmbeddedFile>>,
}
```

### EmbeddedFile (§6)

```rust
pub struct EmbeddedFile {
    pub name: String,
    pub file_type: String,                    // "type" field; must be "TEXT"
    pub filename: Option<String>,             // Plain string (not @fmtstring)
    pub data: Option<FormatString>,
    pub runnable: Option<bool>,
    pub end_of_line: Option<String>,          // FEATURE_BUNDLE_1: "LF", "CRLF", "AUTO"
}
```

## Actions (§5)

```rust
pub struct Action {
    pub command: FormatString,
    pub args: Option<Vec<FormatString>>,
    pub cancelation: Option<CancelationMode>,
    pub timeout: Option<FormatString>,
}

pub struct StepActions {
    pub on_run: Action,
}

pub struct EnvironmentActions {
    pub on_enter: Option<Action>,
    pub on_exit: Option<Action>,
}
```

### CancelationMode

Discriminated union on the `mode` field, implemented as a Rust enum with a custom
`Deserialize` impl:

```rust
pub enum CancelationMode {
    Terminate,
    NotifyThenTerminate {
        notify_period_in_seconds: Option<FormatString>,
    },
    DeferredMode {
        mode: FormatString,
        notify_period_in_seconds: Option<FormatString>,
    },
}
```

The `Terminate` variant rejects any extra fields. The `NotifyThenTerminate` variant
accepts an optional `notifyPeriodInSeconds` field. An explicit JSON/YAML `null` for
`notifyPeriodInSeconds` is treated the same as omitting the field, matching the
Python implementation (pydantic `Optional`).

#### DeferredMode: why the mode decision can be deferred

Format strings in general are already delay-processed: when a template says
`args: ["{{WrappedAction.Command}}"]`, the parser just stores "this is a format
string" and the value gets resolved much later, inside a running session, right
before the action launches — that's when the runtime seeds the `WrappedAction.*`
variables from the action being wrapped. "Resolve later" is the normal pipeline
for every other field.

`mode` is different because it isn't a normal value field — it's the *schema
selector*. The parser needs to know TERMINATE vs NOTIFY_THEN_TERMINATE at parse
time to decide what shape of object it's even reading (only one of them allows
`notifyPeriodInSeconds`). So the "which shape?" decision happens at parse time,
but a forwarded value like `mode: "{{WrappedAction.Cancelation.Mode}}"` only
exists at run time — that mismatch made round-trip cancelation forwarding in
RFC 0008 wrap hooks impossible (the parser rejected the template with "unknown
variant").

`DeferredMode` resolves the mismatch: the parser accepts a format string in
`mode` as a third, "decided later" state (gated on the FEATURE_BUNDLE_1
extension), and the shape decision moves to resolution time, right before the
action runs:

1. The runtime seeds `WrappedAction.Cancelation.Mode` from the wrapped action
   (`"TERMINATE"`, `"NOTIFY_THEN_TERMINATE"`, or null).
2. It resolves the `mode:` expression against that.
3. `"TERMINATE"`/`"NOTIFY_THEN_TERMINATE"` — the cancelation block now acts as
   that method, and its sibling fields are validated against that shape. Null
   (whole-field expressions only) — the whole `cancelation:` block is treated
   as never written. Anything else — the action fails.

Static validation is *not* deferred: at parse time the validator still checks
the expression is well-formed and that `WrappedAction.*` is only referenced
inside wrap hooks. Any format string is accepted — normal interpolation like
`"{{Prefix}}_THEN_TERMINATE"` is permitted; only the resolved value is
constrained. You just can't know *which* of the two modes it'll be until the
wrapped action is in front of you — which is inherent to forwarding: the same
wrap environment gets reused across many steps whose cancelation settings
differ.

The run-time resolution lives in `openjd-sessions`
(`resolve_effective_cancelation` in `runner/mod.rs`). See openjd-specifications
Template Schemas §5.3 and RFC 0008 "Cancelation behavior" for the normative
rules.

## StepScript (§3.5)

```rust
pub struct StepScript {
    pub let_bindings: Option<Vec<String>>,
    pub actions: StepActions,
    pub embedded_files: Option<Vec<EmbeddedFile>>,
}
```

## Host Requirements (§3.3)

```rust
pub struct HostRequirements {
    pub amounts: Option<Vec<AmountRequirement>>,
    pub attributes: Option<Vec<AttributeRequirement>>,
}

pub struct AmountRequirement {
    pub name: FormatString,
    pub min: Option<FormatString>,
    pub max: Option<FormatString>,
}

pub struct AttributeRequirement {
    pub name: FormatString,
    pub any_of: Option<Vec<FormatString>>,
    pub all_of: Option<Vec<FormatString>>,
}
```

`name` is `@fmtstring` (§3.3.1 / §3.3.2): it is resolved at job creation, and
the §3.3.1.1 / §3.3.2.1 constraints apply to the resolved name. The job types
carry the resolved name as a `String`.

## Task Parameter Space (§3.4)

### StepParameterSpaceDefinition

```rust
pub struct StepParameterSpaceDefinition {
    pub task_parameter_definitions: Vec<TaskParameterDefinition>,
    pub combination: Option<String>,
}
```

### TaskParameterDefinition

Discriminated union via `#[serde(tag = "type")]`. Variant names use SCREAMING_CASE to
match the serde tag values directly, with `#[serde(rename = "CHUNK[INT]")]` on `CHUNK_INT`
since brackets aren't valid in Rust identifiers:

| Variant | Type Field | Range Type | Extra Fields |
|---------|-----------|------------|-------------|
| `INT` | `"INT"` | `IntRange` | — |
| `FLOAT` | `"FLOAT"` | `FloatRange` | — |
| `STRING` | `"STRING"` | `StringRange` | — |
| `PATH` | `"PATH"` | `StringRange` | — |
| `CHUNK_INT` | `"CHUNK[INT]"` | `IntRange` | `chunks: ChunksDefinition` |

### Range Types

Ranges accept either a list of values or a range expression string:

```rust
pub enum IntRange {
    List(Vec<FlexInt>),
    Expression(FormatString),
}

pub enum StringRange {
    List(Vec<FormatString>),
    Expression(FormatString),
}

pub enum FloatRange {
    List(Vec<FloatRangeItem>),
    Expression(FormatString),
}
```

`FloatRange::List` uses `FloatRangeItem` — an enum that accepts either a plain `f64` or
a `FormatString` — to handle YAML float edge cases and format string interpolation in
float ranges:

```rust
pub enum FloatRangeItem {
    Float(f64),
    FormatString(FormatString),
}
```

### ChunksDefinition

```rust
pub struct ChunksDefinition {
    pub default_task_count: IntOrFormatString,
    pub target_runtime_seconds: Option<IntOrFormatString>,
    pub range_constraint: RangeConstraint,  // Required field
}

pub enum IntOrFormatString {
    Int(i64),
    FormatString(FormatString),
}

pub enum RangeConstraint {
    Contiguous,
    Noncontiguous,
}
```

## Flexible Deserialization Types

Several wrapper types handle YAML's flexible value representations:

| Type | Accepts | Rejects | Purpose |
|------|---------|---------|--------|
| `FlexInt(i64)` | Integers, floats with `.0`, strings of integers | Bools, nulls | INT parameter defaults/constraints |
| `FlexFloat(f64, Option<String>)` | Numbers, string representations | Bools, nulls | FLOAT parameter defaults/constraints |
| `FlexUint(u64)` | Non-negative integers, string representations | Negatives, bools | Timeout values |
| `BoolValue(bool)` | `true`/`false`, `0`/`1`, `"yes"`/`"no"`, `"on"`/`"off"` | Other strings | BOOL parameter defaults |
| `NullableVec<T>` | Absent field, list of T | Explicit `null` | INT/FLOAT `allowedValues` |

`FlexFloat` preserves the original string representation when parsed from a string, which
is needed for round-trip fidelity in constraint checking.

`NullableVec` exists because the spec distinguishes between an absent `allowedValues` field
(no constraint) and an explicit `null` (invalid). Serde's `Option<Vec<T>>` would accept both.
