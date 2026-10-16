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

## Pass Architecture

The validation pipeline is passes 5–11 of the overall decode pipeline (passes 1–4 are in
the `parse` module — see [parsing.md](parsing.md)). Passes run sequentially. Each pass
receives the template and the computed limits/rules, and appends errors to a shared
`ValidationErrors` collector. All passes run regardless of earlier errors (no
short-circuiting), so users see all problems at once.

| Pass | File | Purpose |
|------|------|---------|
| 5 | `limits.rs` | Enforce numeric limits (name lengths, counts); FEATURE_BUNDLE_1 raises many limits |
| 6 | `structure.rs` | Structural validation (uniqueness, required fields, dependencies) |
| 7 | `feature_bundle_1.rs` | Gate FEATURE_BUNDLE_1 features (simple actions, endOfLine) |
| 8 | `format_strings.rs` | Validate format string variable references; adapts scopes and expression complexity based on EXPR |
| 9 | `task_chunking.rs` | Gate TASK_CHUNKING features (ChunkInt parameters) |
| 10 | `wrap_actions.rs` | Gate WRAP_ACTIONS features (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit) and enforce the single-wrap-layer-per-session rule (RFC 0008) |
| 11 | `control_flow.rs` | Gate CONTROL_FLOW features (`enableIf`, `outputVariables`) and enforce their structural rules (RFC 0010) |

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
- **Pass 10** — WRAP_ACTIONS gating (see below).
- **Pass 11** — `CONTROL_FLOW` applies to Job Templates only: listing it in an
  Environment Template, or giving the environment an `enableIf`, is an error
  whatever the extensions (see below).

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
- Dependencies: no self-dependency, target must exist, no duplicates
- Host requirements: amounts/attributes validation, capability name patterns,
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
   `Session.HasPathMappingRules`, `Session.PathMappingRulesFile`, `Env.File.*`.
   With EXPR: adds `Job.Name` in all environments and `Step.Name` in step
   environments only. Used both for environments inside a job template and for
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

### The `Steps.*` Scope and `enableIf` (CONTROL_FLOW, RFC 0010)

With `CONTROL_FLOW`, pass 8 implements items 3 to 7 of the RFC's §Validation, "At
template validation". The shared code is `validate_v2023_09/steps_scope.rs`, which job
creation also uses (see `specs/model/job-creation.md`).

**Seeding.** `add_steps_scope(symtab, jt, step_index, scope)` adds, for each Step `P` that
Step `S` lists in its `dependencies`, that is not `S`, and whose name is an
`<Identifier>`, the symbols `Steps.P.Output.<V>` and `Steps.P.RawOutput.<V>` for each of
`P`'s output variables and, when `P` has a `parameterSpace`, `Steps.P.TaskParam.<N>` and
`Steps.P.TaskRawParam.<N>` for each Task parameter. Each is `ExprValue::unresolved(T)`,
typed by the RFC's table:

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
type optional, because the RFC's table is keyed on the field being present. The `Raw`
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
| script `timeout`, `notifyPeriodInSeconds`, deferred `mode` | TEMPLATE |
| script `let`, `onRun` `command`/`args`, embedded `data`, and the SimpleAction fields | host |

The job name and every field of `jobEnvironments` (including their `enableIf`) are never
seeded. Environment Templates are not either; pass 11 rejects `CONTROL_FLOW` in them.

**Reference messages.** Before a field is evaluated, `steps_ref_errors` scans the symbols
it reads (`FormatString::accessed_symbols`, or a `let` binding's
`ParsedExpression::accessed_symbols`) for the `Steps` root, and gives each unavailable
reference its own message instead of EXPR's `Undefined variable`. A field with any such
reference is reported with those messages only and is not evaluated. Because the scan is
static, a reference in a branch that is never taken is caught too. In a `let` binding the
message is prefixed `Invalid expression in let binding '<name>': ` and the binding is
bound `unresolved(ANY)`, so later uses of it don't cascade. The checks, in order, with
the first that applies reported:

