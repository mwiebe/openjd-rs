# CONTROL_FLOW prototype plan for openjd-rs

Status: plan, not yet implemented. Target branch: `control-flow-prototype`.

This plan maps the draft `CONTROL_FLOW` extension onto the openjd-rs crates. Its inputs are on branch
`rfc-control-flow` of openjd-specifications:

* `rfcs/0010-control-flow.md`, the draft RFC (called "the RFC" below; `§` references are to its sections);
* `docs/control-flow-investigation/04-design-space.md`, the v1 scope;
* `docs/control-flow-investigation/03-runtime-semantics.md`, the runtime notes. Its §8 maps emission onto the
  `action_filter.rs` stdout pipeline. An earlier RFC draft replaced that with a session-supplied output file (04 T1),
  and the current draft returns to a stdout directive, `openjd_output_var:`, with an `@<file>` form, `collect` modes,
  and metrics as output variables (04 §8, RFC Design Choices 2–4). This plan follows the current RFC, at
  openjd-specifications commit `d9634a5`.

The goal is a prototype that runs the six RFC sample templates end to end with `openjd run`. The prototype also
tests whether the RFC can be implemented as written, and the questions it raises are listed in
[RFC ambiguities surfaced](#rfc-ambiguities-surfaced).

Every PR below goes through the `implement-change` skill. Each PR updates the crate specs in `specs/` and
`public-api.md` in the same change, as `AGENTS.md` requires.

## Summary

* **openjd-model** does most of the work. It gets the schema fields (including `collect` and `unit` on outputs), the
  extension gate, the `Steps.*` scope in template validation, an output directive parser and collector that sessions
  and schedulers can share, and a binding API. With that API,
  `create_job` can leave a Step unbound and a caller can bind it later from upstream values. It also gets
  `jobOutputDefinitions` and a function that evaluates one job output, and `PATH` output types with the
  `RawOutput` and `TaskRawParam` symbols.
* **openjd-expr** needs no new types or functions in the language. `ExprValue::Unresolved` already makes a value
  "unresolved until run time", and `accessed_symbols` already finds the references. Its `path_mapping` module gains a
  reverse-mapping function for `PATH` outputs (E1). A per-segment static-type API for the `null` warning is deferred
  along with the warning (see A1).
* **openjd-sessions** recognizes `openjd_output_var:` in the action filter, reads `@` files as the job user as each
  directive arrives, feeds each value to the model's collector, reports it through the action callback, and returns
  the collected values in `ActionResult`, with `PATH` values mapped back to the submission path context. Its four
  silent line-truncation sites must start marking truncation.
* **openjd-cli** `openjd run` already runs Steps in topological order in one Session. It becomes a scheduler
  simulator: it binds each pending Step before running it, skips disabled Steps and Environments, and keeps outputs in
  memory. It prints each Task's outputs with their units, and each job output as soon as it is evaluated.
  `openjd summary` reports pending Steps and lists the job outputs.
* **openjd-snapshots** needs no change. It is the job-attachments crate and has no dependency on the model. "Snapshot
  test" in this plan means a golden-file test of CLI output.

## Current state, for reference

Each of these was checked against the code at `be266ed` or by running the release CLI.

| Fact | Where |
|---|---|
| Extensions are a closed enum; the CLI enables all of `ModelExtension::ALL` by default | `crates/openjd-model/src/types.rs:356`, `crates/openjd-cli/src/common.rs:168` |
| `CONTROL_FLOW` is rejected today: `Unsupported extension names: CONTROL_FLOW` | `openjd check` probe |
| `WRAP_ACTIONS` is the model for an extension that requires `EXPR` | `crates/openjd-model/src/template/validate_v2023_09/wrap_actions.rs:86` |
| Validation is a fixed sequence of passes 5–10 | `crates/openjd-model/src/template/validate_v2023_09/mod.rs:180` |
| Validation symbol tables seed `ExprValue::unresolved(T)` per scope | `format_strings.rs:40`, `:75`, `:112`, `:169` |
| `create_job` instantiates every Step eagerly, in template order | `crates/openjd-model/src/job/create_job/mod.rs:265` |
| `instantiate_step` evaluates step `let`, host requirements, and ranges, then serializes a filtered symbol table into `job::Step.resolved_symtab` | `crates/openjd-model/src/job/create_job/instantiate.rs:37` |
| Computed list ranges are capped at 1024 elements; `IntRangeExpr` is not | `crates/openjd-model/src/job/create_job/ranges.rs:336`, `:475`, `:556`; `validate_v2023_09/mod.rs:41` |
| An expression range that evaluates to `[]` is already accepted with `EXPR` alone (0 Tasks); an empty range associated with a non-empty one is already an error | CLI probe: `range: "{{Param.L}}"` with `L=[]` and `range: "{{[]}}"` run 0 chunks; `(A, B)` gives "all members must have the same number of values, got 0 and 2" |
| `"1-{{N}}"` with `N=0` is rejected: `Descending range 1-0 requires a negative step` | `crates/openjd-expr/src/range_expr.rs:829`, CLI probe |
| `CallerLimits::max_task_count` is a total over all Steps, checked once in `create_job` | `crates/openjd-model/src/job/create_job/mod.rs:287` |
| Forward path mapping lives in `openjd-expr` (`PathMappingRule::apply_with_format`, `apply_rules_with_format`); the Session sorts its rules by decreasing `source_path` length and re-maps `PATH` `Param.*` values in `build_symbol_table` | `crates/openjd-expr/src/path_mapping.rs:55`, `:209`; `crates/openjd-sessions/src/session.rs:681`, `:2481` |
| Per-Task iteration with chunking disabled already exists | `StepParameterSpaceIterator::new_with_chunk_override`, `crates/openjd-model/src/job/step_param_space.rs:1298` |
| JSON to typed job-parameter values, with element-wise coercion | `coerce_json_to_job_parameter_type`, `crates/openjd-model/src/job/create_job/parameters.rs:1325` |
| Job-parameter constraint checks (`minValue`, `maxLength`, `item`, ...) | `JobParameterDefinition::check_constraints`, `crates/openjd-model/src/template/parameters.rs:496` |
| Symbol references of a format string or expression | `FormatString::accessed_symbols` (`crates/openjd-expr/src/format_string.rs:578`), `ParsedExpression::accessed_symbols` (`crates/openjd-expr/src/eval/parse.rs:35`) |
| `run_task` takes a `job::StepScript`, not a `job::Step`, and returns `ActionResult { state, exit_code, stdout }` | `crates/openjd-sessions/src/session.rs:1758`, `crates/openjd-sessions/src/action.rs:74` |
| Per-action environment variables are computed in `run_task` | `crates/openjd-sessions/src/session.rs:1785`, `evaluate_env_vars` at `:2393` |
| Directives are recognized by `parse_directive` (prefix `openjd_`, then `<kind>: `) and turned into `ActionMessage`s by `process_line`, on the line-reader side; the Session applies them in `apply_message`. A malformed `openjd_env` sends `CancelMarkFailed` | `crates/openjd-sessions/src/action_filter.rs:45`, `crates/openjd-sessions/src/subprocess.rs:1219`, `crates/openjd-sessions/src/session.rs:2286` |
| A recognized directive line is logged by default; `echo_openjd_directives` (default `true`) hides it when `false` | `crates/openjd-sessions/src/action_filter.rs:211`, the `SessionConfig` doc comment at `crates/openjd-sessions/src/session.rs:141`, and the CLI's setting at `crates/openjd-cli/src/run/execution.rs:289` |
| Lines are silently truncated at 64 KiB in four places: `truncate_line` on the direct path, and in the helper `LineFramer::accumulate` (which drops the rest of an over-long line before it is decoded), `decode_and_truncate`, and `fit_out_payload` (which can also cut a shorter line that grows when escaped) | `crates/openjd-sessions/src/subprocess.rs:62`, `crates/openjd-sessions/src/helper/src/framer.rs:195`, `:89`, `:116` |
| Invalid UTF-8 on stdout is decoded with backslash escapes, not rejected | `decode_backslashreplace`, `crates/openjd-sessions/src/subprocess.rs:87` |
| Under cross-user execution the embedded helper runs the action as the job user and streams `Response::Out` lines; its commands are `Run`, `Cancel`, and `Shutdown` | `crates/openjd-sessions/src/helper/src/protocol.rs:49`, `:69` |
| `openjd run` runs every Step in topological order in one Session, and stops running Tasks after the first failure | `crates/openjd-cli/src/run/execution.rs:202`, `:341`, `:378` |
| No crate and no OpenJD spec has a validation-warning channel | `grep -ri warn` over `openjd-model/src` and `openjd-cli/src`, and `grep -i warning` over the 2023-09 schema and EXPR wiki pages, find nothing |
| `repr_sh` has no signature for nested lists (`list[list[T]]`); `repr_py`, `repr_json`, `string`, and `flatten` do | `openjd check` probes |

## openjd-model

### M1. Extension and schema

* Add `ModelExtension::ControlFlow` (`"CONTROL_FLOW"`) to the enum, `ALL`, `as_str`, and `FromStr`
  (`types.rs:356`). Because the CLI takes `ALL` as its default, this also turns it on in `openjd check`, `run`, and
  `summary`.
* `template::JobTemplate` gets `job_output_definitions: Option<Vec<JobOutputDefinition>>`. A `JobOutputDefinition`
  reuses the `OutputVariableDefinition` wrapper, minus `collect` and plus `value: FormatString`. `job::Job` gets
  `outputs: Option<Vec<job::JobOutput>>`, each a `job::OutputVariable`, the unevaluated `value`, and
  `evaluated: Option<JobOutputValue>`, where `enum JobOutputValue { Value(ExprValue), Null }` and `None` means
  pending, with the same serde defaults as the other new fields. `create_job` fills `evaluated` for the outputs it evaluates; callers fill the rest
  from `evaluate_job_output`.
* `template::StepTemplate` (`template/step.rs:233`) gets
  `enable_if: Option<BoolOrFormatString>` and `output_variables: Option<Vec<OutputVariableDefinition>>`.
  `template::Environment` (`template/environment.rs:17`) gets `enable_if`.
* `OutputVariableDefinition`: reuse the job-parameter machinery instead of a parallel set of structs. Deserialize the
  same discriminated union as `JobParameterDefinition`, reject `default`, `userInterface`, and (for `PATH` and
  `LIST[PATH]`) `dataFlow` in validation, keep `objectType` without checking it, and wrap the result in a newtype.
  `collect` (an `OutputCollectMode` enum, `ONCE` by default) and `unit` (an `Option<String>`) are not job-parameter
  fields, so the wrapper takes them out of the mapping before it
  deserializes the union, which would otherwise reject them as unknown keys. The wrapper exposes `name`,
  `job_param_type`, `collect`, `unit`, and `check_constraints`. Three cautions:
  * The RFC says that only declared constraint properties apply (`<OutputVariableDefinition>` item 6). Any default
    that a job-parameter check implies when a property is omitted must be turned off for outputs. Test that a
    `RANGE_EXPR` output longer than 1024 characters is accepted when it declares no `maxLength`.
  * Error paths must say `outputVariables[i]`, not `parameterDefinitions[i]`.
  * Under `collect: APPEND` the constraints split: `item` applies to each element as it arrives, `maxLength` to the
    growing list, and `minLength` only when the action exits. `check_constraints` on a whole list can't express that,
    so the wrapper also exposes `check_element` and `check_length(len, at_exit: bool)`.
* `job::Step` (`job/mod.rs:105`) gets `output_variables: Option<Vec<job::OutputVariable>>`,
  `binding: StepBinding`, and `path_inputs: Vec<job::PathInput>` (M4.5). `job::Step` derives `PartialEq`, `Hash`, `Serialize`, and `Deserialize`
  (`job/mod.rs:103`), and `JobParameterDefinition` implements none of the first three, so `job::OutputVariable`
  cannot hold the M1 wrapper. It is its own struct: a name, a `JobParameterType`, the `collect` mode, the `unit`, and
  an `OutputConstraints` value
  that holds the declared constraint properties in hashable, serializable form (`Float64` for floats).
  `JobParameterType` derives `Serialize` but not `Deserialize` (`types.rs:175`), so it needs `Deserialize` added,
  which is a public API change recorded in `public-api.md`. It has a
  `check_constraints(&ExprValue)` that applies the same rules as the job-parameter checks, and it is built from the
  template definition in `create_job`. `job::Environment` (`job/mod.rs:147`) gets
  `enabled: bool`. New fields use `skip_serializing_if` so that the serialized form of existing jobs doesn't change
  (`tests/integration/test_job_step_serde.rs`). They also need `#[serde(default)]`, with `Bound` as the default
  binding and `true` as the default for `enabled`. Without it, a job serialized before the change, or one whose
  fields were skipped, fails to deserialize with `missing field`.

### M2. Validation

Add a pass 11, `validate_v2023_09/control_flow.rs`, following the `wrap_actions.rs` pattern. It checks the RFC's static
rules (§Validation, "At template validation"):

1. Without the extension, `outputVariables` and `enableIf` are errors at their own paths. A `Steps.*` reference
   already fails as an unknown symbol, because nothing seeds it.
2. With `CONTROL_FLOW`, `EXPR` is required (reuse `check_expr_prerequisite`). `CONTROL_FLOW` in an Environment Template
   is an error, and so is `enableIf` in one.
3. `outputVariables`: 1–32 elements, unique names, an allowed type, valid constraints, `collect: APPEND` only with a
   `LIST[...]` type, a `unit` of 1–32 characters with no Unicode Cc characters, and no `LIST[LIST[INT]]` on a Step
   with a `parameterSpace`. Outputs on a Step with a `CHUNK[INT]` parameter are allowed (M3, chunked Steps).

The scope rules go into pass 8 (`format_strings.rs`), because that pass already builds one symbol table per field
family:

* Add a helper `add_steps_scope(symtab, jt, step_index, ScopeKind)`. For each direct whole-Step dependency `P` of the
  Step whose name is an `<Identifier>`, it seeds `Steps.P.Output.<V>` and `Steps.P.TaskParam.<N>` as
  `ExprValue::unresolved(T)`, typed by the RFC's table in §The `Steps.*` scope, and their `RawOutput` and `TaskRawParam`
  twins with the same types. A producer with `enableIf` gets `T?`, and a producer with a `parameterSpace` gets
  `list[T]`. `Output.<V>` and `TaskParam.<N>` whose type is or contains `path` are seeded only in host scopes; the `Raw`
  forms are seeded everywhere. Call the helper from the Step's `enableIf`, `let`, `hostRequirements`, `range`,
  step-environment, and script symbol tables. Never call it for the job name, `jobEnvironments`, `combination`, or
  Environment Templates.
* An unknown symbol under the `Steps.` prefix gets a specific message instead of EXPR's generic one. Scan
  `accessed_symbols()` for the `Steps.` prefix and compare against the template, with messages for each case: no such
  Step; a Step name that is not an `<Identifier>`; the Step is not a direct dependency ("add `dependsOn: P`"); a
  self-reference; no such output; no such Task parameter; a `PATH`-typed `Output` or `TaskParam` in a
  TEMPLATE-scope field ("use `RawOutput`" or "use `TaskRawParam`"). Because the
  check is a static scan, it also catches a reference inside a branch that is never evaluated.
* `jobOutputDefinitions`: 1–50 elements, unique names, types and constraints as for outputs, except that
  `LIST[LIST[INT]]` is allowed (there is no `parameterSpace` to nest it in). Each `value` is checked
  against a symbol table with `Param.*` (minus `PATH` types), `RawParam.*`, `Job.Name`, and the `Steps.*` symbols of
  every Step with an `<Identifier>` name (with `add_steps_scope` in a mode that skips the direct-dependency rule and
  seeds `PATH`-typed values only in their `Raw` forms), with the target type made optional.
* `enableIf` is validated with target type `bool`, not `bool?`. Its symbol table has no `let` names, no `Task.*`, and
  no `Session.*`.
* The step-graph cycle check (`structure.rs:1118`) is unchanged. Dependency names are already checked at
  `structure.rs:217`.

The RFC's validation warning for `null` in a discarding position (§Modifications to the Expression Language) is
deferred in the prototype. openjd-rs has no warning channel, and adding one means a public API decision in
`decode_job_template` and output from `openjd check` (ambiguity A1). The prototype records the gap in
`specs/model/validation.md`.

### M3. Output directive parser and collector

Add `openjd_model::outputs`. It does no I/O, so sessions, the CLI, `openjd-for-js`, and a scheduler that re-validates
can all share it.

```rust
/// The payload after `openjd_output_var:`.
pub enum OutputDirective<'a> {
    Inline { name: &'a str, task: Option<i64>, json: &'a str },
    File { name: &'a str, task: Option<i64>, path: &'a str },
}
pub fn parse_output_directive(payload: &str) -> Result<OutputDirective<'_>, OutputError>;

/// Collects one Task attempt's directives.
pub struct OutputCollector { /* declared outputs, values so far, retained size, limit */ }
impl OutputCollector {
    pub fn new(
        declared: &[job::OutputVariable],
        max_bytes: usize,
        path_rules: &[PathMappingRule],   // the Session's rules, for PATH values
        worker_format: PathFormat,
        chunk: Option<&[i64]>,            // the chunk's CHUNK[INT] values, or None for an unchunked Step
    ) -> Self;
    /// Steps 1-2 of §Processing a directive: name declared, ONCE not repeated. Called before an `@` file is read.
    pub fn check_name(&self, name: &str, task: Option<i64>) -> Result<(), OutputError>;
    /// Bytes an `@` file for `name` may have, not counting the LAST value it would replace.
    pub fn remaining_bytes_for(&self, name: &str, task: Option<i64>) -> usize;
    /// Steps 4-6: convert the JSON text (inline, or a file's content), check constraints, apply the collect mode,
    /// update the retained size. Returns the accepted value, for the caller to report.
    pub fn accept(&mut self, name: &str, task: Option<i64>, json: &[u8], from_file: bool)
        -> Result<ExprValue, OutputError>;
    /// At exit: ONCE and LAST outputs present, APPEND minLength met, for every Task of the chunk.
    /// One map per Task, in the chunk's order (one map for an unchunked Step).
    pub fn finish(self) -> Result<Vec<IndexMap<String, ExprValue>>, OutputError>;
}
```

* The grammar is the RFC's §The `openjd_output_var` directive: leading spaces or tabs, an `<Identifier>`, an optional
  `[<Int>]` task selector (an optional `-` then digits, parsed as `i64`, no whitespace), `=` with nothing before it, optional spaces or tabs, then either `@` and a non-empty path (trailing spaces and tabs removed)
  or one JSON value with optional trailing spaces or tabs. The
  caller strips the line terminator. File content may start with a BOM and may span lines, and must be valid UTF-8
  without NUL; `accept` checks that when `from_file` is set.
* `NaN`, `Infinity`, and numbers that overflow `f64` are invalid. `serde_json` rejects all three without
  `arbitrary_precision`; pin each with a test.
* Collect modes: `ONCE` rejects a second value (in `check_name`); `LAST` replaces the value and subtracts the
  replaced value's size; `APPEND` converts against the element type and pushes. Every `LAST` value must pass the
  constraints, not only the last.
* Size accounting follows §Limits item 2: for each retained value, the inline JSON's bytes without surrounding
  whitespace, or the whole content of its `@` file.
* Messages name the variable and the problem, and never quote an `@` file's content. They may quote an inline
  value, which the Action printed itself; the RFC asks for the offending text.
* Type conversion: type each JSON value against the target type (the declared type, or the element type under
  `APPEND`), then coerce with EXPR's implicit coercion, `ExprValue::coerce` (`value.rs:904`), as the RFC's §Type
  conversion requires. The RFC infers an array "as a list literal with the target type" (§1.2.6), so arrays are
  converted element by element toward the target element type. `json_to_expr_value` (`parameters.rs:1381`) can't be
  reused as is: it builds lists with a `NULLTYPE` hint (`parameters.rs:1406`), so the RFC row
  `LIST[STRING] L=[1, "a"]` fails with `Invalid list value: make_list expected int element, got string`. Reuse its
  scalar arms. It is `pub(super)`, so move it to a module that both `create_job` and `outputs` can use. Keeping a
  number's written spelling (A6) is not free: the workspace's `serde_json` is built without `arbitrary_precision`, so
  `1.50` is already `1.5` after parsing. If A6 is answered "keep the digits", either enable `arbitrary_precision` (a
  workspace-wide change, through feature unification) or lex the number tokens separately. Do not reuse
  `coerce_json_to_job_parameter_type` either: it follows job-parameter rules, which are more lenient (for example `1`
  is accepted as a `BOOL` at `parameters.rs:691`). Ambiguity A7 asks the RFC which of the two it means. Before
  converting, reject `null` and objects at any depth, and nesting deeper than the target type. Each row of the RFC's
  conversion table, including its three `APPEND` rows, becomes a unit test.
* `PATH` values: after conversion, `accept` maps each `PATH` value or `LIST[PATH]` element back with
  `unapply_rules` (E1); a `Relative` or `NoRoundTrip` error makes the directive invalid (RFC invalid-directive item
  10). The stored `ExprValue::Path` carries the matching rule's `source_path_format` (the value's own format when
  the Session has no rules). Constraints and the size accounting then use the stored form, and so does the value
  `accept` returns. For a
  `PATH` value the size counted is the larger of the reported bytes and the stored value's JSON serialization.
