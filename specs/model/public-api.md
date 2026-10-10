# Public API

[README](README.md) · Public API

This document is the authoritative reference for the `openjd-model` crate's public
API. All public types, functions, and re-exports are listed here with their
signatures, organized by where they are visible from.

The crate implements the [2023-09 Template Schemas][spec-2023-09] specification,
a data-interchange format for describing renderable jobs in a
worker-host-agnostic way. An `openjd-model` caller's typical flow is:

1. Read a YAML or JSON template document.
2. Call [`decode_job_template`] or [`decode_environment_template`] to parse
   and validate it against the schema + declared extensions.
3. Build a [`ModelProfile`] and a [`JobParameterInputValues`] from user input.
4. Call [`preprocess_job_parameters`] to coerce inputs against the
   merged parameter definitions.
5. Call [`create_job`] with the preprocessed values and the
   [`ValidationContext`]. The result is an owned [`job::Job`] ready for
   the session runtime.
6. When the submission attaches Environment Templates, call
   [`apply_environment_templates`] with the Job, the templates in the
   scheduler's order, and the same preprocessed values; it runs the
   submission-time checks (RFC 0009 §1.2.2: requirement matching and the
   wrapping-Environment rule) and returns the external Services, the
   requirement bindings, and the attached Environments to fold into the
   Job. `RequirementBinding` is re-exported at the crate root beside
   `AppliedEnvironmentTemplates` and `AttachedEnvironmentTemplate`.

Beyond that core flow, the crate exposes low-level building blocks —
symbol-table construction, step dependency graphs, lazy parameter-space
iteration, capability lookup, and the full template/job type hierarchy —
so applications (submitters, validators, schedulers) can compose exactly
what they need without re-implementing the schema.

[spec-2023-09]: https://github.com/OpenJobDescription/openjd-specifications/wiki/2023-09-Template-Schemas

## Module Structure

```
openjd_model              — crate root re-exports, the main public surface
├── capabilities          — standard capability names + name validators
├── error                 — ModelError + structured validation errors
├── format_string         — re-exported from openjd_expr
├── job                   — instantiated (resolved) job types
│   ├── create_job        — pipeline functions + MergedParameterDefinition
│   ├── service_symbols   — Service.* / WrappedService.* symbol-table builders (RFC 0009)
│   ├── step_param_space  — StepParameterSpaceIterator
│   └── step_dependency_graph — StepDependencyGraph + friends
├── symbol_table          — re-exported from openjd_expr
├── template              — unresolved/parsed template types (public module)
│   └── parse             — decode_*_template + DocumentType + DecodedTemplate
└── types                 — enums, profile, validation context, parameter values
```

The decode entry points
(`decode_job_template`/`decode_environment_template`/`decode_template`/
`DecodedTemplate`/`DocumentType`) are also re-exported at the crate
root for convenience — they're the primary API and consistent with
the flat re-exports of `error::*` and `types::*`.

The structural template types — `template::JobTemplate`,
`template::EnvironmentTemplate`, `template::StepTemplate`,
`template::Environment`, `template::RunScope`, `template::EnvironmentScript`,
`template::EnvironmentActions`, `template::WrapHookScope`, `template::Action`,
`template::EmbeddedFile`, `template::StepScript`,
`template::Service`, `template::ServicePort`, `template::ServicePortProtocol`,
`template::ServiceHealthCheck`, `template::SERVICE_HEALTH_CHECK_NUMERIC_FIELDS`,
`template::ServiceRestartPolicy`,
`template::CompletedTasksPolicy`, `template::ServiceScript`,
`template::ServiceActions`,
`template::StepActions`, `template::CancelationMode`,
`template::HostRequirements`, `template::AmountRequirement`,
`template::AttributeRequirement`, `template::StepDependency`,
`template::DependencyTarget`,
`template::SimpleAction`, `template::Description`,
`template::ExtensionName`, `template::TaskParameterDefinition`
and its 5 per-variant inner struct types
(`IntTaskParameterDefinition`, `FloatTaskParameterDefinition`,
`StringTaskParameterDefinition`, `PathTaskParameterDefinition`,
`ChunkIntTaskParameterDefinition`),
`template::JobParameterDefinition` and its 12 per-variant inner
struct types (`JobStringParameterDefinition`, …,
`JobListListIntParameterDefinition`), `template::RangeConstraint`,
`template::IntRange`, `template::FloatRange`, `template::StringRange`,
`template::FloatRangeItem`, `template::IntOrFormatString`,
`template::ChunksDefinition`, `template::FlexInt`, `template::FlexFloat`,
the 11 `*UserInterface` struct types (`StringUserInterface`,
`IntUserInterface`, `FloatUserInterface`, `PathUserInterface`,
`BoolUserInterface`, `RangeExprUserInterface`,
`ListSimpleUserInterface`, `ListPathUserInterface`,
`ListIntUserInterface`, `ListFloatUserInterface`,
`HiddenOnlyUserInterface`), `template::FileFilter`, and
`template::StepParameterSpaceDefinition` — are all reachable as
`openjd_model::template::*`. Callers can read fields, write
function signatures that take `&template::StepTemplate`, and
pattern-match on `template::JobParameterDefinition` variants to
access per-variant fields.

The resolved `job::*` types are what most consumers need (format
strings evaluated, parameters bound). The `template::*` types are
useful for callers that want to inspect a template before
instantiation — for example, the `openjd-python` bindings expose
typed `template::*` pyclasses so Python tools can introspect job
templates.

## Entry Points at the Crate Root

### Parsing + Validation

```rust
pub fn decode_job_template(
    template: serde_json::Value,
    supported_extensions: Option<&[&str]>,
    caller_limits: &CallerLimits,
) -> Result<JobTemplate, ModelError>;

pub fn decode_environment_template(
    template: serde_json::Value,
    supported_extensions: Option<&[&str]>,
    caller_limits: &CallerLimits,
) -> Result<EnvironmentTemplate, ModelError>;

pub fn decode_template(
    template: serde_json::Value,
    supported_extensions: Option<&[&str]>,
    caller_limits: &CallerLimits,
) -> Result<DecodedTemplate, ModelError>;
```

Each function takes a generic JSON value (typically produced from YAML by
[`parse::document_string_to_object`]) and returns the parsed template struct
on success. `supported_extensions` is an allowlist of extension names the
application is willing to honor — template extensions not in this list are
rejected. `caller_limits` layers per-deployment policy on top of the
spec-defined limits: document/segment size caps, the opt-in resolved-value
caps (`max_resolved_arg_len`/`max_resolved_data_len`), and the expression
evaluation budgets. None of it is defined by the spec, and none of it can
loosen a spec limit.

[`decode_template`] auto-detects the template kind from
`specificationVersion` and dispatches to the matching decoder.

### Job Instantiation

```rust
pub fn create_job(
    job_template: &JobTemplate,
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
) -> Result<job::Job, ModelError>;

pub fn preprocess_job_parameters(
    job_template: &JobTemplate,
    input_values: &JobParameterInputValues,
    environment_templates: &[EnvironmentTemplate],
    path_options: &PathParameterOptions<'_>,
) -> Result<JobParameterValues, ModelError>;

pub fn merge_job_parameter_definitions(
    job_template: &JobTemplate,
    environment_templates: &[EnvironmentTemplate],
) -> Result<Vec<MergedParameterDefinition>, ModelError>;

pub fn build_symbol_table(
    params: &JobParameterValues,
) -> Result<SymbolTable, ModelError>;

pub fn evaluate_let_bindings(
    bindings: &[String],
    base: &SymbolTable,
    library: Option<&openjd_expr::FunctionLibrary>,
    path_format: openjd_expr::path_mapping::PathFormat,
    memory_limit: Option<usize>,
    operation_limit: Option<usize>,
) -> Result<SymbolTable, ModelError>;

pub fn convert_environment(env: &template::Environment) -> job::Environment;

/// A Step Environment: as `convert_environment`, with `run_scope` materialized
/// as `Some([Task])` (it gives no `runScope`; Template Schemas §4 item 4).
pub fn convert_step_environment(env: &template::Environment) -> job::Environment;

pub fn convert_environment_with_symtab(
    env: &template::Environment,
    symtab: Option<&SymbolTable>,
) -> job::Environment;

/// Which list an Environment is an entry of, for the job-creation check tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentKind { Job, Step }

/// RFC 0009 / Template Schemas §1.2.2: apply a submission's Environment
/// Templates to the Job `create_job` built from the Job Template alone.
pub fn apply_environment_templates(
    job: &job::Job,
    attached: &[AttachedEnvironmentTemplate<'_>],
    job_parameter_values: &JobParameterValues,
    caller_limits: &CallerLimits,
) -> Result<AppliedEnvironmentTemplates, ModelError>;

#[derive(Debug, Clone, Copy)]
pub struct AttachedEnvironmentTemplate<'a> {
    pub template: &'a EnvironmentTemplate,
    /// Replaces `EnvironmentTemplate[i]` as the document's name in errors.
    pub label: Option<&'a str>,
}

impl<'a> AttachedEnvironmentTemplate<'a> {
    pub fn new(template: &'a EnvironmentTemplate) -> Self;
    pub fn with_label(self, label: &'a str) -> Self;
}
impl<'a> From<&'a EnvironmentTemplate> for AttachedEnvironmentTemplate<'a>;

#[derive(Debug, Clone, PartialEq)]
pub struct AppliedEnvironmentTemplates {
    /// Attachment order, then each template's `services` order; each stamped
    /// with its attachment as `document` and with `scope: AllSteps`.
    pub external_services: Vec<job::Service>,
    /// The attached Service each `requiresServices` entry was matched to, in
    /// requirement order (§1.2.2 item 2).
    pub requirement_bindings: Vec<RequirementBinding>,
    /// Attachment order; services-only templates contribute none.
    pub environments: Vec<job::Environment>,
    /// The document of each `environments` entry, index for index.
    pub environment_documents: Vec<job::Document>,
}

impl AppliedEnvironmentTemplates {
    /// `environment_documents`, then `JobTemplate` for each of the Job's own
    /// `job_environments`: the documents of the combined list, index for index.
    pub fn combined_environment_documents(&self, job: &job::Job) -> Vec<job::Document>;
    /// `services` = external then the Job's own; `job_environments` =
    /// attached then the Job's own. Empty lists stay `None`. The bindings do
    /// not fold into the Job.
    pub fn into_combined_job(self, job: job::Job) -> job::Job;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequirementBinding {
    /// The requirement's `name` (also the bound Service's).
    pub requirement: String,
    /// The attached Environment Template declaring the bound Service.
    pub document: job::Document,
    pub service: String,
}
```

