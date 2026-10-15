# CONTROL_FLOW prototype plan for openjd-rs

Status: plan, not yet implemented. Target branch: `control-flow-prototype`.

This plan maps the draft `CONTROL_FLOW` extension onto the openjd-rs crates. Its inputs are on branch
`rfc-control-flow` of openjd-specifications:

* `rfcs/0010-control-flow.md`, the draft RFC (called "the RFC" below; `§` references are to its sections);
* `docs/control-flow-investigation/04-design-space.md`, the v1 scope;
* `docs/control-flow-investigation/03-runtime-semantics.md`, the runtime notes. Its §8 maps emission onto the
  `action_filter.rs` stdout pipeline, but that design was replaced by the session-supplied output file (04 T1, RFC
  Design Choice 2). This plan follows the RFC.

The goal is a prototype that runs the five RFC sample templates end to end with `openjd run`. The prototype also
tests whether the RFC can be implemented as written, and the questions it raises are listed in
[RFC ambiguities surfaced](#rfc-ambiguities-surfaced).

Every PR below goes through the `implement-change` skill. Each PR updates the crate specs in `specs/` and
`public-api.md` in the same change, as `AGENTS.md` requires.

## Summary

* **openjd-model** does most of the work. It gets the schema fields, the extension gate, the `Steps.*` scope in
  template validation, an output-file parser that sessions and schedulers can share, and a binding API. With that API,
  `create_job` can leave a Step unbound and a caller can bind it later from upstream values.
* **openjd-expr** needs no new types or functions. `ExprValue::Unresolved` already makes a value "unresolved until run
  time", and `accessed_symbols` already finds the references. The only expr change is an optional one: a per-segment
  static-type API for the `null` warning, which is deferred along with the warning (see A1).
* **openjd-sessions** creates the output file, exposes `Task.OutputFile` and `OPENJD_TASK_OUTPUT_FILE`, reads and
  validates the file after a successful `onRun` using the model parser, and returns the values in `ActionResult`. The
  stdout filter does not change.
* **openjd-cli** `openjd run` already runs Steps in topological order in one Session. It becomes a scheduler
  simulator: it binds each pending Step before running it, skips disabled Steps and Environments, and keeps outputs in
  memory. `openjd summary` reports pending Steps.
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
| Per-Task iteration with chunking disabled already exists | `StepParameterSpaceIterator::new_with_chunk_override`, `crates/openjd-model/src/job/step_param_space.rs:1298` |
| JSON to typed job-parameter values, with element-wise coercion | `coerce_json_to_job_parameter_type`, `crates/openjd-model/src/job/create_job/parameters.rs:1325` |
| Job-parameter constraint checks (`minValue`, `maxLength`, `item`, ...) | `JobParameterDefinition::check_constraints`, `crates/openjd-model/src/template/parameters.rs:496` |
| Symbol references of a format string or expression | `FormatString::accessed_symbols` (`crates/openjd-expr/src/format_string.rs:578`), `ParsedExpression::accessed_symbols` (`crates/openjd-expr/src/eval/parse.rs:35`) |
| `run_task` takes a `job::StepScript`, not a `job::Step`, and returns `ActionResult { state, exit_code, stdout }` | `crates/openjd-sessions/src/session.rs:1758`, `crates/openjd-sessions/src/action.rs:74` |
| Per-action environment variables are computed in `run_task` | `crates/openjd-sessions/src/session.rs:1785`, `evaluate_env_vars` at `:2393` |
| Cross-user file ownership for files the job user must access | `chown_for_user`, `crates/openjd-sessions/src/embedded_files.rs:102` |
| `openjd run` runs every Step in topological order in one Session, and stops running Tasks after the first failure | `crates/openjd-cli/src/run/execution.rs:202`, `:341`, `:378` |
| No crate and no OpenJD spec has a validation-warning channel | `grep -ri warn` over `openjd-model/src` and `openjd-cli/src`, and `grep -i warning` over the 2023-09 schema and EXPR wiki pages, find nothing |
| `repr_sh` has no signature for nested lists (`list[list[T]]`); `repr_py`, `repr_json`, `string`, and `flatten` do | `openjd check` probes |

## openjd-model

### M1. Extension and schema

* Add `ModelExtension::ControlFlow` (`"CONTROL_FLOW"`) to the enum, `ALL`, `as_str`, and `FromStr`
  (`types.rs:356`). Because the CLI takes `ALL` as its default, this also turns it on in `openjd check`, `run`, and
  `summary`.
* `template::StepTemplate` (`template/step.rs:233`) gets
  `enable_if: Option<BoolOrFormatString>` and `output_variables: Option<Vec<OutputVariableDefinition>>`.
  `template::Environment` (`template/environment.rs:17`) gets `enable_if`.
* `OutputVariableDefinition`: reuse the job-parameter machinery instead of a parallel set of structs. Deserialize the
  same discriminated union as `JobParameterDefinition`, reject `PATH`, `LIST[PATH]`, `default`, and `userInterface`
  in validation, and wrap the result in a newtype. The wrapper exposes `name`, `job_param_type`, and
  `check_constraints`. Two cautions:
  * The RFC says that only declared constraint properties apply (`<OutputVariableDefinition>` item 4). Any default
    that a job-parameter check implies when a property is omitted must be turned off for outputs. Test that a
    `RANGE_EXPR` output longer than 1024 characters is accepted when it declares no `maxLength`.
  * Error paths must say `outputVariables[i]`, not `parameterDefinitions[i]`.
* `job::Step` (`job/mod.rs:105`) gets `output_variables: Option<Vec<job::OutputVariable>>` and
  `binding: StepBinding`. `job::Step` derives `PartialEq`, `Hash`, `Serialize`, and `Deserialize`
  (`job/mod.rs:103`), and `JobParameterDefinition` implements none of the first three, so `job::OutputVariable`
  cannot hold the M1 wrapper. It is its own struct: a name, a `JobParameterType`, and an `OutputConstraints` value
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

1. Without the extension, `outputVariables` and `enableIf` are errors at their own paths. A `Steps.*` or
   `Task.OutputFile` reference already fails as an unknown symbol, because nothing seeds it.
2. With `CONTROL_FLOW`, `EXPR` is required (reuse `check_expr_prerequisite`). `CONTROL_FLOW` in an Environment Template
   is an error, and so is `enableIf` in one.
3. `outputVariables`: 1–32 elements, unique names, an allowed type, valid constraints, no `LIST[LIST[INT]]` on a Step
   with a `parameterSpace`, and no outputs on a Step with a `CHUNK[INT]` parameter.

The scope rules go into pass 8 (`format_strings.rs`), because that pass already builds one symbol table per field
family:

* Add a helper `add_steps_scope(symtab, jt, step_index, ScopeKind)`. For each direct whole-Step dependency `P` of the
  Step whose name is an `<Identifier>`, it seeds `Steps.P.Output.<V>` and `Steps.P.TaskParam.<N>` as
  `ExprValue::unresolved(T)`, typed by the RFC's table in §The `Steps.*` scope. A producer with `enableIf` gets `T?`,
  and a producer with a `parameterSpace` gets `list[T]`. `TaskParam.<N>` for a `PATH` parameter is seeded only in
  host scopes. Call the helper from the Step's `enableIf`, `let`, `hostRequirements`, `range`, step-environment, and
  script symbol tables. Never call it for the job name, `jobEnvironments`, `combination`, or Environment Templates.
* An unknown symbol under the `Steps.` prefix gets a specific message instead of EXPR's generic one. Scan
  `accessed_symbols()` for the `Steps.` prefix and compare against the template, with messages for each case: no such
  Step; a Step name that is not an `<Identifier>`; the Step is not a direct dependency ("add `dependsOn: P`"); a
  self-reference; no such output; no such Task parameter; a `PATH` `TaskParam` in a TEMPLATE-scope field. Because the
  check is a static scan, it also catches a reference inside a branch that is never evaluated.
* `Task.OutputFile` (type `path`) is seeded only in the task-scope symbol table of a Step that declares
  `outputVariables` (`build_task_scope_symtab`, `format_strings.rs:169`).
* `enableIf` is validated with target type `bool`, not `bool?`. Its symbol table has no `let` names, no `Task.*`, and
  no `Session.*`.
* The step-graph cycle check (`structure.rs:1118`) is unchanged. Dependency names are already checked at
  `structure.rs:217`.

The RFC's validation warning for `null` in a discarding position (§Modifications to the Expression Language) is
deferred in the prototype. openjd-rs has no warning channel, and adding one means a public API decision in
`decode_job_template` and output from `openjd check` (ambiguity A1). The prototype records the gap in
`specs/model/validation.md`.

### M3. Output file parser

Add `openjd_model::outputs` with this API:

```rust
pub fn parse_task_output_file(
    bytes: &[u8],
    declared: &[job::OutputVariable],
) -> Result<IndexMap<String, ExprValue>, OutputFileError>;
```

It is a pure function, so sessions, the CLI, `openjd-for-js`, and a scheduler that re-validates can all share it.

* The grammar is the one in §Grammar: an optional BOM, `\n` or `\r\n` line endings, and blank lines made of
  spaces or tabs. No whitespace is allowed before the `=`. Each value is one JSON value, parsed with `serde_json`.
* The checks are the seven rules of §Reading the output file, in order. Rules 1–3 apply to every line. Rules 4–5
  apply to the last line for each variable. Rule 6 covers declared variables with no line. Rule 7, the size limit, is
  enforced by the caller before it reads the bytes. Messages give the 1-based line number and the variable name.
  Messages never repeat raw line content beyond the variable name; see S3 for why.
* Type conversion: type each JSON value against the declared type, then coerce with EXPR's implicit coercion,
  `ExprValue::coerce` (`value.rs:904`), as the RFC's §Type conversion requires. The RFC infers an array "as a list
  literal with the declared type as its target" (§1.2.6), so arrays are converted element by element toward the
  declared element type. `json_to_expr_value` (`parameters.rs:1381`) can't be reused as is: it builds lists with a
  `NULLTYPE` hint (`parameters.rs:1406`), so the RFC row `LIST[STRING] L=[1, "a"]` fails with
  `Invalid list value: make_list expected int element, got string`. Reuse its scalar arms. It is `pub(super)`, so move
  it to a module that both `create_job` and `outputs` can use. Keeping a number's written spelling (A6) is not free:
  the workspace's `serde_json` is built without `arbitrary_precision`, so `1.50` is already `1.5` after parsing. If
  A6 is answered "keep the digits", either enable `arbitrary_precision` (a workspace-wide change, through feature
  unification) or lex the number tokens of the line separately. Do not reuse `coerce_json_to_job_parameter_type`
  either: it
  follows job-parameter rules, which are more lenient (for example `1` is accepted as a `BOOL` at
  `parameters.rs:691`). Ambiguity A7 asks the RFC which of the two it means. Before converting, reject `null` and
  objects at any depth, and nesting deeper than the declared type. Each row of the RFC's conversion table becomes a
  unit test.
* Constraints: `job::OutputVariable::check_constraints` from M1.

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
   (`instantiate.rs:277`) and `build_env_check_symtab` (`:359`). Those tables must seed the new symbols too:
   `Task.OutputFile` as unresolved, and each `Steps.*` symbol as concrete when it is known and unresolved otherwise.
   Without that, every producer script fails `create_job` with `Undefined variable: 'Task.OutputFile'`, and so does
   every script that reads `Steps.*`. Seed `Task.OutputFile` in `build_task_check_symtab` only, and only when the
   Step declares `outputVariables`. Seed the `Steps.*` placeholders in both builders. Do not put them in
   `add_unresolved_session_symbols` (`instantiate.rs:224`): it takes no Step, and it feeds the Environment tables too,
   where `Task.OutputFile` must not appear. Do not put unresolved placeholders in the Step symbol table either,
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
   one element per Task even for a `CHUNK[INT]` parameter. A skipped producer makes every symbol `null`. A `Bound` producer contributes `TaskParam` only. The list type hint follows the declared type,
   so `[]` becomes a typed empty list.
5. **Host-scope `Steps.*` in Steps bound at creation.** A Step such as `FramesToVideo` reads `Steps.*` only in its
   script, so it binds at job creation but needs the values before its Tasks run. Add
   `pub fn attach_step_inputs(step: &job::Step, inputs: &StepInputs) -> Result<job::Step, ModelError>`. It merges the
   concrete `Steps.*` values that the Step reads into `resolved_symtab`. It has to find those symbols itself, because
   `filter_symtab_for_step` (`instantiate.rs:909`) runs only inside `instantiate_step`, where the symbol table has no
   `Steps.*` entries to keep (`copy_symbol_value` skips a missing symbol, `format_string.rs:612`). Reuse
   `collect_all_accessed_symbols` (`instantiate.rs:1014`) and copy the matching values from `inputs`. Expose the list of producers whose
   values a Step needs before dispatch as `job::Step::input_producers()`.
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
   limit fails, naming it. Also add an optional `CallerLimits::max_step_task_count`, the per-Step limit from §Limits item 3.
   See A5.
8. **`StepDependencyGraph`** (`job/step_dependency_graph.rs`) is unchanged. The step graph is fixed.

### Model specs to update

`specs/model/template-types.md`, `validation.md`, `job-creation.md`, `job-types.md`, `parameter-space.md`,
`step-dependencies.md` (skipped and zero-Task dependencies), and `public-api.md`. Also update
`specs/architecture.md` with the binding stage.

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

The expr specs need no change unless the warning lands.

## openjd-sessions

### S1. API

* `run_task` (`session.rs:1758`) needs the Step's output declarations. Add a parameter
  `output_variables: Option<&[openjd_model::job::OutputVariable]>`. The crate is pre-1.0, so this change is
  allowed. A `RunTaskOptions` struct is an alternative if the signature keeps growing.
* `ActionResult` (`action.rs:74`) gets `outputs: Option<IndexMap<String, ExprValue>>`. The value is `Some` only when
  the Step declares outputs and the Task succeeded.
* `SessionLimits` (`limits.rs:23`) gets `max_task_output_file_bytes: Option<usize>`, with a default of at least
  1 MiB and a `From<&CallerLimits>` mapping.

### S2. Before `onRun`

When `output_variables` is `Some`:

1. Create an empty file `<working_dir>/.openjd/task_outputs/<uuid>.txt`, which is inside the Session working
   directory, as §The output file recommends. The runtime creates `.openjd/task_outputs` itself when the Session starts, before any job-user action runs. It
   fails if the path already exists, keeps the directory unwritable by the job user, and checks its owner and mode
   before each use. Under cross-user execution the working directory itself is group-writable (mode `0o770`,
   `tempdir.rs:174`–`:186`), so a
   writable subdirectory would let the job user swap a path component. Give the job user write access to the file only,
   with `chown_for_user` (`embedded_files.rs:102`) on Unix and the `win32_permissions` ACL helper on Windows, as
   embedded files are handled today.
2. Seed `Task.OutputFile` as a `path` in `action_symtab` before the WRAP_ACTIONS block (`action_symtab` is built
   at `session.rs:1786`). The
   wrapped inner scope (`build_wrapped_inner_scope`) and `WrappedAction.*` then see the same path.
3. Add `OPENJD_TASK_OUTPUT_FILE` to the `env_vars` that `run_task` builds (`session.rs:1785`). The variable is per
   action, so environment actions never get it. Under WRAP_ACTIONS, `onWrapTaskRun` replaces `onRun` and so receives
   it.

### S3. After `onRun`

* If the state is not `Success`, delete the file unread and return `outputs: None`. This covers failure,
  cancellation, and timeout.
* If the state is `Success`, open the file relative to a handle on the directory created in S2, without following
  symlinks (`openat` with `O_NOFOLLOW` on Unix, and a reparse-point check on Windows). Require a regular file with one
  link, which rejects hardlinks. A missing file is treated as empty. Read at most `limit + 1` bytes,
  then call `openjd_model::outputs::parse_task_output_file`. On error, log the message and return
  `state: Failed`, with the same logging as an `openjd_fail` message. The runtime can read with more privilege than
  the job user, so a symlink or a message that echoes content could disclose another file. That is the reason M3
  keeps messages free of raw content. See ambiguity A10.
* Delete the file after reading it. The runtime owns its location.
* The stdout filter (`action_filter.rs`) does not change. The RFC says no stdout line sets an output (§Stdout
  messages), and a test pins that `openjd_output_var: X=1` is plain output.

### S4. Steps.* values at Task run time

They arrive in `resolved_symtab` (M4 item 5), so `build_symbol_table` (`session.rs:2461`) needs no change. A `PATH`
`Steps.P.TaskParam.*` would need path mapping in the session. None of the samples use one, so the prototype rejects
it in validation with "not yet supported by this implementation" and records it as a known gap.

### Sessions specs to update

`specs/sessions/session.md` (output file lifecycle), `public-api.md`, and `action-messages.md` (a note that there is
no output directive).

## openjd-cli

### C1. `openjd run` as a scheduler simulator

Today `run_workload` (`execution.rs:341`) iterates `steps_to_run` in topological order and calls `execute_step`. The
prototype keeps that loop and adds a small in-memory scheduler state:

```rust
struct StepRecord { state: SimStepState, task_outputs: Vec<IndexMap<String, ExprValue>> }
enum SimStepState { Pending, Bound, Skipped, Succeeded, Failed, Blocked }
```

For each Step in topological order:

1. If a dependency is `Failed` or `Blocked`, mark the Step `Blocked` and print why. The current stop-on-first-failure
   behavior (`ctx.session_failed`) already prevents it from running.
2. Build `StepInputs` from the records of its direct dependencies with `StepInputs::add_producer`.
3. If the Step is `PendingInputs`, call `bind_step`. On error, print the binding failure (field and expression) and
   mark the Step `Failed`. If it is `Skipped`, print `Step 'X' skipped (enableIf is false)`.
4. Otherwise call `attach_step_inputs`, enter only the enabled step Environments, and run the Tasks as today. Record
   `ActionResult.outputs` for each Task index, and print them after each Task (`Output FrameCount = 1440`).
5. A Step with zero Tasks prints `Step 'X' has 0 tasks` and succeeds.

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
  `{name, state, task_count, outputs: [ {<var>: <value>} ... ]}`.

### C2. `openjd summary` and `openjd check`

* `summary` (`summary.rs:113`) prints a pending Step as `Tasks: determined at run time from
  Steps.VideoToFrames.Output.FrameCount`, which follows §Implementation strategy. The total becomes
  `Total tasks: N + (determined at run time)`. A statically skipped Step is shown as `SKIPPED`.
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
| 1 | `feat(model): CONTROL_FLOW extension, outputVariables and enableIf schema` | — | M1, and M2 items 1–3 (gating, the EXPR prerequisite, structural rules) |
| 2 | `feat(model): Steps.* and Task.OutputFile scopes in template validation` | 1 | the M2 scope rules and their messages, `enableIf` typing, and the `create_job` check-table seeding from M4.3. Until PR 4, `openjd run` of a template that reads `Steps.*` fails when a Task starts, because the value is unresolved; this is acceptable on the prototype branch |
| 3 | `feat(model): task output file parser` | 1 | M3 |
| 4 | `feat(model): step input binding` | 2, 3 | M4: `StepBinding`, static `enableIf`, `bind_step`, `StepInputs`, `attach_step_inputs`, range rules, task-count limits |
| 5 | `feat(sessions): task output file` | 3 | S1–S4 |
| 6 | `feat(cli): simulate step input binding in openjd run` | 4, 5 | C1, C2 |
| 7 | `test(cli): golden tests for the RFC sample templates` | 6 | the sample tests below |

PRs 3 and 5 can proceed in parallel with 2 and 4. PR 7 can start early as a list of failing tests, and the tests turn
on as PR 6 lands.

## Test strategy

The tests follow the repo's standard. Validation failures assert the path and the full message, and expression
failures assert the message, the expression, and the caret (`AGENTS.md`, Test Quality Standard).

**openjd-model** (`tests/integration/test_control_flow_*.rs`):

* Gating: each new field without the extension, `CONTROL_FLOW` without `EXPR`, and `CONTROL_FLOW` in an Environment
  Template.
* Structure: each `outputVariables` rule, including `LIST[LIST[INT]]` on a multi-Task Step, `CHUNK[INT]`, the 32-item
  cap, and a `PATH` type.
* Scope: one test per `Steps.*` error message (M2), one per forbidden field family, a reference in a branch that is
  never taken, a reference through a `let`, and `Task.OutputFile` outside a producer script.
* Types: scalar, list, and optional shapes, checked as `ExprType` on the seeded symbol, and `enableIf` given a `bool?`
  expression.
* Parser: every row of the RFC's conversion table, each of the seven read rules, BOM, CRLF, a final line without a
  newline, last write wins, and whitespace-only lines.
* Binding: pending detection directly and through a `let`; a consumer of a statically skipped producer, and a
  consumer of `TaskParam` only, both bound at creation, including when the consumer comes before the producer in the
  template; a static skip that never evaluates the Step's `let`; a dynamic skip; `fail(...)` at binding; an empty
  product space; empty with associated ranges; a list-literal range that flattens to nothing; the 1024 cap lifted;
  `max_task_count` across bindings; and `TaskParam` alignment and iteration order under a
  `combination: "A * B"` with `B` varying fastest.
* Job creation: a producer script that uses `Task.OutputFile`, and a script that reads `Steps.*`, both pass
  `create_job`'s own carried-forward checks.

**openjd-sessions** (`tests/integration/test_control_flow.rs`):

* The file exists before `onRun` and is writable, with matching `OPENJD_TASK_OUTPUT_FILE` and `Task.OutputFile`.
* Values are returned on success, and the file is discarded on failure, cancellation, and timeout.
* Every parser failure surfaces as a failed Task with its message. The size limit is enforced, and a symlink is
  refused.
* `openjd_output_var:` on stdout is ignored.
* WRAP_ACTIONS forwarding, through a wrap hook that runs the inner command.
* The cross-user Docker tests (`scripts/run_cross_user_tests.sh`) and the Windows cross-user job cover the file's
  permissions.

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
  * S3 fan-out: `Keys=[]`, which gives a zero-Task `Transcode` and a manifest with `totalBytes 0`, and a variant with
    two keys.
  * Sweep: a 2×1×1×2 space for a short runtime, run once with a winner and once taking the `fail(...)` path.
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
picks a behavior for each and records it in the specs.

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
* **A9. `Task.OutputFile` in TEMPLATE-scope action fields.** §The `Steps.*` scope makes `Task.OutputFile` available
  in `<StepScript>` *actions*. But `timeout` and `cancelation` are materialization expressions (§Materialization
  expressions item 7), which are evaluated before any Task exists. Limit it to host-scope fields: `command`, `args`,
  embedded-file `data`, and the script `let`.
* **A10. The output file's integrity.** The RFC doesn't say what happens if the Task deletes the file or replaces it
  with a symlink or a directory, or what permissions it has under cross-user execution. The runtime may read with more
  privilege than the job user, so following a symlink, or a failure message that quotes the file, could disclose
  another file. Specify: a missing file reads as empty, a non-regular file fails the Task, and failure messages don't
  quote content.
* **A11. Step `let` forces `PENDING_INPUTS`.** Step `let` is a materialization expression, so a Step that reads
  `Steps.P.*` only in a step `let` that only its script uses becomes pending. Moving `FramesToVideo`'s `FrameRate`
  into a step `let` is enough. Ordering is unaffected, but the reported state and `openjd summary`'s Task count
  change. Confirm that this is intended, or narrow the rule to `let` names that other materialization expressions
  read.
* **A12. Editorial: `<BlankLine>`.** Read rule 2 refers to `<BlankLine>`, which the grammar doesn't define. The
  grammar's blank line is the `<WS>*` alternative of `<Line>`.
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
