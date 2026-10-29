# Job Creation Pipeline

The `create_job` module transforms parsed templates into instantiated jobs. This is the core
workflow: templates + user-provided parameter values → a `job::Job` ready for session execution.

## Public API

### merge_job_parameter_definitions

```rust
pub fn merge_job_parameter_definitions(
    job_template: &JobTemplate,
    environment_templates: &[EnvironmentTemplate],
) -> Result<Vec<MergedParameterDefinition>, ModelError>
```

Merges parameter definitions from environment templates (processed in order) then the job
template (last), per §1.2.1.

Environment templates and job templates share the same parameter namespace. This allows an
environment template to accept a parameter that defines, for example, which software
installation to provide, while the job template defines that parameter's default value for
the software the job needs. The merge rules accommodate this and similar use cases where
multiple templates collaborate on the same parameters.

Returns a list of `MergedParameterDefinition` entries, each containing the merged definition
and its source template.

**Merge rules:**
- All definitions of the same parameter must have the same type
- `allowedValues`: intersection (must be non-empty after intersection)
- `minLength`/`minValue`: takes the maximum (most restrictive)
- `maxLength`/`maxValue`: takes the minimum (most restrictive)
- PATH-specific: `objectType` and `dataFlow` must be identical across definitions
- Default value: last template to define one wins

Conflicts produce `ModelError::Compatibility` with details about which templates conflict.

### preprocess_job_parameters

```rust
pub fn preprocess_job_parameters(
    job_template: &JobTemplate,
    input_values: &JobParameterInputValues,
    environment_templates: &[EnvironmentTemplate],
    path_options: &PathParameterOptions<'_>,
) -> Result<JobParameterValues, ModelError>
```

Validates and coerces user-provided parameter values against merged definitions.

`PathParameterOptions` consolidates path-related options:

```rust
pub struct PathParameterOptions<'a> {
    pub job_template_dir: &'a Path,
    pub current_working_dir: &'a Path,
    pub allow_template_dir_walk_up: bool,
    pub path_format: PathFormat,
    pub allow_uri_path_values: bool,
}
```

**Pipeline:**
1. Merge parameter definitions from all templates
2. Check for extra (undefined) parameters in input
3. Fill defaults for missing parameters
4. Coerce values to target types
5. Validate PATH parameters (relative path resolution, URI handling)
6. Check constraints (allowedValues, min/max, length bounds)
7. Validate merged constraints across multiple definitions
8. Error on still-missing required parameters

**PATH handling** (Template Schemas §2.2; §2.12 applies the same rules to each `LIST[PATH]` element):
- User-provided relative paths joined to `current_working_dir` and lexically normalized (`.` removed,
  `..` applied, no symlink resolution). Absolute user-provided paths are kept as written, and a
  user-provided path may walk up out of `current_working_dir`.
- Default relative paths joined to `job_template_dir` and lexically normalized. An absolute default is
  an error.
- URI paths (`s3://`, `https://`) preserved as-is when EXPR extension is enabled and
  `allow_uri_path_values` is set; rejected otherwise. URIs are never joined.
- `allow_template_dir_walk_up` controls whether paths can traverse above `job_template_dir`. When `false`, a default is rejected unless its normalized form is `job_template_dir` itself or a descendant. Containment is checked per path component, not by raw string prefix. When `true`, absolute and escaping defaults are accepted.
- `LIST[PATH]` errors name the element in the same `item[i]` form `check_constraints` uses
  (`The default value of LIST[PATH] parameter P at item[1] is an absolute path. ...`), and every bad
  element is reported, not just the first.
- Lexical normalization follows the path format: under `PathFormat::Windows` both `/` and `\` separate
  components; under POSIX only `/` does, and a backslash is an ordinary filename character.
- `allowedValues`, `minLength`, and `maxLength` (and `item.*` for `LIST[PATH]`) are checked at two
  stages (Template Schemas §2.2, §2.12). Template validation checks a default as written. Job
  creation checks the joined value, for submitted values and defaults alike, because the join is done
  by the submitting client and the job may be created elsewhere from the joined values. A relative
  default therefore passes both only if `allowedValues` contains both the default as written and its
  joined path. `preprocess_job_parameters` checks every merged parameter, including those contributed
  only by environment templates; `create_job` re-checks the job template's own parameters.

**Value coercion:**
- `coerce_from_str` — Parses string input (CLI): numeric parsing, boolean aliases
  (`yes`/`no`/`on`/`off`/`1`/`0`), JSON list parsing for list types
- `coerce_to_job_parameter_type` — Validates typed input (library): type compatibility, numeric
  widening (int → float), list element type validation

**Round trip:** a value `preprocess_job_parameters` returns should be a value it accepts as input.
Both its input and output types are public, so a caller can hand its output straight back to it.
`create_job` relies on something adjacent for every caller: it re-runs `check_constraints` over each
value it is given (`create_job/mod.rs`, the "Validate parameter values against template constraints"
loop at the start of `create_job`).

For `PATH` this holds because both passes check constraints against the same resolved value: the
first pass joins a relative default and checks the joined path, and the second pass receives that
absolute path as a submitted value, leaves it as written, and checks it again. Earlier, the first pass
skipped the check for defaults, so a relative default under a `maxLength` passed the first pass and
failed the second.

For lists it holds by construction, because **a job parameter stores a path as a string.** A path is
context-sensitive: the same value does not denote the same file on a Windows submitter and a Linux
worker, and job parameters are settled before there is a host to ask. OpenJD keeps paths POSIX until
template evaluation on the host -- every `with_path_format` call in this crate passes
`PathFormat::Posix`, and `openjd-sessions` passes `PathFormat::host()` -- and stores `PATH` and
`LIST[PATH]` parameter values as strings. `build_symbol_table` does not bind `Param.*` for those two
types at all, for the same reason.

So a `LIST[PATH]` value is an `ExprValue::ListString` at every length, and `ExprValue::ListPath`, which
carries a `PathFormat`, is not a shape a stored parameter value has. It is refused as input at any
length. An empty list is where that could drift, having no elements to infer a variant from:
`coerce_json_to_job_parameter_type` types it from a hint, and the hint must name `STRING`, the variant
the coerced elements produce, rather than the declared `PATH`. Passing `PATH` gave the empty case a
`ListPath` that `openjd-sessions::build_symbol_table` then dropped, leaving `Param.<name>` unbound at
session scope (issue #387). That was issue #389.

### build_symbol_table

```rust
pub fn build_symbol_table(
    params: &JobParameterValues,
) -> Result<SymbolTable, ModelError>
```

Builds a `SymbolTable` with `Param.*` and `RawParam.*` entries from processed parameter
values. Returns `Result` because symbol table insertion can fail.

PATH types are stored as `Unresolved(PATH)` since the source path format may differ
from the host path format — the value must be preserved exactly as a string until path
mapping is applied at session time. `RawParam.*` for PATH types is forced to STRING.

### create_job

```rust
pub fn create_job(
    job_template: &JobTemplate,
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
) -> Result<job::Job, ModelError>