[`create_job`] is the high-level entry point: it resolves the job name
(template scope), instantiates every Service (RFC 0009: `<Service>.let`,
the numeric `@fmtstring` fields, `hostRequirements` — see "Services" in
[job-creation.md](job-creation.md)) and every step, runs the resolved-value
checks on the carried-forward session/task-scope fields
(action `command`/`args`, environment and Service `variables`,
embedded-file `data` — see the Resolved-Value Checks on Carried-Forward
Fields section of [job-creation.md](job-creation.md)), runs the final task-count
limit from [`CallerLimits`], and returns the complete [`job::Job`]. The
`ctx` it takes is a full [`ValidationContext`] — i.e. revision +
extensions + caller limits — and callers commonly get one from
[`JobTemplate::default_validation_context`]. The context's revision
must match the template's and its extensions must cover every
extension the template declares (enabling more is allowed);
`create_job` returns a `Compatibility` error otherwise. An application
that does not support an extension rejects such templates at decode
via `supported_extensions` instead. Every expression
evaluation it performs runs under the caller's evaluation budgets
(`CallerLimits::max_eval_memory_bytes` / `max_eval_operations`).

[`preprocess_job_parameters`] implements the parameter coercion and
default-value pipeline from spec §2: type coercion, constraint checks,
PATH resolution relative to the template directory and the current
working directory, merging of env-template parameters per §1.2.1.

[`merge_job_parameter_definitions`] exposes just the merge step —
useful for UIs that need to display the merged constraints before
collecting user input.

[`build_symbol_table`] and [`evaluate_let_bindings`] let callers
assemble symbol tables outside the `create_job` flow — for example,
to resolve a single format string during template editing.

[`convert_environment`] maps a template-time [`template::Environment`]
to its resolved [`job::Environment`] counterpart without running the
full `create_job` pipeline. This is used by the session runtime when
it needs a resolved environment shape for an environment template
that's being entered directly without a parent job.

[`convert_environment_with_symtab`] is the same conversion but freezes
a filtered copy of the given symbol table into the returned
environment's `resolved_symtab` — only the symbols the environment's
own format strings reference are retained (plus `RawParam.*` fallbacks
for PATH-typed parameters). The CLI uses this for `--environment`
templates so the environment's own `parameterDefinitions` resolve
inside its actions and RFC 0008 wrap hooks. Also available via the
`create_job::` module path.