* Chunked Steps: with `chunk` set, every call must pass `Some(task)` naming a value in `chunk`, and with `chunk`
  `None`, `None`; anything else is invalid (RFC invalid-directive item 11). Values, collect-mode state, and the size
  limit are kept per Task. `finish` fails if any Task lacks a required value, which fails the whole chunk.
* Constraints: `job::OutputVariable::check_constraints`, `check_element`, and `check_length` from M1.

### M4. Job creation and binding

This is the core of the prototype. A `job::Step` is either bound or waiting:

```rust
pub enum StepBinding {
    /// Tasks are enumerable (today's only state).
    Bound,
    /// Materialization expressions read Steps.* symbols that are not yet concrete.
    PendingInputs { waiting_on: Vec<String> },   // Steps.* symbols, sorted
    /// enableIf was false (at job creation, or at binding).
    Skipped,
}
```

A pending Step keeps `parameter_space` and `host_requirements` as `None` until it is bound. Its script and
Environments are kept as they are, because they are carried forward as format strings anyway.

1. **Which Steps are pending.** Pending status comes from static analysis, not from evaluating with `Unresolved`
   values. The RFC's rule is about what a field *references*, and a reference inside a branch that is never taken
   still counts. The materialization set of a Step is the union of `accessed_symbols()` over its `enableIf`, step
   `let`, `hostRequirements` fields, Task parameter `range`s, `chunks` fields, the TEMPLATE-scope fields of its
   step Environments (including their `enableIf`), and the `timeout` and `cancelation` fields of its script. Expand
   step-`let` names to the symbols they read, transitively. The Step is pending if the set contains a `Steps.P.*`
   symbol that is not concrete at Job creation (§When a Step is bound):
   * `Steps.P.Output.*` is concrete at creation only when `P` is statically `Skipped`, as `null`. For example, with
     `EncodeVideo=false`, `Publish` in the render sample binds at creation.
   * `Steps.P.TaskParam.*` is concrete at creation when `P` is `Bound` or statically `Skipped` at creation.

   `create_job` loops over Steps in template order (`create_job/mod.rs:265`), and a Step may appear before a Step it
   depends on (the existing `reverse_dependency_order.yaml` CLI test template). So `create_job` needs to instantiate
   Steps in dependency order, building `StepInputs` from the Steps it has already instantiated, while it still stores
   the Steps in template order. `StepDependencyGraph` is built from a `job::Job`, so it can't supply that order before
   the Job exists. Instead, factor the Kahn's-algorithm pass out of `detect_dependency_cycles`
   (`validate_v2023_09/structure.rs:1074`) so that it returns the order of the template's Steps. As written, that pass
   counts in-degree on each *dependency* (`structure.rs:1091`–`:1097`), so it yields Steps that nothing depends on
   first, and it seeds its queue from `HashMap` iteration. The factored version must therefore reverse the edges
   and break ties by template index, so the order is deterministic. Errors that `instantiate_step` collects into
   `check_errors` then arrive in dependency order, so sort them back into template order before reporting, to keep
   today's message order.