| Case | Message |
|---|---|
| A field that allows no `Steps.*` (job name, `jobEnvironments`) | `'<sym>': Steps.* symbols are not available in this field. Only a Step's enableIf, let, hostRequirements, parameterSpace, stepEnvironments, and script may read them.` |
| Fewer than four parts (`Steps.P.Output`) | `'<sym>' is not a complete Steps.* reference; expected Steps.<StepName>.Output.<Name>, .RawOutput.<Name>, .TaskParam.<Name>, or .TaskRawParam.<Name>.` |
| No such Step | `'<sym>': there is no Step named 'P'.` |
| The Step's name is not an `<Identifier>` | `'<sym>': Step 'P' cannot be referenced through Steps.*, because its name is not an <Identifier>.` |
| Self-reference | `'<sym>': a Step cannot read its own Steps.* symbols.` |
| Not a direct dependency (a transitive one is not enough) | `'<sym>': Step 'P' is not a direct dependency of Step 'S'. Add 'dependsOn: P' to its dependencies to read Steps.P.*.` |
| Unknown member | `'<sym>': 'X' is not a member of Steps.P; expected Output, RawOutput, TaskParam, TaskRawParam.` |
| No such output | `'<sym>': Step 'P' has no output variable 'V'.` |
| `TaskParam` of a Step without a `parameterSpace` | `'<sym>': Step 'P' has no parameterSpace, so it has no Task parameters.` |
| No such Task parameter | `'<sym>': Step 'P' has no Task parameter 'N'.` |
| Path-typed `Output`/`TaskParam` in a TEMPLATE-scope field | `'<sym>' has a path type, so it is available only in fields resolved on the worker host. Use Steps.P.RawOutput.V here.` (or `TaskRawParam`) |

`<sym>` is the symbol as the expression reads it, which may include a property after the
name (`Steps.P.RawOutput.F.name`). Several references in one field are reported in sorted
symbol order. Without `CONTROL_FLOW` none of this applies, and a `Steps.*` reference is an
ordinary `Undefined variable`.

Pass 8 does not evaluate a whole-field expression `range` of a `STRING`, `PATH`, or
`FLOAT` Task parameter, nor a format-string item of a `FLOAT` range list: they are
checked only when job creation resolves them, a gap that predates `CONTROL_FLOW` (a
`range: "{{Param.Nope}}"` passes `openjd check`). For those fields pass 8 runs the
reference scan alone, so the `Steps.*` scope rules hold there, but their expressions are
not type-checked.

**`enableIf`.** A Step's `enableIf` is checked against a table of its own: `Param.*`
(minus `PATH` types), `RawParam.*`, `Job.Name`, `Step.Name`, and the TEMPLATE-scope
`Steps.*` of the Step's direct dependencies, with no `let` names, `Task.*`, or
`Session.*`. A Step Environment's `enableIf` uses the same table. A Job Environment's
uses `Param.*` (minus `PATH`), `RawParam.*`, and `Job.Name`, with the reference scan
rejecting any `Steps.*`. The target type is `bool`, not `bool?`, so EXPR's coercion
rejects an `int` (`Cannot coerce int to bool`) or a static `null`
(`Cannot coerce nulltype to bool`); an unresolved `bool?`, such as a conditional
producer's output, is accepted and checked when concrete. A YAML `bool` needs no check. A
format string must be one expression and nothing else: a literal string (`'yes'`,
`'true'`) or text around an expression (`'{{Param.Run}} '`) would resolve to a string,
which the RFC does not say how to read as a bool (plan ambiguity A8), so the prototype
rejects it with `must be a bool, or a format string that is one expression and nothing
else, such as '{{Param.Run}}'.`, and still scans its references. Without `CONTROL_FLOW`
pass 8 does not look at `enableIf`; pass 11 rejects the field.

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
  wrap hooks. A session's environment stack is the job's `jobEnvironments` plus exactly
  one step's `stepEnvironments`, so this is enforced per step: for every step, the count
  of wrap-defining envs in `jobEnvironments` plus that step's `stepEnvironments` must be
  ≤ 1. Multiple wrap envs in `jobEnvironments` alone are reported once at the
  `jobEnvironments` path (reachable from every session); a step that adds its own wrap env
  on top is reported at that step's `stepEnvironments` path.