[`apply_environment_templates`] is the second stage of a submission
(RFC 0009, Template Schemas §1.2.2 "Services from Environment Templates"):
given the Job that `create_job` built from the Job Template alone and the
scheduler-ordered Environment Templates, it runs the two submission-time
checks that relate documents only the scheduler sees together — requirement
matching (merge rule 2: each `requiresServices` entry matches exactly one
attached Service with its ports and protocols, recorded as a
[`RequirementBinding`]) and the wrapping-Environment rule (merge rule 4) —
reporting every violation as a `ModelValidation` error for the model name
`Submission` with paths rooted at the document (`JobTemplate`,
`EnvironmentTemplate[i]`, or the attachment's label); then instantiates each
template's `services` as **external Services** under that template's own
[`EnvironmentTemplate::profile`], each with `scope: AllSteps` and stamped
with its attachment as [`job::Service::document`] (merge rule 3: inline
Services shadow external ones, so an external Service may be named like a
Service of the Job Template or of another attachment and the pair
`(document, name)` identifies it), and converts its Environment with the
merged parameter table. [`AppliedEnvironmentTemplates::into_combined_job`]
places the external Services before the Job Template's `services` and
the attached Environments before its `jobEnvironments` (merge rule 1). A
caller that enters the attached Environments itself (the CLI's `run`)
reads the two lists instead. The function is also the one-call replacement
for the CLI's per-template `build_symbol_table` +
`convert_environment_with_symtab`, which stays available. See "apply_environment_templates"
in [job-creation.md](job-creation.md).

## Template Types (Unresolved)

These types represent a *parsed, validated* template where format strings
are still unevaluated. They're the output of `decode_*_template` and the
input to [`create_job`] / [`preprocess_job_parameters`].

```rust
pub struct JobTemplate {
    pub specification_version: String,
    pub schema: Option<String>,
    pub extensions: Option<Vec<ExtensionName>>,
    pub name: FormatString,
    pub description: Option<Description>,
    pub parameter_definitions: Option<Vec<JobParameterDefinition>>,
    pub job_environments: Option<Vec<template::Environment>>,
    /// RFC 0009 §1.1 item 8 — requires the `SERVICE` extension.
    pub services: Option<Vec<template::Service>>,
    /// RFC 0009 §1.1 item 9 — requires the `SERVICE` extension.
    pub requires_services: Option<Vec<template::ServiceRequirement>>,
    pub steps: Vec<template::StepTemplate>,
}
```

Methods on `JobTemplate`:

```rust
impl JobTemplate {
    pub fn name(&self) -> &FormatString;
    pub fn description(&self) -> Option<&str>;
    pub fn parameter_definitions_list(&self) -> &[JobParameterDefinition];
    /// Empty when `services` is absent.
    pub fn services(&self) -> &[template::Service];
    /// Empty when `requiresServices` is absent.
    pub fn requires_services(&self) -> &[template::ServiceRequirement];

    /// Build a ModelProfile from the template's declared
    /// specificationVersion + extensions. Entries in `extensions` that
    /// don't parse as a known `ModelExtension` are silently skipped.
    pub fn profile(&self) -> ModelProfile;

    /// Convenience: `ValidationContext::from_profile(self.profile())`.
    /// The context callers want when they just want to `create_job`
    /// whatever the template says.
    pub fn default_validation_context(&self) -> ValidationContext;
}
```

```rust
pub struct EnvironmentTemplate {
    pub specification_version: String,
    /// `$schema` — ignored, as on the Job Template.
    pub schema: Option<String>,
    pub extensions: Option<Vec<ExtensionName>>,
    pub parameter_definitions: Option<Vec<JobParameterDefinition>>,
    /// Optional since RFC 0009: at least one of `environment` or
    /// `services` must be present (enforced by validation).
    pub environment: Option<template::Environment>,
    /// RFC 0009 — requires the `SERVICE` extension; same list
    /// constraints as a Job Template's `services`, minus `dependencies`.
    pub services: Option<Vec<template::Service>>,
}

impl EnvironmentTemplate {
    pub fn environment(&self) -> Option<&template::Environment>;
    /// Empty when `services` is absent.
    pub fn services(&self) -> &[template::Service];
    /// This document's revision + `extensions` — the profile its own
    /// Environment and Services are evaluated under at submission
    /// (§1.2 item 3: an extension applies to the document that lists
    /// it). Counterpart of `JobTemplate::profile`.
    pub fn profile(&self) -> ModelProfile;
    /// `ValidationContext::from_profile(self.profile())`.
    pub fn default_validation_context(&self) -> ValidationContext;
}
```

### Environments

```rust
pub struct template::Environment {
    pub name: String,
    pub description: Option<Description>,
    /// RFC 0009 `dependencies` (§4 item 3) — requires the `SERVICE`
    /// extension. The Services this Environment lists with the `service`
    /// key, as written; permitted on a `jobEnvironments` entry and an
    /// Environment Template's `environment`, never on a `stepEnvironments`
    /// entry.
    pub dependencies: Option<Vec<StepDependency>>,
    /// RFC 0009 `runScope` (§4 item 4) — requires the `SERVICE` extension.
    /// Permitted on a `jobEnvironments` entry and an Environment Template's
    /// `environment`, never on a `stepEnvironments` entry. Plain strings so
    /// an unrecognized `<RunScopeName>` is a path-annotated validation
    /// error; query through `runs_in`.
    pub run_scope: Option<Vec<String>>,
    pub script: Option<template::EnvironmentScript>,
    pub variables: Option<HashMap<String, FormatString>>,
}

impl template::Environment {
    /// A Job Environment entered in Sessions of `kind`? The effective
    /// `runScope`: exactly the kinds named when given, else `[TASK]` when
    /// the Environment lists a Service, every kind otherwise; unknown names
    /// never match. (A Step Environment gives no `runScope`.)
    pub fn runs_in(&self, kind: RunScope) -> bool;
    /// The kinds this Environment is entered in, in `RunScope::ALL` order.
    pub fn effective_run_scope(&self) -> impl Iterator<Item = RunScope> + '_;
    /// The Service names `dependencies` lists with the `service` key, list order.
    pub fn listed_services(&self) -> impl Iterator<Item = &str> + '_;
    /// `dependencies` lists at least one Service.
    pub fn depends_on_service(&self) -> bool;
    /// Any format string references a `Service.*` value.
    pub fn references_service(&self) -> bool;
    /// `runScope` is absent and defaults to `[TASK]`.
    pub fn default_run_scope_is_task_only(&self) -> bool;
}

/// §4 item 4 `<RunScopeName>`: a kind of Session (RFC 0009).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum template::RunScope {
    Task,     // "TASK"
    Service,  // "SERVICE"
}

impl template::RunScope {
    pub const ALL: [RunScope; 2];   // [Task, Service]
    pub fn as_str(&self) -> &'static str;
}
impl Display for template::RunScope;
impl FromStr for template::RunScope { type Err = String; }

pub struct template::EnvironmentScript {
    pub let_bindings: Option<Vec<String>>,
    pub actions: template::EnvironmentActions,
    pub embedded_files: Option<Vec<template::EmbeddedFile>>,
}

pub struct template::EnvironmentActions {
    pub on_enter: Option<template::Action>,
    /// RFC 0008 — require `WRAP_ACTIONS`.
    pub on_wrap_env_enter: Option<template::Action>,
    pub on_wrap_task_run: Option<template::Action>,
    pub on_wrap_env_exit: Option<template::Action>,
    /// RFC 0009 — require both `WRAP_ACTIONS` and `SERVICE`.
    pub on_wrap_service_enter: Option<template::Action>,
    pub on_wrap_service_run: Option<template::Action>,
    pub on_wrap_service_health_check: Option<template::Action>,
    pub on_wrap_service_exit: Option<template::Action>,
    pub on_exit: Option<template::Action>,
}

impl template::EnvironmentActions {
    pub const ON_EXIT_DEFAULT_TIMEOUT_SECONDS: u64 = 300;
    pub const ON_WRAP_SERVICE_HEALTH_CHECK_DEFAULT_TIMEOUT_SECONDS: u64 = 30;
    /// onExit/onWrapEnvExit/onWrapServiceExit → Some(300);
    /// onWrapServiceHealthCheck → Some(30); anything else → None.
    pub fn default_timeout_seconds(action_name: &str) -> Option<u64>;
    /// The four RFC 0009 hooks, in lifecycle order.
    pub fn service_wrap_hooks(&self) -> [(&'static str, &Option<template::Action>); 4];
    pub fn has_any_service_wrap_hook(&self) -> bool;
    // Shared helpers (also on job::EnvironmentActions, with 5 slots / 3 hooks there):
    pub fn named_slots(&self) -> [(&'static str, &Option<template::Action>); 9];
    pub fn iter_named(&self) -> impl Iterator<Item = (&'static str, &template::Action)>;
    pub fn iter_actions(&self) -> impl Iterator<Item = &template::Action>;
    pub fn wrap_hooks(&self) -> [(&'static str, &Option<template::Action>, WrapHookScope); 7];
    pub fn has_any_action(&self) -> bool;
    pub fn has_any_wrap_hook(&self) -> bool;
}

/// The companion template variables a wrap hook exposes beside `WrappedAction.*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum template::WrapHookScope {
    EnvName,   // `WrappedEnv.Name`  — onWrapEnvEnter, onWrapEnvExit
    StepName,  // `WrappedStep.Name` — onWrapTaskRun
    Service,   // `WrappedService.*` — the onWrapService* hooks (RFC 0009)
}
```

`ExtensionName` and `Description` are constrained string newtypes
defined in the `template::constrained_strings` submodule (which is
crate-private as a path) but re-exported as `template::ExtensionName`
and `template::Description`. They reach the public surface as field
types on these structs and are nameable directly through the
`template::*` path.

### Services (`SERVICE` extension, RFC 0009)

A Job Template declares its Services in one `services` list. A Step or a Service depends on a
Service by listing `service: <name>` in its `dependencies` (Template Schemas §3.2); each
Service's scope is computed from those lists (`template::service_scope`, below). A
`<StepTemplate>` has no separate Service list.

```rust
/// What a `<StepDependency>` names (§3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum template::DependencyTarget<'a> {
    Step(&'a str),      // `dependsOn`: satisfied when the Step has completed
    Service(&'a str),   // `service`: satisfied when the Service is READY
}
impl<'a> template::DependencyTarget<'a> {
    pub fn step(self) -> Option<&'a str>;
    pub fn service(self) -> Option<&'a str>;
}
impl Display for template::DependencyTarget<'_>;   // "dependsOn: X" / "service: X"

/// §3.2: one entry of a Step's, a Service's, or a Job Environment's
/// `dependencies`: `dependsOn: <StepName>` or, with `SERVICE`, `service:
/// <ServiceName>`. Exactly one key must be present; validation reports both
/// or neither at the entry, and gates `service` without `SERVICE`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct template::StepDependency {
    pub depends_on: Option<String>,
    pub service: Option<String>,
}
impl template::StepDependency {
    pub fn on_step(name: impl Into<String>) -> Self;
    pub fn on_service(name: impl Into<String>) -> Self;
    pub fn target(&self) -> Option<template::DependencyTarget<'_>>;  // None for both keys or neither
    pub fn step(&self) -> Option<&str>;
    pub fn service(&self) -> Option<&str>;
    pub fn is_well_formed(&self) -> bool;
    pub fn describe(&self) -> String;   // "dependsOn: X" | "service: X" | "dependsOn: X, service: Y" | "{}"
}

/// Helpers over a `dependencies` list (a malformed entry yields nothing).
pub fn template::lists_service(dependencies: Option<&[StepDependency]>, name: &str) -> bool;
pub fn template::listed_service_names(dependencies: Option<&[StepDependency]>) -> impl Iterator<Item = &str>;   // list order
pub fn template::listed_step_names(dependencies: Option<&[StepDependency]>) -> impl Iterator<Item = &str>;      // list order
```

The `<Service>` types (Template Schemas §9) are:

```rust
pub struct template::Service {
    pub name: String,
    pub description: Option<Description>,
    pub let_bindings: Option<Vec<String>>,
    /// §9 item 4: the Steps (`dependsOn`) that complete before the Service
    /// starts and the Services (`service`; of the same document, or required)
    /// that are READY before it starts and are stopped after it.
    pub dependencies: Option<Vec<template::StepDependency>>,
    pub host_requirements: Option<template::HostRequirements>,
    pub ports: Vec<template::ServicePort>,
    pub health_check: Option<template::ServiceHealthCheck>,
    pub restart_policy: Option<template::ServiceRestartPolicy>,
    pub variables: Option<HashMap<String, FormatString>>,
    pub script: template::ServiceScript,
}

impl template::Service {
    /// Declared, or the §9 default `{ type: TCP_CONNECT }` on every TCP port.
    pub fn health_check(&self) -> ServiceHealthCheck;
    /// Declared, or the §9 default `{ maxAttempts: 0, completedTasks: RERUN }`.
    pub fn restart_policy(&self) -> ServiceRestartPolicy;
    pub fn port_names(&self) -> impl Iterator<Item = &str>;
    /// The ports whose `protocol` is TCP — a defaulted TCP_CONNECT's probe set.
    pub fn tcp_port_names(&self) -> impl Iterator<Item = &str>;
}

pub struct template::ServicePort {
    pub name: String,
    /// `<posinteger> | <posintstring>`, modeled like `<Action>.timeout`.
    pub port: Option<FormatString>,
    /// §9.3 item 3; default `TCP`. A literal, not a format string.
    #[serde(default)]
    pub protocol: template::ServicePortProtocol,
}

/// §9.8 — one `requiresServices` entry.
pub struct template::ServiceRequirement {
    pub name: String,
    pub ports: Vec<template::ServiceRequirementPort>,
}
impl template::ServiceRequirement {
    pub fn port_names(&self) -> impl Iterator<Item = &str>;
}

/// §9.8.1
pub struct template::ServiceRequirementPort {
    pub name: String,
    #[serde(default)]
    pub protocol: template::ServicePortProtocol,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum template::ServicePortProtocol {
    #[default]
    Tcp,
    Udp,
}

impl template::ServicePortProtocol {
    pub fn as_str(&self) -> &'static str;   // "TCP" | "UDP"
    pub fn is_default(&self) -> bool;       // `== Tcp`, for skip_serializing_if
}
impl Display for template::ServicePortProtocol;  // as_str()

/// Discriminated on `type`; the numeric fields are `@fmtstring`. STDOUT has
/// no `readiness_interval_seconds` (rejected as an unknown field).
pub enum template::ServiceHealthCheck {
    TcpConnect {
        ports: Option<Vec<String>>,
        readiness_interval_seconds: Option<FormatString>,
        readiness_timeout_seconds: Option<FormatString>,
        health_interval_seconds: Option<FormatString>,
        failure_threshold: Option<FormatString>,
    },
    Command {
        readiness_interval_seconds: Option<FormatString>,
        readiness_timeout_seconds: Option<FormatString>,
        health_interval_seconds: Option<FormatString>,
        failure_threshold: Option<FormatString>,
    },
    Stdout {
        readiness_timeout_seconds: Option<FormatString>,
        health_interval_seconds: Option<FormatString>,
        failure_threshold: Option<FormatString>,
    },
}

/// `["readinessIntervalSeconds", "readinessTimeoutSeconds", "healthIntervalSeconds", "failureThreshold"]`
pub const template::SERVICE_HEALTH_CHECK_NUMERIC_FIELDS: [&str; 4];

impl template::ServiceHealthCheck {
    pub const DEFAULT_TCP_CONNECT_READINESS_INTERVAL_SECONDS: u64 = 1;
    pub const DEFAULT_COMMAND_READINESS_INTERVAL_SECONDS: u64 = 5;
    pub const DEFAULT_READY_TIMEOUT_SECONDS: u64 = 300;
    pub const DEFAULT_HEALTH_INTERVAL_SECONDS: u64 = 30;
    pub const DEFAULT_FAILURE_THRESHOLD: u64 = 3;
    pub fn type_name(&self) -> &'static str;
    pub fn readiness_interval_seconds(&self) -> Option<&FormatString>;  // None for STDOUT
    pub fn readiness_timeout_seconds(&self) -> Option<&FormatString>;
    pub fn health_interval_seconds(&self) -> Option<&FormatString>;
    pub fn failure_threshold(&self) -> Option<&FormatString>;
    /// The four fields, named, in SERVICE_HEALTH_CHECK_NUMERIC_FIELDS order.
    pub fn numeric_fields(&self) -> [(&'static str, Option<&FormatString>); 4];
}
impl Default for template::ServiceHealthCheck;  // TcpConnect with every field None

#[derive(Default)]
pub struct template::ServiceRestartPolicy {
    pub max_attempts: Option<FormatString>,
    pub completed_tasks: Option<CompletedTasksPolicy>,
}

impl template::ServiceRestartPolicy {
    pub const DEFAULT_MAX_ATTEMPTS: i64 = 0;
    // `completed_tasks` has no default: required when `maxAttempts` > 0 (§9.5 item 2).
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum template::CompletedTasksPolicy {
    Keep,
    Rerun,
}

impl template::CompletedTasksPolicy {
    pub fn as_str(&self) -> &'static str;
}

pub struct template::ServiceScript {
    pub let_bindings: Option<Vec<String>>,
    pub actions: template::ServiceActions,
    pub embedded_files: Option<Vec<template::EmbeddedFile>>,
}

pub struct template::ServiceActions {
    pub on_enter: Option<template::Action>,
    pub on_run: template::Action,
    pub on_health_check: Option<template::Action>,
    pub on_exit: Option<template::Action>,
}

impl template::ServiceActions {
    pub const ON_HEALTH_CHECK_DEFAULT_TIMEOUT_SECONDS: u64 = 30;
    pub const ON_EXIT_DEFAULT_TIMEOUT_SECONDS: u64 = 300;
    /// onEnter/onRun → None; onHealthCheck → Some(30); onExit → Some(300).
    pub fn default_timeout_seconds(action_name: &str) -> Option<u64>;
    pub fn named_slots(&self) -> [(&'static str, Option<&template::Action>); 4];
    pub fn iter_named(&self) -> impl Iterator<Item = (&'static str, &template::Action)>;
    pub fn iter_actions(&self) -> impl Iterator<Item = &template::Action>;
}
```

All derive `Debug, Clone, Deserialize` with `#[serde(rename_all = "camelCase",
deny_unknown_fields)]`. The `job::*` counterparts are described under "Job Types".

#### Service scope (`template::service_scope`, Template Schemas §9.1)

```rust
pub mod template::service_scope;   // every item below except the reference-extraction functions is also re-exported from `template`

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ServiceScope {
    AllSteps,                              // {"kind":"allSteps"}
    Steps { steps: BTreeSet<String> },     // {"kind":"steps","steps":["A","B"]}
}
impl ServiceScope {
    pub fn steps<I, S>(names: I) -> Self where I: IntoIterator<Item = S>, S: Into<String>;
    pub fn all_steps_default() -> Self;    // the serde default
    pub fn is_all_steps(&self) -> bool;
    pub fn contains(&self, step: &str) -> bool;
    pub fn step_names(&self) -> Option<&BTreeSet<String>>;
}
impl Display for ServiceScope;             // "every Step" | "Step A" | "Steps A, B" | "no Step" (empty)

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputedServiceScope {
    pub name: String,
    pub scope: ServiceScope,
    pub depends_on_services: BTreeSet<String>, // inline Services it lists with `service`; sorted; excludes itself, required and undeclared names
    pub depends_on_steps: Vec<String>,         // Steps it lists, list order; undeclared names kept
    pub dependent_steps: Vec<String>,          // Steps listing it (rule 1), template order
    pub dependent_services: Vec<String>,       // Services listing it (rule 2), template order
    pub listed_by_job_environment: bool,       // a jobEnvironments entry lists it (rule 3)
}
impl ComputedServiceScope {
    pub fn is_unused(&self) -> bool;           // scope is Steps with no Step (rule 4)
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServiceScopes { /* by name */ }
impl ServiceScopes {
    pub fn get(&self, name: &str) -> Option<&ComputedServiceScope>;
    pub fn iter(&self) -> impl Iterator<Item = &ComputedServiceScope>;
    pub fn scope_of(&self, name: &str) -> ServiceScope;   // AllSteps for an undeclared name
}

/// A cycle in the combined Step/Service/Job-Environment dependency graph (§3.2 constraint 3, §9.9 item 10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceDependencyCycle {
    pub path: Vec<String>,                              // ["Step Use", "Service Indexer", "Step Use"]
    pub via_job_environment: Option<(String, String)>,  // (Job Environment, Service it lists) when a scope edge closed the cycle
}
impl Display for ServiceDependencyCycle;   // "dependencies contain a cycle: Step Use -> Service Indexer -> Step Use." (+ the Job Environment clause)

/// Scopes of every inline Service (rules 1–3), or the first cycle among the
/// Step→Step, Step→Service, Service→Step and Service→Service edges plus the
/// edge from every Step to each Service a Job Environment lists (rule 3).
/// `service` entries naming a `requiresServices` entry are not edges.
pub fn compute_service_scopes(jt: &JobTemplate) -> Result<ServiceScopes, ServiceDependencyCycle>;
/// A cycle among the Service dependencies of an Environment Template's Services.
pub fn service_dependency_cycle(services: &[template::Service]) -> Option<ServiceDependencyCycle>;
/// The Services of `services` that `dependencies` lists with the `service` key,
/// in declaration order — those whose `Service.<name>.<port>.port` /
/// `.connectAddress` the listing Step, Service, or Environment may reference.
pub fn listed_services<'a>(
    dependencies: Option<&'a [StepDependency]>,
    services: &'a [template::Service],
) -> impl Iterator<Item = &'a template::Service> + Clone + 'a;
/// The `requiresServices` entries that `dependencies` lists with the `service` key,
/// in declaration order — the required Services whose `port` / `connectAddress`
/// the listing Step, Service, or Job Environment may reference (§9.8 item 2).
/// Listing one grants access to its values and nothing more: its scope is every
/// Step regardless.
pub fn listed_requirements<'a>(
    dependencies: Option<&'a [StepDependency]>,
    requirements: &'a [template::ServiceRequirement],
) -> impl Iterator<Item = &'a template::ServiceRequirement> + Clone + 'a;
/// Reference extraction: not dependency edges. Used for the diagnostic naming
/// the missing `service: <name>` entry and for the rule that an Environment with
/// `SERVICE` in its explicit `runScope` may not reference one. Reachable only
/// through `template::service_scope::*`.
pub fn step_references(step: &template::StepTemplate) -> BTreeSet<String>;
pub fn environment_references(env: &template::Environment) -> BTreeSet<String>;
pub fn environment_references_service(env: &template::Environment) -> bool;
pub fn service_references(svc: &template::Service) -> BTreeSet<String>;
```

`template::Environment` gains `listed_services()`, `depends_on_service()`,
`references_service()` and `default_run_scope_is_task_only()` beside `runs_in` /
`effective_run_scope`, which report the effective `runScope` of a Job Environment (§4 item 4:
`[TASK]` by default for an Environment that lists a Service in `dependencies`; a Step
Environment gives no `runScope` and is entered only by its Step's Task Sessions).

### Job Parameter Definitions

```rust
/// Discriminated enum over every parameter type the spec allows. The
/// variants in CamelCase upper case reflect the spec's
/// UPPER_SNAKE_CASE `type` discriminator verbatim.
pub enum JobParameterDefinition {
    STRING(JobStringParameterDefinition),
    INT(JobIntParameterDefinition),
    FLOAT(JobFloatParameterDefinition),
    PATH(JobPathParameterDefinition),
    BOOL(JobBoolParameterDefinition),           // EXPR extension
    RANGE_EXPR(JobRangeExprParameterDefinition), // EXPR extension
    LIST_STRING(JobListStringParameterDefinition), // EXPR extension
    LIST_PATH(JobListPathParameterDefinition),   // EXPR extension
    LIST_INT(JobListIntParameterDefinition),     // EXPR extension
    LIST_FLOAT(JobListFloatParameterDefinition), // EXPR extension
    LIST_BOOL(JobListBoolParameterDefinition),   // EXPR extension
    LIST_LIST_INT(JobListListIntParameterDefinition), // EXPR extension
}
```

The inner struct types are reachable via the `template::*` path
(e.g. `openjd_model::template::JobStringParameterDefinition`).
Callers can pattern-match on the enum variants to read
per-variant fields directly:

```rust
use openjd_model::template::JobParameterDefinition;
match def {
    JobParameterDefinition::INT(p) => {
        let _name: &str = p.name.as_str();
        let _default: Option<i64> = p.default.as_ref().map(|f| f.0);
        let _min: Option<i64> = p.min_value.as_ref().map(|f| f.0);
    }
    _ => {}
}
```

Convenience accessors on the enum itself (used by the resolver
and by callers that don't need per-variant fields):

```rust
impl JobParameterDefinition {
    pub fn job_param_type(&self) -> JobParameterType;
    pub fn name(&self) -> &str;
    pub fn description(&self) -> Option<&str>;
    pub fn type_name(&self) -> &str;
    pub fn path_properties(&self) -> (Option<ObjectType>, Option<DataFlow>);
    pub fn default_value(&self) -> Option<String>;

    // Numeric constraints — None if not set on this definition or not
    // applicable to the parameter type.
    pub fn min_value_i64(&self) -> Option<i64>;
    pub fn max_value_i64(&self) -> Option<i64>;
    pub fn min_value_f64(&self) -> Option<f64>;
    pub fn max_value_f64(&self) -> Option<f64>;

    // Length constraints (STRING / LIST_*).
    pub fn min_length(&self) -> Option<usize>;
    pub fn max_length(&self) -> Option<usize>;

    // Allowed-values (STRING / INT / FLOAT).
    pub fn allowed_values_i64(&self) -> Option<Vec<i64>>;
    pub fn allowed_values_f64(&self) -> Option<Vec<f64>>;
    pub fn allowed_values_strings(&self) -> Option<Vec<String>>;

    /// Validate an already-coerced ExprValue against this definition's
    /// constraints. Used by `preprocess_job_parameters`.
    pub fn check_constraints(&self, value: &openjd_expr::ExprValue) -> Result<(), String>;
}
```

### Task Parameter Definitions

```rust
/// Discriminated enum on `type`. Again, variant names mirror the spec's
/// UPPER_SNAKE_CASE strings verbatim.
pub enum TaskParameterDefinition {
    INT(IntTaskParameterDefinition),
    FLOAT(FloatTaskParameterDefinition),
    STRING(StringTaskParameterDefinition),
    PATH(PathTaskParameterDefinition),
    CHUNK_INT(ChunkIntTaskParameterDefinition), // TASK_CHUNKING extension
}

impl TaskParameterDefinition {
    pub fn task_param_type(&self) -> TaskParameterType;
    pub fn name(&self) -> &str;
}
```

### Per-variant `TaskParameterDefinition` types

Each `TaskParameterDefinition` variant wraps a struct with a `name`
and a typed `range` (and, for `CHUNK_INT`, a `chunks` payload):

```rust
pub struct IntTaskParameterDefinition {
    pub name: Identifier,
    pub range: IntRange,
}
pub struct FloatTaskParameterDefinition {
    pub name: Identifier,
    pub range: FloatRange,
}
pub struct StringTaskParameterDefinition {
    pub name: Identifier,
    pub range: StringRange,
}
pub struct PathTaskParameterDefinition {
    pub name: Identifier,
    pub range: StringRange,
}
pub struct ChunkIntTaskParameterDefinition {
    pub name: Identifier,
    pub range: IntRange,
    pub chunks: ChunksDefinition,
}
```

### Range + Parameter-Space Shape Types

These appear as fields on the `TaskParameterDefinition::*` variants:

```rust
pub enum IntRange {
    List(Vec<FlexInt>),       // a concrete list of integers
    Expression(FormatString), // a range expression string to evaluate
}

pub enum StringRange {
    List(Vec<FormatString>),
    Expression(FormatString),
}

pub enum FloatRange {
    List(Vec<FloatRangeItem>),
    Expression(FormatString),
}

pub enum FloatRangeItem {
    Float(f64),
    FormatString(FormatString),
}

pub enum IntOrFormatString {
    Int(i64),
    FormatString(FormatString),
}

pub struct ChunksDefinition {
    pub default_task_count: IntOrFormatString,
    pub target_runtime_seconds: Option<IntOrFormatString>,
    pub range_constraint: RangeConstraint,
}

pub enum RangeConstraint {
    Contiguous,
    Noncontiguous,
}

pub struct StepParameterSpaceDefinition {
    pub task_parameter_definitions: Vec<TaskParameterDefinition>,
    pub combination: Option<String>,
}
```

`FlexInt` and `FlexFloat` are coercion-permissive newtypes around
`i64` and `f64` respectively — they accept either a JSON number or
a JSON string that parses as a number. Each has a `.0` field
holding the inner primitive value:

```rust
pub struct FlexInt(pub i64);
pub struct FlexFloat(pub f64);
```

### `userInterface` types

Each `JobParameterDefinition` variant exposes an optional
`user_interface` field carrying a typed UI hint struct. All variants
share three common fields (`control: Option<String>`,
`label: Option<String>`, `group_label: Option<String>`); some
variants add type-specific extras:

```rust
pub struct StringUserInterface {       /* common only */ }
pub struct IntUserInterface {          /* + single_step_delta: Option<FlexInt> */ }
pub struct FloatUserInterface {        /* + decimals: Option<FlexInt>, single_step_delta: Option<FlexFloat> */ }
pub struct PathUserInterface {         /* + file_filters: Option<Vec<FileFilter>>, file_filter_default: Option<FileFilter> */ }
pub struct BoolUserInterface {         /* common only */ }
pub struct RangeExprUserInterface {    /* common only */ }
pub struct ListSimpleUserInterface {   /* common only — used by LIST[STRING], LIST[BOOL] */ }
pub struct ListPathUserInterface {     /* + file_filters, file_filter_default (same as PathUserInterface) */ }
pub struct ListIntUserInterface {      /* + single_step_delta: Option<FlexInt> */ }
pub struct ListFloatUserInterface {    /* + decimals, single_step_delta (same as FloatUserInterface) */ }
pub struct HiddenOnlyUserInterface {   /* common only — used by LIST[LIST[INT]] */ }

pub struct FileFilter {
    pub label: String,
    pub patterns: Vec<String>,
}
```

The `control` field, when present, is one of `LINE_EDIT`,
`MULTILINE_EDIT`, `DROPDOWN_LIST`, `CHECK_BOX`, `HIDDEN`, or other
string values per the spec; the model preserves it as a free-form
`Option<String>` and leaves enforcement to consumers.

## Instantiated Job Types

These types, re-exported via the `job::` path, are the output of
[`create_job`]. They have no `FormatString` fields at the template-scope
level — all template-scope strings have been resolved to concrete values.
Session- and task-scope strings (in `script.actions`, `variables`,
embedded file contents, etc.) remain as [`FormatString`] for the session
runtime to resolve when worker state is available.

All types below implement `Debug`, `Clone`, `PartialEq`, and `Hash`
with the invariant `a == b ⇒ hash(a) == hash(b)`. Equality is
structural on the *created job*, not the source template: derived
state such as `resolved_symtab` participates, and since its transport
format preserves original float literals, jobs created from `1.0` vs
`1.00` parameter values compare unequal. Map-typed fields (`IndexMap`,
`HashMap`) compare order-insensitively and hash as key-sorted entries.
`f64` fields hash via `to_bits()` after normalizing `-0.0` to `0.0`,
consistent with `-0.0 == 0.0`. Types whose (transitive) fields include
`f64` — `Job`, `Step`, `Service`, `StepParameterSpace`, `TaskParameter`,
`HostRequirements`, `AmountRequirement` — implement `PartialEq` but
not `Eq`; the rest also implement `Eq`. `Job` implements `Serialize`;
every other type here implements `Serialize` and `Deserialize`.

```rust
pub use template::{
    CompletedTasksPolicy, DependencyTarget, RunScope, ServicePortProtocol, ServiceScope,
};   // re-exported as job::*

pub struct job::Job {
    pub name: String,
    pub description: Option<String>,
    pub extensions: Option<Vec<ModelExtension>>,
    pub parameters: IndexMap<String, JobParameter>,
    pub steps: Vec<Step>,
    pub job_environments: Option<Vec<Environment>>,
    /// RFC 0009 `services`, each with its computed scope; external Services
    /// first after `apply_environment_templates`. Omitted from JSON when `None`.
    pub services: Option<Vec<Service>>,
    /// RFC 0009 `requiresServices`. Omitted from JSON when `None`.
    pub requires_services: Option<Vec<ServiceRequirement>>,
}

impl job::Job {
    /// True when `extensions` contains `extension`.
    pub fn has_extension(&self, extension: ModelExtension) -> bool;
    /// `has_extension(ModelExtension::Service)`: only such a Job has
    /// `service` dependency entries, `services` or `requires_services`.
    pub fn service_active(&self) -> bool;
}

pub struct job::JobParameter {
    pub name: String,
    pub param_type: JobParameterType,
    pub value: openjd_expr::ExprValue,
}

pub struct job::Step {
    pub name: String,
    pub description: Option<String>,
    pub script: StepScript,
    pub step_environments: Option<Vec<Environment>>,
    pub parameter_space: Option<StepParameterSpace>,
    pub host_requirements: Option<HostRequirements>,
    pub dependencies: Option<Vec<StepDependency>>,
    /// Complete symbol table at step scope in JSON transport format.
    /// Contains Param.*, RawParam.*, Job.Name, Step.Name, and step-level
    /// let bindings. The session deserializes this with `PathFormat::host()`
    /// and layers Session.* and Task.* values on top at runtime.
    pub resolved_symtab: Option<SerializedSymbolTable>,
}

pub struct job::StepScript {
    pub let_bindings: Option<Vec<String>>,
    pub actions: StepActions,
    pub embedded_files: Option<Vec<EmbeddedFile>>,
}

pub struct job::StepActions {
    pub on_run: Action,
}

pub struct job::Action {
    pub command: FormatString,
    pub args: Option<Vec<FormatString>>,
    pub timeout: Option<FormatString>,
    pub cancelation: Option<CancelationMode>,
}

pub struct job::Environment {
    pub name: String,
    pub description: Option<String>,
    /// RFC 0009 `dependencies`: the Services this Environment lists with the
    /// `service` key, as written (a Job Environment's or an attached
    /// Environment Template's `environment`'s; never a Step Environment's).
    /// Omitted from JSON when `None`.
    pub dependencies: Option<Vec<StepDependency>>,
    /// RFC 0009 `runScope`: the kinds of Session this Environment is entered
    /// in; `None` = every kind; a Step Environment's is always `Some([Task])`.
    /// Omitted from JSON when `None`.
    pub run_scope: Option<Vec<RunScope>>,
    pub script: Option<EnvironmentScript>,
    pub variables: Option<HashMap<String, FormatString>>,
    /// Filtered symbol table with only the symbols this environment references.
    pub resolved_symtab: Option<SerializedSymbolTable>,
}

impl job::Environment {
    /// True iff this Environment is entered in Sessions of kind `kind`.
    pub fn runs_in(&self, kind: RunScope) -> bool;
    /// The Services listed in `dependencies` with the `service` key.
    pub fn depends_on_services(&self) -> impl Iterator<Item = &str>;
    /// `dependencies` lists `service: <name>`.
    pub fn depends_on_service(&self, name: &str) -> bool;
}

// Re-exported from `template` for the job-side types:
pub use template::{CompletedTasksPolicy, RunScope, ServicePortProtocol};

pub struct job::EnvironmentScript {
    pub let_bindings: Option<Vec<String>>,
    pub actions: EnvironmentActions,
    pub embedded_files: Option<Vec<EmbeddedFile>>,
}

pub struct job::EnvironmentActions {
    pub on_enter: Option<Action>,
    /// RFC 0008 — wraps inner environments' `onEnter` actions. Requires
    /// the `WRAP_ACTIONS` extension at template-validation time.
    pub on_wrap_env_enter: Option<Action>,
    /// RFC 0008 — wraps tasks' `onRun` actions. Requires the
    /// `WRAP_ACTIONS` extension at template-validation time.
    pub on_wrap_task_run: Option<Action>,
    /// RFC 0008 — wraps inner environments' `onExit` actions. Requires
    /// the `WRAP_ACTIONS` extension at template-validation time.
    pub on_wrap_env_exit: Option<Action>,
    /// RFC 0009 — in a Service Session, wrap the Service's `onEnter`,
    /// `onRun`, `onHealthCheck`, and `onExit`. Require `WRAP_ACTIONS` and
    /// `SERVICE` at template-validation time. Omitted from JSON when `None`.
    pub on_wrap_service_enter: Option<Action>,
    pub on_wrap_service_run: Option<Action>,
    pub on_wrap_service_health_check: Option<Action>,
    pub on_wrap_service_exit: Option<Action>,
    pub on_exit: Option<Action>,
}

impl job::EnvironmentActions {
    // Same shape as template::EnvironmentActions (9 slots, 7 wrap hooks):
    pub fn named_slots(&self) -> [(&'static str, &Option<Action>); 9];
    pub fn iter_named(&self) -> impl Iterator<Item = (&'static str, &Action)>;
    pub fn iter_actions(&self) -> impl Iterator<Item = &Action>;
    pub fn wrap_hooks(&self) -> [(&'static str, &Option<Action>, template::WrapHookScope); 7];
    pub fn has_any_action(&self) -> bool;
    pub fn has_any_wrap_hook(&self) -> bool;
    pub fn service_wrap_hooks(&self) -> [(&'static str, &Option<Action>); 4];
    pub fn has_any_service_wrap_hook(&self) -> bool;
}

pub struct job::EmbeddedFile {
    pub name: String,
    pub file_type: FileType,
    pub filename: Option<String>,
    pub data: Option<FormatString>,
    pub runnable: Option<bool>,
    pub end_of_line: Option<EndOfLine>,
}

pub enum job::CancelationMode {
    Terminate,
    NotifyThenTerminate { notify_period_in_seconds: Option<FormatString> },
}
```

### Parameter Space (Resolved)

```rust
pub struct job::StepParameterSpace {
    pub task_parameter_definitions: IndexMap<String, TaskParameter>,
    pub combination: Option<String>,
}

pub enum job::TaskParameter {
    Int { range: TaskParamRange<i64>, chunks: Option<ResolvedChunks> },
    Float { range: Vec<Float64> },
    String { range: Vec<String> },
    Path { range: Vec<String> },
    ChunkInt { range: TaskParamRange<i64>, chunks: ResolvedChunks },
}

pub enum job::TaskParamRange<T> {
    List(Vec<T>),
    RangeExpr(RangeExpr),
}

pub struct job::ResolvedChunks {
    pub default_task_count: usize,
    pub target_runtime_seconds: Option<usize>,
    pub range_constraint: RangeConstraint,
}
```

### Host Requirements (Resolved)

Host requirements in the resolved form have their `min`/`max` format
strings evaluated to concrete `f64`s:

```rust
pub struct job::HostRequirements {
    pub amounts: Option<Vec<AmountRequirement>>,
    pub attributes: Option<Vec<AttributeRequirement>>,
}

pub struct job::AmountRequirement {
    pub name: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

pub struct job::AttributeRequirement {
    pub name: String,
    pub any_of: Option<Vec<String>>,
    pub all_of: Option<Vec<String>>,
}

/// One entry of a Step's, a Service's, or a Job Environment's `dependencies`,
/// as written: `dependsOn: <StepName>` or `service: <ServiceName>`. Exactly
/// one key is `Some` (validation rejected both or neither).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct job::StepDependency {
    pub depends_on: Option<String>,   // omitted from JSON when None
    pub service: Option<String>,      // omitted from JSON when None
}
impl job::StepDependency {
    pub fn on_step(name: impl Into<String>) -> Self;
    pub fn on_service(name: impl Into<String>) -> Self;
    pub fn target(&self) -> Option<DependencyTarget<'_>>;
    pub fn step(&self) -> Option<&str>;
    pub fn service(&self) -> Option<&str>;
}
impl From<&template::StepDependency> for job::StepDependency;
```

### Services (Resolved; `SERVICE` extension, RFC 0009)

The instantiated form of a `services` entry (see `specs/model/job-types.md` for the
resolution rules):

```rust
pub struct job::Service {
    pub name: String,
    pub description: Option<String>,
    /// The document that declares this Service (Template Schemas §1.2.2 item
    /// 3): `JobTemplate` (the default; omitted from JSON) or the attached
    /// Environment Template, set by `apply_environment_templates`. Two
    /// Services are the same Service iff `(document, name)` agree.
    pub document: Document,
    /// §9.1: the Steps whose Tasks depend on the Service, computed from the
    /// template's `dependencies` lists and Job Environments; `AllSteps` for an
    /// external Service. Defaults to `AllSteps` when absent from JSON.
    pub scope: ServiceScope,
    /// §9 item 4, as written: `dependsOn` Steps and `service` entries naming a
    /// Service of the same document or a required one. Omitted from JSON when `None`.
    pub dependencies: Option<Vec<StepDependency>>,
    pub host_requirements: Option<HostRequirements>,
    pub ports: Vec<ServicePort>,
    pub health_check: ServiceHealthCheck,
    pub restart_policy: ServiceRestartPolicy,
    pub variables: Option<HashMap<String, FormatString>>,
    pub script: ServiceScript,
    /// Filtered symbol table with only the symbols this Service's
    /// host-resolved format strings reference (incl. `<Service>.let` values).
    pub resolved_symtab: Option<SerializedSymbolTable>,
}

impl job::Service {
    pub fn port_names(&self) -> impl Iterator<Item = &str>;
    /// The Steps it lists, in list order: it starts after each has completed.
    pub fn depends_on_steps(&self) -> impl Iterator<Item = &str>;
    /// The Services it lists with the `service` key, in list order: it
    /// starts after each is READY and is stopped before any of them.
    pub fn depends_on_services(&self) -> impl Iterator<Item = &str>;
    /// `depends_on_services()` contains `name`.
    pub fn depends_on_service(&self, name: &str) -> bool;
    pub fn port(&self, name: &str) -> Option<&ServicePort>;
}

/// §9.8 — an instantiated `requiresServices` entry.
pub struct job::ServiceRequirement {
    pub name: String,
    pub ports: Vec<ServiceRequirementPort>,
}
impl job::ServiceRequirement {
    pub fn port_names(&self) -> impl Iterator<Item = &str>;
}

/// §9.8.1
pub struct job::ServiceRequirementPort {
    pub name: String,
    /// `TCP` (default, omitted from JSON) or `UDP`.
    #[serde(default, skip_serializing_if = "ServicePortProtocol::is_default")]
    pub protocol: ServicePortProtocol,
}

/// The document of a submission that declares an entity. `Ord`, so it can
/// key a `BTreeMap`; `Default` is `JobTemplate`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum job::Document {
    #[default]
    JobTemplate,
    EnvironmentTemplate {
        /// 0-based attachment index, as in error paths.
        index: usize,
        /// The caller's label (omitted from JSON when `None`).
        label: Option<String>,
    },
}

impl job::Document {
    pub fn environment_template(index: usize, label: Option<&str>) -> Self;
    pub fn is_job_template(&self) -> bool;
}
/// `JobTemplate`, the label, or `EnvironmentTemplate[i]`.
impl Display for job::Document;

pub struct job::ServicePort {
    pub name: String,
    /// `None` when the runtime allocates the port (in `protocol`'s space).
    pub port: Option<u16>,
    /// `TCP` (default, omitted from JSON) or `UDP`.
    #[serde(default, skip_serializing_if = "ServicePortProtocol::is_default")]
    pub protocol: ServicePortProtocol,
}

#[serde(tag = "type")]
pub enum job::ServiceHealthCheck {
    #[serde(rename = "TCP_CONNECT")]
    TcpConnect {
        ports: Vec<String>,
        readiness_interval_seconds: u64,
        readiness_timeout_seconds: u64,
        health_interval_seconds: u64,
        failure_threshold: u64,
    },
    #[serde(rename = "COMMAND")]
    Command {
        readiness_interval_seconds: u64,
        readiness_timeout_seconds: u64,
        health_interval_seconds: u64,
        failure_threshold: u64,
    },
    #[serde(rename = "STDOUT")]
    Stdout {
        readiness_timeout_seconds: u64,
        /// `None` = no heartbeat expected (omitted from JSON).
        health_interval_seconds: Option<u64>,
        failure_threshold: u64,
    },
}

impl job::ServiceHealthCheck {
    pub fn type_name(&self) -> &'static str;
    pub fn readiness_interval_seconds(&self) -> Option<u64>;  // None for STDOUT
    pub fn readiness_timeout_seconds(&self) -> u64;
    pub fn health_interval_seconds(&self) -> Option<u64>;
    pub fn failure_threshold(&self) -> u64;
    /// `health_interval_seconds().is_some()`
    pub fn monitors_health(&self) -> bool;
}

pub struct job::ServiceRestartPolicy {
    pub max_attempts: u64,
    /// `Some` whenever `max_attempts > 0` (§9.5 item 2); `None` only with 0.
    /// Omitted from JSON when `None`.
    pub completed_tasks: Option<CompletedTasksPolicy>,
}
impl job::ServiceRestartPolicy {
    /// The policy for a relaunch after a failure; `None` only with `max_attempts` 0.
    pub fn completed_tasks_on_relaunch(&self) -> Option<CompletedTasksPolicy>;
    /// The policy when stopped and started again because a listed Service began
    /// a new Service Session: the value, or `Rerun` when none was given.
    pub fn completed_tasks_on_dependent_restart(&self) -> CompletedTasksPolicy;
    /// `completed_tasks == Some(Keep)` (constraint 10).
    pub fn may_be_suspended(&self) -> bool;
}

pub struct job::ServiceScript {
    pub let_bindings: Option<Vec<String>>,   // "let" wire key, alias "letBindings"
    pub actions: ServiceActions,
    pub embedded_files: Option<Vec<EmbeddedFile>>,
}

pub struct job::ServiceActions {
    pub on_enter: Option<Action>,
    pub on_run: Action,
    pub on_health_check: Option<Action>,
    pub on_exit: Option<Action>,
}

impl job::ServiceActions {
    pub fn named_slots(&self) -> [(&'static str, Option<&Action>); 4];
    pub fn iter_named(&self) -> impl Iterator<Item = (&'static str, &Action)>;
    pub fn iter_actions(&self) -> impl Iterator<Item = &Action>;
}
```

## Shared Types

Everything in this section is re-exported at the crate root from the
`types` module.

### Profile + Validation Context

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum SpecificationRevision {
    V2023_09,
}

#[non_exhaustive]
pub enum ModelExtension {
    TaskChunking,      // RFC 0001 — "TASK_CHUNKING"
    RedactedEnvVars,   // RFC 0003 — "REDACTED_ENV_VARS"
    FeatureBundle1,    // RFC 0004 — "FEATURE_BUNDLE_1"
    Expr,              // RFC 0005 — "EXPR"
    WrapActions,       // RFC 0008 — "WRAP_ACTIONS"
    Service,           // RFC 0009 — "SERVICE" (requires EXPR)
}

impl ModelExtension {
    pub const ALL: &'static [ModelExtension];
    pub fn as_str(&self) -> &'static str;
}

pub type Extensions = HashSet<ModelExtension>;

/// Model-side profile: spec revision + enabled extensions.
///
/// `ModelProfile` is the bridge to `openjd-expr`: call
/// `to_expr_profile(host_context)` to get an
/// `openjd_expr::ExprProfile` whose `FunctionLibrary::for_profile` will
/// match this template's declared rules.
#[derive(Debug, Clone)]
pub struct ModelProfile { /* private fields */ }

impl ModelProfile {
    pub fn new(revision: SpecificationRevision) -> Self;
    #[must_use] pub fn with_extensions(self, extensions: Extensions) -> Self;
    pub fn revision(&self) -> SpecificationRevision;
    pub fn extensions(&self) -> &Extensions;
    pub fn has_extension(&self, ext: ModelExtension) -> bool;

    /// Derive an `ExprProfile` for the given host context. Pass
    /// `HostContext::None` for template-scope work, `HostContext::Unresolved`
    /// for template-validation type checking, and
    /// `HostContext::WithRules(..)` for runtime evaluation with real path
    /// mapping.
    pub fn to_expr_profile(
        &self,
        host_context: openjd_expr::HostContext,
    ) -> openjd_expr::ExprProfile;
}

impl Default for ModelProfile { /* ... */ }
```

```rust
/// Caller-supplied limits beyond what the spec defines.
///
/// These tighten the spec-defined limits but can never loosen them.
/// Every field is optional; `None` means "no additional restriction."
#[derive(Debug, Clone, Default)]
pub struct CallerLimits {
    pub max_step_count: Option<usize>,
    pub max_env_count: Option<usize>,
    pub max_task_count: Option<u64>,
    pub max_step_script_size: Option<usize>,
    pub max_environment_size: Option<usize>,
    pub max_template_size: Option<usize>,
    /// Cap on any resolved string destined for a process argument: the
    /// action `command` (§5.1) and each argv entry an `args` element
    /// produces (§5.2, after null-skip / list-flatten). The spec sets no
    /// maximum but notes the OS imposes one. The cap counts characters,
    /// while OS limits use other units (Linux `MAX_ARG_STRLEN` is
    /// 131072 bytes; the Windows command line is 32767 UTF-16 units) —
    /// pick a value with encoding headroom. Enforced at validation on
    /// the guaranteed lower bound of every possible resolution, at job
    /// creation on the bound recomputed with the job parameters bound
    /// to real values, and at run time by `openjd-sessions` on the
    /// final values.
    pub max_resolved_arg_len: Option<usize>,
    /// Cap on each resolved embedded-file `data` value (§6.1.2 sets no
    /// spec limit). Same enforcement stages as
    /// `max_resolved_arg_len`.
    pub max_resolved_data_len: Option<usize>,
    /// Evaluation memory budget in bytes for each format-string
    /// expression (Expression Language "Memory-bounded evaluation").
    /// `None` = the spec-recommended default
    /// (`openjd_expr::DEFAULT_MEMORY_LIMIT`, 100 MB). Lowering it is
    /// spec-sanctioned; applied to every evaluation at template
    /// validation and at job creation, and at run time when mirrored
    /// into `SessionLimits`.
    pub max_eval_memory_bytes: Option<usize>,
    /// Evaluation operation budget per expression. `None` = the
    /// spec-recommended default (`openjd_expr::DEFAULT_OPERATION_LIMIT`,
    /// 10 million).
    pub max_eval_operations: Option<usize>,
}

/// The thing every validation and instantiation function takes — a
/// `ModelProfile` plus `CallerLimits`. Most callers construct one via
/// `JobTemplate::default_validation_context` or
/// `ValidationContext::new(revision)`.
#[derive(Debug, Clone)]
pub struct ValidationContext {
    pub profile: ModelProfile,
    pub caller_limits: CallerLimits,
}

impl ValidationContext {
    pub fn new(revision: SpecificationRevision) -> Self;
    pub fn with_extensions(revision: SpecificationRevision, extensions: Extensions) -> Self;
    pub fn from_profile(profile: ModelProfile) -> Self;
    #[must_use] pub fn with_caller_limits(self, caller_limits: CallerLimits) -> Self;
}
```

### Parameter Types

```rust
/// Job parameter type — the `type` field on a job parameter definition,
/// after `UPPER_SNAKE_CASE` → Rust-identifier conversion.
#[non_exhaustive]
pub enum JobParameterType {
    String, Int, Float, Path, Bool, RangeExpr,
    ListString, ListInt, ListFloat, ListPath, ListBool, ListListInt,
}

impl JobParameterType {
    pub fn from_spec_str(s: &str) -> Option<Self>;  // case-insensitive
    pub fn as_spec_str(&self) -> &'static str;
    pub fn expr_type(&self) -> openjd_expr::ExprType;
}

pub enum TaskParameterType {
    Int, Float, String, Path, ChunkInt,
}

impl TaskParameterType {
    pub fn from_spec_str(s: &str) -> Option<Self>;
    pub fn as_spec_str(&self) -> &'static str;
    pub fn expr_type(&self) -> openjd_expr::ExprType;
}
```

### Parameter Values

```rust
pub struct JobParameterValue {
    pub param_type: JobParameterType,
    pub value: openjd_expr::ExprValue,
}

pub struct TaskParameterValue {
    pub param_type: TaskParameterType,
    pub value: openjd_expr::ExprValue,
}

/// Input parameter values from the user. CLI callers typically pass
/// every value as `ExprValue::String(..)` and let
/// `preprocess_job_parameters` coerce to the target type; library
/// callers may pass typed values directly.
pub type JobParameterInputValues = HashMap<String, openjd_expr::ExprValue>;

/// Processed job parameter values (name → typed value).
pub type JobParameterValues = HashMap<String, JobParameterValue>;

/// A single task's parameter values, in insertion order.
pub type TaskParameterSet = IndexMap<String, TaskParameterValue>;
```

### Spec-String Enums

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FileType {
    Text,
}

#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EndOfLine {
    Lf, Crlf, Auto,
}

#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ObjectType {
    File, Directory,
}

#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DataFlow {
    None, In, Out, Inout,
}

pub enum TemplateSpecificationVersion {
    JobTemplate2023_09,      // "jobtemplate-2023-09"
    Environment2023_09,      // "environment-2023-09"
}

impl TemplateSpecificationVersion {
    pub fn as_str(&self) -> &'static str;
    pub fn is_job_template(&self) -> bool;
    pub fn is_environment_template(&self) -> bool;
    pub fn revision(&self) -> SpecificationRevision;
}

impl std::str::FromStr for TemplateSpecificationVersion { /* ... */ }
```

All implement `Display`. None (except `SpecificationRevision` and
`JobParameterType`) are currently `#[non_exhaustive]`; see the
future-revision-readiness report for the rationale and the list of
enums that should be marked before stable release.

## Parsing Module

```rust
pub mod parse {
    /// Document format.
    pub enum DocumentType { Json, Yaml }

    /// Maximum structural nesting depth for template documents.
    /// Matches serde_json's hardcoded recursion limit so YAML and JSON
    /// behave identically on deeply nested input.
    pub const MAX_DOCUMENT_DEPTH: usize = 128;

    /// Parse a document string into a generic JSON value, enforcing the
    /// caller-configured maximum document size (if any) before parsing.
    pub fn document_string_to_object(
        document: &str,
        doc_type: DocumentType,
        caller_limits: &CallerLimits,
    ) -> Result<serde_json::Value, ModelError>;

    // decode_* functions re-exported at the crate root (see above).

    /// Result of `decode_template`.
    pub enum DecodedTemplate {
        Job(JobTemplate),
        Environment(EnvironmentTemplate),
    }
}
```

## Error Types

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ModelError {
    /// Structural deserialization failure (bad YAML/JSON, missing
    /// fields, wrong types).
    DecodeValidation(String),

    /// Semantic validation failure — a template parsed but violates
    /// spec rules. The embedded `ValidationErrors` carries a per-field
    /// path for each problem.
    ModelValidation(ValidationErrors),

    /// Format string interpolation error with optional source position.
    FormatStringError {
        message: String,
        input: Option<String>,
        start: Option<usize>,
        end: Option<usize>,
    },

    /// Expression evaluation or symbol table error. Preserves the
    /// full `ExpressionError` with its kind and source context.
    Expression(openjd_expr::ExpressionError),

    /// Incompatible env-template parameter merges (§1.2.1).
    Compatibility(String),

    /// The `specificationVersion` field named a revision this library
    /// does not support.
    UnsupportedSchema(String),
}
```

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathElement {
    Field(String),
    Index(usize),
}

#[derive(Debug, Clone)]
pub struct ValidationError {
    pub path: Vec<PathElement>,
    pub message: String,
    pub detail: Option<ErrorDetail>,
}

#[derive(Debug, Clone)]
pub struct ErrorDetail {
    pub summary: String,
    pub spans: Vec<DiagnosticSpan>,
}

#[derive(Debug, Clone)]
pub struct DiagnosticSpan {
    pub summary: String,
    pub source: String,
    pub start: usize,
    pub end: usize,
    pub caret: usize,
}

#[derive(Debug, Default)]
pub struct ValidationErrors {
    pub errors: Vec<ValidationError>,
    // model_name is private (set by into_result)
}

impl ValidationErrors {
    pub fn single(msg: impl Into<String>) -> Self;
    pub fn add(&mut self, path: &[PathElement], msg: impl Into<String>);
    pub fn add_with_detail(
        &mut self,
        path: &[PathElement],
        msg: impl Into<String>,
        detail: ErrorDetail,
    );
    pub fn is_empty(&self) -> bool;
    pub fn len(&self) -> usize;
    pub fn into_result(self, model_name: &str) -> Result<(), ModelError>;
    pub fn format(&self, model_name: &str) -> String;
}

impl std::fmt::Display for ValidationErrors { /* ... */ }

// Path-construction helpers
pub fn path_field(base: &[PathElement], field: &str) -> Vec<PathElement>;
pub fn path_index(base: &[PathElement], index: usize) -> Vec<PathElement>;
```

Errors format to match Python's Pydantic output: `steps[0] -> script ->
actions -> onRun -> command:\n\t<message>`. The format is part of the
stability contract — tools that consume CLI output depend on it
across both the Python and Rust reference implementations.

## Job Creation Module

### `PathParameterOptions`

Used by [`preprocess_job_parameters`] to control how PATH parameters
are anchored and what sources are allowed.

```rust
pub struct PathParameterOptions<'a> {
    /// Directory containing the job template. Relative PATH defaults
    /// are joined to this.
    pub job_template_dir: &'a str,

    /// Current working directory. Relative PATH user values are joined
    /// to this.
    pub current_working_dir: &'a str,

    /// How path strings are interpreted. `PathFormat::host()` for
    /// local filesystem paths; `Posix` or `Windows` when paths
    /// originate from a known platform (e.g. cross-platform render
    /// farms).
    pub path_format: openjd_expr::path_mapping::PathFormat,

    /// If false, PATH defaults must be relative and within
    /// `job_template_dir`. If true, absolute defaults and `..`
    /// walk-up are permitted.
    pub allow_template_dir_walk_up: bool,

    /// If true, URI values (`scheme://...`) in PATH parameters are
    /// preserved as-is (requires EXPR). If false with EXPR, URIs are
    /// rejected. Without EXPR, the flag is ignored.
    pub allow_uri_path_values: bool,
}

impl<'a> PathParameterOptions<'a> {
    pub fn new(job_template_dir: &'a str, current_working_dir: &'a str) -> Self;
}
```

### `MergedParameterDefinition`

The output of [`merge_job_parameter_definitions`]. Constraints from a
job template and its environment templates are tightened per §1.2.1
(allowed-values intersected, min taking the max, max taking the min).

```rust
#[derive(Debug, Clone)]
pub struct MergedParameterDefinition {
    pub name: String,
    pub param_type: JobParameterType,
    pub default: Option<String>,
    pub object_type: Option<ObjectType>,
    pub data_flow: Option<DataFlow>,
    /// Name of the template that last defined/contributed to this parameter.
    pub source: String,
    // Merged constraint fields are crate-private; access via
    // `check_constraints`.
}

impl MergedParameterDefinition {
    /// Verify that merge produced a satisfiable set (§1.2.1 "No template
    /// may narrow another's constraints to the empty set").
    pub fn validate_satisfiable(&self) -> Result<(), ModelError>;

    pub fn check_constraints(&self, value: &ExprValue) -> Result<(), ModelError>;
    pub fn min_value_i64(&self) -> Option<i64>;
    pub fn max_value_i64(&self) -> Option<i64>;
    pub fn min_value_f64(&self) -> Option<f64>;
    pub fn max_value_f64(&self) -> Option<f64>;
    pub fn min_length(&self) -> Option<usize>;
    pub fn max_length(&self) -> Option<usize>;
    pub fn allowed_values_int(&self) -> Option<&[i64]>;
    pub fn allowed_values_float(&self) -> Option<&[f64]>;
    pub fn allowed_values_str(&self) -> Option<&[String]>;
}
```

### `job::service_symbols` (RFC 0009)

The runtime-facing side of the `Service.*` scope: how a Session runtime
(`openjd-sessions`, a scheduler) seeds the concrete endpoint values a
Service Session or Task Session needs, with the same key spellings and
types pass 8 and job creation type-checked against. The counterpart of
[`build_symbol_table`] for `Env.File.*`/`Param.*`.

```rust
pub const SERVICE_SCOPE: &str = "Service";
pub const SERVICE_FILE_PREFIX: &str = "Service.File";
pub const WRAPPED_SERVICE_SCOPE: &str = "WrappedService";

pub fn service_port_key(service: &str, port: &str) -> String;            // Service.<s>.<p>.port
pub fn service_bind_address_key(service: &str, port: &str) -> String;    // Service.<s>.<p>.bindAddress
pub fn service_connect_address_key(service: &str, port: &str) -> String; // Service.<s>.<p>.connectAddress
pub fn service_file_key(file_name: &str) -> String;                      // Service.File.<name>

/// The allocated endpoint of one port (Template Schemas §7.3.1; RFC 0009
/// "Address forms": addresses are bare hostnames / IPv4 / unbracketed IPv6).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ServiceEndpoint {
    pub port: u16,
    /// The protocol the number was allocated in; `TCP` (default, omitted from JSON) or `UDP`.
    #[serde(default, skip_serializing_if = "ServicePortProtocol::is_default")]
    pub protocol: ServicePortProtocol,
    pub bind_address: String,
    pub connect_address: String,
}

/// One Service's endpoints, in port declaration order.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ServiceEndpoints {
    pub name: String,
    pub ports: Vec<(String, ServiceEndpoint)>,
}

impl ServiceEndpoints {
    pub fn new(name: impl Into<String>, ports: Vec<(String, ServiceEndpoint)>) -> Self;
}

/// Seed `.port` (int) and `.connectAddress` (string), plus `.bindAddress`
/// when `include_bind_address` (the declaring Service's own Session only).
pub fn add_service_symbols(
    symtab: &mut SymbolTable,
    endpoints: &ServiceEndpoints,
    include_bind_address: bool,
) -> Result<(), ModelError>;

/// The `Service.*` table for one Session: every `in_scope` Service without
/// `bindAddress`, and `declaring` (a Service Session's own Service) with it.
pub fn build_service_symbol_table(
    in_scope: &[ServiceEndpoints],
    declaring: Option<&ServiceEndpoints>,
) -> Result<SymbolTable, ModelError>;

/// `WrappedService.Name` / `.PortNames` / `.Ports` / `.BindAddresses` /
/// `.Protocols` (Template Schemas §4.3.1) for the four `onWrapService*` hooks.
pub fn add_wrapped_service_symbols(
    symtab: &mut SymbolTable,
    endpoints: &ServiceEndpoints,
) -> Result<(), ModelError>;
```

Scope — which Services a Session may see — is the caller's decision (RFC
0009 "The `Service.*` scope"): a Task Session sees the inline Services whose
`scope` includes its Step and the attached Services bound to the Job
Template's `requiresServices`; a Service Session sees the Services of
`job::Service::depends_on_services()` (resolved within its `document`, a
required name to the bound attached Service) plus its own. A scheduler
starts a Service once every Service of `depends_on_services()` is READY and
every Step of `depends_on_steps()` has completed; pass 11 guarantees the
graph is acyclic. `Service.File.*` is seeded by the runtime's
embedded-file materialization using `service_file_key`.

### `convert_environment_with_symtab`

Re-exported at the crate root (see
[Entry Points at the Crate Root](#entry-points-at-the-crate-root)) and
also available via the `create_job::` module path:

```rust
/// Convert a template Environment to a job Environment, optionally
/// filtering the symbol table to only the symbols this environment
/// references. When `symtab` is `Some`, the returned Environment's
/// `resolved_symtab` field carries the filtered table so that the
/// session side can reconstruct this environment's context without
/// the whole job's state.
pub fn convert_environment_with_symtab(
    env: &template::Environment,
    symtab: Option<&SymbolTable>,
) -> job::Environment;
```

### `apply_environment_templates` (RFC 0009)

Re-exported at the crate root (signatures under [Job
Instantiation](#job-instantiation)) and also available via the
`create_job::` module path: `apply_environment_templates`,
`AttachedEnvironmentTemplate`, `AppliedEnvironmentTemplates`. Error
contract: a `ModelValidation` error for `Submission` collecting every
merge-rule-3 wrapper violation (at `<doc> -> environment`, `JobTemplate ->
jobEnvironments[i]`, or `JobTemplate -> steps[i] -> stepEnvironments[j]`)
before any Service is instantiated (merge rule 2 — Service names scoped to
their document — rejects nothing); thereafter a per-document error from
`instantiate_service` or the Environment re-checks, with the document
prefixed to its paths (validation errors, also reported for `Submission`)
or to its message (format-string and expression errors). Full messages
and the walk are in [job-creation.md](job-creation.md).

## Parameter Space Iteration

```rust
/// Lazy iterator over a resolved step parameter space.
///
/// Supports random access (`get(index)`) for non-adaptive spaces and
/// sequential iteration via `Iterator`. Construct from a
/// `job::StepParameterSpace` that `create_job` produced.
pub struct StepParameterSpaceIterator { /* private fields */ }

impl StepParameterSpaceIterator {
    pub fn new(space: &job::StepParameterSpace) -> Result<Self, ModelError>;

    /// Build with a one-task-per-chunk override. `Some(1)` disables
    /// adaptive chunking and lets the iterator count individual tasks.
    pub fn new_with_chunk_override(
        space: &job::StepParameterSpace,
        override_count: Option<usize>,
    ) -> Result<Self, ModelError>;

    pub fn names(&self) -> &HashSet<String>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;

    /// Random access. Returns `None` for out-of-bounds and for adaptive
    /// chunking, the one case where chunk N is not a function of N.
    /// A contiguous chunked space answers, though `Iterator::next` still
    /// walks it sequentially — see parameter-space.md.
    pub fn get(&self, index: usize) -> Option<TaskParameterSet>;

    pub fn contains(&self, params: &TaskParameterSet) -> bool;
    pub fn validate_containment(&self, params: &TaskParameterSet) -> Result<(), String>;

    /// True only for adaptive chunking (TASK_CHUNKING with
    /// `targetRuntimeSeconds`).
    pub fn chunks_adaptive(&self) -> bool;

    /// The chunked parameter and its chunk size, for any chunked space —
    /// adaptive or static, and reflecting a chunk override when one was
    /// given. `None` when the space has no chunked parameter.
    pub fn chunks_parameter_name(&self) -> Option<&str>;
    pub fn chunks_default_task_count(&self) -> Option<usize>;

    /// Adaptive-only: a static chunk size cannot change mid-walk, so this
    /// is a no-op for a non-adaptive space.
    pub fn set_chunks_default_task_count(&mut self, value: usize);

    /// Rewind the iterator so a fresh `Iterator::next` walk yields the
    /// same elements again. Preserves the adaptive chunk size set via
    /// `set_chunks_default_task_count`.
    pub fn reset(&mut self);
}

impl Iterator for StepParameterSpaceIterator {
    type Item = TaskParameterSet;
    fn next(&mut self) -> Option<TaskParameterSet>;
    fn size_hint(&self) -> (usize, Option<usize>);
}
```

Random-access indexing uses `O(1)` arithmetic on a product-of-factors
representation — submitters that want to shard a large parameter
space across workers can compute per-worker index slices without
iterating the whole space. Contiguous chunking costs an additional
walk over the range's intervals to locate the chunk's — O(R) in
sub-ranges for step-1 sub-ranges, but O(N) where a range is stepped or
heavily gapped, since each such value is its own interval.

Adaptive chunking (TASK_CHUNKING §4 / RFC 0001) is the one case that
forces sequential iteration, because chunk size depends on runtime
feedback; for that case, callers mutate
`set_chunks_default_task_count` while iterating to reshape chunks
dynamically.

## Step Dependency Graph

```rust
#[derive(Debug)]
pub struct StepDependencyEdge {
    pub origin: usize,     // index of the depended-upon step
    pub dependent: usize,  // index of the depending step
}

#[derive(Debug)]
pub struct StepDependencyNode {
    pub step_index: usize,
    pub name: String,
    pub in_edges: Vec<usize>,   // indices into the edges vector
    pub out_edges: Vec<usize>,
}

/// Directed acyclic graph over an instantiated job's steps. Built from
/// the `dependsOn` entries of `job::Job.steps[].dependencies`; `service`
/// entries (RFC 0009) are skipped.
#[derive(Debug)]
pub struct StepDependencyGraph { /* private fields */ }

impl StepDependencyGraph {
    pub fn new(job: &job::Job) -> Result<Self, ModelError>;
    pub fn node_count(&self) -> usize;
    pub fn step_node(&self, name: &str) -> Option<&StepDependencyNode>;
    pub fn node(&self, index: usize) -> Option<&StepDependencyNode>;
    pub fn edge(&self, index: usize) -> Option<&StepDependencyEdge>;
    pub fn max_indegree(&self) -> usize;
    pub fn max_outdegree(&self) -> usize;

    /// Stable topological sort matching the Python reference
    /// implementation: DFS-based, template order is the tiebreaker.
    /// Returns step indices. Fails with a descriptive cycle path if
    /// the graph is cyclic.
    pub fn topo_sorted(&self) -> Result<Vec<usize>, ModelError>;

    /// Convenience wrapper returning step names instead of indices.
    pub fn topo_sorted_names(&self) -> Result<Vec<String>, ModelError>;
}
```

## Capabilities

Standard capability names are tied to `(revision, extensions)` because
a future revision could introduce capabilities, and the built-in
`STANDARD_*` tables are per-revision. All accessors return a `Result`
so the function signature is forward-compatible for "this revision
doesn't have this capability" outcomes.

```rust
pub mod capabilities {
    /// Amount capability names. Today (2023-09, no extensions):
    /// `amount.worker.vcpu`, `amount.worker.memory`, `amount.worker.gpu`,
    /// `amount.worker.gpu.memory`, `amount.worker.disk.scratch`.
    pub fn standard_amount_capability_names(
        revision: SpecificationRevision,
        extensions: &Extensions,
    ) -> Result<&'static [&'static str], ModelError>;

    /// Attribute capability names (just the names).
    pub fn standard_attribute_capability_names(
        revision: SpecificationRevision,
        extensions: &Extensions,
    ) -> Result<Vec<&'static str>, ModelError>;

    /// Attribute capability names paired with their allowed value sets.
    /// Today: `("attr.worker.os.family", ["linux", "windows", "macos"])`,
    /// `("attr.worker.cpu.arch", ["x86_64", "arm64"])`, and (RFC 0009, not
    /// gated by SERVICE) `("attr.worker.preemptible", ["true", "false"])`.
    pub fn standard_attribute_capabilities(
        revision: SpecificationRevision,
        extensions: &Extensions,
    ) -> Result<&'static [(&'static str, &'static [&'static str])], ModelError>;

    /// Check that a string matches the grammar for an amount capability
    /// name. Does not check whether the name is a *standard* capability —
    /// user-defined capabilities are allowed.
    pub fn validate_amount_capability_name(name: &str) -> Result<(), String>;

    /// Same, for attribute capability names.
    pub fn validate_attribute_capability_name(name: &str) -> Result<(), String>;

    /// Check a single attribute capability *value* against §3.3.2.2 for the
    /// named capability. A standard capability is checked against its allowed
    /// set (case-insensitively) and nothing else; any other capability is
    /// checked against the identifier pattern and the 100-character limit.
    /// Pass the table from `standard_attribute_capabilities`.
    pub fn validate_attribute_capability_value(
        capability_name: &str,
        value: &str,
        standard_capabilities: &[(&str, &[&str])],
    ) -> Result<(), String>;
}
```

## Re-exports from `openjd-expr`

These appear in the crate's public API because they're used as field
types on template / job structs (`FormatString`) or as inputs to
instantiation functions (`SymbolTable`). Re-exporting them here means
downstream callers don't need to depend on `openjd-expr` directly for
common operations.

```rust
pub use openjd_expr::format_string;          // module
pub use openjd_expr::format_string::FormatString;
pub use openjd_expr::symbol_table;           // module
pub use openjd_expr::symbol_table::SymbolTable;
```

## Versioning and Stability Conventions

The crate targets the 2023-09 specification revision exclusively at
present. Per the future-revision-readiness report, the plumbing for a
second revision exists (`EffectiveLimits::from_context` dispatches on
revision, `validation::validate_*` dispatches on revision,
`decode_job_template` wraps its `from_value` call in a revision
match), but no second revision has been defined.

Enums that are marked `#[non_exhaustive]` today:

- `SpecificationRevision`
- `JobParameterType`
- `TemplateSpecificationVersion`
- `ModelExtension`
- `TaskParameterType`
- `FileType`
- `ModelError`

`EndOfLine`, `ObjectType`, and `DataFlow` are intentionally closed:
they represent decidable logical concepts (newline mode, filesystem
entity kind, data direction) whose sets of variants are not expected
to change.
