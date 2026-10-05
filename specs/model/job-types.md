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
| SERVICE EXECUTION (RFC 0009) | When a Service is started on its host | Service `variables`, Service script actions and embedded files, `<ServiceScript>.let` |

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
    pub extensions: Option<Vec<ModelExtension>>,
    pub parameters: IndexMap<String, JobParameter>,      // Insertion-ordered
    pub steps: Vec<Step>,
    pub job_environments: Option<Vec<Environment>>,
    pub services: Option<Vec<Service>>,                  // SERVICE (RFC 0009); omitted from JSON when None
    pub requires_services: Option<Vec<ServiceRequirement>>,  // SERVICE (RFC 0009); omitted from JSON when None
}
```

`parameters` uses `IndexMap` (not `HashMap`) to preserve insertion order for deterministic
output. `services` is the instantiated `services` list in declaration order, each entry carrying
its computed scope (see [Service](#service-rfc-0009-service-extension)); after
`apply_environment_templates`, the attached external Services precede the Job Template's own.
`requires_services` is the instantiated `requiresServices` list. Both serialize only when
present, so a job without Services has the same wire shape as before RFC 0009.

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
}
```

A Step carries no Service list: the Services whose scope includes it are those of
`Job::services` whose `scope` contains its name.

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
    pub run_scope: Option<Vec<RunScope>>,                  // SERVICE (RFC 0009); None = every kind of Session
    pub script: Option<EnvironmentScript>,
    pub variables: Option<HashMap<String, FormatString>>,  // Session-scope
    pub resolved_symtab: Option<SerializedSymbolTable>,
}

impl Environment {
    pub fn runs_in(&self, kind: RunScope) -> bool;         // job-side twin of template::Environment::runs_in
}

pub struct EnvironmentScript {
    pub let_bindings: Option<Vec<String>>,
    pub actions: EnvironmentActions,
    pub embedded_files: Option<Vec<EmbeddedFile>>,
}

pub struct EnvironmentActions {
    pub on_enter: Option<Action>,
    pub on_wrap_env_enter: Option<Action>,                 // WRAP_ACTIONS (RFC 0008)
    pub on_wrap_task_run: Option<Action>,
    pub on_wrap_env_exit: Option<Action>,
    pub on_wrap_service_enter: Option<Action>,             // WRAP_ACTIONS + SERVICE (RFC 0009)
    pub on_wrap_service_run: Option<Action>,
    pub on_wrap_service_health_check: Option<Action>,
    pub on_wrap_service_exit: Option<Action>,
    pub on_exit: Option<Action>,
}

