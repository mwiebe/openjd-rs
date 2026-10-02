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
| 8 | `format_strings.rs`, then `service_scope.rs` | Validate format string variable references; adapts scopes and expression complexity based on EXPR; with SERVICE, the `Service.*` / `Service.File.*` / `WrappedService.*` scopes and every `<Service>`'s format strings and `let` bindings (RFC 0009). `service_scope.rs` then rewrites the generic undefined-variable message of each out-of-scope `Service.*` / `Task.*` reference whose Service the document declares into the scope rule it breaks |
| 9 | `task_chunking.rs` | Gate TASK_CHUNKING features (ChunkInt parameters) |
| 10 | `wrap_actions.rs` | Gate WRAP_ACTIONS features (the three RFC 0008 hooks and, with SERVICE, the four `onWrapService*` hooks), enforce the all-or-nothing / hooks-follow-`runScope` rule (with the effective `[SERVICE]` for a Service's `serviceEnvironments`), and the single-wrap-layer-per-session rule for Task and Service Sessions (RFC 0008, RFC 0009) |
| 11 | `service.rs` | Gate SERVICE features (`jobServices`, `stepServices`, `services`, `runScope`), validate every `<Service>` structurally — including its `serviceEnvironments` — and validate `runScope` (RFC 0009, Template Schemas §4 item 3, §9) |

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
  scope, seeing the Services before it in the list — and the environment, when its `runScope`
  excludes `SERVICE`, sees the `port` / `connectAddress` of every Service in the document (see
  "Service scopes" under pass 8). A services-only document has no environment body, so only
  the Services are walked.
- **Pass 10** — WRAP_ACTIONS gating (see below), on the environment when there is one.
- **Pass 11** — SERVICE: the EXPR prerequisite; the `services` list, gated
  (`services requires the SERVICE extension.`) and otherwise validated by the same
  `validate_service_list` as `jobServices`, with paths rooted at `services[i]`; and the
  environment's `runScope`.

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
- Host requirements (`validate_host_requirements_in_context`, shared with `<Service>`):
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
  mutually exclusive with `script` when enabled
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
session/task scope.

4. **Task scope** — For step scripts. Adds `Task.Param.*`, `Task.RawParam.*`,
   `Task.File.*`. With EXPR: adds `Job.Name`, `Step.Name`, `Env.File.*` from
   step and job environments.

With `SERVICE` (RFC 0009), the session and task scopes additionally carry the
`Service.<name>.<port>.*` endpoints in scope where the field is defined, and a
`<Service>` has two scopes of its own; see "Service scopes" below.

### Service scopes (RFC 0009; Template Schemas §7.3.1 `Service.*` rows, §9 scope list, §9.7 items 1–2, §3.6.2, §4 item 3.2, §4.3.1)

Everything a Service endpoint resolves to is `@fmtstring[host]` — unknown until the scheduler
places the Service — so pass 8 seeds the `Service.*` keys as `Unresolved` placeholders
(`port` as `unresolved[int]`, `bindAddress` and `connectAddress` as `unresolved[string]`,
`Service.File.<name>` as `unresolved[path]`) through the `pub(crate)` seeders in
`job::service_symbols`, which spell the keys exactly as the runtime-facing
`build_service_symbol_table` does. Scope — *which* Services and which values a given field
may see — is decided per call site, and a reference outside its scope is detected as the
crate's ordinary undefined-variable error, exactly as an out-of-scope `WrappedStep.Name` is
under RFC 0008. The same mechanism rejects a reference to a Service or port name that is not
declared (§9.7 item 1). Nothing about `Service.*` is examined unless the template declares
`SERVICE`; without it pass 11 rejects the lists and pass 8 never walks them.

#### Scope-rule diagnostics (`service_scope.rs`)

The symbol-table mechanism finds every violation but explains none of them: the author sees
`Undefined variable: 'Service.Counter.api.connectAddress'.` — and, when another Service's name
is one edit away, a `Did you mean: Service.Cache.main.port` pointing at the wrong Service
(exploratory report stumbles S2–S4, S7, `w05`). After pass 8 has walked a document,
`service_scope::refine_job_template` / `refine_environment_template` revisit the errors it
added (those from the index `errors.errors` had before the pass) whose message contains
`Undefined variable: '<name>'.` and, when `<name>` is `Service.<svc>.…` with `<svc>` **declared
somewhere in the document** (or `Task.*` inside a Service), replace that sentence — and the
`Did you mean` suggestion on the same line — with the rule the reference breaks. The path, the
`Failed to parse interpolation expression at [s, e]. ` / `Invalid expression in let binding
'x': ` prefix, and the expression-source and caret lines that follow are untouched, and the
structured `ErrorDetail` summary and span summaries are updated in step. The reference site is
classified from the error path (`Site`: a Step's `script`, a Service body or its
`serviceEnvironments`, a Job Environment, a Step Environment, an Environment Template's
`environment`, or a job-creation field — `hostRequirements`, any `let`, a `parameterSpace`
range, `timeout` / `notifyPeriodInSeconds`, a Service's `port` / `timeoutSeconds` /
`intervalSeconds` / `maxAttempts`); the declarations are every `jobServices` /
`stepServices` entry (or an Environment Template's `services`). Rules, in the order tried:

| Condition | Message |
|---|---|
| `Task.*` at a Service site | `Task.* is not available within a Service.` |
| job-creation field | `Service.* is not available in <field>: it is resolved at job creation, before any Service has an endpoint.` (`<field>` is `hostRequirements`, `a let binding`, `a parameterSpace range`, `timeout`, `notifyPeriodInSeconds`, `port`, `timeoutSeconds`, `intervalSeconds`, or `maxAttempts`) |
| the site is an Environment with `SERVICE` in its (effective) `runScope` | `Environment 'Conda' is entered in Service Sessions (its runScope includes SERVICE) and may not reference Service.*; declare runScope: [TASK] if it configures Tasks.` |
| `<svc>` is declared later in the referencing Service's own list | `Service 'Backend' is declared later in jobServices than 'Proxy'; a Service may reference only itself and earlier Services.` (`stepServices` / `services` for the other lists) |
| `<svc>` is a Step Service not in scope here | `Service 'Counter' is a Step Service of step 'Count' and is not in scope in step 'After'.` (`in Job Environment 'E'` / `in Service 'B'` for those sites) |
| the port is not declared on the in-scope Service | `Service 'Store' has no port 'mian'; declared ports: main.` |
| `bindAddress` outside the declaring Service | `Service.Proxy.main.bindAddress is available only within the Service 'Proxy' itself; use connectAddress to reach it from elsewhere.` |

Everything else keeps the generic message with its suggestion: a Service name declared nowhere
(a typo is then the likeliest cause — `Undefined variable: 'Service.Cash.main.connectAddress'.
Did you mean: Service.Cache.main.connectAddress`), an unknown value name after a declared port,
`Service.File.*`, or a reference with too few components. The conformance `.invalid` fixtures
check only pass/fail and are unaffected; `tests/integration/test_service_scope.rs` and
`test_service_environments_list.rs` pin the exact messages.

Who sees which Services (the `in_scope` iterator at each site):

| Field | Services whose `port` / `connectAddress` are in scope | `bindAddress` |
|---|---|---|
| step `script` (actions, embedded files, `<StepScript>.let`, `<SimpleAction>.let`) — `build_task_scope_symtab` | every `jobServices` entry and the Step's own `stepServices` | never |
| `jobEnvironments[i]` (variables, actions, embedded files, `<EnvironmentScript>.let`) — `build_session_scope_symtab` | every `jobServices` entry, **only when the environment's `runScope` excludes `SERVICE`** (`!env.runs_in(RunScope::Service)`; the default `runScope` includes it) | never |
| `steps[i].stepEnvironments[j]` | every `jobServices` entry plus that Step's `stepServices`, under the same `runScope` condition | never |
| `jobServices[k]` body (variables, every action, embedded files, `<ServiceScript>.let`) — `validate_service_format_strings` | `jobServices[..k]` (earlier in the list) plus itself | its own only |
| `steps[i].stepServices[k]` body | every `jobServices` entry, `stepServices[..k]`, plus itself | its own only |
| environment template `services[k]` body | `services[..k]` plus itself | its own only |
| `<service path> -> serviceEnvironments[j]` (variables, actions incl. wrap hooks, embedded files, `<EnvironmentScript>.let`) — `build_service_env_scope_symtab` | the same Services as the declaring Service's body, **unconditionally** (a Service Environment carries no `runScope`; it is entered only in the declaring Service's Session, after its ports are allocated) | the declaring Service's only |
| environment template `environment` | every `services` entry, under the `runScope` condition | never |
| any `hostRequirements` (Step's or Service's), `<StepTemplate>.let`, `<Service>.let`, parameter-space ranges, action `timeout` / cancelation fields, numeric Service fields | **none** — job-creation stage | never |

Consequences the tests pin: a Service cannot reference a Service later in its own list, a Job
Service cannot reference any Step Service, a Step cannot reference another Step's Service, a
wrapping Job or Step environment entered in Service Sessions (any `runScope` including
`SERVICE`, so also the default) sees no `Service.*` even in its `onWrapService*` hooks, and
`Task.*` is never seeded for a Service (§9: "`Task.*` values are never available within a
Service"). A Service Environment is the one exception to the `runScope` rule (§4 item 3.2,
§7.3.1 scope rule 1): it sees the declaring Service's own scope, `bindAddress` included, in
every field including its `onWrapService*` hooks (alongside `WrappedService.*`); other
Services see a Service's Service Environments only through that Service's ports (scope rule
2), so `Env.File.*` of one is undefined everywhere else.

`Service.File.<name>` is seeded for the declaring Service only, from its script's
`embeddedFiles`, into the service-execution scope (and so into `<ServiceScript>.let`, like
`Task.File.*` for `<StepScript>.let`).

**Within a `<Service>`** (`validate_service_format_strings`), two scopes follow the
`@fmtstring` stage annotations:

- *Job-creation scope* — the owner's template-scope table (`Param.*` without PATH,
  `RawParam.*`, `Job.Name`; for a Step Service also `Step.Name` and the step-level `let`
  values; for an environment template's Service, `Job.Name` only) plus the `<Service>.let`
  bindings, validated with `validate_let_bindings` against the template library (no host
  functions) and the step-level names as the enclosing (non-shadowable) scope. Used for
  `<Service>.let` itself, `hostRequirements` (through the shared
  `validate_host_requirements_fs`, so the Step and Service checks are one code path with the
  owner's path prefixed), the numeric `@fmtstring` fields, and every action's `timeout` /
  cancelation `mode` / `notifyPeriodInSeconds` (`validate_action_timing_fs`). Never
  `Session.*`, `Service.*`, PATH `Param.*`, or `Task.*` (§9 item 3, §9.7 item 2).
- *Service-execution scope* — `Param.*` including PATH, `RawParam.*`, `Session.*`, every
  job-creation-stage symbol above (copied over, `Param`/`RawParam` excepted), this Service's
  `Service.File.*`, its own three endpoint values, the `port` / `connectAddress` of every
  in-scope Service, and the `<ServiceScript>.let` bindings (host library; the step- and
  service-level names are the enclosing scope). Used for `variables` (with the §4.4.2
  `max_env_var_value_len` constraint, as for an Environment), every action's `command` /
  `args`, and embedded-file `data`. Without EXPR, complex expressions in any of these are
  rejected with `complex expressions require the EXPR extension.`; `let` in either position
  is rejected with `'let' requires the EXPR extension.`; comprehension loop variables may not
  shadow any `let` name in scope.

**Within a Service Environment** (`serviceEnvironments[j]`, §9 item 5), the symbol table is
the session scope a `stepEnvironments` entry gets — `Param.*` including PATH, `RawParam.*`,
`Session.*`, the Service's job-creation-stage symbols (`Job.Name`; for a Step Service
`Step.Name` and the step-level `let` values; for an environment template's Service `Job.Name`
only; and the `<Service>.let` values, which §9 item 3 makes available in `serviceEnvironments`
as a Step's `let` is in its `stepEnvironments`), the Environment's own `Env.File.*`, and its
`<EnvironmentScript>.let` (host library; the step-level and `<Service>.let` names are the
enclosing scope, so `'<name>' shadows enclosing scope.` for either) — plus the declaring
Service's three endpoint values and the `port` / `connectAddress` of every in-scope Service.
Not in scope: `Task.*`, `Service.File.*` (the Service's script's files), the
`<ServiceScript>.let` names, and another Service's `<Service>.let`. The Service Environments
are therefore validated after the `<Service>.let` bindings. The body is validated through
`validate_env_format_strings` exactly like a `stepEnvironments` entry, so the wrap hooks get
`WrappedAction.*` and their companion group, and the actions' `timeout` / cancelation fields
validate against the Service's job-creation scope (`<Service>.let` included; no `Session.*`,
no `Service.*`). Comprehension loop variables are checked as for any environment.

**The `WrappedService.*` group** (§4.3.1) is added to the wrap-hook symbol table for exactly the
four `onWrapService*` hooks (`WrapHookScope::Service` → `add_wrapped_service_scope`):
`WrappedService.Name` (`string`), `WrappedService.PortNames` (`list[string]`),
`WrappedService.Ports` (`list[int]`), `WrappedService.BindAddresses` (`list[string]`),
`WrappedService.Protocols` (`list[string]`, each `TCP` or `UDP`). It is
not available in `onWrapEnvEnter` / `onWrapTaskRun` / `onWrapEnvExit`, and `WrappedEnv.Name` /
`WrappedStep.Name` are not available in the Service hooks.

**Numeric `@fmtstring` fields** (§9.2 note; `SERVICE_PORT_CONSTRAINT`,
`SERVICE_SECONDS_CONSTRAINT`, `SERVICE_MAX_ATTEMPTS_CONSTRAINT`) join the resolved-value
constraint table below with target type `int?`: `port` 1–65535 (`must be between 1 and
65535.`), `timeoutSeconds` / `intervalSeconds` > 0 (`must be > 0.`), `maxAttempts` ≥ 0 (`must
be >= 0.`), non-integers `must be an integer.`, a whole-field `null` accepted as "not
provided". The messages match pass 11's checks on the literal forms and job creation's on the
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
| Service `port` (SERVICE, Template Schemas §9.2) | `int?` | soft cap: 100 chars | coerced integer in 1–65535; `null` = runtime allocates |
| Service `timeoutSeconds` / `intervalSeconds` (SERVICE, §9.3) | `int?` | soft cap: 100 chars | coerced integer > 0; `null` = §9.3 default |
| Service `maxAttempts` (SERVICE, §9.4) | `int?` | soft cap: 100 chars | coerced integer ≥ 0; `null` = 0 |
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

Validates or rejects features gated behind `WRAP_ACTIONS` (RFC 0008), including the RFC 0009
Service hooks (Template Schemas §4.3 "WRAP_ACTIONS extension constraints"):

- **Wrap hooks** (`onWrapEnvEnter`, `onWrapTaskRun`, `onWrapEnvExit`): Rejected on any
  environment when the extension is not declared: `<hook> requires the WRAP_ACTIONS
  extension.` on the hook's path.
- **Service hooks** (`onWrapServiceEnter`, `onWrapServiceRun`, `onWrapServiceReadinessCheck`,
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
- **Hooks follow `runScope`** (constraint 6, §9.7 item 6; `WRAP_ACTIONS` with `SERVICE`,
  replacing the all-or-nothing rule): a *wrapping* environment — one that defines any wrap hook
  (`has_any_wrap_hook`) — must define `onWrapEnvEnter` and `onWrapEnvExit`; must define
  `onWrapTaskRun` iff `runs_in(Task)`; and must define all four `onWrapService*` hooks iff
  `runs_in(Service)`. The default (absent) `runScope` includes both kinds, so RFC 0008's three
  hooks alone are incomplete once `SERVICE` is declared; `runScope: [TASK]` is exactly the RFC
  0008 rule. Each group with missing hooks is one error at the `actions` path naming the
  required hooks, the `runScope` as written (or `default runScope: every kind of Session`), and
  the missing ones:
  - `a wrapping environment must define onWrapEnvEnter and onWrapEnvExit whatever its runScope;
    missing: <hooks> (RFC 0009).`
  - `a wrapping environment whose runScope includes TASK (<runScope>) must define onWrapTaskRun;
    missing: onWrapTaskRun (RFC 0009).`
  - `a wrapping environment whose runScope includes SERVICE (<runScope>) must define
    onWrapServiceEnter, onWrapServiceRun, onWrapServiceReadinessCheck, and onWrapServiceExit;
    missing: <hooks> (RFC 0009).`

  Each hook the `runScope` does not call for is one error on the hook's own path: `<hook> must
  not be defined: this environment's runScope (<runScope>) excludes TASK|SERVICE (RFC 0009).`
  Unrecognized `runScope` names (rejected by pass 11) never match a kind, so the rule is
  evaluated over the recognized names only. A non-wrapping environment is not subject to the
  rule whatever its `runScope`.

  A Service's `serviceEnvironments` (RFC 0009 §9 item 5.2) are checked with the effective
  `runScope: [SERVICE]` whatever they declare (pass 11 rejects a `runScope` written on one),
  under `<service path> -> serviceEnvironments[j] -> script -> actions`. The `runScope` text
  in their messages is `effective runScope: [SERVICE], a Service Environment`: a wrapping
  Service Environment must define `onWrapEnvEnter`, `onWrapEnvExit`, and the four
  `onWrapService*` hooks, and `onWrapTaskRun must not be defined: this environment's runScope
  (effective runScope: [SERVICE], a Service Environment) excludes TASK (RFC 0009).` This is the
  way to wrap one Service in a container without wrapping anything else.
- **Single-wrap-layer rule**: at most one environment reachable in a session may define
  wrap hooks. A session's environment stack is the job's `jobEnvironments` plus exactly
  one step's `stepEnvironments`, so this is enforced per step: for every step, the count
  of wrap-defining envs in `jobEnvironments` plus that step's `stepEnvironments` must be
  ≤ 1. Multiple wrap envs in `jobEnvironments` alone are reported once at the
  `jobEnvironments` path (reachable from every session); a step that adds its own wrap env
  on top is reported at that step's `stepEnvironments` path.

- **Single-wrap-layer rule for Service Sessions** (RFC 0009; with `SERVICE`): a Service
  Session's stack is the scope's environments entered in Service Sessions — `jobEnvironments`
  whose `runScope` includes `SERVICE`, plus the Step's such `stepEnvironments` for a Step
  Service — followed by the Service's own `serviceEnvironments`. For every Service (`jobServices`,
  each Step's `stepServices`, and an environment template's `services`, where the document's own
  `environment` is the outer layer), when its `serviceEnvironments` contribute at least one wrap
  layer and the stack holds more than one, one error is reported at `<service path> ->
  serviceEnvironments`: `only one environment in a Service Session's stack (the scope's
  environments whose runScope includes SERVICE, then this Service's serviceEnvironments) may
  define any wrap hook (RFC 0008, RFC 0009).` Two layers in `jobEnvironments` alone are reported
  there once, not again under every Service; a wrapping `runScope: [TASK]` environment is never
  in a Service Session and does not count.

The Task-Session single-layer rule runs only in the job-template path. An environment template
defines at most one environment, so the rule is trivially satisfied for an isolated env template; if
separately-validated env templates are composed into a session at assembly time
(worker-side), the cross-layer constraint must be enforced there. Likewise the §1.2.2 item 3
rule — a wrapping environment from a document that does not declare `SERVICE` may not have a
Service placed in its scope — relates documents only the scheduler sees together and is not a
template-validation check: it is the submission-time check
`apply_environment_templates` runs on the combined Job (see "Submission-time check" below
and [job-creation.md](job-creation.md)).

## Pass 11: SERVICE Gating and Structure

Validates or rejects features gated behind `SERVICE` (RFC 0009, Template Schemas §1.1 item 8,
§1.2 item 6, §3 item 6, §4 item 3, §9–§9.7). Error paths are `jobServices[i] -> …`,
`steps[i] -> stepServices[j] -> …`, and, in an environment template, `services[i] -> …`.

**Gating (extension not declared):**
- `jobServices` → `jobServices requires the SERVICE extension.`; `stepServices` →
  `stepServices requires the SERVICE extension.`; environment-template `services` →
  `services requires the SERVICE extension.` The list's contents are not examined.
- `runScope` on any `<Environment>` (`jobEnvironments[i]`, `steps[i] -> stepEnvironments[j]`,
  or the environment template's `environment`) → `runScope requires the SERVICE extension.`
  on the `runScope` path. The list's contents are not examined.

**EXPR prerequisite (§9.7 item 7):** declaring `SERVICE` without `EXPR` is an error at
`extensions`, in both job and environment templates, whether or not any Service is defined:
``SERVICE requires EXPR; both must be listed in the template's `extensions` (RFC 0009).``

**With the extension declared:**
- **Lists** (§1.1 item 8, §1.2 item 6, §3 item 6): each `jobServices`/`stepServices`/`services`
  list `must not be empty.` and `must not contain more than 10 elements.`
- **`runScope`** (§4 item 3, §9.7 item 3): `must not be empty.` on the `runScope` path; on each
  offending element's path (`runScope[i]`), `unknown run scope name '<name>'; expected one of
  TASK, SERVICE.` (names are case-sensitive) or `duplicate run scope name '<name>'.` The
  companion rule that an Environment whose `runScope` includes `SERVICE` must not reference
  `Service.*` (§4 item 3.2, §9.7 item 2) is enforced by pass 8 (see "Service scopes").
- **Name uniqueness** (§9.7 item 5): `duplicate service name: '<name>'` on the offending
  element, for a repeat within a list and for a Step Service that shares a name with a Job
  Service. Different Steps may reuse a Step Service name.
- **`serviceEnvironments`** (§9 item 5, `validate_service_environments`), at `<service path> ->
  serviceEnvironments`: if provided, `must not be empty.` (as `stepEnvironments`). Each entry
  `[j]` gets pass 6's `validate_single_environment` (so `must have at least one of 'script' or
  'variables'.`, the `name` checks, action and embedded-file validation), pass 5's embedded-file
  `name` / `filename` length limits, and pass 7's `endOfLine` → `requires the FEATURE_BUNDLE_1
  extension.` gating, on the same paths a `stepEnvironments` entry reports them. Then (item 5.1)
  `serviceEnvironments[j] -> name`: `duplicate environment name: '<name>'` — the Environment
  collision message — for a repeat within the list, for the name of any `jobEnvironments`
  entry, and, for a Step Service, for the name of the declaring Step's `stepEnvironments`; in an
  environment template, for the name of the document's own `environment` (a Job Environment of
  every Job it is attached to). Different Services may reuse a Service Environment name, a Job
  Service's may reuse a Step Environment's name, and a Step Service's may reuse another Step's
  Step Environment's name. And (item 5.2, §9.7 item 3) `serviceEnvironments[j] -> runScope`:
  `must not be provided on a Service Environment: its scope is fixed to the declaring Service's
  Session (RFC 0009).` — the list is not examined. The hooks-follow-`runScope` rule for these
  Environments (pass 10) and their format strings (pass 8, "Service scopes") are elsewhere.
- **`<ServiceName>` and port names** (§9.1, §9.2 item 1, §9.7 item 5): on the `name` field,
  `'<name>' is not a valid identifier.`, `exceeds <max_identifier_len> characters.` (64, or
  512 with FEATURE_BUNDLE_1), and `must not be 'File'; it is reserved for Service.File.*
  references.`
- **Ports** (§9 item 6): `ports` `must not be empty.` / `must not contain more than 10
  elements.`; `duplicate port name '<name>'.` on the element.
- **Port numbers per protocol** (§9 item 6.4, §9.7 item 8): on `ports[i] -> port` of the later
  port, `<TCP|UDP> port <n> is also used by port '<earlier>'; two ports with the same protocol
  must not have the same port number.` Only literal numbers are compared here (one that
  carries an expression is compared at job creation, once resolved — see
  [job-creation.md](job-creation.md)). The same number on a TCP port and a UDP port is
  allowed. `protocol` itself (`TCP` | `UDP`, default `TCP`) is a serde enum, so `udp`, `SCTP`,
  or a format string is `unknown variant`.
- **Numeric `@fmtstring` fields**, checked on the field path when the value carries no
  expression (a format string is type-checked and, when static, range-checked by pass 8, and
  resolved and range-checked at job creation, like `<Action>.timeout`):
  `port` `must be between 1 and 65535.`; `readinessCheck.timeoutSeconds` and
  `readinessCheck.intervalSeconds` `must be > 0.`; `restartPolicy.maxAttempts` `must be >=
  0.`; any of them `must be an integer.` when the text does not parse.
- **Readiness consistency** (§9.7 item 4): `script -> actions`:
  `onReadinessCheck must be defined when readinessCheck.type is COMMAND.`;
  `script -> actions -> onReadinessCheck`: `onReadinessCheck must not be defined when
  readinessCheck.type is <TCP_CONNECT|STDOUT>.` (the default readiness type is
  `TCP_CONNECT`). A `TCP_CONNECT` `ports` list `if provided, must not be empty.` and each entry
  that is not a declared port reports `references undeclared port '<name>'.` on
  `readinessCheck -> ports[k]`.
- **Readiness and port protocols** (§9 item 7, §9.3 item 2, §9.7 item 4): a `TCP_CONNECT`
  `ports` entry naming a UDP port reports, on `readinessCheck -> ports[k]`, `port '<name>' has
  protocol UDP and cannot be probed by a TCP_CONNECT readiness check; only TCP ports may be
  named.` A Service none of whose ports is TCP whose effective check is `TCP_CONNECT` reports,
  on `readinessCheck`, `the default TCP_CONNECT readiness check has no TCP port to probe: none
  of the Service's ports has protocol TCP, so a readinessCheck of type STDOUT or COMMAND is
  required.` when `readinessCheck` is omitted, or `a TCP_CONNECT readiness check has no TCP
  port to probe: …` (same tail) when one of type `TCP_CONNECT` is given. (Not reported when
  `ports` is empty, which is already an error.) A Service with at least one TCP port may omit
  the check whatever else it declares: the default probes its TCP ports only.
- **Reused validators:** `description` (`validate_description`), `variables`
  (`validate_variables`, shared with `<Environment>`), `hostRequirements`
  (`validate_host_requirements_in_context`, shared with `<StepTemplate>`), every defined
  action (`validate_action`), and `script.embeddedFiles` (`must not be empty.` plus
  `validate_embedded_files` and the identifier/filename length limits).

The discriminator of `readinessCheck` and the `completedTasks` enum are enforced by serde
(`unknown variant`), as is the presence of `ports`, `script`, and `onRun`.

Not in this pass: `Service.*` scope rules (§9.7 items 1–2; pass 8, "Service scopes"), the
wrap hooks an Environment's `runScope` calls for (item 6; pass 10), `let` bindings and format
strings inside a Service (pass 8), job creation of Services (see
[job-creation.md](job-creation.md)), and the submission-time check (next section).

## Submission-time check (RFC 0009, Template Schemas §1.2.2 items 2–3, §9.7 closing paragraph)

One of the SERVICE extension's rules relates documents that only the scheduler sees together
— a Job Template and the Environment Templates attached to a submission — and so cannot run
when a template is validated on its own. It runs in `apply_environment_templates`
(`job/create_job/external.rs`), against the combined Job, after `create_job` and before any
external Service is instantiated; every violation is collected into one `ModelValidation`
error whose model name is `Submission` and whose paths are rooted at the document
(`JobTemplate`, `EnvironmentTemplate[i]` by attachment index, or the label the caller gave
the attachment):

- **Service names are scoped to their document (§1.2.2 item 2) — not a check.** An external
  Service's `name` may equal that of another external Service or of a Service in the Job
  Template's `jobServices` or any Step's `stepServices`; the submission is not rejected for
  it. Every `Service.*` reference resolves within its own document (pass 8 "Service scopes"
  seeds only the document's own Services, so a cross-document reference is an
  undefined-variable error at template validation), and the merge keeps same-named Services
  distinct by stamping each with its `job::Document` (see "apply_environment_templates" in
  [job-creation.md](job-creation.md)). Within one document the §9.7 item 5 uniqueness check
  (pass 11) applies as before.
- **Wrapping Environment from a SERVICE-less document (§1.2.2 item 3).** Such an Environment
  (any `WRAP_ACTIONS` hook defined; `runScope` and the `onWrapService*` hooks are both gated
  behind `SERVICE`, so it necessarily has the default scope and no Service hooks) must have
  no Service in its scope in the combined Job: for a Job Environment, the combined
  `jobServices` and every Step's `stepServices`; for a Step Environment, the combined
  `jobServices` and that Step's `stepServices`. Reported at `<doc> -> environment`,
  `JobTemplate -> jobEnvironments[i]`, or `JobTemplate -> steps[i] -> stepEnvironments[j]`,
  naming the document as the cause and quoting the spec's remedy: `wrapping Environment
  '<e>' is defined by <doc|the Job Template>, which does not declare the SERVICE extension,
  so it has the default runScope (every kind of Session) and cannot define the
  onWrapService* hooks; but the combined Job places <Service> in its scope, and the Service
  would run in a Session the Environment enters but cannot wrap. Declare SERVICE in <doc>
  and either define onWrapServiceEnter, onWrapServiceRun, onWrapServiceReadinessCheck, and
  onWrapServiceExit, or declare a runScope that excludes SERVICE (RFC 0009, Template Schemas
  §1.2.2 item 3).` A document that declares `SERVICE` is governed by pass 10's
  hooks-follow-`runScope` rule instead and is never reported here.

The 10-element cap on `services` (pass 11) is per document; the combined list is not capped.
Everything else in §9.7 is checked per document by the passes above.

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
