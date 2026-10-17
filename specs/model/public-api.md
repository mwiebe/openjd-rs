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
│   ├── step_param_space  — StepParameterSpaceIterator
│   └── step_dependency_graph — StepDependencyGraph + friends
├── outputs               — CONTROL_FLOW output directive parser + collector
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
`template::SimpleAction`, `template::SimpleActionKind`, `template::Description`,
`template::ExtensionName`, `template::BoolOrFormatString`,
`template::OutputVariableDefinition`, `template::JobOutputDefinition`,
`template::TaskParameterDefinition`
and its 5 per-variant inner struct types
(`IntTaskParameterDefinition`, `FloatTaskParameterDefinition`,
`StringTaskParameterDefinition`, `PathTaskParameterDefinition`,
`ChunkIntTaskParameterDefinition`),
`template::JobParameterDefinition` and its 12 per-variant inner
struct types (`JobStringParameterDefinition`, …,
`JobListListIntParameterDefinition`), `template::RangeConstraint`,
`template::IntRange`, `template::IntRangeItem`, `template::FloatRange`, `template::StringRange`,
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

/// CONTROL_FLOW: advance pending Step `step_index` of `job` with `inputs`
/// (each producer in the Step's input_producers() whose state is known, on
/// every call). A caller error is a BindingError holding a
/// ModelError::Compatibility.
pub fn advance_step_binding(
    job_template: &JobTemplate,
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
    job: &job::Job,
    step_index: usize,
    inputs: &job::StepInputs,
    committed_task_count: u64,
) -> Result<job::Step, job::BindingError>;

/// CONTROL_FLOW: merge the `Steps.*` values a bound Step's host-scope
/// fields read into its `resolved_symtab`. A caller error (Step not bound, a
/// symbol not concrete) is a ModelError::Compatibility.
pub fn attach_step_inputs(
    step: &job::Step,
    inputs: &job::StepInputs,
) -> Result<job::Step, ModelError>;

/// CONTROL_FLOW: the `Steps.*` symbols job output `output_index` reads
/// (`Steps.<Step>.<Member>.<Name>`, sorted, no repeats). Empty for an
/// output that reads none and for an index past the Job's outputs.
pub fn job_output_inputs(job: &job::Job, output_index: usize) -> Vec<String>;

/// CONTROL_FLOW: evaluate job output `output_index` with the `Steps.*`
/// values in `inputs`. Ok(None) for a null result (constraints not checked).
/// A ModelValidation error at `jobOutputDefinitions[i] -> value` fails the
/// Job; a ModelError::Compatibility is a caller error.
pub fn evaluate_job_output(
    job_template: &JobTemplate,
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
    job: &job::Job,
    output_index: usize,
    inputs: &job::StepInputs,
) -> Result<Option<openjd_expr::ExprValue>, ModelError>;

pub fn convert_environment_with_symtab(
    env: &template::Environment,
    symtab: Option<&SymbolTable>,
) -> job::Environment;
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
extensions + caller limits — and callers commonly get one from
[`JobTemplate::default_validation_context`]. The context's revision
must match the template's and its extensions must cover every
extension the template declares (enabling more is allowed);
`create_job` returns a `Compatibility` error otherwise. An application
that does not support an extension rejects such templates at decode
via `supported_extensions` instead. Every expression
evaluation it performs runs under the caller's evaluation budgets
(`CallerLimits::max_eval_memory_bytes` / `max_eval_operations`). With `CONTROL_FLOW`, it copies each Step's
`outputVariables` onto `job::Step::output_variables`, decides each Job
Environment's `enableIf`, and binds each Step as far as the values concrete
at job creation allow, in dependency order: a Step may come back `Bound`,
`Skipped`, or `PendingInputs` (see [job-creation.md](job-creation.md)
§ Step input binding). Its own checks of the carried-forward fields see the
`Steps.*` symbols of each Step's direct dependencies, concrete or
unresolved. It also stores each `jobOutputDefinitions` entry in
`job::Job::outputs`, and evaluates each one whose `Steps.*` inputs are
concrete at creation; a job output that fails there fails `create_job` (see
[job-outputs.md](job-outputs.md)).

[`advance_step_binding`] moves a pending Step on as upstream values arrive:
it evaluates each group of the Step whose inputs are now concrete, `enableIf`
first, and returns the Step with more known, `Bound`, or `Skipped`, or the
first group that failed as a [`job::BindingError`]. [`attach_step_inputs`]
merges the concrete `Steps.*` values a bound Step's script and Step
Environments read into its `resolved_symtab`, recording the path-typed ones
in `job::Step::path_inputs`. Both take a [`job::StepInputs`] assembled with
`StepInputs::add_producer`.

