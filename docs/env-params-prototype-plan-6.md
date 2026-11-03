# ENV_PARAMS prototype plan, part 6: ENV_PARAMS requires EXPR

This plan brings the `ENV_PARAMS` prototype on `env-params-prototype` (at `82507ee`, which matches spec `d335619`) up to
the specification at `2491d6e`, in one change made through the `implement-change` skill at the cross-crate tier.
[Plan 5](env-params-prototype-plan-5.md) and [plan 4](env-params-prototype-plan-4.md) are the context; their decisions
and ambiguities hold except where this plan says otherwise. (Later superseded: the Session grouping of `openjd run` in
the earlier plans, `session_groups` and a new Session per group, is replaced by one Session that exits and re-enters
Environments between Steps; see `specs/cli/run.md`.)

## Sources

* **Normative spec.** `~/projects/rfc-env-params/openjd-specifications-ep`, branch `rfc-env-params` at `2491d6e`:
  `wiki/2023-09-Template-Schemas.md` §1.1 item 3, §1.2 item 3, and the removals listed below; `rfcs/0010-*.md`
  Backward compatibility and the rationale "ENV_PARAMS requires EXPR". `git show 2491d6e -- wiki rfcs` is the delta.
* **Conformance suite.** `conformance-tests/2023-09/ENV_PARAMS/` at `2491d6e` (README.md V36; AUDIT.md, last addendum).
* **Code baseline.** `82507ee`.

## What the specification changed

| # | Change | Spec |
|---|--------|------|
| D1 | **A document that lists `ENV_PARAMS` must also list `EXPR`**, and an implementation must reject one that does not. The rule is on the list, whether or not any ENV_PARAMS field is used; an Environment Template has the Job Template's prerequisites. | §1.1 item 3, §1.2 item 3 |
| D2 | **Removed: every rule for an ENV_PARAMS document without `EXPR`.** Constraint 6's last sentence (no BOOL, RANGE_EXPR, or LIST value from a document without `EXPR`) and §3.2.2's exception keeping it for a disabled Environment; the paragraph that the requesting document's extensions decide a value's forms; the `EXPR` gating of an entry's `enableIf` (constraint 9) and of an Environment's (§4 item 6 constraint 4); the "with the `EXPR` extension" qualifiers in §3.2, §4 item 5.1, §4.5 items 1 and 4 and the PATH paragraph (a URI PATH value), §4.5 `Env.Param.*` types, §7.3.1 `Job.EnvNames`, and the RFC's §7.4 modification text (the wiki's §7.4 is unchanged). | §3.2, §3.2.2, §4, §4.5, §7.3.1; RFC §7.4 |

A Job Template that lists neither extension keeps its meaning, and may still be attached to an Environment Template
that lists both (JB-74, JB-75).

## Baseline

`cargo build --release` at `82507ee`, run from the spec checkout at `2491d6e` with
`PATH="<worktree>/target/release:$PATH" uv run -q --with pyyaml python3.12 run_openjd_cli_tests.py '2023-09/<slice>/*'`:
`ENV_PARAMS` 302 passed, 3 failed (JT-160, JT-161, ET-43, the new tests of D1); `base` 691/0, `EXPR` 383/0,
`WRAP_ACTIONS` 73/0.

## Delta mapped to crates and modules

