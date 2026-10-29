# ENV_PARAMS prototype plan, part 4: disabled Environments, RANGE_EXPR, the full parameter shape

This plan brings the `ENV_PARAMS` prototype on `env-params-prototype` (at `412eebc`, which matches spec `5535230`) up to
the specification at `bf81141`, in one change made through the `implement-change` skill at the cross-crate tier.
[Plan 3](env-params-prototype-plan-3.md) is the context; its decisions Q1–Q12 and ambiguities A33–A38 hold except where
this plan says otherwise.

## Sources

* **Normative spec.** `~/projects/rfc-env-params/openjd-specifications-ep`, branch `rfc-env-params` at `bf81141`:
  `wiki/2023-09-Template-Schemas.md` ("Schemas"; section numbers are Schemas sections unless noted),
  `wiki/2026-02-Expression-Language.md` ("ExprLang"), and `rfcs/0010-environment-parameters.md`. `git diff 5535230
  bb10e3b` is prose only; `git show bf81141` is the delta.
* **Conformance suite.** `conformance-tests/2023-09/ENV_PARAMS/` at `bf81141` (README.md and the AUDIT.md addenda
  describe each new test).
* **Code baseline.** `412eebc`.

## What the specification changed

| # | Change | Spec |
|---|--------|------|
| D1 | **Disabled Environments.** An Environment is disabled for a Step when its own `enableIf` is `false` for the Step. A literal `false` is known at template validation; an expression is evaluated at job creation *before* constraint 7 and the value checks, with what is known: an effective value that converts to its type is known (whether or not it meets its constraints), and a missing or unconvertible one is `unresolved[T]`; progressive evaluation gives `true`, `false`, or `unresolved[bool]`. Disabled: not entered, not held to constraint 7, values not checked against type and constraints (constraint 6's `EXPR` rule still applies). Not disabled (`true` or unresolved): constraint 7 and the value checks apply. Replaces plan 3 A33 ("Checked") and §7.4's old order. | §3.2.2 new ¶; §3.2 constraint 7 and checked-at ¶; §3.2.1; §4 item 6 tail; §4.5 items 5, PATH ¶; §7.4 |
| D2 | **RANGE_EXPR** is an Environment parameter type (needs `EXPR`); a document without `EXPR` must not supply one (constraint 6); two RANGE_EXPR values are equal when they denote the same integers. | §4.5 item 1; §3.2 constraint 6; §3.2.2 |
| D3 | **`userInterface`, `objectType`, `dataFlow`** permitted, with the Job Parameter's shape and constraints, informational only. | §4.5 items 2–3 |
| D4 | **`RawParam.*` of any type** in an Environment's `hostRequirements` and `enableIf`. | §4 item 5.1, item 6 constraints 1–2 |
| D5 | **§3.3.3 item 4:** a one-value `anyOf` is the `allOf` of that value and combines with an `allOf` by union. | §3.3.3 |
| D6 | §7.5 rule 5 adds a "should report at the earliest stage" preference. | §7.5 |
| D7 | ExprLang §1.2.2 `Job.EnvNames` row defers to §7.3.1. | ExprLang |

## Baseline

`cargo build --release` at `412eebc`, run from the spec checkout at `bf81141` with
`PATH="<worktree>/target/release:$PATH" uv run -q --with pyyaml python3.12 run_openjd_cli_tests.py '<slice>'`:
`ENV_PARAMS` 297 passed, 19 failed (3 Windows-only skipped); `base` 691/0, `EXPR` 383/0, `WRAP_ACTIONS` 73/0.
The 19 are this delta's tests: D1 JT-148, JT-156–JT-158, JB-108, JB-110, JB-113, JB-115, JB-116; D2 JT-23, ET-11,
JB-117; D3 JT-24–JT-26, JT-120, ET-12, ET-34, ET-35. Three passing tests pass for a reason that is not yet
implemented: JB-118 (RANGE_EXPR is rejected outright), and JB-112 and JB-114 (rejected because values and
constraint 7 are checked before the `enableIf`, not because the `enableIf` is unresolved). D4 is already
implemented (plan 2 A27, plan 3 A35): JT-153–JT-155 and JB-111 pass. D5 has no conformance test.

## Delta mapped to crates and modules

| # | Crate | Module | Change |
|---|-------|--------|--------|
| D1 | openjd-model | `job/create_job/env_params.rs` (`DeclaringEnvironment`, `resolve_step_entries`, `JobEnvironments`, `StepSelection`), `external.rs` (`apply_environment_templates`, `check_environment_templates`; `apply_environment_enable_if` removed), `mod.rs`, `template/expr_field.rs` (progressive resolve) | Values computed first with deferred checks; the Environment's `enableIf` decided with known/unresolved values; checks and constraint 7 reported only where not disabled. A Job Environment's `enableIf` moves from the submission stage into the `create_job` body. |
| D1 | openjd-model | `template/validate_v2023_09/env_params.rs` (constraint 6 literal values, constraint 7, the constraint 7 note), `format_strings.rs` (pass 8 target type of entry values) | Checked only for an Environment whose `enableIf` is absent or `true`, or certainly unresolved for the Step (§7.5 rule 5; R3). |
| D2 | openjd-model | `validate_v2023_09/env_params.rs` (`check_definition`), `job/create_job/env_params.rs` (`to_transport`, new `canonical_range_expr`) | RANGE_EXPR allowed with `EXPR`; values stored canonically. |
| D3 | openjd-model | `validate_v2023_09/env_params.rs`, `template/parameters.rs` (`has_user_interface` removed), `template/env_parameters.rs` | Rejections removed. |
| D5 | openjd-model | `job/create_job/host_requirements.rs` (`combine`) | One-value `anyOf` joins the `allOf`. |
| all | specs, tests | `specs/model/{validation,job-creation,job-types,template-types,public-api}.md`, `specs/cli/{check,run}.md`, `specs/openjd-for-js/spec.md`; model, CLI tests | Alongside. No public API change. |

## Design decisions

1. **R1. One decision point per Environment and Step, after the values.** `DeclaringEnvironment::values_for_step`
   returns `PendingValues`: the values (when complete and valid), the value each definition has for the `enableIf`
   (known or unknown), the deferred value-check errors, and the missing names. `decide` evaluates the `enableIf`;
   `commit` reports what was deferred for a Step it does not disable. Job creation (Job Environments) and the
   submission stage (attachments) share all three, so they behave the same.
2. **R2. A Job Environment's `enableIf` is decided in the `create_job` body**, where its values are computed, so
   its value checks can depend on it. Its evaluation errors become `JobTemplate` errors at
   `jobEnvironments[i] -> enableIf` (they were `Submission` errors), still grouped by known values and naming the
   Steps; they are reported after every Step.
3. **R3. Template validation and `check_environment_templates` check constraints 6 and 7 only for an Environment
   whose `enableIf` is absent or `true`, or certainly unresolved for the Step.** A literal `false` disables it; an
   expression may. The exception is an expression that reads only the Environment's own values, each a literal or
   absent, and is unresolved with them: job creation evaluates it with the same values, so it is unresolved
   whatever the Job Parameters are, and the failure is certain. §7.5 rule 5's "should" prefers reporting it early,
   README item 18 lists JB-114 among such certain failures, and the baseline reported JB-114's shape early, so the
   prototype keeps that report (A45). A known result, as of a constant expression (JT-152), stays unknown, as rule 5
   says. The attachment-level error of an unreachable attachment (§3.2.1 items 3–4) follows the same rule (A46).
4. **R4. RANGE_EXPR values are stored canonically** (greedy arithmetic runs), so equal values have one transport
   form and every comparison (Session grouping, shared resolutions) sees them as equal.
5. **R5. The constraint 7 "conditional" note is replaced** by one that says the Environment's `enableIf` depends on
   a value that is missing or does not convert, so it does not disable the Environment.

## Ambiguities and the prototype's interpretation

Numbered after plan 3's A38.

| # | Ambiguity | Interpretation | Clarify spec? |
|---|-----------|----------------|---------------|
| A39 | Which checks a disabled Environment escapes: §3.2.2 says "type and constraints", the checked-at ¶ says constraints 6 and 7. Constraint 5 (keys), the literal forms no type has (`null`, an object), and the list single-expression rule are not named. | Constraint 5 and the forms apply; the single-expression rule is a type check (a list has no string form), so it is skipped. Pass 8's static target type for an entry value is skipped too. | Yes |
| A40 | What a failed `enableIf` evaluation (an error, `null`) does to the Step's values and constraint 7. | Reported once; the Environment is left out of those Steps' sets and its deferred checks are dropped, so one mistake gives one error. *Revised later (`aad5bd2`): the spec leaves this to the implementation, and openjd-rs now checks the Environment as if it is not disabled, so the Step's other mistakes are reported in the same run.* | Maybe |
| A41 | Whether an unresolved result of a type other than `bool` (`unresolved[bool?]`) counts as unresolved. | Yes: any unresolved result does not disable. | No |
| A42 | Whether §4.5 point 1 (a literal `default` checked whether or not used) still applies when the Environment is disabled for every Step that takes it. | Yes: it checks the definition, not a Step's value. A resolved `default` is checked only for a Step the Environment is not disabled for, naming the first. | Yes |
| A43 | How "equal when they denote the same integers" shows in a Session that runs two Steps whose RANGE_EXPR values are written differently. | Values are stored canonically, so the Session sees one form: `1,3,5,7,9` reaches `Env.Param.*` as `1-9:2`. | Yes |
| A44 | §3.3.3 item 4 with several `anyOf`s: whether "one value" is tested before or after item 2's intersection; where the joined value goes. The order changes the outcome: Step `anyOf [a]`, E1 `anyOf [b, c]`, E2 `allOf [x]` is a conflict when item 2 applies first, and satisfiable (`allOf [x, a]`, `anyOf [b, c]`) when item 4 does. | After the intersection; it is appended to the `allOf`'s union. | Yes |
| A45 | Whether a template-validation check may evaluate an `enableIf` expression with the literal values it reads (JB-114: it reads only the missing parameter, so it is unresolved for every Job Parameter value). §7.5 rule 5 calls the value unknown before its stage; README item 18 counts JB-114 among the certain failures the "should" prefers early. | Yes, only for a result that stays unresolved at job creation (R3); a known result stays unknown. | Yes |
| A46 | Whether an unreachable `ALWAYS` attachment with parameters no Step can supply (§3.2.1 items 3–4) is an error when its `enableIf` disables it. | Not when it is disabled for every Step: no Session enters it. Job creation decides it per Step with the parameters unknown; `check_environment_templates` reports it only for an `enableIf` that is absent, `true`, or certainly unresolved (R3). | Yes (no conformance test) |
| A47 | Whether a value that fails to resolve, not only one that fails to convert (a format string or a resolved `default` whose expression fails, such as an index out of range or a division by zero), is unknown to the `enableIf` and unchecked where the Environment is disabled. §3.2.2 names "a value that does not convert". | Yes: any value that does not yield a value of the parameter's type is unknown, and its error is reported only where the Environment is not disabled. | Yes |
| A48 | Whether a value that a document without `EXPR` may not supply (constraint 6, last sentence), but that would convert, is known to the `enableIf`. §3.2.2 names only missing and unconvertible values as unknown. | Unknown, at every stage: job creation reports the value and does not convert it. | Yes |

## Spec observations for the author

* **A39, A42, A43, A44** above.
* **§7.4 and failed `enableIf`s (A40).** §7.4 now orders the `enableIf` before constraint 7 but says nothing of an
  `enableIf` whose evaluation fails.
* **§7.5 rule 5 and README item 18 (A45).** Rule 5 makes an expression `enableIf` unknown before job creation, so it
  forbids reporting JB-114 at template validation, while item 18 says JB-114 may be rejected at either stage and the
  new "should" prefers the earliest stage. The two should be reconciled: rule 5 could say that an expression field
  whose value depends only on values that stay unknown at its own stage is known to be unresolved there. The
  prototype reports such a failure early (R3).
* **§4 item 6, last paragraph (A45).** It says that before job creation "any other Environment is undetermined", which
  the early report for a certainly unresolved Environment also departs from; the reconciliation should cover it. The
  prototype uses the notion for constraints 6 and 7 and the unreachable-attachment error only, not for the
  single-layer rule or the literal host-requirement combination, which the "should" permits.
* **No conformance test for A46**: an unreachable `ALWAYS` attachment with required parameters whose `enableIf` is
  `false`.
* **§3.2.2 progressive evaluation.** The unknown values are `unresolved[T]` in an evaluation at job creation, where
  the Expression Language's progressive evaluation otherwise has every TEMPLATE-scope symbol concrete; the ExprLang
  could name this use.

## Where this plan supersedes plan 3

* **A33** is reversed by D1. **Q8**'s "constraint 7 and the value checks have already run" no longer holds, and a Job
  Environment's `enableIf` is decided in the `create_job` body (R2), not the submission stage.
* **§4.5 items 1–3** as plan 1 and plan 2 implemented them (RANGE_EXPR and the three fields rejected) are reversed by
  D2 and D3.
