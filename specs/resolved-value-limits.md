# Resolved-Value Constraints and Static Evaluation Gates

**Status: IMPLEMENTED.** Merged: the `openjd-expr`
mechanism (PR [#373](https://github.com/OpenJobDescription/openjd-rs/pull/373),
released in `openjd-expr` v0.7.0); gate 1 and gate 3 enforcement
(PR [#383](https://github.com/OpenJobDescription/openjd-rs/pull/383),
plus review follow-ups in
[#397](https://github.com/OpenJobDescription/openjd-rs/pull/397)); the
opt-in `CallerLimits` caps, evaluation-budget plumbing, sessions
`SessionLimits` surface, and CLI default arg cap
(PR [#399](https://github.com/OpenJobDescription/openjd-rs/pull/399));
and gate 2 — the `create_job` evaluation pass over carried-forward
session/task-scope format strings, with the budgets applied to every
job-creation evaluation
(PR [#404](https://github.com/OpenJobDescription/openjd-rs/pull/404)).
The gate sections below describe the behavior **as shipped**. What
remains is the follow-up list at the
[end of this document](#follow-ups).

Cross-cutting design spanning `openjd-expr`, `openjd-model`, and
`openjd-sessions`. When this design is accepted and implemented, the
per-crate spec documents listed in [Follow-up spec edits](#follow-up-spec-edits)
must be updated to match, per the spec/code co-evolution rule in AGENTS.md.

References into the OpenJD specification below cite the
[2023-09 Template Schemas](https://github.com/OpenJobDescription/openjd-specifications/blob/mainline/wiki/2023-09-Template-Schemas.md)
(§ numbers) and the
[2026-02 Expression Language](https://github.com/OpenJobDescription/openjd-specifications/blob/mainline/wiki/2026-02-Expression-Language.md)
documents on mainline.

## Motivation

Before this design, all three of the following templates passed
`openjd check` and job creation, then degraded the worker host at run
time. Each is resolvable — and therefore rejectable — earlier than
that. (As shipped: Examples 1 and 3 fail `openjd check` under the
CLI's default arg cap, and Example 2 fails at `create_job` the moment
`Count` is bound — with gate 3 still enforcing on the worker.)

**Example 1 — decidable at template validation.** The expression
references no template variables, so it is fully evaluatable the moment
the template is parsed:

```yaml
specificationVersion: jobtemplate-2023-09
name: repeat-static
extensions:
  - EXPR
steps:
  - name: RunScript
    script:
      actions:
        onRun:
          command: echo
          args:
            - "{{ 'A' * 10000000 }}"
```

Template validation (pass 8) evaluates it — `validate_fs` →
`FormatString::validate_expressions` computes the entire 10 MB string
at `check` time — and before this design it then discarded the value
and reported success.

**Example 2 — decidable at job submission.** The expression depends only
on a job parameter, so it becomes fully evaluatable at job creation,
when `Param.Count` is bound to a value:

```yaml
specificationVersion: jobtemplate-2023-09
name: repeat-param
extensions:
  - EXPR
parameterDefinitions:
  - name: Count
    type: INT
steps:
  - name: RunScript
    script:
      actions:
        onRun:
          command: echo
          args:
            - "{{ 'A' * Param.Count }}"
```

Submitting with `Count=10000000` should fail at `create_job`, not on the
worker.

**Example 3 — decidable at template validation despite a host-context
part.** The first interpolation (`Session.WorkingDirectory`) only
resolves on the worker host, so the string as a whole can never be fully
evaluated early — but the second interpolation is static, and its size
alone already exceeds any plausible limit. A *lower bound* on the
resolved length is enough to reject at `check` time:

```yaml
specificationVersion: jobtemplate-2023-09
name: repeat-static-suffix
extensions:
  - EXPR
steps:
  - name: RunScript
    script:
      actions:
        onRun:
          command: echo
          args:
            - "{{ Session.WorkingDirectory }}/{{ 'A' * 10000000 }}"
```

The goal: **whenever a resolved-value constraint violation is knowable at
an earlier stage, fail at that stage.** Three gates:

- **Gate 1 — template validation** (`decode_job_template` /
  `decode_environment_template`, pass 8).
- **Gate 2 — job creation** (`create_job`): `Param.*` / `RawParam.*`
  bound to real values.
- **Gate 3 — run time** (`openjd-sessions` runners): everything resolved.
  This gate is the enforcement boundary — a worker can receive a job that
  never passed through this client's `check`, so gate 3 must hold on its
  own. Gates 1 and 2 are early-failure UX.

This staging is exactly the model the spec prescribes. Template Schemas
[§7.4 "Template Processing Stages"](https://github.com/OpenJobDescription/openjd-specifications/blob/mainline/wiki/2023-09-Template-Schemas.md#74-template-processing-stages)
defines the three stages and which
values are known at each; the Expression Language spec § "Progressive
Expression Evaluation" directs implementations to catch "as many errors
as possible with the information available" at each stage.

## What the spec actually constrains

Surveying the [2023-09 Template Schemas](https://github.com/OpenJobDescription/openjd-specifications/blob/mainline/wiki/2023-09-Template-Schemas.md)
for resolved-value constraints
splits the fields of interest into two groups.

### Group A — spec-mandated post-resolution constraints

These constraints apply **"after the format string has been resolved"**
(the spec's own wording). Violations are spec violations, full stop —
enforcing them early can never reject a template that could have run.

| Spec § | Field | Constraint on the resolved value | Enforced today |
|---|---|---|---|
| [§1.1.1](https://github.com/OpenJobDescription/openjd-specifications/blob/mainline/wiki/2023-09-Template-Schemas.md#111-jobname) | `<JobName>` | ≤ 128 chars (≤ 512 with FEATURE_BUNDLE_1); no Cc chars | Gate 1 (lower bound; full check incl. Cc chars when fully static) + gate 2 (resolved name vs `max_job_name_len`, emptiness, and control characters) |
| [§3.3.2.2](https://github.com/OpenJobDescription/openjd-specifications/blob/mainline/wiki/2023-09-Template-Schemas.md#3322-attributecapabilityvalue) | `<AttributeCapabilityValue>` | ≤ 100 chars; latin alphanumeric + `_` + `-`; must start with letter or `_` | Gate 1 (lower bound; full §3.3.2.2 check when fully static) + gate 2 (`validate_attribute_capability_value` on resolved value) |
| [§3.4.2](https://github.com/OpenJobDescription/openjd-specifications/blob/mainline/wiki/2023-09-Template-Schemas.md#342-taskparameterstringvalue) | `<TaskParameterStringValue>` | ≤ 1024 chars | Gate 1 (lower bound) + gate 2 (`ranges.rs` on resolved range elements) |
| [§4.4.2](https://github.com/OpenJobDescription/openjd-specifications/blob/mainline/wiki/2023-09-Template-Schemas.md#442-environmentvariablevaluestring) | `<EnvironmentVariableValueString>` | ≤ 2048 chars (see [reading note](#reading-note-442)) | Gates 1–2 (lower bound; gate 2 with parameters bound, PR #404) + gate 3 (resolved value) |

<a name="reading-note-442"></a>**Reading note on §4.4.2:** the section
says "A string value subject to … Maximum length: 2048 characters"
without the "after resolved" phrase, but the map value it constrains is
annotated `@fmtstring[host]` (§4.4) — the constraint describes the
environment variable's value, which only exists after resolution. This
design adopts the resolved-value reading: enforce 2048 on the resolved
value at gate 3, with lower-bound early failure at gates 1–2. A raw-text
check would be incorrect in both directions (a 3000-char template can
resolve under 2048; a 100-char template can resolve over it).
**Action item:** ~~check what `openjd-model-for-python` does here~~
(verified — see [open question 2](#open-questions-for-review): Python
checks the raw template text) and file an upstream clarification issue
(**still to do**).

### Group B — fields the spec deliberately leaves uncapped

| Spec § | Field | Spec language |
|---|---|---|
| [§5.1](https://github.com/OpenJobDescription/openjd-specifications/blob/mainline/wiki/2023-09-Template-Schemas.md#51-commandstring) | `<CommandString>` | "There is no maximum string length imposed by this specification. Note that the specific operating system that the command is run on will impose its own maximum length." |
| [§5.2](https://github.com/OpenJobDescription/openjd-specifications/blob/mainline/wiki/2023-09-Template-Schemas.md#52-argstring) | `<ArgString>` | Same language as §5.1. |
| [§6.1.2](https://github.com/OpenJobDescription/openjd-specifications/blob/mainline/wiki/2023-09-Template-Schemas.md#612-datastring) | `<DataString>` | "No length limit is imposed by this specification." |

A default-on length cap for these would reject templates the spec
declares valid — non-conforming validation. The spec instead provides
two levers for the DoS case:

1. **The evaluation memory budget.** The Expression Language spec
   § "Memory-bounded evaluation" requires bounded-memory evaluation with
   a **configurable** limit (default 100 MB recommended) and cites
   *literally this attack* as its purpose: "prevents unbounded resource
   consumption from expressions like `"a" * 10000000`". A host that
   wants Example 1 to fail can lower the memory limit — and
   because gates 1–3 all evaluate, a lowered limit fails at gate 1
   already. This is spec-sanctioned configuration, not a conformance
   deviation.
2. **OS-imposed maxima**, which §5.1/§5.2 explicitly acknowledge. An
   implementation surfacing the OS limit early (rather than letting
   `exec` fail with E2BIG) is making a spec-acknowledged failure legible,
   but the *default* posture must remain uncapped.

Consequently Group B gets **opt-in** caller limits (`None` defaults,
consistent with the existing `CallerLimits` convention), not defaults.

### Existing implementation-added limits (survey)

For completeness, limits our implementation already applies to these
fields beyond serde/type checks — the baseline this design extends:

| Where | Field | Limit | Basis |
|---|---|---|---|
| model decode | `command` raw text | ≤ 1024 (`max_command_len`) | Implementation-added (spec §5.1 sets no max); Python-compat carryover. Unchanged by this design. |
| model decode | `args` raw text | control-character check only | §5.2 charset |
| model decode | env var name | 1–256 chars, charset, no leading digit | §4.4.1 (raw — the name is not a format string) |
| model decode | env var value | ≤ 2048 lower bound (was **none** — gap vs §4.4.2, fixed by PR #383) | §4.4.2, resolved-value reading |
| model decode | any format string | ≤ 1 MB text, ≤ 1000 segments (`MAX_FORMAT_STRING_LEN`, expr defensive caps) | implementation defense-in-depth |
| model decode | expression evaluation | 100 MB memory / 10 M op budgets, now caller-configurable via `CallerLimits` (PR #399) | Expression Language spec, defaults |
| sessions | resolved `notifyPeriodInSeconds` | ≤ 600 | §5.3.2, resolved-value check (the precedent this design generalizes) |
| sessions | resolved env values | ≤ 2048 (was **none**, fixed by PR #383) | §4.4.2 |
| sessions | resolved command/args/data | `SessionLimits` opt-in caps (was **none**, added by PR #399) | Group B, caller policy |

## Why this needs no new evaluation machinery

The unresolved-propagation design already answers "is this expression
static at stage X?" as a byproduct:

- Pass 8 evaluates every format-string expression against a
  scope-appropriate symbol table where unknown symbols are
  `ExprValue::unresolved(T)`. Unresolved-ness propagates. **If the
  result is concrete (not `ExprValue::Unresolved`), the expression was
  fully static at that gate.**
- `create_job` (`instantiate.rs`) already builds the gate-2 symbol
  table — real `Param.*` values plus `Unresolved` `Session.*` /
  `Task.*` / `Env.File.*` entries (`check_symtab`) — today used only to
  type-check `let` bindings.
- Evaluation is deterministic across gates: expressions are side-effect
  free (Expression Language spec § "Static Type Checking via Unresolved
  Values"), and anything host-dependent is `Unresolved` at gates 1–2, so
  a concrete gate-1/2 value equals the gate-3 value.

What is missing is purely plumbing: look at the values gates 1 and 2
already compute and throw away, and apply the Group A constraints (plus
any opted-in Group B caps) to them.

## The lower-bound rule (partially-static strings)

A format string is a sequence of segments — literal text and `{{...}}`
expressions. At any gate, each expression segment evaluates to either a
concrete value or `Unresolved`. Even when the whole string is not
static, we can compute a **lower bound on the length of every string it
can possibly resolve to**:

| Segment kind at gate G | Contribution to lower bound (characters) |
|---|---|
| Literal text | `text.chars().count()` |
| Expression → concrete value | `value.to_display_string().chars().count()` |
| Expression → concrete `null` | 0 (null renders as empty string) |
| Expression → `Unresolved` | 0 (may resolve to the empty string) |

```
min_resolved_string_len(fs, G) = Σ contribution(segment_i, G)
```

If `min_resolved_string_len > limit`, **no** run-time resolution can produce a
conforming value — every possible job creation / task execution is
guaranteed to fail the resolved-value constraint — so the gate fails.
This is strictly early detection of a certain violation, never a new
restriction. It covers:

- Fully-static strings (the bound is exact): Example 1, at gate 1, under
  an opted-in arg cap or a lowered memory budget.
- Param-dependent strings at gate 2: Example 2, once `Param.Count` is
  bound.
- Mixed static/host-context strings at gate 1: Example 3 — the
  `Session.WorkingDirectory` segment contributes 0 to the bound, but the
  static segment alone puts it over the limit
  (`bound ≥ 10,000,001 > cap`), so `check` rejects without ever knowing
  the host value.
- Mixed strings against Group A limits, e.g. a job `name` of 200 literal
  chars around a `{{ Param.X }}`: guaranteed > 128 whatever `Param.X` is →
  fails `check`, where today's gate-2-only enforcement would only catch
  it at submission.

```yaml
args:
  - "{{ Session.WorkingDirectory }}/{{ 'A' * 10000000 }}"  # Example 3: bound ≥ 10,000,001 → fails gate 1 under an arg cap
  - "--frame={{ Task.Param.Frame }}"                       # bound = 8 → passes
```

Units are **characters** (`chars().count()`), matching the spec's limits,
which are all stated in characters. A character count is also a valid
lower bound for any byte-denominated cap (every char is ≥ 1 byte), so
one bound serves both.

### Full-constraint checking when fully static

Length is the only constraint a lower bound can check. When a format
string is **fully** static at a gate (`resolved_value` present), the
complete resolved-value constraint check runs — for
`<AttributeCapabilityValue>` that includes the charset and
first-character rules, not just ≤ 100. The rule: *whatever check gate 3
(or gate 2) would run on the resolved value runs at the earliest gate
where the value is fully known; the length portion of the check runs at
the earliest gate where the lower bound already exceeds the limit.*

### Granularity limitation

The bound is per-*segment*, not per-*subexpression*. A single expression
mixing a huge static part with an unresolved part —
`{{ 'A' * 10000000 + Session.WorkingDirectory }}` — evaluates to
`Unresolved` as a whole and contributes 0 to the bound, even though any
resolution is ≥ 10 MB. Gate 3 (or the memory budget, if lowered) still
catches it. Tightening this would require the evaluator to carry a
min-size annotation on `Unresolved` values through every operator (a
size lattice); noted as possible future work, not part of this design.

## Configuration surface

Group A constraints are spec-mandated: always on, not configurable.

Group B caps join `CallerLimits`, keeping its all-`None` convention
("None means no additional restriction beyond the spec"). **Shipped in
PR #399** — as merged, the evaluation budgets landed as `CallerLimits`
fields too:

```rust
// openjd-model: crate::types (as shipped, PR #399)
pub struct CallerLimits {
    // ... existing fields ...
    /// Maximum character length of any resolved string destined for a
    /// process argument (action `command`, each argv entry an `args`
    /// element produces after null-skip / list-flatten). `None`
    /// (default) imposes no limit beyond the spec (§5.1/§5.2 set no
    /// maximum; the OS imposes its own, in units other than
    /// characters).
    pub max_resolved_arg_len: Option<usize>,
    /// Maximum character length of any resolved embedded-file `data`
    /// value. `None` (default) imposes no limit beyond the spec
    /// (§6.1.2 sets none).
    pub max_resolved_data_len: Option<usize>,
    /// Memory budget, in bytes, per format-string expression
    /// evaluation. `None` uses the spec-recommended default
    /// (`openjd_expr::DEFAULT_MEMORY_LIMIT`, 100 MB).
    pub max_eval_memory_bytes: Option<usize>,
    /// Operation budget per format-string expression evaluation.
    /// `None` uses `openjd_expr::DEFAULT_OPERATION_LIMIT` (10 million).
    pub max_eval_operations: Option<usize>,
}
```

- No separate env-var knob: §4.4.2's 2048 is spec-mandated (Group A).
- The evaluation **memory budget** is the spec's primary lever against
  expression-generated blowups. **Shipped:** `CallerLimits` exposes
  `max_eval_memory_bytes` / `max_eval_operations`, and the sessions
  crate mirrors all four fields in `SessionLimits`
  (`From<&CallerLimits>`), carried on `SessionConfig.limits`. A host
  that sets, say, 1 MB gets gate-1 failure for Example 1 with no
  conformance concern. `create_job` applies the budgets to every
  evaluation it performs as of PR #404.
- The `openjd` CLI default: **shipped** (PR #399) as
  `DEFAULT_MAX_ARG_LEN` = 32 * 1024 characters on every platform — a
  single opinionated cap rather than the per-OS values originally
  floated (see [open question 1](#open-questions-for-review)). The
  library defaults stay `None`.

## Mechanism: openjd-expr API change

**Implemented and merged** (PR #373, `openjd-expr` v0.7.0). The API as
shipped — field names, an extra parameter, and an extra field evolved
from the original draft during review:

```rust
/// Outcome of statically evaluating a format string against a symbol
/// table containing `Unresolved` placeholders.
pub struct StaticResolution {
    /// Lower bound, in characters, on the length of any string this
    /// format string can resolve to under the same `target_type` given
    /// to `validate_expressions`. Exact when `resolved_value` is `Some`.
    /// Accumulation saturates rather than wraps (relevant on 32-bit
    /// targets — saturating is the safe direction for a lower bound).
    pub min_resolved_string_len: usize,
    /// The fully-resolved value, present iff every segment evaluated to
    /// a concrete (non-Unresolved) value AND, in the concatenated
    /// multi-segment case, the result is at most
    /// `MAX_STATIC_RESOLVED_VALUE_LEN` (10 MiB) — a defensive cap, since
    /// the per-segment evaluation memory limit does not compose across
    /// segments. For a single-expression format string this is the typed
    /// value (may be a list or null), coerced toward `target_type`;
    /// otherwise the concatenated string.
    pub resolved_value: Option<ExprValue>,
    /// The static type the resolution will eventually produce, available
    /// even when `resolved_value` is `None` (unresolved placeholders
    /// read through to their constraint; payload-dependent coercions
    /// yield unions).
    pub resolved_type: ExprType,
}

impl FormatString {
    pub fn validate_expressions(
        &self,
        symtab: &SymbolTable,
        lib: &FunctionLibrary,
        target_type: Option<&ExprType>,
    ) -> Result<StaticResolution, FormatStringValidationError>;
}
```

Design points (updated to the shipped behavior):

- **No second evaluation, no retention.** The values are produced by the
  evaluation pass 8 already runs. Segments render once into a single
  buffer; peak memory is one segment's evaluation plus the 10 MiB cap.
- **`target_type` must match resolution.** Validation coerces the
  single-expression passthrough root toward `target_type` exactly as
  `resolve_with` does. Gates must pass the same target type the field's
  resolution will use, or the bound describes the wrong resolution: a
  float literal in an INT-typed field resolves to `1` (1 char), not
  `1.0` (3 chars) — with no target the bound over-estimates, which is
  the false-rejection direction.
- `min_resolved_string_len` uses `to_display_string()` character counts,
  matching how values are actually interpolated
  (`resolve_string_with`). A concrete list segment inside a
  multi-segment string contributes its JSON-array display length,
  mirroring resolution behavior.
- `resolved_value` follows the `resolve_with` typed-passthrough rule:
  typed value for exactly-one-expression-zero-literals (exempt from the
  10 MiB cap — no concatenation occurs), string otherwise. Callers use
  it to apply per-element rules for list results and full charset
  checks for Group A fields. **Gates must not rely on `resolved_value`
  being present for huge static strings** — past the cap it is `None`
  while the bound keeps counting, which is exactly the lower-bound rule
  this design is built on.
- `resolved_type` tells gates whether the field resolves to a string
  (the bound is a true string-length bound) or a typed value whose
  display form the bound merely measures — apply length caps only to
  string-consumed fields. It is available even for fully-unresolved
  strings.
- `openjd-expr` supplies mechanism only; which limit applies to which
  field is `openjd-model` / `openjd-sessions` policy.

## Gate 1 — template validation (openjd-model, pass 8)

**Implemented** (PR #383; the opt-in Group B caps and configurable
budgets landed with PR #399).

`validate_fs` (in `template/validate_v2023_09/format_strings.rs`) gains
an optional resolved-value constraint parameter supplied by each call
site (which knows the field):

| Field | Constraint applied to `StaticResolution` |
|---|---|
| job `name` | bound vs `max_job_name_len` (128/512); full JobName check (Cc chars) when fully static |
| `hostRequirements` attribute `anyOf`/`allOf` | bound vs 100; full §3.3.2.2 check when fully static |
| task parameter STRING/PATH range elements | bound vs 1024 |
| environment `variables` values | bound vs 2048 |
| action `command`, `args[*]` | bound vs `caller_limits.max_resolved_arg_len` if set |
| embedded file `data` | bound vs `caller_limits.max_resolved_data_len` if set |
| everything else | none (already constrained elsewhere or non-string) |

Each call site must pass the field's **target type** to
`validate_expressions` (e.g. `nulltype | string | list[string]` for an
`args` element), so that the bound describes the coerced resolution the
field actually performs — see the Mechanism section. `resolved_type`
then tells the gate whether the field resolves to a string (apply the
length constraint) or a typed list whose display form the bound merely
measures (apply per-element rules to `resolved_value` when present,
skip the whole-string length check otherwise).

- Errors are normal `ValidationErrors` entries at the field path, e.g.
  `steps[0] -> script -> actions -> onRun -> args[0]: resolved value is
  at least 10000000 characters, which exceeds the maximum of 131072.`
- Requires `caller_limits` to be visible to pass 8 (today
  `ValidationContext` carries it for `create_job`'s task-count check;
  pass 8 reads the same source).
- `validate_let_bindings` needs no check of its own: a huge `let` value
  only matters if a constrained field interpolates it, and then that
  field's own bound catches it (the binding's concrete value is in the
  symtab, so the referencing segment evaluates concrete).
- **Follow-up (from PR [#383](https://github.com/OpenJobDescription/openjd-rs/pull/383)
  review): single-valued `allOf` literal count.** **Resolved** — `structure.rs`
  now gates on `vals.iter().filter(|v| v.is_literal()).count() > 1`.
  The single-valued
  attribute check in `structure.rs` fired only when
  `vals.iter().all(|v| v.is_literal())`, which is stricter than
  soundness requires. A literal element always contributes exactly one
  element to the resolved list — only expression elements can null-skip
  or list-flatten (`resolve_string_list` semantics) — so the resolved
  count is at least the number of literal elements. An `allOf` of
  `["linux", "windows", "{{ Param.X }}"]` on `attr.worker.os.family`
  therefore violates the single-valued rule under *every* possible
  resolution, but passed `openjd check` and only failed at gate 2
  (the `instantiate.rs` re-check) — exactly the deferral this design
  eliminates elsewhere. Fix: gate on
  `vals.iter().filter(|v| v.is_literal()).count() > 1` instead of
  all-literal; identical to the current condition for all-literal
  lists, so no existing behavior changes.

## Gate 2 — job creation (openjd-model, create_job)

**Implemented** (PR [#404](https://github.com/OpenJobDescription/openjd-rs/pull/404);
the job-name follow-up below shipped earlier with #383/#397). The
authoritative spec is the "Resolved-value checks on carried-forward
fields" section of `specs/model/job-creation.md`; note that the merged
code and specs use the spec's stage names throughout — "template
validation", "job creation", "task execution" — not this document's
gate numbering.

`create_job` already resolves and checks the Group A template-scope
fields (job name, attribute values, task param strings) — unchanged
except for the job-name follow-up below. PR #404 added one pass over
the carried-forward session/task-scope format
strings — action `command`/`args`, environment `variables`,
embedded-file `data` — evaluating each against the gate-2 symbol table
(real `Param.*`/`RawParam.*`; `Unresolved` `Session.*` / `Task.*` /
`Env.File.*`; concrete `Job.Name` / `Step.Name`) and applying the same
table as gate 1.

- The symbol-table construction extends `instantiate.rs`'s existing
  `check_symtab` scaffolding (as shipped:
  `build_task_check_symtab` / `build_env_check_symtab`); environment
  script `let` bindings are evaluated into the table, and a binding
  that fails with the real parameter values fails job creation (a
  deterministic per-session failure caught early).
- Failures are `ModelError::ModelValidation` with the field path,
  consistent with the resolved-value re-checks `create_job` already
  performs. Violations accumulate within one scope (a step script, one
  environment); the first failing scope stops instantiation.
- **Error policy (decided during implementation):** the pass reports
  resolved-value violations and budget exceedances only. Other
  evaluation/parse errors are skipped, because `create_job` may
  deliberately run under a different profile than decode (an existing,
  tested contract) — a budget exceedance is reported since run time
  evaluates the same expression with strictly more symbols bound.
- Cost note: this adds work proportional to what one worker would do
  anyway — done once at submission instead of per-task-per-worker.
  Avoidable later with constant folding (out of scope, below).
- The evaluation budgets (`max_eval_memory_bytes` /
  `max_eval_operations`) now bound **every** evaluation job creation
  performs — job name, `let` bindings, host requirements, task ranges,
  and this pass (also PR #404).
- **Follow-up (from PR [#383](https://github.com/OpenJobDescription/openjd-rs/pull/383)
  review): job-name control characters.** **Resolved** — `create_job`
  now rejects a resolved job name containing control characters,
  alongside the emptiness check. §1.1.1 constrains the
  *resolved* job name to no Cc characters, but gate 2 only re-applied
  the length and emptiness checks. Gate 1's
  `ResolvedConstraint::Text { forbid_control_chars: true }` runs only
  when the name is fully static, so an interpolated name such as
  `name: "render-{{ Param.Suffix }}"` with `Suffix = "a\nb"` passed
  both gates (`min_resolved_string_len` contributes 0 for the
  unresolved segment and `resolved_value` is `None`). This is the same
  asymmetry the emptiness check already fixes at gate 2 — `create_job`
  additionally rejects a resolved job name containing control
  characters (`job_name.chars().any(char::is_control)`), with a
  `ModelError::DecodeValidation` alongside the existing emptiness
  check.

## Gate 3 — run time (openjd-sessions)

**Implemented** (PR #383 for the always-on §4.4.2 check; PR #399 for
the `SessionLimits` caps and budgets on `SessionConfig.limits`).

The enforcement boundary. After format-string resolution produces final
strings:

- `resolve_action_args` (`runner/mod.rs`): resolved `command` and each
  final argv entry (after null-skip and list-flatten) vs
  `max_resolved_arg_len` if set.
- Environment-variable resolution (`session.rs`): each resolved value vs
  **2048** (§4.4.2, always on).
- Embedded-file materialization (`embedded_files.rs`): each resolved
  `data` vs `max_resolved_data_len` if set.

Failures are `SessionError::FormatString { context, reason }` (as
shipped — no dedicated variant was added), following the existing
`notifyPeriodInSeconds ≤ 600` precedent. The sessions configuration
surface gains the two optional caps and the memory-budget override,
mirroring whatever the submitting service set.

## Conformance considerations

- Group A enforcement is pure early detection of spec-mandated
  constraints — it can only fail templates whose every possible
  resolution already violates the spec. **Risk: none in principle**, but
  the 1,038-test conformance suite must pass before merge; any suite
  template that newly fails indicates either a bug in the bound or a
  genuinely-invalid suite fixture (report upstream).
- The §4.4.2 resolved-value reading and the env-value gap fix should be
  cross-checked against `openjd-model-for-python` behavior (it likely
  checks the raw text via Pydantic `max_length`; if so, raw-vs-resolved
  divergence is worth an upstream clarification issue either way).
- Group B caps are `None` by default, so default behavior is
  spec-conforming. Examples 1–3 fail early only when a caller
  opts into an arg cap **or** lowers the (spec-sanctioned, configurable)
  evaluation memory budget.

## Explicitly out of scope

- **Constant folding** — caching gate-1/gate-2 concrete values so later
  stages never re-evaluate. Changes `FormatString` from an immutable
  parsed template into a carrier of resolved state
  (serialization/equality/`resolvedSymTab` implications). Independent
  follow-up.
- **Sub-expression lower bounds** — see
  [Granularity limitation](#granularity-limitation).
- **Argv element-count / total-size (`ARG_MAX`-style) budgets** — can be
  added later inside the same plumbing if needed.
- **Raw-text `command` 1024 cap** — pre-existing, Python-compat,
  unchanged here.

## Open questions for review

All three are now decided:

1. **CLI policy for Group B.** ~~Should the `openjd` CLI set
   `max_resolved_arg_len` by default (e.g. 128 KiB, matching Linux
   `MAX_ARG_STRLEN`), while the library default stays `None`? The
   spec-conformance risk sits with `check` conformance tests running
   through the CLI — the suite must pass with whatever default is
   chosen.~~ **Decided and shipped (PR #399):** the CLI sets an
   opinionated default of **32 * 1024 characters on every platform**
   (`DEFAULT_MAX_ARG_LEN` in `openjd-cli`). Per-OS values were
   considered and rejected because the OS limits are measured in
   different units (Linux `MAX_ARG_STRLEN` is bytes, the Windows
   command line is UTF-16 code units), so no character count maps
   exactly; 32K characters is at most 128 KiB of UTF-8 (within Linux's
   per-string limit) and approximately the Windows command-line
   capacity. The library default stays `None`. The conformance suite
   passes on every platform with this default.
2. **§4.4.2 reading.** ~~Resolved-value (proposed) vs raw-text
   interpretation of the 2048-char env value limit; verify Python
   behavior and consider an upstream clarification issue.~~
   **Verified:** `openjd-model-for-python` checks the **raw template
   text** — `EnvironmentVariableValueString(FormatString)` sets
   `_max_length = 2048`, applied by `DynamicConstrainedStr._validate`
   at Pydantic parse time, before resolution — and
   `openjd-sessions-for-python` never re-checks the resolved value.
   The divergence is real in both directions: Python rejects a
   3000-char template value that resolves under 2048 (this
   implementation accepts it) and accepts a short expression that
   resolves far past 2048 (this implementation rejects it at gate 3).
   This design keeps the resolved-value reading; an upstream
   clarification issue should be filed (**still to do**).
3. **Memory-budget plumbing.** ~~Expose the evaluation memory/op budgets
   through model `ValidationContext`/`CallerLimits` and the sessions
   config surface in this change, or as a separate change? (This design
   assumes yes, in this change — it is the spec's own lever for
   Examples 1 and 3.)~~ **Decided and shipped (PR #399):** the budgets
   are `CallerLimits` fields (`max_eval_memory_bytes`,
   `max_eval_operations`) applied throughout template validation, and
   mirrored into the sessions surface as `SessionLimits` on
   `SessionConfig.limits`. The last gap — `create_job` evaluating under
   the spec defaults — closed with PR #404.

## Follow-up spec edits

To be made alongside the implementation commits (spec/code co-evolution):

| File | Change |
|---|---|
| `specs/expr/format-string.md` | ~~§ Validation: `validate_expressions` returns `StaticResolution`; document the lower-bound computation~~ **Done** (PR #373) — including the target-type rule, the resolved-value cap, and the saturation note |
| `specs/expr/public-api.md` | ~~New `StaticResolution` type; `validate_expressions` signature~~ **Done** (PR #373) |
| `specs/model/validation.md` | ~~Pass 8: resolved-value lower-bound checks, per-field constraint table~~ **Done** (PR #383; opt-in cap rows and budget paragraphs added with the Group B caps) |
| `specs/model/job-creation.md` | ~~`create_job`: new gate-2 evaluation pass over carried-forward format strings~~ **Done** (PR #404 — "Resolved-value checks on carried-forward fields" section, plus corrected `create_job` / `evaluate_let_bindings` signatures) |
| `specs/model/public-api.md` | ~~`CallerLimits` new fields; memory-budget plumbing~~ **Done** (with the Group B caps; also `decode_environment_template` now carries `CallerLimits`) |
| `specs/sessions/runners.md` | ~~`resolve_action_args` length enforcement~~ **Done** (with the Group B caps) |
| `specs/sessions/session.md` / `embedded-files.md` | ~~Env-var 2048 and embedded-file data enforcement~~ **Done** (env-var 2048 with PR #383's gate 3; `SessionConfig.limits` + `EmbeddedFiles::with_limits` with the Group B caps) |
| `specs/sessions/public-api.md` | ~~Configuration surface for caps + budgets~~ **Done** (`SessionLimits`, `SessionConfig.limits`) — plus `specs/cli/check.md`/`run.md` documenting the CLI's uniform 32K-character `max_resolved_arg_len` default |
| `specs/architecture.md` | Pointer to this document |

## Follow-ups

Recorded after PR #404 (which completed gate 2). Items 1–2 are
leftovers from earlier rounds; items 3–8 come from the
[PR #404 review](https://github.com/OpenJobDescription/openjd-rs/pull/404)
(approved with findings recorded rather than requested — none is a
regression, since before #404 job creation applied no resolved-value
checks and no budgets at all). None blocks the design; each is an
independent piece of work.

1. **File the §4.4.2 upstream clarification issue.** Open question 2
   verified the raw-text vs resolved-value divergence against
   `openjd-model-for-python` in both directions; the issue against
   [openjd-specifications](https://github.com/OpenJobDescription/openjd-specifications)
   has not been filed yet.
2. **`specs/architecture.md` pointer to this document** — the one
   remaining row in the spec-edits table above. Natural to do when this
   design doc itself lands on `main`.
3. **Budget exceedance silently dropped inside unresolved-test
   conditionals** (review finding, measured for both budget kinds; the
   most impactful item here). When an `if`/`else` test is unresolved
   (`Session.*` — idiomatic, not contrived), the evaluator runs both
   branches and wraps a dual failure in a compound error whose
   top-level kind is `Other`; the gate-2 budget detection only inspects
   the top-level kind, so the one error class that stage may report is
   suppressed. `{{ 'A' * int(Param.N) if Session.HasPathMappingRules
   else 'B' }}` with huge `N` is accepted at `create_job` under a
   lowered budget — meaning a caller who lowered
   `max_eval_memory_bytes` to protect the submitting process gets
   evaluation against the 100 MB default instead. Fix: check
   budget kinds recursively through `sub_errors()`. The related (and
   opposite-direction) coarseness — both branches *charged* against
   one budget — is documented in `specs/model/job-creation.md`; once
   the suppression is fixed, document both directions together.
4. **Desugar divergence between gates 1 and 2** (review finding,
   measured). Gate 2 checks the *desugared* SimpleAction script
   (`bash:`/`python:`/…, FEATURE_BUNDLE_1), but pass 8 only validates
   `step.script` — it never validates the synthesized
   `command`/`args`/`data`. So for SimpleAction steps the gate-2 check
   is brand-new rather than a re-check: a 500-char `bash:` body under
   `max_resolved_data_len: 100` passes `check` and fails `create_job`,
   and the error path (`steps[0] -> script -> embeddedFiles[0] ->
   data`) names a node the author never wrote. Preferred fix: extend
   pass 8 to validate the desugared script so the stages agree; also
   consider mapping the synthetic path back to the sugar field. Until
   then the "re-run exactly the checks pass 8 applies" claim does not
   hold for SimpleAction steps.
5. **Environment `let` policy asymmetry and parse-profile mismatch**
   (review finding). Two parts:
   - An environment `let` that errors under the job-creation context
     hard-fails `create_job`, while a format string with the identical
     error is deliberately skipped (`report_eval_errors = false`) two
     functions away. The behavior change is disclosed, but the
     `report_eval_errors` doc block should state why `let` is exempt
     from its reasoning — or the `let` errors should be collected and
     filtered the same way.
   - Unambiguous and cheap to fix regardless: the two check-symtab
     builders disagree on parse profile. `evaluate_let_bindings` (used
     for environments) parses with `ParsedExpression::new` — the
     latest profile, every extension — while `build_task_check_symtab`
     parses with the caller's host profile. An env `let` using syntax
     the caller's profile does not enable parses at gate 2 but is
     refused at pass 8.
6. **Whole-template error aggregation at gate 2** (review finding;
   was already noted here pre-review). Three separate
   `ValidationErrors` collections each abort at their own
   `into_result`, inside the per-step closure — one step-script
   violation masks that step's environments, every later step, and all
   `jobEnvironments`. Pass 8 reports everything at once. Mechanical
   fix: thread one `ValidationErrors` through `instantiate_step` and
   the `jobEnvironments` loop, `into_result` once.
7. **Silent-skip observability** (review suggestion). With
   `report_eval_errors = false`, nothing distinguishes "field checked
   and passed" from "field errored and was skipped" — the budget
   suppression above is one reachable route into that state, and
   `add_unresolved_session_symbols` discarding `symtab.set` failures
   (`let _ =`) is another (a failed seed degrades the check to a
   no-op). Tests should assert fields were actually *evaluated*, not
   merely that `create_job` returned `Ok`; and the `set` results
   should not be discarded silently. Related unverified note from the
   review: confirm `CHUNK_INT` binding as `Unresolved(RANGE_EXPR)` in
   the check symtab matches what the session binds at run time — a
   mismatch would surface as a type error and be swallowed.
8. **Re-check deferred numeric constraints at gate 2.** Action
   `timeout`, `notifyPeriodInSeconds`, and the deferred cancelation
   `mode` validate at gate 1 against the template-scope symtab (params
   unresolved) and resolve on the worker. Gate 2 does not re-evaluate
   them with the bound parameters, so `timeout: "{{ Param.T }}"`
   submitted with `T = 0` passes `create_job` and fails only at run
   time. The gate-2 pass has all the machinery to close this — extend
   its constraint table to the `Int`/`CancelationMode` constraints with
   the gate-2 symtab.
9. **TODO: tighten the `create_job` profile contract — leniency
   justified by "the caller might strip EXPR" is a bug.** `create_job`
   currently documents that passing a `ValidationContext` whose
   extensions differ from the ones the template was decoded with is
   supported "application-level policy" (introduced in `848f92a`,
   pinned by `test_model_profile`'s strip-EXPR tests). That contract is
   what forces gate 2's lenient error policy
   (`report_eval_errors = false`): an evaluation error must be treated
   as a possible context artifact, so real errors are skipped — which
   in turn created the budget-kind special-casing, the `let`-binding
   policy exemption, and the silent-skip observability class in item 7.
   No production caller diverges (the CLI derives its profile from the
   template's own declared extensions), and the middle ground is
   incoherent: an application that does not support an extension
   already rejects the template at decode via `supported_extensions`.
   Fix: require the gate-2 context's extensions to match decode's
   (documented as unspecified behavior at minimum; better, enforce with
   an error), then make the gate-2 error policy uniformly strict —
   every surfaced evaluation error is either a defect pass 8 missed or
   a deterministic value-dependent run-time failure, so report them
   all. That deletes the leniency machinery and the exemption
   paragraphs outright and closes item 7's silent-skip gap by
   construction. Rework or remove the strip-EXPR pinning tests
   accordingly.