2. **Static `enableIf`.** An `enableIf` that reads no `Steps.*` is evaluated in `create_job`. If it is false, the Step
   becomes `Skipped` and nothing after `enableIf` is evaluated. That includes the step `let` evaluation and carried-forward checks that
   `instantiate_step` runs today (`instantiate.rs:62`–`:149`), so a skipped Step's `let` may assume the Step runs.
   Environment `enableIf` works the same way and sets `job::Environment.enabled`.
3. **Binding API.**

   ```rust
   pub fn bind_step(
       job_template: &JobTemplate,
       job_parameter_values: &JobParameterValues,
       ctx: &ValidationContext,
       job: &job::Job,
       step_index: usize,
       inputs: &StepInputs,
       committed_task_count: u64,   // Tasks of Steps already bound, for max_task_count (item 7)
   ) -> Result<job::Step, ModelError>;   // Bound or Skipped; Err = the Step FAILS at binding
   ```

   `bind_step` refactors `instantiate_step` (`instantiate.rs:37`) so that one code path serves both `create_job` and
   binding: the Step's symbol table is the job symbol table plus the concrete `Steps.*` symbols from `inputs`. The
   other inputs `instantiate_step` takes today (the job symbol table, `has_expr`, `EffectiveLimits`, `EvalBudgets`)
   are rebuilt from `ctx`, the parameter values, and `job.name`, as `create_job` builds them. The
   evaluation order follows §Materialization expressions. Binding is atomic because the function returns a new Step;
   the caller swaps it in.

   Job creation runs its own format-string checks, separate from pass 8. `instantiate_step` re-checks every Step's
   script and Environments (`instantiate.rs:118`, `:142`) against tables from `build_task_check_symtab`
   (`instantiate.rs:277`) and `build_env_check_symtab` (`:359`). Those tables must seed each `Steps.*` symbol too, as
   concrete when it is known and unresolved otherwise. Without that, every script that reads `Steps.*` fails
   `create_job` with `Undefined variable`. Seed the `Steps.*` placeholders in both builders, for the Step's own
   direct dependencies. Do not put them in `add_unresolved_session_symbols` (`instantiate.rs:224`): it takes no Step,
   so it can't know which producers are visible. Do not put unresolved placeholders in the Step symbol table either,
   because that table becomes `resolved_symtab` through `filter_symtab_for_step`. Only concrete values go there,
   at binding. This seeding lands in PR 2, together with the validation rules, so that a template that passes
   `openjd check` also passes `create_job` (see PR and commit order).
