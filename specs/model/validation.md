# Validation Pipeline

The `template::validate_v2023_09` module implements a multi-pass validation pipeline that
runs after serde deserialization. It validates semantic constraints that serde can't express.
The module lives inside `template/` because validation is a template concern, and is
version-scoped to `v2023_09` because validation rules are specific to a spec revision.

## Entry Points

```rust
pub(crate) fn validate_job_template(
    jt: &JobTemplate,
    ctx: &ValidationContext,
) -> Result<(), ModelError>

pub(crate) fn validate_environment_template(
    et: &EnvironmentTemplate,
    ctx: &ValidationContext,
) -> Result<(), ModelError>
```

Both functions are crate-private — external callers use `decode_job_template` and
`decode_environment_template`, which call these internally.

Both functions compute `EffectiveLimits` and `EffectiveRules` from the context, run all
applicable passes, and return accumulated errors as `ModelError::ModelValidation`.

Before any pass, both check the `ENV_PARAMS` prerequisite (Template Schemas §1.1 item 3;
§1.2 item 3 gives an Environment Template the Job Template's prerequisites): a document that
lists `ENV_PARAMS` must also list `EXPR`. `env_params::check_expr_prerequisite` reports a
document that does not at `extensions`, "ENV_PARAMS requires EXPR; both must be listed in
the template's `extensions` (Template Schemas §1.1 item 3)." ("§1.2 item 3" for an
Environment Template), modeled on the `WRAP_ACTIONS` prerequisite (pass 10), and the
function returns with that error alone, plus pass 10's `WRAP_ACTIONS` prerequisite error when
the document also lists `WRAP_ACTIONS`, which concerns the same list. The rule is on listing
the extension, whether or not any ENV_PARAMS field is used. Every ENV_PARAMS rule in passes 8
and 11 and at job creation therefore takes `EXPR` as listed wherever `ENV_PARAMS` is: none
has a case for an ENV_PARAMS document without `EXPR` (prototype plan 6). A document that
lists only `EXPR` is unaffected, and one that uses an ENV_PARAMS field without listing
`ENV_PARAMS` is rejected by pass 11's gates as before.

## Pass Architecture