[`job_output_inputs`] names the `Steps.*` symbols a job output waits on, and
[`evaluate_job_output`] evaluates it once they are concrete in a
[`job::StepInputs`]; the caller stores the result in
`job::JobOutput::evaluated`. See [job-outputs.md](job-outputs.md).

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
    pub job_output_definitions: Option<Vec<JobOutputDefinition>>,  // CONTROL_FLOW
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
    /// The `jobOutputDefinitions`, empty when there are none.
    pub fn job_output_definitions_list(&self) -> &[JobOutputDefinition];

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
    pub extensions: Option<Vec<ExtensionName>>,
    pub parameter_definitions: Option<Vec<JobParameterDefinition>>,
    pub environment: template::Environment,
}

impl EnvironmentTemplate {
    pub fn environment(&self) -> &template::Environment;
}
```

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

### CONTROL_FLOW Template Types (RFC 0010)

`CONTROL_FLOW` adds `JobTemplate::job_output_definitions` (above) and fields to
two structural template types:

```rust
pub struct template::StepTemplate {
    // ...existing fields...
    pub enable_if: Option<BoolOrFormatString>,
    pub output_variables: Option<Vec<OutputVariableDefinition>>,
}

pub struct template::Environment {
    // ...existing fields...
    pub enable_if: Option<BoolOrFormatString>,   // rejected in an Environment Template
}
```

and three types:

```rust
#[derive(Debug, Clone)]
pub enum template::BoolOrFormatString {
    Bool(bool),
    FormatString(FormatString),
}

/// One `<OutputVariableDefinition>`: the job parameter definition of the
/// declared type plus `collect` and `unit`.
#[derive(Debug, Clone)]
pub struct template::OutputVariableDefinition { /* private fields */ }

impl template::OutputVariableDefinition {
    pub fn name(&self) -> &str;
    pub fn job_param_type(&self) -> JobParameterType;
    pub fn collect(&self) -> OutputCollectMode;
    pub fn unit(&self) -> Option<&str>;
    pub fn description(&self) -> Option<&str>;
    /// The wrapped job parameter definition (name, description,
    /// constraint properties, and `objectType` for PATH types).
    pub fn definition(&self) -> &JobParameterDefinition;
    pub fn constraints(&self) -> &job::OutputConstraints;
    pub fn to_job_output_variable(&self) -> job::OutputVariable;
    pub fn check_constraints(&self, value: &openjd_expr::ExprValue) -> Result<(), String>;
    pub fn check_element(&self, element: &openjd_expr::ExprValue) -> Result<(), String>;
    pub fn check_length(&self, len: usize, at_exit: bool) -> Result<(), String>;
}

/// One `<JobOutputDefinition>`: the job parameter definition of the
/// declared type plus `value` and `unit`.
#[derive(Debug, Clone)]
pub struct template::JobOutputDefinition { /* private fields */ }