pub fn create_job_with_environment_templates(
    job_template: &JobTemplate,
    attached: &[AttachedEnvironmentTemplate<'_>],
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
) -> Result<CreatedJob, ModelError>
```

Full template instantiation pipeline. `create_job` is
`create_job_with_environment_templates` with no attached Environment Templates, returning only
the Job; both run every stage below, including [the submission
stage](#the-submission-stage), so the Job `create_job` returns is complete. Environment templates should
already be merged into `job_parameter_values` via `preprocess_job_parameters` before
calling this function. `ctx` carries the model profile, the
`CallerLimits`, and the submission path policy (`submission_path_format`,
`allow_uri_path_values`, read only for ENV_PARAMS values) that apply to this job
instance. Its revision must
match the template's and its extensions must cover every extension the
template declares — enforced with a `Compatibility` error, since a
context enabling fewer extensions would make every downstream
evaluation ambiguous (template defect vs context artifact). Enabling
more extensions is allowed, as is layering caller policy on top (e.g.
stricter queue-level limits via `with_caller_limits`).

Every expression evaluation this stage performs — the job name, host
requirement values, parameter-space ranges, `let` bindings, and the
carried-forward checks below — runs under the caller's evaluation budgets
(`CallerLimits::max_eval_memory_bytes` / `max_eval_operations`,
defaults: the Expression Language spec's 100 MB / 10 M), the same
budgets template validation and the session runtime apply.

1. Build symbol table from parameter values
2. Resolve template-scope fields:
   - Job name (evaluate FormatString). The resolved name is checked against §1.1.1:
     length vs `max_job_name_len`, non-empty, and no Cc control characters. Decode
     already rejects an empty literal name, control characters in the name's literal
     text (verbatim in every resolution), and a fully static name resolving to an
     empty or control-character-bearing value — what remains only knowable here is
     an emptiness or control character *introduced by an interpolated value*.
   - Step names
   - Host requirement names. Each amount and attribute `name` is `@fmtstring`
     (§3.3.1 / §3.3.2) and resolves with target type `string`
     (`resolve_capability_name`). The resolved name is checked against
     §3.3.1.1 / §3.3.2.1 — at most 100 characters, the capability name pattern,
     and the reserved scopes — through `helpers::check_capability_name`, the check
     decode applies to a literal name and pass 8 to a fully static one, so the
     wording matches. The resolved names are then checked for uniqueness within
     `amounts` and within `attributes`, case-insensitively
     (`check_resolved_names_unique`, §3.3), and the resolved name is what the
     standard-capability value checks below and the job use. Decode can only
     check a name it knows — a literal, or one that is fully static — so a name
     depending on a job parameter is first checked here.
   - Host requirement values (amounts min/max, attribute values). Resolved amount bounds
     must be finite `f64` values; non-numeric, NaN, and infinite results are rejected.
     Resolved `attributes[].anyOf` / `.allOf` elements are re-checked against
     `<AttributeCapabilityValue>` (§3.3.2.2) via
     `capabilities::validate_attribute_capability_value`, because decode skips that check
     when the element is a format string. Errors are reported at
     `steps[i] -> hostRequirements -> attributes[j] -> anyOf[k]` so they read like the
     decode-time error for a literal value. Only the first failing `anyOf`/`allOf` group on
     the first failing attribute is reported; decode accumulates every violation instead.
     The amount bound constraints (`min` non-negative, `max` positive, `min <= max`) are
     re-applied on the resolved value by `check_resolved_amount_bounds`, and the
     `chunks.defaultTaskCount` / `targetRuntimeSeconds` minimums are re-applied by
     `resolve_parameter_space` — see [validation.md](validation.md). Chunks fields
     resolve single whole-field expressions with the target type Expression Language
     §1.3.2 derives from the schema: `int` for the required `defaultTaskCount`
     (so `{{ 4.0 }}` coerces to 4) and `int?` for the optional
     `targetRuntimeSeconds` (a whole-field `null` means the field is omitted);
     multi-segment strings concatenate and parse with surrounding whitespace
     tolerated.
   - Parameter space ranges (evaluate range expressions, resolve FormatString ranges).
     STRING/PATH range elements are list items (Expression Language §1.3.2): a
     whole-field expression targets `string? | list[string]`, so `null` skips the
     element and a list flattens inline — one range element per list element,
     mixable with literal elements. A range emptied by null-skips is rejected
     ("has no elements after resolution"), since the ≥1-element rule was checked
     on field presence at decode. Attribute `anyOf`/`allOf` values get the same
     treatment in `resolve_host_requirements` (`job/create_job/host_requirements.rs`). String-backed FLOAT range elements are trimmed
     and must resolve to finite `f64` values.
   - Step-level let bindings (template scope; a failure is reported at
     `steps[i] -> let[j]` — see the carried-forward checks below)
3. With EXPR extension: inject `Job.Name` and `Job.EnvNames` (before step instantiation)
   and `Step.Name` (per step) into the symbol table. `Job.EnvNames` (ENV_PARAMS, Template
   Schemas §7.3.1) is a `list[string]` of the attached Environments' names, in attachment
   order, then the `jobEnvironments` names, whatever their `activation`; a shared name repeats,
   and Step Environments are not listed. The attached Environments are known before anything
   else (§7.4), so it is concrete before any TEMPLATE-scope expression, including a Step's
   `let`. The order is the order a Session that entered all of them would enter them, since
   the combined Job lists the attached Environments first (prototype plan P2). It is bound
   with `EXPR` alone: template validation rejects a reference without ENV_PARAMS, and every
   `resolved_symtab` keeps only the symbols a field references, so a Job that does not use it
   is unchanged. It is not in the scope of the Job name or of an Environment parameter
   `default`, which resolve before it is bound or in their own table.
4. Carry forward session/task-scope fields as FormatString (plus action
   `timeout`/`notifyPeriodInSeconds`, which validate in template scope but
   resolve on the worker). A SimpleAction step (§8) is desugared here via
   `StepTemplate::resolve_syntax_sugar` — infallible, since the sugar's
   `script` is already a parsed `FormatString` — so the job's `Step` always
   carries a full `StepScript`.
5. Run the resolved-value checks on the carried-forward fields
   (next section) — job environments first, then each step as it is
   instantiated — accumulating every failure into one collection that
   is reported once, after all steps
6. Convert environments from template to job types
7. Build step dependency list from the template's Step entries only: `dependsOn`, and its
   ENV_PARAMS synonym `step` (§3.2 item 1), each a `job::StepDependency` with `dependsOn`.
   An `environment:` entry (ENV_PARAMS) does not delay scheduling (§3.2
   item 2), so it is dropped, and a list left empty by the filter becomes
   `None`.
8. ENV_PARAMS, after the step's `let` bindings (§7.4): evaluate each `environment:` entry's
   `enableIf`, fix which Job Environments the step's Sessions would enter, compute its
   effective Environment parameter values for them, decide each one's own `enableIf` with the
   values that are known, check the values and constraint 7 for the ones it does not disable,
   and keep its enabled entries for external Environments for the submission stage; see [Environment parameter
   values](#environment-parameter-values-env_params). Each Step Environment's own `enableIf`
   is decided here too, and a disabled one is left out of `job::Step::step_environments`
   (see [Environment enableIf](#environment-enableif-env_params))
9. Attach resolved symbol table to each step
10. Run [the submission stage](#the-submission-stage) with the attached Environment Templates
    (possibly none)

#### Environment parameter values (ENV_PARAMS)

**`enableIf`** (§3.2 item 5). Each `environment:` entry's `enableIf` is decided after the
Step's `let` bindings and before the entry's name is resolved (§7.4). It is an expression
field of type `bool` (§7.5), resolved by the expression-field checker's `resolve` (see
[template-types.md](template-types.md#expression-fields-75)) in the Step's template scope
(below), under the template-scope profile and library pass 8 checked it with, the caller's
evaluation budgets, and POSIX paths whatever `ctx.submission_path_format` is: an `enableIf` is
evaluated in template scope, not in a host context (Template Schemas §3.2 item 5 and §4 item
6, spec `5535230`), so unlike the entry's values it reads the `let` bindings with their
template-scope values: a boolean scalar is its
value; an expression must resolve to `true` or `false`. A `null` result ("resolved to null,
which does not fit type bool (Template Schemas §7.5)."), another type ("resolved to a value
of type T, which does not fit type bool …"), or an evaluation failure ("could not be
evaluated: <error>") is reported at `steps[i] -> dependencies[j] -> enableIf`, and that
entry's Environment gets no values for the Step, so the one mistake is reported once. A
scalar or `null` that template validation rejects is rejected here with its message too,
for a template built in code. A **disabled** entry is as if absent: it requests
nothing, supplies nothing, and its name is not resolved (§3.2 item 2). Constraint 3 applies to
entries as written, enabled or not.

**Each Step's Environment set** (§4 item 4). A Step's Sessions would enter every Job
Environment with `activation: ALWAYS` (the default) and every one with `activation:
ON_REQUEST` that an enabled entry of the Step requests, plus its own `stepEnvironments`;
the Environments' own `enableIf`s then take out the ones disabled for the Step (see
[Environment enableIf](#environment-enableif-env_params)). Job creation records, for each
`jobEnvironments` entry, whether the Step enters it, after that Environment's `enableIf`;
the submission stage does the same for the attached Environments and stores the result on
`job::Step::environments` (see [job-types.md](job-types.md#step)).

Template Schemas §3.2.2: for each Step and each Environment its Sessions would enter that declares
`envParameterDefinitions`, every parameter's **effective value** is the value the Step's enabled
`environment:` entry for that Environment supplies, else the parameter's resolved `default`.
So every Step gets values for every `ALWAYS` Job Environment that declares parameters, whether or
not it names it (prototype plan A8), and for each `ON_REQUEST` one it requests; an
Environment the Step does not enter has no values for it, and its parameters without a
`default` need no value from the Step (constraint 7 over the Step's set). An entry that
supplies keys to a Job Environment that declares none is rejected (constraint 5).
`create_job` stores them on `job::Step::environment_parameters`, one
`EnvironmentParameterValues { document: JobTemplate, environment, values }` per such Job
Environment in `jobEnvironments` order (see [job-types.md](job-types.md#step)).

The code is `job/create_job/env_params.rs`, shared with [the submission
stage](#the-submission-stage):

- **Literal values** (no `{{`) are checked with the template-validation function
  (`check_literal_value`: the Job Parameter `default` decoder of the type, list elements that
  are not format strings, the definition's constraints), so the messages match pass 11's, then
  converted.
- **Which extensions apply.** A document that holds a value or a `default` lists `ENV_PARAMS`,
  so it lists `EXPR` (Template Schemas §1.1 item 3, checked at template validation), and every
  value is read with `EXPR`, whatever `ctx` enables. The first half of plan 1's A12 (a value's
  forms follow the extensions of the document that holds it) has no case left to decide (plan
  6); its second half, the caller's URI policy, is below.
- **Format-string values** resolve in `ctx.submission_path_format` (§4.5: the values are paths
  in that format, so `path(RawParam.Dir) / 'sub'` uses its rules) under the caller's evaluation
  budgets. An entry value's scope, and an `enableIf`'s, is the Step's full template scope
  (`Param.*` other than PATH and LIST[PATH], `RawParam.*`, `Job.Name`, `Job.EnvNames`,
  `Step.Name`, and the Step's `let` bindings; §3.2 items 4–5). Job creation
  evaluates the Step's `let` bindings in POSIX; when `submission_path_format` is another
  format, the bindings that the Step's enabled entries' values read (every entry's `enableIf`
  is decided first; a disabled entry has no effect of any kind, §3.2 item 2),
  directly or through other bindings, are evaluated again in that format for the entry values,
  so a `path` binding (`path(RawParam.Dir) / 'x'`, `.parent`) follows the same rules as the
  value itself. Bindings no entry value reads are not evaluated again, so one that would fail
  only in that format (and is read by the Step's script, which keeps the POSIX value) does not
  reject the Job. A binding that is read and fails is reported at `steps[i] -> let[j]`, and the
  Step's entry values are not computed; every entry's `enableIf` and duplicate check (constraint
  3), and every Step Environment's `enableIf`, has already been made, so their errors are
  reported with it, and an entry whose `enableIf`
  failed adds nothing to the bindings evaluated again. An `enableIf` (an entry's or a Step Environment's) does not
  take part: it is evaluated in template scope (Template Schemas §3.2 item 5, §4 item 6, spec
  `5535230`), with POSIX paths in the scope as job creation built it, so a binding only an
  `enableIf` or a disabled entry's value reads is not evaluated again. A
  `default`'s scope is the merged Job Parameters alone, without `Job.Name` or `Job.EnvNames`
  (§4.5 item 4, §7.3.1; plan A21). A single `{{ }}` expression evaluates with the
  parameter's expression type as its target type, and the result is coerced to the
  Job Parameter type (a `path` result becomes its string). Any other format string resolves to
  a string and is converted with `coerce_from_str`, as a submitted Job Parameter value would
  be, with no directory join. A list-typed value that is not a single expression is an error
  (template validation already rejects it for a Job Environment).
- **PATH values**, and each LIST[PATH] element, must be non-empty (plan A16) and absolute in
  `ctx.submission_path_format`, or, with `ctx.allow_uri_path_values`, a URI (plan A12; §4.5 PATH
  paragraph). In POSIX only a leading `/` is absolute (a backslash is an ordinary filename character,
  so `\\srv\share` is relative there). The value is stored as resolved (plan A22): a literal or
  a multi-segment format string as written; a single `{{ }}` expression as the string of its
  result, which for a `path` result follows the submission format's rules (WINDOWS turns `/`
  into `\`).
- The definition's **constraints** are checked against that stored value (constraint 6,
  §4.5 item 5), using `JobParameterDefinition::check_constraints` (for a literal, by the
  template-validation check), so every effective value of an Environment that is not
  disabled for its Step satisfies its definition. A RANGE_EXPR's `minLength` and
  `maxLength` check both the value as written and its canonical form (§4.5 item 5, §2.10):
  a literal's string, a format string's resolved string, or a single `{{ }}` expression's
  string result, which is read by evaluating the expression again with no target type. A
  single expression whose result is a `range_expr` has only its canonical form, which must be
  a valid `<IntRangeExpr>` (the empty `range_expr` of `{{ Param.R[1:] }}` with `R` `5` is an
  error at the value).
- A **RANGE_EXPR** value is stored as its `range_expr`, whose transport form is its canonical
  form, so two values that denote the same integers are equal (§3.2.2): `1-3` and `1,2,3` have
  one transport form, and `Step::environment_values_equal`, Session grouping, and the shared
  resolutions of `enableIf` and `hostRequirements` see them as one. A Session parses it as it
  parses a RANGE_EXPR Job Parameter's value.

**Disabled Environments** (§3.2.2, §7.4). For each Step and each Environment its Sessions
would enter, `DeclaringEnvironment::values_for_step` computes the effective values without
reporting the checks that depend on the Environment's own `enableIf` (`PendingValues`):

- reported at once, whatever the `enableIf`: constraint 5 (a key the Environment does not
  declare);
- kept: each value's conversion and checks (type, constraints, PATH absoluteness, the
  single-expression rule for a list type), a failed resolved `default`, and the parameters
  without a `default` the Step does not supply (constraint 7);
- the value each definition has for the `enableIf`: the effective value when it converts to
  the parameter's type, whether or not it meets the other constraints, and otherwise
  (missing, or not converting) unknown.

`DeclaringEnvironment::decide` then decides the Environment's `enableIf` for the Step
(`EnableIf`): absent or `true` is enabled; the scalar `false` is disabled; an expression is
evaluated (`expr_field::resolve_progressive`) in its scope with each `Env.RawParam.<name>`
(a path as its unmapped string) and each non-PATH `Env.Param.<name>` bound to the known
value or to `unresolved[T]` of its type, and Expression Language progressive evaluation gives `true`,
`false`, or an unresolved result. Steps with the same known values share one evaluation
(keyed by the values, an unknown one as such; a RANGE_EXPR value is keyed by its canonical
form, so `1-3` and `1,2,3`, which are equal values, share one evaluation). For an Environment that is not disabled
(`true` or unresolved), `DeclaringEnvironment::commit` reports what was kept, so a missing or
unconvertible value is reported; for a disabled one it is dropped, and the Environment is
left out of the Step's set with no values. An evaluation that fails (or resolves to `null`,
or to a value that does not fit `bool`) is reported once, after every Step, at
`<environment> -> enableIf` with "Resolved for Step 'A'." (or "Steps 'A' and 'B'")
appended (`report_enable_if_errors`). Job creation fails either way, and which other errors
to report is an implementation choice (plan 4 A40, revised): openjd-rs checks the Environment for those
Steps as if it is not disabled, so a Step's other mistakes for it (a value that breaks a
constraint, a `default` that does not convert, a missing parameter, a second wrapping layer)
are reported in the same run instead of one edit at a time. A constraint 7 error for such an
Environment says its `enableIf` could not be evaluated.

**When a `default` is checked** (§4.5 points 1–2, plan A19). A literal `default` is checked
when `create_job` sets up the Job Environments, before any Step, so one that fails a
path-format check (a relative PATH) is an error even when every Step supplies the parameter.
A non-literal `default` is resolved the first time a Step takes it and cached, since its value
does not depend on the Step; a failure is reported once, at
`jobEnvironments[k] -> envParameterDefinitions[m] -> default`, prefixed
`for Step '<name>', which does not supply this parameter:`, naming the first Step that takes
it and for which the Environment is not disabled; a Step for which the Environment is
disabled does not report it (§3.2.2). A non-literal `default` that no Step takes is never
evaluated. A literal `default` is checked whatever any `enableIf` decides, since the check
is of the definition, not of a Step's value (§4.5 point 1; plan 4 A42).

`create_job` also rejects a second `environment:` entry of one Step for the same Environment
(§3.2 constraint 3), for a template edited after decoding, so neither entry's values are
silently dropped.

A Step whose step-level `let` binding failed is not instantiated (its host requirements and
parameter space need the `let` values), so its Environment parameter values are not computed
or checked either; job creation already fails on the `let` error, and the values are checked
once it is fixed.

A supplied value that fails is reported at `steps[i] -> dependencies[j] -> parameters -> <name>`.
A parameter with neither a supplied value nor a `default`, of an Environment in the Step's set
that is not disabled for it, is reported once per Step at `steps[i]` (constraint 7); template
validation already rejects that for a decoded template whose entries' and Environments'
`enableIf` are absent or a boolean scalar. When the Step's
entry for the Environment was disabled by its `enableIf`, the message says so after the
Environment's name, as at template validation; the submission stage does the same for an
attachment whose entry was disabled (`StepSelection::disabled` carries the names). When the
Environment's own `enableIf` was unresolved for the Step, its group adds "(its own enableIf
depends on a value that is missing or does not convert, so it does not disable the
Environment; Template Schemas §3.2.2)", since a reader who expected it to be `false` would
otherwise not see why the parameter is required.
These errors join the carried-forward check failures in the one `ModelValidation` error
`create_job` reports: literal `default`s first (after the Job Environments' carried-forward
checks), then each Step's values after that Step's other checks, in the Environment's
definition order, then the Job Environments' failed `enableIf` evaluations.

**External entries.** An enabled `environment:` entry that names no `jobEnvironments` entry
refers to an external Environment (§3.2 item 2), whose definitions job creation proper does
not see. It is kept for the submission stage as a crate-private `FrozenEntry`: the name, the
entry's index in `dependencies`, `required` with its default applied, the values as written,
and `symtab`, the symbols of the Step's full template scope that the values' format strings
reference (with the `let` bindings as the entry values see them, above), copied with
`FormatString::copy_used_symtab_values`. Both stages run in one call with one `ctx`, so the
table's `path` values are always in the submission path format the values resolve in.
`Step::resolved_symtab` cannot serve here, because it holds only what the Step's host-side
format strings reference. No returned Job holds frozen entries.

#### Resolved-value checks on carried-forward fields

Of the spec's three processing stages (Template Schemas §7.4: template
validation, job creation, task execution on the worker host), job
creation is the first at which job parameters have real values. The
session/task-scope format strings it carries forward unresolved —
action `command`/`args`, environment `variables` values, embedded-file
`data` — and the template-scope action fields that also resolve on the
worker — `timeout`, cancelation `notifyPeriodInSeconds` and deferred
`mode` — are statically evaluated against a **check symbol table**,
and exactly the resolved-value checks pass 8 applies to those fields
re-run on the result:

| Field | Constraint |
|---|---|
| environment `variables` values | resolved-length bound vs 2048 (§4.4.2, spec-mandated, always on) |
| action `command`, each `args[*]` entry | bound vs `CallerLimits::max_resolved_arg_len`, if set |
| embedded file `data` | bound vs `CallerLimits::max_resolved_data_len`, if set |
| action `timeout` (FB1) | coerced integer > 0; `null` = unset (spec-mandated) |
| cancelation `notifyPeriodInSeconds` (FB1) | coerced integer > 0, ≤ 600; `null` = unset (spec-mandated) |
| deferred cancelation `mode` (FB1) | `TERMINATE` / `NOTIFY_THEN_TERMINATE`; `null` = unset (spec-mandated) |

The last three rows run through the same helper pass 8 uses
(`validate_action_timing_fs`, with `TIMEOUT_CONSTRAINT`,
`NOTIFY_PERIOD_CONSTRAINT`, and `ResolvedConstraint::CancelationMode`),
so `timeout: "{{ Param.T }}"` submitted with `T = 0` fails here with
pass 8's `timeout must be > 0.` instead of on the worker. They evaluate
against the **template-scope** table pass 8 uses for them, with real
values: concrete non-PATH `Param.*`, `RawParam.*`, `Job.Name`, and — for
a step script or step environment — `Step.Name` and the step-level
`let` bindings. On the RFC 0008 wrap hooks the `WrappedAction.*` scope
is seeded unresolved on top, exactly as at pass 8, so round-trip
forwarding (`timeout: "{{ WrappedAction.Timeout }}"`) passes. Every
action is covered: step `onRun`, and environment `onEnter`/`onExit`
plus the three wrap hooks.

At template validation every `Param.*` is unresolved and contributes 0
to the lower bound; here the parameters are bound to real values, so a
violation that depends only on parameter values —
`args: ["{{ 'A' * Param.N }}"]` submitted with a huge `N` — becomes
decidable and fails at submission instead of on every worker (task
execution remains the enforcement boundary). The
walk covers step scripts (`onRun` + embedded files), step environments,
and job environments — including the RFC 0008 wrap hooks, with their
`WrappedAction.*` scopes seeded unresolved, exactly as in pass 8.

The check symbol tables mirror what the session runtime binds at run
time, with everything only a session can know left `Unresolved`:

- **Task scope** (step scripts): concrete `Param.*` / `RawParam.*` /
  `Job.Name` / `Step.Name` / step-level `let` bindings; `Unresolved`
  `Session.*`, PATH `Param.*`, `Task.Param.*` / `Task.RawParam.*`,
  `Task.File.*`. Script-level `let` bindings are evaluated into the
  table (this subsumes the type check job creation has always run on
  them: a binding that fails with the real parameter values fails
  here, deterministically, rather than in every session).
- **Session scope** (job and step environments): as above minus
  `Task.*`, plus this environment's `Env.File.*` and (ENV_PARAMS) its
  `Env.Param.*` / `Env.RawParam.*` (all `Unresolved`, since a Session
  binds them when it enters the environment), and its script-level `let`
  bindings evaluated in.
  An environment `let` binding that fails to evaluate fails job
  creation, under the same error policy as every other evaluation this
  stage performs (see below): the bindings only evaluate when the
  context profile enables EXPR, their expressions already type-checked
  at pass 8 with everything unresolved, so a failure here comes from
  the real parameter values and would deterministically recur in every
  session that enters the environment.

**`let` bindings.** Every `let` block job creation evaluates —
step-level (template scope, `HostContext::None`), step-script and
environment script-level (`HostContext::Unresolved`) — goes through
`check_carried_forward_let_bindings`, which calls the same per-binding
evaluation helper pass 8 uses (`evaluate_let_binding`). Each binding is
**parsed under the context's profile** — the same profile pass 8
parsed it with, never the latest profile, so syntax the profile does
not enable is refused here exactly as at template validation (and a
crate upgrade cannot make job creation accept what pass 8 refused, or
vice versa) — then evaluated under `PathFormat::Posix` with the
caller's budgets. A failure is a check failure like any other: reported
at the binding's path — `steps[i] -> let[j]`,
`steps[i] -> script -> let[j]`,
`steps[i] -> stepEnvironments[k] -> script -> let[j]`,
`jobEnvironments[k] -> script -> let[j]`, or for a SimpleAction
`steps[i] -> <kind> -> let[j]` — with pass 8's message
`Invalid expression in let binding '<name>': <error>` and the caret
aligned to the full `name = expr` binding string, as pass 8 and the
run-time path align it. **Continuation:** as at pass 8, the failed name
is bound `Unresolved(ANY)` and the scope's remaining bindings and
checks still run — a later binding or format string referencing it
evaluates to `Unresolved` instead of cascading `Undefined variable`
errors, and the scope's unrelated violations are still reported. A
failed **step-level** binding additionally makes the step
uninstantiable (host requirements and the parameter space are resolved
for real and need its value), so those two are skipped for that step —
its carried-forward checks, and every later step's, still run, and job
creation fails with the aggregate. Structural checks (names,
duplicates, shadowing, self-reference, count) are template
validation's job and are not repeated; structurally malformed bindings
— no `=`, empty name, empty expression — are skipped rather than
reported: pass 8 rejects all three at decode, so they cannot reach a
template that came through `decode_job_template`, and a hand-built
`JobTemplate` that bypassed decode still fails on them at run time.
The public `evaluate_let_bindings` (below) is the run-time entry point
used by `openjd-sessions` and `openjd-for-js`; the check symtabs do not
use it.

Failures are `ModelError::ModelValidation` at the same field paths
pass 8 uses, e.g.
`steps[0] -> script -> actions -> onRun -> args[0]:` /
`resolves to at least 100000 characters, exceeding the maximum of 1024.`
For a SimpleAction step (§8) the checks run on the desugared
`StepScript` — the same form pass 8 validated — and the paths are
re-rooted onto the field the author wrote through the same
`SimpleActionKind::remap_desugared_path` table pass 8 uses
(`steps[0] -> bash -> script`, `steps[0] -> cmd -> args[1]`,
`steps[0] -> bash -> timeout`, `steps[0] -> bash -> let[0]`, …; see
`specs/model/validation.md` § Reporting on desugared forms). No
diagnostic from this stage names a synthesized node.

**Aggregation.** Like pass 8, job creation reports every check failure
in the template at once: one `ValidationErrors` collection is threaded
through the job environments and every step (`instantiate_step` takes
it as `&mut`) and converted to a single `ModelError::ModelValidation`
once, after all steps are instantiated and before the caller-limit
task-count and size checks. The order is pass 8's order — deterministic
except within one environment's `variables`, which is a `HashMap` and
iterates in arbitrary order (shared with pass 8): `jobEnvironments` in template order, then each step in template
order — its step-level `let`, its script (`let`, `onRun`
`command`/`args`, `timeout`/`cancelation`, embedded-file `data`), then
its `stepEnvironments` in order (`let`, `variables`, every action's
`command`/`args`, every action's `timeout`/`cancelation`, embedded-file
`data`). **Non-check failures are reported alone.** The job name and
parameter constraints are checked before any check failure is
collected, and the caller task-count / size caps after the collection
is converted, so neither interacts with it. Resolving a step's host
requirements or parameter space for real (a value that cannot coerce,
a duplicate resolved capability name, an association length mismatch,
…) aborts immediately, discarding any check failures collected from
earlier scopes. These leave the job unconstructible
rather than describing a defect in a carried-forward field, and they
are reported with their own (pre-existing) error kinds and formats.

**Error policy.** Evaluation/parse errors are reported exactly as in
pass 8. `create_job` requires a context whose revision matches the
template's and whose extensions cover everything the template declares
(a `Compatibility` error otherwise — see `create_job`'s docs), so an
evaluation error at this stage cannot be a context artifact: it is
either a template defect pass 8 missed or a deterministic
value-dependent failure that every session resolving the field would
hit. Both are worth failing at submission. That disjunction being
exhaustive additionally depends on the evaluator propagating
`Unresolved` without error through every operator — this stage
evaluates under a symbol state no other stage sees (`Param.*` concrete,
`Task.*`/`Session.*` unresolved), so an operator that hard-errors on a
partially-resolved input turns a valid template into a submission
failure (as `eval_listcomp`'s concrete-iterable filter path once did —
see ListComp in `specs/expr/evaluator.md`). Budget exceedances
(`MemoryLimitExceeded` / `OperationLimitExceeded`) are reported like
any other error; the evaluator guarantees they propagate out of every
construct that absorbs errors under an unresolved operand — an
unresolved-test conditional whose other branch succeeds, and `and`/`or`
operands past an unresolved one that may pass evaluation on (a value error
there is absorbed — run time may select the other branch or short-circuit —
but the budget was spent in this evaluation regardless; operands after an
unresolved one that always decides, such as `Task.Param.I or …`, are not
evaluated, as at run time; see IfExp and BoolOp in
`specs/expr/evaluator.md`). One coarseness caveat:
for an unresolved-test conditional the evaluator charges both branches
against the budget, while a run-time evaluation with the test resolved
charges one, so a budget within a branch-cost of the limit can fail
here and pass there. Callers lowering the budgets accept that
granularity. Resolved-value constraint violations (the table above)
are always reported.

Cost note: job creation previously did not evaluate these fields, so the pass
adds work proportional to what a single worker would do anyway — done
once at submission instead of per-task-per-worker.

### The submission stage

The last stage of `create_job_with_environment_templates` (Template Schemas §3.2.1–§3.2.2, §4
item 4), the crate-private `apply_environment_templates` in `job/create_job/external.rs`. A
submission combines one Job Template with zero or more Environment Templates that the
scheduler attaches, in its order. It takes the Job that job creation proper built from the
Job Template, what job creation decided for each Step (its frozen entries, and which
`jobEnvironments` entries its Sessions enter, after their own `enableIf`), the `Job.EnvNames`
value, and the merged values
`preprocess_job_parameters` produced with the same Environment Templates. `ctx` supplies the
caller limits and the path policy; its profile is not read. Each attached template is
evaluated under its own profile (`EnvironmentTemplate::profile()`), since an extension applies
to the document that lists it (§1.2 item 3): an attachment that lists `ENV_PARAMS` lists `EXPR`
too, so its `default`s may use `EXPR` when the Job Template lists neither, and a Job Template
that has entries lists both.

1. Each attachment's literal `default`s are checked, as job creation checks the Job
   Template's. An attachment named like a Job Environment gets a warning (§3.2.1 item 4) when
   the Job Template lists `ENV_PARAMS` (without it the Job Template has no entries, and the
   submission behaves as before the extension, RFC Backward compatibility); if its `activation`
   is `ALWAYS` and it has parameters without a `default`, every Session of a Step for which its
   own `enableIf` does not disable it enters it, and no entry can supply them, so that is one
   error at `<attachment> -> environment -> envParameterDefinitions`, and those parameters are
   not reported again per Step under constraint 7. The error is reported after every Step's
   values, when the attachment's `enableIf` (decided in step 3, with the parameters unknown)
   is not `false` for some Step; one disabled for every Step is not entered, so it is not an
   error (§3.2.2). One with `activation: ON_REQUEST` is never entered, since no entry can
   request it, so it is not an error. (Warnings are returned only with a successful result.)
2. Each Step's frozen entries are matched to the attachments by `environment.name`:
   - exactly one: the entry refers to it, whatever `required` says (§3.2.1 item 1);
   - none: with `required` true or absent, an error at
     `JobTemplate -> steps[i] -> dependencies[j] -> environment` naming the Environment and the
     attached documents; with `required: false`, the entry is ignored and a warning is recorded
     (§3.2.1 item 2; RFC "External Environments are named implicitly" item 3);
   - two or more: an error naming the attachments (§3.2.1 item 3). Two same-named attachments
     that no entry names are not an error (plan A11), unless their `activation` is `ALWAYS` and they
     have parameters without a `default`: no entry can reach them, so each such attachment
     that its `enableIf` does not disable for every Step gets one error at its
     `envParameterDefinitions`, as in step 1, and those parameters are not reported per Step.

   A disabled entry was dropped by job creation, so it is not resolved and gets no error or
   warning. An attachment named like a Step's `stepEnvironments` entry is unreachable from that
   Step only because template validation rejects an entry naming one (constraint 2); other
   Steps reach it (§3.2.1 item 5). If its `activation` is `ALWAYS` and it has parameters without a
   `default`, that Step gets an error saying it cannot reach the attachment, instead of the
   constraint 7 "must supply" message.
3. A Step's Sessions enter every attachment with `activation: ALWAYS` and each one with
   `activation: ON_REQUEST` that one of its entries refers to (§3.2.1, after item 5). A
   Job Template without ENV_PARAMS has no entries, so its Steps never enter an
   `ON_REQUEST` attachment (§1.2 item 3). Every Step gets effective values for every
   attachment it enters that declares parameters, from its entry if one refers to it and from
   the `default`s otherwise, with the same conversion and checks as job creation, and the
   attachment's own `enableIf` is decided with the values that are known, as job creation
   decides a Job Environment's (see
   [Disabled Environments](#environment-parameter-values-env_params)); a disabled
   attachment is not entered, its values are not checked, and it is not held to constraint 7.
   An
   attachment that one of the Step's rejected entries names (an ambiguous name) is skipped for
   that Step, so the one mistake is not also reported as "must supply" or "does not supply"
   errors. The entry's format strings resolve against its frozen `symtab`. A key the
   attachment does not declare is an error (constraint 5), including any key for an
   attachment that declares no parameters, such as one written before ENV_PARAMS; and a parameter
   without a `default` of an attachment the Step enters that the
   Step does not supply is an error at `JobTemplate -> steps[i]` (constraint 7 over the
   combined Job, §3.2.1 last paragraph). The lists in these messages are capped as template
   validation caps them (at most 50 names and 512 characters, the first name always listed,
   then "and N more"), with the shared functions `capped_name_list` and
   `missing_parameters_message`, and only an entry's first undeclared key lists the declared
   names. The lists of attachments in the "no attached Environment Template defines it" and
   "ambiguous" messages are capped the same way, since labels are caller-supplied. So a
   submission's error output is linear in the number of Steps and entries whatever the name
   and label lengths.
4. The attachments' Environments are converted with `convert_environment_with_symtab`, with the
   merged Job Parameters, plus `Job.Name` and `Job.EnvNames` when the attachment lists `EXPR`,
   as the frozen table.
5. The Job is combined: `job_environments` becomes the attachments followed by the Job's own
   (a Session enters the attachments first), and each Step's `environment_parameters` becomes
   its values for the attachments followed by its own.
6. Each Step's `environments` lists the combined Job Environments its Sessions enter after
   every Environment's own `enableIf` (the Job Template's, decided by job creation proper; the
   attachments', in step 3; see [Environment enableIf](#environment-enableif-env_params)), or
   is `None` when it enters every one.
7. The `WRAP_ACTIONS` single-layer rule (RFC 0008) is checked over each Step's final
   Environment set: the combined Job Environments it enters and its Step Environments. More
   than one Environment defining wrap hooks is an error at `JobTemplate -> steps[i]` naming
   them, the attached ones with their document. Template validation checks the Job Template's
   own layers that are certainly entered, by entries and Environments whose `enableIf` is
   absent or a boolean scalar (see [validation.md](validation.md)); this check also covers an
   `enableIf` expression, on an entry or an Environment, and the attached Environments
   (prototype plan R8).
8. Each Step gets its combined host requirements (see
   [Combined host requirements](#combined-host-requirements-env_params)), the last stage of
   §7.4.

Every error is collected into one `ModelValidation` error for the model name `Submission`, in
the order above (so a later Step's resolution error precedes an earlier Step's value error,
the attachments' failed `enableIf` evaluations and then the unreachable attachments' errors
of step 1 follow every Step's values, and host-requirement errors come last),
and every path starts with the document: `JobTemplate`, `EnvironmentTemplate[i]`, or the
attachment's label. The stage does not re-run the carried-forward resolved-value checks on
the attachments' own format strings (the `SERVICE` prototype's submission function does).

The submission stage runs only when job creation proper succeeded: a Job Template error
(for example an `enableIf` expression that resolves to `null`) is reported alone, for `JobTemplate`, even when the
submission stage would also have failed.

#### Environment enableIf (ENV_PARAMS)

An Environment's own `enableIf` (Template Schemas §4 item 6) is an expression field of type
`bool`, decided for each Step whose Sessions would otherwise enter the Environment: a Step
Environment's by the expression-field checker's `resolve`, and a Job or attached
Environment's by `resolve_progressive`, with the Step's unknown values unresolved, once per
distinct set of known values. A boolean scalar is its value. Where it is
`false` for a Step, that Step's Sessions do not enter the Environment: it is not in the
Step's sets, so it is not entered or exited, sets no `variables`, is not in the stack, gets
no values, and adds no host requirements (which are not resolved for that Step). Two places
decide it, by what the expression can read:

- **A Step Environment** (constraint 2) reads no effective values, so its `enableIf` is
  decided by `resolve_step_entries` (`job/create_job/env_params.rs`), with the entries'
  `enableIf`s, in the same scope, profile, library, and budgets, with POSIX paths (the Step's
  full template scope as job creation built it; an `enableIf` is not host context, so the
  submission path format does not apply to it). A disabled Step Environment is
  dropped from `job::Step::step_environments`; when that leaves none the field is `None`. An
  error (a `null` result: "resolved to null, which does not fit type bool (Template Schemas
  §7.5)."; another type; or an evaluation failure) is a `JobTemplate` error at
  `steps[i] -> stepEnvironments[j] -> enableIf`, and the Step Environment is kept. This is
  earlier than §7.4 lists it, which changes no result, but an error here is reported by the
  `create_job` body and so hides the submission stage's errors (plan 3 A38).
- **A Job Environment or an attached Environment** (constraint 1) reads its own effective
  values, so its `enableIf` is decided with them (see
  [Disabled Environments](#environment-parameter-values-env_params)): a Job Environment's by
  `resolve_step_entries`, for each Step, in the `create_job` body; an attachment's by the
  submission stage, in step 3. Either way it is decided after the Step's effective values are
  computed and before their checks and constraint 7, the single-layer rule, and host
  requirements (§7.4). It is evaluated under its document's template-scope profile, with the
  caller's budgets and the POSIX path format, in its `hostRequirements` scope (the merged Job
  Parameters, `Job.Name`, `Job.EnvNames`, its `Env.RawParam.*` of any type, and its non-PATH
  `Env.Param.*`, each known or `unresolved[T]`). A `false` leaves the
  Environment out of the Step's entered list and values, so `job::Step::environments` and
  `environment_parameters` leave it out, and its values for the Step are not checked. An error
  is reported at `<environment path> -> enableIf` with "Resolved for Step 'A'." (or "Steps 'A'
  and 'B'") appended: for a Job Environment a `JobTemplate` error at `jobEnvironments[i] ->
  enableIf`, reported with the other job-creation errors, so it hides the submission stage's;
  for an attachment a `Submission` error. The Environment is then checked for those Steps as if
  it is not disabled, so their other mistakes are reported in the same run (plan 4 A40,
  revised): its values and constraint 7; and for an attachment also the single-layer rule, the
  unreachable-attachment check (whose message then says the `enableIf` could not be evaluated, in place of "does not disable it"),
  and its combined host requirements. A Job Environment's failure stops the submission stage,
  which makes those last checks.

`Job.EnvNames` lists every Job Environment whatever its `enableIf` (§7.3.1). A disabled
Environment is not held to constraint 7 for the Step, and the Step's values for it are not
checked (§3.2.2), which replaces plan 3's A33. Session grouping
(`Step::same_environments`) compares the final sets. `CreatedJob::disabled_environments`
records, per Step, the Environments an `enableIf` disabled (Job Environments by document and
name, attached ones first, Step Environments by name), for display; an Environment the Step
would not have entered anyway (an `ON_REQUEST` one it does not request) is not listed, since
its `enableIf` is never evaluated for that Step, and neither is one whose `enableIf` failed.

#### Combined host requirements (ENV_PARAMS)

`combine_step_host_requirements` in `job/create_job/host_requirements.rs` (Template Schemas
§4 item 5.1, §3.3.3). It runs over the combined Job, so the Job Template's own
Environments and the attached ones are handled alike.

- **Resolution.** Each Environment with `hostRequirements` is resolved for the Steps whose
  Sessions enter it (step 6's final sets). Steps whose effective values for it have the same
  transport form share one resolution, so an error is reported once; this is stricter than
  §3.2.2 equality, since FLOAT `1.0` and `1.00` are equal values whose text an expression can
  read, so they resolve separately. A Step whose values for an Environment that declares
  parameters were rejected (a failed or missing value, or an ambiguous entry, already
  reported) is skipped for that Environment, so it gets no extra "Undefined variable" error. The table is the merged Job
  Parameters, `Job.Name`, `Job.EnvNames`, and the Environment's `Env.RawParam.*` of any type
  and non-PATH `Env.Param.*` from the Step's effective
  values. Resolution and its checks are the Step's own (`resolve_host_requirements`, shared
  through a `Site` that only decides how errors read): finite numbers, non-negative `min`,
  positive `max`, `min` ≤ `max` and at least one bound within one requirement, unique names
  within one source ignoring case, the capability-name and attribute-value grammar, and the
  resolved `anyOf`/`allOf` lengths. A failure is an error at the Environment's field, for
  example `JobTemplate -> jobEnvironments[0] -> hostRequirements -> amounts[0] -> min`, with
  "Resolved for Step 'A'." (or "Steps 'A' and 'B'", a list capped as the other ENV_PARAMS
  messages cap theirs) appended to the message's first line. The Step's own requirements are
  resolved earlier, by job creation proper, with their errors unchanged.
- **Combination.** The sources are the Step's own requirements, then each entered
  Environment's in Job order. An Environment the Step does not enter (an `ON_REQUEST`
  one it does not request, a `required: false` entry that resolved to nothing, a disabled
  entry, or an Environment whose own `enableIf` is `false` for the Step) contributes nothing.
  `combine` collects every source's requirements on a capability (names compared ignoring
  case) and combines them at once, so what a host must satisfy does not depend on the order
  of the sources (plan 5 S1); only the stored spelling and value order follow first appearance.
  Amounts merge to the greatest `min` (a missing `min` counts as 0, and stays missing when every source omits it) and the least `max`; amounts do not add. For
  `attr.worker.os.family` and `attr.worker.cpu.arch` (§3.3.3 item 2.1), the requirements
  become one `anyOf` of the values in every `anyOf` and every `allOf` list, values compared
  ignoring case, in the order and spelling of their first appearance (a source's `anyOf`
  before its `allOf`), even when that is one value. For any other attribute (item 2.2), *B* is the union of every `allOf` list and the
  value of every one-value `anyOf` (`[q, Q]` is one value; plan 5 A51), in first-appearance
  order (a source's `anyOf` before its `allOf`) and spelling; the requirements become the
  `allOf` of *B*, when it is not empty, followed by each `anyOf` of two or more values that
  has no value of *B*, as written and in source order (an `anyOf` with a value of *B* is
  implied, so dropped; equal `anyOf`s are each kept, plan 5 A50). So `anyOf [a]` and `anyOf
  [b]` are `allOf [a, b]`. A combined entry keeps the name spelling of the first source that
  names the capability (plan 2 A30), and capabilities keep the order of their first
  appearance. A capability only one source names is kept as written, so a lone `anyOf [a]`
  stays an `anyOf`, and a requirement with both `anyOf` and `allOf` stays one entry.
- **Conflicts.** A merged amount whose `min` exceeds its `max`, and an item 2.1 attribute
  with no value in every list, are errors at `JobTemplate -> steps[i]` naming the Step, the
  capability, and the sources: for an amount, the first with the greatest `min` and the
  first with the least `max`; for an attribute, every source that names it (plan 5 A49), each as
  its `... -> hostRequirements` path with the Environment's name, the list capped as the
  other job-creation lists are (512 characters, then "and N more"). Combining adds no other
  error: requirements on any other attribute can always be satisfied. The §3.3 list-length
  and duplicate-name limits are not applied to the combined set.
- **Result.** The combined set replaces `job::Step::host_requirements`, in the same shape. A
  Step none of whose entered Environments has requirements keeps its own unchanged, so a Job
  without Environment requirements serializes as before.

The result, `CreatedJob { job, environment_documents, warnings, disabled_environments }`,
holds the combined Job, the document of each of its `job_environments`, the warnings as
`SubmissionWarning { path, message }`, and for each Step the Environments an `enableIf`
disabled (`StepDisabledEnvironments`; see
[Environment enableIf](#environment-enableif-env_params)).

`AttachedEnvironmentTemplate` matches the `SERVICE` prototype's (`service-extension`
branch). That branch's public `apply_environment_templates` takes `&CallerLimits` and
returns a delta (`AppliedEnvironmentTemplates` with Services, requirement bindings, and the
attached Environments) that `into_combined_job` folds into the Job; this stage is
crate-private, takes `&ValidationContext` because it needs the submission path policy, and
returns the combined Job. Merging the two is a follow-up (prototype plan P1).

### check_environment_templates

```rust
pub fn check_environment_templates(
    job_template: &JobTemplate,
    attached: &[AttachedEnvironmentTemplate<'_>],
    ctx: &ValidationContext,
) -> Result<Vec<SubmissionWarning>, ModelError>
```

The checks of [the submission stage](#the-submission-stage) that need no Job Parameter values,
run on a decoded Job Template (Template Schemas §3.2 last paragraph: a validator given the Environment
Templates can check constraints 5–7 early; prototype plan A9). Also in
`job/create_job/external.rs`; `openjd check --environment` calls it. The two functions share
the attachment index (`Attachments`), the name resolution of an entry (`match_entry`), and the
sorting of a Step's missing parameters (`sort_missing`), so they report the same conditions
with the same messages and paths, in the same order (except as items 4 and 5 say for the
wrap-layer rule and host requirements):

1. step 1 above, with an empty symbol table: a literal `default` reads no symbols; the error
   for an unreachable attachment is reported only when its own `enableIf` is absent or `true`,
   or certainly unresolved with no values supplied (see
   [validation.md](validation.md#pass-11-env_params-gating)), since one with `false` is not entered and
   one with any other expression is undetermined (§7.5 rule 5);
2. step 2 above, over each Step's template `dependencies`: every `environment:` entry that
   names no `jobEnvironments` entry (the first entry for a name; a repeat is a
   template-validation error), with `required` defaulting to `true`. An entry whose `enableIf`
   is `false` is as if absent. One whose `enableIf` is an expression (even a constant one such
   as `"true"`) is enabled or not only
   when the Job is created, so it is not resolved here, and every attachment it may name is
   skipped for its Step;
3. step 3 above, without resolving anything that needs Job Parameter values
   (`DeclaringEnvironment::check_step_statically`): constraint 5; for each supplied value, a
   literal converted and checked as at submission (type, constraints, and PATH absoluteness
   in `ctx.submission_path_format`, which needs no Job Parameter values), and a format string checked only against the
   single-expression rule for a list-typed parameter; and constraint 7, over the attachments
   the Step enters (every `ALWAYS` one, and each `ON_REQUEST` one an entry refers to). The
   value checks and constraint 7 are made only for an attachment whose own `enableIf` is
   absent or `true`, or certainly unresolved with the Step's values (see
   [validation.md](validation.md#pass-11-env_params-gating); the constraint 7 message then carries the
   unresolved note): one whose `enableIf` is `false` is disabled, and one whose `enableIf` is
   any other expression is undetermined, so a violation need not hold whatever it resolves to
   (§3.2.2, §7.5 rule 5); constraint 5 is checked for every attachment;
4. the `WRAP_ACTIONS` single-layer rule (`check_attached_wrap_layers`), for each Step whose
   Sessions enter a wrapping attachment, over the wrapping Environments the Step's Sessions
   certainly enter, in Session order: attachments with `activation: ALWAYS` and the
   `ON_REQUEST` ones an entry resolved to, Job Environments with `ALWAYS` and the
   `ON_REQUEST` ones an entry enabled by an absent or boolean-scalar `enableIf` requests, then the Step's Step
   Environments, each only when its own `enableIf` is absent or the scalar `true` (one with
   the scalar `false` is not entered, and one with an expression is decided at job
   creation). Job creation's message is reused (`wrap_layers_message`), but it lists only
   the layers the Step certainly enters, so when an `enableIf` expression on an entry or an
   Environment brings in another one, job creation's message counts and lists more. A Step whose
   wrapping Environments are all the Job Template's own is left to template validation;
5. the §3.3.3 combination over literal requirements
   (`host_requirements::check_literal_combinations`): for each Step, the requirements of its
   own `hostRequirements` and of each Environment its Sessions certainly enter (as in item 4,
   so an Environment whose own `enableIf` is not absent or `true` is left out)
   whose format strings are all literals (`literal_part`), resolved with an empty symbol table
   and combined by the same `combine` job creation uses, so a conflict among literal sources
   has job creation's message. When a non-literal requirement of the same capability also
   takes part, job creation may name other sources or bounds, or report a conflict this
   check cannot see. An Environment whose values for the Step fail here (reported by item 3,
   or, for an attachment no entry can reach, by item 1's attachment-level error for its
   parameters without a `default`) is still combined, while job creation skips it until its values are fixed, so `check` can
   report one more conflict than `openjd run` for such a submission, or name that Environment
   where `openjd run` names another source with the same bound. An attachment that
   declares parameters and that an ambiguous entry of the Step names is left out, since job
   creation computes no values for it and so skips its requirements. An `ALWAYS` attachment that
   an entry with an `enableIf` expression names is kept, since the Step's Sessions enter it
   either way. A conflict that involves
   only the Job Template's own requirements (a Job Environment's with its Step's, no
   attachment) is reported here too, though `openjd check` without `--environment` and
   template validation leave it to job creation.
   Combining a subset of a Step's requirements finds only conflicts the full combination also
   has, since more sources can only raise a `min`, lower a `max`, or narrow the values common
   to every list of an `attr.worker.os.family` or `attr.worker.cpu.arch` requirement. A
   Step with no literal Environment requirement is skipped.

`ctx` supplies the caller limits and the path policy, as for job creation. A submission that passes can still be
rejected at job creation, for a format-string value or non-literal `default` that does not
resolve to a valid value with the submitted Job Parameters, for an entry or an Environment
whose `enableIf` is an expression (and the Environments it may bring into a Step's set), or
for a host requirement that is not a literal.

### convert_environment

```rust
pub fn convert_environment(env: &template::Environment) -> job::Environment
```

Converts a template environment to a resolved job environment. Takes 1 argument and is
infallible. Environment variables and script fields remain as FormatString (session-scope).

A separate `convert_environment_with_symtab` function accepts an optional `&SymbolTable`
to filter the symbol table to only symbols referenced by the environment's format strings.

### evaluate_let_bindings

```rust
pub fn evaluate_let_bindings(
    bindings: &[String],
    symtab: &SymbolTable,
    library: Option<&FunctionLibrary>,
    path_format: PathFormat,
    memory_limit: Option<usize>,
    operation_limit: Option<usize>,
) -> Result<SymbolTable, ModelError>
```

Evaluates `"name = expression"` bindings sequentially. Each binding sees the results of
all prior bindings in the same block. Returns a new symbol table with the binding results
added.

The `library` parameter is optional (pass `None` for template-scope bindings that don't
need host functions). `path_format` controls path construction behavior.
`memory_limit` / `operation_limit` bound each binding's evaluation
(`CallerLimits::max_eval_memory_bytes` / `max_eval_operations`); `None`
uses the spec-recommended defaults.

## Design Decisions

### Explicit Instantiation (vs Generic Traversal)

The Python library uses `instantiate_model()` which generically traverses Pydantic models
via metadata to find and resolve FormatString fields. The Rust crate uses explicit conversion
methods on each type instead. This is more verbose but:

- Makes template-scope vs session-scope distinction explicit and compiler-verified
- Avoids runtime reflection or trait object overhead
- Makes it clear which fields are resolved at which phase
- Allows different resolution strategies per field (e.g., host requirements resolve
  FormatStrings to f64, while step names resolve to String)

### Merged Constraint Validation

When multiple templates define the same parameter, constraints are merged (intersection for
allowedValues, most restrictive for ranges). The merged constraints are validated for
consistency — e.g., if the intersection of allowedValues is empty, or if the merged
min > merged max, that's an error. This catches conflicts that individual template
validation wouldn't find.

### Path Normalization Without Filesystem Access

`normalize_path_str` performs pure path normalization (resolving `.` and `..` components)
without filesystem access. This is important because:
- Templates may reference paths that don't exist yet
- The crate should be usable in environments without filesystem access
- Path mapping (for cross-platform execution) happens at session time, not job creation time
