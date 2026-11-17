# Public API

[README](README.md) · Public API

This document is the reference for the `openjd-model` crate's public API. It
describes the public types, functions, and re-exports and their signatures,
organized by where they are visible from. The generated, CI-checked
[`public-api.txt`](public-api.txt) is the complete list of public items and the
authority on their exact signatures. Where the two disagree, this document is
wrong.

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
   [`ValidationContext`], or, with `ENV_PARAMS` and Environment Templates
   attached to the submission, [`create_job_with_environment_templates`].
   The result is an owned [`job::Job`] ready for the session runtime.
   With `SERVICE` (RFC 0009 §1.2.2), `create_job_with_environment_templates`
   also runs the submission-time checks (requirement matching and the
   wrapping-Environment rule), instantiates the attached templates'
   Services as external Services ahead of the Job Template's own, and
   returns the requirement bindings in [`CreatedJob`];
   [`create_job_details`] is `create_job` returning the whole `CreatedJob`
   without those checks. `RequirementBinding` is re-exported at the crate
   root beside `CreatedJob` and `AttachedEnvironmentTemplate`.

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
│   ├── create_job        — pipeline functions + MergedParameterDefinition,
│   │                       create_job_with_environment_templates + its types,
│   │                       check_environment_templates
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
`template::Environment`, `template::EnvironmentActivation`, `template::EnvironmentScript`,
`template::EnvironmentActions`, `template::WrapHookScope`, `template::Action`,
`template::EmbeddedFile`, `template::StepScript`,
`template::Service`, `template::ServicePort`, `template::ServicePortProtocol`,
`template::ServiceHealthCheck`, `template::SERVICE_HEALTH_CHECK_NUMERIC_FIELDS`,
`template::ServiceRestartPolicy`,
`template::CompletedTasksPolicy`, `template::ServiceScript`,
`template::ServiceActions`,
`template::StepActions`, `template::CancelationMode`,
`template::HostRequirements`, `template::AmountRequirement`,
`template::AttributeRequirement`, `template::AttributeValues` (an
attribute's `anyOf` or `allOf`: `List` of format strings, or, with
ENV_PARAMS, one `Expression`; see [template-types.md](template-types.md)),
`template::StepDependency`,
`template::DependencyTarget`, `template::StepKey`, `template::ExprField`,
`template::EnvironmentActivation`,
`template::EnvironmentParameterDefinition`,
`template::EnvironmentParameterValue`, `template::EnvironmentParameterValueMap`,
`template::SimpleAction`, `template::SimpleActionKind`, `template::Description`,
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
access per-variant fields. `template::AmountRequirement::may_be_discarded`
and `template::AttributeRequirement::may_be_discarded` say whether job
creation can discard the requirement in a document that lists ENV_PARAMS
(Template Schemas §3.3), judged from the text alone and conservatively
(an expression bound counts as one that may resolve to `null`), and
`template::AttributeValues` has `as_list` and `is_literal` accessors.

The resolved `job::*` types are what most consumers need (format
strings evaluated, parameters bound). The `template::*` types are
useful for callers that want to inspect a template before
instantiation — for example, the `openjd-python` bindings expose
typed `template::*` pyclasses so Python tools can introspect job
templates.

`template::StepTemplate` additionally exposes the SimpleAction (§8,
FEATURE_BUNDLE_1) surface:

```rust
impl StepTemplate {
    /// Each SimpleAction field the step sets, with its kind, in
    /// `SimpleActionKind::ALL` order.
    pub fn simple_actions(&self) -> impl Iterator<Item = (SimpleActionKind, &SimpleAction)>;
    pub fn simple_action(&self, kind: SimpleActionKind) -> Option<&SimpleAction>;
    /// `script.clone()` when present, otherwise the first SimpleAction
    /// desugared; `None` when the step has neither. Infallible —
    /// `SimpleAction.script` is already a parsed `FormatString`.
    pub fn resolve_syntax_sugar(&self) -> Option<StepScript>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SimpleActionKind { Python, Bash, Cmd, Powershell, Node }

impl SimpleActionKind {
    pub const ALL: [SimpleActionKind; 5];
    pub fn field_name(self) -> &'static str;
    pub fn file_extension(self) -> &'static str;
    pub fn arg_prefix(self) -> &'static [&'static str];
    pub fn synthetic_arg_count(self) -> usize;
    pub fn desugar(self, step_name: &str, sa: &SimpleAction) -> StepScript;
    /// Map a validation-error path on the desugared `StepScript` (relative
    /// to its `script` node) back onto the authored sugar field.
    pub fn remap_desugared_path(self, rel: &[PathElement]) -> Vec<PathElement>;
}
```

See `specs/model/template-types.md` § SimpleAction for the desugaring
rules and `specs/model/validation.md` § Reporting on desugared forms for
the path table.

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

pub fn convert_environment_with_symtab(
    env: &template::Environment,
    symtab: Option<&SymbolTable>,
) -> job::Environment;

// Job creation with the Environment Templates attached to the submission
// (ENV_PARAMS, Template Schemas §3.2.1–§3.2.2, §7.4.1; SERVICE §1.2.2).
// `create_job` is this with no attachments, returning only the Job and
// skipping the SERVICE submission checks; `create_job_details` is
// `create_job` returning the whole `CreatedJob`.
pub fn create_job_with_environment_templates(
    job_template: &JobTemplate,
    attached: &[AttachedEnvironmentTemplate<'_>],
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
) -> Result<CreatedJob, ModelError>;
pub fn create_job_details(
    job_template: &JobTemplate,
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
) -> Result<CreatedJob, ModelError>;

#[derive(Debug, Clone, Copy)]
pub struct AttachedEnvironmentTemplate<'a> {
    pub template: &'a EnvironmentTemplate,
    pub label: Option<&'a str>,     // names the document in messages
}
impl<'a> AttachedEnvironmentTemplate<'a> {
    #[must_use] pub fn new(template: &'a EnvironmentTemplate) -> Self;
    #[must_use] pub fn with_label(self, label: &'a str) -> Self;
}
impl<'a> From<&'a EnvironmentTemplate> for AttachedEnvironmentTemplate<'a>;