4. **Assembling inputs.** Schedulers and the CLI should not have to reimplement the shape rules, so the model provides
   them:

   ```rust
   pub enum ProducerState<'a> {
       /// Bound but not finished: contributes TaskParam only (Output stays unresolved).
       Bound,
       Succeeded { task_outputs: &'a [IndexMap<String, ExprValue>] }, // indexed by iteration order
       Skipped,
   }
   impl StepInputs {
       pub fn add_producer(&mut self, producer: &job::Step, state: ProducerState) -> Result<(), ModelError>;
   }
   ```

   `add_producer` makes `Output.<V>` a scalar when the producer has no parameter space, and a list in iteration order
   when it has one. It fills `TaskParam.<N>` from `StepParameterSpaceIterator::new_with_chunk_override(ps, Some(1))`,
   one element per Task even for a `CHUNK[INT]` parameter. It fills `RawOutput` and `TaskRawParam` with the same values
   (the stored, submission-context form), and for a type that is or contains `path` leaves `Output` and `TaskParam` to
   be derived on the worker (item 5 and S4). A skipped producer sets every symbol to `null`, including `Output` and
   `TaskParam` of path types, so nothing is left to derive; M4.5 then copies those `null`s directly, and they are not
   added to `path_inputs`. A `Bound` producer contributes `TaskParam` only. The list
   type hint follows the declared type, so `[]` becomes a typed empty list.
5. **Host-scope `Steps.*` in Steps bound at creation.** A Step such as `FramesToVideo` reads `Steps.*` only in its
   script, so it binds at job creation but needs the values before its Tasks run. Add
   `pub fn attach_step_inputs(step: &job::Step, inputs: &StepInputs) -> Result<job::Step, ModelError>`. It merges the
   concrete `Steps.*` values that the Step reads into `resolved_symtab`. It has to find those symbols itself, because
   `filter_symtab_for_step` (`instantiate.rs:909`) runs only inside `instantiate_step`, where the symbol table has no
   `Steps.*` entries to keep (`copy_symbol_value` skips a missing symbol, `format_string.rs:612`). Reuse
   `collect_all_accessed_symbols` (`instantiate.rs:1014`) and copy the matching values from `inputs`. For a
   path-typed `Steps.P.Output.X` or `TaskParam.N` that the Step reads, `inputs` holds only the `Raw` twin, so copy
   that instead, as `include_raw_param_fallbacks` (`instantiate.rs:993`) does for `PATH` `Param.*`. Every path-typed
   `Raw` value it stores, whether the Step reads it directly or through this fallback, is stored as a `string` (or
   list of strings) and recorded in `job::Step::path_inputs` (`PathInput { symbol: String, ty: openjd_expr::ExprType }`, where `ty`
   is `path`, `list[path]`, or `list[list[path]]` for a multi-Task producer's `LIST[PATH]` output), which S4 uses.
   Every bound Step that reads `Steps.*` in host-scope fields goes through `attach_step_inputs`, including one bound
   by `bind_step`; `bind_step` doesn't put path-typed `Steps.*` values into `resolved_symtab` itself. Expose the list
   of producers whose values a Step needs before dispatch as `job::Step::input_producers()`.
6. **Range rules under `CONTROL_FLOW`.**
   * An empty list from a format-string range is already accepted (see the table above). Add tests for it under
     `CONTROL_FLOW`. Whether it stays accepted *without* `CONTROL_FLOW` is ambiguity A2.
   * A list-literal range whose evaluated items flatten to nothing needs new code, in two parts. First, `INT` and
     `FLOAT` list literals must flatten list-valued items as EXPR §1.3.2 requires; today only `STRING` and `PATH` do
     (A14). Second, the empty result must be accepted. The RFC allows a list literal to evaluate to empty (§Empty
     parameter spaces), but openjd-rs errors today, with `EXPR` alone: a `STRING` `range: ["{{Param.L}}"]` with
     `L=[]` fails with `Task parameter 'X' range has no elements after resolution` (`ranges.rs:528`). Under
     `CONTROL_FLOW`, accept the empty result for all four types.
   * Lift the 1024-element cap (`ranges.rs:336`, `:475`, `:556`) for format-string ranges when `CONTROL_FLOW` is
     enabled. List literals stay capped as written. This depends on how A3 is answered, so the cap is decided in one
     function that the answer can change.
   * Range strings are unchanged. `"1-0"` stays an error in openjd-rs until A4 is settled.
7. **Task-count limits.** Keep `max_task_count` as the job total. In `create_job`, count only bound Steps. In
   `bind_step`, `committed_task_count` carries the count already committed, and the binding that would exceed the
   limit fails, naming it. Also add an optional `CallerLimits::max_step_task_count`, the per-Step limit from
   §Limits item 4. See A5.
8. **`StepDependencyGraph`** (`job/step_dependency_graph.rs`) is unchanged. The step graph is fixed.
9. **Job outputs.** `create_job` evaluates every job output whose `value` reads no `Steps.*` symbol that is not
   concrete yet, and fails Job creation if one fails, as an invalid parameter does. The rest are evaluated later
   through:

   ```rust
   pub fn job_output_inputs(job: &job::Job, output_index: usize) -> Vec<String>; // Steps.* symbols it waits on
   pub fn evaluate_job_output(
       job_template: &JobTemplate,
       job_parameter_values: &JobParameterValues,
       ctx: &ValidationContext,
       job: &job::Job,
       output_index: usize,
       inputs: &StepInputs,      // built with add_producer for every Step the value reads
   ) -> Result<Option<ExprValue>, ModelError>;   // Ok(None) for a null result; Err fails the Job
   ```

   It evaluates `value` with the job symbol table plus the `Steps.*` symbols from `inputs`, converts to the declared
   type, and checks the constraints (skipped for `null`). `StepInputs` here may hold producers that are not
   dependencies of anything, so `add_producer` doesn't check that. The CLI and a scheduler call
   `evaluate_job_output` whenever a symbol it waits on becomes concrete: when a Step it reads `Output` or
   `RawOutput` of succeeds or is skipped, and when a Step it reads `TaskParam` or `TaskRawParam` of is bound or
   skipped. Deciding when to call it is the caller's
   job, which keeps the scheduling policy (fail the Job, mark stale on re-run) out of the model.

### Model specs to update

`specs/model/template-types.md`, `validation.md`, `job-creation.md`, `job-types.md`, `parameter-space.md`,
`step-dependencies.md` (skipped and zero-Task dependencies), `public-api.md`, and a new `job-outputs.md`. Also update
`specs/architecture.md` with the binding and job output evaluation stages.

## openjd-expr

No new types, functions, or operators, which matches the RFC (§Modifications to the Expression Language).

* "Unresolved until run time" is `ExprValue::Unresolved(T)` (`value.rs:789`), and its coercion is type-level
  (`value.rs:904`). A `T?` placeholder is accepted where `T` is required and checked later (04 T3). That is the RFC's
  stated behavior, and the prototype adds a test that pins it for a `Steps.*` symbol.
* The `Steps` namespace is just symbol-table paths (`Steps.P.Output.V`). `SymbolTable` is hierarchical, and
  `copy_symbol_value` (`format_string.rs:595`) stops at the deepest stored value. So when `filter_symtab_for_step`
  copies the symbols a Step reads, a reference such as `Steps.P.Output.Name.upper()` keeps `Steps.P.Output.Name`.
* No reduction helpers are needed. The RFC's samples use `sum`, `min`, `len`, `range`, slicing, comprehensions, and
  `flatten`, which all exist. `argmin` and `argmax` are RFC 0006 future work.
* Deferred: `FormatString::segment_static_types(...)`, which returns each interpolation's static type and whether it
  is the whole field. The M2 warning needs it, and it ships with the warning.

The expr specs need no change for the language unless the warning lands.

### E1. Reverse path mapping

`PATH` outputs are stored in the submission path context (RFC §Path values), so `openjd_expr::path_mapping`
(`crates/openjd-expr/src/path_mapping.rs`), which has the forward direction (`PathMappingRule::apply_with_format`,
`apply_rules_with_format` at `:209`), gets the reverse:

```rust
/// Map a path reported on the worker back to the submission path context.
pub fn unapply_rules(
    rules: &[PathMappingRule],   // the Session's rules, sorted as for apply_rules
    path: &str,
    worker_format: PathFormat,
) -> Result<String, UnmapError>;
pub enum UnmapError { Relative, NoRoundTrip }
```

It implements the RFC's reference inverse: a URI is returned unchanged; a relative path is `Relative`; an empty rule
list returns the absolute path unchanged; otherwise it normalizes the path, collects the rules whose `destination_path`
matches on a component boundary (case-insensitively for Windows paths), builds each candidate from the remaining
components, tries them by decreasing `destination_path` length and then in the order the caller listed the rules, and
returns the first that maps back. A candidate is the remaining components joined with the separator of the rule's
`source_path_format` (`/` for `URI`), joined to `source_path` with one more separator unless `source_path` already ends
with one, or `source_path` itself when no components remain. The Session passes its rules sorted by `source_path` length
(`session.rs:682`), so "listed order" there is that order; the RFC's tie-break is only a SHOULD, and the round-trip
check keeps the result correct either way.

Two details of the existing forward code matter:
* `apply_rules_with_format` returns its input unchanged when no rule matches, and `apply_with_format` doesn't
  normalize (a `destination_path` with a trailing separator gives `/mnt/p//x`). So the round trip uses a forward
  helper that reports whether a rule matched, and compares the normalized forward result with the normalized reported
  path under the worker format's rules (case-insensitive with both separators on Windows), not as strings.
* `is_rooted` (`path_mapping.rs:182`) does not treat a Windows drive path such as `C:\x` as rooted, so the `Relative`
  check needs its own absoluteness test (a drive letter or UNC prefix on Windows, a leading `/` on POSIX).

It lives beside the forward code and reuses `split_path_parts` (`:174`). Its
tests include the RFC's nested-rule example (`/mnt/p/x/f` is `NoRoundTrip`), a root `source_path` such as `P:\`, a
URI-source rule, and the conversion table row (`/mnt/proj/shot01/out` → `P:\shot01\out`).

`specs/expr/path-mapping.md` gains the reverse-mapping rules beside the forward ones, and `specs/expr/public-api.md`
lists `unapply_rules` and `UnmapError`.

## openjd-sessions

### S1. API

* `run_task` (`session.rs:1758`) takes a `StepScript` and a `resolved_symtab`, not a `job::Step`, so the new inputs
  arrive in a `RunTaskOptions` struct, added as one new parameter beside the existing ones: `output_variables: Option<&[openjd_model::job::OutputVariable]>`, the Step's
  output declarations, and `path_inputs: &[openjd_model::job::PathInput]`, the list M4.5 records (S4 uses it).
  The crate is pre-1.0, so this change is allowed.
* The Session must know whether the Job lists `CONTROL_FLOW`, and for each Environment it enters whether a Job
  Template or an Environment Template defines it. Both decide whether a directive fails its Action or is ordinary
  output. For the first, `openjd run` already passes the Job Template's own profile (`execution.rs:128`), which
  carries its extensions, but the profile is documented as selecting functions and redaction, and a caller may pass a
  wider one. Add an explicit `SessionConfig` field so the rule doesn't depend on how a caller built the profile. For
  the second, `enter_environment` (`session.rs:1133`) takes a bare `&Environment`; add a parameter for its origin to
  it, to `enter_environment_with_output` (`:1147`), which does the work, and to `exit_environment` (`:1479`).
* `ActionMessage` (`action.rs:39`) gets `OutputVar { name: String, value: ExprValue }`, and `SessionConfig` gets an
  optional output callback, `Fn(&session_id, &name, &ExprValue)`, called once per accepted value, so a caller can
  forward each value as it arrives (RFC §Committing values). Putting the values on `ActionStatus` instead would
  re-clone a growing `APPEND` list on every status callback, because `action_status()` builds a new struct each time
  (`session.rs:915`–`:925`).
* `ActionResult` (`action.rs:74`) gets `outputs: Option<Vec<IndexMap<String, ExprValue>>>`: one map per Task, in the
  chunk's order, or a single map for an unchunked Step. The value is `Some` only when the Step declares outputs and
  the Action succeeded. `ActionMessage::OutputVar` gets `task: Option<i64>`.
* For a chunked Step, `run_task` takes the chunk's values from the `CHUNK[INT]` entry of `task_parameter_values` (a
  `range_expr`) and passes them to the collector.
