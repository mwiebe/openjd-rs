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

Next, both check the `SERVICE` prerequisites (RFC 0009 "Backward compatibility"; §1.2 item 3
for an Environment Template): a document that lists `SERVICE` must also list `EXPR` and
`ENV_PARAMS`. `service::check_prerequisites` reports one that does not at `extensions`,
naming the missing ones — "SERVICE requires EXPR and ENV_PARAMS; ENV_PARAMS must also be
listed in the template's `extensions` (RFC 0009)." (`EXPR and ENV_PARAMS must also be
listed …` when both are missing) — and the function returns with that error alone, plus
pass 10's `WRAP_ACTIONS` prerequisite error when it applies. (A document that also breaks
the `ENV_PARAMS` prerequisite gets both messages.) Every SERVICE rule in the passes below and
at job creation therefore takes `EXPR` and `ENV_PARAMS` as listed wherever `SERVICE` is.

## Pass Architecture

The validation pipeline is passes 5–11 of the overall decode pipeline (passes 1–4 are in
the `parse` module — see [parsing.md](parsing.md)). Passes run sequentially. Each pass
receives the template and the computed limits/rules, and appends errors to a shared
`ValidationErrors` collector. All passes run regardless of earlier errors (no
short-circuiting), so users see all problems at once. The exceptions are the
`ENV_PARAMS` and `SERVICE` prerequisites (see [Entry Points](#entry-points)), checked before pass 5.

| Pass | File | Purpose |
|------|------|---------|
| — | `service.rs` (`gate_services_*`) | **SERVICE gating, first** (RFC 0009): in a template that does not declare `SERVICE`, report `services`, `externalServices`, the `service` key of every Step's `dependencies` entries, and every Environment's `dependencies` (`<field> requires the SERVICE extension.`) without examining their contents. Runs ahead of every other pass so these errors lead the list; an `Undefined variable: 'Service.X.*'` (pass 8) that follows from the missing extension is then a consequence and is left unreported (see pass 8) |
| 5 | `limits.rs` | Enforce numeric limits (name lengths, counts); FEATURE_BUNDLE_1 raises many limits |
| 6 | `structure.rs` | Structural validation (uniqueness, required fields, Step dependencies: each entry exactly one of `dependsOn` / `service`, no self or duplicate entries, `dependsOn` names a Step — with SERVICE, `service` entries and cycles are left to pass 12, and a `dependsOn` that names no Step but a Service of that name gets a `did you mean 'service: <name>'?` hint) |
| 7 | `feature_bundle_1.rs` | Gate FEATURE_BUNDLE_1 features (simple actions, endOfLine) |
| 8 | `format_strings.rs`, then `service_diagnostics.rs` | Validate format string variable references; adapts scopes and expression complexity based on EXPR; with SERVICE, the `Service.*` / `Service.File.*` / `WrappedService.*` scopes and every `<Service>`'s format strings and `let` bindings (RFC 0009). `service_diagnostics.rs` then rewrites the generic undefined-variable message of each out-of-scope `Service.*` / `Task.*` / `Step.*` reference whose Service the document declares or requires into the scope rule it breaks, and, without `SERVICE`, drops references to a Service the (gated) `services` / `externalServices` lists name |
| 9 | `task_chunking.rs` | Gate TASK_CHUNKING features (ChunkInt parameters) |
| 10 | `wrap_actions.rs` | Gate WRAP_ACTIONS features (the three RFC 0008 hooks and, with SERVICE, the four `onWrapService*` hooks), enforce the all-or-nothing rule (without SERVICE) or RFC 0009 wrap hook rule 1 (with SERVICE), and the single-wrap-layer-per-session rule (RFC 0008, RFC 0009) |
| 11 | `env_params.rs` | Gate ENV_PARAMS features (Environment `envParameterDefinitions`, `activation`, `hostRequirements`, and `enableIf`, the `step:` and `environment:` forms of `<StepDependency>`, the latter with its `enableIf`) and check their structural rules (Environment Parameters RFC); with SERVICE, also the `environment:` entries of a Job Template Service's `dependencies` (RFC 0009 `<StepDependency>` item 3) |
| 12 | `service.rs` | With SERVICE declared: validate every `<Service>` and `<ServiceRequirement>` structurally (including that a literal `maxRestarts` greater than 0 comes with `completedTasks`), resolve every `service` dependency and each Service's and each Environment's `dependencies` (with the wrong-key hints), and check the combined Step/Service/Job-Environment dependency graph for cycles and every Service's computed scope (`template::service_scope`) (RFC 0009, Template Schemas §4 item 3, §9). The prerequisites are checked before pass 5 (see [Entry Points](#entry-points)); the gated fields of a template without SERVICE are the gating step's, above |

### Environment template pipeline

`validate_environment_template` runs the same passes with job-template-only checks
omitted (there are no steps, so passes 9's ChunkInt checks and pass 6's step/dependency
checks have nothing to walk):

- **Root shape (§1.2)** — `EnvironmentTemplate: must define at least one of 'environment' or
  'services'.` when both are absent (RFC 0009 made `environment` optional). `$schema` is
  accepted and ignored.
- **Limits + structure** — parameter-definition count/uniqueness (its own cap,
  `max_env_template_param_count`), then `validate_single_environment` for the env body when
  there is one.
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
  With SERVICE (RFC 0009 §1.2.2), the document's `services` are walked first — each in Job
  scope, seeing the Services of the document it lists with the `service` key in its
  `dependencies` — and the environment sees the `port` / `connectAddress` of the Services of
  the document it lists in its own `dependencies` (see "Service scopes" under pass 8). A
  services-only document has no environment body, so only the Services are walked.
- With ENV_PARAMS, the environment's `hostRequirements` get pass 6's structural checks
  (`validate_host_requirements_in_context`) and pass 8 checks them against their own table
  (see [Environment hostRequirements](#environment-hostrequirements-env_params)).
- **Pass 10** — WRAP_ACTIONS gating (see below), on the environment when there is one.
- **Pass 11** — ENV_PARAMS gating and the Environment's `envParameterDefinitions` (see
  below). Pass 8 also checks each format-string `default` against the template's own
  `envParameterDefinitions` (see [Environment parameter scopes](#environment-parameter-scopes-env_params)).
- **Pass 12** — SERVICE: the `services` list, gated
  (`services requires the SERVICE extension.`) and otherwise validated by the same
  `validate_service_list` as a Job Template's, with paths rooted at `services[i]`, plus
  `validate_environment_template_dependencies` (each Service's `dependencies` may name only
  another Service of this document, with the `service` key, since the document has no Steps;
  an `environment:` entry is accepted only when its `enableIf` is the literal `false` and it has
  no `parameters`, so that it can only decline an Environment (§1.2 *services* constraint 5; see
  "Dependencies, environment template" below)); the environment's `dependencies` (each entry `service: <name>`
  naming a Service of this document's `services`, see "Pass 12" below); and, last,
  `environment_template_service_cycle` (§1.2 *services* constraint 4): the graph of the
  Services' own `service` entries plus an edge from each Service whose Sessions enter the
  document's environment (it is `ALWAYS`, or `BY_DEFAULT` with its `enableIf` known to be `true` (§7.6 rule 5)
  and not declined by the Service, and it has no `exclusiveGroup`) to each Service the
  environment lists must be acyclic, reported at `services`. There is no exception for an
  environment that lists the Service.
  `externalServices` is not a property of this document and is rejected at decode.

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

**Step validation:**
- No duplicate step names
- Step name non-empty, no control characters
- Must have `script` or exactly one simple action field (mutually exclusive)
- Dependencies: §3.2 constraint 1 (exactly one of `dependsOn`, `step`, `service`, and
  `environment`; `required`, `enableIf`, and `parameters` only with `environment`). With
  ENV_PARAMS the messages name every form the document's extensions make available: "must
  have exactly one of 'dependsOn', 'step', or 'environment'." (with SERVICE as well, "…
  'dependsOn', 'step', 'service', or 'environment'.") for an entry with none, with ", not
  both 'X' and 'Y'.", ", not all three.", or ", not all four." appended for one with more.
  Without ENV_PARAMS (so without SERVICE, which requires it), for an entry that gives a
  `service` key — which the SERVICE gate also reports — an entry
  with `dependsOn` and `service` reports `a dependency names a Step with dependsOn or a
  Service with service, not both: {dependsOn: A, service: X} (Template Schemas §3.2).`, one
  with neither `a dependency must name a Step with dependsOn or a Service with service; this
  entry gives neither (Template Schemas §3.2).`, and such an entry is no dependency (not
  checked further, not an edge, not a listing — so the scope and listing checks of pass 12
  see the Step as not listing the Service: a `Service.*` reference in it is also reported as
  `does not list service: X`, and a Service only that entry names as `unused`; fixing the
  entry clears all three). Without either extension an entry with none of the keys is
  reported as missing `dependsOn`, an entry with two names only those two ("must have
  exactly one of 'X' or 'Y', not both."), an entry with more gets the all-forms message, and
  the message for a misplaced `required`, `enableIf`, or `parameters` says that
  `environment` requires the extension. These located messages replace the serde "unknown
  field" / "missing field" errors that the old single-field struct produced, so for a
  malformed `dependsOn` entry they intentionally differ from the Python reference, which has
  no ENV_PARAMS or SERVICE support and reports Pydantic's field errors. For `dependsOn`
  entries and their ENV_PARAMS synonym `step` (§3.2 item 2), read through
  `StepDependency::step()` so the two are one form: no self-dependency (`cannot depend on
  itself.`), target must exist (`dependency '<step>' not found.`), no duplicates (a `step`
  and a `dependsOn` entry naming one Step are duplicates: `duplicate dependency 'A'.`; a
  repeated `service` entry is `duplicate dependency 'service: X'.` —
  `service::duplicate_dependency`, with the set keyed on `DependencyTarget`). The
  `environment:` form, and the `step` form's gate, are checked in pass 11; a `service` entry
  names a Service and is resolved by pass 12 (gated without SERVICE, see above); cycle
  detection sees the `dependsOn` and `step` entries as one graph.
  - **The wrong-key hint** (`helpers::dependency_kind_hint`, RFC 0009 §3.2): when a `dependsOn`
    (or `step`) names no Step but a declared or required Service of that name exists, the
    message is `dependency 'Svc' names no Step; did you mean 'service: Svc'?`
    (`service::unknown_step_dependency`); the mirror, from pass 12, when a `service` entry
    names no Service but a Step of that name exists: `dependency 'Prep' names no Service; did
    you mean 'dependsOn: Prep'?`. The same two hints apply to a Service's `dependencies`, and
    the `service` form to an Environment's or an Environment Template Service's `dependsOn`
    entry. No match, no hint (`dependency 'Y' not found.`). An `environment:` entry gets none.
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
  entries)." With `SERVICE`, pass 12 applies the same limit, with the same message, to each
  Service's `dependencies`, of a Job Template and of an Environment Template, at
  `services[k] -> dependencies` (RFC 0009 `<StepDependency>` item 3: a Service's entries
  work exactly as a Step's); `service` entries do not count.
- Host requirements: a Step's, and with ENV_PARAMS a `jobEnvironments` entry's or an
  Environment Template's `environment`'s (§4 item 5; each checked on its own, §3.3.3,
  through `validate_host_requirements_in_context`, shared with `<Service>`):
  amounts/attributes validation, capability name patterns,
  reserved scope checks (reserved scopes: `worker`, `job`, `step`, `task`),
  standard capability value validation — a literal value of a standard attribute must be
  in that attribute's allowed set from `capabilities::standard_attribute_capabilities`
  (`attr.worker.os.family`, `attr.worker.cpu.arch`, and RFC 0009's
  `attr.worker.preemptible` with values `true`/`false`). Expression-free amount `min`/`max` values must
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
  Step, a Service (RFC 0009), a Job Environment, and an Environment Template alike. Literal empties stay errors:
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

**Cycle detection** (without SERVICE only):
- Iterative DFS with tri-state marking (Unvisited/Started/Completed) on the step
  dependency graph; a cycle reports `step dependencies contain a cycle.` at the root. With
  SERVICE the Steps', Services' and Job Environments' `dependencies` form one graph, which pass
  11 checks and whose error names the cycle, so this check is skipped.

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
  checker (see [template-types.md](template-types.md#expression-fields-75)) against the same
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

With `SERVICE` (RFC 0009), the session and task scopes additionally carry the
`Service.<name>.<port>.*` endpoints in scope where the field is defined, and a
`<Service>` has two scopes of its own; see "Service scopes" below.

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

### Service scopes (RFC 0009; Template Schemas §7.3.1 `Service.*` rows, §9 scope list, §9.8, §9 job-creation exclusion, §3.6.2, §4 item 3.2, §4.3.1)

Everything a Service endpoint resolves to is `@fmtstring[host]` — unknown until the scheduler
places the Service — so pass 8 seeds the `Service.*` keys as `Unresolved` placeholders
(`port` as `unresolved[int]`, `bindAddress` and `connectAddress` as `unresolved[string]`,
`Service.File.<name>` as `unresolved[path]`) through the `pub(crate)` seeders in
`job::service_symbols`, which spell the keys exactly as the runtime-facing
`build_service_symbol_table` does. Scope — *which* Services and which values a given field
may see — is decided per call site, and a reference outside its scope is detected as the
crate's ordinary undefined-variable error, exactly as an out-of-scope `WrappedStep.Name` is
under RFC 0008. The same mechanism rejects a reference to a Service or port name that is
neither declared nor required (§9 visibility rule). Nothing about `Service.*` is examined unless the
template declares `SERVICE`; without it pass 12 rejects the lists and pass 8 never walks them.

Visibility follows the declared dependencies (§9 scope list, §9.8 item 2), one rule with no
exceptions: a Step's `script` and its `stepEnvironments` see a Service's `port` /
`connectAddress` — inline or required — only when the Step lists `service: <name>` in its
`dependencies`; a Service sees another only when it lists it the same way; a Job Environment
sees only the inline and required Services it lists in its own `dependencies` (§4 item 3); and
an Environment Template's `environment` sees only the Services of its document it lists. The
seeders take the `template::service_scope::listed_services(dependencies, services)` and
`listed_requirements(dependencies, requirements)` iterators at every site, over the Step's list
for a Step's sites and over the Environment's own list for a Job Environment. Listing a required
Service makes its values visible and puts the lister in the matched Service's scope; an entry
that nothing lists is rejected in pass 12 like an unused inline Service. Pass 8 does not decide a Service's *scope*
(the Steps whose Tasks depend on it, §9.1): `template::service_scope::compute_service_scopes`
computes it from the `dependencies` lists — the Steps', the Services' (rule 2, transitive,
including the Services a Service depends on through a Job Environment its Sessions enter), and
the Job Environments' (rule 3: an Environment listing `X` puts in `X`'s scope every Step whose
Sessions enter it) — for pass 12 (cycles, unused Services, a job-wide Service listing a
Step) and job creation; see [template-types.md](template-types.md), "Service scope".

#### Scope-rule diagnostics (`service_diagnostics.rs`)

The symbol-table mechanism finds every violation but explains none of them: the author sees
`Undefined variable: 'Service.Counter.api.connectAddress'.` — and, when another Service's name
is one edit away, a `Did you mean: Service.Cache.main.port` pointing at the wrong Service
(exploratory report stumbles S2–S4, S7, `w05`). After pass 8 has walked a document,
`service_diagnostics::refine_job_template` / `refine_environment_template` revisit the errors
it added (those from the index `errors.errors` had before the pass) whose message contains
`Undefined variable: '<name>'.` and, when `<name>` is `Service.<svc>.…` with `<svc>` **declared
in the document's `services` or required by its `externalServices`** (or `Task.*` / `Step.*`
inside a Service — its body or its job-creation fields, `let` and `hostRequirements`), replace
that sentence — and the
`Did you mean` suggestion on the same line — with the rule the reference breaks. The path, the
`Failed to parse interpolation expression at [s, e]. ` / `Invalid expression in let binding
'x': ` prefix, and the expression-source and caret lines that follow are untouched, and the
structured `ErrorDetail` summary and span summaries are updated in step. The reference site is
classified from the error path (`Site`: a Step's `script`, a Service body, a Job Environment,
a Step Environment, an Environment Template's
`environment`, or a job-creation field — `hostRequirements`, a `let` outside a `script` (a
`<StepTemplate>`'s or `<Service>`'s; a `<StepScript>`, `<ServiceScript>` or
`<EnvironmentScript>` `let` is a session-scope site), a `parameterSpace` range, `timeout` /
`notifyPeriodInSeconds`, a Service's `port` / `maxRestarts` / the four `<ServiceHealthCheck>`
numeric fields — the path segment after `variables` or `let` is a name the author chose
and never a field, so a variable named `timeout` is a variable); the declarations are the
document's `services`, and a
Job Template's `externalServices` count as known with their declared ports. Rules, in the
order tried:

| Condition | Message |
|---|---|
| `Task.*` / `Step.*` anywhere under `services[k]` | `Task.* is not available within a Service.` / `Step.* is not available within a Service.` (neither is in the §9 list of scopes available within a `<Service>`) |
| job-creation field (§9 job-creation exclusion: every field resolved at job creation, before any Service is placed) | `Service.* is not available in <field>: it is resolved at job creation, before any Service has an endpoint.` (`<field>` is `hostRequirements`, `a let binding` — a `<StepTemplate>`'s or `<Service>`'s, not a script's —, `a parameterSpace range`, `timeout`, `a cancelation method's mode or notifyPeriodInSeconds` (both reported on the `cancelation` path), `notifyPeriodInSeconds`, `port`, `readinessIntervalSeconds`, `readinessTimeoutSeconds`, `healthIntervalSeconds`, `failureThreshold`, or `maxRestarts`). Pass 8 never seeds `Service.*` into these fields' symbol tables (`build_template_scope_symtab`, `validate_action_timing_fs`, the numeric `@fmtstring` checks), so the reference is undefined there whatever the scope; this rule only names the field |
| a declared or required Service referenced from a Step's `script`, one of its `stepEnvironments`, another Service, a Job Environment, or an Environment Template's `environment`, whose `dependencies` (the Step's, for a Step Environment) do not list `service: <svc>` (§9 scope rules 2–5, §9.8 item 2) | `Step 'Render' references Service.Cache.main.port but does not list service: Cache in dependencies.` — from a Step Environment, `Step 'Render' references Service.Cache.main.port in stepEnvironments 'Tools' but does not list service: Cache in dependencies.` — from a Service, `Service 'Front' references Service.Back.main.port but does not list service: Back in dependencies.` — from a Job Environment or an Environment Template's `environment`, `Environment 'CacheClient' references Service.Cache.main.port but does not list service: Cache in dependencies.` |
| the port is not declared | `Service 'Store' has no port 'mian'; declared ports: main.` — or, for a required Service, `required Service 'Cache' has no port 'admin'; declared ports: main.`; when the list is empty (itself reported, `must not be empty.`), `…has no port 'main'; it declares no ports.` |
| `bindAddress` of a required Service | `bindAddress of required Service 'Cache' is not available; use connectAddress to reach it.` |
| `bindAddress` outside the declaring Service | `Service.Proxy.main.bindAddress is available only within the Service 'Proxy' itself; use connectAddress to reach it from elsewhere.` |

When the document does **not** declare `SERVICE`, `refine` instead drops every `Undefined
variable: 'Service.<svc>…'` whose `<svc>` the document's (gated, unexamined) `services` or
`externalServices` names: the gating step's `services requires the SERVICE extension.` is the
finding, and the reference is its consequence (exploratory report S10). A `Service.*` name the
lists do not declare keeps the generic message — nothing else explains it.

Everything else keeps the generic message with its suggestion: a Service name declared nowhere
(a typo is then the likeliest cause — `Undefined variable: 'Service.Cash.main.connectAddress'.
Did you mean: Service.Cache.main.connectAddress`), an unknown value name after a declared port,
`Service.File.*`, or a reference with too few components. The dependency is the author's
statement that the entity needs the Service; a reference alone is not taken as one, whether the
Service is inline or required and whether the entity is a Step, a Service, or an Environment.
The conformance `.invalid` fixtures check only pass/fail and are unaffected;
`tests/integration/test_service_scope.rs`, `test_service_scope_rules.rs` and
`test_service_requirements.rs` pin the exact messages.

Who sees which Services (the `in_scope` iterator at each site):

| Field | Services whose `port` / `connectAddress` are in scope | `bindAddress` |
|---|---|---|
| step `script` (actions, embedded files, `<StepScript>.let`, `<SimpleAction>.let`) — `build_task_scope_symtab` | the `services` entries and the `externalServices` entries the Step lists with the `service` key in `dependencies` (`listed_services`, `listed_requirements`; a requirement's declared ports) | never |
| `jobEnvironments[i]` (variables, actions, embedded files, `<EnvironmentScript>.let`) — `build_session_scope_symtab` | the `services` entries and the `externalServices` entries the Environment itself lists with the `service` key in its `dependencies` (`listed_services`, `listed_requirements` over `env.dependencies`; listing an inline Service puts every Step whose Sessions enter the Environment in its scope, §9.1 rule 3), whatever kind of Session enters it: every Step and Service whose Sessions enter it depends on each Service it lists, so the endpoint is known before it is entered (§4 *dependencies*) | never |
| `steps[i].stepEnvironments[j]` | the `services` and `externalServices` entries its Step lists in `dependencies` (a Step Environment has no list of its own) — a Step Environment is entered only by the Task Sessions of its Step | never |
| `services[k]` body (variables, every action, embedded files, `<ServiceScript>.let`) — `validate_service_format_strings` | the other `services` entries and the `externalServices` entries it lists with the `service` key in `dependencies` (any order; pass 12 rejects a cycle), plus itself | its own only |
| environment template `services[k]` body | the other `services` entries it lists in `dependencies`, plus itself | its own only |
| environment template `environment` | the `services` entries it lists in its own `dependencies` (`listed_services`) | never |
| any `hostRequirements` (Step's or Service's), `<StepTemplate>.let`, `<Service>.let`, parameter-space ranges, every `<Action>`'s `timeout`, a cancelation method's `mode` / `notifyPeriodInSeconds`, the numeric `@fmtstring` fields of `<ServicePort>`, `<ServiceHealthCheck>` and `<ServiceRestartPolicy>` | **none** — job-creation stage; `service_diagnostics` names the field | never |

Consequences the tests pin: list order carries no meaning (a Service may list and reference a
later Service), a reference to an inline or required Service without the matching `service`
dependency is an error naming the fix (from a Step, a Service, or an Environment alike), an
`externalServices` entry nothing lists is a pass 12 error, a dependency cycle is a pass 12 error — including a
Service whose Sessions enter a Job Environment that lists it (`X → X`) — a required Service's `bindAddress` and undeclared ports are errors, and
`Task.*` / `Step.*` are never seeded for a Service (the §9 list of format string scopes available
within a `<Service>` does not include them). There is no Environment that sees a Service's `bindAddress`: a
`<Service>` has no `serviceEnvironments` property (the RFC's Rejected Ideas), so the only
Environments a Service Session enters are the Job's: every `activation: ALWAYS` one and every
`BY_DEFAULT` one it does not decline with a disabled `environment:` entry, plus the `ON_REQUEST`
ones it requests with an `environment:` entry in its own `dependencies`, less any whose `enableIf`
is false for it, exactly as for a Step (RFC 0009 "Services run inside Environments";
`template::Entrances`).

`Service.File.<name>` is seeded for the declaring Service only, from its script's
`embeddedFiles`, into the service-execution scope (and so into `<ServiceScript>.let`, like
`Task.File.*` for `<StepScript>.let`).

**Within a `<Service>`** (`validate_service_format_strings`), two scopes follow the
`@fmtstring` stage annotations:

- *Job-creation scope* — the document's template-scope table (`Param.*` without PATH,
  `RawParam.*`, `Job.Name`, and with ENV_PARAMS `Job.EnvNames`) plus the `<Service>.let`
  bindings, validated with
  `validate_let_bindings` against the template library (no host functions). Used for
  `<Service>.let` itself, `hostRequirements` (through the shared
  `validate_host_requirements_fs`, so the Step and Service checks are one code path with the
  owner's path prefixed), the numeric `@fmtstring` fields, and every action's `timeout` /
  cancelation `mode` / `notifyPeriodInSeconds` (`validate_action_timing_fs`), and — for a Job
  Template's Service — the `parameters` values and `enableIf` of its `environment:`
  dependencies (`validate_env_entry_values`, `validate_enable_if`: the same checks as a Step's,
  against the Service's table, which has no `Step.Name`; RFC 0009 `<Service>` item 4). Never
  `Session.*`, `Service.*`, PATH `Param.*`, `Step.*`, or `Task.*` (§9 item 3 for `let`; the §9 scope list has no `Task.*` or `Step.*`).
- *Service-execution scope* — `Param.*` including PATH, `RawParam.*`, `Session.*`, every
  job-creation-stage symbol above (copied over, `Param`/`RawParam` excepted), this Service's
  `Service.File.*`, its own three endpoint values, the `port` / `connectAddress` of every
  other Service of the document it lists with the `service` key in its `dependencies` — inline,
  or a required Service's declared ports — and the
  `<ServiceScript>.let` bindings (host library; the service-level names are the enclosing
  scope). Used for `variables` (with the §4.4.2
  `max_env_var_value_len` constraint, as for an Environment), every action's `command` /
  `args`, and embedded-file `data`. Without EXPR, complex expressions in any of these are
  rejected with `complex expressions require the EXPR extension.`; `let` in either position
  is rejected with `'let' requires the EXPR extension.`; comprehension loop variables may not
  shadow any `let` name in scope.

**The `WrappedService.*` group** (§4.3.1) is added to the wrap-hook symbol table for exactly the
four `onWrapService*` hooks (`WrapHookScope::Service` → `add_wrapped_service_scope`):
`WrappedService.Name` (`string`), `WrappedService.PortNames` (`list[string]`),
`WrappedService.Ports` (`list[int]`), `WrappedService.BindAddresses` (`list[string]`),
`WrappedService.Protocols` (`list[string]`, each `TCP` or `UDP`). It is
not available in `onWrapEnvEnter` / `onWrapTaskRun` / `onWrapEnvExit`, and `WrappedEnv.Name` /
`WrappedStep.Name` are not available in the Service hooks.

**Numeric `@fmtstring` fields** (§9.5.1; `SERVICE_PORT_CONSTRAINT`,
`SERVICE_SECONDS_CONSTRAINT`, `SERVICE_MAX_RESTARTS_CONSTRAINT`) join the resolved-value
constraint table below with target type `int?`: `port` 1–65535 (`must be between 1 and
65535.`), the four `<ServiceHealthCheck>` numeric fields > 0 (`must be > 0.`), `maxRestarts` ≥
0 (`must be >= 0.`), non-integers `must be an integer.`, a whole-field `null` accepted as "not
provided" (for a `STDOUT` check's `healthIntervalSeconds`: no heartbeat). The messages match pass 12's checks on the literal forms and job creation's on the
resolved values.

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
| Service `port` (SERVICE, Template Schemas §9.3) | `int?` | soft cap: 100 chars | coerced integer in 1–65535; `null` = runtime allocates |
| Service `readinessIntervalSeconds` / `readinessTimeoutSeconds` / `healthIntervalSeconds` / `failureThreshold` (SERVICE, §9.3) | `int?` | soft cap: 100 chars | coerced integer > 0; `null` = §9.3 default (STDOUT `healthIntervalSeconds`: no heartbeat) |
| Service `maxRestarts` (SERVICE, §9.4) | `int?` | soft cap: 100 chars | coerced integer ≥ 0; `null` = 0 |
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

Validates or rejects features gated behind `WRAP_ACTIONS` (RFC 0008), including the RFC 0009
Service hooks (Template Schemas §4.3 "WRAP_ACTIONS extension constraints"):

- **Wrap hooks** (`onWrapEnvEnter`, `onWrapTaskRun`, `onWrapEnvExit`): Rejected on any
  environment when the extension is not declared: `<hook> requires the WRAP_ACTIONS
  extension.` on the hook's path.
- **Service hooks** (`onWrapServiceEnter`, `onWrapServiceRun`, `onWrapServiceHealthCheck`,
  `onWrapServiceExit`; RFC 0009): require both `WRAP_ACTIONS` and `SERVICE`. The message on
  the hook's path names what is missing: `<hook> requires the WRAP_ACTIONS extension.`,
  `<hook> requires the SERVICE extension.`, or `<hook> requires the WRAP_ACTIONS and SERVICE
  extensions.`
- **EXPR prerequisite**: `WRAP_ACTIONS` requires `EXPR` to also be declared (the wrap
  mechanism forwards inner-action bytes through the EXPR function library). Declaring
  `WRAP_ACTIONS` without `EXPR` is an error.
- **All-or-nothing rule** (constraint 1; `WRAP_ACTIONS` without `SERVICE`): an environment that
  defines any one of the three RFC 0008 wrap hooks must define all three. Defining a partial
  set is an error at the `actions` path: `an environment that defines any of onWrapEnvEnter,
  onWrapTaskRun, or onWrapEnvExit must define all three (RFC 0008).` The Service hooks do not
  take part in this count (each has already been rejected for lacking `SERVICE`).
- **Wrap hook rule 1** (RFC 0009; `WRAP_ACTIONS` with `SERVICE`, replacing the
  all-or-nothing rule): a *wrapping* environment — one that defines any wrap hook
  (`has_any_wrap_hook`) — must define the hook for every kind of action a kind of Session
  entering it runs inside it: `onWrapEnvEnter`, `onWrapTaskRun`, and `onWrapEnvExit` if any Task
  Session enters it, and `onWrapEnvEnter`, `onWrapEnvExit`, and the four `onWrapService*` hooks if
  any Service Session enters it (`wrap_actions::missing_wrap_hooks`). A hook for a kind of action
  that never runs inside it is permitted, so a `stepEnvironments` wrapper — entered only by its
  Step's Task Sessions — needs the three Task hooks and may also define the Service hooks. The
  rule is checked per kind of Session, here only for a kind known to enter the wrapper: for a
  Job Environment, the first Step and the first Service that `template::Entrances::of` reports
  as certainly entering it (`Entrance::Enters`: `ALWAYS`, `BY_DEFAULT` with `enableIf` known to be `true` (§7.6 rule 5)
  and no entry of the entity or an enabled one (a disabled entry declines it), or
  `ON_REQUEST` requested by an entry whose `enableIf` is known to be `true`, in each case with no
  `exclusiveGroup`; the same rule for Steps and Services, so a Service entering one that lists
  it is also the cycle `X → X` pass 12 reports); for a Step Environment, its Step when the
  Environment's `enableIf` is known to be `true`. Each kind with missing hooks is one error at
  the `actions` path: `the Sessions of Step 'Render' enter this wrapping environment, which
  therefore must define onWrapEnvEnter, onWrapTaskRun, and onWrapEnvExit; missing:
  onWrapTaskRun (RFC 0009 wrap hook rule 1).` (`the Sessions of Service 'Cache' …` with the six
  Service-Session hooks). A wrapper no Session is yet known to enter — including an Environment
  Template's `environment`, which is never rejected here for lacking Service hooks — is checked
  at job creation over the Sessions that enter it in the combined Job (see "Submission-time
  checks" below).

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
  `ON_REQUEST` wrap envs requested by different steps are valid. A Service Session's stack is
  the Job Environments it enters (RFC 0009 "Services run inside Environments"), so the rule
  is checked per Service too, over the Environments `Entrances` reports it certainly
  enters, and reported at `services[k]` with the same message; job creation checks each
  Service's final set (`job::Service::environments`).

The single-layer rule runs only in the job-template path. An environment template defines
at most one environment, so the rule is trivially satisfied for an isolated env template;
`check_environment_templates` checks it for each step whose sessions enter a wrapping
attached environment, over the environments the step certainly enters; job creation
checks it again over each step's set with the attached environment templates; and
the session runtime rejects a second wrap environment at enter time for environments
composed any other way.

Likewise the §1.2.2 item 4
rule — a wrapping Environment that a Service with a non-empty scope enters defines the
`onWrapService*` hooks — relates documents only the scheduler sees
together and is not a template-validation check: it is one of the submission-time checks
the submission stage of `create_job_with_environment_templates` runs on the combined Job (see
"Submission-time checks" below
and [job-creation.md](job-creation.md)).

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

With `SERVICE` (RFC 0009 `<StepDependency>` item 3, `<Service>` item 4), each `<Service>` of a
Job Template's `services` requests Environments as a Step does: its `environment:` entries get
every check above (`check_requester_entries`, at `services[k] -> dependencies[j]`), except
constraint 2, since a Service has no `stepEnvironments`; their values and `enableIf` are checked
in pass 8 against the Service's job-creation scope (no `Step.Name`). Constraint 7 holds for
every Environment a Service's Sessions enter: every `ALWAYS` `jobEnvironments` entry and every
`BY_DEFAULT` one the Service does not decline, and every `ON_REQUEST` one it requests; the
missing parameters are reported once per Service, at `services[k]`, with the reason "every
jobEnvironments entry with activation ALWAYS, each one with activation BY_DEFAULT that the Service
does not decline, and each one with activation ON_REQUEST that the Service requests; Template
Schemas §3.2 constraint 7, RFC 0009 <Service> item 4" and the
disabled-entry note naming "the Service's 'environment:' dependency". A Service whose Sessions
enter an Environment that lists it is the cycle `X -> X` (pass 12). No `<Environment>.dependencies`
list may hold an `environment:` entry, and an Environment Template's Services may hold only a
literal `enableIf: false` one without `parameters`; pass 12 rejects any other.

## Pass 12: SERVICE Gating and Structure

Validates or rejects features gated behind `SERVICE` (RFC 0009, Template Schemas §1.1 items
8–9, §1.2, §4 item 3, §9–§9.8). Error paths are `services[i] -> …`, `externalServices[i] ->
…`, and, in an environment template, `services[i] -> …`.

**Gating (extension not declared)** — `gate_services_job_template` /
`gate_services_environment_template`, run **before pass 5** so these errors lead the list
(exploratory report S10):
- `services` → `services requires the SERVICE extension.` (job and environment templates);
  `externalServices` → `externalServices requires the SERVICE extension.` The list's contents
  are not examined.
- `dependencies` on any `<Environment>` (`jobEnvironments[i]`, `steps[i] -> stepEnvironments[j]`,
  or the environment template's `environment`) → `dependencies requires
  the SERVICE extension.` on the `dependencies` path. The list's contents are not examined,
  and neither is the Step Environment prohibition below (one error, not two).
- The `service` key of a Step's `dependencies` entry (§3.2 item 2) → `service requires the
  SERVICE extension.` on `steps[i] -> dependencies[j] -> service`. The name is not resolved
  (conformance `3.2--service-key-without-service-extension.invalid`). A plain template keeps
  `dependsOn` only.

What follows from the missing extension is a consequence, not a finding, and is left
unreported: `service_diagnostics` drops every `Undefined variable: 'Service.<svc>…'` whose
`<svc>` one of the gated lists names. A template with `services:` and a Step listing `service:
Cache` and referencing `Service.Cache.*`, but no `extensions:`, therefore reports exactly the
`services` and `service` gating errors (plus anything unrelated, such as an undefined `Param.*`).

The pre-RFC-0009 keys `jobServices` (job template root) and `stepServices` (`<StepTemplate>`)
are not properties and are rejected at decode as unknown fields — with their model path and a
rename hint, see [parsing.md](parsing.md) "Pass 3" — as is `externalServices` on an environment
template (§9.1 rule 4). So is `runScope` on any `<Environment>`, with or without `SERVICE`:
RFC 0009 removed it, and the decode error carries the hint `'runScope' is not a property; …`
(see [parsing.md](parsing.md)).

**Prerequisites:** `SERVICE` requires `EXPR` and `ENV_PARAMS`, checked before pass 5 (see
[Entry Points](#entry-points)); a document that breaks the rule gets no other error.

**With the extension declared:**
- **Lists** (§1.1 items 8–9, §1.2): each `services` / `externalServices` list `must not be
  empty.` and `must not contain more than 10 elements.`
- **An Environment's `dependencies`** (§4 item 3 constraint 2;
  `validate_environment_dependencies`), on every `<Environment>` of either template kind:
  - On a `stepEnvironments` entry the list is rejected whole, whatever it names, at
    `steps[i] -> stepEnvironments[j] -> dependencies`: `a Step Environment follows its Step's
    dependencies and must not give a dependencies list of its own (Template Schemas §4 item 3
    constraint 4).` Nothing inside is examined.
  - On a `jobEnvironments` entry or an Environment Template's `environment`: `dependencies`
    `must not be empty.`; on `dependencies[j]`, a malformed entry (both keys or neither) is
    reported as in pass 6, a `dependsOn` entry reports `dependency 'dependsOn: Prepare' names a
    Step, but an Environment is entered by Sessions, not scheduled; an Environment may depend
    only on a Service, as 'service: <name>'.` (with `; did you mean 'service: Prepare'?` in
    place of the period when a Service of that name exists), an unknown `service` name reports
    `dependency 'service: Nope' not found: no Service of that name in services or
    externalServices.` (Job Template) or `… in this document's services.` (Environment
    Template, whose `environment` may list only a Service of the same document — so a document
    with no `services` can list nothing), and a repeat reports `duplicate dependency 'service:
    X'.` (an unknown duplicate gets both). An `environment:` entry reports `dependency
    'environment: <name>' names an Environment; an Environment does not request another
    Environment (RFC 0009 <StepDependency> item 3).`, and its `required` / `enableIf` /
    `parameters` keys are part of the same error.
  - A Job Environment's entries join the dependency graph as an edge from each Step and each
    Service whose Sessions enter it to each Service listed (§3.2 constraint 3, §9.1 rule 3);
    see "Dependencies" below.
- **Name uniqueness** : `duplicate service name: '<name>'` on the offending
  `services` element for a repeat within the list; `duplicate service requirement name:
  '<name>'` on the offending `externalServices` element; and, on `externalServices[i] ->
  name`, `'<name>' is also declared in services; a Service is either declared or required, not
  both.`
- **`<ServiceName>`, requirement names, and port names** (§9.2, §9.3 item 1, §9.8): on the `name` field, `'<name>' is not a valid identifier.`, `exceeds
  <max_identifier_len> characters.` (64, or 512 with FEATURE_BUNDLE_1), and `must not be
  'File'; it is reserved for Service.File.* references.`
- **Dependencies** (§3.2 constraints 1–3, §9 item 4, §9.1, §3.2 constraint 3;
  `validate_job_template_dependencies`), job template:
  - A Step's `service` entries (the one-of rule, its `dependsOn` entries, self-dependency and
    duplicates are pass 6's): on `steps[i] -> dependencies[j]`, an unknown name reports
    `dependency 'service: Nope' not found: no Service of that name in services or
    externalServices.` — or, when a Step of that name exists, `dependency 'Prep' names no
    Service; did you mean 'dependsOn: Prep'?`
  - A Service's `dependencies`, checked wholly here: `services[k] -> dependencies` `must not be
    empty.`; on `services[k] -> dependencies[j]`, a malformed entry as in pass 6, `dependency
    '<step>' not found.` for an unknown Step (`dependency 'Back' names no Step; did you mean
    'service: Back'?` when a Service of that name exists), `cannot depend on itself.` for
    `service: <own name>`, the unknown-Service message above for an unknown `service` name, and
    `duplicate dependency '…'.` Its `environment:` entries are permitted (RFC 0009
    `<StepDependency>` item 3) and are pass 11's, which also reports a repeated one.
  - The combined graph (Step→Step, Step→Service, Service→Step, Service→Service edges, plus an
    edge from each Step and each Service whose Sessions enter a Job Environment to each Service
    that Environment lists; `compute_service_scopes`) must be acyclic. The first cycle is
    reported at the root path, each node spelled `Step X` or `Service X`: `dependencies contain
    a cycle: Step Use -> Service Indexer -> Step Use.`; when entering a Job Environment closed
    it, the message names the entrance: `dependencies contain a cycle: Step Prep -> Service X ->
    Step Prep (the Sessions of Step Prep enter Job Environment 'Client', which lists Service 'X',
    so Step Prep depends on it; Template Schemas §3.2 constraint 3).` A Service whose
    Sessions enter an Environment that lists it, by its `activation` or through its own
    `environment:` entry, is the cycle `X → X`; there is no exception for it: `dependencies
    contain a cycle: Service X -> Service X (the Sessions of Service X enter Job Environment 'E',
    which lists Service 'X', so it would wait for itself to be READY. There is no exception for
    an Environment that lists the Service: keep the Service's Sessions out of it: drop any
    'environment:' dependency of the Service that requests it, and decline a BY_DEFAULT one
    with a disabled 'environment:' dependency of the Service, or make it ON_REQUEST and
    request it only from the Steps that need it; Template Schemas §3.2 constraint
    3).` A cycle is reported here only when every edge on it is known — every `enableIf` it
    depends on is known (§7.6 rule 5: absent, a boolean scalar, or an expression whose value
    is known) and no Environment on it has an `exclusiveGroup` (`template::Entrance::Enters`);
    one that an `enableIf` not known or a group may close is
    checked at job creation over the resolved sets
    (`job::create_job::composition`), as is a cycle through an attached Environment Template.
    A `service` entry naming a required Service is not an edge. When there is a cycle the
    scopes are undefined and the check below is skipped. List order carries no meaning.
  - An unused Service — empty computed scope (§9.1 rule 4: "a Service in whose scope no Step
    falls"; a Service listed only by Services that are themselves unused is unused) — reports
    on `services[k]` (`unused_service_message`): when nothing lists it, `Service 'Cache' is
    unused: no Step, Service, or Job Environment lists it.`; when only other — unused —
    Services list it, they are named: `Service 'Backend' is unused: no Step is in its scope
    (Service 'Proxy' lists it, but no Step is in Proxy's scope either).` / `… (Services 'Side'
    and 'Car' list it, but no Step is in their scope either).` (exploratory report S4: the
    earlier wording claimed nothing listed it). A Service listed only by Job Environments no
    Step's Sessions enter (an `ON_REQUEST` one no Step requests, or one every Step's `enableIf`
    excludes) reports `Service 'X' is unused: no Step is in its scope (Job Environment 'E' lists
    it, but no Step's Sessions enter it).` (`…, and no Step is in the scope of Service 'P'` when
    unused Services list it too). Where the scope depends on an `enableIf` not known (§7.6 rule 5), only a
    Service no Step can be in the scope of is rejected here
    (`ComputedServiceScope::possibly_used`); job creation decides the rest. A Job Environment
    that *references* the Service without listing it does not keep it used (and is itself a
    pass 8 error).
- **Dependencies, environment template** (§1.2 item 6, §3.2 constraint 4, §9 item 4;
  `validate_environment_template_dependencies`): `services[k] -> dependencies` `must not be
  empty.`; on `services[k] -> dependencies[j]`, a malformed entry as in pass 6, a `dependsOn`
  entry reports `dependency 'dependsOn: Prepare' names a Step, but an Environment Template has
  no Steps; a Service here may depend only on a Service of the same document, as 'service:
  <name>'.` (with the `did you mean 'service: Prepare'?` hint when a Service of that name
  exists), an unknown name `dependency 'service: Nope' not found: no Service of that name in
  this document's services.`, plus `cannot depend on itself.` and `duplicate dependency
  '…'.` (also for two `environment:` entries naming one Environment: `duplicate environment
  dependency '<name>'.`). An `environment:` entry may only decline (§1.2 *services* constraint
  5): one whose `enableIf` is not the literal `false` reports `dependency 'environment: <name>'
  must be disabled: a Service in an Environment Template may have only 'environment:' entries
  whose enableIf is the literal false and that have no parameters, which decline the
  Environment. The document's author writes its environment too, and can give it the values
  directly (Template Schemas §1.2 services constraint 5).`; `parameters` on a disabled one
  reports at `-> parameters` `is not permitted on an 'environment:' entry of a Service in an
  Environment Template, which may only decline the Environment (Template Schemas §1.2 services
  constraint 5).`; and one that names the document's own `ALWAYS` environment reports at
  `-> enableIf` the same "no Service can leave out an ALWAYS Environment" error as a Job
  Template's Service. Its name is checked as every Environment name is. Any other name is
  resolved at job creation and by `check`: among the Environments of the Environment Templates
  attached before this one, where a name that two or more define refers to each of them (as
  for a Step, Template Schemas §3.2.1 item 3), a match whose `activation` is `ALWAYS` is an
  error (except one from a document without `ENV_PARAMS`, on which the entry has no effect),
  and no match has no effect. A cycle among the `service` entries and the edges
  the document's `environment` adds (`environment_template_service_cycle`, see the
  environment-template pipeline above) reports `dependencies contain a cycle: Service A ->
  Service B -> Service A.` at `services`. Scope is
  not checked here: an attached Service's scope is computed over the combined Job at
  submission, and one nothing lists is never started rather than rejected.
- **Requirement ports** (§9.8 item 2): `externalServices[i] -> ports` `must not be empty.` /
  `must not contain more than 10 elements.`; `duplicate port name '<name>'.` on the element;
  the identifier rules above on `ports[j] -> name`. `protocol` is the same serde enum as a
  `<ServicePort>`'s.
- **Ports** (§9 item 6): `ports` `must not be empty.` / `must not contain more than 10
  elements.`; `duplicate port name '<name>'.` on the element.
- **Port numbers per protocol** (§9 item 6.4): on `ports[i] -> port` of the later
  port, `<TCP|UDP> port <n> is also used by port '<earlier>'; two ports with the same protocol
  must not have the same port number.` Only literal numbers are compared here (one that
  carries an expression is compared at job creation, once resolved — see
  [job-creation.md](job-creation.md)). The same number on a TCP port and a UDP port is
  allowed. `protocol` itself (`TCP` | `UDP`, default `TCP`) is a serde enum, so `udp`, `SCTP`,
  or a format string is `unknown variant`.
- **Numeric `@fmtstring` fields**, checked on the field path when the value carries no
  expression (a format string is type-checked and, when static, range-checked by pass 8, and
  resolved and range-checked at job creation, like `<Action>.timeout`):
  `port` `must be between 1 and 65535.`; each of `healthCheck.readinessIntervalSeconds`,
  `healthCheck.readinessTimeoutSeconds`, `healthCheck.healthIntervalSeconds`, and
  `healthCheck.failureThreshold` `must be > 0.`; `restartPolicy.maxRestarts` `must be >= 0.`;
  any of them `must be an integer.` when the text does not parse.
- **`completedTasks` required** (§9.5 item 2): a literal `maxRestarts` greater
  than 0 without `completedTasks` reports, on `restartPolicy`, `completedTasks must be provided
  when maxRestarts is greater than 0 (maxRestarts is 2): a template that allows relaunch must
  say what a relaunch means for completed Tasks, KEEP or RERUN (Template Schemas §9.5 item 2).`
  (`service::completed_tasks_required_message`). `maxRestarts` 0, explicit or defaulted, needs
  none; a format-string `maxRestarts` is checked with the same message when it is resolved at
  job creation (see [job-creation.md](job-creation.md)).
- **Health-check consistency** (§9.4 items 3 and 6, §9.7 item 3):
  `script -> actions`: `onHealthCheck must be defined when healthCheck.type is COMMAND.`;
  `script -> actions -> onHealthCheck`: `onHealthCheck must not be defined when
  healthCheck.type is <TCP_CONNECT|STDOUT>.` (the default type is `TCP_CONNECT`). A
  `TCP_CONNECT` `ports` list `if provided, must not be empty.` and each entry that is not a
  declared port reports `references undeclared port '<name>'; declared ports: main, metrics.`
  on `healthCheck -> ports[k]`.
  A `STDOUT` check that gives `failureThreshold` without `healthIntervalSeconds` reports, on
  `healthCheck -> failureThreshold`, `a STDOUT health check gives failureThreshold only
  together with healthIntervalSeconds; without a heartbeat interval there is no probe for it to
  count.` (a format-string `failureThreshold` counts as given). A `STDOUT` check that gives
  `readinessIntervalSeconds` is rejected at decode as an unknown field, since the `STDOUT` form
  has no such property (as `ports` is on the other two forms).
- **Health check and port protocols** (§9 item 7, §9.4 item 2): a `TCP_CONNECT`
  `ports` entry naming a UDP port reports, on `healthCheck -> ports[k]`, `port '<name>' has
  protocol UDP and cannot be probed by a TCP_CONNECT health check; only TCP ports may be
  named.` A Service none of whose ports is TCP whose effective check is `TCP_CONNECT` reports,
  on `healthCheck`, `the default TCP_CONNECT health check has no TCP port to probe: none of
  the Service's ports has protocol TCP, so a healthCheck of type STDOUT or COMMAND is
  required.` when `healthCheck` is omitted, or `a TCP_CONNECT health check has no TCP port to
  probe: …` (same tail) when one of type `TCP_CONNECT` is given. (Not reported when `ports` is
  empty, which is already an error.) A Service with at least one TCP port may omit the check
  whatever else it declares: the default probes its TCP ports only.
- **Reused validators:** `description` (`validate_description`), `variables`
  (`validate_variables`, shared with `<Environment>`), `hostRequirements`
  (`validate_host_requirements_in_context`, shared with `<StepTemplate>`), every defined
  action (`validate_action`), and `script.embeddedFiles` (`must not be empty.` plus
  `validate_embedded_files` and the identifier/filename length limits).

The discriminator of `healthCheck` and the `completedTasks` enum are enforced by serde
(`unknown variant`), as is the presence of `ports`, `script`, and `onRun`. The pre-revision
spellings — `readinessCheck`, `onReadinessCheck`, `timeoutSeconds`, `intervalSeconds` — are
unknown fields (conformance fixtures `9.4--health-old-readiness-check-key.invalid` and
`9.4--health-old-timeout-seconds-key.invalid`).

Not in this pass: `Service.*` scope rules (§9 job-creation exclusion; pass 8, "Service scopes"), the
wrap hooks the Sessions entering an Environment call for (wrap hook rule 1; pass 10), the
`environment:` entries of a Service's `dependencies` (pass 11), `let` bindings and format
strings inside a Service (pass 8), job creation of Services (see
[job-creation.md](job-creation.md)), and the submission-time checks (next section).

## Submission-time checks (RFC 0009, Template Schemas §1.2.2 items 2–4, §9.9 last two rows)

Some of the SERVICE extension's rules relate documents that only the scheduler sees together
— a Job Template and the Environment Templates attached to a submission — and so cannot run
when a template is validated on its own (the cycles through an attached Environment or
Service, or an `enableIf` not known (§7.6 rule 5) or an `exclusiveGroup`, are among them; see
[job-creation.md](job-creation.md)). They run first in the submission stage of
`create_job_with_environment_templates` (`job/create_job/external.rs`), against the combined
Job, after job creation proper and before any external Service is instantiated (`create_job`,
for the Job Template alone, skips them); every violation is collected into one `ModelValidation`
error whose model name is `Submission` and whose paths are rooted at the document
(`JobTemplate`, `EnvironmentTemplate[i]` by attachment index, or the label the caller gave
the attachment):

- **Requirement matching (§1.2.2 item 2).** Each entry of the Job Template's
  `externalServices` must match exactly one attached Service with the same `name`, which must
  declare every listed port with the same `protocol`. Reported at `JobTemplate ->
  externalServices[i]` as `required Service '<r>' is not provided: no Environment Template is
  attached …` / `… none of the attached Environment Templates (<docs>) defines a Service named
  '<r>' …`, `required Service '<r>' is ambiguous: <n> attached Environment Templates define a
  Service with that name (<docs>); a requirement must match exactly one …`, `required Service
  '<r>' is provided by <doc>, which is missing port '<p>'; its ports: <names> …`, or
  `required Service '<r>' is provided by <doc>, whose port '<p>' has protocol <P> but the
  requirement declares <Q> …`, each ending `(Template Schemas §1.2.2 item 2).` Matching uses
  the requirement alone; a match is recorded as a `RequirementBinding`.
- **Service names are per document (§1.2.2 item 3) — not a check.** An external
  Service's `name` may equal that of another external Service or of a Service in the Job
  Template's `services`; the submission is not rejected for it unless a requirement names the
  duplicated external name (the ambiguity above). The Job Template's `Service.*` references
  resolve to its own Services first (pass 8 seeds the document's own Services and its
  requirements), and the merge keeps same-named Services distinct by stamping each with its
  `job::Document` (see "Services from Environment Templates" in [job-creation.md](job-creation.md)).
  Within one document the §9.2 / §1.1 item 9.4 uniqueness check (pass 12) applies as before.
- **Wrap hook rule 1 over the combined Job (§1.2.2 item 4; `check_wrap_hooks`).** Once every
  entity's Environments are known, a wrapping Environment of the combined `jobEnvironments`
  (inline or attached) that the Sessions of a Service with a non-empty scope enter
  (`job::Service::enters_environment`) must define `onWrapEnvEnter`, `onWrapEnvExit`, and the
  four `onWrapService*` hooks; and one from a document that declares `SERVICE` that a Task
  Session enters — or a `stepEnvironments` wrapper whose `enableIf` was not a literal `true` —
  must define `onWrapEnvEnter`, `onWrapTaskRun`, and `onWrapEnvExit` (without `SERVICE`, RFC
  0008's all-three rule was checked at template validation). Reported at `<doc> -> environment
  -> script -> actions` or `JobTemplate -> jobEnvironments[i] -> script -> actions`: `the
  Sessions of Service 'Cache' enter wrapping Environment 'Wrap', defined by <doc>, which
  therefore must define onWrapEnvEnter, onWrapEnvExit, onWrapServiceEnter, onWrapServiceRun,
  onWrapServiceHealthCheck, and onWrapServiceExit; missing: <hooks> (RFC 0009 wrap hook rule 1,
  Template Schemas §1.2.2 item 4).` (an attached Service is named `Service 'Cache' (<doc>)`;
  the Task form names `Step 'S'` and ends `(RFC 0009 wrap hook rule 1).`). When the defining
  document does not declare `SERVICE`, so cannot define the hooks, the message adds: `Its
  document, <doc>, does not declare the SERVICE extension, so it cannot define the
  onWrapService* hooks: declare SERVICE and define them, or declare ENV_PARAMS and make the
  Environment ON_REQUEST, so that only the Steps that request it enter it.` (`<doc>` is `the Job
  Template` or the attachment's name, as written.) An Environment Template on its own is never
  rejected for lacking Service hooks; it is rejected here only when a Service with a non-empty
  scope enters it. The Service Sessions' single-layer rule is checked here too, over each
  Service's final Environment set, reported at the Service (`JobTemplate -> services[i]` or
  `<doc> -> services[i]`) as `Service 'X': its Sessions would enter …` (`Service 'X' (<doc>)`
  for an attached one).

The 10-element cap on `services` (pass 12) is per document; the combined list is not capped.
Everything else in §9.9 is checked per document by the passes above.

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
| `STANDARD_ATTRIBUTE_CAPABILITIES` | `attr.worker.os.family`, `attr.worker.cpu.arch`, `attr.worker.preemptible` (RFC 0009) |
| `RESERVED_SCOPES` | `worker`, `job`, `step`, `task` |

Note: Standard capability names include their `amount.` or `attr.` prefix.

### Utility Functions

- `has_control_chars(s)` — True if string contains control chars other than `\n`, `\r`, `\t`
- `check_capability_reserved_scope(name, standard, path, errors)` — Errors if non-standard
  capability uses a reserved scope
- `validate_env_var_name(name, path, errors)` — Non-empty, ≤256 chars, no leading digit,
  alphanumeric+underscore only
