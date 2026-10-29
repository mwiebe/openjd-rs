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
`template::Environment`, `template::EnvironmentScript`,
`template::EnvironmentActions`, `template::Action`,
`template::EmbeddedFile`, `template::StepScript`,
`template::StepActions`, `template::CancelationMode`,
`template::HostRequirements`, `template::AmountRequirement`,
`template::AttributeRequirement`, `template::StepDependency`,
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
access per-variant fields.

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
// (ENV_PARAMS, Template Schemas §3.2.1–§3.2.2, §7.4). `create_job` is this
// with no attachments, returning only the Job.
pub fn create_job_with_environment_templates(
    job_template: &JobTemplate,
    attached: &[AttachedEnvironmentTemplate<'_>],
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
    /// then the Job Template's `jobEnvironments`.
    pub job: job::Job,
    /// The document of each `job.job_environments` entry, index for index.
    pub environment_documents: Vec<job::Document>,
    pub warnings: Vec<SubmissionWarning>,
    /// Per Step of `job`, index for index, the Environments an `enableIf`
    /// on the Environment (§4 item 6) disabled for it, for display.
    pub disabled_environments: Vec<StepDisabledEnvironments>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepDisabledEnvironments {
    /// Combined Job Environments, attached first, by (document, name).
    pub environments: Vec<job::EnvironmentRef>,
    /// The Step's own `stepEnvironments`, by name, in written order.
    pub step_environments: Vec<String>,
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
```

[`create_job`] is the high-level entry point: it resolves the job name
(template scope), instantiates every step, runs the resolved-value
checks on the carried-forward session/task-scope fields
(action `command`/`args`/`timeout`/`cancelation`, environment
`variables`, embedded-file `data`, and the `let` bindings feeding them
— see the Resolved-Value Checks on Carried-Forward Fields section of
[job-creation.md](job-creation.md); every such failure across the
template is reported together in one `ModelError::ModelValidation`),
runs the final task-count
limit from [`CallerLimits`], and returns the complete [`job::Job`]. The
`ctx` it takes is a full [`ValidationContext`] — i.e. revision +
extensions + caller limits + the submission path policy that
`ENV_PARAMS` values use — and callers commonly get one from
[`JobTemplate::default_validation_context`]. The context's revision
must match the template's and its extensions must cover every
extension the template declares (enabling more is allowed);
`create_job` returns a `Compatibility` error otherwise. An application
that does not support an extension rejects such templates at decode
via `supported_extensions` instead. Every expression
evaluation it performs runs under the caller's evaluation budgets
(`CallerLimits::max_eval_memory_bytes` / `max_eval_operations`).

With `ENV_PARAMS`, [`create_job_with_environment_templates`] runs the
Template Schemas §7.4 job-creation order: it binds `Job.Name` and, with
`EXPR`, `Job.EnvNames` (the attached Environments' names in attachment
order, then the `jobEnvironments` names, whatever their `activation`)
before any Step `let`; evaluates each `environment:` entry's `enableIf`
after the Step's `let`, treating a disabled entry as absent; fixes each
Step's Environment set from each Environment's `activation` and the Step's
enabled entries, recording it in `job::Step::environments`; computes each
Step's effective Environment parameter values for the Environments in
its set into `job::Step::environment_parameters`; resolves each
enabled entry naming no Job Environment against `attached` (each
attachment under its own profile; `ctx`'s profile is not read); resolves
each Environment's own `enableIf` (§4 item 6) for each Step whose
Sessions would enter it, with that Step's effective values that are known
(a missing or unconvertible value is `unresolved[T]`, §3.2.2), taking a
disabled one out of the Step's `environments`, `step_environments`, and
`environment_parameters` and listing it in
`CreatedJob::disabled_environments`; checks each value of an
Environment that is not disabled for the Step against its definition, and
PATH values against `ctx.submission_path_format` /
`ctx.allow_uri_path_values`, and that the Step supplies every parameter
without a `default` (constraint 7); checks
the `WRAP_ACTIONS` single-layer rule over each Step's final set; and last
resolves each Environment's `hostRequirements` for each Step whose
Sessions enter it, with that Step's effective values, and stores the
Step's combined host requirements (Template Schemas §3.3.3) in
`job::Step::host_requirements`. The
Job Template's own checks are reported in one
`ModelError::ModelValidation` for `JobTemplate`; the submission-stage
checks (entry resolution, the attachments' values, constraints 5–7 over
the combined Job, the wrap layers, the Environments' resolved
`hostRequirements` and the combination's conflicts), which run only when the Job
Template's succeed, in one for `Submission`, with paths starting at the
document. [`create_job`] is the same with no
attachments, so its Job is complete: an enabled required entry naming no
Job Environment is a `Submission` error. See
[job-creation.md](job-creation.md#the-submission-stage).

[`check_environment_templates`] runs, on a decoded Job Template before
any Job Parameter values exist, the submission-stage checks that need
none: resolution by name of each entry whose `enableIf` is absent or
the boolean scalar `true` (an expression, even a constant one, is left to
job creation), constraint 5; for an attachment whose
own `enableIf` is absent or `true`, or certainly unresolved for the Step
(see validation.md, pass 11) (one with `false` is disabled, and one
with an expression is undetermined, Template Schemas §3.2.2, §7.5 rule 5),
constraint 6 for literal values (with PATH absoluteness in
`ctx.submission_path_format`), the single-expression rule, and
constraint 7 over the attachments each Step enters (every `ALWAYS` one and
each `ON_REQUEST` one it requests); the attachment-level warnings and
errors (a warning, when the Job Template lists `ENV_PARAMS`, for an
attachment named like a Job Environment; an error for an attachment with
`activation: ALWAYS` that no entry can reach, that has parameters without
a `default`, and whose own `enableIf` is absent, `true`, or certainly
unresolved), and the attachments' literal
`default`s, the `WRAP_ACTIONS` single-layer rule for each Step whose
Sessions enter a wrapping attachment, and the §3.3.3 combination of
each Step's literal host requirements with the literal ones of the
Environments its Sessions certainly enter. It reports with the same
messages and paths (a single-layer or host-requirement error in the same
form, naming only the layers a Session certainly enters and the literal
requirements) and returns the warnings. Format-string values,
non-literal `default`s and host requirements, and entries whose
`enableIf` is an expression are left to job creation. `openjd check
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
    pub steps: Vec<template::StepTemplate>,
}
```

Methods on `JobTemplate`:

```rust
impl JobTemplate {
    pub fn name(&self) -> &FormatString;
    pub fn description(&self) -> Option<&str>;
    pub fn parameter_definitions_list(&self) -> &[JobParameterDefinition];

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
    pub environment: template::Environment,
}

impl EnvironmentTemplate {
    pub fn environment(&self) -> &template::Environment;
    /// The template's revision and declared extensions, as
    /// `JobTemplate::profile` gives a Job Template's.
    pub fn profile(&self) -> ModelProfile;
    /// `ValidationContext::from_profile(self.profile())`.
    pub fn default_validation_context(&self) -> ValidationContext;
}
```

`template::Environment` has an `env_parameter_definitions:
Option<Vec<EnvironmentParameterDefinition>>` field (ENV_PARAMS, §4 item 3,
`envParameterDefinitions`), an `activation: Option<EnvironmentActivation>` field
(ENV_PARAMS, §4 item 4), a
`host_requirements: Option<template::HostRequirements>` field (ENV_PARAMS,
§4 item 5, the Step's type), and an `enable_if: Option<ExprField>` field
(ENV_PARAMS, §4 item 6; an expression field of type `bool`,
permitted on every position) between `description` and `script`:

```rust
impl template::Environment {
    /// `activation` with its default (`EnvironmentActivation::Always`) applied.
    #[must_use] pub fn activation(&self) -> EnvironmentActivation;
    /// `Some(true)` when `enableIf` is absent or the boolean scalar `true`,
    /// `Some(false)` for the scalar `false`, `None` otherwise (an
    /// expression, even a constant one, is decided at job creation).
    #[must_use] pub fn literal_enabled(&self) -> Option<bool>;
}

/// §4 item 4 `<EnvironmentActivation>`; an unrecognized value is a decode error.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EnvironmentActivation {
    #[default]
    Always,     // ALWAYS
    OnRequest,  // ON_REQUEST
}
impl EnvironmentActivation {
    /// `ALWAYS` or `ON_REQUEST`.
    #[must_use] pub fn as_str(self) -> &'static str;
}
```

### `<StepDependency>` and Environment parameters (ENV_PARAMS)

```rust
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StepDependency {
    pub depends_on: Option<String>,
    pub step: Option<String>,           // ENV_PARAMS synonym of depends_on, §3.2 item 1
    pub environment: Option<String>,
    pub required: Option<bool>,
    pub enable_if: Option<ExprField>,   // ENV_PARAMS, §3.2 item 5; @expr bool
    pub parameters: Option<EnvironmentParameterValueMap>,
}

impl StepDependency {
    /// A `dependsOn: name` entry.
    pub fn on_step(name: impl Into<String>) -> Self;
    /// An `environment: name` entry with no `required`, `enableIf`, or
    /// `parameters`.
    pub fn on_environment(name: impl Into<String>) -> Self;
    /// The form of the entry; `None` when it has more than one of
    /// `dependsOn`, `step`, and `environment`, or none (rejected by
    /// template validation).
    pub fn target(&self) -> Option<DependencyTarget<'_>>;
    /// The Step (of a `dependsOn` or `step` entry) / Environment of a
    /// well-formed entry of that form. Unlike the `step` and `environment`
    /// fields, which hold one key as written, `step()` reads either Step
    /// key.
    pub fn step(&self) -> Option<&str>;
    pub fn environment(&self) -> Option<&str>;
    /// `target().is_some()`.
    pub fn is_well_formed(&self) -> bool;
    /// `required` with its default (`true`) applied.
    pub fn is_required(&self) -> bool;
    /// `Some(true)` when `enableIf` is absent or the boolean scalar `true`,
    /// `Some(false)` for `false`, `None` for any other value (an
    /// expression, even a constant one, is decided at job creation).
    pub fn literal_enabled(&self) -> Option<bool>;
}

/// §7.5 expression field (`@expr <type>`), as written. A YAML/JSON
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
    /// A Step, and the key the entry wrote it under.
    Step(&'a str, StepKey),
    Environment(&'a str),
}

impl<'a> DependencyTarget<'a> {
    /// The Step name, whichever key named it.
    pub fn step(self) -> Option<&'a str>;
    pub fn environment(self) -> Option<&'a str>;
}

/// `dependsOn: X` / `step: X` / `environment: X`, as a template writes it.
impl std::fmt::Display for DependencyTarget<'_>;

/// The key of a Step entry (§3.2 item 1). No rule reads it: it exists so
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

Job creation keeps only the Step entries (`dependsOn` and `step`) in
`job::Step::dependencies` (an `environment:` entry does not delay scheduling),
so `job::StepDependency` is unchanged: a `step: X` entry becomes
`dependsOn: X` in the Job. The entries become `job::Step::environment_parameters` and
`job::Step::environments` instead (see Instantiated Job Types).

`ExtensionName` and `Description` are constrained string newtypes
defined in the `template::constrained_strings` submodule (which is
crate-private as a path) but re-exported as `template::ExtensionName`
and `template::Description`. They reach the public surface as field
types on these structs and are nameable directly through the
`template::*` path.

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
`f64` — `Job`, `Step`, `StepParameterSpace`, `TaskParameter`,
`HostRequirements`, `AmountRequirement` — implement `PartialEq` but
not `Eq`; the rest also implement `Eq`.

```rust
pub struct job::Job {
    pub name: String,
    pub description: Option<String>,
    pub extensions: Option<Vec<ModelExtension>>,
    pub parameters: IndexMap<String, JobParameter>,
    pub steps: Vec<Step>,
    pub job_environments: Option<Vec<Environment>>,
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
    /// values.
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
    /// `enableIf` disabled). Serde: default, skipped when `None`.
    pub environment_parameters: Option<Vec<EnvironmentParameterValues>>,
    /// ENV_PARAMS: the Job Environments the Step's Sessions enter, in
    /// `Job::job_environments` order, by `activation`, enabled entries, and
    /// each Environment's own `enableIf`; `None` means every one, and a
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
    /// between them (§3.2.2).
    #[must_use] pub fn same_environments(&self, other: &job::Step) -> bool;
    /// Template Schemas §3.2.2 equality of the two Steps' effective
    /// values: matched by (document, environment); FLOAT compared by value;
    /// RANGE_EXPR by the integers it denotes.
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
    pub script: Option<EnvironmentScript>,
    pub variables: Option<HashMap<String, FormatString>>,
    /// Filtered symbol table with only the symbols this environment references.
    pub resolved_symtab: Option<SerializedSymbolTable>,
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
    pub on_exit: Option<Action>,
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

pub struct job::StepDependency {
    pub depends_on: String,
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
/// `ModelProfile`, `CallerLimits`, and the submission path policy
/// (`submission_path_format`, `allow_uri_path_values`), which only job
/// creation of `ENV_PARAMS` values reads (for the attached Environment
/// Templates, the limits and path policy but not the profile). Most callers construct one via
/// `JobTemplate::default_validation_context` or
/// `ValidationContext::new(revision)`.
#[derive(Debug, Clone)]
pub struct ValidationContext {
    pub profile: ModelProfile,
    pub caller_limits: CallerLimits,
    /// Path format of the Job's PATH values; Environment parameter PATH
    /// values must be absolute in it. Default `PathFormat::host()`.
    pub submission_path_format: openjd_expr::path_mapping::PathFormat,
    /// Accept a URI as an Environment parameter PATH value. Default
    /// `false`.
    pub allow_uri_path_values: bool,
}

impl ValidationContext {
    pub fn new(revision: SpecificationRevision) -> Self;
    pub fn with_extensions(revision: SpecificationRevision, extensions: Extensions) -> Self;
    pub fn from_profile(profile: ModelProfile) -> Self;
    #[must_use] pub fn with_caller_limits(self, caller_limits: CallerLimits) -> Self;
    #[must_use] pub fn with_submission_path_format(self, format: openjd_expr::path_mapping::PathFormat) -> Self;
    #[must_use] pub fn with_allow_uri_path_values(self, allow: bool) -> Self;
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
    /// `range_expr` is checked in canonical form; `preprocess_job_parameters`
    /// also checks its value as written (§2.10).
    pub fn check_constraints(&self, value: &ExprValue) -> Result<(), ModelError>;
}
```

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
    /// does not fit the parameters or the task count overflows `usize`;
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
/// `job::Job.steps[].dependencies`.
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
    /// Today: `("attr.worker.os.family", ["linux", "windows", "macos"])`
    /// and `("attr.worker.cpu.arch", ["x86_64", "arm64"])`.
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
