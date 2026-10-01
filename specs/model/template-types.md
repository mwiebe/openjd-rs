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
    pub job_services: Option<Vec<Service>>,                   // SERVICE extension (RFC 0009)
    pub steps: Vec<StepTemplate>,
}
```

Helper: `parameter_definitions_list()` returns `&[JobParameterDefinition]`, defaulting to
an empty slice when `parameter_definitions` is `None`.

### EnvironmentTemplate (§1.2)

```rust
pub struct EnvironmentTemplate {
    pub specification_version: String,
    pub schema: Option<String>,                              // $schema, ignored
    pub extensions: Option<Vec<ExtensionName>>,
    pub parameter_definitions: Option<Vec<JobParameterDefinition>>,
    pub environment: Option<Environment>,                    // optional since RFC 0009
    pub services: Option<Vec<Service>>,                      // SERVICE extension (RFC 0009)
}

impl EnvironmentTemplate {
    pub fn environment(&self) -> Option<&Environment>;
    pub fn services(&self) -> &[Service];   // empty when `services` is absent
}
```

An Environment Template defines an Environment, a list of Services (§1.2 item 6, §1.2.2
"external Services"), or both; validation rejects a document that defines neither. `services`
has the same list constraints as a Job Template's `jobServices` and is validated by the same
code (pass 11). `environment` is `Option` on the wire too: an explicit `environment: null` is
"not provided". The `$schema` property is accepted and ignored, as on the Job Template.

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
    pub step_services: Option<Vec<Service>>,          // SERVICE extension (RFC 0009)
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

Syntax sugar that expands into a `StepScript` with an embedded file and `onRun` action.
The `resolve_syntax_sugar()` method performs this expansion. A step must have either `script`
or exactly one simple action field — never both.

```rust
pub struct SimpleAction {
    pub let_bindings: Option<Vec<String>>,
    pub script: String,
    pub args: Option<Vec<FormatString>>,
    pub timeout: Option<FormatString>,
    pub cancelation: Option<CancelationMode>,
}
```

### StepDependency (§3.2)

```rust
pub struct StepDependency {
    pub depends_on: String,
}
```

## Environment (§4)

```rust
pub struct Environment {
    pub name: String,
    pub description: Option<Description>,
    pub run_scope: Option<Vec<String>>,       // runScope; SERVICE extension (RFC 0009)
    pub script: Option<EnvironmentScript>,
    pub variables: Option<HashMap<String, FormatString>>,
}

impl Environment {
    /// §4 item 3: entered in Sessions of `kind`? Every kind when `runScope` is
    /// absent; else exactly the kinds the list names (unknown names never match).
    pub fn runs_in(&self, kind: RunScope) -> bool;
    /// The kinds this Environment is entered in, in `RunScope::ALL` order.
    pub fn effective_run_scope(&self) -> impl Iterator<Item = RunScope> + '_;
}
```

### RunScope (§4 item 3 `<RunScopeName>`, `SERVICE` extension, RFC 0009)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RunScope {
    Task,     // "TASK" — Sessions that run Tasks
    Service,  // "SERVICE" — Service Sessions
}

impl RunScope {
    pub const ALL: [RunScope; 2];          // schema order: Task, Service
    pub fn as_str(&self) -> &'static str;
}
impl Display for RunScope;                 // the schema spelling
impl FromStr for RunScope;                 // Err(String) names the unknown value
```

`runScope` is held on `Environment` as `Option<Vec<String>>` rather than `Option<Vec<RunScope>>`
for the same reason `Service::name` is a plain `String`: the spec requires an unrecognized
`<RunScopeName>` to be rejected, and holding the raw text lets pass 11 report it with the
element's path (`runScope[i]`) instead of as a serde `unknown variant` error with no path.
Consumers query the effective scope through `runs_in`, so pass 8's `Service.*` scope
exclusion and the sessions runtime's Session-kind dispatch never re-derive
the "absent means every kind" rule. `RunScope` itself derives serde with the schema spelling,
and `job::Environment::run_scope` carries it as `Option<Vec<RunScope>>` (re-exported from
`job`), since validation has already rejected unrecognized names by then.

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

