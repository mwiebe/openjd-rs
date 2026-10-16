# Instantiated Job Types

The `job` module contains types produced by `create_job()`. These represent a fully instantiated
job where template-scope format strings have been resolved to concrete values. Session-scope and
task-scope fields remain as `FormatString` for evaluation at runtime.

All types derive `Debug, Clone, Serialize` with `#[serde(rename_all = "camelCase")]`.

## Resolution Scopes

The OpenJD specification defines three resolution scopes that determine when format strings
are evaluated:

| Scope | When Resolved | Examples |
|-------|--------------|----------|
| TEMPLATE | At `create_job()` time | Job name, step names, host requirement values, parameter space ranges, step-level let bindings |
| SESSION | At session start | Environment variables, environment script commands, embedded file data |
| TASK | At task execution | Step script commands/args, step embedded file data |

Template-scope fields become concrete `String`/`f64`/`i64` values in the `job::*` types.
Session and task-scope fields remain as `FormatString` because they depend on runtime context
(e.g., `Session.WorkingDirectory`, `Task.Param.*`).

One exception separates symbol scope from resolution time: action `timeout` and
`notifyPeriodInSeconds` validate in TEMPLATE scope (only job-creation-stage symbols may be
referenced — no `Session.*`/`Task.*`/`Env.File.*`), yet they remain `FormatString` in the
`job::*` types and resolve on the worker alongside the host-context fields.

## Type Definitions

### Job

```rust
pub struct Job {
    pub name: String,                                    // Resolved from FormatString
    pub description: Option<String>,
    pub extensions: Option<Vec<String>>,
    pub parameters: IndexMap<String, JobParameter>,      // Insertion-ordered
    pub steps: Vec<Step>,
    pub job_environments: Option<Vec<Environment>>,
}
```

`parameters` uses `IndexMap` (not `HashMap`) to preserve insertion order for deterministic
output.

### JobParameter

```rust
pub struct JobParameter {
    pub name: String,
    pub param_type: JobParameterType,
    pub value: ExprValue,
}
```

Stores the final typed value as an `ExprValue` (from `openjd-expr`), preserving the full
type information. PATH values are stored as `ExprValue::String` because the source path
format may differ from the host path format. During the TEMPLATE scope, `Param.X` is not
accessible for PATH types — only `RawParam.X` is available (as a string). Later, in
SESSION and TASK scope, the session applies path mapping rules and `Param.X` becomes
available as `ExprValue::Path`.

### Step

```rust
pub struct Step {
    pub name: String,                                    // Resolved
    pub description: Option<String>,
    pub script: StepScript,
    pub step_environments: Option<Vec<Environment>>,
    pub parameter_space: Option<StepParameterSpace>,
    pub host_requirements: Option<HostRequirements>,
    pub dependencies: Option<Vec<StepDependency>>,
    pub resolved_symtab: Option<SerializedSymbolTable>,
    pub output_variables: Option<Vec<OutputVariable>>,   // CONTROL_FLOW
    pub binding: StepBinding,                            // CONTROL_FLOW
    pub path_inputs: Vec<PathInput>,                     // CONTROL_FLOW
}
```

The three `CONTROL_FLOW` fields (RFC 0010) are described in
[CONTROL_FLOW fields](#control_flow-fields-rfc-0010) below.

`resolved_symtab` exists to transport symbol values across the network to the worker host
that runs the job. The worker evaluates the format strings that remain unresolved after job
creation, so `resolved_symtab` is filtered to contain exactly the symbols referenced by
those format strings. For a Step, these are: the step script's actions (command, args,
timeout, cancelation), embedded files, script-level let bindings, step-level let bindings,
and any step-scoped environments' variables, actions, and embedded files. Most of these are
host-context (SESSION and TASK scope); timeout and notifyPeriodInSeconds are template scope
(validation rejects `Session.*`/`Task.*`/`Env.File.*` in them) but still resolve on the
worker, referencing only the job-creation-stage symbols carried in `resolved_symtab`.

