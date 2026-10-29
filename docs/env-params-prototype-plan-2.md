# ENV_PARAMS prototype plan, part 2: runScope, hostRequirements, entry enableIf, Job.EnvNames

This plan brings the `ENV_PARAMS` prototype on `env-params-prototype` up to date with the revised specification and
orders the remaining work into PR-sized increments. It changes no code. Each increment is made later through the
`implement-change` skill.

The first prototype's plan, [env-params-prototype-plan.md](env-params-prototype-plan.md), is the context for this one.
Its design decisions 1–9, feature inventory (G, D, E, X, V, S, C items), and ambiguities A1–A22 still hold unless this
plan says otherwise. This plan adds only what the specification changed since then.

## Sources

* **Normative spec.** `~/projects/rfc-env-params/openjd-specifications-ep`, branch `rfc-env-params` at `408910f`:
  `rfcs/0010-environment-parameters.md` ("RFC"; renamed from `NNNN-environment-parameters.md`),
  `wiki/2023-09-Template-Schemas.md` ("Schemas"), `wiki/How-Jobs-Are-Run.md` ("HJAR"), and
  `wiki/2026-02-Expression-Language.md` ("ExprLang"). Section numbers (§3.2, §4, …) are Schemas sections. The
  first plan was written against `cbc1f27`; `git diff cbc1f27..408910f -- wiki rfcs samples` is the specification
  change this plan covers.
* **Conformance suite.** `conformance-tests/2023-09/ENV_PARAMS/` on the same branch: 123 `job_templates/`, 38
  `env_templates/`, and 92 `jobs/` files. Its `README.md` matrix gives each test an ID (JT-nn, ET-nn, JB-nn) and lists
  16 "Rules without a definite test"; `AUDIT.md` records the suite's audit.
* **Code baseline.** This repo at `a758062`, the head of `env-params-prototype` after the first prototype's five PRs.
  All `file:line` citations refer to that commit.

## What the specification changed

| # | Change | Spec |
|---|--------|------|
| N1 | `runScope: ALL \| ONLY_REQUESTED` on `<Environment>`, default `ALL`. `ONLY_REQUESTED` is entered only by the Sessions of a Step that requests it; any other Session does not enter it, set its `variables`, or have it in its stack. Unknown values rejected. Not permitted on `stepEnvironments`. | §4 item 4 |
| N2 | Constraint 7 and Session sharing restated over the Step's **Environment set**: every `ALL` Environment of the Job, every `ONLY_REQUESTED` Environment the Step requests, and the Step's own `stepEnvironments`. Effective values exist only for Environments in the set. | §3.2 constraint 7, §3.2.1 last paragraphs, §3.2.2 ¶4; HJAR |
| N3 | `enableIf: <bool> \| <FormatString>` on the `environment:` entry, with `EXPR`. Same scopes as a `parameters` value. Resolved at job creation before name resolution; `null` is an error. A disabled entry is as if absent. Checks that depend on whether an entry is enabled are made at template validation only for an absent or literal `enableIf`, otherwise at job creation. Constraint 3 applies to entries as written. | §3.2 items 2, 5, constraints 1, 9, checked-at paragraph; §3.2.1 intro; §3.2.2 item 1 |
| N4 | `Job.EnvNames`, `list[string]`: the names of the Job Environments including attached ones, whatever their `runScope`, in the order a Session that entered all of them would enter them; not Step Environments; repeated when two attached Environments share a name. `unresolved[list[string]]` at template validation, concrete at job creation before any TEMPLATE-scope expression. Not available in an Environment parameter `default` or the Job Template `name`. Requires `ENV_PARAMS` and `EXPR`. | §7.3.1 row; ExprLang §1.2.2 |
| N5 | `hostRequirements` on `<Environment>`, with the Step's shape and constraints. Scopes: the document's `RawParam.*` and non-PATH `Param.*`, with `EXPR` `Job.Name` and `Job.EnvNames`, and the Environment's own non-PATH `Env.Param.*` / `Env.RawParam.*`; nothing else. Resolved once per entering Step with that Step's effective values. Not permitted on `stepEnvironments`. | §4 item 5 |
| N6 | **Combined host requirements**: the Step's own plus every entered Environment's, as a conjunction. Same-capability requirements from different sources (names compare case-insensitively) normalize: amounts take the greatest `min` (missing = 0) and least `max` (missing = unbounded), rejecting `min` > `max`; two `anyOf` intersect (case-insensitive), rejecting an empty result; two `allOf` unite; `anyOf` with `allOf` stay as two. Amounts do not add. §3.3 list-length and duplicate-name limits apply per source. The rejection names the Step, the conflicting sources, and the capability. An Environment a Step does not enter contributes nothing. | §3.3.3 |
| N7 | §7.4 Job creation order: attached Environment Templates determined → `Job.EnvNames` concrete → Step `let` → entry `enableIf` → resolve enabled entries (§3.2.1) → each Step's Environment set → effective values → each Environment's `hostRequirements` resolved per Step and combined. | §7.4 Job creation row |
| N8 | Erratum: `<nonnegativefloatstring>` is non-negative, so `min: "0"` is valid. | §3.3.1 |
| N9 | Spec samples `conda-env-params.yaml`, `docker-env-params.yaml`, `video-sfm-composite.yaml` updated to the new design. | `samples/` |

The `SERVICE` and `CONTROL_FLOW` composition text (for example the `CONTROL_FLOW` sentence in the `Job.EnvNames`
row) is out of scope: neither extension is on this branch.

**Already satisfied.** N8: the prototype rejects only `min < 0.0`, at decode (`structure.rs:678-680`), in the
format-string pass (`format_strings.rs:1929-1932`, `positive: false`), and at job creation (`instantiate.rs:797-800`).
PR A adds a test that pins `min: "0"` under `FEATURE_BUNDLE_1`; no code change is expected.

## Baseline

Built with `cargo build --release` at `a758062` and run from the spec checkout at `408910f` with
`PATH="<worktree>/target/release:$PATH" uv run --with pyyaml python3.12 run_openjd_cli_tests.py '<pattern>'`. The
runner needed no change to work with this binary.

| Suite | job_templates | env_templates | jobs | Total |
|-------|---------------|---------------|------|-------|
| `2023-09/ENV_PARAMS/*` | 109 / 14 | 35 / 3 | 52 / 37 | **196 passed, 54 failed** |
| `2023-09/base/*` | 455 / 0 | 40 / 0 | 196 / 0 | 691 passed, 0 failed |
| `2023-09/EXPR/*` | 220 / 0 | 6 / 0 | 157 / 0 | 383 passed, 0 failed |
| `2023-09/WRAP_ACTIONS/*` | (none) | 9 / 0 | 64 / 0 | 73 passed, 0 failed |

