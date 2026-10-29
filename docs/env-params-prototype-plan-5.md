# ENV_PARAMS prototype plan, part 5: the exact attribute conjunction, RANGE_EXPR equality by spelling

This plan brings the `ENV_PARAMS` prototype on `env-params-prototype` (at `f63f2e5`, which matches spec `bf81141`) up to
the specification at `d335619`, in one change made through the `implement-change` skill at the cross-crate tier.
[Plan 4](env-params-prototype-plan-4.md) is the context; its decisions R1–R5 and ambiguities A39–A48 hold except where
this plan says otherwise.

## Sources

* **Normative spec.** `~/projects/rfc-env-params/openjd-specifications-ep`, branch `rfc-env-params` at `d335619`:
  `wiki/2023-09-Template-Schemas.md` §3.3.3 and §3.2.2. `git show d335619` is the delta.
* **Conformance suite.** `conformance-tests/2023-09/ENV_PARAMS/` at `d335619` (README.md; AUDIT.md, the last two
  addenda).
* **Code baseline.** `f63f2e5`.

## What the specification changed

| # | Change | Spec |
|---|--------|------|
| D1 | **Combined host requirements are the exact conjunction**, computed over every source at once, so the result does not depend on source order. Amounts: the greatest `min`, the least `max`. Attributes: values compare ignoring case; a one-value `anyOf` is the `allOf` of that value; *B* is the union of every `allOf` and every one-value `anyOf`. Item 2.1, `attr.worker.os.family` and `attr.worker.cpu.arch`: one `anyOf` of the values in every list; none is a rejection. Item 2.2, any other attribute: one `allOf` of *B* (if not empty) and each `anyOf` of two or more values that contains no value of *B*; never an error. Replaces the pairwise `anyOf` intersection (plan 2 P7) and the mixed-pair rule (plan 4 D5, A44). | §3.3.3 items 1, 2, 2.1, 2.2 |
| D2 | **RANGE_EXPR values are equal only when written the same**: `1-3` and `1,2,3` do not share a Session. Replaces plan 4 R4/A43 (equality by the integers, values stored canonically). | §3.2.2 |

## Baseline

`cargo build --release` at `f63f2e5`, run from the spec checkout at `d335619` with
`PATH="<worktree>/target/release:$PATH" uv run -q --with pyyaml python3.12 run_openjd_cli_tests.py '2023-09/ENV_PARAMS/*'`:
316 passed, 3 failed (3 Windows-only skipped). The 3 are D1's new tests: JB-119 (disjoint custom `anyOf` runs), JB-120
(three sources, order-independent) and JB-121 (`allOf [linux]` against `anyOf [windows, macos]` on `os.family`). D2
has no conformance test (the AUDIT.md addendum says Session sharing is not observable with `openjd run`; see the
spec observations).

## Delta mapped to crates and modules

| # | Crate | Module | Change |
|---|-------|--------|--------|
| D1 | openjd-model | `job/create_job/host_requirements.rs` (`combine`) | Rewritten as one pass per capability over every source. |
| D1 | openjd-model | `job/mod.rs` (`HostRequirements` doc) | A name may repeat: an `allOf` when a source requires a value outright, and any number of `anyOf`s. |
| D2 | openjd-model | `job/create_job/env_params.rs` (`resolve_value`, `values_for_step`, `transport_table`; `canonical_range_expr` deleted), `job/mod.rs` (`range_exprs_equal` deleted), `template/validate_v2023_09/env_params.rs` (`enable_if_certainly_unresolved`) | The value's text is kept in the transport form; comparison is by text. |
| D2 | openjd-cli | `common.rs` (`environment_parameter_entries`) | `run` and `summary` show a RANGE_EXPR value as written. |
| all | specs, tests | `specs/model/{README,job-creation,job-types,public-api}.md`, `specs/cli/{check,run,summary}.md`, `specs/openjd-for-js/spec.md`; model, CLI, openjd-for-js tests | Alongside. No public API signature change. |

## Design decisions

1. **S1. One combination per capability.** `combine` collects every source's requirements on a capability first and
   then forms the result, so no step depends on the order of sources. A capability that only one source names is
   kept as written (plan 2 P7), so a lone `anyOf [a]` stays an `anyOf`.
2. **S2. The combined form** (§3.3.3 says an implementation may store it). Item 2.1: one `anyOf` of the common values,
   in the order and spelling of their first appearance. Item 2.2: the `allOf` of *B* first (values in first-appearance
   order and spelling), then each remaining `anyOf` in source order, as written. The name keeps the first source's
   spelling.
3. **S3. The RANGE_EXPR text** is kept as the transport value, so `Step::environment_values_equal`, Session grouping and
   every shared resolution compare it as text. A Session parses it, as it parses a RANGE_EXPR Job Parameter's value, so
   `Env.Param.*` is the `range_expr` the Expression Language makes of that text; openjd-expr is unchanged.

## Ambiguities and the prototype's interpretation

Numbered after plan 4's A48.