Contents include `RawParam.*`, non-PATH `Param.*` values, `Job.Name`, `Step.Name`, and
let bindings. PATH-typed `Param.*` entries and any `apply_path_mapping` results are excluded
because path mapping rules aren't available until session time. The session layers these
plus `Session.*` and `Task.*` values on top at runtime.

The type is `SerializedSymbolTable` (not `SymbolTable`) — a wire-format type serialized as
`[{"name": str, "value": ..., "type": str}]` for cross-host transfer in a Python-compatible
format.

### CONTROL_FLOW fields (RFC 0010)

`job::Step` derives `PartialEq`, `Hash`, `Serialize`, and `Deserialize`. The
template's `JobParameterDefinition` implements none of the first three, so an output
declaration is carried on the job in its own form.

```rust
pub struct OutputVariable {
    pub name: String,
    pub param_type: JobParameterType,     // serialized as "type": the spec string, "LIST[INT]"
    pub collect: OutputCollectMode,
    pub unit: Option<String>,
    pub constraints: OutputConstraints,
}

pub struct OutputConstraints {
    pub allowed_values: Option<OutputAllowedValues>,
    pub min_value: Option<OutputBound>,
    pub max_value: Option<OutputBound>,
    pub min_length: Option<usize>,
    pub max_length: Option<usize>,
    pub item: Option<Box<OutputConstraints>>,
}
pub enum OutputAllowedValues { Int(Vec<i64>), Float(Vec<Float64>), String(Vec<String>) }
pub enum OutputBound { Int(i64), Float(Float64) }

pub enum StepBinding {
    Bound,                                     // default
    PendingInputs { waiting_on: Vec<String> }, // sorted when binding writes it
    Skipped,
}

pub struct PathInput {
    pub symbol: String,
    pub ty: openjd_expr::ExprType,             // serialized as "type": "path", "list[path]", ...
}
```

Deserialization does not re-check what binding guarantees when it writes these values:
`waiting_on` is kept in the order given, and `PathInput.ty` may be any type its spelling
parses to.

`create_job` fills `output_variables` from the template's `outputVariables`
(`OutputVariableDefinition::to_job_output_variable`), sets `binding` to `Bound`, and
leaves `path_inputs` empty. Step input binding, which a later change adds, produces the
other two `StepBinding` states and records `path_inputs`. The plan (M1) defines these
fields together with the schema, so callers that build or match `job::Step` adapt once,
and the data shapes the later changes fill in are fixed and tested now.

