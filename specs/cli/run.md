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
| `--step` | `Option<String>` | No | — | Step or loop to run (auto-selects if the Job has one top-level Step or loop; runs all if omitted otherwise) |
| `-p`, `--parameter` | `Vec<String>` | No | — | Job parameters |
| `-t`, `--task-param` | `Vec<String>` | No | — | Explicit task parameter values for a single task |
| `--tasks` | `Option<String>` | No | — | JSON array or file of task parameter sets |
| `--maximum-tasks` | `Option<i64>` | No | none (all) | Max tasks to run per Step (-1 = all); for a loop, per body Step instance |
| `--environment` | `Vec<PathBuf>` | No | — | Additional environment template files |
| `--path-mapping-rules` | `Option<String>` | No | — | Path mapping rules (JSON or file://) |
| `--run-dependencies` | `bool` | No | `false` | Run transitive dependency steps first |
| `--no-run-dependencies` | `bool` | No | — | Explicit opt-out (default behavior) |
| `--step-inputs` | `Option<String>` | No | — | `CONTROL_FLOW`: the `Steps.*` output values a Step or loop run alone reads (JSON or file://) |
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

`--maximum-tasks` has no default value, so the run can tell a given value, even `-1`, from
none (`None` runs every Task). A loop needs that: an explicit `--maximum-tasks` is an error
with `--step` naming a loop (see [Loops](#control_flow-loops)).

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
    #[arg(long = "maximum-tasks", allow_negative_numbers = true)]
    pub maximum_tasks: Option<i64>,
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
  │   ├── Resolve step selection (explicit / auto-select / all); a loop or a body
  │   │   Step gets the loop flag rules
  │   ├── Parse explicit task params and require a parameter space
  │   ├── Determine the execution order of the top-level Steps and loops
  │   ├── Create the scheduler simulator; for a Step or loop run alone, give it the
  │   │   --step-inputs values and check that every Steps.* value it reads is concrete
  │   └── Validate RFC 0008's single-wrap-layer rule for every selected stack
  │       (disabled Environments do not count; a body Step instance's stack is
  │       checked as it starts)
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
  │   └── For each top-level Step or loop:
  │       ├── A Step:
  │       │   ├── Begin it: block it, skip it, or attach its Steps.* inputs
  │       │   │   (settling, below, is where a pending Step advances)
  │       │   ├── Enter enabled step environments
  │       │   ├── Execute tasks (explicit, lazy iteration, or no-param single task),
  │       │   │   recording each Task's output values
  │       │   ├── Exit step environments (reverse order)
  │       │   └── Record it succeeded or failed
  │       ├── A loop: begin it, then for each iteration start it, run each body Step
  │       │   instance as a Step (above), and evaluate the iteration boundary
  │       └── Settle: advance pending Steps and loops, and evaluate job outputs
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

`CONTROL_FLOW` (RFC 0011) is enabled by default with every other extension. For a Job
whose template lists it, `run` acts as a scheduler would, in a small in-memory simulator
(`run/simulator.rs`, plan C1). A Job without it has only bound Steps and no job outputs,
so the simulator only records what ran, and the run's output is unchanged.

Loops (RFC 0013) are nodes of the step graph and run as described in
[Loops](#control_flow-loops) below. The simulator addresses a top-level entry as a `Node`
(`Step(i)` into `job.steps`, `Loop(i)` into `job.loops`), in the order of
`StepDependencyGraph::topo_sorted`, whose node indices are not `job.steps` indices once a
Job has a loop.

**State.** The simulator keeps its own copy of the `job::Job`, a `StepRecord` per Step
(`SimStepState`: `Pending`, `Bound`, `Skipped`, `Succeeded`, `Failed`, `Blocked`; each
Task's outputs; why it failed or is blocked), a `LoopRecord` per loop (below), and one
`StepInputs` holding the
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
requirements), `Step 'C' is ready`, or `Step 'C' skipped (enableIf is false)`. A pending
loop is advanced the same way, with `advance_loop_binding` (see [Loops](#control_flow-loops)). A group
that fails prints the `BindingError` (`ERROR: Step 'C' failed in its Task enumeration
group:` and the located message), marks the Step `Failed`, and stops the run before any
more Tasks start, as a failed Task does. The pass still advances the other pending Steps
and then evaluates the job outputs whose inputs are concrete, as a scheduler evaluates
each as soon as its inputs are, so what is known does not depend on the order Steps are
declared in. This is RFC 0011's early failure: a Step whose
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
   (RFC 0011 §Step states), but never satisfies `dependsOn` (§Conditional execution,
   Failure is not skipping): it prints `Step 'S' is skipped and never satisfies dependsOn:
   its dependency 'P' failed`, keeps that as its reason, and its dependents are blocked
   with `its dependency 'S' is skipped but never satisfies dependsOn`. Since a failed Task
   stops the run, this is reached for a Step after a failed Step only at the end of the
   run (below).
2. A Step is not advanced here: its dependencies have finished, and settling advanced it
   after the last of them changed state, so it is bound, skipped, or failed by now. A Step
   still pending is an internal error, unless it reads the outputs of a Step that did not
   run all of its Tasks (below).
3. A `Skipped` Step prints `Step 'X' is skipped; it runs no Tasks` and runs nothing. If the
   run gives it Tasks with `--tasks` or `--task-param` (it was skipped at run time; one
   skipped at Job creation is refused in preflight), the run fails with `ERROR: Step 'S'
   is skipped by its enableIf; the Tasks given with --tasks or --task-param cannot be
   run.`
4. A bound Step that reads `Steps.*` gets `attach_step_inputs`. Only its Step
   Environments with `enabled` are entered; each other prints `Step Environment 'E' is
   disabled by its enableIf; not entering it`. Its Tasks run as before, and each
   successful Action's `ActionResult.outputs` is recorded per Task, in the Step's
   per-Task iteration order, through the model's `ChunkTaskIndex` (a chunk's outputs are
   keyed by `CHUNK[INT]` value). Each value is printed with its unit after its Action
   (`Output N = 3 frames`, or `Output Done[2] = 20` in a chunked Step). Outputs are kept
   sparsely, and only for a Step that declares output variables; an unchunked space run in
   order is placed by position, and a chunked one through the lazy `ChunkTaskIndex`.
   Tasks given with `--tasks` or `--task-param` are kept in the order they ran and placed
   through a `ChunkTaskIndex` only once they are as many as the Step's Tasks. Without
   output variables, given Tasks complete the Step when they name every Task, counting
   each `CHUNK[INT]` value of a given chunk as one Task. Either way a
   large parameter space costs nothing for the Tasks that do not run. A `FLOAT` value is
   matched by number, so a range written `1.50` matches a given `1.5`. A Step with zero Tasks prints `Step 'X'
   has 0 tasks` (only for a `CONTROL_FLOW` template) and succeeds.
5. The Step is recorded `Failed` (reason `a Task failed`, unless a group failed) if the
   run failed while it ran, and `Succeeded` otherwise; then the simulator settles.

Job Environments with `enabled == false` are not entered either (`Job Environment 'E' is
disabled by its enableIf; not entering it`).

**Incomplete producers.** `--maximum-tasks`, `--tasks`, and `--task-param` can leave a
Step with Tasks that did not run. It is recorded `Succeeded` but its outputs are not given
to anything (RFC 0011 §Reductions item 3). A Step that reads them becomes `Blocked` with
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
`job::Step::input_symbols`), and otherwise fails naming all of them: `Step 'C' reads
Steps.* values of Steps this run does not run: Steps.P.Output.V.` followed by `Give them
with --step-inputs, or run Step 'C' with --run-dependencies.`, or by only `Run Step 'C'
with --run-dependencies.` when none of the missing values is an output of a producer bound
at Job creation, which is all `--step-inputs` can give. A key naming a Step whose outputs C
does not read is refused (`--step-inputs: Step 'C' does not read the outputs of Step
'R'.`), as is a Step the Job does not have. A `Steps.P.Output.V` key also gives
`Steps.P.RawOutput.V`, so a missing `RawOutput` is given with the `Output` key. A producer's `TaskParam` comes from its parameter space, so a
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
dependency keeps from ever running, as in item 1 (not after an interruption, which stops
the run without making any Step unable to run; a Step interrupted while its Tasks ran is
`FAILED` with reason `the run was interrupted`), since the run stops at the first failure before
reaching them (RFC 0011 §Job outputs item 4: such an output is unavailable, naming the
Step). It then prints each job output not evaluated and why each Step it waits on has not
given its values: `Job output Sum was not evaluated: it waits on Step 'C', which failed`
(or `which is blocked`, `which this run does not run`, `which did not run all of its
Tasks`, `which the run stopped before`), or `Job output S has no value: its evaluation
failed`. With `--step` alone, the Steps not run are the ones this run does not run. The run succeeds only
after every job output whose inputs became concrete has been evaluated. The CLI imposes no
size limit on job outputs, which the RFC's 1 MiB minimum allows.

<a id="control_flow-loops"></a>
### CONTROL_FLOW: loops

A loop (RFC 0013, plan L8) runs in place in the order of the top-level entries, its
iterations one after another, in the one Session `run` opens, with its Job Environments
entered once (RFC 0013 §Sessions allows this).

**State.** A `LoopRecord` holds the loop's `SimStepState` (with `Running`, `RUNNING`), its
`LoopProgress` while it runs, its `LoopResult` once it succeeds, why it failed or is
blocked, and one `IterationRecord` per iteration started: its `LoopIteration` (the body
Step instances, each as advanced so far), its loop variable values, a `StepRecord` per
instance, and the loop output values its boundary evaluated. While an iteration runs, a
second `StepInputs` holds the `Steps.*` values its instances and its boundary read: the
run's `StepInputs` as the iteration started, and each instance as it is skipped, its Tasks
become known, or it succeeds with all of its Tasks. Each dependency of a loop counts as
read, so its outputs are kept for the loop's body (`common::read_producers`).

**Binding.** A pending loop is advanced in settling, after a producer it reads changes, with
`advance_loop_binding`. Its facts are printed: `Loop 'L': enabled (enableIf is true)`,
`Loop 'L': maxIterations 6`, `Loop 'L': the first while is false, so it has 0
iterations`, `Loop 'L' is ready`, or `Loop 'L' skipped (enableIf is false)`. Before the
first Step, a pending loop is reported as `Loop 'L' is pending inputs:` with one line per
group (`common::loop_binding_lines`: `Enabled:`, `maxIterations:`, and `Start (initial
values and first while):`). A group that fails prints its `BindingError` and fails the
loop, as for a Step.

**Starting it.** A loop behind a failed or blocked dependency is blocked, and one still
pending because a producer did not run all of its Tasks is blocked as a Step is. A skipped
loop prints `Loop 'L' is skipped; it runs no iterations`. A bound loop whose first `while`
is false calls `end_without_iterations`: it succeeds with `Loop 'L' succeeded after 0
iterations (the first while is false)` and `null` and `[]` outputs, or fails on an
`APPEND` output's `minLength`, which stops the run. Any other bound loop prints `Running
loop 'L'` and is `RUNNING`.

**Each iteration** prints `Loop 'L' iteration 0: Samples = 64, Frames = [1, 2, 3]` (or
`Loop 'L' iteration 0` with no variables) and calls `start_iteration`. Each instance it
creates skipped prints `Step 'L[0].S' skipped (enableIf is false)`, and each pending one
`Step 'L[0].S' is pending inputs:` with its group lines. Then the instances run in body
dependency order through the path of a top-level Step, named `<Loop>[<N>].<Step>` in
every line (`Running step 'L[0].RenderPass'`, `Step 'L[1].P': Tasks known (3 tasks)`): a
pending instance is advanced with `advance_body_step_binding` after each instance
finishes, until nothing changes; a bound one gets `attach_step_inputs` with the
iteration's inputs, its stack is checked against RFC 0008's single-wrap-layer rule (a
conflict fails the instance), its enabled Step Environments are entered, and its Tasks
run, `--maximum-tasks` applying to it as to any Step. Only the instances that another
instance or the boundary reads (`input_producers()` of the instances, and the producers
`Loop::expression_inputs` finds in the boundary expressions) get their `TaskParam` lists
and outputs built into the iteration's inputs, so a large parameter space that nothing
reads is not enumerated. When every instance has succeeded with all of its Tasks or been
skipped, the simulator calls `evaluate_iteration_boundary`, prints the iteration's loop
output values (`Loop 'L' iteration 0 outputs: FinalSamples = 64, NoisyFrameCounts = 2`:
the `LAST` value, or the element `APPEND` added), and either starts the next iteration or
ends the loop: `Loop 'L' succeeded after 3 iterations (until is true)` (or `while is
false`), then `Loop 'L' outputs: FinalSamples = 256, NoisyFrameCounts = [2, 1, 0]`. A
succeeded loop is added to the run's `StepInputs` with `add_loop_producer`, its
dependents are advanced, and the job outputs that read it are evaluated.

**Failure.** A failed body Task fails its instance and the loop, as a failed Task fails a
Step: `Loop 'L' failed: its body Step instance 'L[1].Work' failed`. The CLI has no
retries, so every loop failure is final. An instance group that fails, a boundary error,
reaching `maxIterations`, and a `minLength` violation print the model's error on stderr
and its first line on stdout (`Loop 'Refine' failed at the boundary of iteration 1, in
maxIterations`), and fail the loop. A loop's own group that fails at binding prints its `BindingError` on stderr and
`Loop 'L' failed in its start group` (or `enablement`, `bound`) on stdout. A failed loop
stops the run, and its dependents are blocked with `its dependency 'L' failed`; a job
output that reads it waits on `Loop 'L', which failed`.

**Incomplete instances.** An instance that did not run all of its Tasks
(`--maximum-tasks`) is incomplete. The other instances of its iteration follow the rule
for an incomplete producer: one that reads its outputs is blocked with `inputs from Step
'L[0].P' are incomplete because not all of its Tasks ran`, and one that does not read them
runs. The iteration never completes, so the boundary is not evaluated, and the loop is
blocked: `Loop 'L' is blocked: its body Step instance 'L[0].P' did not run all of its
Tasks, so iteration 0 never completes` (or `... 'L[1].Q' is blocked, so iteration 1 never
completes` for an instance blocked behind one). Its dependents are blocked behind it, and
the run does not fail. A dependency of the loop that did not run all of its Tasks blocks the loop
before its first iteration when the loop's boundary expressions read it (`Loop 'L' is
blocked: inputs from Step 'A' are incomplete because not all of its Tasks ran`), and blocks
the instances that read it as for any producer.

**Flag rules** (RFC 0013 §Implementation Impact):

* `--step L` naming a loop runs the whole loop. `--step` naming a body Step fails with
  `Step 'Q' is a body Step of loop 'L', so it runs only within the loop's iterations. Use
  --step L to run the whole loop.`
* `--tasks` and `--task-param` fail whenever the selected Step is a loop, named by `--step`
  or selected as the Job's only top-level entry: `Loop 'L' has no Task parameters or Tasks
  of its own; --tasks cannot be used.`
* An explicit `--maximum-tasks`, even `-1`, fails with `--step` naming a loop: `Loop 'L'
  has no Tasks of its own; --maximum-tasks cannot be used with --step naming a loop.
  Without --step, --maximum-tasks applies to each body Step instance.` In every other run,
  including the selection of the Job's only loop without `--step`, it applies to each body
  Step instance.
* `--step-inputs` with `--step L` supplies outputs of `L`'s dependencies, the only Steps
  outside the loop it and its body read (RFC 0013 §`Steps.*` in and around a loop); a key
  naming another Step is refused with `--step-inputs: Loop 'L' does not read the outputs
  of Step 'X'.` Before the Session starts, the simulator checks that what the loop reads
  is concrete: its groups' inputs while it is pending, and once it is bound, the
  `Steps.*` symbols of producers outside the body that its boundary expressions
  (`Loop::expression_inputs`) and the body Step instances of iteration 0 read (a
  `start_iteration` call it discards). A missing one fails as for a Step: `Loop 'L' reads
  Steps.* values of Steps this run does not run: Steps.B.Output.Text.` followed by the
  hint. An instance that iteration 0 skips is not checked. If a later iteration then reads
  a dependency this run does not run, the loop fails there with `Loop 'L' reads, in
  iteration 1, Steps.* values of 'A', which this run does not run.` followed by `Give them
  with --step-inputs, or run Loop 'L' with --run-dependencies.` (or only `Run Loop 'L'
  with --run-dependencies.` when no such dependency was bound at Job creation, since
  `--step-inputs` cannot give it), the instance that needed it is `FAILED` with that
  reason, and the run reports its result. The simulator classifies a caller error from a
  loop function this way only when the error names a `Steps.*` symbol of such a
  dependency; any other is an internal error.
* For a Step that reads a loop, `--step-inputs` supplies each loop output in its declared
  type: one value under `LAST` and the whole list under `APPEND`, each element checked as a
  reported value is. `null` is accepted for a `LAST` output only when its type is `T?` (a
  `while` loop, or a loop with an `enableIf`; RFC 0013 §`Steps.*` in and around a loop).
  Every output of the loop must be given, even one the Step does not read, as every
  declared `ONCE` or `LAST` output of a Step must (`--step-inputs gives a loop's outputs
  together, so every output of the loop must be given; missing Steps.L.Output.H`). A key
  that names the target itself is refused (`--step-inputs: 'Steps.L.Output.H' names Loop
  'L' itself; give the outputs of the Steps it reads.`). A `Steps.L.TaskParam.*` key is refused (`Loop 'L'
  has no Task parameters`), as is a key naming a body Step (`Step 'P' is a body Step of
  loop 'L', and its outputs are not available outside the loop.`).

When the template lists `CONTROL_FLOW`, `run` also sets `SessionConfig::control_flow` and
passes each Step's `outputVariables` to `run_task`, so `openjd_output_var:` lines are
collected and checked: an invalid directive, or a declared `ONCE` or `LAST` output with no
value, fails the Task, and the message appears in the Task's output.

## Step Selection Logic

The step selection has three modes:

1. **Explicit `--step`** — Run the named top-level Step or loop. Error if the name doesn't
   match any; a body Step's name gets the error that names its loop.
2. **Auto-select** — If the job has exactly one top-level Step or loop and no `--step` is
   provided, it is selected automatically.
3. **All steps** — If the job has several top-level entries and no `--step` is provided,
   all of them run in dependency order. Explicit task params (`--task-param`, `--tasks`)
   are not allowed in this mode because they're ambiguous across steps.

A selected loop gets the flag rules of [Loops](#control_flow-loops). When
`--run-dependencies` is set with an explicit step, `resolve_step_dependencies()` computes
the transitive closure of dependencies over Steps and loops and returns them in
topological order (dependencies before dependents) via recursive DFS.

## Task Execution Modes

Within a selected step, tasks execute in one of three modes:

### No Parameter Space

If the step has no `parameterSpace`, a single task runs with no task parameters.
Supplying `--task-param` or `--tasks` for such a step is rejected during preflight,
including for a `CONTROL_FLOW` Step pending inputs at Job creation: whether a Step has a
`parameterSpace` comes from the template, not from the materialized space. They are also
rejected during preflight for a Step skipped at Job creation.

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

A loop's body Step instances (RFC 0013) are the exception: an instance's Step
Environments, and whether each is enabled, are known only when its iteration
starts, so its stack is checked as the instance starts, before its first Step
Environment is entered. A conflict prints `ERROR: RFC 0008: ...`, fails the
instance with that reason, and so fails the loop and the run, which still
reports its result (see [Loops](#control_flow-loops)).

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
Working directory preserved at: /tmp/OpenJD/k3x9_q2m

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

* `steps`: one entry per Step, in template order, `{name, state, task_count}` with RFC 0011's state name
  (`PENDING_INPUTS`, `READY`, `SKIPPED`, `SUCCEEDED`, `FAILED`), or `BLOCKED`, the
  simulator's own name for a Step that can never run because a dependency failed (RFC 0011
  has no state for it: "blocked; never runs, as today"), and
  `task_count` `null` while its Tasks are unknown; `incomplete: true` for a Step that did
  not run all of its Tasks; `reason` for a failed or blocked Step; and, for a Step that
  declares output variables and started, `outputs`, a list with one map from output name
  to value per Task in iteration order, up to the last Task whose outputs arrived (`null`
  for a Task before it that did not succeed), so a Step whose Tasks all succeeded has one
  per Task and a zero-Task Step has `[]`. For Tasks given with `--tasks` or `--task-param`
  that do not cover the Step, `outputs` lists them in the order they ran, and
  `outputs_in_run_order: true` says so; the same holds for such a Step that failed, which
  keeps the outputs of the given Tasks that succeeded. A producer whose outputs
  `--step-inputs` gave has `inputs_supplied: true` and stays `READY`, since it did not
  run.

  A loop has its own entry, `{name, kind: "loop", state, max_iterations, iterations,
  outputs, iteration_values}`: `state` may also be `RUNNING`; `max_iterations` once known;
  `iterations` the number it ran (or started, for a loop that did not succeed); `outputs`
  the committed loop outputs, for a loop that succeeded or is skipped (`null`s); and
  `iteration_values`, one `{iteration, variables, outputs?}` per iteration started, with
  its loop variable values and the loop output values its boundary evaluated. `reason` and
  `inputs_supplied` are as for a Step. One entry per body Step instance follows it, in run
  order, with `name` `<Loop>[<N>].<Step>`, `loop`, `iteration`, and the fields of a Step.
  When an instance's group fails as its iteration starts, `start_iteration` returns no
  instances, so that iteration has one entry, `{name, state: "FAILED", task_count: null,
  reason, loop, iteration}`, for the instance that failed. A boundary that fails after
  evaluating the loop outputs does not report them, since `evaluate_iteration_boundary`
  returns only the error.
* `job_outputs`: a map from each evaluated job output's name to its value (`null` for no
  value).
* `job_outputs_not_evaluated`: a list of `{name, waits_on}`, as printed at the end, or
  `{name, failed: true}` for a job output whose evaluation failed.

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
- Exit code 1: Any action failed, setup failed, interruption was observed, or (for
  `CONTROL_FLOW`) a Step's materialization group, a job output, or a loop failed (a loop
  group, an instance group, an iteration boundary, `maxIterations`, `minLength`, a missing
  input, or an RFC 0008 conflict in a body Step instance; see
  [Loops](#control_flow-loops))

A Step or loop that is only blocked, by a failed dependency or by incomplete inputs, does not
by itself make the run fail. On failure, the session continues to exit environments
(cleanup) but skips remaining tasks. The `session_failed` flag tracks whether any action
returned a non-`Success` state, or a group, job output, or loop failed.

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
