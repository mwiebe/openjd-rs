# Resolved-Value Constraints and Static Evaluation Gates

**Status: DRAFT — under review. No code implements this yet.**

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

All three of the following templates pass `openjd check` and job
creation today, then degrade the worker host at run time. Each is
resolvable — and therefore rejectable — earlier than that.

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

Template validation (pass 8) in fact **does** evaluate it —
`validate_fs` → `FormatString::validate_expressions` computes the entire
10 MB string at `check` time — but then discards the value and reports
success.

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
| [§1.1.1](https://github.com/OpenJobDescription/openjd-specifications/blob/mainline/wiki/2023-09-Template-Schemas.md#111-jobname) | `<JobName>` | ≤ 128 chars (≤ 512 with FEATURE_BUNDLE_1); no Cc chars | Gate 2 (resolved name vs `max_job_name_len`) |
| [§3.3.2.2](https://github.com/OpenJobDescription/openjd-specifications/blob/mainline/wiki/2023-09-Template-Schemas.md#3322-attributecapabilityvalue) | `<AttributeCapabilityValue>` | ≤ 100 chars; latin alphanumeric + `_` + `-`; must start with letter or `_` | Gate 2 (`validate_attribute_capability_value` on resolved value) |
| [§3.4.2](https://github.com/OpenJobDescription/openjd-specifications/blob/mainline/wiki/2023-09-Template-Schemas.md#342-taskparameterstringvalue) | `<TaskParameterStringValue>` | ≤ 1024 chars | Gate 2 (`ranges.rs` on resolved range elements) |
| [§4.4.2](https://github.com/OpenJobDescription/openjd-specifications/blob/mainline/wiki/2023-09-Template-Schemas.md#442-environmentvariablevaluestring) | `<EnvironmentVariableValueString>` | ≤ 2048 chars (see [reading note](#reading-note-442)) | **Nowhere — pre-existing gap** |

<a name="reading-note-442"></a>**Reading note on §4.4.2:** the section
says "A string value subject to … Maximum length: 2048 characters"
without the "after resolved" phrase, but the map value it constrains is
annotated `@fmtstring[host]` (§4.4) — the constraint describes the
environment variable's value, which only exists after resolution. This
design adopts the resolved-value reading: enforce 2048 on the resolved
value at gate 3, with lower-bound early failure at gates 1–2. A raw-text
check would be incorrect in both directions (a 3000-char template can
resolve under 2048; a 100-char template can resolve over it).
**Action item:** check what `openjd-model-for-python` does here and file
an upstream clarification issue.

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
| model decode | env var value | **none** | gap vs §4.4.2, fixed by this design |
| model decode | any format string | ≤ 1 MB text, ≤ 1000 segments (`MAX_FORMAT_STRING_LEN`, expr defensive caps) | implementation defense-in-depth |
| model decode | expression evaluation | 100 MB memory / 10 M op budgets | Expression Language spec, defaults |
| sessions | resolved `notifyPeriodInSeconds` | ≤ 600 | §5.3.2, resolved-value check (the precedent this design generalizes) |
| sessions | resolved command/args/env values/data | **none** | — |

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
min_resolved_len(fs, G) = Σ contribution(segment_i, G)
```

If `min_resolved_len > limit`, **no** run-time resolution can produce a
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
string is **fully** static at a gate (`static_value` present), the
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
("None means no additional restriction beyond the spec"):

```rust
// openjd-model: crate::types
pub struct CallerLimits {
    // ... existing fields ...
    /// Maximum character length of any resolved string destined for a
    /// process argument (action `command`, each element of `args` after
    /// null-skip / list-flatten). `None` (default) imposes no limit
    /// beyond the spec. Spec §5.1/§5.2 set no maximum but note the OS
    /// imposes one; callers targeting Linux may want 131072
    /// (MAX_ARG_STRLEN), Windows 32767 (command-line limit).
    pub max_resolved_arg_len: Option<usize>,
    /// Maximum character length of any resolved embedded-file `data`
    /// value. `None` (default) imposes no limit beyond the spec (§6.1.2
    /// sets none).
    pub max_resolved_data_len: Option<usize>,
}
```

- No separate env-var knob: §4.4.2's 2048 is spec-mandated (Group A).
- The evaluation **memory budget** is the spec's primary lever against
  expression-generated blowups and is already configurable in
  `openjd-expr`. The model/sessions configuration surfaces should expose
  it (today the defaults are hard-wired at the call sites); a host that
  sets, say, 1 MB gets gate-1 failure for Example 1 with no
  conformance concern. This design makes that plumbing part of the work.
- Whether the `openjd` CLI sets an opinionated default for
  `max_resolved_arg_len` (e.g. 128 KiB) is a CLI policy decision, listed
  as an open question. The library defaults stay `None`.

## Mechanism: openjd-expr API change

`FormatString::validate_expressions` currently evaluates every segment
and returns `Result<(), FormatStringValidationError>`, discarding the
values. It changes to return what it already computed:

```rust
/// Outcome of statically evaluating a format string against a symbol
/// table containing `Unresolved` placeholders.
pub struct StaticResolution {
    /// Lower bound, in characters, on the length of any string this
    /// format string can resolve to. Exact when `static_value` is `Some`.
    pub min_resolved_len: usize,
    /// The fully-resolved value, present iff every segment evaluated to
    /// a concrete (non-Unresolved) value. For a single-expression format
    /// string this is the typed value (may be a list or null); otherwise
    /// the concatenated string.
    pub static_value: Option<ExprValue>,
}

impl FormatString {
    pub fn validate_expressions(
        &self,
        symtab: &SymbolTable,
        lib: &FunctionLibrary,
    ) -> Result<StaticResolution, FormatStringValidationError>;
}
```

Design points:

- **No second evaluation.** The values are produced by the evaluation
  pass 8 already runs; this only stops throwing them away. Gate-1 cost
  is unchanged (the 10 MB example is *already* materialized during
  `check` today).
- `min_resolved_len` uses `to_display_string()` character counts,
  matching how values are actually interpolated
  (`resolve_string_with`). A concrete list segment inside a
  multi-segment string contributes its JSON-array display length,
  mirroring resolution behavior.
- `static_value` follows the `resolve_with` typed-passthrough rule:
  typed value for exactly-one-expression-zero-literals, string
  otherwise. Callers use it to apply per-element rules for list results
  and full charset checks for Group A fields.
- `openjd-expr` supplies mechanism only; which limit applies to which
  field is `openjd-model` / `openjd-sessions` policy.

## Gate 1 — template validation (openjd-model, pass 8)

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

## Gate 2 — job creation (openjd-model, create_job)

`create_job` already resolves and checks the Group A template-scope
fields (job name, attribute values, task param strings) — unchanged. It
gains one pass over the carried-forward session/task-scope format
strings — action `command`/`args`, environment `variables`,
embedded-file `data` — evaluating each against the gate-2 symbol table
(real `Param.*`/`RawParam.*`; `Unresolved` `Session.*` / `Task.*` /
`Env.File.*`; concrete `Job.Name` / `Step.Name`) and applying the same
table as gate 1.

- The symbol-table construction already exists in `instantiate.rs`
  (`check_symtab`); the pass extends its use rather than building new
  scaffolding.
- Failures are `ModelError` with the field path, consistent with the
  resolved-value re-checks `create_job` already performs.
- Cost note: gate 2 currently does *not* evaluate these fields, so this
  adds work proportional to what one worker would do anyway — done once
  at submission instead of per-task-per-worker. Avoidable later with
  constant folding (out of scope, below).

## Gate 3 — run time (openjd-sessions)

The enforcement boundary. After format-string resolution produces final
strings:

- `resolve_action_args` (`runner/mod.rs`): resolved `command` and each
  final argv entry (after null-skip and list-flatten) vs
  `max_resolved_arg_len` if set.
- Environment-variable resolution (`session.rs`): each resolved value vs
  **2048** (§4.4.2, always on).
- Embedded-file materialization (`embedded_files.rs`): each resolved
  `data` vs `max_resolved_data_len` if set.

Failures are `SessionError::FormatString { context, reason }` (or a
dedicated variant — implementer's choice), following the existing
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

1. **CLI policy for Group B.** Should the `openjd` CLI set
   `max_resolved_arg_len` by default (e.g. 128 KiB, matching Linux
   `MAX_ARG_STRLEN`), while the library default stays `None`? The
   spec-conformance risk sits with `check` conformance tests running
   through the CLI — the suite must pass with whatever default is
   chosen.
2. **§4.4.2 reading.** Resolved-value (proposed) vs raw-text
   interpretation of the 2048-char env value limit; verify Python
   behavior and consider an upstream clarification issue.
3. **Memory-budget plumbing.** Expose the evaluation memory/op budgets
   through model `ValidationContext`/`CallerLimits` and the sessions
   config surface in this change, or as a separate change? (This design
   assumes yes, in this change — it is the spec's own lever for
   Examples 1 and 3.)

## Follow-up spec edits

To be made alongside the implementation commits (spec/code co-evolution):

| File | Change |
|---|---|
| `specs/expr/format-string.md` | § Validation: `validate_expressions` returns `StaticResolution`; document the lower-bound computation |
| `specs/expr/public-api.md` | New `StaticResolution` type; `validate_expressions` signature |
| `specs/model/validation.md` | Pass 8: resolved-value lower-bound checks, per-field constraint table |
| `specs/model/job-creation.md` | `create_job`: new gate-2 evaluation pass over carried-forward format strings |
| `specs/model/public-api.md` | `CallerLimits` new fields; memory-budget plumbing |
| `specs/sessions/runners.md` | `resolve_action_args` length enforcement |
| `specs/sessions/session.md` / `embedded-files.md` | Env-var 2048 and embedded-file data enforcement |
| `specs/sessions/public-api.md` | Configuration surface for caps + budgets |
| `specs/architecture.md` | Pointer to this document |
