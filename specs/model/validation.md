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

The validation pipeline is passes 5–11 of the overall decode pipeline (passes 1–4 are in
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
| 8 | `format_strings.rs` | Validate format string variable references; adapts scopes and expression complexity based on EXPR |
| 9 | `task_chunking.rs` | Gate TASK_CHUNKING features (ChunkInt parameters) |
| 10 | `wrap_actions.rs` | Gate WRAP_ACTIONS features (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit) and enforce the single-wrap-layer-per-session rule (RFC 0008) |
| 11 | `env_params.rs` | Gate ENV_PARAMS features (Environment `envParameterDefinitions`, `activation`, `hostRequirements`, and `enableIf`, the `step:` and `environment:` forms of `<StepDependency>`, the latter with its `enableIf`) and check their structural rules (Environment Parameters RFC) |

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

## Pass 5: Limits Enforcement

Walks the template tree checking every name length, list count, and string length against
`EffectiveLimits`. Pure numeric checks with no extension branching.

Checks include:
- Job name length vs `max_job_name_len`
- Parameter count vs `max_param_count`
- Parameter name lengths vs `max_identifier_len`
- Step name lengths vs `max_step_name_len`
- Embedded file name/filename lengths
- Task parameter name lengths
- Environment name lengths vs `max_env_name_len`

## Pass 6: Structural Validation

The largest pass. Validates template structure using `EffectiveRules`. Key checks:

**Template level:**
- At least one step required
- Job name non-empty, no control characters
- Extensions list non-empty if present (enforced early in pass 4)
- Description length and control character validation

**Parameter definitions:**
- Non-empty if present
- No duplicate names (case-sensitive)
- Parameter type in `rules.allowed_job_param_types`
- Type-specific validation via `validate_definition(limits)`

**Environment uniqueness:**
- Names unique within `jobEnvironments`
- Names unique within each step's `stepEnvironments`
- A `stepEnvironments` name must not match any `jobEnvironments` name
- Different steps may reuse a `stepEnvironments` name (only one step's environments are active in a session at a time)

**Step validation:**
- No duplicate step names
- Step name non-empty, no control characters
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
  field errors. For `dependsOn` entries and their ENV_PARAMS synonym `step` (§3.2 item 1),
  read through `StepDependency::step()` so the two are one form: no self-dependency, target
  must exist, no duplicates (a `step` and a `dependsOn` entry naming one Step are
  duplicates). The `environment:` form, and the `step` form's gate, are checked in pass 11,
  and cycle detection sees the `dependsOn` and `step` entries as one graph.
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
  have at most one element) fires at decode when the list has **more than one element
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
- Iterative DFS with tri-state marking (Unvisited/Started/Completed) on the step
  dependency graph

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
ENV_PARAMS (and so EXPR): the template-scope tables of host requirements, parameter-space
ranges, Step `let` bindings, `environment:` entry values and `enableIf`, and action timing
fields, and every session and task scope, in Job Templates and Environment Templates alike
(`add_job_symbols`). It is not in the Job Template `name`'s table or an Environment
parameter `default`'s, so a reference there is "Undefined variable" (§7.3.1). Without
ENV_PARAMS it is not defined at all; with EXPR alone the message suggests `Job.Name`.

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
- **A format-string `default`** (§4.5 item 4) is checked by `validate_env_param_defaults`
  against the template-scope table of the *same document's* top-level
  `parameterDefinitions`: `RawParam.*` for every Job Parameter and `Param.*` for the
  non-PATH ones, with no `Job.Name`, `Step.Name`, `Session.*`, or `Env.*`. For a Job
  Environment that is the Job Template's parameters; for an Environment Template, its own.
- **A format-string `environment:` entry value** (§3.2) is checked by
  `validate_env_entry_values` against the Step's template-scope table
  (`step_template_symtab`): `RawParam.*`, non-PATH `Param.*`, `Job.Name`,
  `Job.EnvNames`, `Step.Name`, and the step-level `let` names. `Task.*`, `Session.*`, and
  `Env.*` are not in it. A step-level `let` is itself template scope, so it cannot carry a
  PATH `Param.*` into an entry.
- **An `environment:` entry's `enableIf`** (§3.2 item 5, constraint 9) is an expression
  field of type `bool` (§7.5), checked by `validate_enable_if` with the expression-field
  checker (see [template-types.md](template-types.md#expression-fields-75)) against the same
  table, for every well-formed `environment:` entry that has one. A scalar other than a boolean
  gets "the integer scalar 1 does not fit type bool: an
  expression field of type bool takes a boolean scalar or a string holding one expression
  (Template Schemas §7.5)."; a present `null` gets "must not be null: …" with the same
  tail. An expression must parse ("Invalid expression: <error>" with the expression and
  caret; a value containing `{{` adds "An expression field takes an expression without
  '{{ }}' (Template Schemas §7.5)." after the error message), its names must be in the table,
  and its static type must fit `bool`: "has type T, which does not fit type bool; an
  expression field converts no type except int to float (Template Schemas §7.5).", or for
  `nulltype` "has type nulltype, which does not fit type bool (Template Schemas §7.5).". A
  `bool?` is accepted; its `null` is a job-creation error (§7.5 rule 2). No conversion
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
expression reads (a `Param.*` string that does not parse as an INT, a constraint, PATH
absoluteness) is left to job creation. The target type is known for a `default` and for an entry that names a
`jobEnvironments` entry. An entry for an external Environment is checked with no target
type, since its definitions are not known until the Job is created; so is an entry for a
`jobEnvironments` entry whose own `enableIf` is not absent or `true`, since a Step's values
for an Environment disabled for it are not checked against their type (§3.2.2), and for an
expression `enableIf` whether it is disabled is not known here (§7.5 rule 5), unless the
expression is certainly unresolved for an entry whose own `enableIf` is absent or `true` (see
[Pass 11](#pass-11-env_params-gating)).

### Environment enableIf (ENV_PARAMS)

An Environment's own `enableIf` (§4 item 6) is an expression field of type `bool` (§7.5),
checked by `validate_env_enable_if` with the expression-field checker and the messages of an
entry's `enableIf` (above), when the document lists ENV_PARAMS (pass 11 reports it
otherwise). Its table depends on where the Environment is:

- **A `jobEnvironments` entry or an Environment Template's `environment`** (constraint 1):
  exactly the table of its `hostRequirements`, `build_env_host_requirements_symtab` (see
  [Environment hostRequirements](#environment-hostrequirements-env_params)): the
  document's `RawParam.*` and non-PATH `Param.*`, `Job.Name`, `Job.EnvNames`, and its own
  `Env.RawParam.*` of any type (PATH as `string`, LIST[PATH] as `list[string]`) and
  `Env.Param.*` of types other than PATH and LIST[PATH].
- **A `stepEnvironments` entry** (constraint 2): the Step's template-scope table
  (`step_template_symtab`), the table of an entry's `enableIf`: `RawParam.*`, non-PATH
  `Param.*`, `Job.Name`, `Job.EnvNames`, `Step.Name`, and the Step's `let` names.

Anything else (constraint 3: `Task.*`, `Session.*`, `Env.File.*`, another Environment's
parameters, a PATH `Env.Param.*`, or, for a Job Environment, `Step.Name` or a `let` name) is
"Undefined variable". An Environment whose `enableIf` is `false` is still validated in full,
its definitions included. It is disabled for every Step (§3.2.2), so constraint 7 and the
checks of the Steps' values for it (constraint 6, and the target type of a format-string
value below) do not apply to it; for an Environment whose `enableIf` is an expression they
are undetermined at template validation and left to job creation (pass 11, below; §7.5 rule
5). For the
single-layer rule an Environment whose `enableIf` is absent or `true` counts as entered, the
scalar `false` as not entered, and an expression defers to job creation (see
[Pass 10](#pass-10-wrap_actions-gating)).

### Environment hostRequirements (ENV_PARAMS)

An Environment's `hostRequirements` (§4 item 5.1) get the Step's checks
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
| attribute `anyOf`/`allOf` values (Template Schemas §3.3.2.2) | `string? \| list[string]` | when certainly a string: ≤ 100; for a standard capability, ≤ longest allowed value | `validate_attribute_capability_value` (charset / allowed set) on the value, or on each element when a list flattens; `null` skips the element |
| amount / attribute `name` (Template Schemas §3.3.1.1 / §3.3.2.1) | `string` | ≤ 100 | `helpers::check_capability_name` (length, pattern, reserved scope); then, across the requirements, uniqueness of the known names, and for a standard attribute name the standard-value and single-valued `allOf` checks on literal values |
| task param STRING/PATH range elements (Template Schemas §3.4.2) | `string? \| list[string]` | when certainly a string: ≤ 1024 | ≤ 1024 chars per element (a list flattens); PATH elements additionally must be non-empty (see below); `null` skips the element |
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
  flattens inline (the spec's worked example is `args`; range elements
  and attribute values are list items under the same rule);
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
  re-checks after resolution ("has no elements after resolution"). A
  range that is entirely one expression can also use the whole-field
  `<ListExpressionString>` form (Expression Language §1.3.12 "Task
  Parameter Range Field Extensions").
- The length bound for list-item fields only applies when the resolution
  is certainly a string (`StaticResolution::resolved_type` is `string`):
  a list-valued resolution distributes its characters across elements,
  so the display-form length says nothing about any single element.
- **Empty range elements are rejected for PATH only.** §3.4.2 sets a
  minimum length of 1 on `<TaskParameterStringValue>` (which STRING and
  PATH ranges share), but the reference implementation enforces it only
  for PATH — its rationale being that an empty string is not a valid
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
  `jobEnvironments` entries with `activation: ALWAYS`, the `ON_REQUEST` ones the step
  requests (ENV_PARAMS, Template Schemas §4 item 4), and the step's own
  `stepEnvironments`. So this is enforced per step: for every step, the count of
  wrap-defining envs among those must be ≤ 1. Multiple `ALWAYS` wrap envs in
  `jobEnvironments` alone are reported once at the `jobEnvironments` path (reachable from
  every session); a step that adds its own wrap env on top is reported at that step's
  `stepEnvironments` path; and a step whose own `stepEnvironments` add no wrap env (or that
  has none) but whose requested `ON_REQUEST` wrap envs make more than one is reported at
  its `dependencies` path. A step with `stepEnvironments` that add no wrap env and requests
  none, under two `ALWAYS` wrap envs, is also reported at its `stepEnvironments` path, beside
  the `jobEnvironments` error, as before ENV_PARAMS. A
  step requests an Environment here only through an entry whose `enableIf` is absent or
  the boolean scalar `true`: one whose `enableIf` is an expression, even a constant one, is
  checked at job creation, over the
  step's resolved set and the attached Environments (see
  [job-creation.md](job-creation.md#the-submission-stage), prototype plan R8). In the same
  way (ENV_PARAMS, §4 item 6 tail) a wrapping Environment counts only when its own
  `enableIf` is absent or the scalar `true`: one with the scalar `false` is not entered, and
  one with an expression is decided at job creation, which checks the final set. So a
  violation is reported here only among Environments certainly entered, and it is one
  whatever the expressions resolve to. The rule for every set-dependent check made before
  job creation is to fail only when the failure is certain at that stage; with two certainly
  entered wrapping Environments and a third whose `enableIf` is an expression, the template is
  rejected here, since no Job could be created from it (Template Schemas §7.5 rule 5 and §4
  item 6's tail, spec `5535230`). Two
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

With it, `activation` (§4 item 4) is rejected on a `stepEnvironments` entry ("is not permitted
on a stepEnvironments entry, which only its own Step's Sessions enter."); an unrecognized
value (constraint 1, case-sensitive: `always` is rejected; plan A24) is a decode error naming
the two values. `hostRequirements` (§4 item 5.2) is rejected on a `stepEnvironments` entry
("is not permitted on a stepEnvironments entry, whose Step has hostRequirements of its
own."); on a `jobEnvironments` entry or an Environment Template's `environment` its §3.3
checks are passes 6 and 8's.

`enableIf` on any Environment (§4 item 6 constraint 4) needs ENV_PARAMS in the document, in
every position, a `stepEnvironments` entry included; without it the field is reported at its
path: "enableIf on an Environment requires the ENV_PARAMS extension.". Its value is checked
in pass 8
(see [Environment enableIf](#environment-enableif-env_params)).

With it, for each environment's `envParameterDefinitions` (§4 item 3, §4.5):

- Not permitted on a `stepEnvironments` entry (constraint 4). A `jobEnvironments` entry
  and an Environment Template's `environment` may declare parameters.
- 1–50 entries (constraints 1–2; the cap is fixed and FEATURE_BUNDLE_1 does not raise
  it), names unique within the list (constraint 3), and each name at most
  `max_identifier_len`. A name may equal a Job Parameter's or another environment's.
- The type: any Job Parameter type available with EXPR, which the document lists (§4.5 item
  1), so every type that decodes is allowed and there is no type check here. Type names are
  case-insensitive, as with EXPR for a Job Parameter; the decode-time case check in
  `parse.rs` does not walk environment `envParameterDefinitions`, since a document without
  EXPR that has them is rejected (by the prerequisite, or without ENV_PARAMS by the gate
  above).
- `userInterface`, `objectType`, and `dataFlow` are permitted (§4.5 items 2–3), with the
  shape and constraints of the Job Parameter of the same type, since the definition is
  decoded as one. They are informational: nothing in job creation or the sessions runtime
  reads them, and an Environment parameter's `dataFlow` does not add to the Job's.
- The definition's own checks run through `JobParameterDefinition::validate_definition`
  (for example `minLength` > `maxLength`, or a `userInterface` control the type does not
  have).
- An explicit `default: null` is no default (decode drops it), as for a Job Parameter.
- A literal `default` is checked against the type and the constraints (§4.5 "checked at
  two points", point 1), even when every Step supplies the parameter. The check decodes
  the literal with the decoder of the Job Parameter type's `default` field (so a STRING
  `default` must be a string and an INT one an integer or `<intstring>`), coerces it with
  `coerce_json_to_job_parameter_type` on the decoder's canonical form (so the trimming
  `FlexInt` / `FlexFloat` do applies, and `" 3 "` is a valid INT literal here as it is for a
  Job Parameter `default`), and calls `check_constraints`. Absoluteness of a
  PATH value depends on the submission path format and is left to job creation. An
  object or `null`, at any depth, is rejected; so is a list element containing `{{`
  (plan A14: list-literal elements are literals).
- A format-string `default` of a list type must be a single `{{ }}` expression (§4.5
  item 4). Its references are checked in pass 8.

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
- Constraint 9 (`enableIf`, §3.2 item 5): an expression field of type `bool` (§7.5), whose
  value pass 8 checks. A boolean, number, string, or `null` decodes, except an integer above
  `i64::MAX` that fits `u64`; that and a list or a map are decode errors naming `enableIf`.
  Constraint 1's "only with `environment`" is checked for `enableIf` in pass 6, with
  `required` and `parameters`.
- Literal forms no parameter type accepts (plan A4): an object or `null` anywhere; a list
  that mixes lists and other values; a list nested beyond
  `LIST[LIST[INT]]`; and a nested list with an element the `LIST[LIST[INT]]` `default`
  decoder (`FlexInt`) rejects, so an integer, an integer-valued float, and an integer string
  (trimmed) are accepted.
- For an entry that names a `jobEnvironments` entry, unless its `enableIf` is the boolean
  scalar `false` (an expression such as `"false"` is decided at job creation), which
  has no effect of any kind (§3.2 item 2; plan A28): constraint 5 (every key names a
  declared parameter; an environment that declares none rejects an entry with any); and,
  only when the Environment's own `enableIf` is absent or `true`, or is **certainly
  unresolved** for the entry (below), constraint 6 against literal values, by the same check
  as a literal `default`, and the rule that a format-string value for a list-typed parameter
  must be a single `{{ }}` expression (a list type has no string form, so another value cannot
  convert). For an Environment whose `enableIf` is `false` these value checks do not apply
  (§3.2.2), and for one whose `enableIf` is any other expression a violation is not certain
  here (§7.5 rule 5), so job creation makes them for the Steps it does not disable (plan 4
  A39). Pass 8 likewise checks a format-string value against the parameter's type only for
  such an Environment. An `enableIf` expression is certainly unresolved for a Step
  (`enable_if_certainly_unresolved`) when it reads only the Environment's `Env.Param.*` /
  `Env.RawParam.*`, every value it reads is a literal (the entry's, for an entry whose own
  `enableIf` is absent or `true`, since one that may be disabled supplies nothing certain; or
  the `default`) or has none, and progressive evaluation with the literals that convert and
  `unresolved[T]` for the
  rest is unresolved: job creation evaluates it with the same values, so the Environment is
  not disabled whatever the Job Parameters are, and a failure of its checks is certain here,
  which §7.5 rule 5 prefers to report at the earliest stage (JB-114's shape; plan 4 A45). A
  known result, as of a constant expression, stays unknown here. The
  first undeclared key of an entry lists the declared names (once each, at most 50 names
  and 512 characters, then "and N more"; the first name is always listed, however long);
  later undeclared keys of the same entry do not
  repeat the list. Job Environments are indexed by name once per template, and each one's
  list is built only when a message needs it.
- Constraint 7 for Job Environments: every Step supplies every parameter without a
  `default` of every `jobEnvironments` entry its Sessions enter: each one with
  `activation: ALWAYS`, and each one with `activation: ON_REQUEST` that it requests. Only an
  Environment whose own `enableIf` is absent or `true`, or certainly unresolved for the Step
  (above; with the values of its entry, if one requests it), is checked: one whose `enableIf`
  is `false` is disabled for every Step and is not held to constraint 7, and one whose
  `enableIf` is any other expression may be disabled, so a violation is not certain and the
  check is left to job creation (§3.2.2, §7.5 rule 5). For a certainly unresolved one the
  Environment's group gets the unresolved note below. Whether
  an entry is enabled is known here only when its `enableIf` is absent or a boolean scalar
  (§3.2, checked-at paragraph; §7.5 item 1): an entry whose `enableIf` is `false` supplies
  and requests nothing, so an `ALWAYS` Environment's parameters then all need their
  `default`s; an entry whose `enableIf` is an expression, even a constant one such as
  `"true"`, defers the check for its Environment and Step to job creation. Reported once per Step, at the Step, listing the missing parameters by
  Environment (at most 50 names and 512 characters, counting the quotes, separators, and
  " of Environment" suffixes, then "and N more"; the first name is always listed, however
  long), so the report
  grows with the number of Steps and not with Steps times parameters. When the Step's entry
  for an Environment in the list is disabled by its `enableIf`, the Environment's group adds
  "(the Step's 'environment:' dependency for it is disabled by its enableIf, so it supplies
  nothing; Template Schemas §3.2 item 5)", counted in the 512 characters, since the Step does
  have an entry for the Environment, which a reader would otherwise take as enabled; job creation and the submission stage add the same note for an
  entry an `enableIf` expression disabled. A second note, also counted in the 512
  characters, marks an Environment whose own `enableIf` is unresolved for the Step (it depends
  on a value that is missing or does not convert): template validation and
  `check_environment_templates` add it for a certainly unresolved one, and job creation and
  the submission stage for any whose `enableIf` was unresolved:
  "(its own enableIf depends
  on a value that is missing or does not convert, so it does not disable the Environment;
  Template Schemas §3.2.2)". An entry counts when its `environment` key names
  that Environment, even if the entry is malformed, so pass 6's constraint 1 error is not
  followed by a second one. A duplicated definition name, or a duplicated Job Environment
  name, is reported once. The check counts each Step's missing parameters from its own
  entries alone (each `ALWAYS` Environment's start missing; an enabled entry supplies some, or
  adds an `ON_REQUEST` one's), and the list stops after 50 names, so it is linear in
  the template size.

An entry that names no `jobEnvironments` entry names an external Environment, whose
definitions are unknown until the Job is created, so only the checks above that do not
need them apply. Job creation keeps such an entry, when enabled, without checking its
values (it does re-check constraint 3 and the Step `let` bindings the values read), and its
submission stage resolves it against the attached Environment Templates and checks
constraints 5–7, its resolved format-string values, and PATH absoluteness for it.
For an entry that names a `jobEnvironments` entry, for every literal Job Environment
`default`, and for every non-literal one a Step takes, `create_job` makes those checks (see
[job-creation.md](job-creation.md#environment-parameter-values-env_params)).

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
