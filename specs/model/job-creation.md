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
     List-literal range elements of every type are list items (Expression Language
     §1.3.2): a whole-field expression targets `T? | list[T]` (`string` for
     STRING/PATH, `int` for INT and CHUNK[INT], `float` for FLOAT), so `null`
     skips the element and a list flattens inline — one range element per list
     element, mixable with literal elements. An INT or CHUNK[INT] list item may be a
     `<TaskParameterStringValue>` (any string containing `{{`, kept as
     `template::IntRangeItem::FormatString`); without EXPR it must resolve to the
     string form of an integer (§3.4.1.1). A FLOAT item that resolves to a scalar
     keeps its written spelling (§7.5 rule 2); the elements of a flattened list keep
     their values only. A list literal emptied by null-skips or empty lists is
     rejected ("has no elements after resolution") without `CONTROL_FLOW`, since the
     ≥1-element rule was checked on field presence at decode; with `CONTROL_FLOW` it
     may be empty (RFC 0010 §Empty parameter spaces). Without `CONTROL_FLOW` every
     resolved list range is capped at `max_task_param_range_len` (1024); with it, the
     cap applies only to a list literal's items as written (the structure pass), not
     to resolved values (RFC 0010 §Range length). Both relaxations are decided in
     `ranges::RangeRules`. Attribute `anyOf`/`allOf` values get the list-item
     treatment in `instantiate`. String-backed FLOAT range elements are trimmed and
     must resolve to finite `f64` values.
   - Step-level let bindings (template scope; a failure is reported at
     `steps[i] -> let[j]` — see the carried-forward checks below)
