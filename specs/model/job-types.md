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
    pub outputs: Option<Vec<JobOutput>>,                 // CONTROL_FLOW job outputs
}
```

`parameters` uses `IndexMap` (not `HashMap`) to preserve insertion order for deterministic
output.

`outputs` holds the template's `jobOutputDefinitions`, in order, or `None` when it has
none. It has `#[serde(default)]` and is skipped when `None`, so a Job without job outputs
serializes as before. See [JobOutput](#joboutput-control_flow) below and
[job-outputs.md](job-outputs.md).

### JobOutput (CONTROL_FLOW)

```rust
pub struct JobOutput {
    pub declaration: OutputVariable,      // name, type, unit, constraints
    pub object_type: Option<ObjectType>,  // PATH and LIST[PATH]: FILE or DIRECTORY
    pub description: Option<String>,
    pub value: FormatString,              // as written
    pub evaluated: Option<JobOutputValue>,
}

pub enum JobOutputValue { Value(ExprValue), Null }
```

`declaration` is an `OutputVariable` (see [CONTROL_FLOW fields](#control_flow-fields-rfc-0010)),
so its constraint checks are the ones a Step's outputs use; its `collect` is always
`ONCE`. `object_type` carries a `PATH` or `LIST[PATH]` output's `objectType` for tools and
user interfaces; it is not checked. `evaluated` is `None` while the output is pending, `Value` with the value
converted to the declared type, or `Null` when the value evaluated to `null` (no value,
which is not an error). `create_job` fills it for the outputs whose inputs are concrete at
creation; a caller fills the rest from `evaluate_job_output`, for example with
`JobOutputValue::from`. Like `Job`, both types implement `Serialize` but not
`Deserialize`. `JobOutput`'s `Serialize` is written by hand to emit one plain struct with
the declaration's fields inline (`#[serde(flatten)]` would serialize as a map, which
`serde_wasm_bindgen` turns into a JS `Map` for openjd-for-js). A pending output with a
description serializes as
`{"name": "N", "type": "INT", "description": "...", "value": "{{Steps.P.Output.N}}"}`, with
`unit`, `constraints`, and `objectType` after `type` when declared. An evaluated one adds
`"evaluated": {"state": "VALUE", "value": {"Int": 8}}` (the `ExprValue` serde form that
`JobParameter.value` also uses), or `"evaluated": {"state": "NULL"}` when it evaluated to
`null`, so an absent `evaluated` is always a pending output. The `state` tag follows
`StepBinding`; a bare JSON `null` would read as `undefined` in openjd-for-js, the same as
an absent field.

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
    PendingInputs(PendingBinding),
    Skipped,
}

pub struct PendingBinding {
    pub enablement: GroupState<()>,                       // Known(()) once enableIf is true or absent
    pub tasks: GroupState<Option<StepParameterSpace>>,    // Known(None): no parameterSpace, one Task
    pub host_requirements: GroupState<Option<HostRequirements>>,
    pub remaining: GroupState<()>,                        // Step Environments and script
    pub producers: Vec<String>,   // producers any group reads, sorted; skipped when empty
}

pub enum GroupState<T> {
    Waiting { on: Vec<String> },   // the group's own Steps.* inputs that are not concrete, sorted
    Known(T),
}

pub struct PathInput {
    pub symbol: String,
    pub ty: openjd_expr::ExprType,             // serialized as "type": "path", "list[path]", ...
}
```

Deserialization does not re-check what binding guarantees when it writes these values:
`GroupState::Waiting::on` is kept in the order given, and `PathInput.ty` may be any type
its spelling parses to.

`create_job` fills `output_variables` from the template's `outputVariables`
(`OutputVariableDefinition::to_job_output_variable`). Under `CONTROL_FLOW` it binds each
Step as far as the values concrete at job creation allow, so `binding` is any of the three
states, and `advance_step_binding` moves a pending Step on (see
[job-creation.md](job-creation.md) § Step input binding). A `PendingInputs` Step keeps
`parameter_space`, `host_requirements`, and `resolved_symtab` as `None`: what is known of
them waits in its `PendingBinding` and moves into the Step's fields when it is bound. Its
script, Step Environments, dependencies, and outputs are filled in as for a bound Step. A
`Skipped` Step has the same three fields `None` for good, and zero Tasks. `path_inputs` is
recorded by `attach_step_inputs`.

`GroupState::Waiting::on` lists the group's own inputs (`Steps.<Step>.<Member>.<Name>`)
that are not concrete, not counting the enablement group's inputs, which every other group
also waits for: it is empty for a group that waits only for `enableIf`.

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
A string a message quotes is cut to 200 characters with its control characters escaped
(see [outputs.md](outputs.md) § Errors).
A `PATH` value may be an `ExprValue::Path` or an `ExprValue::String`, and a `LIST[PATH]`
value a `ListPath` or a `ListString`, because a `PATH` output is stored in the submission
path context and checked in that form. Conversely a `STRING` value may be a `Path` and a
`LIST[STRING]` value a `ListPath`, as for `STRING` and `LIST[STRING]` job parameter
values; the path's text is checked. A `RANGE_EXPR` value may be an
`ExprValue::RangeExpr`, whose length is that of its canonical spelling, or an
`ExprValue::String` holding a valid range expression, whose length is that of the
string as written (as for a `RANGE_EXPR` job parameter value). The two can differ
(`1,2,3` is the range `1-3`); the output collector checks a reported value in its
written spelling and stores the `RangeExpr` ([outputs.md](outputs.md) § Constraints). An empty list built
without a type hint (`list[nulltype]`) satisfies every list type; a typed empty list must
have the declared type. `check_element` on a scalar type reports `INT is not a list type,
so it has no elements`, and so does `check_length` (a scalar's `minLength`/`maxLength`
count characters, not elements).

**Serde.** Every new field has `#[serde(default)]` and is skipped when it holds its
default (`None`, `Bound`, or empty), so a Step serialized before this change still
deserializes, and the serialized form of a Step without `CONTROL_FLOW` features is
unchanged. `StepBinding` is internally tagged: `{"state": "BOUND"}`,
`{"state": "PENDING_INPUTS", "enablement": ..., "tasks": ..., "hostRequirements": ...,
"remaining": ...}`, `{"state": "SKIPPED"}`. A `GroupState` is externally tagged in
camelCase: `{"waiting": {"on": [...]}}` or `{"known": <value>}`, with `null` for `()` and
for a `None` result.

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
"binding": {"state": "PENDING_INPUTS", "enablement": {"known": null},
            "tasks": {"waiting": {"on": ["Steps.P.Output.N"]}},
            "hostRequirements": {"known": null}, "remaining": {"known": null},
            "producers": ["P"]}
```

and a bound Step whose script reads a path-typed output records it as:

```json
"pathInputs": [{"symbol": "Steps.P.RawOutput.Dirs", "type": "list[path]"}]
```

`OutputConstraints` is checked against the declared type only when `create_job` builds
it from the template; deserialization keeps a property of the wrong form (a `float`
bound on an `INT` output, say), which then has no effect on the checks.

`JobParameterType` derives `Deserialize` as well as `Serialize`; both use the variant
names (`"Int"`, `"ListString"`), as `job::JobParameter` already serializes them.

`job::Environment` has `enabled: bool`: `false` when its `enableIf` evaluated to
`false`. A disabled Environment must be left out of the Session's Environment stack (not
entered, its `variables` not set); the caller that builds the stack does this, and
`openjd-sessions` does not read the field. It is serialized only when `false` and defaults to `true`, so
existing jobs read back unchanged. `create_job` decides a Job Environment's; the remaining
group of step input binding decides a Step Environment's, so on a pending Step whose
remaining group is not known yet it is `true` whatever its `enableIf` will give.

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
    pub enabled: bool,                                     // CONTROL_FLOW: false when enableIf is false
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
| `template::StepTemplate` | `job::Step` | `name` resolved; `host_requirements` values resolved; carries `resolved_symtab: Option<SerializedSymbolTable>`; `outputVariables` become `Vec<job::OutputVariable>`; `enableIf` is decided into `binding` (`Skipped` when `false`) |
| `template::OutputVariableDefinition` | `job::OutputVariable` | Declared constraints as `OutputConstraints`; no `description` or `objectType` |
| `template::StepScript` | `job::StepScript` | Structurally identical; action fields remain `FormatString` |
| `template::Environment` | `job::Environment` | `variables` values remain `FormatString` (session-scope); adds `resolved_symtab`; `enableIf` is decided into `enabled` |
| `template::HostRequirements` | `job::HostRequirements` | `min`/`max` are `f64`; `any_of`/`all_of` are `Vec<String>` |
| `template::EmbeddedFile` | `job::EmbeddedFile` | `file_type` is `FileType` enum; `end_of_line` is `Option<EndOfLine>` enum |
| `template::CancelationMode` | `job::CancelationMode` | Both are enums with `Terminate` and `NotifyThenTerminate` variants |
| `template::StepParameterSpaceDefinition` | `job::StepParameterSpace` | Ranges resolved to concrete values; definitions keyed by name in `IndexMap` |
| `template::TaskParameterDefinition` | `job::TaskParameter` | Enum with resolved ranges and optional chunks |