* `run_task` builds the `OutputCollector` with the Session's sorted path mapping rules (`session.rs:681`) and
  `PathFormat::host()`, so `PATH` values are mapped back before they are reported or returned.
* `SessionLimits` (`limits.rs:23`) gets `max_task_output_bytes: Option<usize>`, with a default of at least 1 MiB and
  a `From<&CallerLimits>` mapping. The 64 KiB line limit stays as it is, which meets the RFC's floor.

### S2. Recognizing the directive

* The directive needs its own recognizer. `parse_directive` (`action_filter.rs:45`) matches only
  `openjd_<kind>: ` with a non-empty payload (`:47`–`:52`), but the RFC recognizes any line that begins with
  `openjd_output_var:`, with optional whitespace after the colon. So `openjd_output_var:N=1` is a valid directive and
  `openjd_output_var: ` with nothing after it is an invalid one; neither may fall through as ordinary output. Check
  the `openjd_output_var:` prefix before `parse_directive`, and test both lines.
* The filter is built with a treatment for the current Action: `Allowed` (the Action may report outputs),
  `Forbidden` (a Job Template Action that may not), or `Ignored` (ordinary output: no `CONTROL_FLOW`, or an
  Environment Template Action). `run_task` uses `Allowed` only when `output_variables` is `Some`. Under
  WRAP_ACTIONS, the treatment follows the Action the hook runs in place of, which `run_task`, `enter_environment`,
  and `exit_environment` already know.
* With `Ignored`, the line falls through as today. With `Forbidden`, the filter returns a cancel callback, as a
  malformed `openjd_env` does, and the Action fails with a message that names the directive.
* With `Allowed`, `process_line` (`subprocess.rs:1219`) hands the payload to an `OutputCollector` (M3) owned by the
  line reader for this Action. Processing is synchronous in the reader, so an `@` file is read before any later line
  is processed, as the RFC requires. An invalid directive cancels the Action at once and fails it, as the RFC
  requires, through `CancelMarkFailed`, which calls `cancel_action(None, true)` (`session.rs:2323`), so the Action's
  configured cancelation applies. That path needs one change:
  `process_line` sends a fixed `"Action canceled due to malformed command"` as the `CancelMarkFailed` message
  (`subprocess.rs:1279`–`:1282`), which overwrites any earlier `fail_message` when the Session applies it
  (`session.rs:2322`), and the RFC requires the failure to name the output, the problem, and the offending text.
  Pass the collector's message through as the `fail_message`.
* Truncation: `truncate_line` (`subprocess.rs:62`), and in the helper `LineFramer::accumulate` (`framer.rs:195`),
  `decode_and_truncate` (`framer.rs:89`), and `fit_out_payload` (`framer.rs:116`), cut long lines silently today.
  `accumulate` is the one that cuts an ordinary over-long line on the cross-user path; the other two helper sites
  then see an already-capped line. Each must report that it truncated (for the
  helper, a flag in `Response::Out`, which is a protocol change on both sides of the helper boundary). A truncated
  line that starts with `openjd_output_var:` is invalid, and the discarded remainder is never treated as a line.
* Encoding: a line that `decode_backslashreplace` had to escape is not valid UTF-8, so a directive on it is invalid.
  The reader must carry a "lossy" flag with the decoded line to tell. On the cross-user path the helper decodes the
  line (`framer.rs:48`, `:89`), so this flag also crosses the helper protocol, beside the truncation flag.
* Line ends: the Unix helper sends `line.trim_end()` (`helper/src/runner.rs:61`–`:63`), which strips all trailing
  Unicode whitespace, while the Windows helper (`runner_win.rs:191`–`:197`) and the direct path
  (`subprocess.rs:1121`–`:1126`) strip only the line terminator. The RFC's grammar allows
  only spaces and tabs around the value, so a directive followed by, say, a form feed would be valid on one path and
  invalid on the other. Make the Unix helper strip only the line terminator, and let the parser apply the grammar.
* Logging: a valid directive line is treated like the other directives. It is logged by default and hidden when
  `echo_openjd_directives` is `false` (`action_filter.rs:211`). An invalid or `Forbidden` directive must always stay
  in the log, with ` -- ERROR: <message>` appended, as the error branches for `openjd_env` already do
  (`action_filter.rs:166`–`:175` and `:193`–`:204`, returning `pass_through = true`). It must not reach the shared `:211` path, which
  would hide it when echo is off.

### S3. Reading `@` files

* Resolve a relative path against the Session working directory. An absolute path is used as written, which in
  practice means it must name something inside the working directory; to use a beneath-the-root resolver with it,
  first make it relative to the working directory, and treat a path outside it as invalid.
* Symbolic links are followed, as the RFC's "after following symbolic links" requires: a link that stays inside the
  working directory is valid. What must be inside is the file actually opened. On Linux, open with
  `openat2(RESOLVE_BENEATH)` from a handle to the working directory, which follows links but refuses any that leave
  it, and fall back to opening the path and checking `/proc/self/fd/<n>`. On macOS, open and check `F_GETPATH`; on
  Windows, `GetFinalPathNameByHandleW`. Open without blocking (`O_NONBLOCK` on Unix), because opening a FIFO for
  reading otherwise blocks until a writer appears, which would stall the line reader and, on the helper path, cancel
  handling. Then require a regular file with `fstat`, and read at most `collector.remaining_bytes_for(name) + 1`
  bytes.
