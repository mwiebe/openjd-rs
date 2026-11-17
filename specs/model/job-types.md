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
| SERVICE EXECUTION (RFC 0009) | When a Service is started on its host | Service `variables`, Service script actions and embedded files, `<ServiceScript>.let` |

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
    pub extensions: Option<Vec<ModelExtension>>,
    pub parameters: IndexMap<String, JobParameter>,      // Insertion-ordered
    pub steps: Vec<Step>,
    pub job_environments: Option<Vec<Environment>>,
    pub services: Option<Vec<Service>>,                  // SERVICE (RFC 0009); omitted from JSON when None
    pub external_services: Option<Vec<ServiceRequirement>>,  // SERVICE (RFC 0009); omitted from JSON when None
}

impl Job {
    pub fn has_extension(&self, extension: ModelExtension) -> bool;  // `extensions` contains it
    pub fn service_active(&self) -> bool;                            // has_extension(ModelExtension::Service)
}
```

`parameters` uses `IndexMap` (not `HashMap`) to preserve insertion order for deterministic
output. `services` is the instantiated `services` list in declaration order, each entry carrying
its computed scope (see [Service](#service-rfc-0009-service-extension)); after
`create_job_with_environment_templates`, the attached external Services precede the Job Template's own.
`external_services` is the instantiated `externalServices` list. Both serialize only when
present, so a job without Services has the same wire shape as before RFC 0009.

`service_active` is true for a Job created from a template declaring `SERVICE` — the only Job
whose `dependencies` entries may use the `service` key and which may have `services` or
`external_services`.

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

A Step carries no separate Service list. The Services it depends on are the `service: <name>`
entries of its `dependencies` (see [StepDependency](#stepdependency)); the Services whose scope
includes it are those of `Job::services` whose `scope` contains its name — the ones it lists,
those a Service it lists depends on, transitively, and those listed by a Job Environment its
Sessions enter.

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
Session knows which values to path-map. PATH values are the unmapped strings, of any form
(relative, either path format, or a URI; §4.5 item 3 at spec `af53020`), stored as resolved
(plan A22). A Session binds them as `Env.Param.<name>`
(path-mapped) and `Env.RawParam.<name>` (unmapped).

`Step::environments` lists the Job Environments the Step's Sessions enter (Template Schemas §4
item 4 `activation`, §3.2.2), in `Job::job_environments` order, each as the `(document,
environment)` key `EnvironmentParameterValues` uses: every one with `activation: ALWAYS`
or from an Environment Template that does not list ENV_PARAMS (§4 item 4),
every one with `BY_DEFAULT` that no disabled `environment:` entry of the Step declines (§3.2
item 3.2), and every one with `activation: ON_REQUEST` that an enabled `environment:` entry
of the Step requests, less any whose own `enableIf` (§4 item 6) is `false` for the Step (for
an `ALWAYS` one either is a job-creation error, so a created Job never leaves one out) and any
that an earlier one of its `exclusiveGroup` supersedes (§4 item 7). The Step's own
`step_environments`, entered after these, are in its Environment set too but are not listed;
a Step Environment whose `enableIf` is `false` is left out of `step_environments` (`None` when
none remain), and a disabled Environment has no `environment_parameters` entry. `enableIf` is
not copied to `job::Environment` either (plan 3 Q11). `None` means every Job Environment,
which is what every Step of a Job that uses no `ON_REQUEST` Environment, no Environment
`enableIf` or `exclusiveGroup`, and no disabled `environment:` entry (one declines a `BY_DEFAULT` Environment,
Template Schemas §3.2 item 3.2) gets, so such a Job serializes as before; a created Job never holds
`Some` of the full list, so the field compares directly. `Step::enters_environment(document,
name)` answers for one Environment, and `Step::same_environments(&other)` compares the field
(prototype plan P3). A key rather than an index stays meaningful if a consumer filters the
Environment list. The `activation` itself is not copied to `job::Environment`: the Step's list,
not a property of the Environment, decides entry (plan P3 put it there for display; `openjd
summary` shows each Step's set from this field instead, so it is not needed).

Both fields are `#[serde(default, skip_serializing_if = "Option::is_none")]`, so a Job without
Environment parameters, `ON_REQUEST` Environments, Environment `enableIf`s, or disabled
`environment:` entries serializes exactly as before. They
participate in the derived `PartialEq` and `Hash` of `Step`.