## Service (§9, `SERVICE` extension, RFC 0009)

A Service is a long-lived process with named TCP ports that a scheduler starts before any
Task in its scope is scheduled and keeps running for the lifetime of its scope: the Job for a
`jobServices` entry, the declaring Step for a `stepServices` entry. The types below are the
unresolved template shapes; the `Service.*` format-string scope and the `WrappedService.*`
wrap-hook variables are validated by pass 8 (see [validation.md](validation.md), "Service
scopes"), the runtime-facing symbol builders live in `job::service_symbols`, and job creation
produces `job::Service` (see [job-types.md](job-types.md) and
[job-creation.md](job-creation.md)).

```rust
pub struct Service {
    pub name: String,                                    // <ServiceName> §9.1: identifier, not "File"
    pub description: Option<Description>,
    pub let_bindings: Option<Vec<String>>,               // "let" field in YAML (EXPR)
    pub host_requirements: Option<HostRequirements>,     // same type as StepTemplate's
    pub service_environments: Option<Vec<Environment>>,  // §9 item 5: entered only in this Service's Session
    pub ports: Vec<ServicePort>,                         // 1–10, unique names
    pub readiness_check: Option<ServiceReadinessCheck>,  // None = { type: TCP_CONNECT } on all ports
    pub restart_policy: Option<ServiceRestartPolicy>,    // None = { maxAttempts: 0, completedTasks: RERUN }
    pub variables: Option<HashMap<String, FormatString>>, // same schema as Environment.variables
    pub script: ServiceScript,
}

impl Service {
    pub fn readiness_check(&self) -> ServiceReadinessCheck;  // declared, or the §9 default
    pub fn restart_policy(&self) -> ServiceRestartPolicy;    // declared, or the §9 default
    pub fn port_names(&self) -> impl Iterator<Item = &str>;
}
```

`name` (and `ServicePort::name`) is a plain `String` rather than `Identifier` so that the
identifier, length, and `File` constraints are reported with a field path by the validation
pipeline instead of as a serde error.

### serviceEnvironments (§9 item 5)

`service_environments` is the analogue of a Step's `stepEnvironments`: an ordered list of
ordinary `Environment`s entered only in this Service's Session — after the Environments of the
Service's scope whose `runScope` includes `SERVICE` and before `onEnter`, exited in reverse
after `onExit`. The entries reuse the `<Environment>` type unchanged; what differs is enforced
by validation and job creation rather than the type: `runScope` must not be provided (the
effective scope is `[SERVICE]`, which is what pass 10's hooks-follow-`runScope` rule uses, so a
wrapping Service Environment defines `onWrapEnvEnter`, `onWrapEnvExit`, and the four
`onWrapService*` hooks and wraps that one Service alone); names are unique within the list and
distinct from the Job Environments and, for a Step Service, the declaring Step's Step
Environments; and — unlike a Job or Step Environment entered in Service Sessions — a Service
Environment's format strings have the declaring Service's own `Service.*` scope, `bindAddress`
included (see [validation.md](validation.md), "Service scopes"). Job creation converts each
entry like a Job Environment, with its own `resolved_symtab` (see
[job-creation.md](job-creation.md), "Services").

### ServicePort (§9.2)

```rust
pub struct ServicePort {
    pub name: String,                 // identifier, not "File", unique within the Service
    pub port: Option<FormatString>,   // <posinteger> | <posintstring>, 1–65535; None = runtime allocates
}
```

### Numeric `@fmtstring` fields

`ServicePort::port`, `ServiceReadinessCheck`'s `timeoutSeconds` and `intervalSeconds`, and
`ServiceRestartPolicy::max_attempts` are `<posinteger> | <posintstring>` (or `<integer> |
<intstring>`) marked `@fmtstring`. They are modeled exactly like `<Action>.timeout`: the field
is an `Option<FormatString>`, a YAML integer is accepted and held as its decimal text, and a
format string is kept unevaluated. Unlike `<Action>.timeout`, a format string here is admitted
by `SERVICE` itself (the spec marks the fields `@fmtstring`), not by `FEATURE_BUNDLE_1`.
Validation range-checks a value that carries no expression (see [validation.md](validation.md),
pass 11) and statically checks one that does (pass 8, target type `int?`); job creation
resolves every value in the `<Service>.let` scope with the same target — a `null` result means
the field was not provided and the §9 default applies — and range-checks the result (see
[job-creation.md](job-creation.md), "Services").

### ServiceReadinessCheck (§9.3)

A discriminated union on `type`, derived with `#[serde(tag = "type", deny_unknown_fields)]`,
so a field belonging to another variant (`ports` on `STDOUT`, `intervalSeconds` on
`TCP_CONNECT`) is a deserialization error.

```rust
pub enum ServiceReadinessCheck {
    TcpConnect { ports: Option<Vec<String>>, timeout_seconds: Option<FormatString> },  // "TCP_CONNECT"
    Command { interval_seconds: Option<FormatString>, timeout_seconds: Option<FormatString> }, // "COMMAND"
    Stdout { timeout_seconds: Option<FormatString> },                                   // "STDOUT"
}

impl ServiceReadinessCheck {
    pub const DEFAULT_TIMEOUT_SECONDS: u64 = 300;
    pub const DEFAULT_COMMAND_INTERVAL_SECONDS: u64 = 5;
    pub fn type_name(&self) -> &'static str;                 // "TCP_CONNECT" | "COMMAND" | "STDOUT"
    pub fn timeout_seconds(&self) -> Option<&FormatString>;
}

impl Default for ServiceReadinessCheck;  // TcpConnect { ports: None, timeout_seconds: None }
```

### ServiceRestartPolicy (§9.4)

```rust
pub struct ServiceRestartPolicy {
    pub max_attempts: Option<FormatString>,              // <integer> | <intstring>, >= 0
    pub completed_tasks: Option<CompletedTasksPolicy>,   // None = RERUN
}

impl ServiceRestartPolicy {
    pub const DEFAULT_MAX_ATTEMPTS: i64 = 0;
    pub fn completed_tasks(&self) -> CompletedTasksPolicy;  // declared, or Rerun
}

impl Default for ServiceRestartPolicy;  // both fields None

#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CompletedTasksPolicy {
    Keep,
    #[default]
    Rerun,
}

impl CompletedTasksPolicy {
    pub fn as_str(&self) -> &'static str;  // "KEEP" | "RERUN"
}
```

### ServiceScript (§9.5) and ServiceActions (§9.6)

```rust
pub struct ServiceScript {
    pub let_bindings: Option<Vec<String>>,       // "let" field in YAML (EXPR)
    pub actions: ServiceActions,
    pub embedded_files: Option<Vec<EmbeddedFile>>,
}

pub struct ServiceActions {
    pub on_enter: Option<Action>,
    pub on_run: Action,                          // required: the process that is the service
    pub on_readiness_check: Option<Action>,      // iff readinessCheck.type is COMMAND
    pub on_exit: Option<Action>,
}

impl ServiceActions {
    pub const ON_READINESS_CHECK_DEFAULT_TIMEOUT_SECONDS: u64 = 30;
    pub const ON_EXIT_DEFAULT_TIMEOUT_SECONDS: u64 = 300;
    /// RFC 0009 `<Action>` default-timeout table: onEnter None, onRun None,
    /// onReadinessCheck Some(30), onExit Some(300); None for any other name.
    pub fn default_timeout_seconds(action_name: &str) -> Option<u64>;
    pub fn named_slots(&self) -> [(&'static str, Option<&Action>); 4];
    pub fn iter_named(&self) -> impl Iterator<Item = (&'static str, &Action)>;
    pub fn iter_actions(&self) -> impl Iterator<Item = &Action>;
}
```

The model crate does not apply action default timeouts itself — the sessions runtime does, as
it does for an Environment's `onExit` — so the Service defaults are recorded here as constants
for the runtime to consume.

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
    pub on_wrap_env_enter: Option<Action>,             // WRAP_ACTIONS (RFC 0008)
    pub on_wrap_task_run: Option<Action>,              // WRAP_ACTIONS (RFC 0008)
    pub on_wrap_env_exit: Option<Action>,              // WRAP_ACTIONS (RFC 0008)
    pub on_wrap_service_enter: Option<Action>,         // WRAP_ACTIONS + SERVICE (RFC 0009)
    pub on_wrap_service_run: Option<Action>,           // WRAP_ACTIONS + SERVICE (RFC 0009)
    pub on_wrap_service_readiness_check: Option<Action>, // WRAP_ACTIONS + SERVICE (RFC 0009)
    pub on_wrap_service_exit: Option<Action>,          // WRAP_ACTIONS + SERVICE (RFC 0009)
    pub on_exit: Option<Action>,
}