| # | Ambiguity | Interpretation | Clarify spec? |
|---|-----------|----------------|---------------|
| A49 | Which sources an item 2.1 rejection names: §3.3.3 says "the conflicting sources", but no common value is a property of the whole set, and a smaller subset may already conflict. | Every source that names the attribute, in source order, the list capped as the other job-creation lists are (512 characters, then "and N more"). | Maybe |
| A50 | Whether item 2.2 keeps two equal `anyOf`s, or an `anyOf` that is a superset of another. "each `anyOf`" keeps them. | Kept, each as written. | No |
| A51 | Whether an `anyOf` whose values are all one value ignoring case (`[q, Q]`) has "one value". | Yes, as §3.3.2 compares values (plan 4 kept the same rule). | No |
| A52 | How a RANGE_EXPR value that an expression computes is "written". A literal and a format string have a text; a single `{{ }}` expression with `EXPR` evaluates to a value. | A literal's text as written; a format string's resolved string. A single expression evaluates with `range_expr` as its target type (ExprLang §1.3.2), so even a string result converts, and the `range_expr` is written as openjd-expr prints it: `{{Param.R}}` with RANGE_EXPR `R` `1,2,3`, and `{{ '1,2,3' }}`, are both written `1-3`, equal to a literal `1-3` and not to a literal `1,2,3`. The value is then the `range_expr` that text parses to, as for every other source, since openjd-expr's printed form does not always parse back to the same `range_expr` (`19-18:-5,13-18` prints `13-18,19`, whose `range_expr` prints `13-19`) and a Session has only the text. | Yes |
| A53 | Whether white space is part of the spelling (`' 1-3'` against `'1-3'`). | Yes: the text is compared as written. | Maybe |
| A54 | What `Env.Param.*` yields for two Steps whose values are not equal but denote the same integers. | What a RANGE_EXPR Job Parameter with that text yields: `1,2,3` and `1-3` are both the `range_expr` `1-3`, printed `1-3`, though their Steps do not share a Session. | Maybe |

## Spec observations for the author

* **A49**: item 2.1's "the conflicting sources" could say "the sources that name the attribute".
* **A52**: §3.2.2 "written the same" does not say what an expression-valued RANGE_EXPR is written as.
* **RANGE_EXPR printing.** openjd-expr's printed form of a `range_expr` is neither stable under a re-parse
  (`13-18,19` reprints as `13-19`) nor structure-preserving (`3-7:4` prints `3,7`), and `==` on two `range_expr`s
  compares their sub-ranges, not the integers; any rule that writes a computed RANGE_EXPR as text depends on it.
* **§3.2.2 and the Session's value (A54).** Two Steps that do not share a Session because of spelling still see one
  `Env.Param.*` value when the Expression Language normalizes both texts to one `range_expr`; the rationale could say
  equality by spelling is a choice for simplicity, not because the Session can tell them apart.

* **§3.2.2 equality and RANGE_EXPR testing.** `openjd run` reports "Starting a new Session for Step ..." when values
  differ, so Session sharing of RANGE_EXPR values is observable there after all; the AUDIT.md addendum says it is not.
  (Superseded: `openjd run` now keeps one Session and exits and re-enters the Environment instead, which its
  `Exiting Environment` and `Entering Environment` banners still make observable.)
* **Conformance.** JB-66–JB-69, JB-89, and JB-121 are rejected by the item 2.1 rule (each `openjd run` message names
  the attribute "of which a host has exactly one value"); JB-119 and JB-120 run.

## Out of scope, for a decision

* **RANGE_EXPR `minLength`/`maxLength`.** For an Environment parameter they measure the string openjd-expr prints the
  parsed `range_expr` as, not the text as written: `'1,2,3'` passes `maxLength: 3` (it prints as `1-3`), and
  `'1,3,5,7,9'` fails with "value length 9". With equality by spelling the text is the value, so the length could be
  the text's. For a RANGE_EXPR Job Parameter they are not checked at all (pre-existing): `default: '1,3,5,7,9'` with
  `maxLength: 3` passes `openjd check` and runs. Unchanged here; specs/model/job-creation.md records the Environment
  parameter rule.

* **Repeated resolution errors per spelling** (audit round 1, low, deferred). An Environment's `hostRequirements` are
  resolved once per distinct transport form of a Step's values, so Steps whose RANGE_EXPR values denote the same
  integers but are written differently resolve, and report a failure, separately, though the expression cannot tell
  them apart. Each error is still correct and located.

## Where this plan supersedes earlier plans

* **Plan 2 P7** (pairwise intersection and union, the mixed pair kept), **plan 4 D5 and A44** (item 4 after the
  intersection), and the "name may appear twice" rule of `job::HostRequirements` are replaced by S1 and S2.
* **Plan 4 R4 and A43** (RANGE_EXPR stored canonically) are reversed by S3; `canonical_range_expr`, its overflow and
  wasm32 fixes, and `range_exprs_equal` are deleted.