3. With EXPR extension: inject `Job.Name` (before step instantiation)
   and `Step.Name` (per step) into the symbol table
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
7. Build step dependency list
8. Attach resolved symbol table to each step
9. With `CONTROL_FLOW` (RFC 0010): copy each Step's `outputVariables` onto
   `job::Step::output_variables` as `job::OutputVariable` values, decide each
   Job Environment's `enableIf` (setting `job::Environment::enabled`; a disabled
   one's fields are not checked), and bind each Step as far as the values
   concrete at job creation allow, in dependency order. This replaces the
   per-Step parts of steps 2 and 5–8 above (host requirements, ranges, step
   `let`, each Step's checks, and its symbol table); the job name and the Job
   Environments are resolved and checked as before. See [Step input binding](#step-input-binding-control_flow)
   below.

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
  `Task.*`, plus this environment's `Env.File.*` (`Unresolved`) and its
  script-level `let` bindings evaluated in.
  An environment `let` binding that fails to evaluate fails job
  creation, under the same error policy as every other evaluation this
  stage performs (see below): the bindings only evaluate when the
  context profile enables EXPR, their expressions already type-checked
  at pass 8 with everything unresolved, so a failure here comes from
  the real parameter values and would deterministically recur in every
  session that enters the environment.
- **`Steps.*`** (`CONTROL_FLOW` only; RFC 0010): both tables of a Step,
  and those of its Step Environments, also hold the `Steps.*` symbols of
  the Step's direct dependencies: the concrete value for each symbol its
  materialization groups read that is concrete, and an `Unresolved` value
  of its RFC type for every other, seeded by `add_steps_scope` exactly as
  pass 8 seeds them (see `specs/model/validation.md` § The `Steps.*`
  scope; `add_steps_scope` keeps a key the table already holds). Without
  them, every script that reads `Steps.*` would fail here with `Undefined
  variable`, so a template that passes `openjd check` would fail
  `create_job`. The template-scope table that the carried-forward
  `timeout` / `cancelation` fields resolve against gets the same symbols
  with path types only in their `Raw` forms, as at pass 8. Job
  Environments get none. The placeholders are added to clones of the
  Step's symbol table, and no `Steps.*` value goes into `resolved_symtab`:
  `attach_step_inputs` adds the ones a Step's Tasks read.

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

### Step input binding (`CONTROL_FLOW`)

RFC 0010 §Step input binding. A Step's materialization expressions — its
format strings resolved before a Session exists, plus `enableIf` — fall into
four groups, evaluated separately (`MaterializationGroup`), and in this order
when more than one can be:

| Group | Fields | Result |
|---|---|---|
| Enablement | the Step's `enableIf` | `true` (or absent): go on; `false`: `StepBinding::Skipped` |
| Task enumeration | each Task parameter's `range` (whole field or list items) and `chunks` fields | the materialized `StepParameterSpace`: the Step's Tasks are known |
| Host selection | every `hostRequirements` format string | the evaluated `HostRequirements` |
| Remaining | each Step Environment's `enableIf` and TEMPLATE-scope fields (action `timeout`, cancelation `notifyPeriodInSeconds` and format-string `mode`), and the script's | each Step Environment's `enabled`, and the carried-forward checks of the script and of every enabled Step Environment |

There is no Task graph group: `TASK_DEPENDENCIES` is not implemented.

**Group inputs** come from static analysis (`binding::StepGroups`), not from
evaluating with `Unresolved` values: a group's inputs are the `Steps.*`
symbols its fields reference, as `Steps.<Step>.<Member>.<Name>` (a read such
as `Steps.P.Output.X.name` counts as `Steps.P.Output.X`), including in a branch
that is never taken. A field that reads a step `let` name also reads that
binding's symbols, transitively, and the group *uses* the binding. The
remaining group uses every step `let` binding. `enableIf` may not read `let`.

**The core** (`binding::bind_step`) serves both `create_job` and
`advance_step_binding`. Given the Step, its earlier `PendingBinding` (none at
creation), and a `StepInputs`:

1. It builds the Step's table: the job table, `Step.Name`, and every concrete
   `Steps.*` value its groups read.
2. Enablement first. If its inputs are not all concrete, the Step is
   `PendingInputs` with every group `Waiting` and nothing else evaluated, so a
   failing range in a Step whose `enableIf` is undecided is not reported. Each
   group's `on` still lists its own inputs that are not concrete (not counting
   the enablement group's), so a report shows what is missing. `enableIf`
   resolves with target type `bool`; a `null` result is an error at
   `steps[i] -> enableIf`, and so is text around an expression that does not
   resolve to a bool spelling (`'a{{Param.X}}' resolved to 'ab', which does
   not convert to a bool.`). `false` makes the Step `Skipped`: nothing else of it
   is evaluated, including its `let` bindings and carried-forward checks, so a
   skipped Step's `let` may assume that the Step runs.
3. Then each other group that has not evaluated and whose inputs are concrete,
   in the order above. A group first evaluates the `let` bindings it uses that
   this call has not evaluated yet, in declaration order. A known group is
   kept as it is and never evaluated again. The first group that fails stops
   the Step.
   * Task enumeration resolves the parameter space with the `CONTROL_FLOW`
     range rules, validates its combination (associated ranges must have
     equal lengths, so an empty range associated with a non-empty one is an
     error and associated empty ranges give an empty space), checks that a
     `CHUNK[INT]` range in a Step that declares `outputVariables` has distinct
     values (`must have distinct values in a Step with outputVariables, so that
     each value names one Task; value 1 appears more than once.`), and checks
     the Task-count limits (below). Every error is located, as RFC 0010 §How a
     Step is bound item 3 asks ("the failure message names the field and the
     expression"). A range string that fails to parse after its expressions
     are evaluated names both, and each symbol it reads that is `null`:
     `'1-{{Steps.P.Output.V}}' resolved to '1-' ('Steps.P.Output.V' is null),
     which is not a range expression: Unexpected end of expression: '1-'`.
     A FLOAT list item whose typed resolution and text parse both fail keeps
     the typed resolution's message (its type and caret). The paths: a range or `chunks` error at
     `steps[i] -> parameterSpace -> taskParameterDefinitions[k] -> range` (or
     `-> chunks`), with the expression and caret where evaluating one failed;
     a combination error at `steps[i] -> parameterSpace`; a Task-count limit
     at `steps[i]`. A whole-field range expression that resolves to `null` (a
     conditional producer's `T?` output, say) is `Task parameter 'X' range
     must not resolve to null`; without `CONTROL_FLOW` the earlier messages
     are kept.
   * Host selection resolves `hostRequirements`; a resolution error is
     reported at `steps[i] -> hostRequirements`, with its own text naming the
     field (`hostRequirements amount min: ...`).
   * The remaining group evaluates every `let` binding, then the script's
     carried-forward checks, then each Step Environment: its `enableIf` (an
     error is reported at `steps[i] -> stepEnvironments[j] -> enableIf`), and
     its carried-forward checks if it is enabled. A disabled Environment is
     never entered, so its fields are not checked.
4. When every group is known, the parameter space and host requirements move
   into the Step's own fields, `resolved_symtab` is built from the Step's
   table (every `let` binding evaluated, no `Steps.*` value), and the Step is
   `Bound`. Otherwise it is `PendingInputs` with the groups' states; a pending
   Step keeps `parameter_space`, `host_requirements`, and `resolved_symtab`
   as `None`, so a caller that reads those fields never sees a partial Step.

Step `let` bindings are re-evaluated in each call rather than cached in
`PendingBinding`; a binding whose inputs are concrete always gives the same
value, so this is observably the RFC's "evaluated once".

**At job creation** (`instantiate_steps_with_binding`), Steps are bound in
dependency order (`validate_v2023_09::step_dependency_order`: Kahn's algorithm
with ties broken by template index), and stored in template order. Each Step's
`StepInputs` come from the Steps processed before it, for the producers its groups read: a
skipped producer contributes `null`s, and a producer whose Tasks are known (bound, or
pending with `tasks: Known`) its `TaskParam`s when the groups read its `TaskParam` or
`TaskRawParam`. Nothing else is added, because building a producer's `TaskParam` lists
walks its whole parameter space. `Output` is never concrete at creation
except as a skipped producer's `null`. A group failure is reported by the
error it holds:

* A step `let` error (whichever group's evaluation raised it) and the
  remaining group's check errors are collected per Step and reported
  together with the other Steps' and the Job Environments' errors, in
  template order, as one `ModelValidation` error. So
  `violations_across_steps_and_environments_are_reported_together` keeps
  its errors.
* A Step's or a Job Environment's `enableIf` error, a range or host
  requirement error, a repeated `CHUNK[INT]` value, and a Task-count limit
  abort `create_job` with that error alone (a Step Environment's `enableIf`
  belongs to the remaining group, so its error is collected), as an invalid parameter does — also for a Step that would
  stay pending. Because Steps are bound in dependency order, the error
  reported is the first in that order.

Whether this path is taken, and whether the `CONTROL_FLOW` range rules apply, is decided
by the template's own `extensions`, not by the context, which may enable more: a template
that does not list `CONTROL_FLOW` behaves exactly as before (RFC 0010 Technical
Requirement 1). Without `CONTROL_FLOW`, `instantiate_step` keeps today's behavior: Steps in
template order; a failed step `let` still lets that Step's carried-forward
checks run; host requirements resolve before the parameter space; and the
job-total Task-count limit is checked once, after every Step.

**Task-count limits** (RFC 0010 §Limits item 4) are checked in the Task
enumeration group, when a Step's Tasks become known: `max_step_task_count`
against the Step's Tasks (`Step 'S' has 5 Tasks, which exceeds the caller
limit of 4 Tasks per Step.`), and `max_task_count` against the Step's Tasks
plus the committed count of the other Steps whose Tasks are known (`Total
task count (11) exceeds caller limit of 10: Step 'C' has 9 Tasks, and the
Steps whose Tasks are known before it have 2.`). In `create_job` the
committed count is the running total over the Steps processed before it
whose Tasks are known (bound, or pending with `tasks: Known`); for
`advance_step_binding` the caller passes it. Without `CONTROL_FLOW`,
`max_step_task_count` is checked for every Step after it is instantiated.

#### advance_step_binding

```rust
pub fn advance_step_binding(
    job_template: &JobTemplate,
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
    job: &job::Job,
    step_index: usize,
    inputs: &StepInputs,
    committed_task_count: u64,
) -> Result<job::Step, BindingError>
```

The public wrapper over the core for a pending Step of an existing Job. It
rebuilds the job table from the parameter values and `job.name`, and the
limits and budgets from `ctx`. It returns a new Step for the caller to swap
in: more known, `Bound`, or `Skipped`; a `Bound` or `Skipped` Step, or a
call with no new inputs, comes back unchanged. On every call, `inputs` must
hold each producer in the Step's `input_producers()` whose state is known
(Tasks known, succeeded, or skipped), including producers whose values
`create_job` used and those of groups already known, because the Step's table
and its `let` bindings are rebuilt from `inputs`. A known group whose input
is missing from `inputs` is a caller error. On the first failing group it
returns `BindingError { step, group, errors }`: the Step is `FAILED` at once.
`errors` holds the group's `ModelError`, a `ModelValidation` error with a
path per field (see each group above); where evaluating an expression
failed, the message quotes it with a caret. Its `Display` is `Step '<name>' failed in its
<group> group:` followed by each error. A caller error says nothing about the
Step and does not fail it: its error is a `ModelError::Compatibility`,
`BindingError::is_caller_error()` is `true`, and its `Display` begins
`Binding Step '<name>' was refused in its <group> group:`. These
are a Step index past the Job's (with `step` `#<index>`), a template whose
Step there has another name (both in the enablement group), and a known
group's input missing from `inputs` (in that group: `the remaining group of
Step 'C' was evaluated with 'Steps.P.Output.Gpu', which the given inputs do
not hold; on each call, pass every producer in the Step's input_producers() whose Tasks are known or that has succeeded or been skipped.`).

#### StepInputs and add_producer

`StepInputs` holds the concrete `Steps.*` values a binding reads, per
producer. `add_producer(producer, state)` fills them, typed as the RFC's §The
`Steps.*` scope table types them:

* `ProducerState::Succeeded { task_outputs }` (a bound producer):
  `Output.<V>` and `RawOutput.<V>` for each declared output — the Task's
  value for a producer without a `parameterSpace`, otherwise a list with one
  element per Task in iteration order (chunking disabled), typed by the
  declared type, so zero Tasks give a typed `[]`. Each value is converted to
  the declared type with EXPR coercion. Plus its `TaskParam`s.
* `ProducerState::TasksKnown` (bound, or pending with `tasks: Known`):
  `TaskParam.<N>` and `TaskRawParam.<N>` for each Task parameter, a list
  with one element per Task from `StepParameterSpaceIterator::
  new_with_chunk_override(space, Some(1))`; a `CHUNK[INT]` parameter's
  one-value range becomes its `int`. A pending producer's parameter space is
  read from `PendingBinding.tasks`, so a consumer's Tasks can become known
  before its producer is bound.
* `ProducerState::Skipped` (a skipped producer): every symbol is `null`.

A type that is or contains `path` gets a value only in its `Raw` form (the
submission-context form, as a `path` value; a `PATH` Task parameter's
values are `path` values in the Posix format); its `Output` or `TaskParam`
is concrete (`is_concrete`) but has no value (`get` returns `None`), because
it is derived on the worker host. `add_producer` replaces a producer added
before. `TasksKnown` on a skipped producer is the same as `Skipped`, since its
`TaskParam` is concrete as `null` (and `tasks_known()` is `Some(0)`). It
refuses, as a `ModelError::Compatibility`, a state that does not match the
producer's binding, a `task_outputs` without one map per Task, a Task that
lacks a declared output, and a value that does not convert. It does not check that the producer is a
dependency of anything, because job outputs read any Step.

#### attach_step_inputs

```rust
pub fn attach_step_inputs(step: &job::Step, inputs: &StepInputs) -> Result<job::Step, ModelError>
```

A bound Step that reads `Steps.*` in its script or Step Environments (its
host-scope fields) needs those values in `resolved_symtab` before its Tasks
run; `create_job` and `advance_step_binding` put none there.
`attach_step_inputs` finds the symbols those fields read
(`collect_all_accessed_symbols`) and copies each value from `inputs`. For a
path-typed `Steps.P.Output.X` or `TaskParam.N`, `inputs` holds only the `Raw`
twin, so the twin is stored instead, as `include_raw_param_fallbacks` does
for `PATH` `Param.*`. Every path-typed `Raw` value it stores is stored as a
`string` (or list of strings, or list of lists of strings) and recorded in
`job::Step::path_inputs` with its type (`path`, `list[path]`, or
`list[list[path]]`), which the session uses to build the `path` values and
map `Output` and `TaskParam` forward. A skipped producer's `null`s are
stored as they are and are not recorded. Every `Steps.*` value in
`resolved_symtab` and every `path_inputs` record comes from this call's
`inputs`, so attaching again replaces an earlier call's (calling it again with
the same inputs gives the same Step). It errors, as a `ModelError::Compatibility`,
on a Step that is not bound and on a symbol that is not concrete in `inputs`,
naming it.

#### Reporting

`job::Step::tasks_known()` is the Task count once Tasks are known (`Some(0)`
for a skipped Step). `job::Step::binding_report()` is, for a pending Step,
each group's `GroupReport`: `Known`, or `Waiting { on, for_enablement }`
with its missing `Steps.*` symbols and whether it also waits for the
undecided `enableIf`. `job::Step::input_producers()` lists the producers
whose `Steps.*` symbols the Step reads: those its host-scope fields read (for
`attach_step_inputs`) and, for a pending Step, those any of its groups read,
known or not (`PendingBinding::producers`, which `create_job` and
`advance_step_binding` record). It is what a caller passes on each call; none
for a skipped Step.

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
