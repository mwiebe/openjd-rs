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
| — | `service.rs` (`gate_services_*`) | **SERVICE gating, first** (RFC 0009): in a template that does not declare `SERVICE`, report `services`, `requiresServices`, the `service` key of every Step's `dependencies` entries, and every Environment's `dependencies` and `runScope` (`<field> requires the SERVICE extension.`) without examining their contents. Runs ahead of every other pass so these errors lead the list; an `Undefined variable: 'Service.X.*'` (pass 8) that follows from the missing extension is then a consequence and is left unreported (see pass 8) |
| 5 | `limits.rs` | Enforce numeric limits (name lengths, counts); FEATURE_BUNDLE_1 raises many limits |
| 6 | `structure.rs` | Structural validation (uniqueness, required fields, Step dependencies: each entry exactly one of `dependsOn` / `service`, no self or duplicate entries, `dependsOn` names a Step — with SERVICE, `service` entries and cycles are left to pass 11, and a `dependsOn` that names no Step but a Service of that name gets a `did you mean 'service: <name>'?` hint) |
| 7 | `feature_bundle_1.rs` | Gate FEATURE_BUNDLE_1 features (simple actions, endOfLine) |
| 8 | `format_strings.rs`, then `service_diagnostics.rs` | Validate format string variable references; adapts scopes and expression complexity based on EXPR; with SERVICE, the `Service.*` / `Service.File.*` / `WrappedService.*` scopes and every `<Service>`'s format strings and `let` bindings (RFC 0009). `service_diagnostics.rs` then rewrites the generic undefined-variable message of each out-of-scope `Service.*` / `Task.*` / `Step.*` reference whose Service the document declares or requires into the scope rule it breaks, drops the per-reference errors of an Environment whose explicit `runScope` includes `SERVICE` (pass 11 reports the list once), and, without `SERVICE`, drops references to a Service the (gated) `services` / `requiresServices` lists name |
| 9 | `task_chunking.rs` | Gate TASK_CHUNKING features (ChunkInt parameters) |
| 10 | `wrap_actions.rs` | Gate WRAP_ACTIONS features (the three RFC 0008 hooks and, with SERVICE, the four `onWrapService*` hooks), enforce the all-or-nothing / hooks-follow-`runScope` rule, and the single-wrap-layer-per-session rule (RFC 0008, RFC 0009) |
| 11 | `service.rs` | With SERVICE declared: validate every `<Service>` and `<ServiceRequirement>` structurally (including that a literal `maxAttempts` greater than 0 comes with `completedTasks`), resolve every `service` dependency and each Service's and each Environment's `dependencies` (with the wrong-key hints), check the combined Step/Service/Job-Environment dependency graph for cycles and every Service's computed scope (`template::service_scope`), and validate `runScope` — including that a `stepEnvironments` entry never gives one, and that an Environment whose explicit `runScope` includes `SERVICE` neither lists nor references a Service (RFC 0009, Template Schemas §4 items 3–4, §9). The EXPR prerequisite is checked whether or not SERVICE is declared; the gated fields of a template without SERVICE are the gating step's, above |

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
  `dependencies` — and the environment, when its effective `runScope` excludes `SERVICE` (the
  default once it lists a Service), sees the `port` / `connectAddress` of the Services of the
  document it lists in its own `dependencies` (see "Service scopes" under pass 8). A
  services-only document has no environment body, so only the Services are walked.