#[derive(Debug, Clone, PartialEq)]
pub struct CreatedJob {
    /// The combined Job: attached Environments first, in attachment order,
    /// then the Job Template's `jobEnvironments` (the Job's Environment
    /// order, Template Schemas §1.1 item 7); with SERVICE, the external
    /// Services first (attachment order, then each template's `services`
    /// order, each with its §9.1 scope over the combined Job and its
    /// `document`), then the Job Template's `services`.
    pub job: job::Job,
    /// The document of each `job.job_environments` entry, index for index.
    pub environment_documents: Vec<job::Document>,
    pub warnings: Vec<SubmissionWarning>,
    /// Per Step of `job`, index for index, the Environments an `enableIf`
    /// on the Environment (§4 item 6) disabled for it, or an earlier one
    /// of its `exclusiveGroup` superseded (§4 item 7), for display.
    pub disabled_environments: Vec<StepDisabledEnvironments>,
    /// Per Step of `job`, index for index, every Environment the Step
    /// considered, in the Job's Environment order, with whether its
    /// Sessions enter it or why not, and its `required: false` entries
    /// that named no Environment. `disabled_environments` is its
    /// `Disabled` and `Superseded` items.
    pub environment_statuses: Vec<StepEnvironmentStatuses>,
    /// Per Service of `job.services` (RFC 0009), index for index, the same
    /// statuses: a Service selects Environments exactly as a Step does. A
    /// Job Template's Service considers every Environment of the combined
    /// Job, an Environment Template's only those of its own document and of
    /// the Environment Templates attached before it; neither has Step
    /// Environments. Empty for a Job without Services.
    pub service_environment_statuses: Vec<StepEnvironmentStatuses>,
    /// Per Step of `job`, index for index, the host requirements that
    /// resolved to nothing and were discarded (ENV_PARAMS, §3.3): the
    /// Step's own first, then each entered Environment's, in Job order.
    pub discarded_host_requirements: Vec<Vec<DiscardedHostRequirement>>,
    /// The same per Service of `job.services` (RFC 0009), index for index.
    pub discarded_service_host_requirements: Vec<Vec<DiscardedHostRequirement>>,
    /// The attached Service each `externalServices` entry was matched to, in
    /// requirement order (RFC 0009 §1.2.2 item 2); empty from
    /// `create_job_details`.
    pub requirement_bindings: Vec<RequirementBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscardedHostRequirement {
    /// The Environment that held it; `None` for the Step's or Service's own.
    pub environment: Option<job::EnvironmentRef>,
    pub kind: HostRequirementKind,
    /// The resolved capability name.
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostRequirementKind { Amount, Attribute }

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepDisabledEnvironments {
    /// Combined Job Environments, attached first, by (document, name).
    pub environments: Vec<job::EnvironmentRef>,
    /// The Step's own `stepEnvironments`, by name, in written order.
    pub step_environments: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepEnvironmentStatuses {
    /// Attached Environments in attachment order, then `jobEnvironments`,
    /// then the Step's `stepEnvironments`, each in written order; only the
    /// ones the Step considered.
    pub environments: Vec<StepEnvironmentStatus>,
    /// Enabled `required: false` entries that resolved to no Environment,
    /// in `dependencies` order.
    pub not_found: Vec<OptionalEntryNotFound>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StepEnvironmentStatus {
    pub name: String,
    pub document: job::Document,      // EnvironmentTemplate exactly for Attached
    pub scope: EnvironmentScope,
    pub status: EnvironmentStatus,
    pub exclusive_group: Option<String>, // its exclusiveGroup, whatever the status
    pub superseded_by: Option<SupersedingEnvironment>, // Some exactly when Superseded
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SupersedingEnvironment {
    pub name: String,
    pub document: job::Document,
    pub scope: EnvironmentScope,      // Attached or Job
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnvironmentScope { Attached, Job, Step }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnvironmentStatus { Entered, Declined, Disabled, Superseded }

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OptionalEntryNotFound {
    pub dependency_index: usize,      // 0-based, in the Step's `dependencies`
    pub environment: String,          // the name the entry gives
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionWarning {
    pub path: Vec<PathElement>,
    pub message: String,
}
impl std::fmt::Display for SubmissionWarning; // "path: message"

// The submission-stage checks of create_job_with_environment_templates that
// need no Job Parameter values, on the Job Template itself (Template
// Schemas §3.2 last paragraph).
pub fn check_environment_templates(
    job_template: &JobTemplate,
    attached: &[AttachedEnvironmentTemplate<'_>],
    ctx: &ValidationContext,
) -> Result<Vec<SubmissionWarning>, ModelError>;

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
(action `command`/`args`/`timeout`/`cancelation`, environment and
Service `variables`, embedded-file `data`, and the `let` bindings feeding
them — see the Resolved-Value Checks on Carried-Forward Fields section of
[job-creation.md](job-creation.md); every such failure across the
template's job environments, Services — including their numeric
`@fmtstring` fields — and steps is reported together in one
`ModelError::ModelValidation`), runs the final task-count
limit from [`CallerLimits`], and returns the complete [`job::Job`]. The
`ctx` it takes is a full [`ValidationContext`] — i.e. revision +
extensions + caller limits, with no path policy — and callers commonly get one from
[`JobTemplate::default_validation_context`]. The context's revision
must match the template's and its extensions must cover every
extension the template declares (enabling more is allowed);
`create_job` returns a `Compatibility` error otherwise. An application
that does not support an extension rejects such templates at decode
via `supported_extensions` instead. `create_job` re-checks each value
against the Job Template's parameter definitions, but a RANGE_EXPR value
reaches it as a `range_expr`, with no spelling as written. For a
RANGE_EXPR Job Parameter in a document that does not list `ENV_PARAMS`,
the re-check therefore enforces no `minLength` or `maxLength` at all: a
`range_expr` value that did not come from [`preprocess_job_parameters`]
is accepted whatever its length. Callers that build
`JobParameterValues` themselves must check those lengths on the value
as written (see [job-creation.md](job-creation.md), round trip). Every expression
evaluation it performs runs under the caller's evaluation budgets
(`CallerLimits::max_eval_memory_bytes` / `max_eval_operations`).

With `ENV_PARAMS`, [`create_job_with_environment_templates`] runs the
Template Schemas §7.4.1 job-creation order: it binds `Job.Name` and, with
`EXPR`, `Job.EnvNames` (the attached Environments' names in attachment
order, then the `jobEnvironments` names, whatever their `activation`)
before any Step `let`; evaluates each `environment:` entry's `enableIf`
after the Step's `let`; fixes each Step's Environment set from each
Environment's `activation` and the Step's entries (an enabled entry
requests an `ON_REQUEST` Environment, and a disabled one, which supplies
nothing, declines a `BY_DEFAULT` one, is an error naming the Step and
the Environment for an `ALWAYS` one, and has no effect on an attached
one whose Environment Template does not list `ENV_PARAMS`, which behaves
as `ALWAYS` with no `enableIf`), recording it in
`job::Step::environments`; computes each
Step's effective Environment parameter values for the Environments in
its set into `job::Step::environment_parameters`; resolves each
entry naming no Job Environment, enabled or disabled, against `attached` (each
attachment under its own profile; `ctx`'s profile is not read); checks
that each Step supplies every parameter without a `default` of every
Environment that applies to it, whatever that Environment's own `enableIf`
or `exclusiveGroup` (constraint 7, §7.4.1 step 5); for
each Step, takes the Environments that apply to it in the Job's
Environment order, and supersedes one whose `exclusiveGroup` an earlier
one left in the Step's set already has, without evaluating its `enableIf`
(§4 item 7, §7.4.1 step 6), and resolves every other one's own `enableIf`
(§4 item 6) with that Step's concrete effective values, not evaluating one
that reads a value that is missing, fails to resolve, or does not convert,
which then does not disable the Environment (§3.2.2 item 2); takes a superseded or
disabled one out of the Step's `environments`, `step_environments`, and
`environment_parameters` and lists it in
`CreatedJob::disabled_environments` (for an Environment with `activation:
ALWAYS`, which cannot be disabled, a `false` result is instead an error at
its `enableIf` naming every such Step, §3.2.2 item 6); checks each value of an
Environment that is not disabled for the Step against its definition (a
PATH value of any form is used as given, §4.5 item 3); checks
the `WRAP_ACTIONS` single-layer rule over each Step's final set; and last
resolves each Environment's `hostRequirements` for each Step whose
Sessions enter it, with that Step's effective values, and stores the
Step's combined host requirements (Template Schemas §3.3.3) in
`job::Step::host_requirements`. In a document that lists ENV_PARAMS a
requirement that resolves to nothing (an amount whose `min` and `max`
both resolve to `null`, an attribute whose `anyOf` and `allOf`, written
as one expression, both resolve to `null`) is discarded before
combining and listed in `CreatedJob::discarded_host_requirements`, and a
Step left with no requirements has `host_requirements: None` (§3.3).
With `SERVICE` it does the same for each Service, which selects
Environments exactly as a Step does (RFC 0009 "Services run inside
Environments", with no exception for an Environment that lists it): the Job
Environments its Sessions enter (`job::Service::environments`), its
effective values (`job::Service::environment_parameters`, constraints 5–7
applied to the Service), its Environment statuses
(`CreatedJob::service_environment_statuses`), and its combined host
requirements (`job::Service::host_requirements`, its discards in
`CreatedJob::discarded_service_host_requirements`). A Job Template's
Service chooses among every Environment of the combined Job; an
Environment Template's chooses among its own document's and the earlier
attachments' only, and may only decline (§1.2 *services* constraint 5). It
also computes each Service's final scope and reports a dependency cycle
that only the resolved `enableIf`s and `exclusiveGroup`s reveal. For
every Job Template, whether or not it lists ENV_PARAMS, it takes the §7.4.1
steps over the Job Template and the attachments in one pass and reports
every check failure in one `ModelError::ModelValidation`,
in §7.4.1 order: the template-validation-style checks first, then by
step, then by Step (and after the Steps, by Service: the Job Template's,
then the attached ones), then by the Environment's place in the Job's
Environment order; a Step or Service with an error from steps 1–4 gets no
checks from steps 5–8. Where a Service's errors fall is the prototype's
choice, which RFC 0009 leaves open. The error is shaped by the inputs: for `JobTemplate`, at
the Job Template's paths, without attachments, and for `Submission`, with
every path starting at its document, with them; the warnings' paths
follow the same rule. For a Job Template without ENV_PARAMS that template
validation accepts, this is a change only when Environment Templates are
attached: its own errors and the
attachments' are now reported together, for `Submission`, where before its
own were reported alone, for `JobTemplate`; the attachments' errors are in
§7.4.1 order rather than the order they were found; and a caller limit is
reported only when the attachments add no error. [`create_job`] is the same with no attachments, so
its Job is complete: an enabled required entry naming no Job Environment
is a `JobTemplate` error. See
[job-creation.md](job-creation.md#error-order).

`CreatedJob::environment_statuses` records, for each Step, which
Environments it considered and the §7.4.1 decision about each, so a caller
such as a scheduler can show why a Step's Sessions do not enter one. A Step
considers every Environment its Sessions would enter by `activation` (every
`ALWAYS` and `BY_DEFAULT` one, each `ON_REQUEST` one an enabled entry
requests) and each of its own Step Environments; an `ON_REQUEST` one it does
not request is not listed. They are listed in the Job's Environment order
(`StepEnvironmentStatus::scope` `Attached`, then `Job`, then `Step`), each
with one [`EnvironmentStatus`]: `Declined` for a `BY_DEFAULT` one a disabled
entry declines (step 4; its `enableIf` is not evaluated; never an attached
Environment whose Environment Template does not list ENV_PARAMS, which
behaves as `ALWAYS` and is always `Entered`), `Superseded` for
one an earlier member of its `exclusiveGroup` that remains in the set
supersedes (step 6; its `enableIf` is not evaluated, so a `false` one is
still `Superseded`), `Disabled` for one its own `enableIf` turns off (step
6), and `Entered` otherwise. The `Entered` items of scope `Attached` and
`Job` are exactly `job::Step::environments` (all of the Job's when that is
`None`), those of scope `Step` are `job::Step::step_environments`, and
`disabled_environments` is the `Disabled` and `Superseded` items.
Each item also carries the Environment's `exclusive_group` (`None` for one
without, and for every Step Environment, where the field is not permitted),
and a `Superseded` item carries `superseded_by`, the [`SupersedingEnvironment`]
the Step's Sessions enter instead: the earliest member of the group in the
Job's Environment order that remains in the Step's set (§7.4.1 step 6). It
gives the winner's `name`, `document` and `scope`; `document` tells an attached
Environment and a Job Environment of the same name apart, and `scope`, which
follows from it, lets the winner's own status item be matched on the same three
fields. `superseded_by` is
`None` for every other status. A group whose first member is disabled by its
`enableIf` is won by the next, and the disabled one is `Disabled` with no
`superseded_by`.
`StepEnvironmentStatuses::not_found` lists the enabled `required: false`
entries that resolved to no Environment (each also a warning); a disabled
entry that names nothing has no effect and is not listed. For a Job
Template without ENV_PARAMS every one of its own Environments is
`Entered`; an attached Environment Template that lists ENV_PARAMS can
still be `Disabled` or `Superseded`, and one that does not is always
`Entered`. [`create_job`] returns only the Job;
a caller that wants the statuses calls
[`create_job_with_environment_templates`] with no attachments, which is
what `create_job` does. `CreatedJob::service_environment_statuses` gives
the same record for each Service, index for index with `job.services`,
with no Step Environments; an attached Service, which requests nothing,
considers no `ON_REQUEST` Environment and has no `not_found` entries.
`CreatedJob` has public fields and is not `#[non_exhaustive]`, so
adding `environment_statuses` breaks code outside the crate that builds a `CreatedJob` with a
struct literal or destructures it without `..`; reading its fields is unaffected. The same holds
for `StepEnvironmentStatus`, which gained `exclusive_group` and `superseded_by`.

[`check_environment_templates`] runs, on a decoded Job Template before
any Job Parameter values exist, the submission-stage checks that need
none: resolution by name of each entry whose `enableIf` is known to be
`true` (absent, the boolean scalar, or an expression whose value template
validation knows, §7.6 rule 5; any other is left to job creation),
constraint 5; constraint 7 over the attachments that apply to each Step
(every `ALWAYS` one, every `BY_DEFAULT` one it does not decline with a
disabled entry, and each `ON_REQUEST` one it requests), whatever each
one's own `enableIf` or `exclusiveGroup` (§3.2 constraint 7); for an
attachment known not to be disabled for the Step (its `activation` is
`ALWAYS`, or its own `enableIf` is known to be `true`, or reads a literal
value that does not convert, so it is not evaluated, §3.2.2 item 2; see
validation.md, pass 11) and that no earlier attachment of its
`exclusiveGroup` supersedes or may supersede (§4 item 7),
constraint 6 for literal values and the single-expression rule; the same
value checks for each Job Environment with an `exclusiveGroup` a Step is
known to enter, which template validation leaves out; a disabled entry naming an
attached `ALWAYS` Environment, an error; the
attachment-level warnings and errors (a warning, when the Job Template
lists `ENV_PARAMS`, for an attachment named like a Job Environment; an
error for an attachment that no enabled entry can reach and that has
parameters without a `default`, when its `activation` is `ALWAYS` or
`BY_DEFAULT` and it certainly applies to some Step or, with `SERVICE`,
Job Template Service, which for a `BY_DEFAULT` one is one with no entry
for its name that is disabled or whose `enableIf` is not known, whatever
its own `enableIf` or `exclusiveGroup`), and the
attachments' literal
`default`s, the `WRAP_ACTIONS` single-layer rule for each Step, and the
§3.3.3 combination of
each Step's literal host requirements with the literal ones of the
Environments its Sessions certainly enter (with the group rule applied).
It reports with the same
messages and paths (a single-layer or host-requirement error in the same
form, naming only the layers a Session certainly enters and the literal
requirements), shaped as job creation's error is (for `JobTemplate`
without attachments, for `Submission` with them), except that it reports
every violation it is certain of, in its own order, where job creation
orders by §7.4.1 step and takes no step after step 4 for a Step or Service
with an error from steps 1–4, and that a Step's constraint 7 error leaves
out the attachments it cannot know apply to the Step, and returns the
warnings. With `SERVICE`, each Service of the Job Template's `services`
gets the same checks as a Step (resolution, constraints 5–7, and its
combined literal host requirements, the latter only when its scope is known
not to be empty), with job creation's Service messages (RFC 0009
`<StepDependency>` item 3, `<Service>` item 5). Each attached Service
known to be used gets its disabled entries resolved among its own and the
earlier attachments' Environments (§1.2 *services* constraint 5),
constraint 7 over the `ALWAYS` and `BY_DEFAULT` Environments of its own and
the earlier documents that apply to it (with every parameter at its
`default`) and its combined literal host requirements; and every Service
known to be used gets RFC 0009 wrap hook rule 1 and the single-layer rule
over the wrapping Environments its Sessions certainly enter. With
`SERVICE` it also matches the `externalServices` requirements first
(§1.2.2 item 2, reported alone on failure), and reports, alone, a cycle of
the combined dependency graph (`<StepDependency>` constraint 3) made of
edges that are certain without Job Parameter values. Format-string values,
non-literal `default`s and host requirements, and entries whose
`enableIf` is not known before job creation are left to job creation. `openjd check
--environment` calls it. See
[job-creation.md](job-creation.md#check_environment_templates).

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

With `SERVICE`, the submission stage of
[`create_job_with_environment_templates`] (RFC 0009, Template Schemas §1.2.2
"Services from Environment Templates") first runs the two submission-time
checks that relate documents only the scheduler sees together — requirement
matching (merge rule 2: each `externalServices` entry matches exactly one
attached Service with its ports and protocols, recorded as a
[`RequirementBinding`]) and the wrapping-Environment rule (merge rule 4) —
reporting every violation as a `ModelValidation` error for the model name
`Submission` with paths rooted at the document (`JobTemplate`,
`EnvironmentTemplate[i]`, or the attachment's label); it then instantiates
each template's `services` as **external Services** under that template's own
[`EnvironmentTemplate::profile`], each with its scope computed over the
combined Job and stamped with its attachment as [`job::Service::document`]
(merge rule 3: Service names are per document, so an attached Service may be
named like a Service of the Job Template or of another attachment and the pair
`(document, name)` identifies it), and runs the carried-forward checks on each
attached Environment; the check failures of every attached document's
Services and Environment are reported with the stage's other errors in one
`Submission` error, as `create_job` aggregates the Job Template's. The
combined Job places the external Services before the Job Template's
`services` and the attached Environments before its `jobEnvironments` (merge
rule 1). The `SERVICE` prototype's separate public
`apply_environment_templates` / `AppliedEnvironmentTemplates` are merged into
this one entry point. See "Services from Environment Templates" in
[job-creation.md](job-creation.md).

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
    pub external_services: Option<Vec<template::ServiceRequirement>>,
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
    /// Empty when `externalServices` is absent.
    pub fn external_services(&self) -> &[template::ServiceRequirement];

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
    pub schema: Option<String>,                 // "$schema", accepted and ignored
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
    pub script: Option<template::EnvironmentScript>,
    pub variables: Option<HashMap<String, FormatString>>,
}

impl template::Environment {
    /// The Service names `dependencies` lists with the `service` key, list order.
    pub fn listed_services(&self) -> impl Iterator<Item = &str> + '_;
    /// `dependencies` lists at least one Service.
    pub fn depends_on_service(&self) -> bool;
    /// Any format string references a `Service.*` value.
    pub fn references_service(&self) -> bool;
}

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

`template::Environment` has an `env_parameter_definitions:
Option<Vec<EnvironmentParameterDefinition>>` field (ENV_PARAMS, §4 item 3,
`envParameterDefinitions`), an `activation: Option<EnvironmentActivation>` field
(ENV_PARAMS, §4 item 4), a
`host_requirements: Option<template::HostRequirements>` field (ENV_PARAMS,
§4 item 5, the Step's type), an `enable_if: Option<ExprField>` field
(ENV_PARAMS, §4 item 6; an expression field of type `bool`,
permitted on every position), and an `exclusive_group: Option<String>` field
(ENV_PARAMS, §4 item 7; an `<Identifier>`, not on a `stepEnvironments` entry or an
`ALWAYS` Environment) between `description` and `script`. Adding
`exclusive_group` is a breaking change for code that builds a
`template::Environment` with a struct literal. Its methods:

```rust
impl template::Environment {
    /// `activation` with its default (`EnvironmentActivation::ByDefault`)
    /// applied, as it applies in a document that lists ENV_PARAMS. An
    /// Environment from a document that does not list it behaves as
    /// `ALWAYS`, with no effect from a disabled entry; this method does not
    /// know the document, and job creation applies the rule to attachments.
    #[must_use] pub fn activation(&self) -> EnvironmentActivation;
    /// The `enableIf` as written, when it is a literal: `Some(true)` when
    /// it is absent or the boolean scalar `true`, `Some(false)` for the
    /// scalar `false`, `None` for an expression. Template validation also
    /// knows an expression such as `"true"` (§7.6 rule 5), and every check
    /// before job creation reads that value, not this one.
    #[must_use] pub fn literal_enabled(&self) -> Option<bool>;
}

/// §4 item 4 `<EnvironmentActivation>`; an unrecognized value is a decode error.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EnvironmentActivation {
    Always,     // ALWAYS: every Session enters it; its enableIf being false
                // for a Step, or a Step's disabled entry for it, is an
                // error (§4 item 4, §3.2.2 item 6, §3.2 item 3.2). An
                // Environment from a document without ENV_PARAMS behaves
                // as ALWAYS, but a disabled entry has no effect on it
    #[default]
    ByDefault,  // BY_DEFAULT, the default in a document that lists
                // ENV_PARAMS: every Session enters it, except a Step's that
                // declines it with a disabled entry, for which its
                // enableIf is false, or for which an earlier one of its
                // exclusiveGroup supersedes it
    OnRequest,  // ON_REQUEST: a Step's that requests it, with the same
                // enableIf and exclusiveGroup exceptions
}
impl EnvironmentActivation {
    /// `ALWAYS`, `BY_DEFAULT`, or `ON_REQUEST`.
    #[must_use] pub fn as_str(self) -> &'static str;
}
```

### `<StepDependency>` and Environment parameters (ENV_PARAMS)

```rust
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StepDependency {
    pub depends_on: Option<String>,
    pub step: Option<String>,           // ENV_PARAMS synonym of depends_on, §3.2 item 2
    pub service: Option<String>,        // SERVICE (RFC 0009)
    pub environment: Option<String>,
    pub required: Option<bool>,
    pub enable_if: Option<ExprField>,   // ENV_PARAMS, §3.2 item 3.2; @expr bool
    pub parameters: Option<EnvironmentParameterValueMap>,
}

impl StepDependency {
    /// A `dependsOn: name` entry.
    pub fn on_step(name: impl Into<String>) -> Self;
    /// A `service: name` entry (SERVICE).
    pub fn on_service(name: impl Into<String>) -> Self;
    /// An `environment: name` entry with no `required`, `enableIf`, or
    /// `parameters`.
    pub fn on_environment(name: impl Into<String>) -> Self;
    /// The form of the entry; `None` when it has more than one of
    /// `dependsOn`, `step`, `service`, and `environment`, or none
    /// (rejected by template validation).
    pub fn target(&self) -> Option<DependencyTarget<'_>>;
    /// The Step (of a `dependsOn` or `step` entry) / Service /
    /// Environment of a well-formed entry of that form. Unlike the `step`
    /// and `environment` fields, which hold one key as written, `step()`
    /// reads either Step key.
    pub fn step(&self) -> Option<&str>;
    pub fn service(&self) -> Option<&str>;
    pub fn environment(&self) -> Option<&str>;
    /// `target().is_some()`.
    pub fn is_well_formed(&self) -> bool;
    /// The entry as written, for messages: "dependsOn: X" | "step: X" |
    /// "service: X" | "environment: X", or every naming key of a
    /// malformed entry ("dependsOn: X, service: Y"), or "{}".
    pub fn describe(&self) -> String;
    /// `required` with its default (`true`) applied.
    pub fn is_required(&self) -> bool;
    /// Whether the entry is enabled, as written, when `enableIf` is a
    /// literal: `Some(true)` when it is absent or the boolean scalar
    /// `true`, `Some(false)` for `false`, `None` for an expression.
    /// Template validation also knows an expression such as `"true"`
    /// (§7.6 rule 5), and every check before job creation reads that value,
    /// not this one.
    pub fn literal_enabled(&self) -> Option<bool>;
}

/// §7.6 expression field (`@expr <type>`), as written. A YAML/JSON
/// boolean decodes as `Bool`, an integer as `Int`, another number as
/// `Float`, a string as `Expression`, and a present `null` as `Null`
/// (rejected by template validation); a list, a map, or an integer above
/// `i64::MAX` that fits `u64` is a decode error. JSON number parsing gives
/// a larger or more negative integer as a float, so it decodes as `Float`.
/// The target type is the field's, not the value's.
#[derive(Debug, Clone)]
// + hand-written `impl Deserialize`, `impl PartialEq` (floats by bits), `impl Eq`
pub enum ExprField {
    Bool(bool),
    Int(i64),
    Float(f64),
    Expression(String),
    Null,
}
impl ExprField {
    /// The boolean scalar, or `None` for any other value.
    pub fn as_bool(&self) -> Option<bool>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DependencyTarget<'a> {
    /// A Step, and the key the entry wrote it under; satisfied when the
    /// Step has completed.
    Step(&'a str, StepKey),
    /// A Service (SERVICE); satisfied when the Service is READY.
    Service(&'a str),
    Environment(&'a str),
}

impl<'a> DependencyTarget<'a> {
    /// The Step name, whichever key named it.
    pub fn step(self) -> Option<&'a str>;
    pub fn service(self) -> Option<&'a str>;
    pub fn environment(self) -> Option<&'a str>;
}

/// `dependsOn: X` / `step: X` / `service: X` / `environment: X`, as a
/// template writes it.
impl std::fmt::Display for DependencyTarget<'_>;

/// The key of a Step entry (§3.2 item 2). No rule reads it: it exists so
/// messages can name what was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StepKey {
    DependsOn,  // `dependsOn`
    Step,       // `step` (ENV_PARAMS)
}
impl StepKey {
    /// `dependsOn` or `step`.
    #[must_use] pub fn as_str(self) -> &'static str;
}

/// §4.5 `<EnvironmentParameterDefinition>`. `definition.default_value()`
/// is always `None`; the `default` lives in `default`. An explicit
/// `default: null` decodes as `None`, as for a Job Parameter.
#[derive(Debug, Clone)]
// + a hand-written `impl Deserialize`
pub struct EnvironmentParameterDefinition {
    pub definition: JobParameterDefinition,
    pub default: Option<EnvironmentParameterValue>,
}

impl EnvironmentParameterDefinition {
    pub fn name(&self) -> &str;
    pub fn job_param_type(&self) -> JobParameterType;
    pub fn type_name(&self) -> &str;
}

/// A `default` or an `environment:` entry value, as written.
#[derive(Debug, Clone, PartialEq, Eq)]
// + a hand-written `impl Deserialize` (classifies via `from_json`)
pub enum EnvironmentParameterValue {
    FormatString(FormatString),      // a string containing `{{`
    Literal(serde_json::Value),      // anything else
}

impl EnvironmentParameterValue {
    /// Classify a value; a string containing `{{` that does not parse
    /// as a format string is an error.
    pub fn from_json(value: serde_json::Value) -> Result<Self, String>;
    pub fn as_literal(&self) -> Option<&serde_json::Value>;
    pub fn as_format_string(&self) -> Option<&FormatString>;
}

/// The `parameters` map of an `environment:` entry, in key order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct EnvironmentParameterValueMap(pub BTreeMap<String, EnvironmentParameterValue>);

impl EnvironmentParameterValueMap {
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn iter(&self) -> impl Iterator<Item = (&String, &EnvironmentParameterValue)>;
    pub fn get(&self, name: &str) -> Option<&EnvironmentParameterValue>;
}
```

Job creation keeps the Step entries (`dependsOn` and `step`) and the
`service` entries in `job::Step::dependencies` (an `environment:` entry does
not delay scheduling), so `job::StepDependency` keeps the `SERVICE`
prototype's two keys: a `step: X` entry becomes `dependsOn: X` in the Job. The entries become `job::Step::environment_parameters` and
`job::Step::environments` instead (see Instantiated Job Types).

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

`template::StepDependency` and `template::DependencyTarget` are above (the
`service` key and `DependencyTarget::Service`); without `SERVICE` the
`service` key is gated, and validation reports an entry with more than one
key or none at the entry.

```rust
/// Helpers over a `dependencies` list (a malformed entry yields nothing).
pub fn template::lists_service(dependencies: Option<&[StepDependency]>, name: &str) -> bool;
pub fn template::listed_service_names(dependencies: Option<&[StepDependency]>) -> impl Iterator<Item = &str>;   // list order
pub fn template::listed_step_names(dependencies: Option<&[StepDependency]>) -> impl Iterator<Item = &str>;      // list order; dependsOn and step
```

The `<Service>` types (Template Schemas §9) are:

```rust
pub struct template::Service {
    pub name: String,
    pub description: Option<Description>,
    pub let_bindings: Option<Vec<String>>,
    /// §9 item 4: the Steps (`dependsOn`) that complete before the Service
    /// starts and the Services (`service`; of the same document, or required)
    /// that are READY before it starts and are stopped after it; and, in a
    /// Job Template's `services` only (ENV_PARAMS), `environment:` entries
    /// requesting Job Environments and supplying their parameters (§3.2,
    /// RFC 0009 `<Service>` item 4; `Step.Name` is not in their scope).
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
    /// Declared, or the §9 default `{ maxRestarts: 0 }` (no `completedTasks`).
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

/// §9.8 — one `externalServices` entry.
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
    pub max_restarts: Option<FormatString>,
    pub completed_tasks: Option<CompletedTasksPolicy>,
}

impl template::ServiceRestartPolicy {
    pub const DEFAULT_MAX_RESTARTS: i64 = 0;
    // `completed_tasks` has no default: required when `maxRestarts` > 0 (§9.5 item 2).
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

/// Whether an entity's Sessions enter a Job Environment, as known at template
/// validation (RFC 0009 "Services run inside Environments", Template Schemas §7.6 rule 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Entrance {
    Enters,         // every enableIf it depends on is known to be `true` (§7.6 rule 5), and the
                    // Environment has no exclusiveGroup
    DoesNotEnter,
    Undetermined,   // an enableIf it depends on is not known, or the Environment has an
                    // exclusiveGroup (an attached member may supersede it): decided at job creation
}
impl Entrance {
    pub fn may_enter(self) -> bool;            // != DoesNotEnter
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entrances {
    pub steps: Vec<Vec<Entrance>>,     // steps[i][e]: Step i, jobEnvironments entry e
    pub services: Vec<Vec<Entrance>>,  // services[k][e]: Service k of jt.services, entry e
}
/// Every entrance of a Job Template, as template validation knows it in the
/// document's own context: from each Environment's `activation`, what its own
/// `enableIf` is known to be, and what the entity's `environment:` entry for
/// it is known to be (§7.6 rule 5), then the `exclusiveGroup` rule. Steps and
/// Services get the same rule. A grouped Environment is never `Enters`, since
/// an attached Environment of its group may come first and supersede it.
impl Entrances {
    pub fn of(jt: &JobTemplate) -> Self;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputedServiceScope {
    pub name: String,
    pub scope: ServiceScope,                   // Steps known at template validation; job creation computes the final scope where an enableIf not known (§7.6 rule 5) decides more
    pub possibly_used: bool,                   // false when no Step can be in the scope, whatever every enableIf resolves to (rule 4)
    pub depends_on_services: BTreeSet<String>, // inline Services it lists with `service`; sorted; excludes itself, required and undeclared names
    pub depends_on_steps: Vec<String>,         // Steps it lists, list order; undeclared names kept
    pub dependent_steps: Vec<String>,          // Steps listing it (rule 1), template order
    pub dependent_services: Vec<String>,       // Services listing it (rule 2), template order
    pub listing_environments: Vec<String>,     // jobEnvironments entries listing it (rule 3), template order
    pub listed_by_job_environment: bool,       // a jobEnvironments entry every Step is known to enter lists it (rule 3)
}
impl ComputedServiceScope {
    pub fn is_unused(&self) -> bool;           // !possibly_used (rule 4)
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServiceScopes { /* by name, plus each externalServices entry's scope */ }
impl ServiceScopes {
    pub fn get(&self, name: &str) -> Option<&ComputedServiceScope>;
    pub fn iter(&self) -> impl Iterator<Item = &ComputedServiceScope>;
    pub fn scope_of(&self, name: &str) -> ServiceScope;   // empty `Steps {}` for an undeclared name
    /// The scope each `externalServices` entry confers on the attached
    /// Service it is matched to, by the §9.1 rules applied to the required
    /// name (rules 1–3, rule 3 over the Steps whose Sessions enter a listing
    /// Job Environment), as known at template validation; an entry nothing
    /// lists has an empty scope, which validation rejects (rule 4).
    pub fn required_service_scopes(&self) -> BTreeMap<String, ServiceScope>;
    /// Some Step can be in the scope of the `externalServices` entry `name`.
    pub fn requirement_possibly_used(&self, name: &str) -> bool;
}

/// How entering an Environment closed a cycle: the Sessions of `from`
/// (`Step X` / `Service X`) enter `environment`, which lists `service`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CycleViaEnvironment {
    pub from: String,
    pub environment: String,   // `Job Environment 'E'`, or `Environment 'E'` in an Environment Template
    pub service: String,
}

/// A cycle in the Step/Service dependency graph (§3.2 constraint 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceDependencyCycle {
    pub path: Vec<String>,                          // ["Step Use", "Service Indexer", "Step Use"]
    pub via_environment: Option<CycleViaEnvironment>, // the first edge that entering an Environment added
}
impl Display for ServiceDependencyCycle;   // "dependencies contain a cycle: Step Use -> Service Indexer -> Step Use." (+ the Environment clause; for `X -> X`, that the Service would wait for itself, with the remedies)

/// Scopes of every inline Service (rules 1–3), or the first known cycle among
/// the `dependsOn`/`step`/`service` edges plus an edge from each Step and each
/// Service whose Sessions enter a Job Environment to each Service it lists (a
/// Service whose Sessions enter an Environment that lists it, by its
/// `activation` or through its own `environment:` entry, is the edge `X -> X`).
/// An edge that depends on an `enableIf` not known (§7.6 rule 5) or an `exclusiveGroup` is
/// left to job creation. `service` entries naming an `externalServices` entry
/// are not edges.
pub fn compute_service_scopes(jt: &JobTemplate) -> Result<ServiceScopes, ServiceDependencyCycle>;
/// A cycle among an Environment Template's Services: their `service` entries
/// plus an edge from each Service known to enter the document's `environment`
/// to each Service it lists.
pub fn environment_template_service_cycle(et: &EnvironmentTemplate) -> Option<ServiceDependencyCycle>;
/// The Services of `services` that `dependencies` lists with the `service` key,
/// in declaration order — those whose `Service.<name>.<port>.port` /
/// `.connectAddress` the listing Step, Service, or Environment may reference.
pub fn listed_services<'a>(
    dependencies: Option<&'a [StepDependency]>,
    services: &'a [template::Service],
) -> impl Iterator<Item = &'a template::Service> + Clone + 'a;
/// The `externalServices` entries that `dependencies` lists with the `service` key,
/// in declaration order — the required Services whose `port` / `connectAddress`
/// the listing Step, Service, or Job Environment may reference (§9.8 item 2).
/// Listing one makes its values visible and puts the lister in the matched
/// Service's scope, as for an inline Service (`ServiceScopes::required_service_scopes`).
pub fn listed_requirements<'a>(
    dependencies: Option<&'a [StepDependency]>,
    requirements: &'a [template::ServiceRequirement],
) -> impl Iterator<Item = &'a template::ServiceRequirement> + Clone + 'a;
/// Reference extraction: not dependency edges. Used for the diagnostic naming
/// the missing `service: <name>` entry. Reachable only through
/// `template::service_scope::*`.
pub fn step_references(step: &template::StepTemplate) -> BTreeSet<String>;
pub fn environment_references(env: &template::Environment) -> BTreeSet<String>;
pub fn environment_references_service(env: &template::Environment) -> bool;
pub fn service_references(svc: &template::Service) -> BTreeSet<String>;
```

`template::Environment` gains `listed_services()`, `depends_on_service()` and
`references_service()`. It has no `runScope`: that property is unknown (a decode error
whose hint points at `activation`, `enableIf` and `environment:` entries), and which
Sessions enter a Job Environment is known at template validation from `Entrances::of`
(one rule for Task and Service Sessions, RFC 0009 "Services run inside
Environments"). A Step
Environment is entered only by its Step's Task Sessions.

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

    // Length constraints (STRING / PATH / RANGE_EXPR / LIST_*).
    pub fn min_length(&self) -> Option<usize>;
    pub fn max_length(&self) -> Option<usize>;

    // Allowed-values (STRING / INT / FLOAT).
    pub fn allowed_values_i64(&self) -> Option<Vec<i64>>;
    pub fn allowed_values_f64(&self) -> Option<Vec<f64>>;
    pub fn allowed_values_strings(&self) -> Option<Vec<String>>;

    /// Validate an ExprValue against this definition's constraints.
    /// `preprocess_job_parameters` and `create_job` do not call
    /// it: they check the merged definition
    /// (`MergedParameterDefinition::check_constraints`). Environment
    /// parameter values are checked with it, with `AsWrittenAndCanonical`,
    /// and then with the 1024-character limit of §4.5 item 5, which this
    /// method does not apply (crate-private
    /// `EnvironmentParameterDefinition::check_value_constraints`).
    /// `length_check` decides whether a RANGE_EXPR string's canonical form is
    /// length-checked besides the string: pass
    /// `RangeExprLengthCheck::for_job_parameter` of the declaring document's
    /// profile (§2.10), or `AsWrittenAndCanonical` for an Environment
    /// parameter (§4.5 item 4). It changes nothing for the other types. For
    /// INT and FLOAT a `String` value is parsed as an `<intstring>` or
    /// `<floatstring>`, ignoring white space around the number (Template
    /// Schemas §7.6).
    pub fn check_constraints(
        &self,
        value: &openjd_expr::ExprValue,
        length_check: RangeExprLengthCheck,
    ) -> Result<(), String>;

    /// The template-validation checks of the definition: a `default` that
    /// satisfies its constraints (under `length_check` as for
    /// `check_constraints`) and a `userInterface` control the type allows,
    /// for every type; for STRING, PATH, INT and FLOAT also consistent
    /// constraints (min ≤ max, minLength ≤ maxLength, allowedValues
    /// within bounds). BOOL, RANGE_EXPR and the LIST types do not check
    /// constraint consistency. Returns every message found.
    pub fn validate_definition(
        &self,
        limits: &EffectiveLimits,
        length_check: RangeExprLengthCheck,
    ) -> Result<(), Vec<String>>;
}

/// Which spellings of a RANGE_EXPR string value `minLength` and
/// `maxLength` constrain. A `range_expr` value has only its canonical
/// form, which is checked under either variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RangeExprLengthCheck {
    /// The value as written only: a RANGE_EXPR Job Parameter in a
    /// document that does not list ENV_PARAMS (§2.10).
    AsWritten,
    /// The value as written and its canonical form: a RANGE_EXPR Job
    /// Parameter in a document that lists ENV_PARAMS (§2.10), and every
    /// Environment parameter of type RANGE_EXPR (§4.5 item 4).
    AsWrittenAndCanonical,
}

impl RangeExprLengthCheck {
    /// `AsWrittenAndCanonical` when `profile` lists ENV_PARAMS, else
    /// `AsWritten`.
    pub fn for_job_parameter(profile: &ModelProfile) -> Self;
    /// True for `AsWrittenAndCanonical`.
    pub fn checks_canonical(self) -> bool;
}

`JobRangeExprParameterDefinition` has the RANGE_EXPR forms of these two,
`check_value_constraints(&self, &ExprValue, RangeExprLengthCheck)` and
`validate_definition(&self, RangeExprLengthCheck)`, with the same meaning
of the rule. A `range_expr` value has only its canonical form, which
is checked under either rule.
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
    List(Vec<IntOrFormatString>), // an <IntRangeList>: integers and format strings
    Expression(FormatString),     // a range expression string to evaluate
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

`IntOrFormatString` is an integer or a format string. It is the element
type of `IntRange::List` (Template Schemas §3.4.1.1 `<IntRangeList>`, used
by both `INT` and `CHUNK[INT]`) and the type of the two `ChunksDefinition`
count fields. A JSON integer, an integer-valued float, or a string that
parses as a base-10 integer (surrounding whitespace allowed) decodes to
`Int`. A string containing `{{` decodes to `FormatString`, which is
resolved at job creation, or fails to decode when it is not a valid
format string. Decoding also fails for any other string, a non-integral
float, a boolean, `null`, an array or an object. `as_i64()` returns the integer of
an `Int`, or `None` for a `FormatString`.

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
    CompletedTasksPolicy, DependencyTarget, ServicePortProtocol, ServiceScope, StepKey,
};   // re-exported as job::*

pub struct job::Job {
    pub name: String,
    pub description: Option<String>,
    pub extensions: Option<Vec<ModelExtension>>,
    pub parameters: IndexMap<String, JobParameter>,
    pub steps: Vec<Step>,
    pub job_environments: Option<Vec<Environment>>,
    /// RFC 0009 `services`, each with its computed scope; external Services
    /// first in the combined Job. Omitted from JSON when `None`.
    pub services: Option<Vec<Service>>,
    /// RFC 0009 `externalServices`. Omitted from JSON when `None`.
    pub external_services: Option<Vec<ServiceRequirement>>,
}

impl job::Job {
    /// True when `extensions` contains `extension`.
    pub fn has_extension(&self, extension: ModelExtension) -> bool;
    /// `has_extension(ModelExtension::Service)`: only such a Job has
    /// `service` dependency entries, `services` or `external_services`.
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
    /// The Step's own Environments; with ENV_PARAMS, less any whose
    /// `enableIf` is `false` for the Step (`None` when none remain).
    pub step_environments: Option<Vec<Environment>>,
    pub parameter_space: Option<StepParameterSpace>,
    /// The Step's own resolved `hostRequirements`; with ENV_PARAMS, its
    /// combined host requirements (§3.3.3): its own and those of every
    /// Environment its Sessions enter, normalized per capability. A
    /// name other than `attr.worker.os.family` and `attr.worker.cpu.arch`
    /// may repeat: at most one `allOf`, then `anyOf`s of two or more
    /// values. With ENV_PARAMS, requirements that resolved to nothing
    /// are discarded first (§3.3); `None` when none are left.
    pub host_requirements: Option<HostRequirements>,
    pub dependencies: Option<Vec<StepDependency>>,
    /// The step-scope symbols (non-PATH `Param.*`, `RawParam.*`, `Job.Name`,
    /// `Job.EnvNames`, `Step.Name`, step-level `let` bindings) that the Step's step-level
    /// `let` bindings and host-side format strings reference, plus `RawParam.X` for each referenced
    /// PATH `Param.X`, in JSON transport format. The session deserializes
    /// this with `PathFormat::host()` and layers Session.* and Task.*
    /// values on top at runtime.
    pub resolved_symtab: Option<SerializedSymbolTable>,
    /// ENV_PARAMS effective values, one entry per Environment the Step's
    /// Sessions enter that declares parameters (none for one its
    /// `enableIf` disabled or its group superseded). Serde: default,
    /// skipped when `None`.
    pub environment_parameters: Option<Vec<EnvironmentParameterValues>>,
    /// ENV_PARAMS: the Job Environments the Step's Sessions enter, in
    /// `Job::job_environments` order, by `activation`, the Step's entries
    /// (an enabled one requests an `ON_REQUEST` Environment, a disabled one
    /// declines a `BY_DEFAULT` one and has no effect on one from an
    /// Environment Template that does not list ENV_PARAMS), each
    /// Environment's own `enableIf`, and
    /// each `exclusiveGroup` (§4 item 7); `None` means every one, and a
    /// created Job never holds `Some` of the full list. Serde: default,
    /// skipped when `None`.
    pub environments: Option<Vec<EnvironmentRef>>,
}

impl job::Step {
    /// Whether the Step's Sessions enter the Job Environment `name` of
    /// `document`.
    #[must_use] pub fn enters_environment(&self, document: &Document, name: &str) -> bool;
    /// `self.environments == other.environments`: whether a Session can run
    /// both Steps' Tasks without exiting and entering Job Environments
    /// between them (How Jobs Are Run, Environment Parameters).
    #[must_use] pub fn same_environments(&self, other: &job::Step) -> bool;
    /// Template Schemas §3.2.2 equality of the two Steps' effective
    /// values: matched by (document, environment); each value by its type
    /// and string form (its transport form), so a FLOAT by the text it was
    /// written with; RANGE_EXPR by the integers it denotes.
    #[must_use] pub fn environment_values_equal(&self, other: &job::Step) -> bool;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum job::Document {
    #[default]
    JobTemplate,
    EnvironmentTemplate { index: usize, label: Option<String> }, // label skipped when None
}
impl job::Document {
    #[must_use] pub fn environment_template(index: usize, label: Option<&str>) -> Self;
    #[must_use] pub fn is_job_template(&self) -> bool;
}
impl std::fmt::Display for job::Document; // JobTemplate | <label> | EnvironmentTemplate[i]

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct job::EnvironmentParameterValues {
    pub document: Document,
    pub environment: String,
    /// One entry per parameter, keyed by name, typed with the parameter's
    /// own expression type (`path`, `list[path]`, ...); PATH unmapped; a
    /// RANGE_EXPR value in canonical form (§3.2.2).
    pub values: SerializedSymbolTable,
}
impl job::EnvironmentParameterValues {
    #[must_use] pub fn values_equal(&self, other: &job::EnvironmentParameterValues) -> bool;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct job::EnvironmentRef {
    pub document: Document,
    pub environment: String,
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
    pub script: Option<EnvironmentScript>,
    pub variables: Option<HashMap<String, FormatString>>,
    /// Filtered symbol table with only the symbols this environment references.
    pub resolved_symtab: Option<SerializedSymbolTable>,
}

/// Which Sessions enter a Job Environment is recorded on the entities, not
/// here: `job::Step::environments` and `job::Service::environments`.
impl job::Environment {
    /// The Services listed in `dependencies` with the `service` key.
    pub fn depends_on_services(&self) -> impl Iterator<Item = &str>;
    /// `dependencies` lists `service: <name>`.
    pub fn depends_on_service(&self, name: &str) -> bool;
}

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
    /// u64, not usize, so a size above 2^32 is kept on 32-bit targets
    /// (wasm32) rather than truncated.
    pub default_task_count: u64,
    pub target_runtime_seconds: Option<u64>,
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
    /// Environment Template, set by the submission stage. Two
    /// Services are the same Service iff `(document, name)` agree.
    pub document: Document,
    /// §9.1: the Steps whose Tasks depend on the Service, computed from the
    /// template's `dependencies` lists and Job Environments; for an external
    /// Service, computed over the combined Job at submission (empty when
    /// nothing lists it). Defaults to `AllSteps` when absent from JSON.
    pub scope: ServiceScope,
    /// §9 item 4, as written: `dependsOn` Steps and `service` entries naming a
    /// Service of the same document or a required one. Omitted from JSON when `None`.
    pub dependencies: Option<Vec<StepDependency>>,
    /// Its own resolved `hostRequirements`; when an Environment its Sessions
    /// enter has `hostRequirements`, its combined host requirements (RFC 0009
    /// `<Service>` item 5, Template Schemas §3.3.3): its own and every entered
    /// Environment's, each resolved with this Service's effective values,
    /// normalized per capability as for a Step. A conflict rejects the
    /// submission, naming the Service.
    pub host_requirements: Option<HostRequirements>,
    /// ENV_PARAMS effective values of this Service (§3.2.2 applied to the
    /// Service, RFC 0009 `<Service>` item 4): one entry per entered
    /// Environment that declares parameters, in entry order. A Service Session
    /// enters each Environment with these values (the CLI passes them as
    /// `ServiceSessionConfig::environment_parameters`). Serde: default,
    /// skipped when `None`.
    pub environment_parameters: Option<Vec<EnvironmentParameterValues>>,
    /// The Job Environments this Service's Sessions enter (RFC 0009
    /// "Services run inside Environments"), in `Job::job_environments`
    /// order, chosen exactly as a Step's are: every `ALWAYS` one and every
    /// `BY_DEFAULT` one the Service does not decline with a disabled
    /// `environment:` entry, and every `ON_REQUEST` one an enabled
    /// `environment:` entry of the Service requests, less any whose own
    /// `enableIf` is `false` for the Service and any that an earlier one of
    /// its `exclusiveGroup` supersedes. A Service of the Job Template chooses
    /// among every Job Environment, and one of an Environment Template only
    /// among those of its own document and of the Environment Templates
    /// attached before it. `None` means every one, as for
    /// `Step::environments`. Serde: default, skipped when `None`.
    pub environments: Option<Vec<EnvironmentRef>>,
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
    /// Whether this Service's Sessions enter the Job Environment `name` of
    /// `document` (`environments` is `None`, or lists it).
    #[must_use] pub fn enters_environment(&self, document: &Document, name: &str) -> bool;
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

/// §9.8 — an instantiated `externalServices` entry.
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
    pub max_restarts: u64,
    /// `Some` whenever `max_restarts > 0` (§9.5 item 2); `None` only with 0.
    /// Omitted from JSON when `None`.
    pub completed_tasks: Option<CompletedTasksPolicy>,
}
impl job::ServiceRestartPolicy {
    /// The policy for a relaunch after a failure; `None` only with `max_restarts` 0.
    pub fn completed_tasks_on_relaunch(&self) -> Option<CompletedTasksPolicy>;
    /// The policy when stopped and started again because a listed Service began
    /// a new Service Session: the value, or `Rerun` when none was given.
    pub fn completed_tasks_on_dependent_restart(&self) -> CompletedTasksPolicy;
    /// `completed_tasks == Some(Keep)` (constraint 9).
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
    EnvParams,         // RFC 0010 (draft) — "ENV_PARAMS"
    Service,           // RFC 0009 — "SERVICE" (requires EXPR and ENV_PARAMS)
}

impl ModelExtension {
    /// The fully supported extensions, every variant above, in this order.
    /// The default allowlist of the CLI and openjd-for-js.
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
    /// Cap on the `environment:` entries (ENV_PARAMS) in one Step's
    /// `dependencies`, counted as written (disabled and `required:
    /// false` entries count; `dependsOn` and `step` entries do not).
    /// Template Schemas §3 sets no maximum. Checked at template
    /// validation only, at `steps[i] -> dependencies`, so it takes
    /// effect only in the limits passed to decoding.
    pub max_step_environment_entries: Option<usize>,
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
/// `ModelProfile` and `CallerLimits` (for the attached Environment
/// Templates, the limits but not the profile). It has no path policy:
/// job creation evaluates every TEMPLATE-scope field, `ENV_PARAMS`
/// values included, with POSIX semantics, and uses a PATH Environment
/// parameter value as given (Expression Language §1.2.1; Template
/// Schemas §4.5 item 3). Most callers construct one via
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
        /// The `ExpressionError` behind a `create_job` resolution failure
        /// (job name; in a Step's own host requirements, names, amount
        /// `min`/`max`, attribute `anyOf`/`allOf` values; an amount bound
        /// whose text is not a finite number is `Expression` instead), or
        /// carried by a converted `FormatStringValidationError`; otherwise
        /// `None`. Also returned by `Error::source()`. With ENV_PARAMS, an
        /// Environment's host requirement resolution failure is a
        /// `ModelValidation` error at its field instead, with no `source`.
        source: Option<openjd_expr::ExpressionError>,
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
    /// (and relative `LIST[PATH]` default elements) are joined to this.
    pub job_template_dir: &'a str,

    /// Current working directory. Relative PATH user values (and
    /// relative `LIST[PATH]` user value elements) are joined to this.
    pub current_working_dir: &'a str,

    /// How path strings are interpreted. `PathFormat::host()` for
    /// local filesystem paths; `Posix` or `Windows` when paths
    /// originate from a known platform (e.g. cross-platform render
    /// farms).
    pub path_format: openjd_expr::path_mapping::PathFormat,

    /// If false, PATH defaults (and `LIST[PATH]` default elements)
    /// must be relative and within `job_template_dir`. If true,
    /// absolute defaults and `..` walk-up are permitted.
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

    /// Check a coerced value against the merged constraints. A RANGE_EXPR
    /// `range_expr` is checked in canonical form against the lengths of
    /// the definitions whose document lists ENV_PARAMS, and against no
    /// lengths when none does; `preprocess_job_parameters` also checks
    /// its value as written against every definition's (§2.10).
    pub fn check_constraints(&self, value: &ExprValue) -> Result<(), ModelError>;
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
Template's `externalServices`; a Service Session sees the Services of
`job::Service::depends_on_services()` (resolved within its `document`, a
required name to the bound attached Service) plus its own. A scheduler
starts a Service once every Service of `depends_on_services()` is READY and
every Step of `depends_on_steps()` has completed; pass 12 guarantees the
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

### Submission-time Service checks (RFC 0009)

Part of [`create_job_with_environment_templates`] (signatures under [Job
Instantiation](#job-instantiation)). Error contract: a `ModelValidation`
error for `Submission` collecting every requirement-matching failure of
§1.2.2 item 2 (at `JobTemplate -> externalServices[i]`: no attached Service of
that name, two or more, a missing port, or a protocol mismatch) and every
item-4 wrapper violation (at `<doc> -> environment` or `JobTemplate ->
jobEnvironments[i]`), reported alone, before anything else of the submission
stage (item 3 — Service names are per document — rejects nothing); thereafter
`instantiate_service` and the Environment re-checks report into the submission
stage's collection, and a resolution failure from them aborts with the
document prefixed to its paths (validation errors, also reported for
`Submission`) or to its message (format-string and expression errors).
`create_job` and `create_job_details` skip these checks. Full messages and the
walk are in [job-creation.md](job-creation.md).

## Parameter Space Iteration

```rust
/// Lazy iterator over a resolved step parameter space.
///
/// Supports random access (`get(index)`) for non-adaptive spaces and
/// sequential iteration via `Iterator`. Construct from a
/// `job::StepParameterSpace` that `create_job` produced.
pub struct StepParameterSpaceIterator { /* private fields */ }

impl StepParameterSpaceIterator {
    /// Errors: `ModelError::DecodeValidation` naming the parameter when a
    /// `CHUNK[INT]` parameter's range (a list or a range expression) has a
    /// value of magnitude 2^62 or more (see parameter-space.md "Chunking");
    /// `ModelError::DecodeValidation` also when the combination expression
    /// does not fit the parameters or the task count overflows `u64`;
    /// and `ModelError::DecodeValidation` naming the parameter and the
    /// chunk when static `NONCONTIGUOUS` chunking would build a chunk with
    /// more than `openjd_expr::MAX_RANGE_EXPR_CHUNKS` runs in canonical
    /// form, e.g. `Task parameter 'F': chunk 1 (values 50004 to
    /// 3499850112): Range expression has too many sub-ranges in canonical
    /// form (50001); maximum is 50000` (see parameter-space.md
    /// "Noncontiguous Static Chunking"). Adaptive chunks never fail this
    /// way: one that would exceed the cap is shortened.
    pub fn new(space: &job::StepParameterSpace) -> Result<Self, ModelError>;

    /// Build with a one-task-per-chunk override. `Some(1)` disables
    /// adaptive chunking and lets the iterator count individual tasks.
    /// Same errors as `new`.
    pub fn new_with_chunk_override(
        space: &job::StepParameterSpace,
        override_count: Option<u64>,
    ) -> Result<Self, ModelError>;

    pub fn names(&self) -> &HashSet<String>;

    /// The exact number of task parameter sets (tasks, or chunks for a
    /// chunked parameter); 0 for adaptive chunking. A u64 on every
    /// target: on 32-bit targets (wasm32) a space can have more tasks
    /// than a usize can count, e.g. "1-5000000000".
    pub fn len(&self) -> u64;
    pub fn is_empty(&self) -> bool;

    /// Random access. Returns `None` for out-of-bounds and for adaptive
    /// chunking, the one case where chunk N is not a function of N.
    /// A contiguous chunked space answers, though `Iterator::next` still
    /// walks it sequentially — see parameter-space.md.
    pub fn get(&self, index: u64) -> Option<TaskParameterSet>;

    pub fn contains(&self, params: &TaskParameterSet) -> bool;
    pub fn validate_containment(&self, params: &TaskParameterSet) -> Result<(), String>;

    /// True only for adaptive chunking (TASK_CHUNKING with
    /// `targetRuntimeSeconds`).
    pub fn chunks_adaptive(&self) -> bool;

    /// The chunked parameter and its chunk size, for any chunked space —
    /// adaptive or static, and reflecting a chunk override when one was
    /// given. `None` when the space has no chunked parameter.
    pub fn chunks_parameter_name(&self) -> Option<&str>;
    pub fn chunks_default_task_count(&self) -> Option<u64>;

    /// Adaptive-only: a static chunk size cannot change mid-walk, so this
    /// is a no-op for a non-adaptive space. Any value is accepted: 0 acts
    /// as 1, and a size beyond the values that remain (up to u64::MAX)
    /// makes the next chunk take all of them (for CONTIGUOUS, the rest of
    /// its interval). The exception is a NONCONTIGUOUS chunk whose canonical
    /// form would have more than 50,000 runs: it is shortened to 50,000
    /// values.
    pub fn set_chunks_default_task_count(&mut self, value: u64);

    /// Rewind the iterator so a fresh `Iterator::next` walk yields the
    /// same elements again. Preserves the adaptive chunk size set via
    /// `set_chunks_default_task_count`.
    pub fn reset(&mut self);
}

impl Iterator for StepParameterSpaceIterator {
    type Item = TaskParameterSet;
    fn next(&mut self) -> Option<TaskParameterSet>;
    /// The exact remaining count when it fits a usize; otherwise
    /// (usize::MAX, None), never a truncated count. (0, None) for
    /// adaptive chunking.
    fn size_hint(&self) -> (usize, Option<usize>);
}
```

The iterator is deliberately not an `ExactSizeIterator`: that trait's
`len` is an exact `usize`, which a 32-bit target cannot give for every
space. `len()` above is the exact count (see parameter-space.md, "Counts
Are 64-bit").

Random-access indexing uses `O(1)` arithmetic on a product-of-factors
representation — submitters that want to shard a large parameter
space across workers can compute per-worker index slices without
iterating the whole space. A chunked parameter adds the cost of
building one chunk. For a range expression it never lists the
range's values: a NONCONTIGUOUS chunk is a `RangeExpr::slice`
(O(log R + runs it spans)), and a CONTIGUOUS chunk is found by binary
search over the range's interval groups (O(log G), with at most three
groups per run however many values the range has). A list range (at
most 1,024 values in a template) builds a NONCONTIGUOUS chunk from its
values, and has up to one interval group per value.

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
