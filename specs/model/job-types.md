# Instantiated Job Types

The `job` module contains types produced by `create_job()`. These represent a fully instantiated
job where template-scope format strings have been resolved to concrete values. Session-scope and
task-scope fields remain as `FormatString` for evaluation at runtime.

All types derive `Debug, Clone, Serialize`, most with `#[serde(rename_all = "camelCase")]`.
`Document` is internally tagged instead (`{"kind": "JobTemplate"}`, with its variant fields in
camelCase).

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
referenced — no `Session.*`/`Task.*`/`Env.File.*`/`Env.Param.*`), yet they remain `FormatString` in the
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
    pub host_requirements: Option<HostRequirements>,     // Combined with ENV_PARAMS (§3.3.3)
    pub dependencies: Option<Vec<StepDependency>>,
    pub resolved_symtab: Option<SerializedSymbolTable>,
    pub environment_parameters: Option<Vec<EnvironmentParameterValues>>, // ENV_PARAMS
    pub environments: Option<Vec<EnvironmentRef>>,                        // ENV_PARAMS
}
```

`resolved_symtab` exists to transport symbol values across the network to the worker host
that runs the job. The worker evaluates the format strings that remain unresolved after job
creation, so `resolved_symtab` is filtered to contain exactly the symbols referenced by
those format strings. For a Step, these are: the step script's actions (command, args,
timeout, cancelation), embedded files, script-level let bindings, step-level let bindings,
and any step-scoped environments' variables, actions, and embedded files. Most of these are
host-context (SESSION and TASK scope); timeout and notifyPeriodInSeconds are template scope
(validation rejects `Session.*`/`Task.*`/`Env.File.*`/`Env.Param.*` in them) but still resolve on the
worker, referencing only the job-creation-stage symbols carried in `resolved_symtab`.

Contents include `RawParam.*`, non-PATH `Param.*` values, `Job.Name`, `Job.EnvNames`
(ENV_PARAMS), `Step.Name`, and let bindings. PATH-typed `Param.*` entries and any `apply_path_mapping` results are excluded
because path mapping rules aren't available until session time. The session layers these
plus `Session.*` and `Task.*` values on top at runtime.

The type is `SerializedSymbolTable` (not `SymbolTable`) — a wire-format type serialized as
`[{"name": str, "value": ..., "type": str}]` for cross-host transfer in a Python-compatible
format.

### Environment parameter values (ENV_PARAMS)

```rust
pub enum Document {                       // serde: {"kind": "JobTemplate"} or
    JobTemplate,                          //   {"kind": "EnvironmentTemplate", "index": 0, "label": ...}
    EnvironmentTemplate { index: usize, label: Option<String> },
}

pub struct EnvironmentParameterValues {
    pub document: Document,
    pub environment: String,
    pub values: SerializedSymbolTable,
}

pub struct EnvironmentRef {
    pub document: Document,
    pub environment: String,
}
```

`Step::environment_parameters` holds the Step's effective values (Template Schemas §3.2.2),
one `EnvironmentParameterValues` per Environment its Sessions enter that declares
`envParameterDefinitions`, in the order a Session enters them: the attached Environment Templates'
Environments first (added by the submission stage of `create_job_with_environment_templates`),
then the Job Environments. Environment names are unique only within a document, so an entry is keyed by
`(document, environment)`; `Document` is the same type as on the `SERVICE` prototype branch.
`values` has one entry per parameter, keyed by the bare parameter name, with the parameter's
own expression type in `type` (`path` for PATH, `list[path]` for LIST[PATH], `int`, ...), so a
Session knows which values to path-map. PATH values are the unmapped strings, in the
submission path format, stored as resolved (plan A22). A Session binds them as `Env.Param.<name>`
(path-mapped) and `Env.RawParam.<name>` (unmapped).

`Step::environments` lists the Job Environments the Step's Sessions enter (Template Schemas §4
item 4 `activation`, §3.2.2), in `Job::job_environments` order, each as the `(document,
environment)` key `EnvironmentParameterValues` uses: every one with `activation: ALWAYS`, and every
one with `activation: ON_REQUEST` that an enabled `environment:` entry of the Step requests,
less any whose own `enableIf` (§4 item 6) is `false` for the Step. The Step's own
`step_environments`, entered after these, are in its Environment set too but are not listed;
a Step Environment whose `enableIf` is `false` is left out of `step_environments` (`None` when
none remain), and a disabled Environment has no `environment_parameters` entry. `enableIf` is
not copied to `job::Environment` either (plan 3 Q11). `None` means every Job Environment,
which is what every Step of a Job that uses no `ON_REQUEST` Environment and no Environment
`enableIf` gets, so such a Job serializes as before; a created Job never holds
`Some` of the full list, so the field compares directly. `Step::enters_environment(document,
name)` answers for one Environment, and `Step::same_environments(&other)` compares the field
(prototype plan P3). A key rather than an index stays meaningful if a consumer filters the
Environment list. The `activation` itself is not copied to `job::Environment`: the Step's list,
not a property of the Environment, decides entry (plan P3 put it there for display; `openjd
summary` shows each Step's set from this field instead, so it is not needed).

Both fields are `#[serde(default, skip_serializing_if = "Option::is_none")]`, so a Job without
Environment parameters, `ON_REQUEST` Environments, or Environment `enableIf`s serializes exactly
as before. They
participate in the derived `PartialEq` and `Hash` of `Step`.