* Same-user Sessions read the file directly; that meets the RFC's access rule.
* Cross-user Sessions must read it as the job user. The parent must not read the file itself, even with an owner or
  link-count check, because the RFC requires no more access than the job user. PR 5 has the helper, which runs as
  the job user, resolve `@` directives as it reads the Action's output, so the RFC's ordering falls out of the
  stream:
  * `RunCommand` (`protocol.rs:19`) gets an optional `output_files: { max_bytes }`. The parent sets it only for an
    `Allowed` Action; otherwise the helper leaves `openjd_output_var:` lines alone, so `Ignored` lines stay ordinary
    output and `Forbidden` lines reach the parent unchanged and fail there. The root for path resolution is
    `RunCommand.cwd`, which is the Session working directory (`session.rs:2055`, `:2127`).
  * The helper sub-crate depends only on `serde`, `serde_json`, `nix`, and `windows` (`helper/Cargo.bundled.toml`), so
    it can't use the model's parser. It gets a minimal recognizer for the `@` form only, which must accept exactly the
    lines the M3 parser parses as `File` and extract the same name, selector, and path: the `openjd_output_var:`
    prefix, optional spaces or tabs, an `<Identifier>`, an optional `[<Int>]` selector, `=`, optional spaces or tabs,
    `@`, and a non-empty path with trailing spaces and tabs removed. Anything else goes to the parent as an ordinary `Out` line for the model parser. A line the
    helper truncated or had to decode lossily is not resolved; it is forwarded as an `Out` line with its S2 flags, and
    the parent rejects it. The file operations need new `nix` features (`fs`) and `windows` features
    (`Win32_Storage_FileSystem`), which go through the helper's own `deny.toml` and gates.
  * For a `@` directive, the helper reads the file as above, up to `max_bytes + 1`, and sends a new
    `Response::OutputFile { line, content | error }` in place of the `Out` line. The parent reads newline-delimited
    JSON with a 128 KiB line cap (`cross_user_helper.rs:23`, checked at `:119`–`:128`), and content up to 1 MiB
    doesn't fit one line, so the frame is a header followed by chunks of JSON-escaped content, each sized by its
    encoded length as `fit_out_payload` does.
  * The helper doesn't know the declared outputs or the values so far, so it reads before the parent's checks. To
    keep the RFC's outcome, it reports a read failure in the frame rather than failing the line itself, and the
    parent applies `check_name` first, then the read result, then the remaining-size check. This differs from the
    RFC's order in two ways that don't change the outcome: the helper may read a file whose directive is then
    rejected, and it reads up to the whole per-Task limit rather than what remains.
  * The parent re-parses `OutputFile.line` with the M3 parser and treats a disagreement with the helper's name or
    path as invalid. An `@` directive on the cross-user path that arrives as a plain `Out` line, because the
    helper's recognizer didn't match it, is invalid too. The parent never reads the file.
  * A helper command to read a file on request, answered while a `Run` is in progress, was considered. Its reply
    would arrive on the stream the parent's `select!` loop (`cross_user_helper.rs:620`) reads, interleaved with later
    `Out` lines, which the parent would have to hold back to keep the ordering.
* On the direct path, file reads are blocking I/O inside the async line reader. They are bounded by the size limit;
  use `spawn_blocking` and await it before the next line, so ordering is kept.
* The runtime never modifies or deletes the file.

### S4. At exit, and `Steps.*` values at Task run time

* If the Action did not succeed, or a directive was invalid, return `outputs: None`. Values already reported through
  the callback are discarded by the caller.
* Otherwise call `collector.finish()`. An error fails the Task with its message; success returns the values.
* `Steps.*` values arrive in `resolved_symtab` (M4 item 5), with path-typed values only in their `RawOutput` and
  `TaskRawParam` forms. These must stay in the stored (submission) form until they are mapped, but
  `build_symbol_table` deserializes the base with `to_symtab(PathFormat::host())` (`session.rs:2473`), which on a
  Windows worker rewrites `/` to `\` in every `path`-typed entry, so a POSIX submission path would no longer match a
  POSIX rule. `RawParam.*` avoids this by being stored as `string` (`create_job/parameters.rs:1429`–`:1447`). The
  same applies here: `attach_step_inputs` stores path-typed `Raw` values as `string` (or `list[string]`,
  `list[list[string]]`), and lists the symbols in `job::Step::path_inputs` with their declared types.
  `build_symbol_table` then gets a second loop beside the `Param.*` one (`:2480`–`:2525`): for each entry of
  `path_inputs` it reads the stored strings, sets `RawOutput.<V>` or `TaskRawParam.<N>` to `path` values in the
  stored form (`ExprValue::new_path` with the stored value's own format: Windows for a drive path or one starting
  with `\\`, URI for a URI, POSIX otherwise; not the host format; `new_path` normalizes separators within that
  format, so `P:/x` reads back as `P:\x`, which is the same Windows path), and
  sets `Output.<V>` or `TaskParam.<N>` to the forward-mapped `path` values, element by element.

### Sessions specs to update

`specs/sessions/action-filter.md` and `action-messages.md` (the new directive and its three treatments),
`subprocess.md` and `embedded-cross-user-helper.md` (truncation flags, the `@` file read), `session.md` (collection,
reverse mapping of `PATH` outputs, forward mapping of path-typed `Steps.*`, and the new configuration), and
`public-api.md`.

## openjd-cli

### C1. `openjd run` as a scheduler simulator

Today `run_workload` (`execution.rs:341`) iterates `steps_to_run` in topological order and calls `execute_step`. The
prototype keeps that loop and adds a small in-memory scheduler state:

```rust
struct StepRecord { state: SimStepState, task_outputs: Vec<IndexMap<String, ExprValue>> }
enum SimStepState { Pending, Bound, Skipped, Succeeded, Failed, Blocked }
```

Before the loop, print the job outputs that `create_job` already evaluated. Then, for each Step in topological
order:

1. If a dependency is `Failed` or `Blocked`, mark the Step `Blocked` and print why. The current stop-on-first-failure
   behavior (`ctx.session_failed`) already prevents it from running.
2. Build `StepInputs` from the records of its direct dependencies with `StepInputs::add_producer`.
3. If the Step is `PendingInputs`, call `bind_step`. On error, print the binding failure (field and expression) and
   mark the Step `Failed`. If it is `Skipped`, print `Step 'X' skipped (enableIf is false)`.
4. Otherwise call `attach_step_inputs`, enter only the enabled step Environments, and run the Tasks as today. Record
   `ActionResult.outputs` for each Task index (for a chunk, each map goes to the Task whose `CHUNK[INT]` value it is
   for), and print them after each Task with their units
   (`Output mpjpe_mm = 52.6 mm`). Values reported while a Task runs can be printed as they arrive, marked as not
   committed.
5. A Step with zero Tasks prints `Step 'X' has 0 tasks` and succeeds.
6. After each Step is bound, succeeds, or is skipped, call `evaluate_job_output` for every job output that is still
   pending and whose inputs are now all concrete (`job_output_inputs`), store the result, and print it once, as
   `Job output VideoCount = 12`. An error prints the output, expression, and message, marks the run failed, and stops
   running Tasks, as a failed Task does. At the end, list the job outputs that were never evaluated, naming the Step
   each waits on. With `--step` alone, those are the outputs that read Steps not run. The run is reported as succeeded
   only after every job output whose inputs are concrete has been evaluated. The CLI imposes no size limit on job
   outputs, which the RFC's 1 MiB minimum allows.

Job Environments with `enabled == false` are not entered (the loop at `execution.rs:360`).

Options:

* `--step X` without `--run-dependencies`, for a Step that reads `Steps.*`, needs the upstream values. Add
  `--step-inputs file://inputs.yaml | '<json>'`, a map of `Steps.P.Output.V` and `Steps.P.TaskParam.N` to values,
  checked against the declared types with the M3 coercion. Without it, the error names the missing producers. This
  option is what the RFC's proxy testing did by hand.
* `--maximum-tasks` or `--tasks` on a producer leaves its outputs incomplete. Its consumers become `Blocked` with
  "inputs from Step 'P' are incomplete because not all of its Tasks ran". This follows the RFC's completeness rule
  (§Reductions item 3).
* `--output json|yaml`: `RunResult` (`run/result.rs:7`) gets a `steps` array of
  `{name, state, task_count, outputs: [ {<var>: <value>} ... ]}` and a `job_outputs` map of name to value (`null`
  for no value) plus a list of the ones not evaluated.
* No path mapping rules (the default) means `PATH` outputs are stored as reported; `--path-mapping-rules` exercises
  reverse mapping.

### C2. `openjd summary` and `openjd check`

* `summary` (`summary.rs:113`) prints a pending Step as `Tasks: determined at run time from
  Steps.VideoToFrames.Output.FrameCount`, which follows §Implementation strategy. The total becomes
  `Total tasks: N + (determined at run time)`. A statically skipped Step is shown as `SKIPPED`.
* `summary` lists the job outputs, with their types and units, and which Steps each waits on. This is the CLI's
  form of the RFC's SHOULD that a pending job output be shown with what it waits for (§Job outputs item 2).
* `check` needs no change beyond M2.

### CLI specs to update

`specs/cli/run.md`, `summary.md`, and `output-formatting.md`, plus the CLI help text in `src/help.rs` (Q8).

## Other crates

* **openjd-snapshots**: no change (see the summary).
* **openjd-for-js**: it builds on `openjd-model`, so validation picks up `CONTROL_FLOW` automatically. Rebuild and run
  its vitest suite in the PRs that touch the model. JS bindings for `bind_step` are out of scope for the prototype.

## PR and commit order

Each PR is one `implement-change` loop on a topic branch from `control-flow-prototype`. The lead merges it.