**`OutputConstraints`** holds the constraint properties the definition *declared*, and
only those. A property that a job parameter of the same type implies when omitted (the
RFC's example is the 1024-character default `maxLength` of a `RANGE_EXPR` parameter) is
never added (RFC 0010 `<OutputVariableDefinition>` item 6). Floats are `Float64`, so the
struct is hashable. For a list type, `min_length`/`max_length` bound the number of
elements and `item` holds the element constraints; `LIST[LIST[INT]]` nests a second
`item`. For `STRING`, `PATH`, and `RANGE_EXPR`, the lengths count characters.

**Constraint checks.** `OutputVariable` has three checks, each returning
`Result<(), String>` with a message `Output variable '<name>': <problem>`:

| Method | Checks |
|--------|--------|
| `check_constraints(&ExprValue)` | One Task's whole value: its type, then for a scalar the value constraints, and for a list `minLength`, `maxLength`, and every element (`item[i]`, or `item[j][i]` inside `LIST[LIST[INT]]`) |
| `check_element(&ExprValue)` | One element reported under `collect: APPEND`, against the element type and `item`. An error for a scalar type |
| `check_length(len, at_exit)` | A list being collected under `APPEND`: `maxLength` always, `minLength` only when `at_exit` |

`check_element` and `check_length` exist because under `APPEND` the constraints split:
`item` applies to each element as it arrives, `maxLength` to the growing list, and
`minLength` only when the Action exits. The checks follow the job-parameter rules
(`allowedValues`, `minValue`/`maxValue`, character lengths) but report an output's name.
A `PATH` value may be an `ExprValue::Path` or an `ExprValue::String`, and a `LIST[PATH]`
value a `ListPath` or a `ListString`, because a `PATH` output is stored in the submission
path context and checked in that form. Conversely a `STRING` value may be a `Path` and a
`LIST[STRING]` value a `ListPath`, as for `STRING` and `LIST[STRING]` job parameter
values; the path's text is checked. A `RANGE_EXPR` value may be an
`ExprValue::RangeExpr`, whose length is that of its canonical spelling, or an
`ExprValue::String` holding a valid range expression, whose length is that of the
string as written (as for a `RANGE_EXPR` job parameter value). The two can differ
(`1,2,3` is the range `1-3`); which spelling a reported directive value is measured in is
decided with output collection, which builds the stored value. An empty list built
without a type hint (`list[nulltype]`) satisfies every list type; a typed empty list must
have the declared type. `check_element` on a scalar type reports `INT is not a list type,
so it has no elements`.

**Serde.** Every new field has `#[serde(default)]` and is skipped when it holds its
default (`None`, `Bound`, or empty), so a Step serialized before this change still
deserializes, and the serialized form of a Step without `CONTROL_FLOW` features is
unchanged. `StepBinding` is internally tagged: `{"state": "BOUND"}`,
`{"state": "PENDING_INPUTS", "waitingOn": [...]}`, `{"state": "SKIPPED"}`.

Names use the template's spellings where it has them: `OutputVariable.param_type` is
serialized as `"type"` with the spec string (`"FLOAT"`, `"LIST[STRING]"`; read
case-insensitively, and an unknown name is `unknown output variable type: 'X'`),
`collect` as `"ONCE"`/`"LAST"`/`"APPEND"`, and the constraint keys in camelCase.
`OutputAllowedValues` and `OutputBound` are externally tagged by the value kind in
lowercase, so an `allowedValues` list of strings that look like numbers cannot be read
back as numbers. `PathInput.ty` is serialized as `"type"` with the expression type's
spelling. For example, a Step with two outputs and a pending binding serializes these
fields as:

```json
"outputVariables": [
  {"name": "T", "type": "FLOAT", "unit": "s",
   "constraints": {"allowedValues": {"float": [0.0, 1.5]}, "minValue": {"float": 0.0}}},
  {"name": "K", "type": "LIST[STRING]", "collect": "APPEND",
   "constraints": {"maxLength": 9, "item": {"allowedValues": {"string": ["a"]}}}}
],
"binding": {"state": "PENDING_INPUTS", "waitingOn": ["Steps.P.Output.N"]},
"pathInputs": [{"symbol": "Steps.P.RawOutput.Dirs", "type": "list[path]"}]
```

`OutputConstraints` is checked against the declared type only when `create_job` builds
it from the template; deserialization keeps a property of the wrong form (a `float`
bound on an `INT` output, say), which then has no effect on the checks.

`JobParameterType` derives `Deserialize` as well as `Serialize`; both use the variant
names (`"Int"`, `"ListString"`), as `job::JobParameter` already serializes them.

`job::Environment` does not get the plan's `enabled` field yet. Nothing could set it to
`false` before static `enableIf` lands with step input binding (until then `create_job`
refuses an `enableIf` it cannot evaluate), and the field would change every
`job::Environment` struct literal in the sessions and CLI crates and their tests (about
60 sites) for no behavior. That change adds the field with the code that sets it.

### StepScript, StepActions, Action

```rust
pub struct StepScript {
    pub let_bindings: Option<Vec<String>>,
    pub actions: StepActions,
    pub embedded_files: Option<Vec<EmbeddedFile>>,
}

pub struct StepActions {
    pub on_run: Action,
}

pub struct Action {
    pub command: FormatString,                           // Task-scope, unresolved
    pub args: Option<Vec<FormatString>>,                 // Task-scope, unresolved
    pub timeout: Option<FormatString>,                   // Template-scope symbols only, unresolved
    pub cancelation: Option<CancelationMode>,
}
```

`command` and `args` remain as `FormatString` because they may reference `Task.Param.*`
variables that are only available at task execution time. `timeout` (and
`notifyPeriodInSeconds` inside `cancelation`) also travel unresolved and are resolved by
the session runtime, but validation restricts them to template-scope symbols (they are
plain `@fmtstring` in the spec — job-creation stage), so no `Session.*`, `Task.*`, or
`Env.File.*` references can appear in them.

`let_bindings` deserializes from the `let` wire key, matching the template side and the
OpenJD wire format, and serializes back out as `let`. The legacy `letBindings` spelling
is accepted as an alias on input for backward compatibility. `EnvironmentScript` (below)
carries `let_bindings` with the same wire behavior. Both `StepScript` and
`EnvironmentScript` reject unknown fields on input, so a misspelled or unrecognized
script key fails loudly instead of being silently dropped.

### Environment, EnvironmentScript, EnvironmentActions

```rust
pub struct Environment {
    pub name: String,
    pub description: Option<String>,
    pub script: Option<EnvironmentScript>,
    pub variables: Option<HashMap<String, FormatString>>,  // Session-scope
    pub resolved_symtab: Option<SerializedSymbolTable>,
}

pub struct EnvironmentScript {
    pub let_bindings: Option<Vec<String>>,
    pub actions: EnvironmentActions,
    pub embedded_files: Option<Vec<EmbeddedFile>>,
}

pub struct EnvironmentActions {
    pub on_enter: Option<Action>,
    pub on_exit: Option<Action>,
}
```

`resolved_symtab` on `Environment` serves the same purpose as on `Step`: transporting
symbol values to the worker host. It is filtered to contain only the symbols referenced
by this environment's worker-resolved format strings — its variables, script actions
(including their timeouts), embedded files, and script-level let bindings. The same `RawParam` fallback applies:
if a format string references `Param.X` for a PATH-typed parameter, `RawParam.X` is
included instead.

### EmbeddedFile, CancelationMode

```rust
pub struct EmbeddedFile {
    pub name: String,
    pub file_type: FileType,                             // Typed enum, not String
    pub filename: Option<String>,                        // Plain string (not @fmtstring)
    pub data: Option<FormatString>,                      // Session/task-scope
    pub runnable: Option<bool>,
    pub end_of_line: Option<EndOfLine>,                  // Typed enum, not String
}
```

`file_type` is `FileType` (an enum) and `end_of_line` is `Option<EndOfLine>` (an enum),
not raw strings.

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

`CancelationMode` is an enum (same pattern as the template side — see
[template-types.md](template-types.md#cancelationmode) for the `DeferredMode`
rationale) with hand-written serde impls. The wire shape is
`{"mode": <string>, ...}`; a derived `#[serde(tag = "mode")]` representation
cannot express `DeferredMode`, whose tag position holds the raw format string
itself. Serialization omits `notifyPeriodInSeconds` when unset; deserialization
accepts an explicit `null` as unset for backward compatibility with documents
written by the previous derived impl (which always wrote
`"notifyPeriodInSeconds": null`).

### StepParameterSpace

```rust
pub struct StepParameterSpace {
    pub task_parameter_definitions: IndexMap<String, TaskParameter>,  // Insertion-ordered
    pub combination: Option<String>,
}
```

`task_parameter_definitions` uses `IndexMap` (not `HashMap`) to preserve definition order,
which matters for the default product combination.

### TaskParameter

Fully resolved task parameter with concrete range values:

```rust
pub enum TaskParameter {
    Int { range: TaskParamRange<i64>, chunks: Option<ResolvedChunks> },
    Float { range: Vec<Float64> },
    String { range: Vec<String> },
    Path { range: Vec<String> },
    ChunkInt { range: TaskParamRange<i64>, chunks: ResolvedChunks },
}
```

`Int` and `ChunkInt` ranges may be either a materialized list or a `RangeExpr` (from
`openjd-expr`) for compact representation of large integer sequences. `Float`, `String`,
and `Path` ranges are always materialized lists because they don't have a compact
representation.

### Float range elements

A `<FloatRangeList>` element is an `openjd_expr::value::Float64`, not a bare `f64`. Template
Schemas §7.5 keeps the decimal places a `<floatstring>` was written with, and `2.50_f64`
cannot carry them; `Float64` already pairs a value with an optional preserved spelling for
exactly this purpose, so the range reuses it rather than defining a parallel type.

`resolve_float_range` builds each element: a `<float>` literal or an expression result
through `Float64::new`, which renders via `format_float`, and a `<floatstring>` through
`Float64::with_str` after trimming, stripping redundant leading zeros (§7.5 rule 1) and
checking the text against `max_task_param_string_len`. Over that cap the element keeps only
its value, which is how it rendered before the text was carried at all.

`TaskParameter`'s `Hash` hashes each element's rendering as well as its value, because
`Float64::hash` takes only the value while its `PartialEq` compares both: two ranges holding
the same number but spelling it differently produce different command lines and are not the
same job.

### TaskParamRange, ResolvedChunks

```rust
pub enum TaskParamRange<T> {
    List(Vec<T>),
    RangeExpr(RangeExpr),
}

pub struct ResolvedChunks {
    pub default_task_count: usize,
    pub target_runtime_seconds: Option<usize>,
    pub range_constraint: RangeConstraint,
}
```

### Host Requirements (Resolved)

```rust
pub struct HostRequirements {
    pub amounts: Option<Vec<AmountRequirement>>,
    pub attributes: Option<Vec<AttributeRequirement>>,
}

pub struct AmountRequirement {
    pub name: String,
    pub min: Option<f64>,                                // Resolved from FormatString
    pub max: Option<f64>,                                // Resolved from FormatString
}

pub struct AttributeRequirement {
    pub name: String,
    pub any_of: Option<Vec<String>>,                     // Resolved from FormatString
    pub all_of: Option<Vec<String>>,                     // Resolved from FormatString
}
```

### StepDependency

```rust
pub struct StepDependency {
    pub depends_on: String,
}
```

## Template → Job Type Mapping

| Template Type | Job Type | Key Differences |
|--------------|----------|----------------|
| `template::JobTemplate` | `job::Job` | `name` is `String` not `FormatString`; parameters carry resolved values; `parameters` is `IndexMap` |
| `template::StepTemplate` | `job::Step` | `name` resolved; `host_requirements` values resolved; carries `resolved_symtab: Option<SerializedSymbolTable>`; `outputVariables` become `Vec<job::OutputVariable>`; `enableIf` is not carried yet |
| `template::OutputVariableDefinition` | `job::OutputVariable` | Declared constraints as `OutputConstraints`; no `description` or `objectType` |
| `template::StepScript` | `job::StepScript` | Structurally identical; action fields remain `FormatString` |
| `template::Environment` | `job::Environment` | `variables` values remain `FormatString` (session-scope); adds `resolved_symtab` |
| `template::HostRequirements` | `job::HostRequirements` | `min`/`max` are `f64`; `any_of`/`all_of` are `Vec<String>` |
| `template::EmbeddedFile` | `job::EmbeddedFile` | `file_type` is `FileType` enum; `end_of_line` is `Option<EndOfLine>` enum |
| `template::CancelationMode` | `job::CancelationMode` | Both are enums with `Terminate` and `NotifyThenTerminate` variants |
| `template::StepParameterSpaceDefinition` | `job::StepParameterSpace` | Ranges resolved to concrete values; definitions keyed by name in `IndexMap` |
| `template::TaskParameterDefinition` | `job::TaskParameter` | Enum with resolved ranges and optional chunks |
