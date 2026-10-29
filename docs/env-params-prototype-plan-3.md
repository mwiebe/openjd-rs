# ENV_PARAMS prototype plan, part 3: renames, `step:`, expression fields, Environment `enableIf`

This plan brings the `ENV_PARAMS` prototype on `env-params-prototype` up to date with the specification at `2a5a930`
and orders the remaining work into three PRs. It changes no code. Each PR is made later through the
`implement-change` skill.

[Plan 2](env-params-prototype-plan-2.md) is the context: it covers `runScope`, Environment `hostRequirements`, entry
`enableIf`, `Job.EnvNames`, and the §7.4 order, all now implemented. Its decisions P1–P8, inventory items, and
ambiguities A23–A32 still hold except where [Where this plan supersedes plan 2](#where-this-plan-supersedes-plan-2)
says otherwise. The first plan, [env-params-prototype-plan.md](env-params-prototype-plan.md), is still the context for
plan 2.

## Sources

* **Normative spec.** `~/projects/rfc-env-params/openjd-specifications-ep`, branch `rfc-env-params` at `2a5a930`:
  `rfcs/0010-environment-parameters.md` ("RFC"), `wiki/2023-09-Template-Schemas.md` ("Schemas"),
  `wiki/How-Jobs-Are-Run.md` ("HJAR"), and `wiki/2026-02-Expression-Language.md` ("ExprLang"). Section numbers are
  Schemas sections. `git diff 408910f 2a5a930` is the change this plan covers; `2a5a930` is the merge of
  `rfc-env-params-refine` (`4cc7269`, `208f01e`, `633887c`, `ff3d261`, `3b75f87`). PR 4 also follows `5535230`,
  a later single commit that adds §7.5 rule 5 (the stage rule for early checks) and states that both `enableIf`
  fields are evaluated in template scope; its conformance tests are those of `2a5a930`.
* **Conformance suite.** `conformance-tests/2023-09/ENV_PARAMS/` at `2a5a930` (unchanged at `5535230`): 149 `job_templates/`, 42
  `env_templates/`, and 101 `jobs/` files (292). New since `408910f`: JT-124–JT-149, ET-39–ET-42, JB-93–JB-101. 32 files
  were renamed (`git diff -M --name-status 408910f 2a5a930`), mostly `*-run-scope-*` → `*-activation-*`,
  `*-only-requested-*` → `*-on-request-*`, and `*-all-required-*` → `*-always-required-*`, plus
  `3.2--enable-if-not-single-expression.invalid` → `3.2--enable-if-braces.invalid` (JT-98), so plan 2's file lists
  cite old names.
* **Code baseline.** This repo at `ed1deb4`, the head of `env-params-prototype` after epic 9fe8f347. All `file:line`
  citations refer to that commit.

## What the specification changed

| # | Change | Spec |
|---|--------|------|
| D1 | **Rename** `parameterDefinitions` → `envParameterDefinitions` on `<Environment>`. The top-level Job Parameter `parameterDefinitions` of a Job Template or Environment Template is unchanged. | §4 item 3; §1.2.1; §3.2 items 4, constraint 5; §3.2.1 item 1; ExprLang |
| D2 | **Rename** `runScope: <RunScope>` → `activation: <EnvironmentActivation>`, values `ALL` → `ALWAYS`, `ONLY_REQUESTED` → `ON_REQUEST`. Default `ALWAYS`. Same constraints. | §4 item 4; every text that cited the old names |
| D3 | **`step: <StepName>`**, a synonym of `dependsOn:` in `<StepDependency>`, `ENV_PARAMS`-gated. Every `dependsOn` rule applies; a `step:` and a `dependsOn:` entry naming one Step are duplicates; one dependency graph. Constraint 1 becomes "exactly one of *dependsOn*, *step*, or *environment*". | §3.2 item 1, constraint 1; RFC Validation table: the `ENV_PARAMS`-listing rule, the constraint 1 rule, and the `step`-obeys-`dependsOn` rule |
| D4 | **Expression fields** (`@expr <type>`). A value is a boolean, integer, or float scalar that fits the target type, used as written, or a string holding one expression with no `{{ }}`. A YAML null is an error, not an omitted field. The static type must *fit* the target (ExprLang §1.2.3 step 1); only `int` → `float` widening applies; `U?` is accepted where `U` fits, and a `null` result is an evaluation error even on an optional field; a `nulltype` expression is an error; `unresolved[U]` checks as `U`. `{{` is a parse error, and the message should say that an expression field takes no `{{ }}`. Each field's definition gives its scopes and stage. Errors other than evaluation errors are template-validation errors. | new §7.5; Notations list; §7.4 annotation paragraph; ExprLang §1.3.2 note |
| D5 | **Entry `enableIf` is `@expr bool`.** Same scopes and stage as before. For the checked-at paragraph, only an absent `enableIf` or a boolean scalar counts as known at template validation; a constant expression such as `"true"` defers to job creation. | §3.2 item 5, constraint 9, checked-at ¶ |
| D6 | **`enableIf` on every `<Environment>`** (`@expr bool`, `ENV_PARAMS` and `EXPR`), on `jobEnvironments`, `stepEnvironments`, and an Environment Template's `environment`. Resolved at job creation once per Step whose Sessions would otherwise enter it, with that Step's effective values; `null` is an error. Scopes: a Job Environment or Environment Template has exactly its `hostRequirements` scopes (§4 item 5.1); a Step Environment has `RawParam.*`, non-PATH `Param.*`, `Job.Name`, `Job.EnvNames`, `Step.Name`, and the Step's `let`. Never `Task.*`, `Session.*`, `Env.File.*`, or another Environment's parameters. | §4 item 6 |
| D7 | **Effects of a disabled Environment** for a Step: not entered or exited, no `variables`, not in the stack, no host requirements (and its `hostRequirements` are not resolved for that Step). `Job.EnvNames` still lists it. Constraint 7 is checked over the set *before* Environment `enableIf`, so it never exempts a Step. A check over a Step's Environment set (the `WRAP_ACTIONS` single-layer rule) is made at template validation only when every relevant `enableIf` is absent or a boolean scalar (a `false` scalar counting as not entered), otherwise at job creation. | §4 item 6 tail; §3.2 constraint 7; §3.2.1 last ¶s; §3.3.3 last ¶; `Job.EnvNames` row |
| D8 | **§7.4 order**: … effective values and their checks, constraint 7 included → each Environment's `enableIf` per Step → `hostRequirements` resolved over the final set and combined. Session sharing (§3.2.2), `jobEnvironments` and `stepEnvironments` items, and HJAR updated to match. | §7.4 Job creation row; §3.2.2 ¶4; §1.1 item 7; §3 item 5; HJAR |
| D9 | `SERVICE` / `CONTROL_FLOW` composition removed from the RFC (moved to `docs/env-params/composition-carry-over.md`). Neither extension is on this branch: no code change, and none is to be added. | RFC |
| D10 | Samples: `conda-env-params.yaml` lists `[ENV_PARAMS, EXPR]` and has `enableIf: "Env.Param.Packages != ''"` (its "no packages" shell branch removed); `docker-env-params.yaml` renamed fields; `video-sfm-composite.yaml` uses `step:` and expression-field entry `enableIf`s. | `samples/` |
| D11 | Conformance README rows V31–V34 added (V7, V28, V29, V30 marked removed); "Rules without a definite test" items 11 and 13 resolved. JT-98 and JT-141 now test `{{ }}` rejection. | `conformance-tests/2023-09/ENV_PARAMS/README.md` |

Nothing else changed: §3.3.3, §4 item 5, §4.5, §3.2.1, and `Job.EnvNames` keep their rules apart from the renames and
the `enableIf` additions above.

## Baseline

Built with `cargo build --release` at `ed1deb4` and run from the spec checkout at `2a5a930` with
`PATH="<worktree>/target/release:$PATH" uv run --with pyyaml python3.12 run_openjd_cli_tests.py '<pattern>'`. The
runner needed no change to work with this binary.

| Suite | job_templates | env_templates | jobs | Total |
|-------|---------------|---------------|------|-------|
| `2023-09/ENV_PARAMS/*` | 109 / 40 | 29 / 13 | 39 / 59 | **177 passed, 112 failed** |
| `2023-09/base/*` | 455 / 0 | 40 / 0 | 196 / 0 | 691 passed, 0 failed |
| `2023-09/EXPR/*` | 220 / 0 | 6 / 0 | 157 / 0 | 383 passed, 0 failed |
| `2023-09/WRAP_ACTIONS/*` | (none) | 9 / 0 | 64 / 0 | 73 passed, 0 failed |

Cells are passed / failed. The same three `runOn: [windows]` `jobs/` files as in plan 2 were skipped on Linux
(`4.5--list-path-elements-mapped-windows`, `4.5--path-value-mapped-in-env-param-windows`,
`4.5--path-value-unmapped-in-env-raw-param-windows`), so 98 of 101 ran. base, EXPR, and WRAP_ACTIONS pass in full,
so there is no regression to fix.

**How the failures were classified.** By rerunning the suite against the same binary on three rewritten copies of
`ENV_PARAMS/` (YAML keys only, comments untouched):

1. **sim1**: R1 and R2 reverted (`envParameterDefinitions` → `parameterDefinitions`; `activation:` →
   `runScope:` with `ALWAYS` → `ALL`, `ON_REQUEST` → `ONLY_REQUESTED`): 265 passed, 24 failed (177 + 89 rename-only,
   less JT-98, which the renames turn from pass to fail; see PR 3 below).
2. **sim2**: sim1 plus `- step: X` → `- dependsOn: X`: 266 passed, 23 failed.
3. **sim3**: sim2 plus every string `enableIf` without `{{` wrapped in `{{ }}` (the prototype's current form): 274
   passed, 15 failed.

A failing test that passes in sim1 needs only the renames; one that first passes in sim2 needs S; in sim3, X; one that
fails in all three needs E (confirmed by reading each: every one has an Environment-level `enableIf`). The features
each test uses were extracted from its parsed YAML. The simulations only approximate the target: sim2 cannot tell
`step:` from `dependsOn:`, and sim3 says nothing about `.invalid` tests, so both are used for valid tests only, and the
`.invalid` side is in [Passes that need a re-check](#passes-that-need-a-re-check-129).

## Classified ENV_PARAMS failures

Classes: **R1** the `envParameterDefinitions` rename, **R2** the `activation` rename, **S** `step:`, **X** expression
fields with entry `enableIf` as `@expr bool`, **E** `enableIf` on `<Environment>` (marked "scalar" when every such
`enableIf` in the test is a boolean scalar, "expr" when one is an expression, which also needs X's checker), **(c)** a
suspected test bug or spec ambiguity.

There is **no class (c) failure**. Every failure stops at `unknown field` for `envParameterDefinitions`, `activation`,
`step`, or `enableIf` (on an Environment), or at the prototype's old `enableIf` form check ("must be a boolean literal
or a format string consisting of a single '{{ }}' expression"), before any Session runs.

| Class needed after R1/R2 | job_templates | env_templates | jobs | Total | PR |
|--------------------------|---------------|---------------|------|-------|----|
| none (R1/R2 only) | 32 | 11 | 46 | 89 | 2 |
| S | 2 | 0 | 0 | 2 | 2 |
| X | 2 | 0 | 6 | 8 | 3 |
| E (with S or X where marked) | 4 | 2 | 7 | 13 | 4 |
| **Total** | **40** | **13** | **59** | **112** | |

Of the 112, 72 use the R1 field only, 7 the R2 field only, 26 both, and 7 neither (a test "uses" a field if any
of its documents does). By the first error the baseline reports: unknown field `envParameterDefinitions` 65,
`activation` 31, Environment `enableIf` 12, `step` 2, and the old `enableIf` form 2.

### job_templates (40)

| Test | Renames | Still needs after R1/R2 | PR |
|------|---------|-------------------------|----|
| `3.2--enable-if-expression.yaml` | R1 | X | 3 |
| `3.2--enable-if-reads-step-name-and-let.yaml` | R1 | X | 3 |
| `3.2--step-depends-on-and-environment-entries.yaml` | R1 | S | 2 |
| `3.2--step-entry.yaml` | — | S | 2 |
| `4--env-enable-if-literal.yaml` | R1 | E (scalar) | 4 |
| `4--env-enable-if-reads-job-param-and-env-names.yaml` | R1 | E (expr) | 4 |
| `4--env-enable-if-reads-own-env-param.yaml` | R1 | E (expr) | 4 |
| `4--step-env-enable-if-reads-step-name-and-let.yaml` | — | E (expr) | 4 |

Rename only, PR 2 (32: R1 28, R1+R2 2, R2 2): `1.1--minimal-env-params` (R1);
`3.2--always-required-param-supplied-by-every-step` (R1); `3.2--depends-on-and-environment-entries` (R1);
`3.2--enable-if-literal` (R1); `3.2--entry-without-parameters` (R1); `3.2--int-literal-as-intstring` (R1);
`3.2--list-literal-value` (R1); `3.2--on-request-entry-disabled-literal` (R1+R2);
`3.2--on-request-required-param-non-requesting-step` (R1+R2); `3.2--required-true-on-job-environment` (R1);
`3.2--value-reads-env-names` (R1); `3.2--value-reads-job-name` (R1); `3.2--value-reads-job-parameter` (R1);
`3.2--value-reads-raw-path-param` (R1); `3.2--value-reads-step-let` (R1); `3.2--value-reads-step-name` (R1);
`4--activation-always` (R2); `4--activation-on-request` (R2); `4--host-requirements-read-env-param` (R1);
`4--parameter-definitions-50` (R1); `4--parameter-name-in-two-environments` (R1);
`4--parameter-name-same-as-job-parameter` (R1); `4.5--base-types-without-expr` (R1);
`4.5--default-reads-job-parameter` (R1); `4.5--default-reads-raw-path-param` (R1); `4.5--expr-types` (R1);
`4.5--list-default-single-expression` (R1); `4.5--typed-expression-default` (R1);
`7.3.1--env-param-in-embedded-file` (R1); `7.3.1--env-param-in-let` (R1); `7.3.1--env-param-in-variables` (R1);
`7.3.1--env-raw-param-in-on-exit` (R1). (All `.yaml`.)

### env_templates (13)

| Test | Renames | Still needs after R1/R2 | PR |
|------|---------|-------------------------|----|
| `4--env-enable-if-reads-own-env-param.yaml` | R1 | E (expr) | 4 |
| `4--env-enable-if-reads-own-job-parameter.yaml` | R1 | E (expr) | 4 |

Rename only, PR 2 (11: R1 10, R2 1): `1.2--extensions-env-params` (R1); `4--activation-on-request` (R2);
`4--host-requirements-read-env-param` (R1); `4--parameter-definitions-50` (R1);
`4--parameter-name-same-as-job-parameter` (R1); `4.5--default-reads-own-job-parameter` (R1);
`4.5--default-reads-raw-path-param` (R1); `4.5--list-path-default-empty` (R1);
`4.5--required-parameter-activation-always` (R1); `7.3.1--env-param-in-variables-let-and-files` (R1);
`7.3.1--wrap-hook-reads-own-param` (R1). (All `.yaml`.)

### jobs (59)

| Test | Renames | Still needs after R1/R2 | PR |
|------|---------|-------------------------|----|
| `3.2--enable-if-false-always-takes-defaults.test.yaml` | R1, R2 | X | 3 |
| `3.2--enable-if-false-not-entered.test.yaml` | R1, R2 | X | 3 |
| `3.2--enable-if-true-enters.test.yaml` | R1, R2 | X | 3 |
| `3.2--example-choose-conda-when-alone.test.yaml` | R1, R2 | X | 3 |
| `3.2--example-choose-docker-when-both.test.yaml` | R1, R2 | X | 3 |
| `3.3.3--disabled-entry-requirements-not-applied.test.yaml` | R2 | X | 3 |
| `3.3.3--env-enable-if-false-requirements-not-applied.test.yaml` | — | E (scalar) | 4 |
| `4--attached-env-enable-if-per-step.test.yaml` | R1 | S, E (expr) | 4 |
| `4--env-enable-if-false-second-wrapper-allowed.test.yaml` | — | E (scalar) | 4 |
| `4--env-enable-if-false-variables-not-set.test.yaml` | — | E (scalar) | 4 |
| `4--env-enable-if-per-step.test.yaml` | R1 | S, E (expr) | 4 |
| `4--step-env-enable-if-false-not-entered.test.yaml` | — | E (expr) | 4 |
| `7.3.1--env-names-includes-disabled-environment.test.yaml` | — | E (scalar) | 4 |

Rename only, PR 2 (46: R1 24, R1+R2 19, R2 3): `1.2--job-template-without-extension-skips-on-request` (R1+R2);
`1.2--job-template-without-extension-takes-defaults` (R1+R2); `1.2.1--env-parameters-not-merged` (R1);
`3.2--value-from-step-let-and-step-name` (R1); `3.2.1--attached-always-required-supplied-by-every-step` (R1+R2);
`3.2.1--attached-on-request-required-not-requested` (R1+R2); `3.2.1--attached-reachable-from-other-step` (R1);
`3.2.1--example-steps-own-software-conda-queue` (R1); `3.2.1--external-environment-receives-values` (R1);
`3.2.1--external-required-false-present-receives-values` (R1+R2); `3.2.1--job-environment-shadows-attached` (R1);
`3.2.1--several-attached-each-own-values` (R1+R2); `3.2.2--example-execution-per-step-values` (R1);
`3.2.2--format-string-value-converted-to-int` (R1); `3.2.2--int-one-and-string-one-both-accepted` (R1);
`3.3.3--any-of-and-all-of-not-combined` (R1+R2); `3.3.3--any-of-intersection-ignores-case` (R2);
`3.3.3--example-environment-host-requirements` (R1+R2); `3.3.3--non-conflicting-requirements-run` (R1+R2);
`3.3.3--on-request-requirements-not-applied` (R1+R2); `3.3.3--same-capability-in-step-and-environment` (R1+R2);
`4--always-entered-by-step-without-entry` (R1+R2); `4--attached-on-request-not-entered-without-request` (R1+R2);
`4--example-parameterized-job-environment` (R1+R2); `4--on-request-entered-by-requesting-step` (R1+R2);
`4--on-request-keeps-order` (R2); `4--on-request-not-entered-by-other-step` (R1+R2);
`4--on-request-variables-not-set-for-other-step` (R1+R2); `4--on-request-wrapper-not-applied-to-other-step` (R1+R2);
`4.5--default-reads-merged-job-parameter` (R1); `4.5--default-reads-submitted-job-parameter` (R1);
`4.5--example-queue-conda-step-configures` (R1); `4.5--list-path-elements-mapped-posix` (R1);
`4.5--omitted-parameter-takes-default` (R1); `4.5--path-allowed-values-checked-unmapped-posix` (R1);
`4.5--path-uri-value` (R1); `4.5--path-value-mapped-in-env-param-posix` (R1);
`4.5--path-value-unmapped-in-env-raw-param-posix` (R1); `4.5--resolved-default-unused-violates-constraint` (R1);
`4.5--value-argv-exact` (R1); `4.5--value-reaches-on-enter-args` (R1); `4.5--value-reaches-on-exit-args` (R1);
`4.5--value-reaches-task-through-variables` (R1); `7.3.1--env-names-in-step-let-feeds-value` (R1);
`7.3.1--env-names-includes-unrequested-on-request` (R2); `7.3.1--wrap-hook-reads-wrapper-env-param` (R1+R2). (All
`.test.yaml`.)

### Passes that need a re-check (129)

`openjd check` and `openjd run` treat any non-zero exit as a pass for an `.invalid` test, so a passing `.invalid` test
proves nothing until it fails for the rule it names. 157 `.invalid` tests pass at baseline. 129 of them pass for an
unrelated reason: 128 on `unknown field` (`envParameterDefinitions` 93, `activation` 14, Environment `enableIf` 14,
`step` 7), and `jobs/3.2.1--attached-always-required-entry-disabled.invalid` on the old `enableIf` form check. The
other 28 fail for their own rule today (gating, `hostRequirements` scope and shape, `Job.EnvNames` placement,
external resolution). One of them, `job_templates/3.2--enable-if-without-expr.invalid` (`enableIf: true` without
`EXPR`), has an entry `enableIf`, whose decoding PR 3 changes, so it joins PR 3's group; the other 27 use none of the
changed fields.

Each PR's tests must show, for each file in its group, that the rejection comes from the rule the file names, with the
intended message (AGENTS.md: assert path and message).

**PR 2 (107).** Unmasked by R1, R2, and S. In sim2, every one of the R1/R2 files is rejected for its own rule
(constraint 7, duplicates, value constraints, scopes, `default` rules, gating, external resolution, combined-host
conflicts). The S files cannot be judged by simulation; PR 2 checks each by hand. In particular
`3.2--step-and-depends-on.invalid` must fail on constraint 1 (one entry with both keys), `3.2--step-and-environment.invalid`
on constraint 1 with `step` named in the message, and `3.2--step-entry-without-extension.invalid` on the `ENV_PARAMS`
gate. `4--activation-unknown.invalid` (both directories) must name `ALWAYS` and `ON_REQUEST` in its message.

* env_templates (19): `4--activation-unknown`, `4--activation-without-extension`,
  `4--host-requirements-env-path-param`, `4--parameter-definitions-51`, `4--parameter-definitions-duplicate-name`,
  `4--parameter-definitions-empty`, `4--parameter-definitions-without-extension`, `4.5--bool-type-without-expr`,
  `4.5--default-reads-env-names`, `4.5--default-reads-path-param`, `4.5--default-reads-undefined-param`,
  `4.5--list-default-not-single-expression`, `4.5--list-type-without-expr`,
  `4.5--literal-default-violates-constraint`, `4.5--path-data-flow`, `4.5--path-object-type`,
  `4.5--range-expr-type`, `4.5--user-interface`, `7.3.1--env-param-undeclared`.
* job_templates (68): `3.2--always-required-param-entry-disabled-literal`, `3.2--always-required-param-not-supplied`,
  `3.2--duplicate-entries-complementary-enable-if`, `3.2--duplicate-environment-entries`,
  `3.2--enable-if-with-depends-on`, `3.2--entry-without-key`, `3.2--environment-and-depends-on`,
  `3.2--list-value-not-single-expression`, `3.2--literal-below-min-value`, `3.2--literal-exceeds-max-length`,
  `3.2--literal-not-in-allowed-values`, `3.2--literal-wrong-type`, `3.2--on-request-required-param-not-supplied`,
  `3.2--parameters-empty`, `3.2--parameters-with-depends-on`, `3.2--parameters-with-step`,
  `3.2--required-false-on-job-environment`, `3.2--required-with-depends-on`, `3.2--required-with-step`,
  `3.2--step-and-depends-on-cycle`, `3.2--step-and-depends-on-duplicate`, `3.2--step-and-depends-on`,
  `3.2--step-and-environment`, `3.2--step-entry-without-extension`, `3.2--step-name-value-without-expr`,
  `3.2--step-self`, `3.2--step-unknown`, `3.2--unknown-parameter-key`, `3.2--value-reads-env-scope`,
  `3.2--value-reads-path-param`, `3.2--value-reads-session`, `3.2--value-reads-task-param`,
  `4--activation-on-step-environment`, `4--activation-unknown`, `4--activation-without-extension`,
  `4--host-requirements-env-list-path-param`, `4--host-requirements-env-path-param`,
  `4--host-requirements-env-raw-path-param`, `4--host-requirements-other-env-param`, `4--parameter-definitions-51`,
  `4--parameter-definitions-duplicate-name`, `4--parameter-definitions-empty`,
  `4--parameter-definitions-on-step-environment`, `4--parameter-definitions-without-extension`,
  `4.5--bool-type-without-expr`, `4.5--default-reads-env-names`, `4.5--default-reads-env-param`,
  `4.5--default-reads-job-name`, `4.5--default-reads-list-path-param`, `4.5--default-reads-path-param`,
  `4.5--default-reads-undefined-param`, `4.5--list-default-not-single-expression`, `4.5--list-path-object-type`,
  `4.5--list-type-without-expr`, `4.5--literal-default-below-min-value`,
  `4.5--literal-default-not-in-allowed-values`, `4.5--literal-default-wrong-type`, `4.5--path-data-flow`,
  `4.5--path-object-type`, `4.5--range-expr-type`, `4.5--user-interface`, `7.3.1--env-param-in-step-environment`,
  `7.3.1--env-param-in-step-host-requirements`, `7.3.1--env-param-in-step-script`, `7.3.1--env-param-int-type-error`,
  `7.3.1--env-param-of-other-environment`, `7.3.1--env-param-undeclared`, `7.3.1--env-raw-param-in-step-script`.
* jobs (20): `3.2--format-string-value-violates-constraint`, `3.2.1--attached-always-required-not-supplied`,
  `3.2.1--attached-on-request-required-not-supplied`, `3.2.1--external-ambiguous-name`,
  `3.2.1--external-lacks-parameter`, `3.2.1--external-list-literal-for-string`,
  `3.2.1--external-list-value-not-single-expression`, `3.2.1--external-value-not-allowed`,
  `3.2.1--external-value-wrong-type`, `3.2.1--non-expr-document-supplies-bool-parameter`,
  `3.2.1--non-expr-document-supplies-list-parameter`, `3.3.3--amount-min-exceeds-max`,
  `3.3.3--attribute-any-of-empty-intersection`, `3.3.3--capability-name-compared-ignoring-case`,
  `3.3.3--conflict-resolved-from-env-param`, `4--resolved-host-requirements-duplicate-name`,
  `4--resolved-host-requirements-negative-min`, `4.5--relative-literal-path-default-unused`,
  `4.5--relative-path-value`, `4.5--resolved-default-taken-violates-constraint`.

**PR 3 (9).** After R1/R2 the first eight reach the old `enableIf` form check, which rejects them for the wrong reason, or, for
`3.2--enable-if-braces.invalid`, accepts them: the prototype accepts `enableIf: "{{ true }}"`, so **PR 2 turns JT-98
from pass to fail**, and PR 3 turns it back. Each must fail on its §7.5 rule: job_templates `3.2--enable-if-braces`
(rule 1, with the "without `{{ }}`" hint), `3.2--enable-if-not-bool` (rule 2, `int` does not fit `bool`),
`3.2--enable-if-reads-path-param` and `3.2--enable-if-reads-task-param` (scope), `3.2--enable-if-string` (rule 2,
`string` does not fit `bool`); jobs `3.2--enable-if-null` (null at job creation), `3.2--required-param-entry-disabled-by-expression`
and `3.2.1--attached-always-required-entry-disabled` (constraint 7 at job creation, after a `false` expression). The
ninth, job_templates `3.2--enable-if-without-expr`, fails for its own rule today (`enableIf requires the EXPR
extension.`) and must still do so on the new type.

**PR 4 (14).** Unmasked by E; each must fail on its §4 item 6 rule: env_templates `4--env-enable-if-reads-step-name`,
`4--env-enable-if-without-expr`; job_templates `3.2--required-param-env-enable-if-false-still-required` (constraint 7
is not exempted), `4--env-enable-if-braces`, `4--env-enable-if-not-bool`, `4--env-enable-if-reads-other-env-param`,
`4--env-enable-if-reads-path-env-param`, `4--env-enable-if-reads-session`, `4--env-enable-if-reads-step-name`,
`4--env-enable-if-without-env-params`, `4--env-enable-if-without-expr`, `4--step-env-enable-if-reads-task-param`;
jobs `4--env-enable-if-expression-second-wrapper-rejected` (single-layer rule at job creation),
`4--env-enable-if-null`.

## Spec delta mapped to crates and modules

| # | Crate | Module (at `ed1deb4`) | Change |
|---|-------|-----------------------|--------|
| D1 | openjd-model | `template/environment.rs:22-26` (`Environment::parameter_definitions`); `template/parse.rs:223-235` (reads the raw key `parameterDefinitions` of an Environment to collect written type names); `validate_v2023_09/{env_params.rs,format_strings.rs}` (paths and messages, e.g. `env_params.rs:417`); `job/create_job/{env_params.rs,external.rs}` and `job/mod.rs` (doc comments, messages) | Field and serde name; raw-key reader; every path element and message that names an Environment's `parameterDefinitions`. The top-level Job Parameter `parameterDefinitions` does not change: the Environment Template's (`template/environment_template.rs:21`), `structure.rs:79-80`, and `limits.rs:26-36`. |
| D2 | openjd-model | `template/environment.rs:27-80` (`run_scope`, `RunScope`, `as_str`), `template/mod.rs:57` (re-export); every `RunScope::` use: `validate_v2023_09/env_params.rs:383, :754-813`, `wrap_actions.rs:144-145`, `job/create_job/env_params.rs:806, :901`, `external.rs:230, :283, :607, :850, :878, :1080, :1174-1179`; doc comments `job/create_job/mod.rs:130-134`, `job/mod.rs:145-151`; message text naming `runScope`, `ALL`, `ONLY_REQUESTED` (the constraint 7 reason `EVERY_JOB_ENVIRONMENT` at `env_params.rs:228-230`, and its counterparts in `job/create_job/env_params.rs`, `external.rs`) | Type, variants, field, method, messages |
| D2 | openjd-cli | `run/execution.rs:90, :337`, `summary.rs:151` (doc comments) | Comments only |
| D1, D2 | tests, fixtures | `crates/openjd-model/tests/integration/test_env_params_*.rs`, `tests/fixtures/env_params/*.yaml`; `crates/openjd-cli/tests/cli_tests/env_params.rs`, `tests/templates/env_params/*.yaml` (and `run_scope_grouping.yaml`'s name); `crates/openjd-for-js/tests/test_env_params.rs`, `js-tests/smoke.test.js`; `crates/openjd-sessions/tests/integration/test_env_params_session.rs` | Template text and expected messages |
| D3 | openjd-model | `template/step.rs:293-470` (`DependencyTarget` `:293-321` and its `Display` `:323-330`, `StepDependency`, `target`, `step`, `is_well_formed`, `on_step`); `validate_v2023_09/structure.rs:1090-1135` (constraint 1 and "permitted only with 'environment'"), `:231-241` (self, unknown, duplicate, via `dep.step()`), `:1156` (graph); `validate_v2023_09/env_params.rs:295-310` (the `environment:` gate, next to which the `step:` gate goes); `job/create_job/instantiate.rs:209-211` (job `StepDependency` from `dep.step()`) | New key, gate, constraint 1 message; the rest reads `step()` and needs no change |
| D4, D5 | openjd-model | `template/step.rs` `EnableIf` and its `Deserialize`; `validate_v2023_09/env_params.rs:670-700` (`check_enable_if_form`, pass 11), `format_strings.rs:373-410` (`validate_enable_if`, pass 8), `structure.rs:1126`; `job/create_job/env_params.rs:736-780` (`entry_enabled`), `:991-999` (the frozen-entry reference list), `external.rs` `check_environment_templates` (`:958`, which treats an expression `enableIf` as ambiguous) | New expression-field type and checker; entry `enableIf` switched to it |
| D4 | openjd-expr | `types.rs` (`ExprType::satisfies`, `code`, `params`), `eval/parse.rs` (`ParsedExpression`) | No change expected: the fit check and null stripping can be written in openjd-model over the public accessors |
| D6, D7, D8 | openjd-model | `template/environment.rs` (new field); `validate_v2023_09/env_params.rs` `check_environment` (`:367`, gating), `format_strings.rs` (`build_env_host_requirements_symtab` `:230` for the Job/attached scope; the Step template scope for Step Environments), `wrap_actions.rs:107-168` (single-layer rule at TV), `env_params.rs:711` (constraint 7, unchanged rule); `job/create_job/instantiate.rs:146, :195-245` (Step Environments), `external.rs:660-745` (per-Step set, then `check_wrap_layers` `:691`, then `combine_step_host_requirements` `:731`), `host_requirements.rs:743` (`environment_symtab`, reusable for the `enableIf` scope), `external.rs:827, :958` (`check_attached_wrap_layers`, `check_environment_templates`) | New field, TV checks, JC resolution between effective values and host requirements |
| D7 | openjd-cli | `summary.rs:154-205` (`lists_env_params`, `session_env_infos`); `check.rs` | Reads the model's sets; no logic change expected beyond tests |
| D8 | openjd-sessions | none (`session.rs` enters what it is given) | Specs only (`specs/sessions/session.md`) |
| D10 | fixtures | `crates/openjd-model/tests/fixtures/env_params/`, `crates/openjd-cli/tests/templates/env_params/` (copies of the spec samples) | Updated in the PR that lands each feature |
| all | specs | `specs/model/{template-types,validation,job-creation,job-types,step-dependencies,parsing,parameters,public-api,README}.md`, `specs/cli/{check,run,summary}.md`, `specs/openjd-for-js/spec.md`, `specs/sessions/{session,public-api}.md`, `crates/openjd-model/README.md` | Alongside each PR |

Plans 1 and 2 are historical and keep the old names.

## Design decisions

Q1–Q12 extend plan 2's P1–P8.

1. **Q1. The Rust names follow the spec.** `template::Environment::parameter_definitions` becomes
   `env_parameter_definitions` (serde `envParameterDefinitions`), which also stops it reading like the Environment
   Template's top-level `parameter_definitions`. `template::RunScope` becomes `template::EnvironmentActivation { Always,
   OnRequest }` (`SCREAMING_SNAKE_CASE`, so `ALWAYS` and `ON_REQUEST`), and `Environment::run_scope` (field and
   method) becomes `activation`. No serde alias keeps the old names: the spec has none, and `deny_unknown_fields` then
   rejects `parameterDefinitions`, `runScope`, `ALL`, and `ONLY_REQUESTED` on an Environment, which is correct. Every
   message, path element, and doc comment that names the old field or values is restated. Values stay
   case-sensitive (A24).
2. **Q2. `step:` is a template-only synonym.** `template::StepDependency` gains `step: Option<String>`. `target()`
   returns `DependencyTarget::Step(name)` when exactly one of `depends_on`, `step`, and `environment` is present, so
   self, unknown-Step, duplicate (`structure.rs:240`, which then catches a `step:` and a `dependsOn:` naming one Step),
   cycle, and graph checks need no change. The key as written is needed for messages and for the `Display` of `DependencyTarget`, which today prints
   `dependsOn: X` for every Step entry (`step.rs:326`) and whose `Step(&str)` variant carries no key, so
   `DependencyTarget::Step` gains the key: `Step(&str, StepKey)` with a public `StepKey { DependsOn, Step }`
   (a public API change, listed below). With `ENV_PARAMS`, constraint 1's
   messages name all three keys (`must have exactly one of 'dependsOn', 'step', or 'environment'`, with `, not both.`
   when more than one is present). Without it, the existing design stands (`structure.rs:1094-1121`,
   `specs/model/validation.md:118-121`, pinned by `test_env_params_schema.rs:104-117`): an entry with no key is
   reported as missing `dependsOn`, since neither other form exists there; an entry with `step` plus `dependsOn` or
   `environment` keeps the "not both" message, which names only the keys it has. `step` requires `ENV_PARAMS` in the document, reported next to the
   `environment:` gate (`env_params.rs:305`): "the 'step' form of a dependency requires the ENV_PARAMS extension."
   `job::StepDependency { depends_on }` is unchanged, so a `step: X` entry becomes `dependsOn: X` in the Job and the
   Job JSON, sessions, CLI, and openjd-for-js are unaffected.
   *As implemented (PR 2):* with more than one key the message names the keys written:
   `…, not both 'step' and 'environment'.` for two and `…, not all three.` for three, since "not both" cannot
   describe three keys. Without `ENV_PARAMS`, two keys give `must have exactly one of 'X' or 'Y', not both.`; three
   give the all-forms message with `, not all three.`.
3. **Q3. One expression-field type and checker, used by both `enableIf` fields.** A new public
   `template::ExprField` replaces `template::EnableIf`: `Bool(bool)`, `Int(i64)`, `Float(f64)`, or
   `Expression(String)` (the text, not a `FormatString`). Decoding accepts any JSON/YAML boolean, number, or string,
   so that a scalar of the wrong type (`enableIf: 1`) is reported by template validation with its path and the §7.5
   rule rather than by serde. A present `null` is an error (§7.5) reported at the field's path at template validation;
   since `Option<ExprField>` with `serde(default)` would read it as absent, the field uses a `deserialize_with` that
   distinguishes present-null (the mechanism is PR 3's choice). A crate-private checker takes the field, its target
   `ExprType`, and its symbol table:
   * at template validation it parses the string with `ParsedExpression` under the document's profile; on a parse
     error whose text contains `{{` it adds "an expression field takes an expression without '{{ }}' (Template
     Schemas §7.5)"; it type-checks against the symbol table and applies the fit rule (Q4);
   * at job creation it evaluates and checks the value: `null` is an error naming §7.5 and the field's section, an
     `int` widens for a `float` target, and anything else that does not fit is an error.
   A boolean, integer, or float scalar is checked for fit and used as written. `Float(f64)` is not `Eq`, while
   `StepDependency` derives `PartialEq, Eq` (`template/step.rs:340`, `specs/model/public-api.md:450`): PR 3 either
   implements `Eq` for `ExprField` by hand (comparing the float's bits, as written values are never NaN) or drops
   `Eq` from `StepDependency` as a public API change; the first is preferred. `StepDependency::literal_enabled`
   keeps its contract: `Some(b)` for an absent field (`true`) or a `Bool` scalar, `None` otherwise.
   *As implemented (PR 3):* a present `null` decodes as a fifth variant, `ExprField::Null`, through
   `expr_field::deserialize_enable_if`, so template validation reports it at the field's path (A36).
   The `{{` hint follows the parse error's message and precedes its expression and caret lines, so
   the error stays one block. `template::Environment::enable_if` is added now, decoded with the
   same type and rejected by pass 11 as not yet implemented, so PR 4 adds its semantics only. The
   checker's messages are generic over the target type and name §7.5 only, not the field's own
   section (the path names the field). A list or map value is a decode error naming the field.
4. **Q4. The fit rule, over openjd-expr's public type API.** For static type `T` and target `G`: an
   `unresolved[U]` is checked as `U`; if `G` admits `nulltype`, `T` must satisfy `G`; otherwise `T = nulltype` is an
   error, and for a union `T` its `nulltype` members are removed and the rest must satisfy `G`. `int` satisfying a
   `float` target is added, element-wise for lists. `ExprType::satisfies` (`openjd-expr/src/types.rs:595`) already
   implements ExprLang §1.2.3 step 1, and `code()` / `params()` are public, so this lives in openjd-model; openjd-expr
   changes only if PR 3 finds a helper belongs there, in which case `specs/expr/public-api.md` changes with it. Under
   this rule a `string` for `bool` is an error whatever its text (JT-149); plan 2's A25 reading (no coercion, `bool?`
   accepted, `null` a job-creation error) is now the spec, and its "no target type" implementation note is replaced
   by this checker.
5. **Q5. "Known at template validation" means absent or a `Bool` scalar.** For the §3.2 checked-at paragraph
   (constraint 7, requests) and for the single-layer rule (Q9), only an absent `enableIf` or a `Bool` scalar is known
   at template validation; every `Expression`, including a constant one such as `"true"`, is decided at job creation
   (D5). A28 (constraints 5–7 skipped for a literal `false` entry) still holds, for a `Bool(false)` scalar.
6. **Q6. Environment `enableIf` is one field on `template::Environment`.** `enable_if: Option<ExprField>`, valid on
   all three positions (unlike `activation`, `hostRequirements`, and `envParameterDefinitions`, it is permitted on
   `stepEnvironments`). It needs both `ENV_PARAMS` and `EXPR` in the document that contains the Environment, with one
   message per missing extension (JT-138, JT-139, ET-42). Its symbol table at template validation is the
   `hostRequirements` table (`build_env_host_requirements_symtab`, PR B) for a Job Environment or an Environment
   Template, and the Step's template scope (the `parameters`-value table, including `Step.Name` and the Step's `let`)
   for a Step Environment. A28's analogue: an Environment with `enableIf: false` is still validated in full.
7. **Q7. Step Environment `enableIf` is resolved in the `create_job` body.** It reads no effective values, so it is
   evaluated per Step in `instantiate_step` (`instantiate.rs:40`) after the Step's `let`, in the same scope as entry
   `enableIf`; the Step's full template scope exists only there. This evaluates it earlier than §7.4 lists it, which
   changes nothing when there is no error, since the final sets are the same, but an error in it aborts before the
   submission stage (`create_job/mod.rs:165-171`) and so hides that stage's errors (A38). A disabled Step Environment is dropped from `job::Step::step_environments`; if none remain, the field
   is `None`, so the Job serializes as if the Step had none.
8. **Q8. Job and attached Environment `enableIf` is resolved in the submission stage, between effective values and
   host requirements.** In `external.rs` after each Step's `entered` list is computed (`:665-688`) and before
   `check_wrap_layers` (`:691`) and `combine_step_host_requirements` (`:731`): for each Step and each Environment its
   Sessions would enter that has an `Expression` (a `Bool` is used directly), evaluate with the table
   `host_requirements.rs` `environment_symtab` (`:743`) builds from that Step's effective values. Steps with equal
   effective values for the Environment share one evaluation, as host requirements do (P6). An error is a `Submission`
   error at `<environment path> -> enableIf` with "Resolved for Step 'A'." appended. A `false` clears the Environment
   from the Step's `entered` list, so `job::Step::environments` becomes `Some` of the rest, its
   `environment_parameters` entry is removed (effective values exist only for entered Environments), its
   `hostRequirements` are never resolved for the Step, and `check_wrap_layers` sees the final set. Constraint 7 and
   the value checks have already run, so they apply to a Step for which the Environment is disabled (D7, A33).
9. **Q9. The single-layer rule at template validation follows Q5.** `validate_wrap_actions_job_template`
   (`wrap_actions.rs:107-210`, per-Step loop from `:167`) counts a wrapping Environment whose `enableIf` is absent or `Bool(true)`, skips one
   with `Bool(false)`, and leaves a Step to job creation when any wrapping Environment in its candidate set has an
   `Expression` (JB-100 accepted, JB-101 rejected at job creation). `check_environment_templates` treats an attached
   Environment the same way: `Bool(false)` is not entered, an `Expression` makes it ambiguous, as PR C did for an
   entry whose `enableIf` is an expression, for both its wrap-layer and literal host-requirement checks.
10. **Q10. `Job.EnvNames` and Session sharing need no new code.** `Job.EnvNames` is built from names and already
    lists every Job Environment (JB-99, `7.3.1--env-names-includes-disabled-environment`, pins it). Session sharing
    compares `Step::environments`, `step_environments`, and values (`Step::same_environments`), which Q7 and Q8 make
    reflect `enableIf`; the doc comments say so.
11. **Q11. No new job-type field.** `job::Environment` gets no `enable_if`: the field is resolved before a Job exists,
    as entry `enableIf` is (plan 2 P5). `openjd summary`'s "Session environments" already reads the set, so a disabled
    Environment drops out of it.
12. **Q12. Fixtures follow the spec samples in the PR that lands each feature.** PR 2 renames the fields in every
    fixture and switches `video-sfm-composite.yaml`'s `dependsOn:` entries to `step:`; its entry `enableIf`s keep the
    `{{ }}` form the prototype accepts until PR 3, which writes them as expression fields; PR 4 adds the Conda
    sample's Environment `enableIf`, its `EXPR` extension, and removes its "no packages" shell branch.

## Public API changes

| PR | Crate | Change |
|----|-------|--------|
| 2 | openjd-model | `template::Environment::parameter_definitions` → `env_parameter_definitions`. `template::RunScope { All, OnlyRequested }` → `template::EnvironmentActivation { Always, OnRequest }` (with `as_str` returning `ALWAYS` / `ON_REQUEST`). `template::Environment::run_scope` (field and method) → `activation`. `template::StepDependency` gains `step: Option<String>`. `template::DependencyTarget::Step(&str)` becomes `Step(&str, StepKey)`, with a new public `template::StepKey { DependsOn, Step }`, so `Display` prints the key as written (Q2); `DependencyTarget::step()` is unchanged. `ModelExtension::EnvParams` doc comment (`types.rs:364`) names the new fields. |
| 3 | openjd-model | New `template::ExprField` (Q3), with a `Null` variant for a present `null` (see Q3, as implemented). `template::EnableIf` removed; `template::StepDependency::enable_if` becomes `Option<ExprField>`. `StepDependency::literal_enabled` keeps its signature and contract. As implemented, `template::Environment::enable_if: Option<ExprField>` is added here too, decoded and rejected as not yet implemented. |
| 4 | openjd-model | `template::Environment::enable_if: Option<ExprField>` semantics (the field itself lands in PR 3), and a `literal_enabled` helper like the entry's. No job-type change: `job::Step::environments`, `step_environments`, and `environment_parameters` are documented to exclude Environments whose `enableIf` is `false` for the Step. As implemented, also `CreatedJob::disabled_environments` and the new `StepDisabledEnvironments` (re-exported at the crate root), for display. |
| 2–4 | openjd-cli, openjd-sessions, openjd-for-js | No Rust API change expected. The Job JSON shape is unchanged by every PR (Q2, Q11); the openjd-for-js decoded template fields follow openjd-model's names. |
| 2–4 | openjd-expr | None expected (Q4). |

Every PR updates `specs/model/public-api.md` for its row.

## Ambiguities and the prototype's interpretation

Numbered after plan 2's A23–A32. CT-n is the conformance README's "Rules without a definite test" item n.

| # | Ambiguity | Interpretation | Clarify spec? |
|---|-----------|----------------|---------------|
| A33 | Whether an Environment's effective values are checked for a Step for which its `enableIf` is `false`: §7.4 checks them (constraint 7 included) before Environment `enableIf`, but §4.5 item 2's "checked only where it becomes an effective value, that is, for each Step that enters the Environment without supplying the parameter" could be read as excusing a Step that never enters. | Checked: §7.4's order is explicit, and the enableIf may itself read the values (Q8). | Yes: one sentence in §4 item 6. |
| A34 | The meaning of "every Environment involved" for the single-layer rule (§4 item 6 tail). | The wrapping Environments that the Step's Sessions would enter by `activation` and enabled entries, plus its wrapping Step Environments; a non-wrapping Environment's `enableIf` does not defer the check (Q9). | Yes. |
| A35 | `RawParam.<PATH>` of a Job Parameter in an Environment `enableIf` (CT-14 extends to it). | Permitted, as A27 for `hostRequirements`, since the scopes are the same. | Yes, with A27. |
| A36 | Where a present YAML `null` for an expression field is reported. §7.5 makes it an error; it does not say at which stage. | Template validation, at the field's path (§7.5 rule 4 puts the other form errors there). | No. |
| A37 | Whether the `{{` hint applies when the parse fails for another reason too. | Added whenever the text contains `{{`, since it is a "should". | No. |
| A38 | Whether §7.4's order constrains which errors are reported together, as well as the result. | It constrains the result only: a Step Environment `enableIf` error is reported by the `create_job` body, before the submission stage's errors (Q7). | No. |

A24 now reads: `activation` values are case-sensitive (CT-8); `always` is rejected. A25 is resolved by the spec (CT-11,
CT-13); see Q4.

## Open items from epic 9fe8f347 that this delta touches

From task f1a69486's result, and the earlier tasks' results it carries forward:

* **`check --environment` and literal conflicts in the Job Template's own requirements** (f1a69486, for the user
  item 2). Unchanged by the delta, but Q9 changes which Environments `check_environment_templates` counts as
  certainly entered. PR 4 states the new rule in `specs/cli/check.md`; the open question to the user stands.
* **Deferred I2** (check combines an Environment whose values failed; check names only certainly-entered layers).
  Environment `enableIf` adds a third reason a layer is uncertain. PR 4 extends the documented caveat; no new
  deferral is expected.
* **Two `ALL` wrappers in `jobEnvironments` reported again at a Step's `stepEnvironments`** (9d7c8610, for the user
  item 3). PR 4 rewrites that check (Q9); it fixes the double report if it falls out, and otherwise lists it again.
* **Plan 2 P5 "as implemented": `enableIf` resolved with no target type.** Replaced by Q3/Q4 in PR 3.
* **A27 / CT-14** applies to Environment `enableIf` too (A35). The spec notes from PR B on `allOf` union for
  single-valued capabilities, a requirement with both `anyOf` and `allOf`, naming three or more sources, A26 and A29
  are untouched by the delta and stand.
* **`specs/openjd-for-js/spec.md` tables that list getters the wasm API lacks** (f1a69486, item 1). PR 2 edits the
  same file for the renames; it fixes only the renamed names and leaves the table mismatch to the user's decision.

Untouched by the delta and not carried into this plan: the openjd-sessions `resolved_symtab` doc mismatch, empty
`amounts: []`, and PR B's deferred low items.

## PR sequence

One PR per remaining epic task, in order, each merged into `env-params-prototype` before the next starts. Each
follows `implement-change` at the cross-crate tier (2–4 auditors with disjoint angles, mutation auditors one at a
time) and runs fmt, clippy, `cargo test --workspace`, `cargo doc` with `-D warnings`, the copyright check, `cargo
+1.97.0 check --workspace`, the openjd-for-js wasm build and vitest (each PR edits its tests or fixtures), and the
conformance slices `ENV_PARAMS`, `base`, `EXPR`, `WRAP_ACTIONS`. Each reports the tests it flipped and the re-check
group above for its PR.

| PR | Epic task | Title | Crates | Spec | Flips | Expected `ENV_PARAMS` after |
|----|-----------|-------|--------|------|-------|-----------------------------|
| 2 | renames and `step:` | `feat(model)!: envParameterDefinitions, activation, and step: dependencies` | openjd-model; tests and fixtures in openjd-cli, openjd-sessions, openjd-for-js; specs | D1, D2, D3; D10 renames and `step:` | 91: the 89 rename-only tests and the 2 S tests. **Unflips** `job_templates/3.2--enable-if-braces.invalid` (see PR 3). | 267 passed, 22 failed (8 X, 13 E, JT-98) |
| 3 | expression fields | `feat(model)!: expression fields; entry enableIf is @expr bool` | openjd-model (openjd-expr only if Q4 needs it); every test and fixture that writes an entry `enableIf` in the `{{ }}` form (openjd-model `tests/integration/test_env_params_{check,host_requirements,selection,selection_job}.rs`, openjd-cli fixtures, openjd-for-js `js-tests/smoke.test.js:347`); specs | D4, D5; D10 `video-sfm-composite` entry `enableIf` | 9: the 8 X tests and JT-98 | 276 passed, 13 failed (all E) |
| 4 | Environment `enableIf` | `feat(model): enableIf on Environments, end to end` | openjd-model; openjd-cli (`check`, `summary` tests and docs); openjd-for-js tests; specs incl. sessions | D6, D7, D8; D10 Conda `enableIf` | 13: the E tests | 289 passed, 0 failed |

Counts assume Linux (3 Windows-only tests skipped). base, EXPR, and WRAP_ACTIONS must stay 691/0, 383/0, 73/0.

### PR 2 detail

* **Code.** Q1 and Q2 at the D1, D2, D3 rows' locations. Constraint 1's `ENV_PARAMS` messages
  (`structure.rs:1105-1116`) and `specs/model/validation.md:118-121`, the `step` gate, and the `Display` of a Step
  entry.
* **Tests.** Path-and-message assertions for: the renamed fields' gating, placement (not on `stepEnvironments`), and
  unknown-value messages; the old names rejected as unknown fields on an Environment (and `parameterDefinitions`
  still accepted at the Environment Template's top level); `step:` accepted with `ENV_PARAMS` and rejected without,
  `step` with `dependsOn` or `environment` in one entry, `required`/`enableIf`/`parameters` with `step`, self,
  unknown, duplicate across `step:` and `dependsOn:`, a cycle through both forms, and the created Job's
  `dependsOn` for a `step:` entry.
* **Re-check.** The PR 2 group (107), with the S files by hand.

### PR 3 detail

* **Code.** Q3–Q5 at the D4/D5 row's locations: the type, its decode (with present-null), the crate-private
  checker for template validation and job creation, entry `enableIf` switched to it, pass 11's form check replaced,
  pass 8's type check replaced, `entry_enabled` replaced, and `check_environment_templates`' notion of a literal
  entry.
* **Tests.** Every §7.5 rule against entry `enableIf`: each scalar (`true`, `false`, `1`, `1.0`), a string
  expression, `"true"` deferring constraint 7 to job creation, `{{ }}` with the hint, a parse error, `string` and
  `int` rejected for `bool`, `bool?` accepted and `null` rejected at job creation, a `nulltype` expression, an
  `unresolved` operand, present-null rejected; the strict rule must hold where the old code coerced. The checker's
  `int` → `float` and `U?` branches need a test of their own even though no `float` field exists yet (a unit test
  on the checker).
* **Re-check.** The PR 3 group (9).

### PR 4 detail

* **Code.** Q6–Q10 at the D6–D8 row's locations; `openjd check --environment` per Q9; `summary` unchanged except
  tests; the Conda fixtures per Q12.
* **Tests.** Gating (each extension missing), placement on all three positions, both scope tables (each allowed
  and each forbidden reference of §4 item 6 constraints 1–3, including another Environment's parameter and a PATH
  `Env.Param`), per-Step resolution with effective values, `null` and non-bool results, every effect of D7
  (not entered, no `variables`, no host requirements and no resolution error from them, `Job.EnvNames` unchanged,
  constraint 7 not exempted, values still checked per A33), Step Environment `enableIf` with `Step.Name` and `let`,
  Session grouping by the resulting sets, the single-layer rule at template validation for scalars and at job
  creation for expressions (job and attached Environments), and `check --environment` for each.
* **Re-check.** The PR 4 group (14). Then the full run: every `ENV_PARAMS` test should pass; any that does not gets a
  one-line diagnosis in the task result.
* *As implemented (PR 4):* Q6–Q10 as planned. A Step Environment's `enableIf` is decided with the entries in
  `resolve_step_entries`, sharing their scope. Every `enableIf` (an entry's, a Step, Job, or attached
  Environment's) is a TEMPLATE-scope expression field and resolves with POSIX paths whatever the submission path
  format; the submission-path-format re-evaluation of `let` bindings applies only to entry parameter values (a
  change to PR 3's entry `enableIf`, by the user's decision). A Job or attached Environment's is decided by
  `apply_environment_enable_if` in the
  submission stage, between the effective values and the single-layer rule, grouped by values as host
  requirements are; an error leaves the Environment out of those Steps' sets. Every set-dependent check made
  before job creation (the single-layer rule at template validation and in `check_environment_templates`, the
  literal host-requirement combination there, and constraint 7) fails only when the failure is certain at that
  stage: it counts an Environment only when it is certainly entered (an `enableIf` absent or `true`, and an
  entry that requests it likewise), so a violation among certainly entered Environments is reported early, and
  one that an `enableIf` expression, `ON_REQUEST` resolution, or attachment ambiguity could avoid is left to
  job creation.
  For display, `CreatedJob` gains `disabled_environments: Vec<StepDisabledEnvironments>` (not a job type, so Q11
  holds); `openjd summary` lists them under `Disabled environments:`, and `openjd check` prints a line per Step
  for the Environments a scalar `enableIf: false` disables. The CHANGELOGs are generated by release-plz from
  the commits, so they are not edited by hand.

## Spec observations for the user

Not prototype work; recorded so the RFC author can decide.

* **A33–A35** above.
* *Both PR 4 items below were adopted by the spec at `5535230`: §7.5 rule 5 (a check made before evaluation fails
  only when the failure holds for every value the unknown fields could take), applied in §3.2's checked-at
  paragraph and §4 item 6's tail; and §3.2 item 5 / §4 item 6 stating that both `enableIf` fields are evaluated in
  template scope (POSIX paths, no path mapping, template-scope `let` values). The prototype matches `5535230`;
  the conformance tests are unchanged from `2a5a930`.*
* **§4 item 6 tail, the stage of set-dependent checks (PR 4).** ("Every Environment involved" read as the
  Environments that form the violation.) Proposed wording, replacing "is made before the Job
  is created only when the *enableIf* of every Environment involved is absent or a boolean scalar … otherwise it is
  made when the Job is created": "is made at each stage with the information available there: it fails before the
  Job is created when a violation is certain from the Environments every relevant Session certainly enters (those
  whose own *enableIf*, and whose requesting entry's *enableIf*, is absent or `true`; an Environment whose scalar is
  `false` counting as not entered), and otherwise it is made when the Job is created." The prototype implements
  this rule.
* **`enableIf` is evaluated in TEMPLATE scope, not host context (PR 4).** Every `enableIf` (an `environment:` entry's,
  and a Job, Step, or attached Environment's) resolves with POSIX paths, whatever the submission path format, and
  reads `let` bindings with the values job creation computed; only an entry's parameter values follow the
  submission path format (§4.5). The spec could say so in §3.2 item 5 and §4 item 6 (or in §7.5 for every expression
  field resolved at job creation).
* **ExprLang §1.2.2 still disagrees with Schemas §7.3.1** (plan 2): "every template can also read" `Job.EnvNames`,
  "in entry order", without §7.3.1's exclusions or its order wording. The delta did not touch that row.
* **The RFC Validation table's single-layer rule says "absent or a literal"; Schemas §4 item 6 says "absent or a
  boolean scalar".** The table's expression-field rule defines a fitting scalar as a literal, so they agree; the wiki's wording is the more precise.
* No disagreement was found between the epic description and the spec.

## Where this plan supersedes plan 2

* Every use of `parameterDefinitions` (on an Environment), `runScope`, `ALL`, `ONLY_REQUESTED`, and `RunScope` in
  plan 2 now reads as `envParameterDefinitions`, `activation`, `ALWAYS`, `ON_REQUEST`, and `EnvironmentActivation`
  (Q1). Plan 2 is not edited.
* **P5** (entry `enableIf` as `EnableIf::Literal | Expression(FormatString)`, resolved with no target type): replaced
  by Q3–Q5. P5's other parts (no job type records disabled entries) stand.
* **P8** (gating per document) gains `step` (`ENV_PARAMS`) and Environment `enableIf` (`ENV_PARAMS` and `EXPR`).
* **R8** (single-layer rule per Step's set, deferred for an expression entry `enableIf`): extended by Q9 to
  Environment `enableIf`.
* **A24** and **A25**: see above. A28 now applies to a `Bool(false)` scalar only (Q5).
* Plan 2's baseline, classified failures, and re-check list describe the suite at `408910f` and are historical.