| # | PR | Depends on | Contents |
|---|---|---|---|
| 1 | `feat(model): CONTROL_FLOW extension, outputVariables and enableIf schema` | — | M1 except `jobOutputDefinitions`, and M2 items 1–3 (gating, the EXPR prerequisite, structural rules) |
| 2 | `feat(model): Steps.* scope in template validation` | 1 | the M2 scope rules and their messages except the job output scope, `enableIf` typing, and the `create_job` check-table seeding from M4.3. Until PR 4, `openjd run` of a template that reads `Steps.*` fails when a Task starts, because the value is unresolved; this is acceptable on the prototype branch |
| 3a | `feat(expr): reverse path mapping` | — | E1 |
| 3 | `feat(model): output directive parser and collector` | 1, 3a | M3 |
| 4 | `feat(model): step input binding` | 2, 3 | M4: `StepBinding`, static `enableIf`, `bind_step`, `StepInputs`, `attach_step_inputs`, range rules (including the `INT`/`FLOAT` list-item flattening of A14), task-count limits |
| 5 | `feat(sessions): openjd_output_var directive` | 3, 4 | S1–S4, including the helper protocol and truncation changes, so the helper sub-crate's gates and the cross-user tests apply |
| 4a | `feat(model): job outputs` | 2, 4 | the M1 `jobOutputDefinitions` schema, the M2 job output scope, and M4 item 9 |
| 6 | `feat(cli): simulate step input binding in openjd run` | 4, 4a, 5 | C1, C2 |
| 7 | `test(cli): golden tests for the RFC sample templates` | 6 | the sample tests below |

PR 3 can proceed in parallel with 2. PR 5's directive work (S1–S3) can start after 3, but its S4 part needs
`job::Step::path_inputs` from PR 4. PR 7 can start early as a list of failing tests, and the tests turn
on as PR 6 lands.

## Test strategy

The tests follow the repo's standard. Validation failures assert the path and the full message, and expression
failures assert the message, the expression, and the caret (`AGENTS.md`, Test Quality Standard).

**openjd-model** (`tests/integration/test_control_flow_*.rs`):

* Gating: each new field without the extension, `CONTROL_FLOW` without `EXPR`, and `CONTROL_FLOW` in an Environment
  Template.
* Structure: each `outputVariables` rule, including `LIST[LIST[INT]]` on a multi-Task Step, outputs accepted on a
  `CHUNK[INT]` Step, the 32-item
  cap, `PATH` and `LIST[PATH]` accepted with `objectType` and rejected with `dataFlow`, `collect: APPEND` on a scalar
  type, and `unit` length and control characters.
* Scope: one test per `Steps.*` error message (M2), one per forbidden field family, a reference in a branch that is
  never taken, and a reference through a `let`.
* Types: scalar, list, and optional shapes, checked as `ExprType` on the seeded symbol, and `enableIf` given a `bool?`
  expression.
* Parser and collector: every row of the RFC's conversion table, each "invalid if" rule of §Processing a directive
  that the model owns, both forms of the directive, leading whitespace, whitespace before `=` rejected, whitespace
  after `=` accepted (`N= 1`, `N= @f`), an empty `@` path, `NaN` and `1e400`, file content with a BOM and line breaks;
  `ONCE` repeated and missing, `LAST` replacing a value and its size, `LAST` with an invalid intermediate value,
  `APPEND` with zero and several values, `item` and `maxLength` checked per element and `minLength` at exit, a list
  value under `APPEND` becoming one element, and the size limit for inline and file values. Chunked: a selector
  required, forbidden, and outside the chunk (each invalid), `[012]` matching 12, `ONCE` per Task, the size limit per
  Task, a missing value for one Task failing `finish`, and a chunk of one Task still requiring the selector.
* Binding: pending detection directly and through a `let`; a consumer of a statically skipped producer, and a
  consumer of `TaskParam` only, both bound at creation, including when the consumer comes before the producer in the
  template; a static skip that never evaluates the Step's `let`; a dynamic skip; `fail(...)` at binding; an empty
  product space; empty with associated ranges; non-empty `INT` and `FLOAT` list-literal ranges with a list-valued item
  (`["{{Param.L}}"]`); a list-literal range that flattens to nothing; the 1024 cap lifted;
  `max_task_count` across bindings; and `TaskParam` alignment and iteration order under a
  `combination: "A * B"` with `B` varying fastest.
* Job creation: a script that reads `Steps.*` passes `create_job`'s own carried-forward checks.
* `PATH` outputs: `PATH` and `LIST[PATH]` declarations; `Output` of a `PATH` producer in a TEMPLATE-scope field is
  rejected with the `RawOutput` hint, and `RawOutput` there is accepted; the collector maps a reported path back,
  rejects a relative one and one with no round trip, and checks constraints on the stored form.
* Job outputs: each schema rule; a `value` reading a Step that isn't a dependency of anything is accepted; `Param` of
  a `PATH` type rejected and `RawParam` accepted; a path-typed `Steps.P.Output.X` rejected with the `RawOutput`
  hint and `RawOutput` accepted; evaluation at creation, and a failure there failing `create_job`;
  an output reading only `TaskParam` of a Step bound at creation (evaluated at creation), and one reading a
  statically skipped Step (evaluated at creation, `null`); `evaluate_job_output` for a later Step, a dynamically
  skipped Step (`null`), a zero-Task Step with `min([])` (error), a constraint violation, and a `null` result with a
  constraint (not checked). CLI: a run with a failing job output fails, and a run's success is reported after its
  last evaluable job output.

**openjd-sessions** (`tests/integration/test_control_flow.rs`):

* Values are returned on success and reported through the callback as they arrive, on stdout and on stderr.
  Nothing is returned on failure, cancellation, or timeout, or when the action exits 0 after an invalid directive.
* On a Windows worker with a POSIX submission path and a POSIX rule (or simulated with `to_symtab(Windows)` and
  `PathFormat::Windows`), a consumer's `RawOutput` keeps `/` and its `Output` is mapped. A script that reads only
  `Steps.P.RawOutput.X.name` gets a `path`, not a `string`.
* With path mapping rules, a `PATH` output is returned in the submission form; a consumer's script sees
  `Steps.P.Output.X` forward-mapped and `Steps.P.RawOutput.X` unmapped; a `LIST[PATH]` `TaskParam` of a `PATH`
  parameter is mapped element by element.
* Every collector failure surfaces as a failed Task with its message.
* Logging: with `echo_openjd_directives = false`, a valid directive is hidden and an invalid one stays in the log with
  its error; with the default, both appear.
* Cancel at once: an invalid directive followed by a long `sleep` cancels the Action well before the sleep ends,
  through its configured cancelation, with a message that names the output. A test that only checks the final state
  would also pass with fail-at-exit, so it must check the timing.
* Recognition: `openjd_output_var:N=1` with no space is a directive, and `openjd_output_var: ` with nothing after it
  fails the Task.
* `@` files: relative and absolute paths; a file written and closed before the directive; a symlink that stays
  inside the working directory succeeds; a file outside the working directory, a symlink out of it, a directory, and
  a missing file all fail; a file over the size limit fails without
  being read in full.
* The three treatments: a directive in `onRun` of a producer; in a Job Template `onEnter` and in the `onRun` of a Step
  without `outputVariables` (both fail); in an Environment Template action and in a Session without `CONTROL_FLOW`
  (ordinary output).
* A directive line over 64 KiB is invalid, on the direct path and through the helper, including a line under 64 KiB
  that grows past it when escaped.
* A directive line that is not valid UTF-8 is invalid, on both paths; a directive followed by a form feed is treated
  the same on both paths; an `@` file that is not UTF-8 or contains NUL fails the Task.
* An `@` FIFO fails the Task without hanging the reader, on both paths.
* Cross-user: an `@` file between 128 KiB and 1 MiB succeeds; an `@` directive in an Environment Template action is
  passed through unchanged and its file is not read; a directive in a Job Template `onEnter` cancels and fails the
  Action; an undeclared name with a missing `@` file reports the undeclared name.
* Cross-user parity: `@` directives with a space and with a tab after the colon, with `N= @f`, and with trailing
  spaces after the path give the same result as on the direct path. Run with the RFC samples' own form
  (`openjd_output_var: Keys=@keys.json`).
* WRAP_ACTIONS environment hooks: a directive from `onWrapEnvEnter` or `onWrapEnvExit` that wraps a Job Template
  Environment fails the Action, and one that wraps an Environment Template Environment is ordinary output.
* `LAST` with no directive fails the Task at exit.
* WRAP_ACTIONS: directives from `onWrapTaskRun` and from the wrapped process count for the Task, including when the
  wrapping Environment comes from an Environment Template.
* Cross-user (Docker, `scripts/run_cross_user_tests.sh`, and the Windows job): an `@` file that the job user can't
  read, including through a hard link into the working directory, fails the Task.

**openjd-cli golden tests**, one per RFC sample:

* Location: `crates/openjd-cli/tests/templates/control_flow/<sample>.yaml`, with
  `<sample>.expected.txt` beside it.
* Each sample is the RFC YAML with the application commands (`ffmpeg`, `boto3`, `bake-node`, ...) replaced by
  deterministic Python stubs that print a marker and write fixed outputs. The existing Python shim in `cli_tests.rs`
  supplies an interpreter.
* The test runs `openjd run`, normalizes timestamps and temporary paths, and compares the result with the expected
  file. `OPENJD_UPDATE_GOLDEN=1` rewrites the file. The helper is a few dozen lines in the test file, so the
  workspace gets no new dev-dependency such as `insta` (that would need `cargo deny`).
