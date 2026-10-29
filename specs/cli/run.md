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
    [--run-dependencies] [--no-run-dependencies]
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
| `--environment` (alias `--env`) | `Vec<PathBuf>` | No | — | Environment Template files, attached in the order given (see [Environment Templates and Environment Parameters](#environment-templates-and-environment-parameters-env_params)) |
| `--path-mapping-rules` | `Option<String>` | No | — | Path mapping rules (JSON or file://) |
| `--run-dependencies` | `bool` | No | `false` | Run transitive dependency steps first |
| `--no-run-dependencies` | `bool` | No | — | Explicit opt-out (default behavior) |
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
`RunContext` owns the run's one `Session`, entered-environment ledger, failure state,
interruption flag, task count, run start time, and the Session's working directory once
it has ended with `--preserve`.

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
  │   └── create_job_with_environment_templates() with the --environment
  │       templates → Job whose job_environments lead with the attached
  │       Environments, and their documents; print its warnings to stderr
  │
  ├── 2. SELECTION AND PREFLIGHT
  │   ├── Resolve step selection (explicit / auto-select / all)
  │   ├── Parse explicit task params and require a parameter space
  │   └── Determine step execution order
  │
  ├── 3. SESSION CREATION
  │   ├── Create SessionConfig from parameters, path rules, and model profile
  │   └── Session::with_config()
  │
  ├── 4. STEP EXECUTION
  │   └── For each step, in order (stopping after a failure or an interruption):
  │       ├── Move to the step's job environments: exit entered ones (LIFO)
  │       │   down to the longest prefix the step also enters with equal
  │       │   Environment parameter values, then enter the rest of the step's
  │       │   combined job environments (attached ones first) with its values
  │       ├── Enter step environments
  │       ├── Execute tasks (explicit, lazy iteration, or no-param single task)
  │       └── Exit step environments (reverse order)
  │
  ├── 5. ENVIRONMENT EXIT
  │   └── Exit every entered environment (LIFO), including any whose
  │       enter action failed (cleanup guarantee)
  │
  └── 6. RESULTS
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

Job creation (with its submission checks and the RFC 0008 single-layer check) and
selection happen before session creation. Invalid runs
therefore do not create a working directory or print session-start banners.

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

Selection does not change which Environments a Step's Tasks run with: before each Step the
Session moves to that Step's Environment set (`ENV_PARAMS`, see below), so `--step C` for a Step that does not
request an `ON_REQUEST` Environment does not enter it, even if another Step of the Job
does. `openjd run` does not match host requirements against the local host; job creation
still combines each Step's (Template Schemas §3.3.3) and rejects a conflict, and
`openjd summary` shows the combined set.

## Task Execution Modes

Within a selected step, tasks execute in one of three modes:

### No Parameter Space

If the step has no `parameterSpace`, a single task runs with no task parameters.
Supplying `--task-param` or `--tasks` for such a step is rejected during preflight.

### Explicit Task Parameters

When `--task-param` or `--tasks` is provided, the CLI runs exactly the specified tasks.
Task parameter values are injected as `Task.Param.<name>` and `Task.RawParam.<name>` in
the symbol table. Each value is coerced to the type the step's parameter space declares
for it (`coerce_task_params` in `run/params.rs`): `INT` and `FLOAT` values are parsed, and a
`CHUNK[INT]` value is parsed as a range expression. For a `CONTIGUOUS` `CHUNK[INT]` parameter
the value carries the contiguous display flag, so it is written `<N>-<M>` even with one or
two values (`-t Frame=5-6` is written `5-6`, not `5,6`), as Template Schemas §3.4.1.5
requires and as the chunks the CLI computes itself are.

The value is accepted when it has the same values as one of the step's chunks
(`RangeExpr` equality is by value), however it is spelled, and the parameter-space error
quotes it in the form the chunk is written. **Divergence from openjd-cli (Python):** for a
`CONTIGUOUS` parameter, Python rejects a value not spelled `<N>-<M>` (`-t Frame=5,6` gives
`... is not a contiguous interval like '1-5' as required by the chunk range constraint`),
while openjd-rs accepts it when `5-6` is a chunk and writes it `5-6`; and for `-t Frame=5`
when `5` is not a chunk, openjd-rs's error quotes the value as `'5-5'`.

### Lazy Parameter Space Iteration

The default mode iterates the step's parameter space lazily via
`StepParameterSpaceIterator`. The iterator is never collected into a `Vec` — tasks are
consumed one at a time, which is critical for large parameter spaces (e.g., 100,000 frames)
that would exhaust memory if materialized.

For each task:
1. Get the next parameter set from the iterator
2. Copy it into a `TaskParameterSet`
3. Call `Session::run_task()`; the sessions crate builds symbols and materializes the script
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

For each step, the `env_templates` and `job_envs` are only those the step enters (every one
with `activation: ALWAYS`, and each `ON_REQUEST` one the step requests, less any whose own
`enableIf` is `false` for it; see [One Session, re-entering
Environments between Steps](#one-session-re-entering-environments-between-steps)):

```
For each step:
  Exit:   the entered env_templates and job_envs the step does not keep, most recent first
  Enter:  the step's env_templates and job_envs not yet entered, in Job order
  Enter:  step_envs[0], step_envs[1], ...
  (run tasks)
  Exit:   step_envs[N], ..., step_envs[1], step_envs[0]
Exit:   every entered job_env and env_template, most recent first
```

A Job whose Steps all enter the same Job Environments with equal values (every Job without
`ENV_PARAMS` values, `activation: ON_REQUEST`, or an Environment `enableIf`) enters them once,
before the first step, and exits them once, after the last, as before `ENV_PARAMS`.

Environment template environments (from `--environment` files) are the leading entries of
the combined Job's `job_environments`. Job creation converts each from
`template::Environment` to `job::Environment` with
`openjd_model::convert_environment_with_symtab()`, passing a symbol table built from
the preprocessed parameter values, plus `Job.Name` and `Job.EnvNames` when the Environment
Template lists `EXPR`.
This freezes the environment template's own `Param.*`/`RawParam.*` values into its
`resolved_symtab`, which the session's RFC 0008 wrap-hook dispatch merges into hook
scope — a wrap hook resolved against a step's symbol table could not otherwise see
the environment template's own parameters. The Job Template's own environments get the
same frozen table from job creation (`Param.*`, `RawParam.*`, `Job.Name`, `Job.EnvNames`).

Step environments receive the step's symbol table (`step_symtab`) for format string
resolution. Job and template environments receive their own `resolved_symtab`, so their
`onEnter`, `onExit`, and `variables` see `Job.Name` and `Job.EnvNames` (the Session builds an
Environment's scope from the table it is passed, not from the `Environment`'s field).

**Single-wrap-layer check (RFC 0008).** Job creation checks every Step's Environment set
— the attached and Job Environments it enters, by `activation` and `enableIf`, and its Step
Environments whose `enableIf` is not `false` —
and rejects the Job if more than one environment in a set defines a wrap hook (see
[../model/job-creation.md](../model/job-creation.md#the-submission-stage)). It checks every
Step of the Job, as a scheduler would for the submission, so `--step` does not exempt the
Steps a run does not select (the CLI's own preflight, removed, checked only the selected
Steps). The RFC requires
rejection "before entering any Environment", so this happens before the first wrapper's own
`onEnter` can run. The session's enter-time check (`SessionError::MultipleWrapEnvironments`)
remains as defense in depth for library callers.

Every entered environment — template, job, and step scoped — is tracked in an
`entered_envs` list of `EnteredEnvironment` values containing its identifier,
name, symbol table, and, for a job or template environment, its index in the
combined Job's `job_environments` and the Environment parameter values it was
entered with. A failed enter action marks the session
failed but still records the environment (the session keeps a failed-enter environment on its stack),
prints the failing action's `Process exited with code: N` line, and skips
entering further environments and running tasks. Step environments are
unwound to the pre-step baseline at the end of each step (or after a step-env
enter failure); before the next step, job and template environments are unwound
to the prefix that step keeps; final cleanup then exits every remaining recorded
environment in LIFO order. This is the OpenJD cleanup guarantee (extended to wrapped
exits by RFC 0008 "Lifecycle and cleanup guarantees"): every environment
entered or attempted has its `onExit` (or substituted `onWrapEnvExit`) run
before the session ends. An environment rejected before entry is not recorded
and not exited.

## Environment Templates and Environment Parameters (ENV_PARAMS)

`openjd run` treats each `--environment` file as an Environment Template the scheduler
attaches, in the order given (Template Schemas §3.2.1), labeled with its path as given so
messages name it. It calls `openjd_model::create_job_with_environment_templates` with them,
the merged Job Parameters, and `common::submission_context` (the template's profile, the
CLI's caller limits, the host submission path format, and URI path values allowed, as for
Job Parameters). Its submission stage:

- resolves each `environment:` dependency that names no `jobEnvironments` entry against
  the attached Environments by name. A missing `required` Environment, an ambiguous name, an
  undeclared key, a bad value, or a parameter without a `default` that a Step does not
  supply fails the run with one `Submission` validation error before any Session starts,
  each error at its path (`JobTemplate -> steps[0] -> dependencies[1] -> parameters -> X`),
  followed, like every job-creation error, by the Job Template's help text. A
  `required: false` entry that resolves to nothing has no effect, and an entry whose
  `enableIf` is `false` is not resolved at all;
- fixes each Step's Environment set (`job::Step::environments`): every Environment with
  `activation: ALWAYS`, and each `ON_REQUEST` one an enabled entry of the Step requests,
  less any whose own `enableIf` (Template Schemas §4 item 6) is `false` for the Step, which
  the Step's Sessions then do not enter or exit and whose `variables` they do not set; a
  Step Environment whose `enableIf` is `false` is likewise left out of
  `job::Step::step_environments`, so `execute_step` does not enter it;
- computes every Step's effective values for every attached Environment it would enter
  that declares parameters (§3.2.2), decides the Environment's own `enableIf` with the values
  that are known (a missing or unconvertible value is unresolved, and does not disable it),
  and checks the values against their type and constraints, and constraint 7, only for the
  Environments it does not disable (constraint 5 for every one), as
  job creation does for the Job Template's own;
- returns warnings, which are printed to stderr as `WARNING: <path>: <message>`: one per
  `required: false` entry that resolves to nothing, and one per attached Environment named
  like a `jobEnvironments` entry (§3.2.1 item 4).

`CreatedJob::environment_documents` records which document each `job_environments` entry comes
from. When neither the Job Template nor any attached Environment Template lists `ENV_PARAMS`,
this stage only converts the attached Environments, so a run behaves as before. When only an
Environment Template lists it, its Environment is entered with its `default`s (RFC Backward
compatibility), and an attachment named like a Job Environment is not warned about, since a
Job Template without `ENV_PARAMS` has no entries that could name it.

### One Session, re-entering Environments between Steps

Each time a Session enters an Environment it uses one set of values and keeps them until it
exits the Environment, and it may run a Step's Tasks only when the Environments it has
entered and not exited are exactly the Step's, with equal values (§3.2.2, How Jobs Are Run).
Between Steps a Session may exit Environments, in the reverse of the order it entered them,
and enter others, or enter one again with another Step's values. `openjd run` runs every
selected Step, in the order they run (the topological order, or the `--run-dependencies`
order), in one Session, as it did before `ENV_PARAMS`, and moves the Session between Steps
that way.

Before each Step, `job_environment_targets` lists the combined job environments the Step
enters (`job::Step::enters_environment`: every one with `activation: ALWAYS`, each
`ON_REQUEST` one it requests, less any whose own `enableIf` is `false` for it), in Job order,
each with the Step's values for it. `RunContext::move_to_job_environments` keeps the longest
prefix of the entered environments whose every entry is the same environment (its index in
`job_environments`, which names it by `(document, name)`) entered with values equal to the
Step's (`EnvironmentParameterValues::values_equal`, the comparison
`job::Step::environment_values_equal` makes, so INT `1` and `"1"` are equal, FLOAT compares
numerically, and RANGE_EXPR by canonical form), or with no values on both sides. It exits
the rest, most recently entered first, and then enters the Step's remaining environments in
order. An environment above the first difference is exited and entered again even when its
own values are unchanged, because exits follow the reverse of entry order. For example, if
Step A enters `First`, the `ON_REQUEST` `Middle`, and `Last`, and Step B enters only `First`
and `Last`, the Session exits `Last` and `Middle` and enters `Last` again before B. Steps are
never reordered to reduce re-entries: in the RFC's video sample, `DecodeFrames` and
`EncodeVideo` have equal values but are not adjacent, so with the Conda Environment the
Session enters Conda four times.

The run prints no message of its own for a move: the `Exiting Environment: <name>` and
`Entering Environment: <name>` banners of the environments it exits and enters, which precede
the Step's `Running step '<name>'` line, show it. The session ID is `cli-<pid>`, and the run has
one working directory. Exits use the same `exit_environments_down_to` as the end of the run,
so an Environment entered under an RFC 0008 wrapper is exited through the wrapper's
`onWrapEnvExit` while the wrapper is still entered, and entered again through its
`onWrapEnvEnter`. A failed action or an interruption stops the run before the next Step's
move. A failed `onExit` or `onEnter` during a move stops it too: the Session enters no
further environment, does not run the Step, then exits every entered environment (including
one whose enter failed) and ends, and the run fails. An `--environment` file that
does not parse or validate is named in the error (`Environment Template '<path>': ...`).

Each combined job environment is entered with the values of the Step it is entered for,
picked by `(document, name)` from the Step's
`environment_parameters` (an attached Environment may share a name with a Job Environment,
§3.2.1 item 4), through `Session::enter_environment_with_parameters`. An Environment that
declares no parameters, and every Step Environment, is entered with none. After the
`Entering Environment` banner the values are printed, before path mapping, in
parameter-name order (the order of the stored table, not the declaration order), each in the
display form `Param.*` values use (`ExprValue::to_display_string`; a RANGE_EXPR value in
canonical form, so `1,2,3` shows as `1-3`):

```
Environment parameter values:
Channels(STRING) = conda-forge
Packages(STRING) = ffmpeg=7
```

A list value prints as `[1, 2]` or `["/a", "/b"]`. The Session binds the values as `Env.Param.*`
(PATH and `LIST[PATH]` path-mapped) and `Env.RawParam.*` for that Environment only, and reuses
them for its `onExit` and wrap hooks.

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
| `limits` | `SessionLimits` with `max_resolved_arg_len = common::DEFAULT_MAX_ARG_LEN` (the CLI's uniform 32K-character default — see [check.md § Caller-Limits Policy](check.md#caller-limits-policy)); everything else `None` |

The same OS-max cap applies at every enforcing stage the CLI drives:
template decode uses `common::caller_limits()` (early failure on the
lower bound), `create_job` carries the same limits on its
`ValidationContext` (early failure once parameter values are bound),
and the session mirrors them via `SessionConfig::limits` (the
authoritative check on final values). As its job-creation context the CLI passes
`common::submission_context(&job_template)`: `default_validation_context()`
with those caller limits, the host submission path format, and URI path
values allowed.

## Result Output

After all steps complete (or on failure), the command prints a summary:

### Human-Readable

```
--- Results of local session ---

Session ended successfully
Working directory preserved at: /tmp/OpenJD/k3x9_q2m

Job: MyJob
Step: Render
Duration: 42.123 seconds
Chunks run: 100
```

With `--preserve` the `Working directory preserved at:` line names the Session's working
directory. A run that ends with an error before the summary still prints that line.

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