- **Pass 10** — WRAP_ACTIONS gating (see below), on the environment when there is one.
- **Pass 11** — SERVICE: the EXPR prerequisite; the `services` list, gated
  (`services requires the SERVICE extension.`) and otherwise validated by the same
  `validate_service_list` as a Job Template's, with paths rooted at `services[i]`, plus
  `validate_environment_template_dependencies` (each Service's `dependencies` may name only
  another Service of this document, with the `service` key, since the document has no Steps, and
  must be acyclic); and the environment's `dependencies` (each entry `service: <name>` naming a
  Service of this document's `services`, see "Pass 11" below) and `runScope`. `requiresServices`
  is not a property of this document and is rejected at decode.

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
- Dependencies (§3.2): each entry gives exactly one of `dependsOn` and `service` — an entry with
  both reports `a dependency names a Step with dependsOn or a Service with service, not both:
  {dependsOn: A, service: X} (Template Schemas §3.2).`, one with neither `a dependency must name
  a Step with dependsOn or a Service with service; this entry gives neither (Template Schemas
  §3.2).`, and such an entry is no dependency (not checked further, not an edge, not a listing);
  no self-dependency (`cannot depend on itself.`); no duplicates (`duplicate dependency 'A'.` for a
  Step, `duplicate dependency 'service: X'.` for a Service — `service::duplicate_dependency`);
  and a `dependsOn` entry must name a Step (`dependency '<step>' not found.`). A `service` entry
  names a Service and is resolved by pass 11 (gated without SERVICE, see above).
  - **The wrong-key hint** (`helpers::dependency_kind_hint`, RFC 0009 §3.2): when a `dependsOn`
    names no Step but a declared or required Service of that name exists, the message is
    `dependency 'Svc' names no Step; did you mean 'service: Svc'?` (`service::unknown_step_dependency`);
    the mirror, from pass 11, when a `service` entry names no Service but a Step of that name
    exists: `dependency 'Prep' names no Service; did you mean 'dependsOn: Prep'?`. The same two
    hints apply to a Service's `dependencies`, and the `service` form to an Environment's or an
    Environment Template Service's `dependsOn` entry. No match, no hint (`dependency 'Y' not
    found.`).
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

### Service scopes (RFC 0009; Template Schemas §7.3.1 `Service.*` rows, §9 scope list, §9.8, §9.9 items 1–2, §3.6.2, §4 item 3.2, §4.3.1)

Everything a Service endpoint resolves to is `@fmtstring[host]` — unknown until the scheduler
places the Service — so pass 8 seeds the `Service.*` keys as `Unresolved` placeholders
(`port` as `unresolved[int]`, `bindAddress` and `connectAddress` as `unresolved[string]`,
`Service.File.<name>` as `unresolved[path]`) through the `pub(crate)` seeders in
`job::service_symbols`, which spell the keys exactly as the runtime-facing
`build_service_symbol_table` does. Scope — *which* Services and which values a given field
may see — is decided per call site, and a reference outside its scope is detected as the
crate's ordinary undefined-variable error, exactly as an out-of-scope `WrappedStep.Name` is
under RFC 0008. The same mechanism rejects a reference to a Service or port name that is
neither declared nor required (§9.9 item 1). Nothing about `Service.*` is examined unless the
template declares `SERVICE`; without it pass 11 rejects the lists and pass 8 never walks them.

Visibility follows the declared dependencies (§9 scope list, §9.8 item 2), one rule with no
exceptions: a Step's `script` and its `stepEnvironments` see a Service's `port` /
`connectAddress` — inline or required — only when the Step lists `service: <name>` in its
`dependencies`; a Service sees another only when it lists it the same way; a Job Environment
sees only the inline and required Services it lists in its own `dependencies` (§4 item 3); and
an Environment Template's `environment` sees only the Services of its document it lists. The
seeders take the `template::service_scope::listed_services(dependencies, services)` and
`listed_requirements(dependencies, requirements)` iterators at every site, over the Step's list
for a Step's sites and over the Environment's own list for a Job Environment. Listing a required
Service grants access to its values and nothing more: its scope is every Step and it is READY
before any Task runs whether or not anything lists it, and a requirement that nothing lists is
accepted (unlike an unused inline Service, pass 11). Pass 8 does not decide a Service's *scope*
(the Steps whose Tasks depend on it, §9.1): `template::service_scope::compute_service_scopes`
computes it from the `dependencies` lists — the Steps', the Services', and the Job
Environments' (rule 3) — for pass 11 (cycles, unused Services, a job-wide Service listing a
Step) and job creation; see [template-types.md](template-types.md), "Service scope".

#### Scope-rule diagnostics (`service_diagnostics.rs`)

The symbol-table mechanism finds every violation but explains none of them: the author sees
`Undefined variable: 'Service.Counter.api.connectAddress'.` — and, when another Service's name
is one edit away, a `Did you mean: Service.Cache.main.port` pointing at the wrong Service
(exploratory report stumbles S2–S4, S7, `w05`). After pass 8 has walked a document,
`service_diagnostics::refine_job_template` / `refine_environment_template` revisit the errors
it added (those from the index `errors.errors` had before the pass) whose message contains
`Undefined variable: '<name>'.` and, when `<name>` is `Service.<svc>.…` with `<svc>` **declared
in the document's `services` or required by its `requiresServices`** (or `Task.*` / `Step.*`
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
`notifyPeriodInSeconds`, a Service's `port` / `maxAttempts` / the four `<ServiceHealthCheck>`
numeric fields); the declarations are the document's `services`, and a
Job Template's `requiresServices` count as known with their declared ports. Rules, in the
order tried:

| Condition | Message |
|---|---|
| `Task.*` / `Step.*` anywhere under `services[k]` | `Task.* is not available within a Service.` / `Step.* is not available within a Service.` (§9 item 3: the RFC states the two together) |
| job-creation field (§9.9 item 2: every field resolved at job creation, before any Service is placed) | `Service.* is not available in <field>: it is resolved at job creation, before any Service has an endpoint.` (`<field>` is `hostRequirements`, `a let binding` — a `<StepTemplate>`'s or `<Service>`'s, not a script's —, `a parameterSpace range`, `timeout`, `a cancelation method's mode or notifyPeriodInSeconds` (both reported on the `cancelation` path), `notifyPeriodInSeconds`, `port`, `readinessIntervalSeconds`, `readinessTimeoutSeconds`, `healthIntervalSeconds`, `failureThreshold`, or `maxAttempts`). Pass 8 never seeds `Service.*` into these fields' symbol tables (`build_template_scope_symtab`, `validate_action_timing_fs`, the numeric `@fmtstring` checks), so the reference is undefined there whatever the scope; this rule only names the field |
| the site is a Job Environment (or an Environment Template's `environment`) with `SERVICE` in its explicit `runScope` | **the error is dropped.** Pass 11 reports the rule once, on the `runScope` list: `Environment 'Conda' is entered in Service Sessions (its runScope includes SERVICE) and may not reference Service.*; declare runScope: [TASK] if it configures Tasks.` (`… may not depend on a Service …` when it lists one and references none, `… may not depend on or reference a Service …` when both) — and each reference is a consequence of it (exploratory report S12, S21). A Step Environment gives no `runScope` (pass 11 rejects any) and is always a Task-Session site, so this rule never applies to it |
| a declared or required Service referenced from a Step's `script`, one of its `stepEnvironments`, another Service, a Job Environment, or an Environment Template's `environment`, whose `dependencies` (the Step's, for a Step Environment) do not list `service: <svc>` (§9 scope rules 2–5, §9.8 item 2) | `Step 'Render' references Service.Cache.main.port but does not list service: Cache in dependencies.` — from a Step Environment, `Step 'Render' references Service.Cache.main.port in stepEnvironments 'Tools' but does not list service: Cache in dependencies.` — from a Service, `Service 'Front' references Service.Back.main.port but does not list service: Back in dependencies.` — from a Job Environment or an Environment Template's `environment`, `Environment 'CacheClient' references Service.Cache.main.port but does not list service: Cache in dependencies.` |
| the port is not declared | `Service 'Store' has no port 'mian'; declared ports: main.` — or, for a required Service, `required Service 'Cache' has no port 'admin'; declared ports: main.` |
| `bindAddress` of a required Service | `bindAddress of required Service 'Cache' is not available; use connectAddress to reach it.` |
| `bindAddress` outside the declaring Service | `Service.Proxy.main.bindAddress is available only within the Service 'Proxy' itself; use connectAddress to reach it from elsewhere.` |

When the document does **not** declare `SERVICE`, `refine` instead drops every `Undefined
variable: 'Service.<svc>…'` whose `<svc>` the document's (gated, unexamined) `services` or
`requiresServices` names: the gating step's `services requires the SERVICE extension.` is the
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
| step `script` (actions, embedded files, `<StepScript>.let`, `<SimpleAction>.let`) — `build_task_scope_symtab` | the `services` entries and the `requiresServices` entries the Step lists with the `service` key in `dependencies` (`listed_services`, `listed_requirements`; a requirement's declared ports) | never |
| `jobEnvironments[i]` (variables, actions, embedded files, `<EnvironmentScript>.let`) — `build_session_scope_symtab` | the `services` entries and the `requiresServices` entries the Environment itself lists with the `service` key in its `dependencies` (`listed_services`, `listed_requirements` over `env.dependencies`; listing an inline Service puts every Step in its scope, §9.1 rule 3), **only when the environment's effective `runScope` excludes `SERVICE`** (`!env.runs_in(RunScope::Service)`; the default follows the dependency, so this only bites an explicit `runScope` that includes `SERVICE`, which pass 11 rejects beside it) | never |
| `steps[i].stepEnvironments[j]` | the `services` and `requiresServices` entries its Step lists in `dependencies` (a Step Environment has no list of its own), always — a Step Environment gives no `runScope` and is entered only by the Task Sessions of its Step | never |
| `services[k]` body (variables, every action, embedded files, `<ServiceScript>.let`) — `validate_service_format_strings` | the other `services` entries and the `requiresServices` entries it lists with the `service` key in `dependencies` (any order; pass 11 rejects a cycle), plus itself | its own only |
| environment template `services[k]` body | the other `services` entries it lists in `dependencies`, plus itself | its own only |
| environment template `environment` | the `services` entries it lists in its own `dependencies` (`listed_services`), under the `runScope` condition | never |
| any `hostRequirements` (Step's or Service's), `<StepTemplate>.let`, `<Service>.let`, parameter-space ranges, every `<Action>`'s `timeout`, a cancelation method's `mode` / `notifyPeriodInSeconds`, the numeric `@fmtstring` fields of `<ServicePort>`, `<ServiceHealthCheck>` and `<ServiceRestartPolicy>` | **none** — job-creation stage (§9.9 item 2); `service_diagnostics` names the field | never |

Consequences the tests pin: list order carries no meaning (a Service may list and reference a
later Service), a reference to an inline or required Service without the matching `service`
dependency is an error naming the fix (from a Step, a Service, or an Environment alike), a
requirement nothing lists is accepted, a dependency cycle is a pass 11 error, a wrapping Job
environment whose explicit `runScope` includes `SERVICE` sees no `Service.*` even in its
`onWrapService*` hooks, a required Service's `bindAddress` and undeclared ports are errors, and
`Task.*` / `Step.*` are never seeded for a Service (§9: "`Task.*` and `Step.*` values are never
available within a Service"). There is no Environment that sees a Service's `bindAddress`: a
`<Service>` has no `serviceEnvironments` property (the RFC's Rejected Ideas), so the only
Environments a Service Session enters are the Job's, which follow the `runScope` rule.

`Service.File.<name>` is seeded for the declaring Service only, from its script's
`embeddedFiles`, into the service-execution scope (and so into `<ServiceScript>.let`, like
`Task.File.*` for `<StepScript>.let`).

**Within a `<Service>`** (`validate_service_format_strings`), two scopes follow the
`@fmtstring` stage annotations:

- *Job-creation scope* — the document's template-scope table (`Param.*` without PATH,
  `RawParam.*`, `Job.Name`) plus the `<Service>.let` bindings, validated with
  `validate_let_bindings` against the template library (no host functions). Used for
  `<Service>.let` itself, `hostRequirements` (through the shared
  `validate_host_requirements_fs`, so the Step and Service checks are one code path with the
  owner's path prefixed), the numeric `@fmtstring` fields, and every action's `timeout` /
  cancelation `mode` / `notifyPeriodInSeconds` (`validate_action_timing_fs`). Never
  `Session.*`, `Service.*`, PATH `Param.*`, `Step.*`, or `Task.*` (§9 item 3, §9.9 item 2).
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

**Numeric `@fmtstring` fields** (§9.3 note; `SERVICE_PORT_CONSTRAINT`,
`SERVICE_SECONDS_CONSTRAINT`, `SERVICE_MAX_ATTEMPTS_CONSTRAINT`) join the resolved-value
constraint table below with target type `int?`: `port` 1–65535 (`must be between 1 and
65535.`), the four `<ServiceHealthCheck>` numeric fields > 0 (`must be > 0.`), `maxAttempts` ≥
0 (`must be >= 0.`), non-integers `must be an integer.`, a whole-field `null` accepted as "not
provided" (for a `STDOUT` check's `healthIntervalSeconds`: no heartbeat). The messages match pass 11's checks on the literal forms and job creation's on the
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
| Service `readinessIntervalSeconds` / `readinessTimeoutSeconds` / `healthIntervalSeconds` / `failureThreshold` (SERVICE, §9.3) | `int?` | soft cap: 100 chars | coerced integer > 0; `null` = §9.3 default (STDOUT `healthIntervalSeconds`: no heartbeat) |
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
- **Hooks follow `runScope`** (constraint 6, §9.9 item 6; `WRAP_ACTIONS` with `SERVICE`,
  replacing the all-or-nothing rule): a *wrapping* environment — one that defines any wrap hook
  (`has_any_wrap_hook`) — must define `onWrapEnvEnter` and `onWrapEnvExit`; must define
  `onWrapTaskRun` iff `runs_in(Task)`; and must define all four `onWrapService*` hooks iff
  `runs_in(Service)`. For a Job Environment (or an Environment Template's `environment`) the
  *effective* `runScope` decides: absent, it is every kind — so RFC 0008's three hooks alone
  are incomplete once `SERVICE` is declared — unless the environment lists a Service in
  `dependencies`, in which case it defaults to `[TASK]` and RFC 0008's three hooks are exactly
  right; `runScope: [TASK]` is likewise the RFC 0008 rule. A wrapping Step Environment has no
  `runScope` (pass 11 rejects one) and is entered only by the Task Sessions of its Step, so it
  must define exactly RFC 0008's three hooks (`EffectiveRunScope::of_step_environment`): the
  `onWrapService*` hooks are rejected on it, and a missing `onWrapTaskRun` is reported. Each
  group with missing hooks is one error at the `actions` path naming the required hooks, the
  `runScope` as written (or `default runScope: every kind of Session` / `default runScope:
  [TASK], since the environment depends on a Service` / `a Step Environment, entered only by
  Task Sessions`), and the missing ones:
  - `a wrapping environment must define onWrapEnvEnter and onWrapEnvExit whatever its runScope;
    missing: <hooks> (RFC 0009).`
  - `a wrapping environment whose runScope includes TASK (<runScope>) must define onWrapTaskRun;
    missing: onWrapTaskRun (RFC 0009).`
  - `a wrapping environment whose runScope includes SERVICE (<runScope>) must define
    onWrapServiceEnter, onWrapServiceRun, onWrapServiceHealthCheck, and onWrapServiceExit;
    missing: <hooks> (RFC 0009).`

  Each hook the `runScope` does not call for is one error on the hook's own path: `<hook> must
  not be defined: this environment's runScope (<runScope>) excludes TASK|SERVICE (RFC 0009).`
  Unrecognized `runScope` names (rejected by pass 11) never match a kind, so the rule is
  evaluated over the recognized names only. A non-wrapping environment is not subject to the
  rule whatever its `runScope`.

- **Single-wrap-layer rule**: at most one environment reachable in a session may define
  wrap hooks. A session's environment stack is the job's `jobEnvironments` plus exactly
  one step's `stepEnvironments`, so this is enforced per step: for every step, the count
  of wrap-defining envs in `jobEnvironments` plus that step's `stepEnvironments` must be
  ≤ 1. Multiple wrap envs in `jobEnvironments` alone are reported once at the
  `jobEnvironments` path (reachable from every session); a step that adds its own wrap env
  on top is reported at that step's `stepEnvironments` path. A Service Session's stack is a
  subset of every Task Session's (the Job's environments whose `runScope` includes
  `SERVICE`), so the same check covers Service Sessions.

The single-layer rule runs only in the job-template path. An environment template
defines at most one environment, so the rule is trivially satisfied for an isolated env template; if
separately-validated env templates are composed into a session at assembly time
(worker-side), the cross-layer constraint must be enforced there. Likewise the §1.2.2 item 4
rule — a wrapping environment from a document that does not declare `SERVICE` may not meet a
Service in the combined Job's `jobEnvironments` — relates documents only the scheduler sees
together and is not a template-validation check: it is one of the submission-time checks
`apply_environment_templates` runs on the combined Job (see "Submission-time checks" below
and [job-creation.md](job-creation.md)).

## Pass 11: SERVICE Gating and Structure

Validates or rejects features gated behind `SERVICE` (RFC 0009, Template Schemas §1.1 items
8–9, §1.2, §4 item 3, §9–§9.8). Error paths are `services[i] -> …`, `requiresServices[i] ->
…`, and, in an environment template, `services[i] -> …`.

**Gating (extension not declared)** — `gate_services_job_template` /
`gate_services_environment_template`, run **before pass 5** so these errors lead the list
(exploratory report S10):
- `services` → `services requires the SERVICE extension.` (job and environment templates);
  `requiresServices` → `requiresServices requires the SERVICE extension.` The list's contents
  are not examined.
- `runScope` on any `<Environment>` (`jobEnvironments[i]`, `steps[i] -> stepEnvironments[j]`,
  or the environment template's `environment`) → `runScope requires the SERVICE extension.`
  on the `runScope` path. The list's contents are not examined.
- `dependencies` on any `<Environment>` (the same three positions) → `dependencies requires
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
rename hint, see [parsing.md](parsing.md) "Pass 3" — as is `requiresServices` on an environment
template (§9.9 item 11).

**EXPR prerequisite (§9.9 item 7):** declaring `SERVICE` without `EXPR` is an error at
`extensions`, in both job and environment templates, whether or not any Service is defined:
``SERVICE requires EXPR; both must be listed in the template's `extensions` (RFC 0009).``

**With the extension declared:**
- **Lists** (§1.1 items 8–9, §1.2): each `services` / `requiresServices` list `must not be
  empty.` and `must not contain more than 10 elements.`
- **`runScope`** (§4 item 4, §9.9 item 3; `validate_run_scope`):
  - On a `stepEnvironments` entry the field is rejected whole, whatever it names — even
    `[TASK]`, the only kind that could enter it (constraint 4; conformance
    `4--run-scope-on-step-environment.invalid`): `runScope is not permitted on a Step
    Environment: a Step Environment is entered only by the Task Sessions of its Step, so there
    is no kind of Session for it to choose (Template Schemas §4 item 4 constraint 4).` Nothing
    inside is examined. A Step Environment is always a Task-Session site (pass 8 seeds its
    Step's Services unconditionally), so a reference beside the list is not a second error.
  - Elsewhere (a `jobEnvironments` entry, an Environment Template's `environment`): `must not
    be empty.` on the `runScope` path; on each offending element's path (`runScope[i]`),
    `unknown run scope name '<name>'; expected one of TASK, SERVICE.` (names are
    case-sensitive) or `duplicate run scope name '<name>'.` Then, once on the `runScope` path,
    when the list includes `SERVICE` and the Environment lists a Service in `dependencies`
    and/or references `Service.*` (constraint 2 / §4 item 3 constraint 5 / §9.9 item 2):
    `Environment 'Client' is entered in Service Sessions (its runScope includes SERVICE) and
    may not depend on a Service; declare runScope: [TASK] if it configures Tasks.` — `… may not
    reference Service.*; …` when it only references, `… may not depend on or reference a
    Service; …` when it does both. The per-reference `Undefined variable` errors pass 8 found
    for the Environment are dropped by `service_diagnostics` in favor of this one error
    (exploratory report S12, S21). An absent `runScope` defaults to `[TASK]` for an Environment
    that lists a Service and needs no check.
- **An Environment's `dependencies`** (§4 item 3, §3.2 constraint 5, §9.9 item 14;
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
    requiresServices.` (Job Template) or `… in this document's services.` (Environment
    Template, whose `environment` may list only a Service of the same document — so a document
    with no `services` can list nothing), and a repeat reports `duplicate dependency 'service:
    X'.` (an unknown duplicate gets both).
  - An explicit `runScope` that includes `SERVICE` together with the list is the `runScope`
    check's (above): one error on the `runScope` path, naming the dependency, the reference,
    or both.
  - A Job Environment's entries join the dependency graph as an edge from every Step to each
    Service listed (§3.2 constraint 3, §9.1 rule 3); see "Dependencies" below.
- **Name uniqueness** (§9.9 item 5): `duplicate service name: '<name>'` on the offending
  `services` element for a repeat within the list; `duplicate service requirement name:
  '<name>'` on the offending `requiresServices` element; and, on `requiresServices[i] ->
  name`, `'<name>' is also declared in services; a Service is either declared or required, not
  both.`
- **`<ServiceName>`, requirement names, and port names** (§9.2, §9.3 item 1, §9.8, §9.9 item
  5): on the `name` field, `'<name>' is not a valid identifier.`, `exceeds
  <max_identifier_len> characters.` (64, or 512 with FEATURE_BUNDLE_1), and `must not be
  'File'; it is reserved for Service.File.* references.`
- **Dependencies** (§3.2 constraints 1–3, §9 item 4, §9.1, §9.9 items 9–11;
  `validate_job_template_dependencies`), job template:
  - A Step's `service` entries (the one-of rule, its `dependsOn` entries, self-dependency and
    duplicates are pass 6's): on `steps[i] -> dependencies[j]`, an unknown name reports
    `dependency 'service: Nope' not found: no Service of that name in services or
    requiresServices.` — or, when a Step of that name exists, `dependency 'Prep' names no
    Service; did you mean 'dependsOn: Prep'?`
  - A Service's `dependencies`, checked wholly here: `services[k] -> dependencies` `must not be
    empty.`; on `services[k] -> dependencies[j]`, a malformed entry as in pass 6, `dependency
    '<step>' not found.` for an unknown Step (`dependency 'Back' names no Step; did you mean
    'service: Back'?` when a Service of that name exists), `cannot depend on itself.` for
    `service: <own name>`, the unknown-Service message above for an unknown `service` name, and
    `duplicate dependency '…'.`
  - The combined graph (Step→Step, Step→Service, Service→Step, Service→Service edges, plus an
    edge from every Step to each Service a Job Environment lists, since every Step is in its
    scope; `compute_service_scopes`) must be acyclic. The first cycle is reported at the root
    path, each node spelled `Step X` or `Service X`: `dependencies contain a cycle: Step Use ->
    Service Indexer -> Step Use.`; when a Job Environment's scope edge closed it, the message
    names it: `dependencies contain a cycle: Step Prepare -> Service Indexer -> Step Prepare
    (Job Environment 'Client' lists Service 'Indexer', so every Step depends on it; a Service a
    Job Environment lists, or any Service it depends on, cannot depend on a Step).` (conformance
    `9--dependencies-cycle-via-job-environment-scope*.invalid`). A `service` entry naming a
    required Service is not an edge. When there is a cycle the scopes are undefined and the
    check below is skipped. List order carries no meaning.
  - An unused Service — empty computed scope (§9.1 rule 4: "a Service in whose scope no Step
    falls"; a Service listed only by Services that are themselves unused is unused) — reports
    on `services[k]` (`unused_service_message`): when nothing lists it, `Service 'Cache' is
    unused: no Step, Service, or Job Environment lists it.`; when only other — unused —
    Services list it, they are named: `Service 'Backend' is unused: no Step is in its scope
    (Service 'Proxy' lists it, but no Step is in Proxy's scope either).` / `… (Services 'Side'
    and 'Car' list it, but no Step is in their scope either).` (exploratory report S4: the
    earlier wording claimed nothing listed it). A Job Environment that *references* the Service
    without listing it does not keep it used (and is itself a pass 8 error).
- **Dependencies, environment template** (§1.2 item 6, §3.2 constraint 4, §9 item 4;
  `validate_environment_template_dependencies`): `services[k] -> dependencies` `must not be
  empty.`; on `services[k] -> dependencies[j]`, a malformed entry as in pass 6, a `dependsOn`
  entry reports `dependency 'dependsOn: Prepare' names a Step, but an Environment Template has
  no Steps; a Service here may depend only on a Service of the same document, as 'service:
  <name>'.` (with the `did you mean 'service: Prepare'?` hint when a Service of that name
  exists), an unknown name `dependency 'service: Nope' not found: no Service of that name in
  this document's services.`, plus `cannot depend on itself.` and `duplicate dependency
  '…'.`; a cycle among the `service` entries (`service_dependency_cycle`) reports
  `dependencies contain a cycle: Service A -> Service B -> Service A.` at `services`. Scope is
  not checked: every Step of every Job is in an external Service's scope.
- **Requirement ports** (§9.8 item 2): `requiresServices[i] -> ports` `must not be empty.` /
  `must not contain more than 10 elements.`; `duplicate port name '<name>'.` on the element;
  the identifier rules above on `ports[j] -> name`. `protocol` is the same serde enum as a
  `<ServicePort>`'s.
- **Ports** (§9 item 6): `ports` `must not be empty.` / `must not contain more than 10
  elements.`; `duplicate port name '<name>'.` on the element.
- **Port numbers per protocol** (§9 item 6.4, §9.9 item 8): on `ports[i] -> port` of the later
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
  `healthCheck.failureThreshold` `must be > 0.`; `restartPolicy.maxAttempts` `must be >= 0.`;
  any of them `must be an integer.` when the text does not parse.
- **`completedTasks` required** (§9.5 item 2, §9.9 item 12): a literal `maxAttempts` greater
  than 0 without `completedTasks` reports, on `restartPolicy`, `completedTasks must be provided
  when maxAttempts is greater than 0 (maxAttempts is 2): a template that allows relaunch must
  say what a relaunch means for completed Tasks, KEEP or RERUN (Template Schemas §9.5 item 2).`
  (`service::completed_tasks_required_message`). `maxAttempts` 0, explicit or defaulted, needs
  none; a format-string `maxAttempts` is checked with the same message when it is resolved at
  job creation (see [job-creation.md](job-creation.md)).
- **Health-check consistency** (§9.4 items 3 and 6, §9.7 item 3, §9.9 item 4):
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
- **Health check and port protocols** (§9 item 7, §9.4 item 2, §9.9 item 4): a `TCP_CONNECT`
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

Not in this pass: `Service.*` scope rules (§9.9 items 1–2; pass 8, "Service scopes"), the
wrap hooks an Environment's `runScope` calls for (item 6; pass 10), `let` bindings and format
strings inside a Service (pass 8), job creation of Services (see
[job-creation.md](job-creation.md)), and the submission-time checks (next section).

## Submission-time checks (RFC 0009, Template Schemas §1.2.2 items 2–4, §9.9 closing paragraph)

Two of the SERVICE extension's rules relate documents that only the scheduler sees together
— a Job Template and the Environment Templates attached to a submission — and so cannot run
when a template is validated on its own. They run in `apply_environment_templates`
(`job/create_job/external.rs`), against the combined Job, after `create_job` and before any
external Service is instantiated; every violation is collected into one `ModelValidation`
error whose model name is `Submission` and whose paths are rooted at the document
(`JobTemplate`, `EnvironmentTemplate[i]` by attachment index, or the label the caller gave
the attachment):

- **Requirement matching (§1.2.2 item 2).** Each entry of the Job Template's
  `requiresServices` must match exactly one attached Service with the same `name`, which must
  declare every listed port with the same `protocol`. Reported at `JobTemplate ->
  requiresServices[i]` as `required Service '<r>' is not provided: no Environment Template is
  attached …` / `… none of the attached Environment Templates (<docs>) defines a Service named
  '<r>' …`, `required Service '<r>' is ambiguous: <n> attached Environment Templates define a
  Service with that name (<docs>); a requirement must match exactly one …`, `required Service
  '<r>' is provided by <doc>, which is missing port '<p>'; its ports: <names> …`, or
  `required Service '<r>' is provided by <doc>, whose port '<p>' has protocol <P> but the
  requirement declares <Q> …`, each ending `(Template Schemas §1.2.2 item 2).` Matching uses
  the requirement alone; a match is recorded as a `RequirementBinding`.
- **Inline Services shadow external ones (§1.2.2 item 3) — not a check.** An external
  Service's `name` may equal that of another external Service or of a Service in the Job
  Template's `services`; the submission is not rejected for it unless a requirement names the
  duplicated external name (the ambiguity above). The Job Template's `Service.*` references
  resolve to its own Services first (pass 8 seeds the document's own Services and its
  requirements), and the merge keeps same-named Services distinct by stamping each with its
  `job::Document` (see "apply_environment_templates" in [job-creation.md](job-creation.md)).
  Within one document the §9.9 item 5 uniqueness check (pass 11) applies as before.
- **Wrapping Environment from a SERVICE-less document (§1.2.2 item 4).** Such an Environment
  (any `WRAP_ACTIONS` hook defined; `runScope` and the `onWrapService*` hooks are both gated
  behind `SERVICE`, so it necessarily has the default scope and no Service hooks) must not be
  in the combined Job's `jobEnvironments` when the combined Job has any Service, since Service
  Sessions enter exactly those Environments. Reported at `<doc> -> environment` or
  `JobTemplate -> jobEnvironments[i]`, naming the document as the cause and quoting the spec's
  remedy: `wrapping Environment '<e>' is defined by <doc|the Job Template>, which does not
  declare the SERVICE extension, so it has the default runScope (every kind of Session) and
  cannot define the onWrapService* hooks; but the combined Job places <Service> in its scope,
  and the Service would run in a Session the Environment enters but cannot wrap. Declare
  SERVICE in <doc> and either define onWrapServiceEnter, onWrapServiceRun,
  onWrapServiceHealthCheck, and onWrapServiceExit, or declare a runScope that excludes SERVICE
  (RFC 0009, Template Schemas §1.2.2 item 4).` A wrapping Environment in a Step's
  `stepEnvironments` is never entered by a Service Session and is not checked. A document that
  declares `SERVICE` is governed by pass 10's hooks-follow-`runScope` rule instead and is never
  reported here.

The 10-element cap on `services` (pass 11) is per document; the combined list is not capped.
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