The single-layer rule runs only in the job-template path. An environment template defines
one environment, so the rule is trivially satisfied for an isolated env template; if
separately-validated env templates are composed into a session at assembly time
(worker-side), the cross-layer constraint must be enforced there.

## Pass 11: CONTROL_FLOW Gating and Structure

Validates or rejects the fields `CONTROL_FLOW` (RFC 0010) adds. It implements items 1, 2,
and the Environment Template part of item 6 of the RFC's §Validation, "At template
validation". Items 3 to 7 (the `Steps.*` scope and `enableIf`) are in pass 8.

**Without the extension**, each field is an error at its own path, with the message
`<field> requires the CONTROL_FLOW extension.`:

- `steps[i] -> enableIf`
- `steps[i] -> outputVariables`
- `jobEnvironments[i] -> enableIf` and `steps[i] -> stepEnvironments[j] -> enableIf`

`jobOutputDefinitions` is not part of the schema yet, so it fails deserialization as an
unknown field with or without the extension. A `Steps.*` reference fails in pass 8 as an
undefined variable, because without the extension nothing seeds the `Steps` root. Pass 8
checks `enableIf` and the `Steps.*` scope only with the extension (see § The `Steps.*`
Scope and `enableIf`).

**EXPR prerequisite.** `CONTROL_FLOW` without `EXPR` is an error at `extensions`
(`CONTROL_FLOW requires EXPR; both must be listed in the template's `extensions` (RFC
0010).`). The check is `helpers::check_expr_prerequisite`, which pass 10 shares for
`WRAP_ACTIONS`.

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

Outputs on a Step with a `CHUNK[INT]` Task parameter are allowed. The distinct-values rule
applies only to a `range` written as a YAML list: a range string such as `"1-10"` cannot
repeat a value, because its sub-ranges may not overlap, and a computed range is checked
when it is materialized (RFC 0010 §Chunked Steps item 1), which job creation does not do
yet.

Constraint validity reuses `JobParameterDefinition::validate_definition` on the wrapped
definition, so the rules are exactly those of the job parameter of the same type (for
example, `minValue` may not exceed `maxValue`, and `allowedValues` must respect
`minLength`/`maxLength`). Its messages start with `Parameter 'X': `, which pass 11
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
- an `enableIf` that is a number, list, or mapping, or a string that is not a well-formed
  format string.

An explicit `null` for `enableIf`, `outputVariables`, or an output's `collect`, `unit`,
`default`, `userInterface`, or `dataFlow` means the field is omitted, as for the
template's other optional fields, so it is neither gated nor reported.

**Environment Templates.** `CONTROL_FLOW` applies to Job Templates only. Listing it in an
Environment Template is an error at `extensions`, and an `enableIf` on the template's
environment is an error at `environment -> enableIf`, whether or not the template lists
the extension.

### Known gaps inherited from job parameters

Constraint validity is exactly the job parameter rule, including its gaps: the EXPR-type
job parameter definitions (`RANGE_EXPR` and the `LIST[...]` types) check `minLength`
against `maxLength`, `item` bounds, and `RANGE_EXPR` `minLength >= 1` only through their
`default`, so an output (or a job parameter) declaring `minLength: 5, maxLength: 1`
passes. Fixing that belongs in `expr_parameters.rs`, for both.

### Not yet checked

These RFC rules belong to later changes in the prototype plan
(`docs/control-flow-prototype-plan.md`):

- `jobOutputDefinitions` (item 10), including its `Steps.*` scope, in which any Step is
  readable without a dependency.
- The warning for a conditional Step's `null` in a discarding position (item 8).
  openjd-rs has no validation-warning channel; adding one is a public API decision (see
  ambiguity A1 in the plan).

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