impl EnvironmentActions {
    // Generated by `impl_environment_actions_helpers!`, with the same nine slots and seven
    // wrap hooks as the template side:
    pub fn named_slots(&self) -> [(&'static str, &Option<Action>); 9];
    pub fn iter_named(&self) -> impl Iterator<Item = (&'static str, &Action)>;
    pub fn iter_actions(&self) -> impl Iterator<Item = &Action>;
    pub fn wrap_hooks(&self) -> [(&'static str, &Option<Action>, WrapHookScope); 7];
    pub fn has_any_action(&self) -> bool;
    pub fn has_any_wrap_hook(&self) -> bool;
    pub fn service_wrap_hooks(&self) -> [(&'static str, &Option<Action>); 4];
    pub fn has_any_service_wrap_hook(&self) -> bool;
}
```

`run_scope` is typed (`RunScope`, re-exported from `job` together with
`CompletedTasksPolicy`) rather than the template side's raw strings: pass 11 has already
rejected unrecognized names, so conversion parses each entry and a Session runtime can
dispatch on the enum. The wrap hooks and `runScope` all use
`#[serde(default, skip_serializing_if = "Option::is_none")]`, so a job without them has the
pre-RFC wire shape and older documents deserialize.

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

### Service (RFC 0009, `SERVICE` extension)

```rust
pub struct Service {
    pub name: String,
    pub description: Option<String>,
    pub document: Document,                                // §1.2.2 item 3; omitted from JSON when JobTemplate
    pub scope: ServiceScope,                               // §9.1, computed; {"kind":"allSteps"} | {"kind":"steps","steps":[..]}
    pub references: Vec<String>,                           // §9.1 rule 3: same-document Services it references; omitted when empty
    pub dependencies: Option<Vec<StepDependency>>,         // §9 item 4; omitted from JSON when None
    pub host_requirements: Option<HostRequirements>,       // Resolved, like Step's
    pub ports: Vec<ServicePort>,                           // Declaration order
    pub health_check: ServiceHealthCheck,                  // §9.4 defaults applied
    pub restart_policy: ServiceRestartPolicy,              // §9.5 defaults applied
    pub variables: Option<HashMap<String, FormatString>>,  // Service-execution scope, unresolved
    pub script: ServiceScript,                             // Service-execution scope, unresolved
    pub resolved_symtab: Option<SerializedSymbolTable>,
}

impl Service {
    pub fn port_names(&self) -> impl Iterator<Item = &str>;
    pub fn port(&self, name: &str) -> Option<&ServicePort>;
}

pub use template::ServiceScope;                            // re-exported; see template-types.md "Service scope"

pub struct ServiceRequirement {                            // §9.8, instantiated requiresServices entry
    pub name: String,
    pub ports: Vec<ServiceRequirementPort>,
}
impl ServiceRequirement { pub fn port_names(&self) -> impl Iterator<Item = &str>; }

pub struct ServiceRequirementPort {                        // §9.8.1
    pub name: String,
    #[serde(default, skip_serializing_if = "ServicePortProtocol::is_default")]
    pub protocol: ServicePortProtocol,
}

#[serde(tag = "kind")]
pub enum Document {
    JobTemplate,                                           // the default
    EnvironmentTemplate { index: usize, label: Option<String> },  // 0-based attachment index; label omitted when None
}

impl Document {
    pub fn environment_template(index: usize, label: Option<&str>) -> Self;
    pub fn is_job_template(&self) -> bool;
}
impl Display for Document;   // "JobTemplate" | <label> | "EnvironmentTemplate[i]"

pub struct ServicePort {
    pub name: String,
    pub port: Option<u16>,                                 // None = runtime allocates (in `protocol`'s space)
    #[serde(default, skip_serializing_if = "ServicePortProtocol::is_default")]
    pub protocol: ServicePortProtocol,                     // §9.3 item 3: TCP (default, omitted from JSON) | UDP
}

pub use template::ServicePortProtocol;                     // re-exported; see template-types.md

#[serde(tag = "type")]
pub enum ServiceHealthCheck {
    TcpConnect {                                   // "TCP_CONNECT"
        ports: Vec<String>,                        // every declared TCP port when the template named none
        readiness_interval_seconds: u64,           // default 1
        readiness_timeout_seconds: u64,                // default 300
        health_interval_seconds: u64,              // default 30
        failure_threshold: u64,                    // default 3
    },
    Command {                                      // "COMMAND"
        readiness_interval_seconds: u64,           // default 5
        readiness_timeout_seconds: u64,
        health_interval_seconds: u64,
        failure_threshold: u64,
    },
    Stdout {                                       // "STDOUT"
        readiness_timeout_seconds: u64,
        health_interval_seconds: Option<u64>,      // None = no heartbeat (omitted from JSON)
        failure_threshold: u64,                    // default 3; meaningful only with the interval
    },
}

impl ServiceHealthCheck {
    pub fn type_name(&self) -> &'static str;
    pub fn readiness_interval_seconds(&self) -> Option<u64>;  // None for STDOUT
    pub fn readiness_timeout_seconds(&self) -> u64;
    pub fn health_interval_seconds(&self) -> Option<u64>;     // None only for STDOUT without a heartbeat
    pub fn failure_threshold(&self) -> u64;
    pub fn monitors_health(&self) -> bool;                    // health_interval_seconds().is_some()
}

pub struct ServiceRestartPolicy {
    pub max_attempts: u64,
    pub completed_tasks: CompletedTasksPolicy,
}

pub struct ServiceScript {
    pub let_bindings: Option<Vec<String>>,                 // "let" wire key (alias "letBindings"); <ServiceScript>.let
    pub actions: ServiceActions,
    pub embedded_files: Option<Vec<EmbeddedFile>>,
}

pub struct ServiceActions {
    pub on_enter: Option<Action>,
    pub on_run: Action,
    pub on_health_check: Option<Action>,
    pub on_exit: Option<Action>,
}

impl ServiceActions {
    pub fn named_slots(&self) -> [(&'static str, Option<&Action>); 4];
    pub fn iter_named(&self) -> impl Iterator<Item = (&'static str, &Action)>;
    pub fn iter_actions(&self) -> impl Iterator<Item = &Action>;
}
```

The job-creation-stage fields are resolved: `<Service>.let` (evaluated into the Service's
job-creation symbol table and transported in `resolved_symtab`), the numeric `@fmtstring`
fields (`port`, the four `<ServiceHealthCheck>` fields, `maxAttempts` — resolved with target
`int?`, range-checked, and defaulted per §9 when absent or `null`), and `hostRequirements`.
`variables` and the whole `script` are `@fmtstring[host]` and stay `FormatString`s, exactly
as an Environment's do: they reference `Session.*`, `Service.File.*`, and in-scope
`Service.<name>.<port>.*` endpoints that only the Service Session can bind. Action `timeout`
and cancelation fields travel unresolved too, restricted to job-creation-stage symbols as on a
Step. `<Service>.let` itself is not carried (its values are), while `<ServiceScript>.let` is,
for the host to evaluate.

`scope` is the set of Steps whose Tasks depend on the Service (Template Schemas §9.1), computed
by `create_job` from the template's `Service.*` references through
`template::compute_service_scopes` — `AllSteps` for a Service a Job Environment references, one
nothing references, or one a Job-wide Service references; `Steps { .. }` otherwise — and
`AllSteps` for every external Service, stamped by `apply_environment_templates`. `references`
are the names of the other Services **of the same document** the Service references (§9.1 rule
3), sorted: a scheduler starts the Service after each is READY and stops it before any of them.
`dependencies` are the Steps that must complete before the Service starts (§9 item 4); never set
on an external Service. A scheduler therefore reads the lifecycle of every Service off the Job
without re-deriving it: start before the first Task of any Step in `scope`, after `dependencies`
and `references`; stop once no Step in `scope` has a Task left. `scope` deserializes as
`AllSteps` when absent, so a Job serialized before scopes were recorded still loads.

`document` is the document of the submission that declares the Service (Template Schemas
§1.2.2 item 3, RFC 0009 "Inline Services shadow external ones"): `Document::JobTemplate` for
every `services` entry `create_job` produces, and the attached Environment Template (by 0-based
attachment index, with the caller's label when it gave one) for an external Service, stamped by
`apply_environment_templates`. Service names are unique within the list that declares them and
nothing more — an external Service may be named like a Service of the Job Template or of another
attachment (an error only when a `requiresServices` entry names it) — so two Services of a
combined Job are the same Service iff `(document, name)` agree; a scheduler keys on that pair. A
`Service.*` reference resolves within the referencing entity's own document, except that the Job
Template's references to a required Service resolve to the attached Service the requirement was
bound to (`AppliedEnvironmentTemplates::requirement_bindings`). `Document` is `Ord` so it can
key a `BTreeMap`/`BTreeSet`; `Display` names the document as the submission-time error paths
do. The field is omitted from JSON for the Job Template's own Services (the default on
deserialization), so a Job without attachments serializes as before.

`resolved_symtab` is filtered like an Environment's: the symbols referenced by `variables`,
every script action (command, args, timeout, cancelation), embedded-file `data`, and
`<ServiceScript>.let` — which is how the `<Service>.let` values reach the host — with the
`RawParam.*` fallback for PATH parameters. A Service Session layers `Session.*`,
`Service.File.*`, and the `Service.*` endpoints from
`job::service_symbols::build_service_symbol_table` on top. The same fields, walked by
`job::service_symbols::referenced_service_names`, give a scheduler every Service this one
references by name — `references` restricted to its document, plus any required external
Service.

`Service` implements `PartialEq` and `Hash` with the module's invariant; `variables` hashes as
key-sorted entries. It also implements `Deserialize`, so a created job's Services round-trip
through the wire format.

## Template → Job Type Mapping

| Template Type | Job Type | Key Differences |
|--------------|----------|----------------|
| `template::JobTemplate` | `job::Job` | `name` is `String` not `FormatString`; parameters carry resolved values; `parameters` is `IndexMap` |
| `template::StepTemplate` | `job::Step` | `name` resolved; `host_requirements` values resolved; carries `resolved_symtab: Option<SerializedSymbolTable>` |
| `template::StepScript` | `job::StepScript` | Structurally identical; action fields remain `FormatString` |
| `template::Environment` | `job::Environment` | `variables` values remain `FormatString` (session-scope); `run_scope` is `Vec<RunScope>` not `Vec<String>`; adds `resolved_symtab` |
| `template::Service` | `job::Service` | `let` evaluated into `resolved_symtab`; `port`, the `healthCheck` numeric fields, and `maxAttempts` are integers with defaults applied; `health_check`/`restart_policy` are non-optional with defaults applied; `host_requirements` resolved; `variables`/`script` remain `FormatString`; adds the computed `scope` and `references` |
| `template::ServiceRequirement` | `job::ServiceRequirement` | Structurally identical |
| `template::HostRequirements` | `job::HostRequirements` | `min`/`max` are `f64`; `any_of`/`all_of` are `Vec<String>` |
| `template::EmbeddedFile` | `job::EmbeddedFile` | `file_type` is `FileType` enum; `end_of_line` is `Option<EndOfLine>` enum |
| `template::CancelationMode` | `job::CancelationMode` | Both are enums with `Terminate` and `NotifyThenTerminate` variants |
| `template::StepParameterSpaceDefinition` | `job::StepParameterSpace` | Ranges resolved to concrete values; definitions keyed by name in `IndexMap` |
| `template::TaskParameterDefinition` | `job::TaskParameter` | Enum with resolved ranges and optional chunks |