The validation pipeline is passes 5–12 of the overall decode pipeline (passes 1–4 are in
the `parse` module — see [parsing.md](parsing.md)). Passes run sequentially. Each pass
receives the template and the computed limits/rules, and appends errors to a shared
`ValidationErrors` collector. All passes run regardless of earlier errors (no
short-circuiting), so users see all problems at once. The one exception is the
`ENV_PARAMS` prerequisite (see [Entry Points](#entry-points)), checked before pass 5.

| Pass | File | Purpose |
|------|------|---------|
| 5 | `limits.rs` | Enforce numeric limits (name lengths, counts); FEATURE_BUNDLE_1 raises many limits |
| 6 | `structure.rs` | Structural validation (uniqueness, required fields, dependencies) |
| 7 | `feature_bundle_1.rs` | Gate FEATURE_BUNDLE_1 features (simple actions, endOfLine) |
| 8 | `format_strings.rs` | Validate format string variable references; adapts scopes and expression complexity based on EXPR; with CONTROL_FLOW, seeds and checks the `Steps.*` and `Loop.*` scopes, checks `enableIf`, `while`, and `until` as expression fields of type `bool`, and checks a loop's own expressions (RFC 0013) |
| 9 | `task_chunking.rs` | Gate TASK_CHUNKING features (ChunkInt parameters) |
| 10 | `wrap_actions.rs` | Gate WRAP_ACTIONS features (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit) and enforce the single-wrap-layer-per-session rule (RFC 0008) |
| 11 | `env_params.rs` | Gate ENV_PARAMS features (Environment `envParameterDefinitions`, `activation`, `hostRequirements`, and `enableIf`, the `step:` and `environment:` forms of `<StepDependency>`, the latter with its `enableIf`) and check their structural rules (Environment Parameters RFC) |
| 12 | `control_flow.rs` | Gate CONTROL_FLOW features (Step `enableIf`, `outputVariables`, `jobOutputDefinitions`, loops), require `ENV_PARAMS` and `EXPR` with it, and enforce their structural rules (RFC 0011, RFC 0012, RFC 0013) |

### Environment template pipeline

`validate_environment_template` runs the same passes with job-template-only checks
omitted (there are no steps, so passes 9's ChunkInt checks and pass 6's step/dependency
checks have nothing to walk):

- **Limits + structure** — parameter-definition count/uniqueness (its own cap,
  `max_env_template_param_count`), then `validate_single_environment` for the env body.
- **Pass 7** — `validate_feature_bundle_1_environment_template`: `endOfLine` on the
  environment's embedded files requires FEATURE_BUNDLE_1.
- **Pass 8** — `validate_format_strings_environment_template`: the environment body
  (`variables`, action `command`/`args`, embedded files — all `@fmtstring[host]`) is
  validated in **session scope** exactly like an environment inside a job template.
  `Param.*`/`RawParam.*` come from the template's own `parameterDefinitions`;
  `Session.*` and `Env.File.*` are available; with EXPR, `Job.Name` is also in scope
  (the environment runs inside some job's session at runtime) but `Step.Name` is NOT
  (an environment template is not attached to a step). Action `timeout` and
  `notifyPeriodInSeconds` are plain `@fmtstring` (job-creation stage) and validate in
  **template scope** instead — no `Session.*`/`Env.File.*`. `let` bindings require
  EXPR; with EXPR they are validated and type-checked into the symbol table. Complex
  expressions (anything beyond a bare `{{Name.Path}}` reference) require EXPR in
  variables, action commands/args, and embedded-file data. Embedded-file
  `filename` is a plain string (not `@fmtstring`) — brace syntax in it is
  literal text and no format-string validation applies.
- With ENV_PARAMS, the environment's `hostRequirements` get pass 6's structural checks
  (`validate_host_requirements_in_profile`) and pass 8 checks them against their own table
  (see [Environment hostRequirements](#environment-hostrequirements-env_params)).
- **Pass 10** — WRAP_ACTIONS gating (see below).
- **Pass 11** — ENV_PARAMS gating and the Environment's `envParameterDefinitions` (see
  below). Pass 8 also checks each format-string `default` against the template's own
  `envParameterDefinitions` (see [Environment parameter scopes](#environment-parameter-scopes-env_params)).
- **Pass 12** — `CONTROL_FLOW` applies to Job Templates only: listing it in an
  Environment Template is an error whatever the extensions. An Environment Template's
  `enableIf` is RFC 0010's, gated by `ENV_PARAMS` in pass 11.

## Pass 5: Limits Enforcement

Walks the template tree checking every name length, list count, and string length against
`EffectiveLimits`. Pure numeric checks with no extension branching.

Checks include:
- Job name length vs `max_job_name_len`
- Parameter count vs `max_param_count`
- Parameter name lengths vs `max_identifier_len`
- Step name lengths vs `max_step_name_len`, for every top-level Step, loop, and body Step
  (RFC 0013)
- Embedded file name/filename lengths
- Task parameter name lengths
- Environment name lengths vs `max_env_name_len`

## Pass 6: Structural Validation

The largest pass. Validates template structure using `EffectiveRules`. Key checks:

**Template level:**
- At least one step required (a loop counts)
- Caller limit `max_step_count` on the number of Steps, counting every top-level Step, loop,
  and body Step once (RFC 0013 §Limits item 3; the plan's A18): `steps: exceeds caller
  limit of N steps (M steps).`
- Job name non-empty, no control characters
- Extensions list non-empty if present (enforced early in pass 4)
- Description length and control character validation

**Parameter definitions:**
- Non-empty if present
- No duplicate names (case-sensitive)
- Parameter type in `rules.allowed_job_param_types`
- Type-specific validation via `validate_definition(limits, length_check)`, where `length_check` is
  `RangeExprLengthCheck::for_job_parameter(profile)`: a RANGE_EXPR `default` is length-checked in
  canonical form only when the document lists `ENV_PARAMS` (§2.10). Constraint consistency
  (`minLength` ≤ `maxLength` and the like) is checked only for STRING, PATH, INT and FLOAT (see
  [parameters.md](parameters.md#constraint-checking-vs-definition-validation))

**Environment uniqueness:**
- Names unique within `jobEnvironments`
- Names unique within each step's `stepEnvironments`
- A `stepEnvironments` name must not match any `jobEnvironments` name
- Different steps may reuse a `stepEnvironments` name (only one step's environments are active in a session at a time)

**Step validation** runs on every entry in template order, and on each loop's body Steps
right after the loop, so a body Step gets every check below at its own path
(`steps[i] -> loop -> steps[j]`):
- No duplicate step names among all top-level Steps, loops, and body Steps (RFC 0013 §Job
  Template constraint 1): `name: duplicate step name: 'X'`, at the later one
- Step and loop name non-empty, no control characters, no format string; a loop's
  `description` as a Step's
- Must have `script` or exactly one simple action field (mutually exclusive)
- Dependencies: §3.2 constraint 1 (exactly one of `dependsOn`, `step`, and `environment`;
  `required`, `enableIf`, and `parameters` only with `environment`). With ENV_PARAMS the
  messages name all three forms: "must have exactly one of 'dependsOn', 'step', or
  'environment'." for an entry with none, with ", not both 'X' and 'Y'." or ", not all
  three." appended for one with more. Without ENV_PARAMS an entry with none of the keys is
  reported as missing `dependsOn`, an entry with two names only those two ("must have
  exactly one of 'X' or 'Y', not both."), an entry with all three gets the all-forms
  message with ", not all three.", and the message for a misplaced `required`,
  `enableIf`, or `parameters` says that `environment` requires the extension. These
  located messages replace the serde "unknown field" / "missing field" errors that the
  old single-field struct produced, so for a malformed `dependsOn` entry they intentionally
  differ from the Python reference, which has no ENV_PARAMS support and reports Pydantic's
  field errors. For `dependsOn` entries and their ENV_PARAMS synonym `step` (§3.2 item 2),
  read through `StepDependency::step()` so the two are one form: no self-dependency, target
  must exist, no duplicates (a `step` and a `dependsOn` entry naming one Step are
  duplicates). The `environment:` form, and the `step` form's gate, are checked in pass 11,
  and cycle detection sees the `dependsOn` and `step` entries as one graph (the top-level
  graph and each loop body's). With loops (RFC 0013 §Job Template constraint 2,
  §`<LoopDefinition>` item 5.3), a top-level Step or loop may depend only on top-level
  entries, and a body Step only on Steps of its own body:

  | Case | Message (at `... -> dependencies[k]`) |
  |---|---|
  | A top-level Step or loop names a body Step | `dependency 'W' is a body Step of loop 'L'; a top-level Step or loop may depend only on top-level Steps and loops. Depend on loop 'L' and read its outputs instead.` |
  | A body Step names a top-level Step | `dependency 'S' is not a Step of loop 'L's body; a body Step may depend only on Steps of the same body. Add 'dependsOn: S' to loop 'L' instead: every body Step reads the Steps its loop depends on.` |
  | A body Step names its own loop | `a body Step cannot depend on its own loop 'L'.` |
  | A body Step names another loop's body Step | `dependency 'W' is a body Step of loop 'M'; a body Step may depend only on Steps of the same body.` |
- A loop's body is not empty: `steps[i] -> loop -> steps: must contain at least one
  element.` (A body entry with a `loop` key fails to parse; see
  [template-types.md](template-types.md) § StepLoop and LoopDefinition.)
- Caller limit `CallerLimits::max_step_environment_entries` (opt-in; Template Schemas §3
  item 4 sets no maximum on `dependencies` but lets an implementation constrain it): the
  number of entries with a non-null `environment` in one Step's `dependencies`, counted as
  written. A disabled entry (`enableIf` that is or resolves to `false`) and a
  `required: false` entry that names no Environment count; `dependsOn` and `step` entries
  do not. The count is literal, so this pass is the only stage that checks it: job creation
  and `check_environment_templates` do not change how many entries a Step has, and do not
  check it even when their context sets it (as for `max_step_count` and `max_env_count`), so
  a caller sets it in the limits passed to decoding. Entries are
  counted with or without ENV_PARAMS (without it, pass 11 also rejects each one). The error
  is at `steps[i] -> dependencies`: "exceeds caller limit of N 'environment' entries (M
  entries)."
- Host requirements: a Step's, and with ENV_PARAMS a `jobEnvironments` entry's or an
  Environment Template's `environment`'s (§4 item 5; each checked on its own, §3.3.3,
  through `validate_host_requirements_in_profile`): amounts/attributes validation,
  capability name patterns,
  reserved scope checks (reserved scopes: `worker`, `job`, `step`, `task`),
  standard capability value validation. Expression-free amount `min`/`max` values must
  parse as finite numbers; validation of values containing expressions is deferred until
  job creation.

  A capability `name` is `@fmtstring` (§3.3.1 / §3.3.2), and its §3.3.1.1 / §3.3.2.1
  constraints — at most 100 characters, the capability name pattern, and the reserved
  scopes — apply to the resolved name. `helpers::check_capability_name` applies them in
  three places, so the wording matches: here for a literal name, in pass 8 for a name
  that is fully static (see the resolved-value table below), and at job creation for
  the resolved name. Uniqueness within `amounts` and within `attributes` is checked
  here between literal names, in pass 8 for any pair that includes a fully static name,
  and at job creation between the resolved names. The standard-capability value checks
  and the single-valued rule below key off the name, so they run for a literal name
  here, for a fully static name in pass 8, and for every other name at job creation.

  Two deferrals behave differently, so they are worth stating separately. An
  `attributes[].anyOf` / `.allOf` element that is a format string skips the
  `<AttributeCapabilityValue>` pattern, length and standard-value checks here, and job
  creation resumes all three (see [job-creation.md](job-creation.md)). The single-valued
  standard-attribute rule (`attr.worker.os.family` / `attr.worker.cpu.arch` `allOf` may
  have at most one element, Template Schemas §3.3.2.1) fires at decode when the list has **more than one element
  that certainly contributes exactly one resolved element**: literals, and multi-segment
  format strings, which always concatenate to a single string (Expression Language
  §1.3.2). Only a whole-field single-expression element can null-skip or list-flatten,
  making its resolved count unknowable here. Two or more certain elements violate the
  rule under every possible resolution; lists with at most one defer the count to job
  creation, which re-checks after resolution. An
  amount `min`/`max`
  that is a format string skips the non-negative, positive and `min <= max` bound checks
  here, and job creation re-applies all three on the resolved value
  (`check_resolved_amount_bounds`). The same holds for `chunks.defaultTaskCount` and
  `chunks.targetRuntimeSeconds`: decode reads `as_i64()`, which is `None` for a format
  string, and job creation rejects a resolved value below the §3.4.1.5 minimum rather than
  clamping it.

  Only the first failing group is reported on the job-creation path, for both attributes and
  amounts, because the caller collects through `?`. Decode accumulates every violation.

  **Empty and discarded requirements (ENV_PARAMS, Template Schemas §3.3).** In a document
  that lists ENV_PARAMS (`EffectiveRules::env_params_enabled`), constraint 1 does not
  apply: `hostRequirements: {}`, or empty `amounts` and `attributes` lists, are valid on a
  Step, a Job Environment, and an Environment Template alike. Literal empties stay errors:
  an amount with neither `min` nor `max` ("must have at least one of min or max."), and a
  literal `anyOf: []` or `allOf: []` ("must not be empty."). A requirement that may resolve
  to nothing may be discarded at job creation, and constraints 3 and 4 apply to the
  requirements left (§3.3), so the duplicate-name check here compares only requirements
  that are certainly kept: an amount with a literal bound, and an attribute with a list
  `anyOf` or `allOf` (`AmountRequirement::may_be_discarded`,
  `AttributeRequirement::may_be_discarded`). Pass 8 also compares a requirement that has a bound
  or an expression field that is certainly present: fully static and not `null`, or of a
  static type, after coercion to the field's target type, with no `nulltype` member
  (`certainly_present`; `{{Param.F}}` for a FLOAT `F`), and job creation compares the rest after discarding. This follows §7.6 rule 5: an
  error is reported early exactly when it holds whatever the unknown values resolve to
  (conformance JB-138). Without ENV_PARAMS nothing
  is discarded, so every literal duplicate is reported.

  **`anyOf` / `allOf` as one expression (ENV_PARAMS, §3.3.2).** Each field is a
  `template::AttributeValues`: `List` for the list form, or `Expression` when the value is
  a string. Here a string is rejected without ENV_PARAMS ("must be a list of values, not a
  string. ..."), and with it unless it is exactly one `{{ }}` expression with no other
  text (`structure::is_one_expression`; "... A list has no string form, so the expression
  cannot be joined with other text"). Pass 8 checks the expression itself (see the
  resolved-value table). The list-form checks above (length, element pattern, the
  single-valued `allOf` rule, the standard values) apply only to the list form.
- Parameter space: ≤16 task parameters, no duplicate names, type allowed,
  range validation per type, combination expression validation
- Script actions: command non-empty, length limits, `Task.File.*` references
  must match embedded file names
- Embedded files: no duplicate names, type must be `TEXT`, valid identifier names,
  data required; `filename` must be a single safe path component — non-empty, no
  path separators (`/` or `\`), no null characters, and not `.` or `..`
- SimpleAction fields (`bash`/`python`/`cmd`/`powershell`/`node`, §8; only
  when FEATURE_BUNDLE_1 is enabled — otherwise pass 7 rejects the field):
  the field is desugared (`SimpleActionKind::desugar`) and the resulting
  `StepScript` runs through the *same* script-action and embedded-file
  checks above, so the sugar and its expansion can never disagree on
  what is valid. Plus one check the desugared form cannot express: an
  authored `args: []` is rejected (the desugared list always holds the
  generated file reference). See
  [Reporting on desugared forms](#reporting-on-desugared-forms) for how
  the paths are reported.

**Cycle detection:**
- Kahn's algorithm on the name-keyed dependency graph of the top-level entries (Steps and
  loops), reported at the root (`step dependencies contain a cycle.`), and on each loop
  body's graph, reported at `steps[i] -> loop -> steps` (`the loop body's step
  dependencies contain a cycle.`)

**Combination expression validation:**
- Character allowlist, balanced parentheses, tokenization
- All referenced parameters must exist and appear exactly once
- All defined parameters must appear in the expression

## Pass 7: FEATURE_BUNDLE_1 Gating

Validates or rejects features gated behind `FEATURE_BUNDLE_1`:

- **SimpleAction fields** (bash, python, cmd, powershell, node): Rejected without extension;
  mutually exclusive with `script` when enabled. This pass only gates the
  field's presence — its contents are validated by passes 6 and 8 on the
  desugared form (see Pass 8 § Step Scripts and SimpleActions).
- **`endOfLine` on embedded files**: Rejected without extension; must be `LF`, `CRLF`, or
  `AUTO` when enabled

## Pass 8: Format String Validation

The most complex pass. Validates that all format string references resolve to defined
variables by building scope-appropriate symbol tables.

### Symbol Table Construction

Four scope levels, each building on the previous:

1. **Param symtab** — `Param.*` and `RawParam.*` from job parameter definitions.
   PATH types excluded from `Param.*` at template scope (host-only).
   `RawParam.*` for PATH types is STRING.

2. **Template scope** — For job name, host requirements, parameter space ranges.
   Uses param symtab without PATH parameters.

3. **Session scope** — For environment scripts/variables. Adds `Session.WorkingDirectory`,
   `Session.HasPathMappingRules`, `Session.PathMappingRulesFile`, `Env.File.*`, and
   `Env.Param.*` / `Env.RawParam.*` for the environment's own `envParameterDefinitions`
   (ENV_PARAMS; see [Environment parameter scopes](#environment-parameter-scopes-env_params)).
   With EXPR: adds `Job.Name` in all environments and `Step.Name` in step
   environments only; with ENV_PARAMS (which requires EXPR), also `Job.EnvNames` (see
   below). Used both for environments inside a job template and for
   standalone environment templates (which never get `Step.Name`).

Scope selection follows the spec's `@fmtstring` stage annotations, not the
container: `timeout` and `notifyPeriodInSeconds` are plain `@fmtstring`
(resolved at job creation, before any session exists), so they validate in
template scope — with `Job.Name`/`Step.Name`/step `let` bindings where
applicable, but no `Session.*`, no `Env.File.*`, no `Task.*`, and the
template function library (no `apply_path_mapping`) — even though they sit
on actions whose `command`/`args` are `@fmtstring[host]` and validate in
session/task scope. One helper (`validate_action_timing_fs`, wrapped per
environment by `validate_env_actions_timing_fs`, which adds the
unresolved `WrappedAction.*` scope on the RFC 0008 wrap hooks) applies
the `timeout` / `notifyPeriodInSeconds` / deferred `mode` constraints for
every action; job creation calls the same helpers with the parameters
bound, so a parameter-dependent violation (`timeout: "{{ Param.T }}"`
with `T = 0`) fails at submission at the same path with the same
message (see `specs/model/job-creation.md`).

4. **Task scope** — For step scripts. Adds `Task.Param.*`, `Task.RawParam.*`,
   `Task.File.*`. With EXPR: adds `Job.Name`, `Step.Name`, `Env.File.*` from
   step and job environments.

**`Job.EnvNames`** (ENV_PARAMS, Template Schemas §7.3.1; ExprLang §1.2.2) is
`unresolved[list[string]]` in every table that has `Job.Name` when the document lists
ENV_PARAMS (and so EXPR), except an Environment parameter `default`'s: the template-scope
tables of host requirements, parameter-space ranges, Step `let` bindings, `environment:`
entry values and `enableIf`, and action timing fields, and every session and task scope, in
Job Templates and Environment Templates alike (`add_job_symbols`). It is not in the Job
Template `name`'s table or an Environment parameter `default`'s, so a reference there is
"Undefined variable" (§7.3.1). In a `default`, whose table has `Job.Name`
(`build_env_param_default_symtab`), the message suggests `Job.Name`. Without ENV_PARAMS it
is not defined at all; with EXPR alone the message also suggests `Job.Name`.

### Environment parameter scopes (ENV_PARAMS)

Four places involve Environment parameters, each with its own scope. Pass 8 checks the
format-string `default`s and entry values only when the document lists ENV_PARAMS;
without it, pass 11 rejects those fields outright. The `Env.Param.*` / `Env.RawParam.*`
symbols are added either way (see the first item).

- **`Env.Param.<name>` and `Env.RawParam.<name>`** (§4.5, §7.3.1) are added to the
  session-scope table of an environment, for that environment's own definitions only, by
  `add_env_param_symbols`. Every field validated with that table sees them: `variables`,
  the script's `let`, every action's `command`/`args` (wrap hooks validate against a clone
  of the table, so a wrapping environment's hooks see its own parameters and no other
  environment's), and embedded-file `data`. They are absent from every other table except
  the one for the environment's own `hostRequirements` (non-PATH parameters only; see
  [Environment hostRequirements](#environment-hostrequirements-env_params)), so a
  reference from a Step, from another environment, from a job-creation field such as
  `timeout`, or to a name the environment does not declare is the ordinary "Undefined
  variable" error. `Env.Param.<name>` has the parameter type's expression type
  and `Env.RawParam.<name>` the same except `string` for PATH and `list[string]` for
  LIST[PATH], the same map `build_param_symtab` uses for `Param.*` / `RawParam.*`. Step
  Environments cannot declare parameters (pass 11), so in practice only Job Environments
  and Environment Templates have these symbols. A duplicated parameter name is typed by its
  first definition, as the entry checks type it. The symbols are added whether or not the
  document lists ENV_PARAMS: without it, pass 11's one gate error on `envParameterDefinitions`
  is the whole report, and the environment's own references to those parameters do not add
  "Undefined variable" errors.
- **A format-string `default`** (§4.5 item 1) is checked by `validate_env_param_defaults`
  against the template-scope table of the *same document's* top-level
  `parameterDefinitions`: `RawParam.*` for every Job Parameter and `Param.*` for the
  non-PATH ones, and `Job.Name` as `unresolved[string]` (§7.3.2), with no `Job.EnvNames`,
  `Step.Name`, `Session.*`, or `Env.*`. For a Job
  Environment that is the Job Template's parameters; for an Environment Template, its own.
- **A format-string `environment:` entry value** (§3.2) is checked by
  `validate_env_entry_values` against the Step's template-scope table
  (`step_template_symtab`): `RawParam.*`, non-PATH `Param.*`, `Job.Name`,
  `Job.EnvNames`, `Step.Name`, and the step-level `let` names. `Task.*`, `Session.*`, and
  `Env.*` are not in it. A step-level `let` is itself template scope, so it cannot carry a
  PATH `Param.*` into an entry.
- **An `environment:` entry's `enableIf`** (§3.2 item 3.2, constraint 9) is an expression
  field of type `bool` (§7.6), checked by `validate_enable_if` with the expression-field
  checker (see [template-types.md](template-types.md#expression-fields-76)) against the same
  table, for every well-formed `environment:` entry that has one. A scalar other than a boolean
  gets "the integer scalar 1 does not fit type bool: an
  expression field of type bool takes a boolean scalar or a string holding one expression
  (Template Schemas §7.6)."; a present `null` gets "must not be null: …" with the same
  tail. An expression must parse ("Invalid expression: <error>" with the expression and
  caret; a value containing `{{` adds "An expression field takes an expression without
  '{{ }}' (Template Schemas §7.6)." after the error message), its names must be in the table,
  and its static type must fit `bool`: "has type T, which does not fit type bool; an
  expression field converts no type except int to float (Template Schemas §7.6).", or for
  `nulltype` "has type nulltype, which does not fit type bool (Template Schemas §7.6).". A
  `bool?` is accepted; its `null` is a job-creation error (§7.6 rule 2). No conversion
  applies: a `string` or `int` expression is an error whatever its value.
- **An Environment's own `enableIf`** (§4 item 6): see
  [Environment enableIf](#environment-enableif-env_params).

A value that is a single `{{ }}` expression is checked with
the parameter's expression type as its target type, so a static type that can never
convert (`{{ [1] }}` for an INT) is an error at template validation, even when every Step
supplies the parameter (prototype plan A17). Static evaluation folds constant
sub-expressions, as it does for every other EXPR-typed field (an action `timeout` of
`{{ 'abc' }}` fails the same way), so a constant expression whose value cannot convert
(`{{ 'abc' }}` for an INT) is also an error here. A value check that depends on what an
expression reads (a `Param.*` string that does not parse as an INT, a constraint) is left
to job creation. The target type is known for a `default` and for an entry that names a
`jobEnvironments` entry. An entry for an external Environment is checked with no target
type, since its definitions are not known until the Job is created. So is a value of an
entry whose own `enableIf` is not known to be `true` (§7.6 rule 5; see [Known enableIf
values](#known-enableif-values-env_params)), since a disabled entry's values are not checked
(§3.2 item 3.2), and a value for a `jobEnvironments` entry that may be disabled for the Step,
since a Step's values for an Environment disabled for it are not checked against their type
(§3.2.2): one with an `exclusiveGroup`, which may be superseded, and one whose `activation` is
not `ALWAYS` and whose own `enableIf` is not known to be `true`, unless that `enableIf` reads a
literal value of the entry's that does not convert, so it is not evaluated and the Environment
is certainly not disabled (§3.2.2 item 2, `enable_if_reads_unconvertible_literal`; see
[Pass 11](#pass-11-env_params-gating)).

### Environment enableIf (ENV_PARAMS)

An Environment's own `enableIf` (§4 item 6) is an expression field of type `bool` (§7.6),
checked by `validate_env_enable_if` with the expression-field checker and the messages of an
entry's `enableIf` (above), when the document lists ENV_PARAMS (pass 11 reports it
otherwise). Its table depends on where the Environment is:

- **A `jobEnvironments` entry or an Environment Template's `environment`** (§7.3.2):
  exactly the table of its `hostRequirements`, `build_env_host_requirements_symtab` (see
  [Environment hostRequirements](#environment-hostrequirements-env_params)): the
  document's `RawParam.*` and non-PATH `Param.*`, `Job.Name`, `Job.EnvNames`, and its own
  `Env.RawParam.*` of any type (PATH as `string`, LIST[PATH] as `list[string]`) and
  `Env.Param.*` of types other than PATH and LIST[PATH].
- **A `stepEnvironments` entry** (§7.3.2): the Step's template-scope table
  (`step_template_symtab`), the table of an entry's `enableIf`: `RawParam.*`, non-PATH
  `Param.*`, `Job.Name`, `Job.EnvNames`, `Step.Name`, and the Step's `let` names.

Anything else (constraint 3: `Task.*`, `Session.*`, `Env.File.*`, another Environment's
parameters, a PATH `Env.Param.*`, or, for a Job Environment, `Step.Name` or a `let` name) is
"Undefined variable". An Environment whose `enableIf` is known to be `false` is still
validated in full, its definitions included. It is disabled for every Step (§3.2.2), so the
checks of the Steps' values for it (constraint 6, and the target type of a format-string
value above) do not apply to it; for an Environment whose `enableIf` is not known they are
undetermined at template validation and left to job creation (pass 11, below; §7.6 rule 5).
Constraint 7 does not depend on an Environment's own `enableIf`: a Step it applies to must
supply its parameters without a `default` whatever the `enableIf` is (§3.2 constraint 7). For
the single-layer rule an Environment whose `enableIf` is known to be `true` counts as entered,
one known to be `false` as not entered, and any other defers to job creation (see
[Pass 10](#pass-10-wrap_actions-gating)).

#### Known enableIf values (ENV_PARAMS)

Every check before job creation that reads whether an entry is enabled or an Environment is
disabled reads one value per `enableIf`, so they all agree: `format_strings::KnownEnableIfs`,
built once per Job Template by `known_enable_ifs` after pass 7 and passed to passes 8, 10 and
11 (and `known_environment_template_enable_if` for an Environment Template's `environment`).
An `enableIf` is *known* (Template Schemas §7.6 rule 5, §3.2.2 item 5) when it is absent
(`true`), a boolean scalar, or an expression that evaluates to a concrete `bool` in its
template-validation scope (above) with every value unknown there bound to `unresolved[T]` of
its type: the Job Parameters, `Job.*`, `Step.Name`, and `Env.Param.*` / `Env.RawParam.*`
(`expr_field::known_value` / `known_bool`). The Expression Language's rules for unknown
operands decide the result (Expression Language §2.1.6 "Unknown operands": `U and false` is
`false`, `U or true` is `true`, `not U` is `U`; and "Conditional Expressions with Unknown
Conditions": `c if U else c` is `c`). So a constant expression such as `"true"` or
`"false"` is known here, as are `"Param.F and false"` and `"Param.F or true"`, and
`"Param.F"` is not. An expression that fails to parse or evaluate is not known (its error is
reported elsewhere). An entry's or a Step Environment's scope includes the Step's `let`
bindings, evaluated with the same unknown values; a binding that fails is `unresolved`. These
values decide only which errors template validation reports; job creation evaluates every
`enableIf` again. `Environment::literal_enabled` and `StepDependency::literal_enabled` return
only the literal (`None` for any expression) and are not what these checks read.

An Environment whose `activation` is `ALWAYS` cannot be disabled (§4 item 4, §3.2.2 item
6). Pass 11 rejects one whose `enableIf` is known to be `false`, the scalar or an expression
such as `"false"`, on a `jobEnvironments` entry or an Environment Template's `environment`,
at its `enableIf`: "Environment 'Lic' has activation ALWAYS, so it cannot be disabled, but
its enableIf is false for every Step (Template Schemas §4 item 4)." An expression that is not
known is not rejected here: job creation reports each Step it is `false` for. Since such an Environment is
entered by every Step's Sessions or the Job is rejected, every check above and below that
depends on whether a Step enters it (constraints 6 and 7, the target type of a format-string
value, the single-layer rule) treats it as entered whatever its `enableIf`.

### Environment hostRequirements (ENV_PARAMS)

An Environment's `hostRequirements` (§7.3.2) get the Step's checks
(`validate_host_requirements_fs`, shared with a Step's) against their own table,
`build_env_host_requirements_symtab`: the template scope of the *same document's*
`parameterDefinitions` (`RawParam.*` for every Job Parameter, PATH ones included as
`string`, plan A27, and `Param.*` for the non-PATH ones), `Job.Name`, `Job.EnvNames`, and
the environment's own `Env.RawParam.*` of every type (PATH as `string`, LIST[PATH] as
`list[string]`, the unmapped value) and `Env.Param.*` of types other than PATH and
LIST[PATH] (a path-mapped value needs a host). `Step.Name`, `let` names, `Session.*`, `Env.File.*`, and another
environment's parameters are not in it, so a reference is "Undefined variable". The check
runs for `jobEnvironments` entries and an Environment Template's `environment` when the
document lists ENV_PARAMS; pass 11 reports the field otherwise, and on a
`stepEnvironments` entry. A Step's own `hostRequirements` keep their table, which has no
`Env.*`.

### Step Scripts and SimpleActions

One helper (`validate_step_script_format_strings`) validates a `StepScript`
in full — script-level `let` bindings (into the task symtab), the
EXPR-gated complex-expression rejection, the `onRun` action's
`command`/`args` in task scope with the opt-in `max_resolved_arg_len`
bound, `timeout`/`cancelation` in template scope, each embedded file's
`data` in task scope with the opt-in `max_resolved_data_len` bound, and
the comprehension-variable rules (against the step's and the script's
`let` names) on `command`, `args`, and embedded `data`.

An authored `script` runs through it once. Each SimpleAction field the
step sets (§8; only when FEATURE_BUNDLE_1 is enabled and the step has no
`script` — pass 7 rejects both other cases, and validating sugar against
a task symtab whose `Task.File.*` came from an authored `script` would
report the generated file reference as undefined) is desugared and runs
through the **same helper**, so a `bash:` step is validated exactly as
its `script:` equivalent would be — the design goal that job creation
"re-runs exactly the checks pass 8 applies" holds for sugar too. Each
field gets a clone of the task symtab, so its `let` bindings do not leak
into a sibling field. Consequences worth naming, because they changed
behaviour (all of these previously passed `check`): a SimpleAction `let`
without EXPR is now rejected (it was silently ignored); undefined
references in `script`/`args`/`timeout`/`notifyPeriodInSeconds` are
checked at decode; a static `script` over an opted-in
`max_resolved_data_len` fails `check` (it used to surface only at
`create_job`); and the pass-6 structural checks now reach the sugar —
`script: ""`, `args: []`, control characters in `args[k]`, literal
`timeout: 0` / `notifyPeriodInSeconds: 700`, and a dangling
`{{Task.File.Other}}` in `args`.

#### Reporting on desugared forms

Every diagnostic must name a node that exists in the template the author
wrote — never `steps[0] -> script -> embeddedFiles[0] -> data` for a step
that has no `script`. Validation of a desugared SimpleAction therefore runs
into a scratch `ValidationErrors` rooted at the synthesized `script` node
(path `[]`), and `ValidationErrors::extend_remapped` re-roots each error
through `SimpleActionKind::remap_desugared_path` before it joins the real
collection (messages and structured detail are carried unchanged):

| Desugared path (relative to `script`)          | Reported path (relative to `steps[i] -> <kind>`) |
|------------------------------------------------|--------------------------------------------------|
| `let[j]`                                       | `let[j]`                                         |
| `actions -> onRun -> args`                     | `args`                                           |
| `actions -> onRun -> args[k]`, `k ≥ offset`    | `args[k − offset]`                               |
| `actions -> onRun -> timeout`                  | `timeout`                                        |
| `actions -> onRun -> cancelation`              | `cancelation`                                    |
| `embeddedFiles[0] -> data`                     | `script`                                         |
| anything synthesized — `command`, the `onRun` node itself, `args[k]` for `k < offset`, the generated file's `name`/`filename` | the `<kind>` field itself |

`offset` is `SimpleActionKind::synthetic_arg_count()`: the interpreter
prefix (`/C` for `cmd`, `-File` for `powershell`, none otherwise) plus the
generated `{{Task.File.<name>}}` reference — `1` for `bash`/`python`/`node`,
`2` for `cmd`/`powershell`. Pass 6 and job creation use the same remap, so
a violation is reported at the same authored path whichever stage finds
it (a dangling `Task.File.*` reference is still reported by both pass 6 —
at the field, as the script node — and pass 8 — at the arg — exactly as
for an authored `script`).

`SimpleAction.script` is a `FormatString` (the spec types it `<DataString>`,
`@fmtstring[host]`), so malformed `{{ ... }}` syntax fails at parse time
like every other format-string field; it no longer survives to job
creation.

### The `Steps.*` Scope and `enableIf` (CONTROL_FLOW, RFC 0011)

With `CONTROL_FLOW`, pass 8 implements items 2 to 6 of RFC 0011 §Validation, "At
template validation", and the scope and typing part of item 9 (see § Job output values
below). The shared code is `validate_v2023_09/steps_scope.rs`, which job
creation also uses (see `specs/model/job-creation.md`).

**Seeding.** `add_control_flow_scope(symtab, jt, reader, scope)` seeds the symbols a field
read by `reader` may see (`StepsReader`: a Step at a `StepLocation`, one of a loop's own
fields, a job output, or nothing); job creation calls it with the Step's `StepLocation`,
for a top-level Step or a body Step instance. For a top-level Step `S` it adds, for each Step or loop `P` that
`S` lists in its `dependencies`, that is not `S`, and whose name is an
`<Identifier>`, the symbols `Steps.P.Output.<V>` and `Steps.P.RawOutput.<V>` for each of
`P`'s output variables and, when `P` has a `parameterSpace`, `Steps.P.TaskParam.<N>` and
`Steps.P.TaskRawParam.<N>` for each Task parameter. Each is `ExprValue::unresolved(T)`,
typed by the table of RFC 0011 §The `Steps.*` scope:

| Producer `P` | `Output.<V>` | `TaskParam.<N>` |
|---|---|---|
| no `parameterSpace`, no `enableIf` | `T` | not seeded |
| no `parameterSpace`, `enableIf` | `T?` | not seeded |
| `parameterSpace`, no `enableIf` | `list[T]` | `list[U]` |
| `parameterSpace`, `enableIf` | `list[T]?` | `list[U]?` |

`T` is the output type's expression type (`LIST[INT]` gives `list[int]`, so a multi-Task
producer's `LIST[INT]` output is `list[list[int]]`). `U` is `int`, `float`, `string`, or
`path` for an `INT`, `FLOAT`, `STRING`, or `PATH` Task parameter, and `int` for
`CHUNK[INT]`, one element per Task. Any `enableIf`, including a literal `true`, makes the
type optional, because RFC 0011's table is keyed on the field being present. The `Raw`
forms have the same types. In a TEMPLATE-scope field, an `Output` or `TaskParam` whose
type is or contains `path` (`PATH`, `LIST[PATH]` outputs; `PATH` Task parameters) is not
seeded; its `Raw` form is.

**Where each field family is seeded.** A Step's fields are seeded with its own direct
dependencies' symbols:

| Field | Scope |
|---|---|
| `enableIf` | TEMPLATE |
| step `let` | TEMPLATE |
| `hostRequirements` (names, `min`/`max`, `anyOf`/`allOf`) | TEMPLATE |
| `parameterSpace` Task parameter `range`s and `chunks` fields | TEMPLATE |
| Step Environment `enableIf`, `timeout`, `notifyPeriodInSeconds`, deferred `mode` | TEMPLATE |
| Step Environment `variables`, script `let`, action `command`/`args`, embedded `data` | host |
| script `timeout`, `notifyPeriodInSeconds`, deferred `mode`, and a SimpleAction's `timeout` / `cancelation` | TEMPLATE |
| script `let`, `onRun` `command`/`args`, embedded `data`, and a SimpleAction's `script` / `args` / `let` | host |

The job name and every field of `jobEnvironments` (including their `enableIf`) are never
seeded. Environment Templates are not either; pass 12 rejects `CONTROL_FLOW` in them.

**Reference messages.** Before a field is evaluated, `control_flow_ref_errors` scans the
symbols it reads (`FormatString::accessed_symbols`, or a `let` binding's
`ParsedExpression::accessed_symbols`) for the `Steps`, `Loop`, and `RawLoop` roots (the
last two are covered in § Loops below), and gives each unavailable
reference its own message instead of EXPR's `Undefined variable`. A field with any such
reference is reported with those messages only and is not evaluated. Because the scan is
static, a reference in a branch that is never taken is caught too. In a `let` binding the
message is prefixed `Invalid expression in let binding '<name>': ` and the binding is
bound `unresolved(ANY)`, so later uses of it are not reported as undefined variables
(as for any failed binding, an attribute read from it still fails, as `Cannot access
attribute 'foo' on any`). The checks, in order, with
the first that applies reported:

| Case | Message |
|---|---|
| A field that allows no `Steps.*` (job name, `jobEnvironments` other than an Environment parameter `default`, which reports an undefined variable because its table has no `Steps.*` or `Loop.*`) | `'<sym>': Steps.* symbols are not available in this field. Only a Step's enableIf, let, hostRequirements, parameterSpace, stepEnvironments, and script, a loop's enableIf, while, until, initial, next, and output values, and a job output's value, may read them.` |
| Fewer than four parts (`Steps.P.Output`) | `'<sym>' is not a complete Steps.* reference; expected Steps.<StepName>.Output.<Name>, .RawOutput.<Name>, .TaskParam.<Name>, or .TaskRawParam.<Name>.` |
| No such Step | `'<sym>': there is no Step named 'P'.` |
| The Step's name is not an `<Identifier>` | `'<sym>': Step 'P' cannot be referenced through Steps.*, because its name is not an <Identifier>.` |
| Self-reference (a Step's field only) | `'<sym>': a Step cannot read its own Steps.* symbols.` |
| Not a direct dependency (a transitive one is not enough; a Step's field only) | `'<sym>': Step 'P' is not a direct dependency of Step 'S'. Add 'dependsOn: P' to its dependencies to read Steps.P.*.` |
| The reader may not read `P` for a loop reason (RFC 0013; see § Loops) | one of the loop messages in § Loops |
| Unknown member | `'<sym>': 'X' is not a member of Steps.P; expected Output, RawOutput, TaskParam, TaskRawParam.` |
| No such output | `'<sym>': Step 'P' has no output variable 'V'.` (`loop 'P'` for a loop) |
| `TaskParam` or `TaskRawParam` of a loop | `'<sym>': 'P' is a loop, which has no Task parameters. Declare a loop output and read Steps.P.Output.<Name> instead.` |
| `TaskParam` of a Step without a `parameterSpace` | `'<sym>': Step 'P' has no parameterSpace, so it has no Task parameters.` |
| No such Task parameter | `'<sym>': Step 'P' has no Task parameter 'N'.` |
| Path-typed `Output`/`TaskParam` in a TEMPLATE-scope field | `'Steps.P.Output.V' has a path type, so it is available only in fields resolved on the worker host. Use Steps.P.RawOutput.V here.` (or `TaskParam` and `TaskRawParam`) |

`<sym>` is the symbol as the expression reads it, which may include a property after the
name (`Steps.P.RawOutput.F.name`); the path-type message names only the four-part symbol. Several references in one field are reported in sorted
symbol order. Without `CONTROL_FLOW` none of this applies, and a `Steps.*` reference is an
ordinary `Undefined variable`.

A format-string item of an `INT`, `CHUNK[INT]` or `FLOAT` range list (a string
containing `{{`, `template::IntOrFormatString::FormatString` or
`template::FloatRangeItem::FormatString`) is checked at `range[k]` with its list-item
target (the "task param INT/CHUNK[INT]/FLOAT range list elements" row of the table in
[Spec-Mandated Resolved-Value Constraints](#spec-mandated-resolved-value-constraints)),
and with the same `Steps.*` scope as the Step's other TEMPLATE-scope fields.

Pass 8 does not evaluate a whole-field expression `range` of a `STRING`, `PATH`, or
`FLOAT` Task parameter: it is checked only when job creation resolves it, a gap that
predates `CONTROL_FLOW` (a `range: "{{Param.Nope}}"` passes `openjd check`). For those
fields pass 8 runs the reference scan alone, so the `Steps.*` scope rules hold there, but
their expressions are not type-checked.

**`enableIf`.** A Step's `enableIf` is checked against a table of its own: `Param.*`
(minus `PATH` types), `RawParam.*`, `Job.Name`, `Step.Name`, and the TEMPLATE-scope
`Steps.*` of the Step's direct dependencies, with no `let` names, `Task.*`, or
`Session.*`. A Step Environment's `enableIf` uses the Step's template-scope table, `let`
names included (RFC 0011 `<Environment>` item 1), with those `Steps.*` symbols; a Job
Environment's is RFC 0010's (see [Environment enableIf](#environment-enableif-env_params)),
with the reference scan rejecting any `Steps.*`. Without `CONTROL_FLOW` pass 8 does not look
at a Step's `enableIf`; pass 12 rejects the field.

A Step's `enableIf` (and a loop's `enableIf`, `while`, and `until`, below) is an
expression field of target type `bool` (RFC 0010's Template Schemas §7.6;
`validate_condition` calls `expr_field::validate` under the template-scope profile). The
reference scan runs first, as for a format string, so a `Steps.*`, `Loop.*`, or
`RawLoop.*` reference the field may not read gets the messages of the tables above and
the field is not evaluated. Then:

| Value | Result |
|---|---|
| `true` or `false` | accepted, with no check |
| another scalar (`1`, `1.5`) | `the integer scalar 1 does not fit type bool: an expression field of type bool takes a boolean scalar or a string holding one expression (Template Schemas §7.6).` (`the float scalar 1.5 ...`) |
| `null` | `must not be null: an expression field of type bool takes a boolean scalar or a string holding one expression (Template Schemas §7.6).` |
| a string that does not parse | `Invalid expression: <parse error>` with the expression and a caret. When the value contains `{{`, the summary is followed by `An expression field takes an expression without '{{ }}' (Template Schemas §7.6).`, so `'{{Param.Run}}'` reads `Invalid expression: Set literals are not supported. An expression field takes an expression without '{{ }}' (Template Schemas §7.6).` |
| a name that is not available | `Invalid expression: Undefined variable: 'run'.` with the expression and a caret |
| static type `bool`, `bool?`, or `unresolved[bool]` | accepted; a `bool?`'s `null` fails when it is evaluated |
| static type `nulltype` (`'null'`) | `has type nulltype, which does not fit type bool (Template Schemas §7.6).` |
| any other static type | `has type string, which does not fit type bool; an expression field converts no type except int to float (Template Schemas §7.6).` (or `int`, and so on) |

A type message is followed by the expression on a line of its own
(`expr_field::with_source`). No string converts to `bool`: `'Param.Mode'` for a STRING
parameter is a type error whatever its value, so a condition over a string is written as
a comparison (`"Param.Mode == 'on'"`), and the string `'true'` is the expression `true`,
which is accepted.

An Environment's `enableIf` is RFC 0010's expression field (see [Environment
enableIf](#environment-enableif-env_params)); RFC 0011 leaves it to `ENV_PARAMS`.

**Entry scope.** An `environment:` entry's values and `enableIf` (RFC 0011
`<Environment>` item 2) may not read `Steps.*`, `Loop.*`, or `RawLoop.*`, directly or
through a step `let` binding, so a Step's Environment selection can be worked out at job
creation (`format_strings::EntryScope`). Each such read is reported at the field, before
any other check of it: `'Steps.P.Output.N': an environment entry may not read Steps.*,
Loop.*, or RawLoop.* (RFC 0011 <Environment> item 2): which Environments a Step's Sessions
enter, and its Environment parameter values, depend only on values known when the Job is
created.`, or, for a binding that reads one, `'B' is a let binding that reads Steps.*,
Loop.*, or RawLoop.*, which an environment entry may not read, directly or through a let
binding (RFC 0011 <Environment> item 2).`

**Job output values.** Each `jobOutputDefinitions[k] -> value` is checked against a table
of its own (`validate_job_output_values`): `Param.*` (minus `PATH` and `LIST[PATH]`
types), `RawParam.*`, `Job.Name`, and, through `add_job_output_steps_scope`, the
TEMPLATE-scope `Steps.*` symbols of every Step whose name is an `<Identifier>`, with no
dependency required. So a `PATH` or `LIST[PATH]` `Output` or `TaskParam` is available only
through `RawOutput` or `TaskRawParam`, and gets the path-type message above. The
reference scan runs with the job output reader, which skips the self-reference and
direct-dependency checks. `Step.Name`, `Task.*`, `Session.*`, and `let` names are not in
the table, so EXPR reports them as undefined variables (a `PATH` `Param.*` often with a
`Did you mean: RawParam.X` hint, EXPR's closest-name suggestion, which can name another
symbol instead). The target type is the declared type's expression type
made optional (`INT` gives `int?`), because a `null` result means the output has no value:
a static non-`null` value of another type fails EXPR's coercion (`Cannot coerce list[int]
to int?`), and an unresolved `T?`, such as a conditional producer's output, is accepted.
A value with text around an expression resolves to a string, which is converted to the
declared type when it is evaluated; a literal value, or one that is fully static, is
converted here, and fails with `must convert to int: Cannot convert 'abc' to int: invalid
digit found in string.`. One that is not static is checked on its type alone: a string
converts to `int`, `float`, `bool`, `path`, `range_expr`, and `string`, so for a
`LIST[...]` type it fails with `resolves to text, which must convert to list[int]: Cannot
coerce string to list[int].`. A `STRING` value needs no conversion.

### Loops (CONTROL_FLOW, RFC 0013)

Pass 8 implements items 7 (the format-string form), 8, 9, and 10 of RFC 0013 §Validation,
"At template validation". Every body Step's fields are validated exactly as a top-level
Step's, with the reader `StepsReader::Step` at its `StepLocation`, so the scope rules and
seeding below follow from `add_control_flow_scope`.

**`Steps.*` in and around a loop** (RFC 0013 §`Steps.*` in and around a loop). The
producers a reader sees:

| Reader | Sees `Steps.*` of |
|---|---|
| A top-level Step | the top-level Steps and loops it depends on |
| A body Step of loop `L` | the body Steps it depends on (same iteration), and the entries `L` depends on |
| `L`'s `enableIf`, `while`, each `initial` | the entries `L` depends on |
| `L`'s `until`, each `next`, each output `value` | the entries `L` depends on, and every body Step of `L` |
| `L`'s `maxIterations` | nothing |
| A job output | every top-level Step and loop, with no dependency required |

A loop's symbols are `Steps.L.Output.<V>` and `Steps.L.RawOutput.<V>` for each loop
output, typed by RFC 0013's table, with no per-Task list:

| Loop `L` | `LAST` output | `APPEND` output |
|---|---|---|
| `until`, no `enableIf` | `T` | `list[E]` |
| `while`, no `enableIf` | `T?` | `list[E]` |
| `enableIf` | `T?` | `list[E]?` |

A loop output whose `collect` is missing or `ONCE` (pass 12 rejects both) is typed as
`LAST`. In a TEMPLATE-scope field a path-typed loop `Output` is not seeded, only its
`RawOutput`, as for a Step. The messages for a reference a loop rule forbids:

| Case | Message |
|---|---|
| A body Step read outside its loop (top-level Step, job output, another loop or its body) | `'<sym>': Step 'A' is a body Step of loop 'L', and is available only within that loop. Read a loop output instead, as Steps.L.Output.<Name>.` |
| A body Step reads a top-level entry its loop does not depend on | `'<sym>': Step 'P' is not a dependency of loop 'L'. A body Step reads the Steps its loop depends on: add 'dependsOn: P' to the dependencies of loop 'L' to read Steps.P.*.` |
| A body Step reads its own loop | `'<sym>': Step 'B' is in the body of loop 'L', which cannot read the loop's own outputs: they are available only after the loop ends.` |
| A loop's field reads the loop itself | `'<sym>': a loop cannot read its own Steps.* symbols; they are available only after the loop ends.` |
| A loop's field reads a top-level entry it does not depend on | `'<sym>': Step 'P' is not a direct dependency of loop 'L'. Add 'dependsOn: P' to its dependencies to read Steps.P.*.` |
| `enableIf`, `while`, or `initial` reads a body Step | `'<sym>': Step 'A' is a body Step of loop 'L', which the loop's <field> cannot read: it is evaluated before an iteration runs. The loop's until, next, and output values may read its body Steps.` |
| `maxIterations` reads `Steps.*` | `'<sym>': Steps.* symbols are not available in a loop's maxIterations, which is known at Job creation: it may read only Param.*, RawParam.*, and Job.Name.` |

**The `Loop.*` scope** (RFC 0013 §The `Loop.*` scope). For a body Step of loop `L`
(every field it has: `enableIf`, `let`, `hostRequirements`, `parameterSpace` ranges and
`chunks`, `stepEnvironments`, and `script`), and for `L`'s `while`, `until`, `next`, and
output `value`s, `add_control_flow_scope` seeds `Loop.Iteration` (`int`) and, for each
variable `V` of `L`, `Loop.V` and `RawLoop.V`, both of the variable's expression type
(`PATH` is `path`, as for `RawOutput`). In a TEMPLATE-scope field a path-typed `Loop.V` is
not seeded, only `RawLoop.V`; every one of `L`'s own expressions is TEMPLATE scope. A
variable named `Iteration` (pass 11 rejects it) is not seeded. The reference messages:

| Case | Message |
|---|---|
| Outside any loop's body and conditions | `'<sym>': Loop.* and RawLoop.* symbols are available only in the body Steps of a loop and in the loop's while, until, next, and output values.` |
| `L`'s `enableIf` or `initial` | `'<sym>': Loop.* and RawLoop.* symbols are not available in a loop's <field>, which is evaluated before the loop's first iteration.` |
| `L`'s `maxIterations` | `'<sym>': Loop.* and RawLoop.* symbols are not available in a loop's maxIterations, which is known at Job creation: it may read only Param.*, RawParam.*, and Job.Name.` |
| `Loop` or `RawLoop` alone | `'<sym>' is not a complete reference; expected Loop.<VarName>, RawLoop.<VarName>, or Loop.Iteration.` |
| `RawLoop.Iteration` | `'<sym>': Loop.Iteration has no RawLoop form; use Loop.Iteration.` |
| Not a variable of `L` | `'<sym>': loop 'L' has no variable 'V'.`, followed by ` 'V' is a variable of loop 'M', which is available only in that loop.` when another loop declares it |
| A path-typed `Loop.V` in a TEMPLATE-scope field | `'Loop.V' has a path type, so it is available only in fields resolved on the worker host. Use RawLoop.V here.` |

**A loop's own expressions** (`validate_loop_expressions`), each with its own table of
`Param.*` (minus `PATH` types), `RawParam.*`, and `Job.Name`, plus:

| Field | Path | Also reads | Target type |
|---|---|---|---|
| `enableIf` | `steps[i] -> enableIf` | `Step.Name` (the loop's name), the loop's dependencies | `bool`, an expression field, as a Step's `enableIf` |
| `maxIterations` (a format string) | `steps[i] -> loop -> maxIterations` | nothing | `int`, 1 to 1,000 when static (`maxIterations must be from 1 to 1000.`) |
| each `initial` | `steps[i] -> loop -> variables[k] -> initial` | the loop's dependencies | the variable's type |
| `while` | `steps[i] -> loop -> while` | the loop's dependencies, `Loop.*` | `bool`, an expression field |
| `until` | `steps[i] -> loop -> until` | the loop's dependencies, its body Steps, `Loop.*` | `bool`, an expression field |
| each `next` | `steps[i] -> loop -> variables[k] -> next` | as `until` | the variable's type |
| each output `value` | `steps[i] -> outputVariables[k] -> value` | as `until` | the output's type, or under `APPEND` its element type |

None of them reads `let` names, `Task.*`, or `Session.*`. `enableIf`, `while`, and `until`
are checked as a Step's `enableIf` is (above), and `maxIterations`, `initial`, `next`, and
the output `value`s are format strings. The variable and output targets
are not made optional (`ResolvedConstraint::LoopValue`), because a `null` result is an
error: a static `null` fails with `Cannot coerce nulltype to int`. As everywhere in
EXPR, an unresolved `T?`, such as a conditional body Step's output, is accepted where `T`
is required and checked when concrete (RFC 0011 Design Choice 5). A literal `initial`,
`next`, or `value`, or one that is fully static, is converted here as a job output's is
(`must convert to int: Cannot convert 'abc' to int: invalid digit found in string.`).

`TASK_DEPENDENCIES` is not an extension openjd-rs supports, so RFC 0013's rules for it
(§Composition with other extensions) are not checked.

### Let Binding Validation

Let bindings are validated with these rules:
- Non-empty if present, ≤50 bindings
- Each binding has `=` separator
- Name: non-empty, starts with lowercase/underscore, alphanumeric+underscore
- No duplicate names
- No shadowing of enclosing scope names
- No self-references (checked via regex on non-string-literal portions)
- Expression parsed and type-checked; result type added to symtab for subsequent bindings
- On error, the binding is added as `unresolved(ANY)` to the symbol table to prevent
  cascading type errors in subsequent bindings that reference it. (The `unresolved(ANY)`
  type is from the `openjd-expr` type system — see `specs/expr/type-system.md`.)
- An expression error is reported at `<scope> -> let[j]` as
  `Invalid expression in let binding '<name>': <error>`, the caret aligned to the
  full `name = expr` binding string.

The per-binding parse/evaluate/report step (`evaluate_let_binding`) is shared
with job creation, which re-evaluates every `let` block with the parameters
bound (`check_carried_forward_let_bindings`, without the structural rules
above) — so a value-dependent failure reads identically at both stages,
including the `unresolved(ANY)` continuation. See
`specs/model/job-creation.md`.

### Function Libraries

Two libraries control available functions in expressions:
- **`template_lib`** — Template-scope expressions. Built from a profile with
  `HostContext::None` (no host functions registered at all).
- **`host_lib`** — Task/session-scope expressions. Built from a profile with
  `HostContext::Unresolved` so `apply_path_mapping` type-checks against its
  signature (a stub returning `Unresolved(path)`) without real rules being
  available at validation time.

Both libraries are obtained from
`openjd_expr::FunctionLibrary::for_profile(&profile)`. The model's
`SpecificationProfile::to_expr_profile(host_context)` helper produces the
right `ExprProfile` from a model profile.

### Path Format

Every evaluation this pass performs — format-string expressions and
`let` bindings alike — runs under `PathFormat::Posix`. Template
validation happens outside host context, where the model keeps all
paths POSIX (see `preprocess_job_parameters` in
`specs/model/job-creation.md`, which explains why paths stay POSIX
until template evaluation on the host); only `openjd-sessions`
evaluates under
`PathFormat::host()`. This keeps pass 8 consistent with job creation's
re-checks (which read POSIX-format values out of their check symbol
tables) and makes validation outcomes independent of the OS running
them — a template accepted by `check` on Linux is accepted on Windows
and vice versa.

### Spec-Mandated Resolved-Value Constraints

Pass 8 uses the values that static evaluation can resolve: for fields
whose spec constraints apply to the *resolved* value — "after the format
string has been resolved" in the spec's wording — the
`openjd_expr::StaticResolution` returned by `validate_expressions` is
checked against a per-field `ResolvedConstraint`. This makes template
validation the earliest of the spec's three processing stages (Template
Schemas §7.4: template validation, job creation, task execution on the
worker host) to catch a violation that is already knowable, instead of
deferring it to job submission or the worker. Two stages of checking:

1. **Lower bound** — `min_resolved_string_len` holds for every possible
   run-time resolution (unresolved segments contribute 0), so a bound
   past the field's limit is a certain violation and fails `check`
   without knowing the unresolved parts.
2. **Full value check** — when `resolved_value` is present the field is
   fully static, and the same check job creation or the worker would run
   on the resolved value runs here, with matching error messages.

| Field | Target type | Bound | Fully-static check |
|---|---|---|---|
| job `name` (Template Schemas §1.1.1) | `string` | ≤ `max_job_name_len` (128, 512 with FB1) | non-empty (§1.1.1 min 1); no Cc control characters |
| attribute `anyOf`/`allOf` written as one expression (ENV_PARAMS, Template Schemas §3.3.2) | `list[string]?` | — | `null` omits the field (valid); a list must have 1 to 50 elements, each passing `validate_attribute_capability_value`, and at most one for the `allOf` of a single-valued standard attribute (§3.3.2.1). Also, whether or not it is static, a static type of `list[int]` or `list[float]` is rejected (see below) |
| attribute `anyOf`/`allOf` values (Template Schemas §3.3.2.2) | `string? \| list[string]` | when certainly a string: ≤ 100; for a standard capability, ≤ longest allowed value | `validate_attribute_capability_value` (charset / allowed set) on the value, or on each element when a list flattens; `null` skips the element |
| amount / attribute `name` (Template Schemas §3.3.1.1 / §3.3.2.1) | `string` | ≤ 100 | `helpers::check_capability_name` (length, pattern, reserved scope); then, across the requirements, uniqueness of the known names, and for a standard attribute name the standard-value and single-valued `allOf` checks on literal values |
| task param STRING/PATH range elements (Template Schemas §3.4.2) | `string? \| list[string]` | when certainly a string: ≤ 1024 | ≤ 1024 chars per element (a list flattens); PATH elements additionally must be non-empty (see below); `null` skips the element |
| task param INT/CHUNK[INT]/FLOAT range list elements (Template Schemas §3.4.1.1, §3.4.1.2) | with EXPR `int? \| list[int]`, FLOAT `float? \| int \| list[float]`; without EXPR none, the element resolves to a string (`TaskParameterType::range_list_item_target`) | none | none beyond static typing against the target, which rejects a value that can never coerce (a `bool`, a `list[string]` with a non-numeric member such as `["a"]`). There is no fully-static value check: an element that resolves to a string that does not parse as a number, such as `"a{{ 1 }}"`, passes `check` and fails at job creation even when `check` could know it. `null` skips the element. The reference implementation does no static typing here, so its `check` accepts these templates and they fail when the job runs |
| environment variable values (Template Schemas §4.4.2) | `string` | ≤ `max_env_var_value_len` (2048) | (length is the whole constraint) |
| action `timeout` (FB1 `<posintstring>`, Template Schemas §5) | `int?` | soft cap: 100 chars | coerced integer > 0; `null` = unset |
| `notifyPeriodInSeconds` (Template Schemas §5.3.2, FB1) | `int?` | soft cap: 100 chars | coerced integer > 0, ≤ 600; `null` = unset |
| cancelation `mode` (FB1 deferred, Template Schemas §5.3) | `string?` | ≤ 21 chars (longest valid value) | `TERMINATE` / `NOTIFY_THEN_TERMINATE`; `null` = cancelation unset |
| chunks `defaultTaskCount` (TASK_CHUNKING, Template Schemas §3.4.1.5) | `int` | soft cap: 100 chars | coerced integer ≥ 1 |
| chunks `targetRuntimeSeconds` (TASK_CHUNKING, Template Schemas §3.4.1.5) | `int?` | soft cap: 100 chars | coerced integer ≥ 0; `null` = unset |
| amount `min` / `max` (FB1 float strings, Template Schemas §3.3.1) | `float?` | soft cap: 100 chars | finite float, ≥ 0 / > 0; `null` = unset |
| action `command` (Template Schemas §5.1) — **opt-in** `CallerLimits::max_resolved_arg_len` | none (resolution is `resolve_string_with`) | ≤ cap, unconditionally (everything renders inline into one string) | (length is the whole constraint) |
| action `args[*]` (Template Schemas §5.2) — **opt-in** `CallerLimits::max_resolved_arg_len` | none (resolution is `resolve_with` with no target) | when certainly a string: ≤ cap | ≤ cap per final argv entry (a list flattens into one entry per element; `null` skips) |
| embedded file `data` (Template Schemas §6.1.2) — **opt-in** `CallerLimits::max_resolved_data_len` | none (resolution is `resolve_string_with`) | ≤ cap, unconditionally | (length is the whole constraint) |

The three opt-in rows are **caller policy, not spec constraints**: §5.1,
§5.2 and §6.1.2 deliberately set no maximum (the OS imposes its own on
process arguments), so the caps default to `None` and impose nothing.
When a caller sets one, this pass fails early on the lower bound — and,
unlike the spec-mandated rows (whose literal cases the raw-text passes
cover), also checks a purely-literal field's exact raw length, since no
other pass length-checks args or data. Job creation re-runs the same
checks with the job parameters bound to real values (see the
Resolved-Value Checks on Carried-Forward Fields section of
`specs/model/job-creation.md`), and the
session runtime enforces the cap on the final resolved values. The
`openjd` CLI sets `max_resolved_arg_len` to an opinionated 32K-character
default on every platform (see `specs/cli/`).

Separately from per-field constraints, pass 8 evaluates every
format-string expression — and every `let` binding, which is the same
expression machinery — under the caller's **evaluation budgets**
(`CallerLimits::max_eval_memory_bytes` / `max_eval_operations`, defaults:
the Expression Language spec's 100 MB / 10 M). These bound each segment's
evaluation here and — when the caller mirrors them into `SessionLimits` —
at run time; they are the spec's own lever against expression blowups
like `'A' * 10000000`. A lowered budget fails at this pass first, as an
ordinary `Failed to parse interpolation expression` error at the field
path. Job creation applies the same budgets to every evaluation it
performs (see `specs/model/job-creation.md`).

Numeric fields have no exact length maximum — leading zeros are legal and
surrounding whitespace is tolerated in string forms — so they use a soft
100-character cap (`MAX_RESOLVED_NUMERIC_LEN`): no reasonable numeric
value is longer, and an i64 needs at most 20 characters.

#### Target types: the general rule

Expression Language **§1.3.2 "Evaluation Within Template Schemas"** gives
every format-string field a target type for its whole-field expressions,
derived from the schema context:

- a required field of type `T` targets `T`;
- an optional field targets `T?` — a `null` result means "field omitted";
- a list item targets `T? | list[T]` — `null` skips the item, a list
  flattens inline (the spec's worked example is `args`; task parameter
  range list elements and attribute values are list items under the
  same rule, except that INT, CHUNK[INT] and FLOAT range elements of a
  template without EXPR resolve to strings, see
  `specs/model/job-creation.md`);
- a format string with surrounding text always concatenates to a string,
  with each expression evaluated unconstrained.

Every constrained field follows the rule (for
`timeout`/`notifyPeriodInSeconds`/`mode` the Template Schemas doc also
mandates the target explicitly), and **the later processing stages
resolve each field with the same target validation checks it with** —
job name and range elements in `create_job`, attribute values and amounts
in `instantiate`, environment variable values and the action numeric
fields in `openjd-sessions`. A field's validation-time target must
always equal its resolution-time target, or validation rejects values
resolution accepts (or vice versa).

Consequences of the target types worth naming:

- For the scalar `string` fields, there is no `list[T] → string` or
  `null → string` conversion, so a whole-field list or `null` is an
  error. The job name additionally may not resolve to an empty string
  (§1.1.1 minimum length 1; checked statically here, and on the resolved
  value at job creation) and may not contain Cc control characters.
  The control-character rule is enforced three ways: the raw-text pass
  rejects a control character in the name's *literal* text (literal
  runs appear verbatim in every resolution — expression source text is
  exempt, since it never appears in a resolved value); this pass checks
  the resolved value when the name is fully static; and job creation
  re-checks the resolved name, catching a control character introduced
  by an interpolated value. Environment variable values have **no**
  minimum — §4.4.2's minimum length is 0 characters, so an empty
  resolution is legal. Null (and lists) *inside* surrounding text remain
  ordinary interpolation and render their display form.
- For the list-item fields (range elements, attribute values), a
  whole-field `null` skips the element and a list flattens inline —
  `range: ["first", "{{ RawParam.Paths }}", "last"]` yields one element
  per path between the literals. The per-element constraints apply to
  each flattened element. Because null-skips can empty a list whose
  non-emptiness was checked on field presence at decode, job creation
  re-checks after resolution ("... has no anyOf values after resolution", naming the
  attribute and the Step or Environment; "has no elements after resolution" for a range). A
  range that is entirely one expression can also use the whole-field
  `<ListExpressionString>` form (Expression Language §1.3.12 "Task
  Parameter Range Field Extensions").
- **Implementation choice: a list of numbers is rejected at template validation.** An
  `anyOf` or `allOf` written as one expression (ENV_PARAMS) whose static type, before
  coercion to the `list[string]?` target, is a list, or a choice of lists, whose element
  types are only `int`, `float`, and `nulltype` with at least one number
  (`list[int]`, `list[float] | list[int]`, `list[int] | list[nulltype]`, `list[int?]`), and
  that is not itself optional (no `nulltype` member at the top), is rejected in pass 8
  (`never_valid_attribute_list`): "resolves to a list[int], whose
  elements can never be attribute values: ...". It is the only error reported for the
  field, so a static list does not also get one error per element. The target type would convert each element to its string form,
  which starts with a digit or a sign and so never satisfies `<AttributeCapabilityValue>`
  (§3.3.2.2), and an empty list (the `nulltype` element of a possibly empty list) is an
  error too, so no value of that type is valid. A static `[]` alone is left to the
  empty-list message. The
  specification checks the elements only at job creation; openjd-rs reports the error
  earlier because it holds for every value, as §7.6 rule 5 allows. An expression that
  may also resolve to `null`, such as `list[int]?`, is accepted, since `null` omits the
  field. A list of `bool` is accepted: its string forms are identifiers.
- The length bound for list-item fields only applies when the resolution
  is certainly a string (`StaticResolution::resolved_type` is `string`):
  a list-valued resolution distributes its characters across elements,
  so the display-form length says nothing about any single element.
- **Empty range elements are rejected for PATH only.** §3.4.2 sets a
  minimum length of 1 on `<TaskParameterStringValue>` (which the range
  lists of every type share; an empty INT or FLOAT element fails anyway,
  because it does not parse as a number), but the reference
  implementation enforces it only for PATH — its rationale being that an empty string is not a valid
  path on any OS — and accepts empty STRING elements end to end
  (`TaskParameterStringValueAsJob` is explicitly `min_length=0`). We
  match that for compatibility. PATH elements are rejected at every
  stage where they become knowable: a literal `""` at raw-text
  validation (structure), a static resolution here, and an interpolated
  value at job creation (`resolve_string_range`). STRING elements are
  never rejected for emptiness.

One further deliberate exclusion: **literals**. `validate_fs` skips
literal format strings; the raw-text passes (structure/limits) already
check those, and for a literal the raw text and resolved value coincide.

## Pass 9: TASK_CHUNKING Gating

Validates or rejects features gated behind `TASK_CHUNKING`:

- `ChunkInt` task parameters rejected without extension
- With extension: `defaultTaskCount` ≥ 1, `targetRuntimeSeconds` ≥ 0
- Only one `ChunkInt` parameter per step
- `ChunkInt` parameter must not appear inside parentheses in the combination expression
  (must not be in an associative combination)

## Pass 10: WRAP_ACTIONS Gating

Validates or rejects features gated behind `WRAP_ACTIONS` (RFC 0008):

- **Wrap hooks** (`onWrapEnvEnter`, `onWrapTaskRun`, `onWrapEnvExit`): Rejected on any
  environment when the extension is not declared.
- **EXPR prerequisite**: `WRAP_ACTIONS` requires `EXPR` to also be declared (the wrap
  mechanism forwards inner-action bytes through the EXPR function library). Declaring
  `WRAP_ACTIONS` without `EXPR` is an error.
- **All-or-nothing rule**: an environment that defines any one wrap hook must define all
  three. Defining a partial set is an error.
- **Single-wrap-layer rule**: at most one environment reachable in a session may define
  wrap hooks. A session's environment stack is its step's Environment set: the
  `jobEnvironments` entries with `activation: ALWAYS`, those with `BY_DEFAULT` the step does
  not decline, the `ON_REQUEST`
  ones the step requests (ENV_PARAMS, Template Schemas §4 item 4, §3.2 item 3.2), and the step's own
  `stepEnvironments`. So this is enforced per step: for every step, the count of
  wrap-defining envs among those must be ≤ 1. Multiple `ALWAYS` or `BY_DEFAULT` wrap envs in
  `jobEnvironments` that every step certainly enters are reported once at the
  `jobEnvironments` path (reachable from every session); a `BY_DEFAULT` one is certainly
  entered by a step that has no entry for it or one whose `enableIf` is known to be `true`
  (one known to be `false` declines it, and one that is not known may; see [Known enableIf
  values](#known-enableif-values-env_params)). A step that adds its own
  wrap env on top is reported at that step's
  `stepEnvironments` path; a step whose own `stepEnvironments` add no wrap env (or that
  has none) but whose requested `ON_REQUEST` wrap envs make more than one is reported at
  its `dependencies` path; and a step without `stepEnvironments` whose `ALWAYS` and
  `BY_DEFAULT` wrap envs alone make more than one, when another step declines one so the
  `jobEnvironments` report does not apply, is reported at the step's path. A step with `stepEnvironments` that add no wrap env and requests
  none, under two such wrap envs, is also reported at its `stepEnvironments` path, beside
  the `jobEnvironments` error, as before ENV_PARAMS. A
  step requests an Environment here only through an entry whose `enableIf` is known to be
  `true` (absent, the boolean scalar, or an expression such as `"true"`, §7.6 rule 5): one
  whose `enableIf` is not known is checked at job creation, over the
  step's resolved set and the attached Environments (see
  [job-creation.md](job-creation.md#the-submission-stage), prototype plan R8). In the same
  way (ENV_PARAMS, §4, after item 10) a wrapping Environment counts only when its own
  `enableIf` is known to be `true`: one known to be `false` is not entered, and any other is
  decided at job creation, which checks the final set. A wrapping
  Job Environment with an `exclusiveGroup` (§4 item 7) does not count, since an attached
  Environment of its group may supersede it (see [Pass 11](#pass-11-env_params-gating)). An
  `ALWAYS` wrapping Environment counts whatever its `enableIf`, since it cannot be
  disabled (§3.2.2 item 6). So a
  violation is reported here only among Environments certainly entered, and it is one
  whatever the expressions resolve to. The rule for every set-dependent check made before
  job creation is to fail only when the failure is certain at that stage; with two certainly
  entered wrapping Environments and a third whose `enableIf` is not known, the template is
  rejected here, since no Job could be created from it (Template Schemas §7.6 rule 5 and the
  paragraph after §4 item 10; spec `5535230`). Two
  `ON_REQUEST` wrap envs requested by different steps are valid.

The single-layer rule runs only in the job-template path. An environment template defines
one environment, so the rule is trivially satisfied for an isolated env template;
`check_environment_templates` checks it for each step whose sessions enter a wrapping
attached environment, over the environments the step certainly enters; job creation
checks it again over each step's set with the attached environment templates; and
the session runtime rejects a second wrap environment at enter time for environments
composed any other way.

## Pass 11: ENV_PARAMS Gating

Validates or rejects the features of the Environment Parameters RFC (`ENV_PARAMS`, draft,
`rfcs/0010-environment-parameters.md` in openjd-specifications), in
`validate_env_params_job_template` and `validate_env_params_environment_template`.

Without the extension:

- `envParameterDefinitions` on any environment is reported at its path:
  "envParameterDefinitions on an Environment requires the ENV_PARAMS extension."
- An `environment:` dependency entry is reported at the entry: "the 'environment' form of
  a dependency requires the ENV_PARAMS extension."
- A `step:` dependency entry is reported at the entry: "the 'step' form of a dependency
  requires the ENV_PARAMS extension." Its Step checks (pass 6) still run.
- `activation` on any environment is reported at its path: "activation on an Environment
  requires the ENV_PARAMS extension."
- `hostRequirements` on any environment is reported at its path: "hostRequirements on an
  Environment requires the ENV_PARAMS extension."
- `enableIf` on any environment is reported at its path (see below).
- `exclusiveGroup` on any environment is reported at its path: "exclusiveGroup on an
  Environment requires the ENV_PARAMS extension."

With it, `activation` (§4 item 4, and the paragraph after item 10) is rejected on a
`stepEnvironments` entry ("is not permitted on a stepEnvironments entry, which only its own
Step's Sessions enter."); an unrecognized
value (constraint 1, case-sensitive: `always` and `by_default` are rejected; plan A24) is a
decode error naming the three values. `hostRequirements` (§4, after item 10) is rejected on a
`stepEnvironments` entry ("is not permitted on a stepEnvironments entry, whose Step has
hostRequirements of its own."); on a `jobEnvironments` entry or an Environment Template's `environment` its §3.3
checks are passes 6 and 8's.

`exclusiveGroup` (§4 item 7, and the paragraph after item 10) is rejected on a
`stepEnvironments` entry ("is not permitted on a stepEnvironments entry, which only its own
Step's Sessions enter."). On a `jobEnvironments` entry or an Environment Template's
`environment`, a value that is not an `<Identifier>` (§7.1), or is longer than
`max_identifier_len`, is reported at its path ("'Software Provider' is not a valid
exclusiveGroup: it must match [A-Za-z_][A-Za-z0-9_]* and be at most 64 characters (Template
Schemas §7.1)."), and so is one on an Environment whose `activation` is `ALWAYS` (constraint 1:
"Environment 'A' has activation ALWAYS, which cannot be disabled, so it must not have an
exclusiveGroup (Template Schemas §4 item 7 constraint 1)."). Both can be reported for one
value.

Template validation does not know the Environment Templates the Job will be submitted with,
and an attached Environment of a grouped Job Environment's group comes first in the Job's
Environment order (§1.1 item 7) and may supersede it, which disables it for the Step
(§3.2.2). So a Job Environment with an `exclusiveGroup` is never known here to be entered
(§4, after item 10; JT-189), and every check below that applies only to an Environment known to be
entered leaves it to job creation: the single-layer rule (pass 10) does not count it, and
neither constraint 6 nor pass 8's typing of a format-string value is applied to an entry's
values for it. Constraints 5 and 7 still apply: constraint 7 does not depend on the group
(§3.2 constraint 7), so a Step it applies to must supply its parameters without a `default`
even where it may be superseded. `check_environment_templates` knows the attachments, and applies the group bullet to
them (see [job-creation.md](job-creation.md#check_environment_templates)).

`enableIf` on any Environment (§4 item 6) needs ENV_PARAMS in the document, in
every position, a `stepEnvironments` entry included; without it the field is reported at its
path: "enableIf on an Environment requires the ENV_PARAMS extension.". Its value is checked
in pass 8
(see [Environment enableIf](#environment-enableif-env_params)).

With it, for each environment's `envParameterDefinitions` (§4 item 3, §4.5):

- Not permitted on a `stepEnvironments` entry (§4, after item 10). A `jobEnvironments` entry
  and an Environment Template's `environment` may declare parameters.
- 1–50 entries (constraints 1–2; the cap is fixed and FEATURE_BUNDLE_1 does not raise
  it), names unique within the list (constraint 3), and each name at most
  `max_identifier_len`. A name may equal a Job Parameter's or another environment's.
- The type: any Job Parameter type available with EXPR, which the document lists (§4.5),
  so every type that decodes is allowed and there is no type check here. Type names are
  case-insensitive, as with EXPR for a Job Parameter; the decode-time case check in
  `parse.rs` does not walk environment `envParameterDefinitions`, since a document without
  EXPR that has them is rejected (by the prerequisite, or without ENV_PARAMS by the gate
  above).
- `userInterface`, `objectType`, and `dataFlow` are permitted (§4.5), with the
  shape and constraints of the Job Parameter of the same type, since the definition is
  decoded as one. They are informational: nothing in job creation or the sessions runtime
  reads them, and an Environment parameter's `dataFlow` does not add to the Job's.
- The definition's own checks run through `JobParameterDefinition::validate_definition`
  (for example `minLength` > `maxLength` for a STRING, or a `userInterface` control the type
  does not have), with `RangeExprLengthCheck::AsWrittenAndCanonical`, so a RANGE_EXPR's lengths check its canonical form too
  (§4.5 item 4).
- An explicit `default: null` is no default (decode drops it), as for a Job Parameter.
- A literal `default` is checked against the type and the constraints (§4.5 item 2), even
  when every Step supplies the parameter. The check decodes the literal with the decoder of
  the Job Parameter type's `default` field (so a STRING
  `default` must be a string and an INT one an integer or `<intstring>`), coerces it with
  `coerce_json_to_job_parameter_type` on the decoder's canonical form (so the trimming
  `FlexInt` / `FlexFloat` do applies, and `" 3 "` is a valid INT literal here as it is for a
  Job Parameter `default`), and calls `EnvironmentParameterDefinition::check_value_constraints`:
  `check_constraints` with `AsWrittenAndCanonical`, then the length limit of §4.5 item 5 (a
  STRING or PATH value, and each element of a LIST[STRING] or LIST[PATH] one, has at most 1024
  characters, counted as characters). The same check applies to a literal value of an
  `environment:` entry. A PATH
  value of any form, a relative literal `default` included, is used as given at every stage
  (§4.5 item 3 at spec `af53020`). An
  object or `null`, at any depth, is rejected; so is a list element containing `{{`
  (plan A14: list-literal elements are literals).
- A format-string `default` of a list type must be a single `{{ }}` expression (§4.5
  item 1). Its references are checked in pass 8.

For each `environment:` entry of a Step (§3.2), with constraint 1 already checked in
pass 6:

- The name is an `<EnvironmentName>` (§4.1): non-empty, at most `max_env_name_len`
  characters, no Cc characters, with the messages used for an Environment's `name`.
  This is checked for every entry with an `environment` key.
- Constraint 2: it must not name a `stepEnvironments` entry of the same Step.
- Constraint 3: no two `environment:` entries of one list name the same Environment.
- Constraint 4: `parameters`, if given, has 1–50 entries, and each key is an
  `<Identifier>` of at most `max_identifier_len` characters.
- Constraint 8: `required: false` is rejected on an entry that names a
  `jobEnvironments` entry.
- Constraint 9 (`enableIf`, §3.2 item 3.2): an expression field of type `bool` (§7.6), whose
  value pass 8 checks. A boolean, number, string, or `null` decodes, except an integer above
  `i64::MAX` that fits `u64`; that and a list or a map are decode errors naming `enableIf`.
  Constraint 1's "only with `environment`" is checked for `enableIf` in pass 6, with
  `required` and `parameters`.
- Literal forms no parameter type accepts (plan A4): an object or `null` anywhere; a list
  that mixes lists and other values; a list nested beyond
  `LIST[LIST[INT]]`; and a nested list with an element the `LIST[LIST[INT]]` `default`
  decoder (`FlexInt`) rejects, so an integer, an integer-valued float, and an integer string
  (trimmed) are accepted.
- For an entry that names a `jobEnvironments` entry and whose `enableIf` is known to be
  `true` ([Known enableIf values](#known-enableif-values-env_params); a disabled entry's
  parameters are not checked, §3.2 item 3.2; an entry whose `enableIf` is not known may be
  disabled, so job creation checks it if it resolves to `true`, §3.2 checked-at paragraph,
  §7.6 rule 5; plan A28, decided 2026-11-03):
  constraint 5 (every key names a
  declared parameter; an environment that declares none rejects an entry with any, and a
  `required: false` entry cannot name a Job Environment, constraint 8, so the exception for
  an attachment that declares none, §3.2.1 item 1, does not arise here); and, only for an
  Environment without an `exclusiveGroup` that is known not to be disabled for the Step
  (its `activation` is `ALWAYS`, or its own `enableIf` is known to be `true`, or that
  `enableIf` reads a literal of the entry's that does not convert, below), constraint 6
  against literal values, by the same check as a literal `default`, and the rule that a
  format-string value for a list-typed parameter must be a single `{{ }}` expression (a list
  type has no string form, so another value cannot convert). For an Environment whose
  `enableIf` is known to be `false` these value checks do not apply (§3.2.2), and for one
  whose `enableIf` is not known a violation is not certain here (§7.6 rule 5), so job
  creation makes them for the Steps it does not disable (plan 4 A39). Pass 8 likewise checks a
  format-string value against the parameter's type only for such an Environment, and only for
  an entry whose `enableIf` is known to be `true`. An Environment's `enableIf` expression is
  certainly not evaluated at job creation for the Step (`enable_if_reads_unconvertible_literal`)
  when it reads (references, whether or not evaluation reaches the reference) an
  `Env.Param.<name>` or `Env.RawParam.<name>` whose value for the Step is a literal, the
  entry's or the `default`, that does not convert to the parameter's type: job creation does
  not evaluate such an `enableIf` (§3.2.2 item 2), so the Environment is not disabled whatever
  the Job Parameters are, and a failure of its checks is certain here, which §7.6 rule 5
  prefers to report at the earliest stage (JB-114's shape; plan 4 A45). A missing value does
  not count: it is a constraint 7 error already. A format-string value or `default`, or an
  expression that does not parse, gives `false`. The
  first undeclared key of an entry lists the declared names (once each, at most 50 names
  and 512 characters, then "and N more"; the first name is always listed, however long);
  later undeclared keys of the same entry do not
  repeat the list. Job Environments are indexed by name once per template, and each one's
  list is built only when a message needs it.
- Constraint 7 for Job Environments (`check_required_parameters_supplied`): every Step
  supplies every parameter without a `default` of every `jobEnvironments` entry that applies
  to it: each one with `activation: ALWAYS`, each one with `BY_DEFAULT` it does not decline,
  and each one with `activation: ON_REQUEST` that it requests. Whether an Environment applies
  does not depend on its own `enableIf` or its `exclusiveGroup` (§3.2 constraint 7), so
  neither is read: a Step must supply the parameters even where the `enableIf` is `false` or
  the Environment is superseded, and grouped Environments are checked too. The check needs
  only what the Step's entries are known to be ([Known enableIf
  values](#known-enableif-values-env_params); §3.2 checked-at paragraph, §7.6 rule 5): an entry
  known to be enabled supplies its values and requests an `ON_REQUEST` Environment; one known
  to be `false` supplies and requests nothing, and declines a `BY_DEFAULT` Environment, which
  then requires nothing of the Step (§3.2 item 3.2); for an `ALWAYS` one it is an error
  (below), and template validation still checks the Environment as applying, so its
  parameters then all need their `default`s (job creation takes no later §7.4.1 step for the
  Step after the error); an entry whose `enableIf` is not known defers the check for a
  `BY_DEFAULT` or `ON_REQUEST` Environment and its Step to job creation, since disabled it
  declines or requests nothing. For an `ALWAYS` one it does not: disabled, the entry is an
  error of its own and supplies nothing, so a parameter the entry does not supply is missing
  whatever its `enableIf` resolves to, and is reported here (§7.6 rule 5, the earliest
  stage). Reported once per Step, at the Step: "must supply, in
  an 'environment:' dependency, every parameter without a default of each Environment that
  applies to it (<why>), whatever that Environment's own enableIf is; missing: ...", listing
  the missing parameters by Environment (at most 50 names and 512 characters, counting the
  quotes, separators, and " of Environment" suffixes, then "and N more"; the first name is
  always listed, however long), so the report grows with the number of Steps and not with
  Steps times parameters. When the Step's entry for an Environment in the list is disabled by
  its `enableIf`, the Environment's group adds "(the Step's 'environment:' dependency for it
  is disabled by its enableIf, so it supplies nothing; Template Schemas §3.2 item 3.2)",
  counted in the 512 characters, since the Step does have an entry for the Environment, which
  a reader would otherwise take as enabled; `check_environment_templates` adds the same note
  for an attachment (job creation does not: a disabled entry for an `ALWAYS` Environment is a
  §7.4.1 step 4 error, which stops the Step before constraint 7). Since a disabled entry
  declines a `BY_DEFAULT` Environment and requests no `ON_REQUEST` one, the note arises only
  for an `ALWAYS` one. It is the only note. An entry counts when its `environment` key names
  that Environment, even if the entry is malformed, so pass 6's constraint 1 error is not
  followed by a second one. A duplicated definition name, or a duplicated Job Environment
  name, is reported once. The check counts each Step's missing parameters from its own
  entries alone (each `ALWAYS` or `BY_DEFAULT` Environment's start missing; an enabled entry supplies some, or
  adds an `ON_REQUEST` one's; a disabled one removes a `BY_DEFAULT` one's), and the list stops after 50 names, so it is linear in
  the template size.
- §3.2 item 3.2: an entry whose `enableIf` is known to be `false` (the boolean scalar, or an
  expression such as `"false"`) and that names a `jobEnvironments` entry with `activation:
  ALWAYS` is an error at `steps[i] ->
  dependencies[j] -> enableIf`: "is false, so Step 'S' would decline Environment 'E', whose
  activation is ALWAYS: no Step can leave out an ALWAYS Environment (Template Schemas §3.2
  item 3.2)." An `enableIf` that is not known, and an entry naming an external Environment,
  are left to job creation (§7.6 rule 5).

An entry that names no `jobEnvironments` entry names an external Environment, whose
definitions are unknown until the Job is created, so only the checks above that do not
need them apply. Job creation keeps such an entry, when enabled, without checking its
values (it does re-check constraint 3), and its
submission stage resolves it against the attached Environment Templates and checks
constraints 5–7 and its resolved format-string values for it.
For an entry that names a `jobEnvironments` entry, for every literal Job Environment
`default`, and for every non-literal one a Step takes, `create_job` makes those checks (see
[job-creation.md](job-creation.md#environment-parameter-values-env_params)).

## Pass 12: CONTROL_FLOW Gating and Structure

Validates or rejects the fields `CONTROL_FLOW` (RFC 0011, RFC 0012, RFC 0013) adds. It implements
item 1, the Environment Template part of item 5, and the structural part of item 9 of
RFC 0011 §Validation, "At template validation", and item 1 of RFC 0012 §Validation.
RFC 0011 items 2 to 6 (the `Steps.*` scope and `enableIf`) and the scope and typing of a
job output's `value` (the rest of RFC 0011 item 9) are in pass 8.

**Without the extension**, each field is an error at its own path, with the message
`<field> requires the CONTROL_FLOW extension.`:

- `jobOutputDefinitions`
- `steps[i] -> enableIf`
- `steps[i] -> outputVariables`
- a loop (RFC 0013), at `steps[i] -> loop`: `loop requires the CONTROL_FLOW extension (RFC
  0013).` Its body Steps still get every other pass's checks at their own paths, and pass 8,
  which seeds no `Loop.*` without the extension, reports a `Loop.*` reference as an
  undefined variable.

Each per-Step rule here applies to body Steps too, at `steps[i] -> loop -> steps[j]`.

A `Steps.*` reference fails in pass 8 as an
undefined variable, because without the extension nothing seeds the `Steps` root. Pass 8
checks `enableIf` and the `Steps.*` scope only with the extension (see § The `Steps.*`
Scope and `enableIf`).

An Environment's `enableIf` is RFC 0010's (`ENV_PARAMS`): pass 11 gates it, and pass 8
checks it (see [Environment enableIf](#environment-enableif-env_params)).

**Prerequisites.** `CONTROL_FLOW` builds on `ENV_PARAMS` (RFC 0011 §Extension), which in turn
requires `EXPR`, so a template that lists `CONTROL_FLOW` must list both
(`control_flow::check_prerequisites`). Each missing one is an error at `extensions`:
`CONTROL_FLOW requires EXPR; both must be listed in the template's `extensions` (RFC
0011).` (through `helpers::check_expr_prerequisite`, which pass 10 shares for
`WRAP_ACTIONS`) and `CONTROL_FLOW requires ENV_PARAMS; both must be listed in the
template's `extensions` (RFC 0011).` A template that lists neither gets both, and the
`ENV_PARAMS` prerequisite's own error for `EXPR` too only when it lists `ENV_PARAMS`. The
check also runs on the early-return path taken when the `ENV_PARAMS` prerequisite fails (see
[Entry Points](#entry-points)), so the error is not lost.

**With the extension**, each Step's `outputVariables` must satisfy:

| Rule | Path | Message |
|------|------|---------|
| At least one element | `outputVariables` | `if provided, must contain at least one element.` |
| At most 32 elements | `outputVariables` | `must not contain more than 32 elements.` |
| Name length within the identifier limit (64, or 512 with `FEATURE_BUNDLE_1`) | `outputVariables[k]` | `name exceeds N characters.` (a name over 512 characters already fails deserialization: `Identifier length must be 1..=512, got N`) |
| Unique names within the Step | `outputVariables[k]` (each repeat) | `duplicate output variable name: 'X'` |
| No `default`, `userInterface`, or (on `PATH` and `LIST[PATH]`) `dataFlow` | `outputVariables[k] -> <property>` | `is not allowed in an output variable definition.` |
| Constraint properties valid for the type | `outputVariables[k]` | the job parameter definition's message, relabeled `Output variable 'X': ...` |
| `collect: APPEND` only with a `LIST[...]` type | `outputVariables[k] -> collect` | `APPEND requires a LIST[...] type; 'INT' is not a list type.` |
| `unit` of 1 to 32 characters, none in Unicode Cc | `outputVariables[k] -> unit` | `must not be empty.`, `exceeds 32 characters.`, `contains control characters.` |
| No `LIST[LIST[INT]]` on a Step with a `parameterSpace` | `outputVariables[k] -> type` | `LIST[LIST[INT]] is not allowed in a Step with a parameterSpace: ...` |
| In a Step with outputs, a literal-list `CHUNK[INT]` `range` has distinct values (checked only with `TASK_CHUNKING`; without it pass 9 rejects the parameter itself) | `parameterSpace -> taskParameterDefinitions[k] -> range` | `must have distinct values in a Step with outputVariables, so that each value names one Task; value 2 appears more than once.` |

With the extension, `jobOutputDefinitions` must satisfy the same rules, through the same
helpers, with these differences:

| Rule | Path | Message |
|------|------|---------|
| 1 to 50 elements | `jobOutputDefinitions` | `if provided, must contain at least one element.`, `must not contain more than 50 elements.` |
| Unique names among the job outputs (a job parameter of the same name is fine: separate namespaces) | `jobOutputDefinitions[k]` | `duplicate job output name: 'X'` |
| No `collect`, `default`, `userInterface`, or (on `PATH` and `LIST[PATH]`) `dataFlow` | `jobOutputDefinitions[k] -> <property>` | `is not allowed in a job output definition.` |
| Constraint properties valid for the type | `jobOutputDefinitions[k]` | relabeled `Job output 'X': ...` |
| `unit` | `jobOutputDefinitions[k] -> unit` | as for an output variable |

There is no `collect: APPEND` rule (there is no `collect`), and `LIST[LIST[INT]]` is
allowed, because a job output has no `parameterSpace` to nest it in. The name length rule
is the output variable's. The deserialization rules below apply too, with `job output`
in the messages, `value` and `unit` as the extra fields an unknown-field message lists,
and a missing or `null` `value` reported as `missing 'value' field in job output
definition`. Each definition's errors are reported in the order of the table rows, and an
output variable's `collect` error comes before its `unit` error.

Outputs on a Step with a `CHUNK[INT]` Task parameter are allowed. Pass 12 checks the
distinct-values rule on the integer items of a `range` written as a YAML list. A range
string such as `"1-10"` cannot repeat a value, because its sub-ranges may not overlap. A
computed range, and a YAML list with format-string items, is checked when it is
materialized (RFC 0012 §Chunked Steps item 1), in the Task enumeration group at job
creation or at step input binding (see [job-creation.md](job-creation.md) § Step input
binding).

Constraint validity reuses `JobParameterDefinition::validate_definition` on the wrapped
definition, so the rules are exactly those of the job parameter of the same type (for
example, `minValue` may not exceed `maxValue`, and `allowedValues` must respect
`minLength`/`maxLength`). Its messages start with `Parameter 'X': `, which pass 12
replaces with `Output variable 'X': `. A constraint property that the type does not have
(such as `minValue` on a `STRING`, or `allowedValues` on a `BOOL`) is an unknown field of
that job parameter definition and fails deserialization.

Some rules are enforced while the definition is deserialized, so they report the serde
message without a path, as other schema errors do. An unknown field in an output
variable's own mapping is reported with an output variable's fields
(`unknown field `minValue`, expected one of `name`, `description`, `allowedValues`,
`minLength`, `maxLength`, `collect`, `unit``), not the wrapped job parameter
definition's, which would list `default` and `userInterface`:

- an unknown type (`unknown output variable type: 'DICT'`), a missing one, or one that
  is not a string (`'type' must be a string, got a number`);
- a `collect` that is not `ONCE`, `LAST`, or `APPEND` (case-sensitive);
- a `unit` that is not a string;
- a `name` that is not an `<Identifier>`;
- a Step's or a loop's `enableIf`, or a loop's `while` or `until`, that is a list or a
  mapping (`enableIf must be a boolean, integer, float, or string (an expression field,
  Template Schemas §7.6)`), or an integer above `i64::MAX`;
An explicit `null` for `outputVariables`,
`jobOutputDefinitions`, or an output's `collect`, `unit`, `default`, `userInterface`, or
`dataFlow` means the field is omitted, as for the template's other optional fields, so it
is neither gated nor reported. A `null` for a Step's or a loop's `enableIf`, or a loop's
`while` or `until`, is not: an expression field's `null` is an error (§7.6), reported by
pass 8 with the extension and gated by pass 12 without it. A `while: null` beside an `until`, or the reverse, counts
as present, so it gets both pass 8's `must not be null` and pass 12's `not both`.

**Loops** (RFC 0013 §Validation items 1, 5, 6, and the literal form of 7), with the
extension:

| Rule | Path | Message |
|------|------|---------|
| Exactly one of `while` and `until` | `steps[i] -> loop` | `must have exactly one of 'while' and 'until', not both.` or `must have exactly one of 'while' and 'until'.` |
| A literal `maxIterations` from 1 to 1,000 | `steps[i] -> loop -> maxIterations` | `must be from 1 to 1000; got N.` |
| `variables`: 1 to 32 elements | `steps[i] -> loop -> variables` | as for `outputVariables` |
| A variable is not named `Iteration` | `... -> variables[k] -> name` | `'Iteration' is reserved: Loop.Iteration is the index of the current iteration.` |
| Unique variable names, name length, constraint properties | `... -> variables[k]` | `duplicate loop variable name: 'V'`; relabeled `Loop variable 'V': ...` |
| No `collect`, `unit`, `default`, `userInterface`, or (on path types) `dataFlow` on a variable | `... -> variables[k] -> <property>` | `is not allowed in a loop variable definition.` |
| Loop `outputVariables`: 1 to 32 elements, unique names | `steps[i] -> outputVariables`, `[k]` | as for a Step's; `duplicate loop output name: 'X'` |
| `collect` is present | `... -> outputVariables[k] -> collect` | `is required in a loop output definition: LAST or APPEND.` |
| `collect` is not `ONCE` | same | `ONCE is not allowed for a loop output, whose value is evaluated at every iteration boundary; use LAST or APPEND.` |
| `APPEND` only with a `LIST[...]` type | same | `APPEND requires a LIST[...] type; 'INT' is not a list type.` |
| No `default`, `userInterface`, `dataFlow`; `unit` | `... -> outputVariables[k] -> <property>` | `is not allowed in a loop output definition.`; as for an output variable |

`LIST[LIST[INT]]` is allowed for both: a loop has no `parameterSpace`. Missing `initial`,
`next`, or `value`, an unknown field (listing `initial` and `next`, or `collect`, `value`,
and `unit`), and an unknown type fail deserialization, as for an output variable, with
`loop variable` or `loop output` in the message. Names, dependencies, the body's shape, and
cycles are pass 6's, and the expressions pass 8's.

**A loop's `dependencies`** may not have an `environment:` entry, since a loop has no
Sessions (RFC 0011 `<Environment>`, after item 8; RFC 0013 `<StepLoop>`): `a loop's dependencies may not have an
'environment' entry: a loop has no Sessions. Put the entry on the body Steps that need the
Environment (RFC 0011).`, at `steps[i] -> dependencies[k]`. Its `dependsOn` and `step`
entries are allowed.

**Environment Templates.** `CONTROL_FLOW` applies to Job Templates only. Listing it in an
Environment Template is an error at `extensions`: `CONTROL_FLOW is not allowed in an
Environment Template; it applies to Job Templates only (RFC 0011).` The template's
environment's `enableIf` is RFC 0010's.

### Known to be enabled (`CONTROL_FLOW`)

RFC 0011 `<Environment>` item 3 has a Step's Environment selection applied only once the
Step's `enableIf` is `true`, so a check that would fail a Step for its Environments fails a
Step that runs. Before job creation, such a check is made only for a Step **known to be
enabled**: Template Schemas §7.6 rule 5 applied to the Step's `enableIf`, with the
`CONTROL_FLOW` scope (`Steps.*`, `Loop.*`, `RawLoop.*`) unknown; for a body Step, its loop's
`enableIf` must also be known to be `true`, and so must its `while` (the first iteration's,
with `Loop.*` unknown) if it has one: an `until` loop runs its first iteration. A Step without an `enableIf`, outside a loop, is known to be enabled.
`KnownEnableIfs::step_known_enabled` holds the answer for every Step template, in
`JobTemplate::step_templates()` order (body Steps included), and every check below reads it:

- pass 11: the error for a disabled entry that names an `ALWAYS` Environment, constraints 5
  and 6 for an entry's values, and constraint 7 (`check_required_parameters_supplied`);
- pass 8: the type and resolve checks of an entry's values (a Step not known to be enabled
  gets only the undefined-variable check of their scope, since resolving a value is part of
  its selection, RFC 0011 `<Environment>` item 3, "Before Job creation");
- pass 10: the single-wrap-layer rule;
- `check_environment_templates`: an entry that names nothing or a missing `required`
  Environment, the values, constraint 7, and the `ALWAYS` error, though an ambiguous
  Environment name is always reported;
- the literal combined host requirements (`host_requirements::check_literal_combinations`).

For any other Step these checks are left to job creation, which makes them when the Step's
selection is applied (see
[job-creation.md](job-creation.md#environment-selection-control_flow)). Without
`CONTROL_FLOW` every Step is known to be enabled, so nothing changes.

### Known gaps inherited from job parameters

Constraint validity is exactly the job parameter rule, including its gaps: the EXPR-type
job parameter definitions (`RANGE_EXPR` and the `LIST[...]` types) check `minLength`
against `maxLength`, `item` bounds, and `RANGE_EXPR` `minLength >= 1` only through their
`default`, so an output (or a job parameter) declaring `minLength: 5, maxLength: 1`
passes. Fixing that belongs in `expr_parameters.rs`, for both.

### Not yet checked

These RFC rules belong to later changes in the prototype plan
(`docs/control-flow-prototype-plan.md`):

- The warning for a conditional Step's `null`, or a `while` loop's `LAST` output (RFC
  0013), in a discarding position (RFC 0011 §Validation item 7). openjd-rs has no
  validation-warning channel; adding one is a public API decision (see ambiguity A1 in the
  plan).

## Error Infrastructure

See [error-handling.md](error-handling.md) for details on `ValidationErrors`, `PathElement`,
and error formatting.

## Shared Helpers

### Regex Patterns

| Pattern | Purpose |
|---------|--------|
| `AMOUNT_CAP_RE` | Amount capability name: `[scope:]amount.name[.sub]` |
| `ATTR_CAP_RE` | Attribute capability name: `[scope:]attr.name[.sub]` |
| `ATTR_VALUE_RE` | Attribute value: `[A-Za-z_][A-Za-z0-9_-]*` |

### Constants

| Constant | Values |
|----------|--------|
| `STANDARD_AMOUNT_CAPABILITIES` | `amount.worker.vcpu`, `amount.worker.memory`, `amount.worker.gpu`, `amount.worker.gpu.memory`, `amount.worker.disk.scratch` |
| `STANDARD_ATTRIBUTE_CAPABILITIES` | `attr.worker.os.family`, `attr.worker.cpu.arch` |
| `RESERVED_SCOPES` | `worker`, `job`, `step`, `task` |

Note: Standard capability names include their `amount.` or `attr.` prefix.

### Utility Functions

- `has_control_chars(s)` — True if string contains control chars other than `\n`, `\r`, `\t`
- `check_capability_reserved_scope(name, standard, path, errors)` — Errors if non-standard
  capability uses a reserved scope
- `validate_env_var_name(name, path, errors)` — Non-empty, ≤256 chars, no leading digit,
  alphanumeric+underscore only
