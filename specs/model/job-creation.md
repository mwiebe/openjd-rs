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

**PATH handling:**
- User-provided relative paths joined to `current_working_dir`
- Default relative paths joined to `job_template_dir`
- URI paths (`s3://`, `https://`) preserved as-is when EXPR extension is enabled
- `allow_template_dir_walk_up` controls whether paths can traverse above `job_template_dir`. When `false`, a default is rejected unless its normalized form is `job_template_dir` itself or a descendant. Containment is checked per path component, not by raw string prefix.

**Value coercion:**
- `coerce_from_str` — Parses string input (CLI): numeric parsing, boolean aliases
  (`yes`/`no`/`on`/`off`/`1`/`0`), JSON list parsing for list types
- `coerce_to_job_parameter_type` — Validates typed input (library): type compatibility, numeric
  widening (int → float), list element type validation

**Round trip:** a value `preprocess_job_parameters` returns should be a value it accepts as input.
Both its input and output types are public, so a caller can hand its output straight back to it.
`create_job` relies on something adjacent for every caller: it re-runs `check_constraints` over each
value it is given (`create_job/mod.rs:54`-`:59`).

A goal rather than an established invariant. One case is known to violate it: a scalar `PATH` with a
**relative** default plus a `maxLength`, `minLength` or `allowedValues` constraint. The first pass joins
the default to `job_template_dir` and does not constrain-check a default, while the second pass receives
the joined absolute path as submitted input and measures the constraint against it. Measured, a relative
default of `out` under `maxLength: 8` reports `value length 72 exceeds maximum 8` on the second pass.

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
```

Full template instantiation pipeline. Environment templates should
already be merged into `job_parameter_values` via `preprocess_job_parameters` before
calling this function. `ctx` carries the model profile and the
`CallerLimits` that apply to this job instance. Its revision must
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
     treatment in `instantiate`. String-backed FLOAT range elements are trimmed
     and must resolve to finite `f64` values.
   - Step-level let bindings
   - With SERVICE (RFC 0009): every Service's `<Service>.let`, numeric
     `@fmtstring` fields, and `hostRequirements` (see "Services" below)
3. With EXPR extension: inject `Job.Name` (before step instantiation)
   and `Step.Name` (per step) into the symbol table
4. Carry forward session/task-scope fields as FormatString (plus action
   `timeout`/`notifyPeriodInSeconds`, which validate in template scope but
   resolve on the worker)
5. Run the resolved-value checks on the carried-forward fields
   (next section)
6. Convert environments from template to job types
7. Build step dependency list
8. Attach resolved symbol table to each step

`instantiate_step` and `instantiate_service` share an `InstantiateCtx` (the
extension flags, limits, budgets, and the template's `jobServices` slice,
which every Step's scope needs) and the template-scope `let` evaluator
`evaluate_template_let_bindings`, which `<StepTemplate>.let` and
`<Service>.let` both use. `resolve_host_requirements` takes the owner's
path (`steps[i]`, `jobServices[k]`, `steps[i] -> stepServices[k]`) so the
same code — and the same messages — serve a Step's and a Service's
`hostRequirements`; every path it reports is built from that prefix.

#### Services (RFC 0009, Template Schemas §9)

`jobServices` are instantiated in job scope before the steps (every Step's
Task Sessions may reference them), each seeing the Services before it in the
list; `stepServices` are instantiated inside `instantiate_step` in the
Step's scope (`Step.Name` and the step-level `let` values are available),
each seeing every Job Service and the Step Services before it. Per Service
(`instantiate_service`, path `jobServices[k]` or
`steps[i] -> stepServices[k]`):

1. **`<Service>.let`** — evaluated with `evaluate_template_let_bindings`
   into a clone of the scope's symbol table (template library, no PATH
   `Param.*`, no host context). A binding that fails reports
   `service let binding '<name>': <error>` (an `Expression` error); pass 8
   already type-checked it with everything unresolved, so a failure comes
   from the real parameter values.
2. **`hostRequirements`** — `resolve_host_requirements` against that table,
   with every check decode and pass 8 could not finish on a non-literal
   value (name pattern and uniqueness, bounds, attribute values) re-applied
   on the resolved values, reported at the Service's path.
3. **Numeric `@fmtstring` fields** (§9.2 note) — `resolve_service_int`
   resolves `port`, `readinessCheck.timeoutSeconds` / `intervalSeconds`,
   and `restartPolicy.maxAttempts` with target type `int?`, the same target
   pass 8 validated them with: a whole-field `null` is "not provided"
   (`port: None`, or the §9 default — 300 s timeout, 5 s interval, 0
   attempts); a multi-segment string concatenates and parses with
   surrounding whitespace tolerated (`must be an integer.` otherwise); the
   result must lie in the field's range (`must be between 1 and 65535.`,
   `must be > 0.`, `must be >= 0.`), reported as a `ModelValidation` error
   at the field path with pass 11's wording. A resolution failure (a
   whole-field value the `int?` target rejects) is a `FormatStringError`
   whose message carries the field path. The §9 defaults also fill the
   `readinessCheck` / `restartPolicy` objects when the template omits
   them, and a `TCP_CONNECT` check without `ports` is expanded to every
   declared port, so `job::Service` never needs the template defaults.
4. **Carried-forward re-checks** — `build_service_check_symtab` extends the
   Service's table with the `Unresolved` placeholders the Service Session
   binds (`Session.*`, PATH `Param.*`, `Service.File.*` for its embedded
   files, its own `Service.<name>.<port>.*` including `bindAddress`, and
   the `port` / `connectAddress` of every in-scope Service), evaluates the
   `<ServiceScript>.let` bindings into it (`script let binding '<name>':
   ...` on failure), and `check_carried_forward_service` re-runs the pass
   8 constraints on `variables` (§4.4.2 length), every action's
   `command` / `args`, and embedded-file `data` — the Service counterpart
   of `check_carried_forward_environment`.
5. **`serviceEnvironments` re-checks** (§9 item 5) — for each entry,
   `build_service_env_check_symtab` builds the Service Environment's check
   table: the Service's job-creation table (`Param.*`, `RawParam.*`,
   `Job.Name`; for a Step Service `Step.Name` and the step-level `let`
   values; and the `<Service>.let` values, which §9 item 3 makes available
   in `serviceEnvironments` as a Step's are in `stepEnvironments`) plus the
   `Unresolved` placeholders for `Session.*`, PATH `Param.*`, the declaring
   Service's own `Service.<name>.<port>.*` with `bindAddress`, the `port` /
   `connectAddress` of every in-scope Service, and the Environment's own
   `Env.File.*`, with its `<EnvironmentScript>.let` evaluated in. Unlike
   `build_env_check_symtab` the `Service.*` seeding is unconditional and
   includes the declaring Service (a Service Environment is entered only in
   that Service's Session). `check_carried_forward_environment` then re-runs
   the pass 8 constraints at `<service path> -> serviceEnvironments[j]`.
6. **Conversion** — `variables` and `script` are carried as
   `FormatString`s; `resolved_symtab` is `filter_symtab_for_service`
   (the symbols those fields and `<ServiceScript>.let` reference, with the
   `RawParam.*` fallback). Each `serviceEnvironments` entry is converted
   with `convert_environment_with_symtab` against the Service's
   job-creation table — the path a Job Environment takes — so each carries
   its own filtered `resolved_symtab` into
   `job::Service::service_environments`.

The check symbol tables of the entities *around* a Service also change:
`build_task_check_symtab` seeds the `port` / `connectAddress` of every Job
Service and the Step's own Services, and `build_env_check_symtab` seeds the
environment's in-scope Services (Job Services for a job environment, plus
the Step's for a step environment) only when the environment's `runScope`
excludes `SERVICE` — the same scope rules pass 8 applied, so a reference
that validated resolves here and at run time.

Environment conversion carries `runScope` (parsed to `Vec<RunScope>`) and
the four `onWrapService*` hooks into `job::Environment`.

`job::service_symbols::referenced_service_names` walks a Service's
`service_environments` too (`collect_env_accessed_symbols`, the same walk
`filter_symtab_for_environment` uses), so a Service that references another
only from a Service Environment still starts after it (RFC 0009 ordering
constraint 2).

#### Resolved-value checks on carried-forward fields

Of the spec's three processing stages (Template Schemas §7.4: template
validation, job creation, task execution on the worker host), job
creation is the first at which job parameters have real values. The
session/task-scope format strings it carries forward unresolved —
action `command`/`args`, environment `variables` values, embedded-file
`data` — are statically evaluated against a **check symbol table**, and
exactly the resolved-value checks pass 8 applies to those fields re-run
on the result:

| Field | Constraint |
|---|---|
| environment `variables` values | resolved-length bound vs 2048 (§4.4.2, spec-mandated, always on) |
| Service `variables` values (RFC 0009) | the same §4.4.2 bound |
| Service action `command`/`args`, embedded-file `data` (RFC 0009) | the same opt-in caps as a Step's / Environment's |
| action `command`, each `args[*]` entry | bound vs `CallerLimits::max_resolved_arg_len`, if set |
| embedded file `data` | bound vs `CallerLimits::max_resolved_data_len`, if set |

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
  `Task.*`, plus this environment's `Env.File.*` (`Unresolved`) and its
  script-level `let` bindings evaluated in.
  An environment `let` binding that fails to evaluate fails job
  creation, under the same error policy as every other evaluation this
  stage performs (see below): the bindings only evaluate when the
  context profile enables EXPR, their expressions already type-checked
  at pass 8 with everything unresolved, so a failure here comes from
  the real parameter values and would deterministically recur in every
  session that enters the environment.

Both scopes evaluate their script-level `let` bindings through one
shared path: each binding is **parsed under the context's host
profile** — the same profile pass 8 parsed it with, never the latest
profile, so syntax the profile does not enable is refused here exactly
as at template validation (and a crate upgrade cannot make job creation
accept what pass 8 refused, or vice versa) — then evaluated under
`PathFormat::Posix` with the caller's budgets. Failures from either
scope carry the same `script let binding '<name>': <error>` diagnostic,
with the caret aligned to the bare expression (pass 8 and the run-time
path align it to the full `name = expr` binding string; the check path
reports the expression alone because its message already names the
binding). Structurally malformed bindings — no `=`, empty name, empty
expression — are skipped rather than reported: pass 8 rejects all three
at decode, so they cannot reach a template that came through
`decode_job_template`, and a hand-built `JobTemplate` that bypassed
decode still fails on them at run time. The public
`evaluate_let_bindings` (below) is the run-time entry point used by
`openjd-sessions` and `openjd-for-js`; the check symtabs do not use it.

Failures are `ModelError::ModelValidation` at the same field paths
pass 8 uses, e.g.
`steps[0] -> script -> actions -> onRun -> args[0]:` /
`resolves to at least 100000 characters, exceeding the maximum of 1024.`
Violations accumulate within one scope (a step script, one
environment's fields), but the first failing scope stops instantiation
— consistent with the fail-fast resolved-value re-checks `create_job`
already performs, and unlike pass 8's whole-template aggregation.

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
operands past an unresolved one (a value error there is absorbed —
run time may select the other branch or short-circuit — but the budget
was spent in this evaluation regardless; see IfExp and BoolOp in
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

### apply_environment_templates

```rust
pub fn apply_environment_templates(
    job: &job::Job,
    attached: &[AttachedEnvironmentTemplate<'_>],
    job_parameter_values: &JobParameterValues,
    caller_limits: &CallerLimits,
) -> Result<AppliedEnvironmentTemplates, ModelError>
```

Applies the Environment Templates of a submission to the Job that
`create_job` built from the Job Template alone (Template Schemas §1.2.2
"Services from Environment Templates", RFC 0009 "Environment Template" and
the "Validation" section's submission-time check). `create_job` stays
a function of the Job Template only; this is the second stage of a
submission, and the only place the crate sees several documents together.

Inputs: `attached` in the scheduler's order (an
`AttachedEnvironmentTemplate` is a `&EnvironmentTemplate` plus an optional
label — a file path, say — that replaces the positional document name
`EnvironmentTemplate[i]` in error paths and messages);
`job_parameter_values` as `preprocess_job_parameters` produced them for the
same submission (every template's `parameterDefinitions` merged per
§1.2.1); and the caller's limits, applied to every template as they were
to the Job Template.

Output: `AppliedEnvironmentTemplates { external_services, environments,
environment_documents }` — the external Services instantiated in start
order (attachment order, then each template's `services` order), each
stamped with its attachment as `job::Service::document`; the attached
Environments converted in attachment order (a services-only template
contributes none); and, index for index with `environments`, the
`job::Document` each came from (`combined_environment_documents(&job)`
extends the list with `JobTemplate` for the Job's own, matching the
folded `job_environments`). `into_combined_job(job)` folds them into the Job: `job_services`
becomes external Services followed by the Job Template's own, and
`job_environments` the attached Environments followed by the Job
Template's own (merge rule 1: "placed before every Service in the Job
Template's `jobServices`; the attached Environments are placed in
`jobEnvironments` as today"). `Job::extensions` is left as the Job
Template declared it — an extension applies to the document that lists
it (§1.2 item 3), and every external Service and attached Environment
carries the symbols it needs in its own `resolved_symtab`. A runtime that
enters the attached Environments itself (as the CLI does today) may read
the two lists instead of folding.

**Per-document profile.** Each attachment is evaluated under its own
`EnvironmentTemplate::profile()` (its `specificationVersion` and
`extensions`, the counterpart of `JobTemplate::profile`) with the caller
limits layered on. The Job Template need not declare `SERVICE` or `EXPR`
for an attachment to use them, and vice versa: the RFC's queue-cache
example applies a `[SERVICE, EXPR, FEATURE_BUNDLE_1]` attachment to a Job
Template with no `extensions` at all. The symbol table for a template is
`build_symbol_table(job_parameter_values)` (the merged `Param.*` /
`RawParam.*`), plus `Job.Name` when that template declares `EXPR` — the
same table pass 8 validated the document against, now with real values.

**Order of work.** The submission-time check runs first, against the
combined Job, and every violation is reported in one `ModelValidation`
error for the model name `Submission` (no single template is "the"
model). Only if it passes are the Services instantiated and the
Environments converted:

1. **Merge rule 2 — Service names are scoped to their document.** Nothing
   is rejected: an external Service may have the same `name` as a Service
   in another attachment or in the Job Template's `jobServices` or any
   Step's `stepServices`. Every `Service.*` reference resolves within its
   own document (template validation, pass 8, enforces that per document),
   so no name is ever looked up across documents; what the merge must do
   is keep same-named Services distinct. Each external Service is stamped
   with `document = Document::EnvironmentTemplate { index, label }` (its
   0-based attachment index and the caller's label, if any) after
   `instantiate_service`, while `create_job`'s own Services keep the
   default `Document::JobTemplate`; a scheduler keys Services on
   `(document, name)`. A repeat within one document remains a
   template-validation error (§9.7 item 5). The combined Service list is
   still assembled with provenance (every external Service, then the Job's
   `jobServices`, then each Step's `stepServices`) — the wrapper check
   below names a witness from it.

2. **Merge rule 3 — wrapping Environments from SERVICE-less documents.**
   A document that does not declare `SERVICE` cannot write `runScope` or
   the `onWrapService*` hooks (both gated), so any wrapping Environment it
   defines (one with any `WRAP_ACTIONS` hook) has the default `runScope`
   and is entered in every Service Session in its scope without being
   able to wrap the Service. The scope of a Job Environment — the Job
   Template's or an attached one — is every Service of the combined Job
   (combined `jobServices` plus every Step's `stepServices`, since a Step
   Service's Session enters the Job Environments too); a Step
   Environment's is only its Step's `stepServices`, since a Job Service's
   Session enters `jobEnvironments` alone. Whether a document declares `SERVICE` is read from
   `EnvironmentTemplate::profile()` for an attachment and `Job::extensions`
   for the Job Template. A violation is reported at the Environment
   (`EnvironmentTemplate[i] -> environment`, `JobTemplate ->
   jobEnvironments[i]`, or `JobTemplate -> steps[i] ->
   stepEnvironments[j]`), naming the document as the cause and the first
   Service in scope as the witness, with the spec's remedy:

   ```
   wrapping Environment 'QueueContainer' is defined by EnvironmentTemplate[0], which does
   not declare the SERVICE extension, so it has the default runScope (every kind of
   Session) and cannot define the onWrapService* hooks; but the combined Job places
   Service 'Cache' (JobTemplate -> jobServices[0]) in its scope, and the Service would run
   in a Session the Environment enters but cannot wrap. Declare SERVICE in
   EnvironmentTemplate[0] and either define onWrapServiceEnter, onWrapServiceRun,
   onWrapServiceReadinessCheck, and onWrapServiceExit, or declare a runScope that excludes
   SERVICE (RFC 0009, Template Schemas §1.2.2 item 3).
   ```

   For the Job Template the document reads `the Job Template`. Both
   directions the spec names are covered by the same walk: a queue's
   wrapper attachment with a Job that declares Services (or with another
   attachment that does), and a Job Template wrapper — job- or step-level
   — with an attachment that defines a Service. A document that does
   declare `SERVICE` is not subject to this rule: pass 10's
   hooks-follow-`runScope` rule already made its wrapper either define
   the four hooks or exclude `SERVICE` from `runScope`. With nothing in
   scope (no Service anywhere) a SERVICE-less wrapper is accepted exactly
   as before RFC 0009.

3. **External Services.** Each `services[k]` is instantiated with the same
   `instantiate_service` as a `jobServices[k]` entry, in Job scope, with
   `InstantiateCtx` built from the attachment's profile and
   `job_services` set to that document's `services` (so the "in scope"
   iterator is `services[..k]` — earlier Services of the same document,
   the only ones a `Service.*` reference can name; a reference to another
   document's Service, or a Job Template's reference to an external
   Service, is a template-validation error and cannot reach here).
   `<Service>.let`, `hostRequirements`, the numeric `@fmtstring` fields,
   and the carried-forward re-checks all run as for a Job Service.
4. **Attached Environments.** The carried-forward resolved-value checks
   run as for a `jobEnvironments` entry (`build_env_check_symtab` seeded
   with the document's own Services, only when the Environment's
   `runScope` excludes `SERVICE`, exactly the pass 8 scope), then
   `convert_environment_with_symtab` freezes the attachment's table into
   `resolved_symtab`, and the attachment's `Document` is pushed onto
   `environment_documents` so a runtime seeding `Service.*` for the
   Environment seeds that document's Services only.

Errors raised inside one document in steps 3–4 are attributed to it:
validation errors get the document prefixed to every path and report for
`Submission` (`EnvironmentTemplate[0] -> services[0] -> ports[0] -> port:
must be between 1 and 65535.`); format-string and expression errors get
the document name prefixed to the message (`Expression error:
EnvironmentTemplate[0]: service let binding 'share': ...`).

**Limits.** The 10-element cap on `services` is per document (pass 11)
and is not re-applied to the combined list: three documents at the cap
combine to 30 Services. `CallerLimits::max_environment_size` is applied
by `create_job` to the Job Template's Environments; an application that
wants it on attached Environments measures the returned `environments`.

**Unchanged behavior.** An attachment that defines no Services and
declares no `SERVICE` has exactly the pre-RFC-0009 behavior: its
Environment is converted with the merged parameter table (what the CLI
did by hand with `build_symbol_table` + `convert_environment_with_symtab`),
and the wrapper rule has no Service in scope.

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

`normalize_path` performs pure path normalization (resolving `.` and `..` components)
without filesystem access. This is important because:
- Templates may reference paths that don't exist yet
- The crate should be usable in environments without filesystem access
- Path mapping (for cross-platform execution) happens at session time, not job creation time