* Variants:
  * Video frame count: `FrameCount=3`, which gives 3 Tasks and a scalar `FrameRate` in a host-scope field.
  * Chunked render: the RFC's chunked `RenderTimeS[frame]` example with stub commands, `defaultTaskCount: 3`, and a
    variant that omits one frame's directive and fails its chunk.
  * Task metrics: a 2×1×1×1 space whose stub prints `frames_done` twice (`LAST`), three `chunk_error_mm` values
    (`APPEND`), and the three `ONCE` metrics, so the output shows each Task's values with their units. A second
    variant prints `mpjpe_mm` twice and fails the Task.
  * S3 fan-out: `Keys=[]`, which gives a zero-Task `Transcode` and a manifest with `totalBytes 0`, and a variant with
    two keys.
  * Sweep: a 2×1×1×2 space for a short runtime, run once with a winner and once taking the `fail(...)` path. The
    output shows `VideoCount`, `BestSingleTaskErrorMm`, and `ResultDirs` appearing after `Discover`, `Sweep`, and
    `RunWinner`, and a variant with path mapping rules shows `ResultDirs` in the submission form.
  * Scene graph: `openjd check` rejects `TASK_DEPENDENCIES` as unsupported. A variant without `dependsOnSubspace` runs
    the computed fan-out.
  * Render and QC: `EncodeVideo=false` (static skip, so `Publish` prints `frames only`), noise above and below the
    threshold (dynamic skip), and `BadFrames=[]` (zero Tasks).
  * Plus a `--step` run with `--step-inputs`, and a `--maximum-tasks` run that blocks the consumer.

**Conformance**: propose a `conformance-tests/2023-09/CONTROL_FLOW/` set in openjd-specifications, in the same format
as `EXPR/jobs/*.test.yaml`. It is a separate repo and a separate task, and it is not part of this plan's PRs.

**Python parity (Q7)**: not applicable until openjd-model-for-python implements the extension. The M3 parser and the
binding API are shaped so that a Python port can mirror them.

## RFC ambiguities surfaced

These are the questions the RFC needs to answer before an implementation can be called conformant. The prototype
picks a behavior for each and records it in the specs. A9, A10, and A12 were about the session-supplied output file,
and the RFC revision at `5a86c21` that replaced it with the directive settled them. A15 and A16 were settled in
`1e89748` and `61f27ee`. Resolved items are kept, struck through, so the numbering stays stable.

* **A1. The validation warning has no channel.** §Modifications to the Expression Language says that template
  validation "MUST emit a warning". No OpenJD specification, the conformance runner, nor openjd-rs has a warning
  concept. The RFC should define where warnings appear (the decode result, `openjd check` output) and how they are
  tested, or change MUST to SHOULD. Prototype: deferred.
* **A2. Empty computed ranges are already accepted without `CONTROL_FLOW`.** The RFC calls an empty list from a
  format-string range a relaxation that applies under `CONTROL_FLOW`. openjd-rs already accepts it with `EXPR` alone
  and runs zero Tasks. The EXPR specification should say whether that case is an error. If it is, openjd-rs has a bug,
  and fixing it breaks templates that run today. If it is not, the "relaxed rule" isn't new.
* **A3. Technical Requirement 1 conflicts with two relaxations.** TR1 says a template that lists `CONTROL_FLOW` but
  doesn't use a feature behaves exactly as today. Yet listing it lifts the 1024-element cap on computed ranges (§Range
  length), and, per the RFC's framing, it allows empty computed ranges. §Backward compatibility counts only "one
  existing rule" as relaxed, but the range-length change is a second one. Open Question 10 asks the related question;
  keying both relaxations on ranges that read `Steps.*` would satisfy TR1.
* **A4. Editorial: the Video example's `"1-0"` note.** The RFC already records that openjd-rs rejects `"1-0"`
  (§Empty parameter spaces, "some implementations reject it instead", and Open Question 11). The Video example's note
  says only that `"1-0"` is `[1]` under §3.4.1.1.1. It should mention the divergence too, because openjd-rs fails with
  `Descending range 1-0 requires a negative step`, and `minValue: 1` is the only guard that works everywhere.
* **A5. Task-count limits.** The RFC allows a per-Step limit checked at binding. openjd-rs and services already have a
  per-Job total (`CallerLimits::max_task_count`) that is checked at creation. The RFC should say whether a Job-total
  limit applies cumulatively at each binding, so that the binding that crosses it fails, and that pending Steps count
  as zero at creation.
* **A6. JSON numbers coerced to `STRING`.** The conversion rule makes any number outside `i64`, or with a fraction or
  exponent, a `float` before it is coerced. `S=12345678901234567890` then becomes `"1.2345678901234567e+19"` under EXPR coercion (checked with
  `ExprValue::coerce`), while the job-parameter path keeps the digits. The
  RFC doesn't say whether `S=1.50` gives `"1.50"` or `"1.5"`. Specify whether a `STRING` output keeps the written
  digits (openjd-rs `Float64` can carry an original spelling, but M3 notes the parsing cost), and whether an out-of-range number such as `1e400` is
  an error.
* **A7. Which coercion, and how lenient (Open Question 16).** The RFC makes two statements that don't agree in
  openjd-rs. It converts with EXPR's implicit coercion (§1.2.3), and it also says a `Steps.*` value behaves exactly
  like a `Param.*` value of the same type. But job-parameter values go through a separate, more lenient conversion:
  `B=1` is a valid `BOOL` job parameter (`parameters.rs:691`, and `openjd run -p '{"B": 1}'` accepts it), while EXPR
  coercion rejects it (`Cannot coerce int to bool`). Both paths accept `N="12"` for `INT` and `B="yes"`, `"on"`, or
  `"1"` for `BOOL`. The RFC's table shows none of these cases. The RFC should name one conversion and add these rows
  (or make output conversion stricter). The prototype uses EXPR coercion, which is the RFC's normative text.
* **A8. `enableIf` that is not a single whole-field expression.** The RFC doesn't say how
  `enableIf: "yes"` (no braces) or `enableIf: "{{Param.X}} "` (text around the expression) resolves. EXPR's
  string-to-bool coercion would accept the first and reject `"true "`. Require a YAML `bool` or a single whole-field
  expression, or state the coercion.
* ~~**A9. `Task.OutputFile` in TEMPLATE-scope action fields.**~~ Resolved: `Task.OutputFile` no longer exists.
* ~~**A10. The output file's integrity.**~~ Resolved: there is no session-supplied file. For `@` files, §File values
  now requires the opened file to be a regular file inside the Session working directory, checked on the handle, and
  read with no more access than the Action's user.
* **A11. Step `let` forces `PENDING_INPUTS`.** Step `let` is a materialization expression, so a Step that reads
  `Steps.P.*` only in a step `let` that only its script uses becomes pending. Moving `FramesToVideo`'s `FrameRate`
  into a step `let` is enough. Ordering is unaffected, but the reported state and `openjd summary`'s Task count
  change. Confirm that this is intended, or narrow the rule to `let` names that other materialization expressions
  read.
* ~~**A12. Editorial: `<BlankLine>`.**~~ Resolved: the line grammar that used it is gone.
* **A13. Nested-list fan-in and `repr_sh`.** A per-Task `LIST[T]` output from a multi-Task producer becomes
  `list[list[T]]` (§Reductions item 4), and `repr_sh` has no signature for nested lists, while `repr_py`,
  `repr_json`, and `flatten` do. A short note in §Reductions ("`flatten` first for shell use") would spare authors a
  validation error.
* **A14. Flattening of list-valued items in `INT` and `FLOAT` range lists.** §Empty parameter spaces says that after a
  list literal's items are evaluated, "EXPR flattens a list-valued item", which matches the Expression Language's list
  item rule (§1.3.2: target type `T? | list[T]`, and a list result "is flattened inline"). openjd-rs does this only for
  `STRING` and `PATH` ranges (`ranges.rs:500`–`:522`). An `INT` list literal accepts only integers:
  `range: ["{{Param.L}}"]` with a `LIST[INT]` parameter fails `openjd check` with
  `Cannot parse '{{Param.L}}' as integer`. A `FLOAT` list literal parses each item's text as one float
  (`ranges.rs:407`), so with a `LIST[FLOAT]` parameter it passes `check` and then fails `openjd run` with
  `Cannot parse '[1.5, 2.5]' as float`. This is mainly an openjd-rs gap against the EXPR spec (Q5), to be fixed before
  M4.6. The RFC should still say explicitly that range list items follow §1.3.2, because the §1.3.12 table types the
  `INT` range as `list[int | FormatString] | RangeString` and doesn't say that an item may be a list.
* ~~**A15. Is a directive line logged?**~~ Resolved in `1e89748`, refined in `61f27ee`: like the other messages, the
  line appears in the log by default; a runtime option to hide recognized messages may hide a valid directive, but
  an invalid one always stays in the log with its error. openjd-rs's `echo_openjd_directives` gives the first part,
  and S2 adds the second.
* ~~**A16. Cancel at once, or fail at exit?**~~ Resolved in `1e89748`: an invalid directive MUST cancel and fail its
  Action at once.