**Equality.** A Session may run Tasks of two Steps without exiting and re-entering Job
Environments between them only when they enter the same Job Environments
(`Step::same_environments`) and their effective values are equal (§3.2.2, fourth
paragraph). (Each Step's own `step_environments` are entered and exited around its
Tasks, as without the extension.) `Step::environment_values_equal(&other)` implements §3.2.2
equality of the values: both Steps have values
for the same `(document, environment)` keys, and for each, the same parameter names with the
same types and equal values. A value whose type contains `float` compares numerically, so
FLOAT `1.0` and `1.00` are equal (plan A1); a `range_expr` value compares by the integers it
denotes, so `1-3` and `1,2,3` are equal even in a deserialized Job that holds another spelling
(§3.2.2; job creation stores the canonical form, see
[job-creation.md](job-creation.md#environment-parameter-values-env_params)); every other value
compares by its transport form, so INT `1` and `"1"` are equal (both resolve to the INT 1) and
PATH values compare by unmapped string. A list that names one `(document, environment)` twice is not one `create_job`
produces; it compares unequal to every list, including itself, on either side. The derived
`==` on `environment_parameters` compares the transport form, which keeps a
FLOAT's text, so it treats `1.0` and `1.00` as different. `EnvironmentParameterValues::values_equal`
applies the same rule to one Environment's values.

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
plain `@fmtstring` in the spec — job-creation stage), so no `Session.*`, `Task.*`,
`Env.File.*`, or (ENV_PARAMS) `Env.Param.*` / `Env.RawParam.*` references can appear in them.

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
`openjd-expr`) for compact representation of large integer sequences. A `RangeExpr`
compares and hashes by its values, so two range-expression ranges written differently
(`"1,3,5,7,9"` and `"1-9:2"`) but holding the same values are equal, and so are the
`TaskParameter`s holding them. `Float`, `String`, and `Path` ranges are always
materialized lists because they don't have a compact representation.

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

With ENV_PARAMS, `Step::host_requirements` holds the Step's **combined** host requirements
(Template Schemas §3.3.3): its own and those of every Environment its Sessions enter, each
Environment's resolved with the Step's effective values, normalized per capability (see
[job-creation.md](job-creation.md#combined-host-requirements-env_params)). A consumer that
reads a Step's requirements, such as a scheduler, reads the combined ones; the Environments'
own requirements are not stored. The set is a conjunction in the same shape as a Step's own,
except that a name other than `attr.worker.os.family` and `attr.worker.cpu.arch` may appear
more than once when more than one source names it: an `allOf` entry when any source requires
a value outright, then any number of `anyOf` entries of two or more values each (§3.3.3 item
2.2). A Step none of
whose entered Environments has requirements keeps its own, unchanged.

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
| `template::StepTemplate` | `job::Step` | `name` resolved; `host_requirements` values resolved, and with ENV_PARAMS combined with the entered Environments' (§3.3.3); carries `resolved_symtab: Option<SerializedSymbolTable>`; `environment:` dependency entries, with `activation` and both `enableIf`s, become `environment_parameters` / `environments` (ENV_PARAMS); a Step Environment whose `enableIf` is `false` is dropped |
| `template::StepScript` | `job::StepScript` | Structurally identical; action fields remain `FormatString` |
| `template::Environment` | `job::Environment` | `variables` values remain `FormatString` (session-scope); adds `resolved_symtab` |
| `template::HostRequirements` | `job::HostRequirements` | `min`/`max` are `f64`; `any_of`/`all_of` are `Vec<String>` |
| `template::EmbeddedFile` | `job::EmbeddedFile` | `file_type` is `FileType` enum; `end_of_line` is `Option<EndOfLine>` enum |
| `template::CancelationMode` | `job::CancelationMode` | Both are enums with `Terminate` and `NotifyThenTerminate` variants |
| `template::StepParameterSpaceDefinition` | `job::StepParameterSpace` | Ranges resolved to concrete values; definitions keyed by name in `IndexMap` |
| `template::TaskParameterDefinition` | `job::TaskParameter` | Enum with resolved ranges and optional chunks |