impl template::JobOutputDefinition {
    pub fn name(&self) -> &str;
    pub fn job_param_type(&self) -> JobParameterType;
    pub fn value(&self) -> &FormatString;
    pub fn unit(&self) -> Option<&str>;
    pub fn description(&self) -> Option<&str>;
    /// The wrapped job parameter definition (name, description,
    /// constraint properties, and `objectType` for PATH types).
    pub fn definition(&self) -> &JobParameterDefinition;
    pub fn constraints(&self) -> &job::OutputConstraints;
    /// The job-side form, with `evaluated: None`.
    pub fn to_job_output(&self) -> job::JobOutput;
}
```

`BoolOrFormatString`, `OutputVariableDefinition`, and `JobOutputDefinition`
implement `Deserialize`. See [template-types.md](template-types.md) §
CONTROL_FLOW Types.

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
    List(Vec<IntRangeItem>),  // a list of integers and format strings
    Expression(FormatString), // a range expression string to evaluate
}

/// An `<IntRangeList>` item: an `<integer>` or `<intstring>`, or a string
/// containing `{{` (a `<TaskParameterStringValue>`, resolved at job
/// creation with target type `int? | list[int]` under EXPR).
pub enum IntRangeItem {
    Int(i64),
    FormatString(FormatString),
}

impl IntRangeItem {
    pub fn as_int(&self) -> Option<i64>;
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

The job types below implement `Debug`, `Clone`, `PartialEq`, and `Hash`
(the step input binding helpers `StepInputs`, `ProducerState`, and
`BindingError` are working values, not parts of a job, and their derives
are listed with them) with the invariant `a == b ⇒ hash(a) == hash(b)`. Equality is
structural on the *created job*, not the source template: derived
state such as `resolved_symtab` participates, and since its transport
format preserves original float literals, jobs created from `1.0` vs
`1.00` parameter values compare unequal. Map-typed fields (`IndexMap`,
`HashMap`) compare order-insensitively and hash as key-sorted entries.
`f64` fields hash via `to_bits()` after normalizing `-0.0` to `0.0`,
consistent with `-0.0 == 0.0`. Types whose (transitive) fields include
`f64` — `Job`, `Step`, `StepParameterSpace`, `TaskParameter`,
`HostRequirements`, `AmountRequirement` — implement `PartialEq` but
not `Eq`, and so do the CONTROL_FLOW types that hold an
`openjd_expr::value::Float64` (which implements `PartialEq` and `Hash`
but not `Eq`): `OutputVariable`, `OutputConstraints`,
`OutputAllowedValues`, `OutputBound`, and `JobOutput`, as do `StepBinding`,
`PendingBinding`, and `GroupState`, which can hold a `StepParameterSpace` and
`HostRequirements`.
The rest also implement `Eq`.

```rust
pub struct job::Job {
    pub name: String,
    pub description: Option<String>,
    pub extensions: Option<Vec<ModelExtension>>,
    pub parameters: IndexMap<String, JobParameter>,
    pub steps: Vec<Step>,
    pub job_environments: Option<Vec<Environment>>,
    /// CONTROL_FLOW job outputs, in template order. `#[serde(default)]`,
    /// skipped when `None`.
    pub outputs: Option<Vec<JobOutput>>,
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
    /// CONTROL_FLOW: the Step's output declarations, or `None`.
    pub output_variables: Option<Vec<OutputVariable>>,
    /// CONTROL_FLOW: the binding state. A pending or skipped Step keeps
    /// `parameter_space`, `host_requirements`, and `resolved_symtab` `None`.
    pub binding: StepBinding,
    /// CONTROL_FLOW: path-typed `Steps.*` values stored in
    /// `resolved_symtab` in submission form, recorded by
    /// `attach_step_inputs`.
    pub path_inputs: Vec<PathInput>,
}

impl job::Step {
    /// CONTROL_FLOW: the Task count once Tasks are known (bound, or pending
    /// with `tasks: Known`); `Some(0)` when skipped; `None` otherwise.
    pub fn tasks_known(&self) -> Option<u64>;
    /// CONTROL_FLOW: what is known of a pending Step, per group; `None`
    /// for a bound or skipped Step.
    pub fn binding_report(&self) -> Option<BindingReport>;
    /// CONTROL_FLOW: the producers whose `Steps.*` symbols the Step reads:
    /// in its script and Step Environments, and for a pending Step in any
    /// group, known or not. What a caller passes on each binding call.
    /// Sorted; empty for a skipped Step.
    pub fn input_producers(&self) -> Vec<String>;
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
    /// CONTROL_FLOW: `false` when its `enableIf` evaluated to `false`; the
    /// caller leaves it out of the Session (openjd-sessions does not read it).
    /// Serialized only when `false`; defaults to `true`.
    pub enabled: bool,
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

### CONTROL_FLOW Job Types (RFC 0010)

Re-exported from `job`. Every new `job::Step` field has `#[serde(default)]` and is
skipped when it holds its default, so a Step serialized before `CONTROL_FLOW` still
deserializes and a Step without `CONTROL_FLOW` features serializes as before.

```rust
#[derive(Debug, Clone, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct job::OutputVariable {
    pub name: String,
    /// Serialized as "type" with the spec string ("LIST[INT]"), read
    /// case-insensitively.
    pub param_type: JobParameterType,
    pub collect: OutputCollectMode,           // skipped when ONCE
    pub unit: Option<String>,
    pub constraints: OutputConstraints,       // skipped when empty
}

impl job::OutputVariable {
    /// One Task's whole value: its type, then the declared constraints
    /// (for a list: minLength, maxLength, and every element).
    pub fn check_constraints(&self, value: &ExprValue) -> Result<(), String>;
    /// One element reported under `collect: APPEND`. Errors for a
    /// scalar type.
    pub fn check_element(&self, element: &ExprValue) -> Result<(), String>;
    /// The length of a list being collected under APPEND: maxLength
    /// always, minLength only when `at_exit`. Errors for a scalar type.
    pub fn check_length(&self, len: usize, at_exit: bool) -> Result<(), String>;
}

/// The constraint properties the definition declared, and only those.
/// Every field is skipped when `None`.
#[derive(Debug, Clone, Default, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct job::OutputConstraints {
    pub allowed_values: Option<OutputAllowedValues>,
    pub min_value: Option<OutputBound>,
    pub max_value: Option<OutputBound>,
    pub min_length: Option<usize>,
    pub max_length: Option<usize>,
    pub item: Option<Box<OutputConstraints>>,
}

impl job::OutputConstraints {
    pub fn is_empty(&self) -> bool;
}

/// Externally tagged in lowercase: {"int": [1, 2]}, {"string": ["a"]}.
#[derive(Debug, Clone, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum job::OutputAllowedValues {
    Int(Vec<i64>),
    Float(Vec<openjd_expr::value::Float64>),
    String(Vec<String>),
}

/// Externally tagged in lowercase: {"int": 1}, {"float": 0.5}.
#[derive(Debug, Clone, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum job::OutputBound {
    Int(i64),
    Float(openjd_expr::value::Float64),
}

/// Internally tagged on "state": BOUND, PENDING_INPUTS, SKIPPED; a
/// PENDING_INPUTS value holds the PendingBinding's fields beside "state".
#[derive(Debug, Clone, Default, PartialEq, Hash, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "SCREAMING_SNAKE_CASE", rename_all_fields = "camelCase")]
pub enum job::StepBinding {
    #[default]
    Bound,
    PendingInputs(PendingBinding),
    Skipped,
}

impl job::StepBinding {
    pub fn is_bound(&self) -> bool;
}

#[derive(Debug, Clone, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct job::PendingBinding {
    pub enablement: GroupState<()>,
    pub tasks: GroupState<Option<StepParameterSpace>>,
    pub host_requirements: GroupState<Option<HostRequirements>>,
    pub remaining: GroupState<()>,
    /// The producers any group reads, sorted; skipped when empty.
    pub producers: Vec<String>,
}

/// Externally tagged in camelCase: {"waiting": {"on": [...]}}, {"known": ...}.
#[derive(Debug, Clone, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum job::GroupState<T> {
    /// `on`: the group's own `Steps.<Step>.<Member>.<Name>` inputs that are
    /// not concrete, sorted (not counting the enablement group's).
    Waiting { on: Vec<String> },
    Known(T),
}

impl<T> job::GroupState<T> {
    pub fn is_known(&self) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive] // TASK_DEPENDENCIES would add a Task graph group
pub enum job::MaterializationGroup {
    Enablement,
    TaskEnumeration,
    HostSelection,
    Remaining,
}
/// "enablement", "Task enumeration", "host selection", "remaining".
impl Display for job::MaterializationGroup;

/// A group that failed, which fails its Step at once.
#[derive(Debug)]
pub struct job::BindingError {
    pub step: String,
    pub group: MaterializationGroup,
    pub errors: Vec<ModelError>,
}
impl job::BindingError {
    /// Every error is a ModelError::Compatibility: a caller error, which
    /// does not fail the Step. `false` when `errors` is empty.
    pub fn is_caller_error(&self) -> bool;
}
/// "Step '<step>' failed in its <group> group:" (for a caller error,
/// "Binding Step '<step>' was refused in its <group> group:") then each
/// error on its own line.
impl Display for job::BindingError;
impl std::error::Error for job::BindingError;

#[derive(Debug, Clone, Copy)]
pub enum job::ProducerState<'a> {
    /// Tasks known, not finished: TaskParam and TaskRawParam only.
    TasksKnown,
    /// Each Task's outputs, in iteration order (chunking disabled).
    Succeeded { task_outputs: &'a [outputs::TaskOutputs] },
    /// Every symbol of the producer is null.
    Skipped,
}

#[derive(Debug, Clone, Default)]
pub struct job::StepInputs { /* private */ }

impl job::StepInputs {
    pub fn new() -> Self;
    /// Add or replace a producer's `Steps.*` values, typed by the RFC's
    /// table. Path-typed values are held only in their Raw form. TasksKnown
    /// on a skipped producer is Skipped. A caller error is a
    /// ModelError::Compatibility.
    pub fn add_producer(&mut self, producer: &Step, state: ProducerState<'_>)
        -> Result<(), ModelError>;
    /// The concrete value of `Steps.<Step>.<Member>.<Name>`; `null` for any
    /// symbol of a skipped producer; `None` when not concrete and for a
    /// path-typed Output or TaskParam (derived on the worker host).
    pub fn get(&self, symbol: &str) -> Option<&ExprValue>;
    /// Concrete, including a path-typed Output or TaskParam whose Raw twin is.
    pub fn is_concrete(&self, symbol: &str) -> bool;
    /// The producers added, sorted.
    pub fn producers(&self) -> impl Iterator<Item = &str>;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum job::GroupReport {
    Known,
    Waiting { on: Vec<String>, for_enablement: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct job::BindingReport {
    pub enablement: GroupReport,
    pub tasks: GroupReport,
    pub host_requirements: GroupReport,
    pub remaining: GroupReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct job::PathInput {
    pub symbol: String,
    /// Serialized as "type" with its spelling, e.g. "list[path]".
    pub ty: openjd_expr::ExprType,
}
```

Job outputs (`job::Job::outputs`). Like `job::Job`, they implement `Serialize` but
not `Deserialize`, because `openjd_expr::ExprValue` does not.

```rust
#[derive(Debug, Clone, PartialEq, Hash)]
pub struct job::JobOutput {
    /// Name, type, unit, and declared constraints; serialized inline with
    /// the other fields. Its `collect` is always ONCE.
    pub declaration: OutputVariable,
    /// `objectType` of a PATH or LIST[PATH] output; skipped when None.
    pub object_type: Option<ObjectType>,
    pub description: Option<String>,          // skipped when None
    pub value: FormatString,
    /// None while pending; skipped when None.
    pub evaluated: Option<JobOutputValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum job::JobOutputValue {
    /// Serialized as {"state": "VALUE", "value": <ExprValue>}.
    Value(ExprValue),
    /// The value evaluated to `null`: the output has no value. Serialized
    /// as {"state": "NULL"}.
    Null,
}

impl Serialize for job::JobOutput;      // one plain struct (see job-types.md)
impl Serialize for job::JobOutputValue;

impl job::JobOutputValue {
    pub fn value(&self) -> Option<&ExprValue>;
}

/// `Some(v)` → `Value(v)`, `None` → `Null`, as `evaluate_job_output`
/// returns them.
impl From<Option<ExprValue>> for job::JobOutputValue;
```

See [job-types.md](job-types.md) § CONTROL_FLOW fields, which has an example of the
serialized form, and [job-outputs.md](job-outputs.md).

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

## Output Values (`outputs::*`, CONTROL_FLOW)

The `openjd_output_var` directive parser and the collector that turns one
Action's directives into each Task's output values (RFC 0010 §Output
emission). No I/O. See [outputs.md](outputs.md).

```rust
/// RFC 0010 §Limits item 2: the least `max_bytes` a runtime may pass.
pub const outputs::MIN_OUTPUT_BYTES_LIMIT: usize = 1_048_576;

/// The text after `openjd_output_var:`, parsed. Borrows from the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum outputs::OutputDirective<'a> {
    /// `json` is unparsed, without surrounding spaces and tabs.
    Inline { name: &'a str, task: Option<i64>, json: &'a str },
    /// `path` is the text after `@` without trailing spaces and tabs; never empty.
    File { name: &'a str, task: Option<i64>, path: &'a str },
}

impl<'a> outputs::OutputDirective<'a> {
    pub fn name(&self) -> &'a str;
    pub fn task(&self) -> Option<i64>;
}

/// Grammar only (line terminator already removed). A task selector
/// outside the i64 range is a `Grammar` error.
pub fn outputs::parse_output_directive(payload: &str)
    -> Result<outputs::OutputDirective<'_>, outputs::OutputError>;

/// One Task's values, by output name, in declaration order.
pub type outputs::TaskOutputs = IndexMap<String, ExprValue>;

#[derive(Debug, Clone)]
pub struct outputs::OutputCollector { /* private */ }

impl outputs::OutputCollector {
    /// `chunk`: the chunk's CHUNK[INT] values in iteration order, or
    /// `None` for an unchunked Step. A repeated value is a `Selector` error;
    /// `collect: APPEND` on a non-list type is a `Declaration` error.
    /// `path_rules`: the Session's full rules in its order, unfiltered.
    pub fn new(
        declared: &[job::OutputVariable],
        max_bytes: usize,
        path_rules: &[openjd_expr::path_mapping::PathMappingRule],
        worker_format: openjd_expr::path_mapping::PathFormat,
        chunk: Option<Vec<i64>>,
    ) -> Result<Self, outputs::OutputError>;
    /// Declared name; selector present iff chunked and in the chunk; a
    /// ONCE output not yet reported for that Task.
    pub fn check_name(&self, name: &str, task: Option<i64>) -> Result<(), outputs::OutputError>;
    /// `max_bytes` less the Task's held size, not counting a LAST value
    /// this one would replace.
    pub fn remaining_bytes_for(&self, name: &str, task: Option<i64>) -> usize;
    /// Repeats `check_name`; converts `json` (inline text, or a file's
    /// whole content when `from_file`), reverse-maps PATH values, checks
    /// constraints, applies the collect mode, counts the size. Returns the
    /// value (ONCE/LAST) or the new element (APPEND). State is unchanged
    /// on `Err`.
    pub fn accept(&mut self, name: &str, task: Option<i64>, json: &[u8], from_file: bool)
        -> Result<ExprValue, outputs::OutputError>;
    /// At exit: ONCE/LAST values present and APPEND minLength met for
    /// every Task. Keyed by CHUNK[INT] value in chunk order (`None` when
    /// unchunked).
    pub fn finish(self)
        -> Result<IndexMap<Option<i64>, outputs::TaskOutputs>, outputs::OutputError>;
}

/// `Display` is the message. It may quote an inline value's text and the
/// converted value that is invalid, but never an `@` file's text as such.
/// Quoted text and an appended EXPR coercion message are cut to 200
/// characters with control characters escaped. See outputs.md § Errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct outputs::OutputError { /* private */ }

impl outputs::OutputError {
    pub fn kind(&self) -> outputs::OutputErrorKind;
    pub fn message(&self) -> &str;
}
impl std::fmt::Display for outputs::OutputError;
impl std::error::Error for outputs::OutputError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum outputs::OutputErrorKind {
    Grammar, Undeclared, Selector, Repeated, Json, FileContent,
    Conversion, Path, Constraint, Size, Missing, Declaration,
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
    ControlFlow,       // RFC 0010 — "CONTROL_FLOW" (requires EXPR; Job Templates only)
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
    /// With CONTROL_FLOW, checked as each Step's Tasks become known.
    pub max_task_count: Option<u64>,
    /// The Task count of one Step, checked when its Tasks become known.
    pub max_step_task_count: Option<u64>,
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
/// after `UPPER_SNAKE_CASE` → Rust-identifier conversion. Also the type of
/// a CONTROL_FLOW output variable. `Serialize` and `Deserialize` use the
/// variant names (`"Int"`, `"ListString"`), not the spec strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

/// Hash since CONTROL_FLOW: job::JobOutput holds one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// Hash since CONTROL_FLOW: job::JobOutput holds one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ObjectType {
    File, Directory,
}

#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DataFlow {
    None, In, Out, Inout,
}

/// RFC 0010 `<OutputCollectMode>` (CONTROL_FLOW). Case-sensitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OutputCollectMode {
    #[default] Once, Last, Append,
}

impl OutputCollectMode {
    pub fn as_spec_str(&self) -> &'static str;   // "ONCE", "LAST", "APPEND"
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

    /// Incompatible env-template parameter merges (§1.2.1); a create_job
    /// context that does not cover the template; with CONTROL_FLOW, a caller
    /// error in step input binding (advance_step_binding,
    /// StepInputs::add_producer, attach_step_inputs).
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
- `outputs::OutputErrorKind`, so a later change can split a kind without
  breaking callers that match on it
- `job::MaterializationGroup`, which `TASK_DEPENDENCIES` would extend with a
  Task graph group

`EndOfLine`, `ObjectType`, and `DataFlow` are intentionally closed:
they represent decidable logical concepts (newline mode, filesystem
entity kind, data direction) whose sets of variants are not expected
to change.

The `CONTROL_FLOW` enums are closed too, while the extension is a draft:
`OutputCollectMode` (the RFC's three modes), `template::BoolOrFormatString`
(the two forms of an `enableIf` value), `job::StepBinding` (the RFC's three
binding states), `job::OutputAllowedValues` / `job::OutputBound` (the
value kinds the constraint properties take), and `outputs::OutputDirective`
(the RFC's two directive forms), `job::GroupState` and `job::GroupReport`
(the two states of a group), `job::JobOutputValue` (a value or `null`), `job::ProducerState` (the states a producer
contributes inputs in), and `template::IntRangeItem` (the two forms of an
`<IntRangeList>` item). A change to the RFC that adds
a variant is a breaking change to these, which the crate's pre-1.0 version
allows.
