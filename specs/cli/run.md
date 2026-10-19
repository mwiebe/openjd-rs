# run Command

## Purpose

`openjd run <path>` executes a job template locally in an OpenJD session. It is the
most complex command in the CLI, orchestrating the full session lifecycle: template parsing,
parameter resolution, job creation, environment enter/exit, task iteration with adaptive
chunking, embedded file materialization, let binding evaluation, and structured result output.

## Interface

```
openjd run <PATH> [--step <STEP>] [-p <KEY=VALUE>]... [-t <KEY=VALUE>]...
    [--tasks <JSON|file://path>] [--maximum-tasks <N>]
    [--environment <PATH>]... [--path-mapping-rules <JSON|file://path>]
    [--run-dependencies] [--no-run-dependencies] [--step-inputs <JSON|file://path>]
    [--extensions <EXT>] [--preserve] [--verbose]
    [--timestamp-format <FMT>] [--output <FMT>]
```

### Arguments

| Argument | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `path` | `PathBuf` | Yes | — | Path to the job template file |
| `--step` | `Option<String>` | No | — | Step to run (auto-selects if single step; runs all if omitted with multi-step job) |
| `-p`, `--parameter` | `Vec<String>` | No | — | Job parameters |
| `-t`, `--task-param` | `Vec<String>` | No | — | Explicit task parameter values for a single task |
| `--tasks` | `Option<String>` | No | — | JSON array or file of task parameter sets |
| `--maximum-tasks` | `i64` | No | `-1` | Max tasks to run (-1 = all) |
| `--environment` | `Vec<PathBuf>` | No | — | Additional environment template files |
| `--path-mapping-rules` | `Option<String>` | No | — | Path mapping rules (JSON or file://) |
| `--run-dependencies` | `bool` | No | `false` | Run transitive dependency steps first |
| `--no-run-dependencies` | `bool` | No | — | Explicit opt-out (default behavior) |
| `--step-inputs` | `Option<String>` | No | — | `CONTROL_FLOW`: the `Steps.*` output values a Step run alone reads (JSON or file://) |
| `--extensions` | `Option<String>` | No | all | Comma-separated extension names |
| `--preserve` | `bool` | No | `false` | Keep session working directory after completion |
| `--verbose` | `bool` | No | `false` | Enable DEBUG-level logging |
| `--timestamp-format` | `String` | No | `relative` | Log timestamp format: `relative`, `local`, `utc` |
| `--output` | `String` | No | `human-readable` | Output format: `human-readable`, `json`, `yaml` |

### Mutual Exclusivity

Three task selection modes are mutually exclusive (enforced by clap `conflicts_with_all`):

- `--task-param` — Run a single task with explicit parameter values
- `--tasks` — Run specific tasks from a JSON array
- `--maximum-tasks` — Run up to N tasks from the parameter space

### RunArgs Struct

```rust
#[derive(Args)]
pub struct RunArgs {
    pub path: PathBuf,
    #[arg(long)]
    pub step: Option<String>,
    #[arg(short = 'p', long = "parameter")]
    pub parameters: Vec<String>,
    #[arg(long = "task-param", short = 't', action = clap::ArgAction::Append,
          conflicts_with_all = ["tasks", "maximum_tasks"])]
    pub task_params: Vec<String>,
    #[arg(long = "tasks", conflicts_with_all = ["task_params", "maximum_tasks"])]
    pub tasks: Option<String>,
    #[arg(long = "environment", alias = "env")]
    pub environments: Vec<PathBuf>,
    #[arg(long = "path-mapping-rules")]
    pub path_mapping_rules: Option<String>,
    #[arg(long = "step-inputs")]
    pub step_inputs: Option<String>,
    #[arg(long = "run-dependencies")]
    pub run_dependencies: bool,
    #[arg(long = "no-run-dependencies")]
    pub no_run_dependencies: bool,
    #[arg(long = "maximum-tasks", default_value = "-1")]
    pub maximum_tasks: i64,
    #[arg(long = "extensions")]
    pub extensions: Option<String>,
    #[arg(long)]
    pub preserve: bool,
    #[arg(long)]
    pub verbose: bool,
    #[arg(long = "timestamp-format", value_parser = ["relative", "local", "utc"],
          default_value = "relative")]
    pub timestamp_format: String,
    #[arg(long = "output", value_parser = ["human-readable", "json", "yaml"],
          default_value = "human-readable")]
    pub output: String,
}
```

## Execution Pipeline

The async `execute()` entry point delegates to cohesive execution phases. A
`RunContext` owns the `Session`, entered-environment ledger, failure state,
interruption flag, task count, and session start time.

```
execute(args).await
  │
  ├── 1. SETUP
  │   ├── Enable verbose logging if --verbose
  │   ├── Initialize SESSION_START and TIMESTAMP_FORMAT globals
  │   ├── Read and parse job template
  │   ├── Read and parse environment templates (--environment)
  │   ├── Parse CLI parameters → input_values
  │   ├── Load path mapping rules
  │   ├── preprocess_job_parameters() → param_values
  │   └── create_job() → Job
  │
  ├── 2. SELECTION AND PREFLIGHT
  │   ├── Resolve step selection (explicit / auto-select / all)
  │   ├── Parse explicit task params and require a parameter space
  │   ├── Determine step execution order
  │   ├── Create the scheduler simulator; for a Step run alone, give it the
  │   │   --step-inputs values and check that every Steps.* value it reads is concrete
  │   └── Validate RFC 0008's single-wrap-layer rule for every selected stack
  │       (disabled Environments do not count)
  │
  ├── 3. SESSION CREATION
  │   ├── Create SessionConfig from parameters, path rules, and model profile
  │   └── Session::with_config()
  │
  ├── 4. ENVIRONMENT ENTRY
  │   ├── Print what job creation decided (CONTROL_FLOW), and settle
  │   ├── Enter environment template environments (--environment files)
  │   └── Enter enabled job environments (from template's jobEnvironments)
  │
  ├── 5. STEP EXECUTION
  │   └── For each step:
  │       ├── Begin it: block it, advance it if pending, skip it, or attach
  │       │   its Steps.* inputs
  │       ├── Enter enabled step environments
  │       ├── Execute tasks (explicit, lazy iteration, or no-param single task),
  │       │   recording each Task's output values
  │       ├── Exit step environments (reverse order)
  │       └── Record it succeeded or failed, then settle: advance pending Steps
  │           and evaluate job outputs
  │
  ├── 6. ENVIRONMENT EXIT
  │   └── Exit every entered environment (LIFO), including any whose
  │       enter action failed (cleanup guarantee)
  │
  └── 7. RESULTS
      ├── List the job outputs not evaluated (CONTROL_FLOW)
      ├── Treat any observed interruption as a failed run
      ├── Capture run duration (before filesystem cleanup)
      ├── Print session summary (format depends on --output)
      ├── Cleanup session (unless --preserve)
      └── Exit with code 1 if any action failed
```

The environment ledger is manipulated only through `RunContext` methods.
Each step unwinds to its pre-step baseline before propagating an error, and
the outer execution phase always unwinds the remaining environments. Thus a
`Session::run_task()` error cannot bypass environment cleanup.

Selection and RFC 0008 preflight happen before session creation. Invalid runs
therefore do not create a working directory or print session-start banners.

### CONTROL_FLOW: the scheduler simulator

`CONTROL_FLOW` (RFC 0010) is enabled by default with every other extension. For a Job
whose template lists it, `run` acts as a scheduler would, in a small in-memory simulator
(`run/simulator.rs`, plan C1). A Job without it has only bound Steps and no job outputs,
so the simulator only records what ran, and the run's output is unchanged.

**State.** The simulator keeps its own copy of the `job::Job`, a `StepRecord` per Step
(`SimStepState`: `Pending`, `Bound`, `Skipped`, `Succeeded`, `Failed`, `Blocked`; each
Task's outputs; why it failed or is blocked), and one `StepInputs` holding the
concrete `Steps.*` values so far of the Steps that something reads: a skipped Step's
`null`s, the outputs (and `TaskParam` lists) of a Step that succeeded with all of its
Tasks, and the `TaskParam` lists of a Step whose Tasks are known. Those last lists are
built only when something that reads the Step is advanced, attached, or evaluated, so a
large parameter space is not enumerated for nothing; until then the simulator counts them
as concrete. A pending Step is advanced again only once one of its producers has changed
state since its last advance (create_job already evaluated everything evaluable with the
values concrete at creation), so a settle costs nothing for Steps nothing new can bind.

**Before the first Step,** `run` prints, for the Steps this run runs, the job outputs `create_job` evaluated
(`Job output Label = "x"`), each Step it skipped (`Step 'S' skipped (enableIf is
false)`), and, for each pending Step, what is known of it (`common::binding_lines`; see
[summary.md](summary.md)).

**Settling.** After each Step succeeds or is skipped (and once before the first Step), the
simulator advances every pending Step of the run with `advance_step_binding`, in
dependency order, repeating until a pass makes no Step's Tasks known and skips none,
since a Step whose Tasks become known makes its consumers' `TaskParam` inputs concrete.
Each fact a call adds is printed: `Step 'C': enabled (enableIf is true)`, `Step 'C':
Tasks known (3 tasks)`, `Step 'C': host requirements known` (for a Step that has host
requirements), `Step 'C' is ready`, or `Step 'C' skipped (enableIf is false)`. A group
that fails prints the `BindingError` (`ERROR: Step 'C' failed in its Task enumeration
group:` and the located message), marks the Step `Failed`, and stops the run before any
more Tasks start, as a failed Task does; the job outputs whose inputs are concrete by then
are still evaluated first. This is the RFC's early failure: a Step whose
parameter space fails fails as soon as the producer it reads finishes, before a later
producer that only its script reads has run. Then every job output not yet evaluated
whose inputs (`job_output_inputs`) are all concrete is evaluated with
`evaluate_job_output` and printed once (`Job output Total = 30 px`, or `= null (no
value)`). An output evaluated from a Step's `TaskParam` stays committed if that Step later
fails. A job output that fails prints its error and fails the run, as a failed Task does;
the other job outputs whose inputs are concrete are still evaluated.
A caller error from the model (`is_caller_error`, `ModelError::Compatibility`) is a bug in
the simulator and is reported as an internal error.

**Each Step,** in the run's order:

1. A `Failed` Step does not run. A Step whose dependency in this run failed or is blocked
   becomes `Blocked` and prints `Step 'X' is blocked: its dependency 'P' failed` (or
   `is blocked`). A skipped Step behind a failure stays `SKIPPED`, which is terminal
   (RFC 0010 §Step states), but never satisfies `dependsOn` (§Conditional execution,
   Failure is not skipping): it prints `Step 'S' is skipped and never satisfies dependsOn:
   its dependency 'P' failed`, keeps that as its reason, and its dependents are blocked
   with `its dependency 'S' is skipped but never satisfies dependsOn`. Since a failed Task
   stops the run, this is reached for a Step after a failed Step only at the end of the
   run (below).
2. A `Pending` Step is advanced. Its dependencies have finished, so it binds, is skipped,
   or fails. A Step still pending then is an internal error, unless it reads the outputs
   of a Step that did not run all of its Tasks (below).
3. A `Skipped` Step prints `Step 'X' is skipped; it runs no Tasks` and runs nothing.
4. A bound Step that reads `Steps.*` gets `attach_step_inputs`. Only its Step
   Environments with `enabled` are entered; each other prints `Step Environment 'E' is
   disabled by its enableIf; not entering it`. Its Tasks run as before, and each
   successful Action's `ActionResult.outputs` is recorded per Task, in the Step's
   per-Task iteration order, through the model's `ChunkTaskIndex` (a chunk's outputs are
   keyed by `CHUNK[INT]` value). Each value is printed with its unit after its Action
   (`Output N = 3 frames`, or `Output Done[2] = 20` in a chunked Step). Outputs are kept
   sparsely, and only for a Step that declares output variables; an unchunked space run in
   order is placed by position, and a chunked one or Tasks given with `--tasks` or
   `--task-param` through the lazy `ChunkTaskIndex`, so a large parameter space costs
   nothing for the Tasks that do not run. A Step with zero Tasks prints `Step 'X'
   has 0 tasks` (only for a `CONTROL_FLOW` template) and succeeds.
5. The Step is recorded `Failed` (reason `a Task failed`, unless a group failed) if the
   run failed while it ran, and `Succeeded` otherwise; then the simulator settles.

Job Environments with `enabled == false` are not entered either (`Job Environment 'E' is
disabled by its enableIf; not entering it`).

**Incomplete producers.** `--maximum-tasks`, `--tasks`, and `--task-param` can leave a
Step with Tasks that did not run. It is recorded `Succeeded` but its outputs are not given
to anything (RFC 0010 §Reductions item 3). A Step that reads them becomes `Blocked` with
`inputs from Step 'P' are incomplete because not all of its Tasks ran`, and a job output
that reads them is not evaluated. This does not fail the run. A dependent that reads none
of them runs as before.

**A Step run alone.** `--step X` without `--run-dependencies` runs no other Step, so the
`Steps.*` values X reads from its producers must come from `--step-inputs`
(`file://inputs.yaml`, YAML unless the file ends in `.json`, or inline JSON): a map from
`Steps.P.Output.V` to a value, which for a producer with a `parameterSpace` is a list with
one value per Task. Each value is checked against the output's declared type and
constraints with the collector that checks reported values (`OutputCollector`), and every
declared `ONCE` or `LAST` output of the producer must be given. Before the Session starts,
the simulator checks that every `Steps.*` value X reads is then concrete (its groups'
inputs if it is pending, and the symbols its script and Step Environments read,
`job::Step::input_symbols`), and otherwise fails naming them: `Step 'C' reads Steps.* values of Steps this run does not run:
Steps.P.Output.V.` followed by `Give them with --step-inputs, or run Step 'C' with
--run-dependencies.` A producer's `TaskParam` comes from its parameter space, so a
`Steps.P.TaskParam.N` key is refused. So is a producer that is skipped (its values are
`null`), and one that is not bound at Job creation. This narrows the plan's C1 option,
which also lists `Steps.P.TaskParam.N` keys: a producer not bound at Job creation has a
binding that only its own inputs can advance, so supplying its `TaskParam` lists would
mean supplying a parameter space the model never materialized, and its outputs could not
be checked against it; such a Step is run with `--run-dependencies` instead. The
supplied values are a producer's state like any other, so job outputs that read only
supplied and concrete values are evaluated and printed. `--step-inputs` without a Step run
alone is an error (see [parameter-parsing.md](parameter-parsing.md) for its formats).

**The end of the run** first marks every Step of the run that a failed or blocked
dependency keeps from ever running, as in item 1, since the run stops at the first failure before
reaching them (RFC 0010 §Job outputs item 4: such an output is unavailable, naming the
Step). It then prints each job output not evaluated and why each Step it waits on has not
given its values: `Job output Sum was not evaluated: it waits on Step 'C', which failed`
(or `which is blocked`, `which this run does not run`, `which did not run all of its
Tasks`, `which the run stopped before`), or `Job output S has no value: its evaluation
failed`. With `--step` alone, the Steps not run are the ones this run does not run. The run succeeds only
after every job output whose inputs became concrete has been evaluated. The CLI imposes no
size limit on job outputs, which the RFC's 1 MiB minimum allows.

When the template lists `CONTROL_FLOW`, `run` also sets `SessionConfig::control_flow` and
passes each Step's `outputVariables` to `run_task`, so `openjd_output_var:` lines are
collected and checked: an invalid directive, or a declared `ONCE` or `LAST` output with no
value, fails the Task, and the message appears in the Task's output.

## Step Selection Logic

The step selection has three modes:

1. **Explicit `--step`** — Run the named step. Error if the name doesn't match any step.
2. **Auto-select** — If the job has exactly one step and no `--step` is provided, that
   step is selected automatically.
3. **All steps** — If the job has multiple steps and no `--step` is provided, all steps
   run in index order. Explicit task params (`--task-param`, `--tasks`) are not allowed
   in this mode because they're ambiguous across steps.

When `--run-dependencies` is set with an explicit step, `resolve_step_dependencies()`
computes the transitive closure of dependencies and returns them in topological order
(dependencies before dependents) via recursive DFS.

## Task Execution Modes

Within a selected step, tasks execute in one of three modes:

### No Parameter Space

If the step has no `parameterSpace`, a single task runs with no task parameters.
Supplying `--task-param` or `--tasks` for such a step is rejected during preflight, or,
for a `CONTROL_FLOW` Step pending inputs at Job creation, when it is bound. They are
rejected during preflight for a skipped Step.

### Explicit Task Parameters

When `--task-param` or `--tasks` is provided, the CLI runs exactly the specified tasks.
Task parameter values are injected as `Task.Param.<name>` and `Task.RawParam.<name>` in
the symbol table. The parameter type is set to `String` for `--task-param` values (the
CLI doesn't have type information for explicit params).

### Lazy Parameter Space Iteration

The default mode iterates the step's parameter space lazily via
`StepParameterSpaceIterator`. The iterator is never collected into a `Vec` — tasks are
consumed one at a time, which is critical for large parameter spaces (e.g., 100,000 frames)
that would exhaust memory if materialized.

For each task:
1. Get the next parameter set from the iterator
2. Copy it into a `TaskParameterSet`
3. Call `Session::run_task()` with `RunTaskOptions` carrying the Step's `output_variables` and
   `path_inputs`; the sessions crate builds symbols and materializes the script
4. Check the result and stop on failure

`--maximum-tasks` limits the iteration count. A value of -1 (default) means no limit.

## Adaptive Chunking

When a step uses `CHUNK[INT]` task parameters (from the TASK_CHUNKING extension), the
iterator produces chunks of varying size. The CLI implements adaptive chunking to
dynamically adjust chunk sizes toward a target runtime:

```
For each completed task:
1. Count items in the chunk (from the RangeExpr value)
2. Accumulate total items and total duration
3. If the cumulative duration is zero or non-finite, keep the current chunk size and wait
   for a measurable sample
4. Compute duration_per_task = total_duration / total_items
5. Compute ideal_chunk_size = target_runtime_seconds / duration_per_task
6. For the first 10 tasks, blend: 75% current + 25% ideal (conservative ramp)
7. Clamp to minimum of 1
8. Update iterator's default task count if changed
```

Deferring adjustment when elapsed time is not measurable prevents a zero duration from
producing an infinite ideal size that would saturate to `usize::MAX`. The blending in step 6
prevents wild oscillation when early tasks have atypical durations. After 10 tasks, the
estimate stabilizes and the ideal size is used directly.

`target_runtime_seconds` comes from the step's CHUNK[INT] parameter definition. If not
specified or zero, the model layer's `StepParameterSpaceIterator` sets `chunks_adaptive()`
to `false` (it requires `target_runtime_seconds > 0`), so `is_adaptive` is `false` in the
CLI and the entire adaptive adjustment block is skipped. The iterator uses its
`defaultTaskCount` as-is throughout execution.

## Environment Lifecycle

Environments are entered and exited in a strict order:

```
Enter:  env_templates[0], env_templates[1], ..., job_envs[0], job_envs[1], ...
  For each step:
    Enter:  step_envs[0], step_envs[1], ...
    (run tasks)
    Exit:   step_envs[N], ..., step_envs[1], step_envs[0]
Exit:   job_envs[N], ..., job_envs[0], env_templates[N], ..., env_templates[0]
```

Environment template environments (from `--environment` files) are converted from
`template::Environment` to `job::Environment` via
`openjd_model::convert_environment_with_symtab()`, passing a symbol table built from
the preprocessed parameter values (`openjd_model::build_symbol_table(&param_values)`).
This freezes the environment template's own `Param.*`/`RawParam.*` values into its
`resolved_symtab`, which the session's RFC 0008 wrap-hook dispatch merges into hook
scope — a wrap hook resolved against a step's symbol table could not otherwise see
the environment template's own parameters.

Step environments receive the step's symbol table (`step_symtab`) for format string
resolution. Job and template environments receive `None` for the step symbol table.

An Environment whose `enableIf` is `false` (`job::Environment::enabled`) is not entered,
and so is not exited.

Each environment is entered and exited with an `EnvironmentOptions` whose `origin` is
`EnvironmentTemplate` for an `--environment` file and `JobTemplate` for a job or step
environment, so that under `CONTROL_FLOW` an `openjd_output_var` directive in a job
template environment's action fails it, while one in an environment template's action is
ordinary output. The origin used on entry is recorded and passed again on exit. A step
environment's `EnvironmentOptions` also carries the Step's `path_inputs`, on entry and on
exit, so path-typed `Steps.*` symbols it reads are mapped as they are for the Step's
Tasks.

**Single-wrap-layer preflight (RFC 0008).** Before entering any environment,
the CLI validates every session stack this run will build — external
environment templates + job environments + each selected step's step
environments — and rejects the run if more than one environment in any stack
defines a wrap hook. The RFC requires rejection "before entering any
Environment", so this happens before the first wrapper's own `onEnter` can
run. The session's enter-time check (`SessionError::MultipleWrapEnvironments`)
remains as defense in depth for library callers.

Every entered environment — template, job, and step scoped — is tracked in an
`entered_envs` list of `EnteredEnvironment` values containing its identifier,
name, and optional step symbol table. A failed enter action marks the session
failed but still records the environment (the session keeps a failed-enter environment on its stack),
prints the failing action's `Process exited with code: N` line, and skips
entering further environments and running tasks. Step environments are
unwound to the pre-step baseline at the end of each step (or after a step-env
enter failure); final cleanup then exits every remaining recorded environment
in LIFO order. This is the OpenJD cleanup guarantee (extended to wrapped
exits by RFC 0008 "Lifecycle and cleanup guarantees"): every environment
entered or attempted has its `onExit` (or substituted `onWrapEnvExit`) run
before the session ends. An environment rejected before entry is not recorded
and not exited.

## Script Runtime Delegation

The CLI passes the instantiated `StepScript`, task parameter values, and the step's
serialized symbol table to `Session::run_task()`. The sessions crate owns script-level
`let` evaluation, embedded-file allocation and materialization, action format-string
resolution, subprocess execution, and path-mapping symbol materialization. The CLI only
selects and sequences tasks.

## Host-Context Function Library

The CLI passes the instantiated job's `ModelProfile` and path-mapping rules through
`SessionConfig`. `Session::with_config()` derives the matching expression profile and
host-context function library used by environment and task actions.

## Session Configuration

The `SessionConfig` struct is populated with:

| Field | Source |
|-------|--------|
| `session_id` | `"cli-{pid}"` where pid is the current process ID |
| `job_parameter_values` | From `preprocess_job_parameters()` |
| `path_mapping_rules` | From `--path-mapping-rules` (None if empty) |
| `retain_working_dir` | From `--preserve` |
| `callback` | `None` (CLI doesn't use action callbacks) |
| `os_env_vars` | `None` (inherit current environment) |
| `session_root_directory` | `None` (use system temp) |
| `user` | `None` (run as current user) |
| `profile` | `ModelProfile` built from the job template's declared extensions |
| `control_flow` | Whether that profile, the Job Template's own, lists `CONTROL_FLOW` |
| `output_callback` | `None` (the CLI prints each Action's values from `ActionResult.outputs` once it exits) |
| `limits` | `SessionLimits` with `max_resolved_arg_len = common::DEFAULT_MAX_ARG_LEN` (the CLI's uniform 32K-character default — see [check.md § Caller-Limits Policy](check.md#caller-limits-policy)); everything else `None` |

The same OS-max cap applies at every enforcing stage the CLI drives:
template decode uses `common::caller_limits()` (early failure on the
lower bound), `create_job` carries the same limits on its
`ValidationContext` (early failure once parameter values are bound),
and the session mirrors them via `SessionConfig::limits` (the
authoritative check on final values). As its `create_job` context the
CLI passes `job_template.default_validation_context()`, with those
caller limits layered on.

## Result Output

After all steps complete (or on failure), the command prints a summary:

### Human-Readable

```
--- Results of local session ---

Session ended successfully
Working directory preserved at: /tmp/openjd-cli-12345

Job: MyJob
Step: Render
Duration: 42.123 seconds
Chunks run: 100
```

For a `CONTROL_FLOW` Job with job outputs, a `Job outputs:` list follows, one line per
evaluated output (`Total = 30 px`) and per output not evaluated (`Sum: not evaluated
(waits on Step 'C', which failed)`, or `Count: has no value (its evaluation failed)`).

### JSON

```json
{
  "status": "success",
  "message": "Session ended successfully",
  "job_name": "MyJob",
  "step_name": "Render",
  "duration": 42.123,
  "chunks_run": 100
}
```

For a template that lists `CONTROL_FLOW`, three more fields follow (other templates'
output matches the Python CLI's):

* `steps`: one entry per Step, `{name, state, task_count}` with the RFC's state name
  (`PENDING_INPUTS`, `READY`, `SKIPPED`, `SUCCEEDED`, `FAILED`, `BLOCKED`) and
  `task_count` `null` while its Tasks are unknown; `incomplete: true` for a Step that did
  not run all of its Tasks; `reason` for a failed or blocked Step; and, for a Step that
  declares output variables and started, `outputs`, a list with one map from output name
  to value per Task in iteration order, up to the last Task whose outputs arrived (`null`
  for a Task before it that did not succeed), so a Step whose Tasks all succeeded has one
  per Task and a zero-Task Step has `[]`.
* `job_outputs`: a map from each evaluated job output's name to its value (`null` for no
  value).
* `job_outputs_not_evaluated`: a list of `{name, waits_on}`, as printed at the end.

### YAML

```yaml
status: success
message: Session ended successfully
job_name: MyJob
step_name: Render
duration: 42.123
chunks_run: 100
```

## Error Handling and Exit Codes

- Exit code 0: All actions completed successfully
- Exit code 1: Any action failed, setup failed, or interruption was observed

On action failure, the session continues to exit environments (cleanup) but skips
remaining tasks. The `session_failed` flag tracks whether any action returned a
non-`Success` state.

An interruption signal cancels the active session action. On Unix the handled
signals are SIGINT and SIGTERM; on Windows they are Ctrl+C and Ctrl+Break
(Ctrl+Break matters because a process started with CREATE_NEW_PROCESS_GROUP —
as worker tooling typically does — cannot receive Ctrl+C, making
CTRL_BREAK_EVENT the only console event another process can deliver to the
CLI alone). An interruption observed between actions also marks the run
failed, so stopping between environment entries cannot produce a successful
zero-task result. The run prints "Interruption signal received.", reports the
session as failed, and exits with code 1.

Setup errors (file not found, parse failure, invalid parameters) return immediately
via `Result::Err`, which `main()` prints to stderr before exiting with code 1.

## Differences from Python CLI

| Aspect | Python | Rust |
|--------|--------|------|
| Session wrapper | `LocalSession` context manager | Direct `Session` API calls |
| Signal handling | SIGINT/SIGTERM handlers with `cancel()` | Tokio signal task cancels the session token |
| Adaptive chunking | In `LocalSession._run_tasks_adaptive_chunking()` | Inline in task iteration loop |
| Step ordering | `StepDependencyGraph` topological sort for all-steps mode | `StepDependencyGraph` topological sort |
| Environment conversion | Implicit (Python sessions accept template types) | Explicit `convert_environment_with_symtab()` call |
| Callback | `LocalSession._action_callback()` handles states | No callback; result checked after each await |