impl EnvironmentActions {
    pub const ON_EXIT_DEFAULT_TIMEOUT_SECONDS: u64 = 300;
    pub const ON_WRAP_SERVICE_READINESS_CHECK_DEFAULT_TIMEOUT_SECONDS: u64 = 30;
    /// §5 timeout table: onExit/onWrapEnvExit/onWrapServiceExit → Some(300),
    /// onWrapServiceReadinessCheck → Some(30), every other slot → None.
    pub fn default_timeout_seconds(action_name: &str) -> Option<u64>;
    /// The four RFC 0009 hooks, in lifecycle order.
    pub fn service_wrap_hooks(&self) -> [(&'static str, &Option<Action>); 4];
    pub fn has_any_service_wrap_hook(&self) -> bool;

    // Generated by `impl_environment_actions_helpers!` (shared with job::EnvironmentActions):
    pub fn named_slots(&self) -> [(&'static str, &Option<Action>); 9];   // declaration order
    pub fn iter_named(&self) -> impl Iterator<Item = (&'static str, &Action)>;
    pub fn iter_actions(&self) -> impl Iterator<Item = &Action>;
    pub fn wrap_hooks(&self) -> [(&'static str, &Option<Action>, WrapHookScope); 7];
    pub fn has_any_action(&self) -> bool;
    pub fn has_any_wrap_hook(&self) -> bool;   // the definition of a *wrapping* Environment
}

pub enum WrapHookScope {
    EnvName,   // `WrappedEnv.Name`  — onWrapEnvEnter, onWrapEnvExit
    StepName,  // `WrappedStep.Name` — onWrapTaskRun
    Service,   // `WrappedService.*` — the four onWrapService* hooks
}
```

The slots are enumerated once per struct in the `impl_environment_actions_helpers!` invocation
(`slots: [...]`, `wrap_hooks: [...]`); the array lengths above are derived from those lists.
The job-side `job::EnvironmentActions` is invoked with the same nine slots and seven hooks, and
`convert_environment` carries the RFC 0009 hooks and `runScope` across (typed as
`Vec<RunScope>` on the job side — see [job-types.md](job-types.md)).

The wrap-hook default timeouts follow the wrapped action: `onWrapEnvExit` takes `onExit`'s 300
seconds (sessions `env_script.rs`), and by the same rule `onWrapServiceExit` takes
`<ServiceActions>.onExit`'s 300 seconds and `onWrapServiceReadinessCheck` takes
`onReadinessCheck`'s 30 seconds. The spec's §5 table lists the `<ServiceActions>` defaults but
has no rows for the `onWrapService*` hooks; the values here are the analogy, recorded as
constants for the runtime to consume.

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
inside wrap hooks. A wrap hook's `timeout` and `cancelation` fields validate
against the same scope as its `command` and `args` — `WrappedAction.*` plus the
hook's companion group (`WrappedEnv.Name`, `WrappedStep.Name`, or the RFC 0009
`WrappedService.*`) — because the runtime resolves all of a hook's fields
against one symbol table. Any format string is accepted — normal interpolation like
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