Cells are passed / failed. Three `ENV_PARAMS/jobs/` files have `runOn: [windows]` and were skipped on Linux
(`4.5--list-path-elements-mapped-windows`, `4.5--path-value-mapped-in-env-param-windows`,
`4.5--path-value-unmapped-in-env-raw-param-windows`), so 89 of 92 ran.

An independent run by the conformance-suite lead against the same commit gave the same failing set, file for file.

**No regressions.** base, EXPR, and WRAP_ACTIONS pass in full, so the plan has no prototype-branch fix to add.

## Classified ENV_PARAMS failures

Every failure is class **(a)**, a feature the prototype lacks. There is no class (b) failure (a round-3 behavior the
prototype has but gets wrong) and no class (c) failure (suspected test bug or spec ambiguity). Each template failure
is `unknown field` for `runScope`, `hostRequirements`, or `enableIf`, or `Undefined variable: 'Job.EnvNames'`. Each
`jobs/` failure stops at one of those errors before any Session runs, so the output the test expects is missing.

The **Lacks** column names the new features the test uses (generated by script from each file), and "§7.4 order"
where it reads `Job.EnvNames` together with a `let`, an `enableIf`, or attached Environments. The **PR** column gives the PR expected to flip it: the PR
that lands the last of those features. `A*` means PR A flips it only if PR A includes the `openjd run` wiring of
[decision P4](#design-decisions); otherwise PR C does. A test is `A*` only when some Step's Sessions must *not* enter
an `ONLY_REQUESTED` Environment; when every Step requests it, entering every Job Environment, as `openjd run` does
today, is already correct. This was checked by running copies of the candidate tests with `runScope` removed against
the baseline binary: `3.2.1--external-required-false-present-receives-values`, `3.2.1--several-attached-each-own-values`,
and `4--only-requested-entered-by-requesting-step` passed. The 8 copies that use no other new field failed: six show
an `ONLY_REQUESTED` Environment entered, or its variables or wrapper applied, for a Step that does not request it;
two (`3.2.1--attached-only-requested-required-not-requested`, `4--example-parameterized-job-environment`) are
rejected by constraint 7 for the non-requesting Step, which PR A's model change removes, after which that Step must
not enter an Environment it has no values for. The copies that also use
`enableIf`, `hostRequirements`, or `Job.EnvNames` still fail on that field, so they were classified by reading:
`3.2--enable-if-true-enters`, `3.2--example-choose-conda-when-alone`, and
`3.3.3--example-environment-host-requirements` have every Step requesting (or one enabled requester), so they are `A`
or `B`; the rest have a Step that must not enter the Environment.

By the first error the baseline binary reports, the 54 failures are rejected on `runScope` 24, entry `enableIf` 6,
`Job.EnvNames` 12, and Environment `hostRequirements` 12 (this matches the conformance-suite lead's count). Many tests
use more than one feature: `Job.EnvNames` appears in 18 of them. §7.4 ordering is needed by every test that combines
`Job.EnvNames` with `let`, `enableIf`, or attached Environments, and N8 by none.

### job_templates (14)

| Test | Lacks | PR |
|------|-------|----|
| `3.2--enable-if-expression.yaml` | `enableIf`, `Job.EnvNames`, §7.4 order | A |
| `3.2--enable-if-literal.yaml` | `enableIf` | A |
| `3.2--enable-if-reads-step-name-and-let.yaml` | `enableIf` | A |
| `3.2--only-requested-entry-disabled-literal.yaml` | `runScope` (`ONLY_REQUESTED`), `enableIf` | A |
| `3.2--only-requested-required-param-non-requesting-step.yaml` | `runScope` (`ONLY_REQUESTED`) | A |
| `3.2--value-reads-env-names.yaml` | `Job.EnvNames` | A |
| `4--run-scope-all.yaml` | `runScope` (`ALL`) | A |
| `4--run-scope-only-requested.yaml` | `runScope` (`ONLY_REQUESTED`) | A |
| `7.3.1--env-names-in-step-let.yaml` | `Job.EnvNames`, §7.4 order | A |
| `7.3.1--env-names-in-step-script.yaml` | `Job.EnvNames` | A |
| `4--host-requirements-on-job-environment.yaml` | Environment `hostRequirements` | B |
| `4--host-requirements-read-env-names.yaml` | `Job.EnvNames`, Environment `hostRequirements` | B |
| `4--host-requirements-read-env-param.yaml` | Environment `hostRequirements` | B |
| `4--host-requirements-read-job-name.yaml` | Environment `hostRequirements` | B |

### env_templates (3)

| Test | Lacks | PR |
|------|-------|----|
| `4--run-scope-only-requested.yaml` | `runScope` (`ONLY_REQUESTED`) | A |
| `7.3.1--env-names-in-on-enter.yaml` | `Job.EnvNames` | A |
| `4--host-requirements-read-env-param.yaml` | Environment `hostRequirements` | B |

### jobs (37)

| Test | Lacks | PR |
|------|-------|----|
| `1.2--job-template-without-extension-takes-defaults` | `runScope` (`ALL`) | A |
| `3.2--enable-if-false-all-takes-defaults` | `runScope` (`ALL`), `enableIf` | A |
| `3.2--enable-if-false-skips-missing-required` | `enableIf` | A |
| `3.2.1--attached-all-required-supplied-by-every-step` | `runScope` (`ALL`) | A |
| `3.2.1--external-required-false-missing-ignored` | `Job.EnvNames` | A |
| `4--all-entered-by-step-without-entry` | `runScope` (`ALL`) | A |
| `7.3.1--env-names-excludes-step-environments` | `Job.EnvNames` | A |
| `7.3.1--env-names-expands-to-argv` | `Job.EnvNames` | A |
| `7.3.1--env-names-in-environment-script` | `Job.EnvNames`, §7.4 order | A |
| `7.3.1--env-names-in-step-let-feeds-value` | `Job.EnvNames`, §7.4 order | A |
| `7.3.1--env-names-includes-attached` | `Job.EnvNames`, §7.4 order | A |
| `7.3.1--env-names-job-environments-in-order` | `Job.EnvNames` | A |
| `7.3.1--env-names-repeats-shared-attached-name` | `Job.EnvNames`, §7.4 order | A |
| `7.3.1--wrap-hook-reads-wrapper-env-param` | `runScope` (`ALL`) | A |
| `1.2--job-template-without-extension-skips-only-requested` | `runScope` (`ONLY_REQUESTED`) | A* |
| `3.2--enable-if-false-not-entered` | `runScope` (`ONLY_REQUESTED`), `enableIf` | A* |
| `3.2--enable-if-true-enters` | `runScope` (`ONLY_REQUESTED`), `enableIf` | A |
| `3.2--example-choose-conda-when-alone` | `runScope` (`ONLY_REQUESTED`), `enableIf`, `Job.EnvNames`, §7.4 order | A |
| `3.2--example-choose-docker-when-both` | `runScope` (`ONLY_REQUESTED`), `enableIf`, `Job.EnvNames`, §7.4 order | A* |
| `3.2.1--attached-only-requested-required-not-requested` | `runScope` (`ONLY_REQUESTED`) | A* |
| `3.2.1--external-required-false-present-receives-values` | `runScope` (`ONLY_REQUESTED`) | A |
| `3.2.1--several-attached-each-own-values` | `runScope` (`ONLY_REQUESTED`) | A |
| `4--attached-only-requested-not-entered-without-request` | `runScope` (`ONLY_REQUESTED`) | A* |
| `4--example-parameterized-job-environment` | `runScope` (`ONLY_REQUESTED`) | A* |
| `4--only-requested-entered-by-requesting-step` | `runScope` (`ONLY_REQUESTED`) | A |
| `4--only-requested-keeps-order` | `runScope` (`ONLY_REQUESTED`) | A* |
| `4--only-requested-not-entered-by-other-step` | `runScope` (`ONLY_REQUESTED`) | A* |
| `4--only-requested-variables-not-set-for-other-step` | `runScope` (`ONLY_REQUESTED`) | A* |
| `4--only-requested-wrapper-not-applied-to-other-step` | `runScope` (`ONLY_REQUESTED`) | A* |
| `7.3.1--env-names-includes-unrequested-only-requested` | `runScope` (`ONLY_REQUESTED`), `Job.EnvNames` | A* |
| `3.3.3--any-of-and-all-of-not-combined` | `runScope` (`ALL`), Environment `hostRequirements` | B |
| `3.3.3--any-of-intersection-ignores-case` | `runScope` (`ALL`), Environment `hostRequirements` | B |
| `3.3.3--non-conflicting-requirements-run` | `runScope` (`ALL`), Environment `hostRequirements` | B |
| `3.3.3--same-capability-in-step-and-environment` | `runScope` (`ALL`), Environment `hostRequirements` | B |
| `3.3.3--disabled-entry-requirements-not-applied` | `runScope` (`ONLY_REQUESTED`), `enableIf`, `Job.EnvNames`, Environment `hostRequirements`, §7.4 order | B* |
| `3.3.3--example-environment-host-requirements` | `runScope` (`ONLY_REQUESTED`), Environment `hostRequirements` | B |
| `3.3.3--only-requested-requirements-not-applied` | `runScope` (`ONLY_REQUESTED`), Environment `hostRequirements` | B* |

`B*` is as `A*`: PR B flips it if the `openjd run` wiring has landed by then, otherwise PR C.

### Passes that need a re-check (50)

These `.invalid` tests use a new field or symbol, so whether each still fails for its own rule can only be judged once
that field or symbol exists. Today 42 are rejected because a new field is unknown (`deny_unknown_fields` on
`Environment` and `StepDependency`), which is never the rule the test names. Seven are rejected with
`Undefined variable: 'Job.EnvNames'`; for six of them (`4.5--default-reads-env-names` in both directories,
`7.3.1--env-names-in-job-name`, `7.3.1--env-names-without-env-params`, `7.3.1--env-names-without-expr` in both
directories) that may also be the right final message, since the symbol is out of scope there, while
`7.3.1--env-names-is-list` must instead fail on its type. One fails for its own rule, noted at the end of this
section. Once a PR makes the field or symbol known, each of these must still fail, now at the right rule. The PR that
adds the field re-runs them, and its tests assert the intended message for at least the rules marked in the inventory
below. A regression here shows up as a newly *passing* `.invalid` template, which the runner reports as a failure.

* **job_templates (29):** `3.2--all-required-param-entry-disabled-literal`, `3.2--duplicate-entries-complementary-enable-if`,
  `3.2--enable-if-not-bool`, `3.2--enable-if-not-single-expression`, `3.2--enable-if-reads-path-param`,
  `3.2--enable-if-reads-task-param`, `3.2--enable-if-with-depends-on`, `3.2--enable-if-without-expr`,
  `3.2--only-requested-required-param-not-supplied`, `4--run-scope-on-step-environment`, `4--run-scope-unknown`,
  `4--run-scope-without-extension`, `4.5--default-reads-env-names`, `7.3.1--env-names-in-job-name`,
  `7.3.1--env-names-is-list`, `7.3.1--env-names-without-env-params`, `7.3.1--env-names-without-expr` (PR A);
  `4--host-requirements-duplicate-amount-name`, `4--host-requirements-empty`, `4--host-requirements-env-list-path-param`,
  `4--host-requirements-env-path-param`, `4--host-requirements-env-raw-path-param`,
  `4--host-requirements-on-step-environment`, `4--host-requirements-other-env-param`, `4--host-requirements-param-path`,
  `4--host-requirements-session`, `4--host-requirements-step-name`, `4--host-requirements-without-extension`,
  `7.3.1--env-param-in-step-host-requirements` (PR B). All with the `.invalid.yaml` suffix.
* **env_templates (8):** `4--run-scope-unknown`, `4--run-scope-without-extension`, `4.5--default-reads-env-names`,
  `7.3.1--env-names-without-expr` (PR A); `4--host-requirements-env-path-param`, `4--host-requirements-session`,
  `4--host-requirements-step-name`, `4--host-requirements-without-extension` (PR B).
* **jobs (13):** `3.2--enable-if-null`, `3.2--required-param-entry-disabled-by-expression`,
  `3.2.1--attached-all-required-entry-disabled`, `3.2.1--attached-all-required-not-supplied`,
  `3.2.1--attached-only-requested-required-not-supplied` (PR A); `3.3.3--amount-min-exceeds-max`,
  `3.3.3--attached-environment-conflict`, `3.3.3--attribute-any-of-empty-intersection`,
  `3.3.3--capability-name-compared-ignoring-case`, `3.3.3--conflict-between-two-environments`,
  `3.3.3--conflict-resolved-from-env-param`, `4--resolved-host-requirements-duplicate-name`,
  `4--resolved-host-requirements-negative-min` (PR B). All with the `.invalid.test.yaml` suffix. The `jobs/` runner
  accepts any non-zero exit for `.invalid`, so for these the PR's own tests must show the rejection comes from job
  creation with the intended message, not from template validation.

The exception is `7.3.1--env-param-in-step-host-requirements.invalid.yaml`, which already fails for its own rule
(`Env.Param` is undefined in a Step's `hostRequirements`, a Step field). It is listed because PR B touches the scope it
relies on. All 50 must still fail after PR B, each for the rule it names.

## Current code the changes touch

| Area | Today | Where |
|------|-------|-------|
| Template `<Environment>` | `name`, `description`, `parameterDefinitions`, `script`, `variables`; `deny_unknown_fields`. One type for Job, Step, and attached Environments | `crates/openjd-model/src/template/environment.rs:16-28` |
| Template `<StepDependency>` | struct of options `depends_on`, `environment`, `required`, `parameters` | `crates/openjd-model/src/template/step.rs:342-356` |
| `WRAP_ACTIONS` single-layer check | counts every `jobEnvironments` wrapper as in every Session, then adds each Step's `stepEnvironments` wrappers | `crates/openjd-model/src/template/validate_v2023_09/wrap_actions.rs:113-168` |
| ENV_PARAMS template checks | `validate_env_params_job_template`, `check_environment_entry`, `check_required_parameters_supplied` (constraint 7 against every Job Environment) | `crates/openjd-model/src/template/validate_v2023_09/env_params.rs:195`, `:460`, `:593` |
| Format-string scopes | `build_session_scope_symtab` adds `Job.Name` with `EXPR`; separate scopes for `default`, entry values, step `let`, host requirements | `format_strings.rs:112-146`, `:318`, `:1874-1877` (Step host requirements), `:2032-2043`, `:2271-2280`, `:2317` |
| Job creation | `create_job(job_template, values, ctx)` sees no Environment Templates; binds `Job.Name`, then instantiates each Step (its `let`, host requirements, Job Environment values, unresolved external entries with a frozen symbol table) | `crates/openjd-model/src/job/create_job/mod.rs:125`, `:235`, `:296-309` |
| Step host requirements | resolved and checked per Step at job creation | `instantiate.rs:521` (`resolve_host_requirements`), `:734`, `:770`, `:834` |
| Submission stage | `apply_environment_templates(job, attached, values, ctx)` resolves external entries, computes attached values, binds `Job.Name` for attached Environments; `into_combined_job` puts attached Environments first | `crates/openjd-model/src/job/create_job/external.rs:541`, `:724-727`, `:163` |
| Early external checks | `check_environment_templates`, used by `openjd check --environment` | `external.rs:779` |
| Job types | `Job::job_environments` (one list for every Session); `Step::environment_parameters` (values per Environment that declares parameters, for every Job Environment); `Step::unresolved_environment_entries`; `Step::host_requirements` | `crates/openjd-model/src/job/mod.rs:71-78`, `:105-141`, `:342-362`, `:660` |
| `openjd run` | groups consecutive Steps by equal values (`session_groups`), enters every `job_environments` entry in each Session, checks one wrap Environment per stack over `job_environments` plus `stepEnvironments` | `crates/openjd-cli/src/run/execution.rs:90-104`, `:376-408`, `:250-278` |
| `openjd summary` | lists Job Environments and per-Step values | `crates/openjd-cli/src/summary.rs` |
| Sessions | enters whatever Environment the caller passes; no API takes "the Job's Environments" (`grep job_environments crates/openjd-sessions/src` finds nothing) | `crates/openjd-sessions/src/session.rs` (`enter_environment_with_parameters`) |

## Design decisions

P1–P8 extend the first plan's decisions 1–9; where one replaces an earlier decision, it says so.

1. **P1. One job-creation entry point runs §7.4 in order.** `Job.EnvNames` must be concrete before a Step's `let`
   (N7), and it lists the attached Environments, which `create_job` does not see today (`create_job/mod.rs:125`). A
   Step `let` that reads it feeds frozen external entries, Step scripts, and `enableIf`, so it cannot be patched in
   afterwards. PR A therefore adds
   `create_job_with_environment_templates(job_template, attached: &[AttachedEnvironmentTemplate], job_parameter_values, ctx) -> Result<CreatedJob, ModelError>`,
   returning the combined Job, the per-entry Environment documents, and the submission warnings (the fields
   `AppliedEnvironmentTemplates` and `combined_environment_documents` provide today). Internally, the `create_job`
   body takes the attached Environments' names and binds `Job.EnvNames` next to `Job.Name`; the submission stage
   follows. `create_job` keeps its signature but changes behavior: it means "no attached Environments", so its Job is
   complete, and an unresolved required external entry is an error, as `apply_environment_templates` with no
   attachments reports today. The public field `job::Step::unresolved_environment_entries` and the type
   `job::UnresolvedEnvironmentEntry` are removed: the frozen entries become crate-private state passed between the two
   internal stages, since no returned Job has any. `apply_environment_templates` becomes crate-private: a Job created without the attachments' names would
   carry a wrong `Job.EnvNames` with no way to detect it. The CLI (`run`, `summary`) and openjd-for-js's `createJob`
   switch to the new function. This replaces the first plan's decision 5 as the public shape; the
   `service-extension` reconciliation it mentions becomes "merge two entry points", noted as a follow-up.
2. **P2. `Job.EnvNames` order is the order a Session enters the Environments**: the attached Environments in
   attachment order, then the Job Template's `jobEnvironments`, which is the order `into_combined_job` builds
   (`external.rs:163`) and `openjd run` enters. The suite accepts this order or the reverse (CT-1, A23).
3. **P3. Each Step carries the Job Environment part of its Environment set.** `job::Step` gains
   `environments: Option<Vec<EnvironmentRef>>`, where `EnvironmentRef { document: Document, environment: String }`
   names an entry of the combined `Job::job_environments`, in Job order. `(document, environment)` is the key
   `EnvironmentParameterValues` already uses (first plan, decision 4), and it stays meaningful if a consumer filters
   the Environment list, which an index would not. `None` means every Job Environment, the meaning without
   `ENV_PARAMS`, and is what a Job whose documents do not list `ENV_PARAMS` gets, so its serialization is unchanged
   (G6). `Step::environment_parameters` is restricted to Environments in the set (N2). `job::Environment` gains
   `run_scope: Option<RunScope>`, serialized only when present, for display; the Step's set, not this field, decides
   entry. The Step's own Step Environments are in its Environment set (N2) but not in this field; they stay on
   `Step::step_environments`, entered after the listed ones as today. The epic's phrase "replacing
   any single resolved list" is read as: the Step's list replaces `Job::job_environments` as the input to a Session,
   while `Job::job_environments` stays as the definitions the list refers to. *As implemented (PR A):*
   `None` means exactly "every Job Environment", so a Step of an `ENV_PARAMS` Job that enters every one also gets
   `None`, and a created Job never holds `Some` of the full list (`Step::same_environments` compares the field).
   `job::Environment` did not gain `run_scope`: nothing reads it yet, and adding a field to `job::Environment`
   would touch every struct literal in the sessions tests; PR C can add it if `openjd summary` needs it. The CLI's
   own wrap-stack preflight was removed, since job creation checks the single-layer rule over each Step's set, for
   every Step (so `openjd run --step` no longer exempts unselected Steps). `openjd run` also enters each Job and
   attached Environment with its own frozen `resolved_symtab`, which it previously did not, so `Job.Name` and
   `Job.EnvNames` resolve in their `onEnter`; and every job-creation error, `Submission` ones included, is followed
   by the Job Template's help text.
4. **P4. `openjd run` enters the Step's set, in PR A.** The epic assigns CLI work to the last task, but without this
   change the model's Environment set is invisible end to end and 11 `jobs/` tests (`A*` above) cannot flip until
   then. Leaving it also opens a gap: `ModelExtension::EnvParams` is already in `ALL` (`types.rs:381`), so once PR A
   decodes `runScope`, `openjd run` on the integration branch would accept `ONLY_REQUESTED` and enter it in every
   Session, the hazard the first plan's decision 9 guarded against. The change is small and local: `run_workload` enters the Environments of the group's first Step's set
   (`execution.rs:388-403`), `session_groups` also requires equal sets (`:94`), and `validate_wrap_environment_stacks`
   uses each Step's set (`:250-278`). **Lead decision:** keep this in PR A (recommended) or leave it to PR C. If left,
   the `A*` and `B*` tests move to PR C. `summary`, `check`, and openjd-for-js stay in PR C either way.
5. **P5. Entry `enableIf` lives on the template entry only.** The template `StepDependency` gains
   `enable_if: Option<EnableIf>`, `EnableIf::Literal(bool) | EnableIf::Expression(FormatString)`, decoded the way the
   first plan's decision 2 classifies values (a string is a format string). Job creation evaluates it with target type
   `bool` in the entry-value scope, after the Step's `let`, and drops a disabled entry before resolution, so no job
   type records disabled entries and the crate-private frozen-entry state of P1 needs no `enableIf` field. Template validation skips
   enabled-dependent checks for an expression `enableIf` (N3) and treats a literal `false` entry as absent for
   constraints 5–7 (see A28). *As implemented (PR A):* the expression is resolved, and type-checked at TV, with
   no target type, and its value or static type must be `bool` (or `bool | nulltype` at TV): a `bool` target would
   coerce a `null` or another type before the check could report it as such (A25).
6. **P6. Environment host requirements resolve per Step at the end of job creation.** The template `Environment`
   gains `host_requirements: Option<template::HostRequirements>`, reusing the Step type. For each Step and each
   Environment in its set that has them, the requirements are resolved with a symbol table of the Environment
   document's non-PATH `Param.*`, `RawParam.*`, `Job.Name` and `Job.EnvNames` with `EXPR`, and the Step's non-PATH
   effective `Env.Param.*` / `Env.RawParam.*` for that Environment, then checked by the existing per-source checks
   (`instantiate.rs:734`, `:770`, `:834`, generalized from their Step-only paths). The Job Template's Environments are
   resolved in the `create_job` body, attached ones in the submission stage, and the combination runs once, after
   both, over every source. *As implemented (PR B):* every Environment, the Job Template's own and the attached
   ones, is resolved in the submission stage, over the combined Job, since that is where each Step's Environment set
   and effective values are final. The table holds the merged Job Parameters rather than only the document's
   non-PATH ones; template validation has already rejected every other reference. Steps with equal effective values
   for an Environment share one resolution, so an error is reported once, naming those Steps. A resolution error is a
   `Submission` error at the Environment's field with "Resolved for Step 'A'." appended; a Step's own requirements
   keep their earlier errors. The per-source checks this paragraph cites in `instantiate.rs` (at `a758062`) moved
   to `job/create_job/host_requirements.rs`, generalized over a `Site` that names where errors are reported.
7. **P7. The combined set replaces `Step::host_requirements`, in the same shape.** A consumer that reads a Step's
   requirements reads the combined ones (N6). Normalization follows §3.3.3: same-name amounts (case-insensitive)
   become one; same-name `anyOf`s become one with the intersection; same-name `allOf`s become one with the union; a
   same-name `anyOf` and `allOf` stay two entries. The merged entry keeps the spelling of its first source, in the
   order Step, then the Environments in the Step's set order, and an `anyOf` intersection keeps the first source's
   value order and spelling. The job-type `HostRequirements` is a list, so two entries with one name are
   representable; its doc comment says names may repeat only as an `anyOf`/`allOf` pair. Conflicts are collected
   into one `ModelValidation` error, one message per Step and capability, naming the Step, each source (`step`,
   `jobEnvironments[i] (<name>)`, or the attachment label and Environment name), and the capability. No new public
   function is planned; schedulers read the result from the Job. *As implemented (PR B):* a capability that only one
   source names is kept as written, so a requirement with both `anyOf` and `allOf` stays one entry; when several
   sources name it, the merged `anyOf` entry precedes the merged `allOf` entry. A missing `min` counts as 0 but stays
   missing when every source omits it. Each source is named by its `... -> hostRequirements` path, with the
   Environment's name for an Environment; a conflict is reported once per Step and capability, at
   `JobTemplate -> steps[i]`, under the `Submission` model.
8. **P8. Gating follows the containing document.** `runScope` and `hostRequirements` need `ENV_PARAMS` in the document
   that contains the Environment, `enableIf` needs `EXPR` in the document that contains the entry, and `Job.EnvNames`
   needs both in the document that references it, as the first plan's G2 and G4 do for the other fields.

## Ambiguities and the prototype's interpretation

Numbered after the first plan's A1–A22. The conformance README's "Rules without a definite test" items are cited as
CT-n.

| # | Ambiguity | Interpretation | Clarify spec? |
|---|-----------|----------------|---------------|
| A23 | Where attached Environments fall in `Job.EnvNames` (CT-1). | Attached first, in attachment order (P2), matching the order `openjd run` enters them. | Yes: the base spec should state the entry order of attached Environments. |
| A24 | `runScope` case (CT-8). | Case-sensitive, as other 2023-09 enums; `all` is rejected. | Yes, one word. |
| A25 | Whether `enableIf` coerces a `string` to `bool` (CT-11). | No. The expression's static type must be `bool` or `bool?` (constraint 9 says "of type `bool`"); a `null` result is a job-creation error (item 5). This keeps `3.2--enable-if-null.invalid` (JB-46) a job-creation test (CT-13). | Yes: say whether `bool?` passes template validation. |
| A26 | Empty `hostRequirements` on an Environment Template (CT-12). | Rejected, as on a Job Environment: §4 item 5 gives the Step's "same shape and constraints". | Yes. |
| A27 | `RawParam.<PATH>` of a Job Parameter in an Environment's `hostRequirements` (CT-14). | Permitted, as in a `default` and an entry value; only `Param.<PATH>` / `Param.<LIST[PATH]>` is rejected. | Yes: move the parenthetical in §4 item 5.1. |
| A28 | Constraints 2, 5, 6 on a literal `enableIf: false` entry (CT-15). | Constraints 1–4, 8, and 9 apply to every entry as written; 5–7 are skipped for a literal-`false` entry, which "has no effect of any kind". | Yes. |
| A29 | §3.3.3 "missing `min` is 0" versus a capability's defined minimum (CT-16). | Use 0, as §3.3.3 says. The prototype has no table of capability minimums to apply. | Yes. |
| A30 | Which spelling a merged capability name or `anyOf` value keeps. | The first source's, in Step-then-Environment order (P7). | No; any spelling is equal under the case-insensitive rule. |
| A31 | `Job.EnvNames` in a Step's own `hostRequirements`. §4 item 5 names it for an Environment's; the §7.3.1 row makes it available "in every format string … resolved at Job creation or later". | Available, by §7.3.1. | No. |
| A32 | Whether an Environment Template's own `Job.EnvNames` reference needs its own `EXPR`. | Yes, per document (P8). | No. |

## Feature inventory

"TV" is template validation, "JC" is job creation (the `create_job` body), "SUB" is the submission stage, now inside
`create_job_with_environment_templates`.

| # | Statement | Spec | Stage | Module | PR |
|---|-----------|------|-------|--------|----|
| R1 | `runScope` decodes as `RunScope { All, OnlyRequested }`; unknown value rejected | §4 item 4, constraint 1 | decode | `template/environment.rs` | A |
| R2 | `runScope` requires `ENV_PARAMS`; not on `stepEnvironments` | §4 item 4 constraint 2 | TV | `validate_v2023_09/env_params.rs` | A |
| R3 | Step Environment set = `ALL` Environments + requested `ONLY_REQUESTED` (enabled entries) + own Step Environments, in Job order | §4 item 4; §3.2 item 2; §3.2.2 ¶4 | JC, SUB | `job/create_job/env_params.rs`, `external.rs`; `job::Step::environments` (P3) | A |
| R4 | Effective values and constraint 7 over the set only; a non-requesting Step does not supply an `ONLY_REQUESTED` Environment's required parameter | §3.2 constraint 7; §3.2.1 last ¶ | TV (absent or literal `enableIf`), JC, SUB | `env_params.rs` `check_required_parameters_supplied` (`:593`), `job/create_job/env_params.rs`, `external.rs`, `check_environment_templates` | A |
| R5 | A Job Template without `ENV_PARAMS` never enters an attached `ONLY_REQUESTED` Environment | §1.2 item 3; README JB tests `1.2--*` | SUB | `external.rs` | A |
| R6 | Sharing rule: equal sets as well as equal values | §3.2.2 ¶4; HJAR | API, CLI | `Step::environment_values_equal` doc or a new `Step::same_environments`; `openjd run` `session_groups` (P4) | A |
| R7 | `openjd run` enters the Step's set; wrap-stack check per set | §4 item 4 `ONLY_REQUESTED` | CLI | `run/execution.rs` (P4) | A (or C) |
| R8 | The `WRAP_ACTIONS` single-layer rule is checked per Step over its Environment set: two wrapping `ONLY_REQUESTED` Job Environments requested by different Steps are valid. At template validation only for entries with an absent or literal `enableIf`; otherwise at job creation, over each Step's set | §4.3 `WRAP_ACTIONS` extension constraints item 2 (single-layer rule); §4 item 4; §3.2 checked-at ¶ | TV, JC, SUB | `validate_v2023_09/wrap_actions.rs:113-168`, which today counts every `jobEnvironments` wrapper as "shared by every session"; a per-Step check at job creation over the combined Job (attached wrappers included) | A |
| R9 | Error messages that say every Session enters every Job or attached Environment are restated for `runScope` | §3.2 constraint 7 | TV, SUB | messages: `validate_v2023_09/env_params.rs:189` with its `why` argument at `:657`, `job/create_job/env_params.rs:783`, `job/create_job/external.rs:479` (`EVERY_ATTACHED`, used at `:715` and `:882`), and `external.rs:281`, `:339`, `:457`; doc comments `validate_v2023_09/env_params.rs:587`, `external.rs:527`, `job/create_job/env_params.rs:399`; the test assertion at `validate_v2023_09/env_params.rs:969` | A |
| F1 | `enableIf` decodes as a bool literal or format string; only with `environment` | §3.2 constraint 1 | decode, TV | `template/step.rs`, `structure.rs` | A |
| F2 | `enableIf` requires `EXPR`; a single `{{ }}` expression of type `bool` (A25) | constraint 9 | TV | `env_params.rs`, `format_strings.rs` entry-value scope with target `bool` | A |
| F3 | Scopes as a `parameters` value; PATH `Param.*` and `Task.*` rejected | §3.2 item 5 | TV | `format_strings.rs` | A |
| F4 | Evaluated at JC after Step `let`, before name resolution; `null` is an error at the entry's path | §3.2 item 5; §7.4 | JC | `job/create_job/env_params.rs` `resolve_step_entries` (`:685`) | A |
| F5 | Disabled entry as if absent: not resolved, supplies nothing, requests nothing; an `ALL` Environment then takes every `default` | §3.2 items 2, 5; §3.2.1 intro | JC, SUB | same | A |
| F6 | Enabled-dependent TV checks deferred for an expression `enableIf`; constraint 3 applies as written | §3.2 checked-at ¶ | TV | `env_params.rs` | A |
| J1 | `Job.EnvNames` is `unresolved[list[string]]` at TV in every scope resolved at JC or later, with `ENV_PARAMS` and `EXPR` in the referencing document | §7.3.1; ExprLang §1.2.2 | TV | `format_strings.rs` (every scope that adds `Job.Name`, `:146`, `:383`, `:1877`, `:2039`, `:2112`, `:2280`, `:2523`) | A |
| J2 | Not in an Environment parameter `default` or the Job `name` | §7.3.1; §4.5 item 4 | TV | `format_strings.rs` default scope (`:2043`), name check | A |
| J3 | Concrete at JC before any TEMPLATE-scope expression; attached Environments included, Step Environments excluded, a shared name repeated | §7.3.1; §7.4 | JC | `create_job` body (`mod.rs:235`), P1, P2 | A |
| J4 | Available in attached Environments' scripts and Step `let` that feeds frozen entries | §7.3.1 | SUB | `external.rs:724-727` (next to `Job.Name`) | A |
| O1 | §7.4 order end to end | §7.4 | JC, SUB | `create_job_with_environment_templates` (P1) | A |
| H1 | `hostRequirements` decodes on `<Environment>`; requires `ENV_PARAMS`; not on `stepEnvironments` | §4 item 5, item 5.2 | decode, TV | `template/environment.rs`, `env_params.rs` | B |
| H2 | Per-source §3.3 checks at TV (list lengths, duplicate literal names, constraint 1 incl. A26) | §4 item 5; §3.3 | TV | `structure.rs` host-requirement checks, generalized from Step paths | B |
| H3 | Scopes exactly as N5 (A27); `Step.Name`, `let`, `Session.*`, other Environments' and PATH `Env.Param.*` rejected | §4 item 5.1 | TV | new host-requirement scope in `format_strings.rs` | B |
| H4 | Resolved per entering Step with its effective values; resolved per-source checks (non-negative `min`, `min` ≤ `max`, duplicate names after resolution, name and value grammar) | §4 item 5.1; §3.3.1–§3.3.2 | JC, SUB | `instantiate.rs:521-870`, generalized (P6) | B |
| H5 | Combination and normalization; conflicts rejected naming Step, sources, capability (P7, A29, A30) | §3.3.3 items 1–4 | JC/SUB end | new `job/create_job/host_requirements.rs` | B |
| H6 | Environments not entered contribute nothing (`ONLY_REQUESTED` not requested, `required: false` unresolved, disabled entry) | §3.3.3 last ¶ | JC/SUB | falls out of R3 and F5; pinned by tests | B |
| H7 | Combined set stored on `Step::host_requirements` | §3.3.3 ¶1 | JC/SUB | `job/mod.rs` doc comments | B |
| C1 | `openjd summary` shows each Step's Environment set, effective values, and combined host requirements | epic task 4 | CLI | `summary.rs` | C |
| C2 | `openjd check` surfaces the new TV errors; `--environment` runs the set-aware constraint 7 and resolves attached `hostRequirements` checks that need no Job Parameter values | §3.2 last ¶ | CLI | `check.rs`, `check_environment_templates` | C |
| C3 | openjd-for-js exposes `runScope`, `hostRequirements`, `enableIf`, `Job.EnvNames`, and the Step's `environments`; `createJob` uses P1 | epic task 4 | JS | `crates/openjd-for-js/src/model.rs`, `js-tests/` | C (P1 switch in A) |
| C4 | Samples updated from the spec samples (N9) | N9 | CLI fixtures | `crates/openjd-cli/tests/templates/env_params/` | C |
| C5 | Sessions: no API change expected (no API takes the Job's Environments); specs say a Session enters the Step's set | HJAR | docs | `specs/sessions/session.md` | C |

## PR sequence

One PR per remaining epic task, in order, each merged into `env-params-prototype` before the next starts (the epic's
integration rule). Each follows `implement-change` with the cross-crate audit tier (2–4 auditors per round with
disjoint angles). Every PR runs the gates the first plan lists (fmt, clippy, tests, `cargo doc` with `-D warnings`,
copyright headers, MSRV `cargo +1.97.0 check --workspace`) and the conformance slices `2023-09/ENV_PARAMS/*`,
`base/*`, `EXPR/*`, `WRAP_ACTIONS/*`. PR A and PR C also run the openjd-for-js wasm build and vitest suite, since they
change the `job::Step` JSON shape and openjd-for-js's `createJob`; CI runs that suite in its own job, and every
workspace job excludes the crate (`.github/workflows/ci.yml:152`, `:192`). Each PR reports which ENV_PARAMS tests
flipped and confirms that the re-check list's tests for its features still fail, for the intended rule.

| PR | Epic task | Title | Crates | Public API changes | Spec | Inventory | Flips |
|----|-----------|-------|--------|--------------------|------|-----------|-------|
| A | `9d7c8610` | `feat(model): runScope, entry enableIf, Job.EnvNames, and §7.4 job-creation order` | openjd-model; openjd-cli `run` (P4, if kept) and `run`/`summary` switched to P1; openjd-for-js `createJob` switched to P1; openjd-sessions integration tests switched to P1 | `template::Environment::run_scope`, `template::RunScope`; `template::StepDependency::enable_if`, `template::EnableIf`; `job::Step::environments`, `job::EnvironmentRef`, `job::Environment::run_scope` (not added, see P3); `create_job_with_environment_templates`, `CreatedJob`; `create_job` now returns a complete Job and rejects an unresolved required external entry; `job::Step::unresolved_environment_entries` and `job::UnresolvedEnvironmentEntry` removed; `apply_environment_templates` made crate-private (`AppliedEnvironmentTemplates` with it); `Job.EnvNames` symbol (openjd-expr needs no change: the symbol table already holds `list[string]`) | §4 item 4; §3.2 items 2, 5, constraints 1, 3, 7, 9, checked-at ¶; §3.2.1; §3.2.2; §7.3.1 `Job.EnvNames`; ExprLang §1.2.2; §7.4 Job creation (up to effective values); HJAR Environment Parameters; N8 test | R1–R9, F1–F6, J1–J3 (J4), O1 | 31 tests marked A, plus 11 marked A* if P4 is kept (42 of 54) |
| B | `7314bc1c` | `feat(model): Environment hostRequirements and combined host requirements` | openjd-model | `template::Environment::host_requirements`; `job::Step::host_requirements` documented as the combined set | §4 item 5; §3.3.3; §7.4 Job creation (last clause) | H1–H7 | 10 tests marked B, plus 2 marked B* if the `openjd run` wiring has landed (12) |
| C | `f1a69486` | `feat(cli): ENV_PARAMS runScope, enableIf, and host requirements in summary, check, and openjd-for-js` | openjd-cli, openjd-for-js, openjd-sessions (specs only), samples | openjd-for-js decoded model and `Job.toJSON()` shape gain the new fields; no Rust API change expected in sessions | HJAR; §3.2 last ¶; N9 | C1–C5, R7 if not in A | the 11 A* and 2 B* tests if P4 moved here; then all 54, with the 3 Windows-only tests run on Windows by CI if available |

### PR A detail

* **Files.** `template/environment.rs`, `template/step.rs`, `template/mod.rs`; `validate_v2023_09/{env_params.rs,
  structure.rs,format_strings.rs,wrap_actions.rs}`; `job/mod.rs`; `job/create_job/{mod.rs,env_params.rs,external.rs,instantiate.rs}`;
  `lib.rs` re-exports; `crates/openjd-cli/src/run/execution.rs`, `src/summary.rs` (P1 switch, P4);
  `crates/openjd-for-js/src/model.rs` (P1 switch); specs `specs/model/{template-types.md,validation.md,job-creation.md,
  job-types.md,step-dependencies.md,public-api.md}`, `specs/cli/run.md` (P4), `specs/openjd-for-js/spec.md` (the
  `Step` JSON shape gains `environments`).
* **Blast radius of P1.** Every other file that names `apply_environment_templates`,
  `unresolved_environment_entries`, or `UnresolvedEnvironmentEntry` at `a758062` (`grep -rln` over `crates`, `specs`,
  `README.md`) changes with it: tests `crates/openjd-model/tests/integration/{test_env_params_check.rs,
  test_env_params_external.rs,test_env_params_job_creation.rs,test_step_dependency_graph.rs}` and
  `crates/openjd-sessions/tests/integration/test_env_params_session.rs` (which calls `apply_environment_templates`,
  so PR A edits openjd-sessions tests); `crates/openjd-model/src/{lib.rs,types.rs}`; docs
  `crates/openjd-model/README.md`, `specs/model/{README.md,architecture.md,parameters.md}`,
  `specs/cli/{architecture.md,check.md,summary.md}`, and `specs/sessions/session.md`. PR A also links this plan from
  `specs/model/README.md`, next to the first plan's link.
* **Tests.** Full path-and-message assertions (AGENTS.md) for: `runScope` gating, unknown value, step-environment
  rejection; `enableIf` gating, form, type (A25), scope, `null` at JC, disabled-entry effects (absent `ALL` defaults,
  unrequested `ONLY_REQUESTED`, skipped constraint 7, skipped resolution of an unattached name); deferred constraint 7
  for an expression `enableIf`; `Job.EnvNames` type at TV, absence from `default` and `name`, contents and order at JC
  (attached first, repeats, no Step Environments, unrequested `ONLY_REQUESTED` included), value in an attached
  Environment's script and in a `let` that feeds a frozen entry; per-Step sets and serialization unchanged without
  `ENV_PARAMS` (G6); `check_environment_templates` set-aware constraint 7; the single-layer rule over each Step's set (two wrapping
  `ONLY_REQUESTED` Job Environments requested by different Steps accepted, by the same Step rejected, and the check
  deferred to job creation for an expression `enableIf`; no conformance test covers this); N8 (`min: "0"` accepted). A CLI test for
  two Steps with different sets running in two Sessions, if P4 is kept.
* **Re-check.** The 17 job_templates, 4 env_templates, and 5 jobs tests marked PR A in
  [Passes that need a re-check](#passes-that-need-a-re-check-50).

### PR B detail

* **Files.** `template/environment.rs`; `validate_v2023_09/{env_params.rs,structure.rs,format_strings.rs}`;
  `job/create_job/{instantiate.rs,external.rs,mod.rs}`; new `job/create_job/host_requirements.rs`; `job/mod.rs` (doc
  comments); specs `specs/model/{template-types.md,validation.md,job-creation.md,job-types.md,public-api.md}`.
* **Tests.** Each per-source check on an Environment's requirements at TV and after resolution, with the path naming
  the Environment; each scope rule of N5; each §3.3.3 rule: amounts (greatest `min`, least `max`, missing bounds,
  conflict), `anyOf` intersection ignoring case and empty-intersection conflict, `allOf` union, mixed pair kept,
  case-insensitive names, amounts not adding, conflicts between two Environments and with an attached one, a conflict
  that only appears after resolution from `Env.Param`; no contribution from an Environment the Step does not enter;
  the combined set's shape and spelling (P7, A30).
* **Re-check.** The 12 job_templates, 4 env_templates, and 8 jobs tests marked PR B.

### PR C detail

* **Files.** `crates/openjd-cli/src/{summary.rs,check.rs,help.rs}`, `run/execution.rs` (if P4 moved here);
  `crates/openjd-cli/tests/templates/env_params/` (samples from the spec's N9 changes); `crates/openjd-for-js/src/model.rs`,
  `js-tests/`, `tests/`; specs `specs/cli/{run.md,check.md,summary.md}`, `specs/openjd-for-js/spec.md`,
  `specs/sessions/session.md`; `CHANGELOG.md` where the skill requires it.
* **Tests.** `summary` output for sets, values, and combined requirements; `check` messages for the new TV errors and
  for `--environment` set-aware constraint 7; openjd-for-js vitest for the new fields; the updated sample fixtures run.
* **Full run.** `2023-09/ENV_PARAMS/*` should pass in full; any failure gets a one-line diagnosis in the task result.
* *As implemented (PR C):* R7 landed in PR A, so no `run/execution.rs` change was needed; PR C adds a `--step` test.
  `job::Environment::run_scope` was not added: `summary` reads each Step's set from `job::Step::environments`, so
  nothing needs it. `summary` shows, only when the Job Template or an attached Environment Template lists
  `ENV_PARAMS`, each Step's `Session environments` (its set, in Session order) and `Host requirements` (the combined set), and the JSON gains
  `session_environments` and `host_requirements`. C2 goes beyond the inventory, at the lead's request:
  `check_environment_templates` also applies the `WRAP_ACTIONS` single-layer rule to Steps that enter a wrapping
  attachment, and combines (§3.3.3) the literal requirements of each Step and the Environments its Sessions certainly
  enter, so a literal conflict fails at check time, with job creation's message when only literal requirements of that
  capability take part (job creation may name other sources or bounds when a non-literal one also does); a conflict
  that needs a non-literal requirement, or one an `enableIf` expression brings in, is still left to job creation. The constraint 7 message now says when an entry's
  `enableIf` disabled it (PR A audit item B2-3), at template validation, job creation, the submission stage, and
  `check_environment_templates`. openjd-for-js needed no Rust change: the fields decode through `openjd-model`, and
  `Job.toJSON()` already carries `environments` and the combined `hostRequirements`; PR C adds tests and spec text.
  No sessions API change was needed (C5); `session.md` states which Environments a Session enters. `CHANGELOG.md`
  files are generated by release-plz from the commits, so none was edited. The CLI and model sample fixtures were
  updated from the spec's N9 samples (Docker's `runScope: ONLY_REQUESTED`, `hostRequirements`, and
  `FEATURE_BUNDLE_1`; the video sample's Conda `enableIf`).

## Spec observations for the user

These are not prototype work; they are recorded so the RFC author can decide.

* **Epic description versus spec.** The epic says `Job.EnvNames` lists names "in entry order"; ExprLang §1.2.2 says
  "in entry order" and Schemas §7.3.1 says "in the order a Session that entered all of them would enter them". The
  prototype reads both as P2. The epic omits that `Job.EnvNames` is also unavailable in the Job Template's `name`;
  task `9d7c8610`'s description and §7.3.1 include it.
* **ExprLang §1.2.2 versus Schemas §7.3.1.** ExprLang says "every template can also read" `Job.EnvNames`, without
  §7.3.1's exclusions (an Environment parameter `default` and the Job Template's `name`). The prototype follows
  §7.3.1 (J2); ExprLang could add the exceptions. No other disagreement was found.
* **Clarifications** A23–A29 above, which are the conformance README's CT-1, CT-8, CT-11–CT-16 restated with
  the prototype's reading.

## Follow-ups outside this epic

* Unifying `create_job_with_environment_templates` with `service-extension`'s submission stage (replaces the first
  plan's `apply_environment_templates` reconciliation follow-up).
* Python parity, as in the first plan.
* `SERVICE` and `CONTROL_FLOW` composition (the `CONTROL_FLOW` note in the `Job.EnvNames` row).