The Job records only which Environments a Step enters, not why the others are left out. That
belongs to job creation, which returns it beside the Job in `CreatedJob::environment_statuses`
(see [public-api.md](public-api.md)): per Step, every Environment it considered in the Job's
Environment order, each `Entered`, `Declined`, `Disabled` or `Superseded` (with its
`exclusiveGroup`, and for a superseded one the group member entered instead), and its
`required: false` entries that named nothing. It is not a `Step` field because it describes the
decisions of one submission and no Session needs it, and keeping it out leaves the Job's
serialized form unchanged.

**Equality.** A Session may run Tasks of two Steps without exiting and re-entering Job Environments
between them only when they enter the same Job Environments (`Step::same_environments`) and their
effective values are equal (How Jobs Are Run, Environment Parameters). (Each Step's own
`step_environments` are entered and exited around its Tasks, as without the extension.)
`Step::environment_values_equal(&other)` implements §3.2.2 equality of the values: both Steps have
values for the same `(document, environment)` keys, and for each, the same parameter names with the
same types and the same string form, the text `{{Env.RawParam.<name>}}` gives the value. A value
compares by its transport form, which is that string form (for a list, the string forms of its
elements in order): a FLOAT, or an element of a `LIST[FLOAT]`, keeps the text it was written with,
so `3.10` and `3.1` are not equal although they are the same number, while `"3.10"` and `" 3.10 "`
are equal (the white space is not part of the text), and a FLOAT given as a YAML or JSON number is
written as a computed float is, so the number `1.50` equals `"1.5"` and not `"1.50"`, and the number
`2` is `2.0`; a STRING compares by code points, with no Unicode normalization; INT `1`, `"1"`, and
`"01"` are equal (all resolve to the INT 1) and BOOL `"yes"` equals `true`; PATH values compare by
unmapped string. The one exception is a `range_expr` value, which compares by the integers it
denotes, so `1-3` and `1,2,3` are equal even in a deserialized Job that holds another spelling
(§3.2.2; its string form is its canonical form, and job creation stores that, see
[job-creation.md](job-creation.md#environment-parameter-values-env_params)). A list that names one
`(document, environment)` twice is not one `create_job` produces; it compares unequal to every list,
including itself, on either side. So for the Steps of a Job from `create_job`, the derived `==` on
`environment_parameters` agrees with this comparison except for the order of the list.
`EnvironmentParameterValues::values_equal` applies the same rule to one Environment's values. A
scheduler can key Sessions on the transport form; it needs no numeric comparison. The comparison
assumes values in the form `create_job` writes them: a deserialized Job whose INT or BOOL values are
spelled another way (`"007"`, `"yes"`) compares them by that text, which can cost an unneeded exit
and enter but never shares an Environment between values a Session can tell apart. A FLOAT's stored
text is the text a Session renders, so for a FLOAT the comparison is exact.

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
    pub dependencies: Option<Vec<StepDependency>>,         // SERVICE (RFC 0009); the Services it lists, as written
    pub script: Option<EnvironmentScript>,
    pub variables: Option<HashMap<String, FormatString>>,  // Session-scope
    pub resolved_symtab: Option<SerializedSymbolTable>,
}

impl Environment {
    pub fn depends_on_services(&self) -> impl Iterator<Item = &str>; // the `service` entries' names, list order
    pub fn depends_on_service(&self, name: &str) -> bool;
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

A `job::Environment` records nothing about which Sessions enter it: there is no `runScope`
(RFC 0009 removed it). Each Step records the Job Environments its Sessions enter in
`Step::environments` and each Service in `Service::environments` (see
[Service](#service-rfc-0009-service-extension)), so a runtime reads entrance off the entity, not
the Environment. `dependencies` (Template Schemas §4 item 3) is carried as written,
like a Step's or a Service's: it is present only on a `jobEnvironments` entry or an attached
Environment Template's `environment` (a Step Environment never has one), every entry is
`service: <name>` naming a Service of the Environment's own document or, in the Job Template, a
required one, and `depends_on_services` is the list a runtime seeds the Environment's
`Service.<name>.<port>.*` symbols from — exactly the Services its format strings may reference.
No Service of a created Job enters an Environment that lists it: a Service whose Sessions would
enter one, by its `activation` or through its own `environment:` entry, is a cycle, which
template validation or job creation rejects.
The wrap hooks and `dependencies` use
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

`resolve_float_range` builds each element. A `<float>` literal, an element of a
whole-field range expression, and a list-element value with no spelling of its own go
through `Float64::new`, which renders via `format_float`. A `<floatstring>`, and a
list-element value that is an integer or a float carrying a spelling other than its
`format_float` rendering, go through
`Float64::with_str` after trimming, stripping redundant leading zeros (§7.5 rule 1) and
checking the text against `max_task_param_string_len`. Over that cap the element keeps only
its value, which is how it rendered before the text was carried at all. See
[job-creation.md](job-creation.md) for how list elements resolve.

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
    // u64 rather than usize, so a value above 2^32 is kept on 32-bit
    // targets (wasm32); see parameter-space.md, "Counts Are 64-bit".
    pub default_task_count: u64,
    pub target_runtime_seconds: Option<u64>,
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
whose entered Environments has requirements keeps its own, unchanged. Requirements that
resolved to nothing were discarded before combining and are not stored; `CreatedJob`
lists them per Step for display (Template Schemas §3.3).

### StepDependency

```rust
pub struct StepDependency {                                // Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize
    pub depends_on: Option<String>,                        // `dependsOn`: the Step named; omitted from JSON when None
    pub service: Option<String>,                           // `service`: the Service named (SERVICE); omitted when None
}

impl StepDependency {
    pub fn on_step(name: impl Into<String>) -> Self;
    pub fn on_service(name: impl Into<String>) -> Self;
    pub fn target(&self) -> Option<DependencyTarget<'_>>;  // None for both keys or neither (validation rejects)
    pub fn step(&self) -> Option<&str>;
    pub fn service(&self) -> Option<&str>;
}
impl From<&template::StepDependency> for StepDependency;

pub use template::DependencyTarget;                        // re-exported; see template-types.md
```

One entry of a Step's, a Service's, or a Job Environment's `dependencies` (Template Schemas
§3.2, §4 item 3, §9 item 4): `dependsOn: <StepName>`, satisfied when that Step has completed, or
`service: <ServiceName>` (SERVICE extension), satisfied when that Service is READY. Template
validation has rejected an entry giving both keys or neither, so exactly one is `Some` in a Job.
The entry is kept as written, so a created Job serializes its `dependencies` exactly as the
template did. `StepDependencyGraph::new` builds its Step-to-Step graph from the `dependsOn`
entries only: `service` entries are skipped, since the Services a Step waits for gate its Tasks
on READY rather than ordering it among Steps.

### Service (RFC 0009, `SERVICE` extension)

```rust
pub struct Service {
    pub name: String,
    pub description: Option<String>,
    pub document: Document,                                // §1.2.2 item 3; omitted from JSON when JobTemplate
    pub scope: ServiceScope,                               // §9.1, computed; {"kind":"allSteps"} | {"kind":"steps","steps":[..]}
    pub dependencies: Option<Vec<StepDependency>>,         // §9 item 4: dependsOn Steps and service Services; omitted from JSON when None
    pub host_requirements: Option<HostRequirements>,       // Resolved; combined with entered Environments' (§3.3.3)
    pub environment_parameters: Option<Vec<EnvironmentParameterValues>>, // ENV_PARAMS; omitted from JSON when None
    pub environments: Option<Vec<EnvironmentRef>>,         // Job Environments its Sessions enter; None = every one; omitted when None
    pub ports: Vec<ServicePort>,                           // Declaration order
    pub health_check: ServiceHealthCheck,                  // §9.4 defaults applied
    pub restart_policy: ServiceRestartPolicy,              // §9.5 maxRestarts resolved; completedTasks as given
    pub variables: Option<HashMap<String, FormatString>>,  // Service-execution scope, unresolved
    pub script: ServiceScript,                             // Service-execution scope, unresolved
    pub resolved_symtab: Option<SerializedSymbolTable>,
}

impl Service {
    pub fn enters_environment(&self, document: &Document, name: &str) -> bool; // environments is None, or lists (document, name)
    pub fn port_names(&self) -> impl Iterator<Item = &str>;
    pub fn depends_on_steps(&self) -> impl Iterator<Item = &str>;     // Step entries of `dependencies`, list order
    pub fn depends_on_services(&self) -> impl Iterator<Item = &str>;  // `service` entries' names, list order
    pub fn depends_on_service(&self, name: &str) -> bool;             // depends_on_services() contains `name`
    pub fn port(&self, name: &str) -> Option<&ServicePort>;
}

pub use template::ServiceScope;                            // re-exported; see template-types.md "Service scope"

pub struct ServiceRequirement {                            // §9.8, instantiated externalServices entry
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
    pub max_restarts: u64,
    pub completed_tasks: Option<CompletedTasksPolicy>,     // Some whenever max_restarts > 0; omitted from JSON when None
}

impl ServiceRestartPolicy {
    pub fn completed_tasks_on_relaunch(&self) -> Option<CompletedTasksPolicy>;        // None only with max_restarts 0
    pub fn completed_tasks_on_dependent_restart(&self) -> CompletedTasksPolicy;        // the value, or Rerun when None
    pub fn may_be_suspended(&self) -> bool;                                             // == Some(Keep)
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
fields (`port`, the four `<ServiceHealthCheck>` fields, `maxRestarts` — resolved with target
`int?`, range-checked, and defaulted per §9 when absent or `null`), and `hostRequirements`.
`completedTasks` has no default (§9.5 item 2): job creation requires it when the resolved
`maxRestarts` is greater than 0 (a literal one was checked at template validation), so
`completed_tasks` is `Some` for every Service that can be relaunched. It may be `None` only when
`max_restarts` is 0, in which case no relaunch happens and the value matters only when the
Service is stopped and started again because a Service it lists began a new Service Session —
`completed_tasks_on_dependent_restart` reads an omitted value as `RERUN` there (RFC 0009
"Dependents of a relaunched Service"), and `may_be_suspended` is true only for an explicit
`KEEP` (constraint 9).
`variables` and the whole `script` are `@fmtstring[host]` and stay `FormatString`s, exactly
as an Environment's do: they reference `Session.*`, `Service.File.*`, and in-scope
`Service.<name>.<port>.*` endpoints that only the Service Session can bind. Action `timeout`
and cancelation fields travel unresolved too, restricted to job-creation-stage symbols as on a
Step. `<Service>.let` itself is not carried (its values are), while `<ServiceScript>.let` is,
for the host to evaluate.

`scope` is the set of Steps whose Tasks depend on the Service (Template Schemas §9.1), computed
by `create_job` from the template's `dependencies` lists, with every `enableIf` decided: the
Steps that list it (rule 1); every Step in the scope of a Service that depends on it — lists it,
or whose Sessions enter a Job Environment that lists it — transitively (rule 2); and every Step
whose Sessions enter a Job Environment that lists it (rule 3: every Step for an `ALWAYS`
Environment, every Step that does not decline it for a `BY_DEFAULT` one, the requesting Steps for
an `ON_REQUEST` one, less any for which `enableIf` is `false`), which is `AllSteps` when that is every Step. `Steps { .. }` is never empty for a Job
created from a decoded template, because a Job Template Service with no Step in its scope is
unused and rejected (rule 4) — at template validation, or at job creation when an `enableIf`
expression decided it. An external Service's scope is
computed over the combined Job by the same rules in the submission stage of `create_job_with_environment_templates`; it is `Steps {}` when nothing in the
Job lists it, and such a Service is never started.

`environments` names the Job Environments the Service's Sessions enter (RFC 0009 "Services run
inside Environments"), in `Job::job_environments` order over the combined Job, attached
Environments included, chosen exactly as a Step's are: every `activation: ALWAYS` Environment and
every `BY_DEFAULT` one the Service does not decline with a disabled `environment:` entry, plus every
`ON_REQUEST` Environment an enabled `environment:` entry of the Service's own `dependencies`
requests, less any whose `enableIf` is `false` for the Service or that an earlier one of its
`exclusiveGroup` supersedes. A Service of an Environment Template chooses only among those of its
own document and of the Environment Templates attached before it. `None`
means every Job Environment, as for `Step::environments`; query it with `enters_environment`.
`environment_parameters` holds the Service's effective values (Template Schemas §3.2.2 applied to
the Service, RFC 0009 `<Service>` item 4) — one entry for each entered Environment that declares
`envParameterDefinitions`, in entry order, `None` when there is none — with the same
`EnvironmentParameterValues` shape as `Step::environment_parameters`. A Service Session enters
each Environment with these values (the CLI passes them as the sessions crate's
`ServiceSessionConfig::environment_parameters`); there is no Job-wide default set.
`host_requirements` are the Service's **combined** host requirements (RFC 0009 `<Service>` item
5, §3.3.3): its own and those of every Environment its Sessions enter, each resolved with the
Service's effective values and normalized per capability as for a Step; a conflict rejects the
submission naming the Service. A scheduler places the Service only on a host that satisfies them.

`dependencies` are the Steps and Services the Service depends on, as written (§9 item 4); its
`environment:` entries are not kept here (they become `environments` and
`environment_parameters`).
`depends_on_steps()` yields its Step entries — the Steps that must complete before it starts;
`depends_on_services()` yields the names of its `service` entries — the Services that must be
READY before it starts and that it is stopped before. A `service` name resolves within the
Service's own `document`: another Service of that document or, for a Job Template Service, a
required external Service (resolved through `CreatedJob::requirement_bindings`).
A scheduler therefore reads the lifecycle of every Service off the Job without
re-deriving it: start before the first Task of any Step in `scope`, once every Step of
`depends_on_steps()` has completed and every Service of `depends_on_services()` is READY; stop
once no Step in `scope` has a Task left, before any Service of `depends_on_services()`. `scope`
deserializes as `AllSteps` when absent.

`document` is the document of the submission that declares the Service (Template Schemas
§1.2.2 item 3, RFC 0009 "Service names are per document"): `Document::JobTemplate` for
every `services` entry `create_job` produces, and the attached Environment Template (by 0-based
attachment index, with the caller's label when it gave one) for an external Service, stamped by
`create_job_with_environment_templates`. Service names are unique within the list that declares them and
nothing more — an external Service may be named like a Service of the Job Template or of another
attachment (an error only when an `externalServices` entry names it) — so two Services of a
combined Job are the same Service iff `(document, name)` agree; a scheduler keys on that pair. A
`Service.*` reference resolves within the referencing entity's own document, except that the Job
Template's references to a required Service resolve to the attached Service the requirement was
bound to (`CreatedJob::requirement_bindings`). `Document` is `Ord` so it can
key a `BTreeMap`/`BTreeSet`; `Display` names the document as the submission-time error paths
do. The field is omitted from JSON for the Job Template's own Services (the default on
deserialization), so a Job without attachments serializes as before.

`resolved_symtab` is filtered like an Environment's: the symbols referenced by `variables`,
every script action (command, args, timeout, cancelation), embedded-file `data`, and
`<ServiceScript>.let` — which is how the `<Service>.let` values reach the host — with the
`RawParam.*` fallback for PATH parameters. A Service Session layers `Session.*`,
`Service.File.*`, and the `Service.*` endpoints from
`job::service_symbols::build_service_symbol_table` on top, passing as `in_scope` the Services of `depends_on_services()` — plus every required
external Service for a Job Template Service — and the Service itself as `declaring`. Validation
rejects a `Service.<name>.*` reference to any other Service, so a scheduler needs no walk of the
format strings to know which endpoints to bind.

`Service` implements `PartialEq` and `Hash` with the module's invariant; `variables` hashes as
key-sorted entries. It also implements `Deserialize`, so a created job's Services round-trip
through the wire format.

## Template → Job Type Mapping

| Template Type | Job Type | Key Differences |
|--------------|----------|----------------|
| `template::JobTemplate` | `job::Job` | `name` is `String` not `FormatString`; parameters carry resolved values; `parameters` is `IndexMap` |
| `template::StepTemplate` | `job::Step` | `name` resolved; `host_requirements` values resolved, and with ENV_PARAMS combined with the entered Environments' (§3.3.3); carries `resolved_symtab: Option<SerializedSymbolTable>`; `environment:` dependency entries, with `activation` and both `enableIf`s, become `environment_parameters` / `environments` (ENV_PARAMS); a Step Environment whose `enableIf` is `false` is dropped |
| `template::StepScript` | `job::StepScript` | Structurally identical; action fields remain `FormatString` |
| `template::Environment` | `job::Environment` | `variables` values remain `FormatString` (session-scope); `dependencies` carried as written; adds `resolved_symtab` |
| `template::Service` | `job::Service` | `let` evaluated into `resolved_symtab`; `port`, the `healthCheck` numeric fields, and `maxRestarts` are integers with defaults applied; `health_check`/`restart_policy` are non-optional with defaults applied; `host_requirements` resolved (with ENV_PARAMS, discarding as for a Step) and combined with the entered Environments' (§3.3.3); `environment:` dependency entries become `environments` / `environment_parameters` (ENV_PARAMS); `variables`/`script` remain `FormatString`; adds the computed `scope` and `document`; Step and Service `dependencies` entries kept as written |
| `template::ServiceRequirement` | `job::ServiceRequirement` | Structurally identical |
| `template::HostRequirements` | `job::HostRequirements` | `min`/`max` are `f64`; `any_of`/`all_of` are `Vec<String>`, from either `template::AttributeValues` form; with ENV_PARAMS a requirement that resolves to nothing is discarded, empty `amounts`/`attributes` lists are `None`, and a Step or Service left with none has `host_requirements: None` (Template Schemas §3.3) |
| `template::EmbeddedFile` | `job::EmbeddedFile` | `file_type` is `FileType` enum; `end_of_line` is `Option<EndOfLine>` enum |
| `template::CancelationMode` | `job::CancelationMode` | Both are enums with `Terminate` and `NotifyThenTerminate` variants |
| `template::StepParameterSpaceDefinition` | `job::StepParameterSpace` | Ranges resolved to concrete values; definitions keyed by name in `IndexMap` |
| `template::TaskParameterDefinition` | `job::TaskParameter` | Enum with resolved ranges and optional chunks |