| # | Crate | Module | Change |
|---|-------|--------|--------|
| D1 | openjd-model | `template/validate_v2023_09/{mod,env_params,wrap_actions}.rs` | `env_params::check_expr_prerequisite`, called first by `validate_job_template` and `validate_environment_template`; on failure they return at once (with the `WRAP_ACTIONS` prerequisite error too when it applies). |
| D2 | openjd-model | `template/validate_v2023_09/env_params.rs` | Deleted: the `EXPR` gates of both `enableIf`s (`check_enable_if_form`), the no-`EXPR` bool and list literal forms, the Environment parameter type check (every type is allowed with `EXPR`), and `supplier_has_expr` in `enable_if_certainly_unresolved` (plan 4 A48). |
| D2 | openjd-model | `template/validate_v2023_09/format_strings.rs` | Deleted: the no-`EXPR` branches for `default`s and entry values (complex-expression rejection, no target type) and the `expr_active` gating of the entry `enableIf` check and the Environment `hostRequirements` and `enableIf` tables. |
| D2 | openjd-model | `template/parse.rs` | The decode-time type-name case check no longer walks `envParameterDefinitions`: with `EXPR` names are case-insensitive, and any other document that has them is rejected. |
| D2 | openjd-model | `job/create_job/{env_params,external,host_requirements,mod}.rs` | Deleted: `requires_expr`, `supplier_lacks_expr` (constraint 6's last sentence), the `has_expr` fields of `Supplied`, `DeclaringEnvironment`, `JobEnvironments`, and `EnvironmentSource`, the no-`EXPR` format-string conversion and URI message, and the per-attachment `EXPR` check of the `enableIf` table. |
| all | specs, docs, tests | `specs/model/{README,validation,job-creation,parsing,parameters,template-types,public-api}.md`, `specs/cli/{check,run}.md`, `specs/openjd-for-js/spec.md`, `crates/openjd-model/README.md`; model, CLI, sessions, openjd-for-js tests and fixtures | Every document that lists `ENV_PARAMS` lists `EXPR`; tests of the deleted rules are deleted or rewritten for the new rule. No public API signature changes. |

## Design decisions

1. **E1. The prerequisite gates the whole pipeline.** It is checked before pass 5 and, when it fails, is the only error
   (with `WRAP_ACTIONS`' prerequisite, which concerns the same list). This is what makes the no-`EXPR` paths
   unreachable, so they are deleted rather than kept; the message and location (`extensions`) follow the `WRAP_ACTIONS`
   prerequisite's, citing §1.1 item 3 for a Job Template and §1.2 item 3 for an Environment Template.
2. **E2. Rules that serve documents without `ENV_PARAMS` stay.** The `EXPR` gating of Job Parameter types, `let`,
   complex expressions, `Job.Name`, and `Step.Name` in base documents is unchanged, and `Job.EnvNames` is still defined
   only with `ENV_PARAMS`, so a document that lists only `EXPR` still gets "Undefined variable" for it. An attached
   Environment Template that lists only `EXPR` still freezes `Job.Name` in its `resolved_symtab`, and one that lists
   neither does not.
3. **E3. Decode errors come first.** The prerequisite is a template-validation rule; the decode-stage errors
   (unsupported or duplicate extension names, a Job Parameter or Task Parameter type name in the wrong case without
   `EXPR`) are still reported before it, as before. Environment parameter type names are no longer collected for the
   case check (see D2, `parse.rs`).

## Ambiguities and the prototype's interpretation

Numbered after plan 5's A54.

| # | Ambiguity | Interpretation | Clarify spec? |
|---|-----------|----------------|---------------|
| A55 | Whether the new rule's rejection should be the only error, or reported beside the document's other errors. §1.1 item 3 says only that the document must be rejected. | The only error (E1): every other rule assumes `EXPR`, so further messages would describe a document the specification does not define. | No |
| A56 | Whether a document that lists `ENV_PARAMS` and `EXPR` but is evaluated under a context that enables only `ENV_PARAMS` (an API caller's `create_job` context) is affected. | No: `create_job` already requires a context that enables every extension the template lists. | No |

## Spec observations for the author

* **§1.2 item 3's "prerequisites".** The Job Template's item 3 states the prerequisite as a sub-item of the list of
  names; §1.2 item 3 now says "the same available names and prerequisites", which is clear, but the RFC's Validation
  table row ("A document that lists `ENV_PARAMS` also lists `EXPR`") could name both templates explicitly.
* **`ON_REQUEST` rationale.** The RFC's "`ON_REQUEST` is the part of that which needs no expression" still reads as if a
  document might lack `EXPR`; it now means only that `activation` is not itself an expression.
* **Conformance.** JT-160, JT-161, and ET-43 are rejected with the new rule's message at `extensions`.

## Where this plan supersedes earlier plans

* **Plan 4 A48** (a value a document without `EXPR` may not supply is unknown to the `enableIf`) has no case left, and
  neither has the first half of **plan 1 A12** (whose `EXPR` admits a URI PATH value; a value's forms follow the
  document that holds it): every document that holds a value lists `EXPR`. A12's second half, that the caller's
  `ValidationContext::allow_uri_path_values` decides whether a URI is accepted, still holds.
