# summary Command

## Purpose

`openjd summary <path>` prints summary information about a job template: its parameters,
steps, task counts, environments, and dependencies. It can summarize the entire job or a
single step. The command instantiates the job (resolving parameters and creating the job
object) to produce accurate task counts that reflect parameter space expansion.

For a `CONTROL_FLOW` Job, `create_job` may leave a Step skipped or pending inputs, and
`summary` reports what it knows of each (plan C2, RFC 0011 §Implementation strategy), as
described in [CONTROL_FLOW](#control_flow) below. A job output that fails at creation
fails `create_job`, so `summary` reports that error. A loop (RFC 0013) is shown with its
bound and body, as described in [Loops](#loops) below.

## Interface

```
openjd summary <PATH> [--step <STEP>] [-p <KEY=VALUE>]... [--extensions <EXT>] [--output <FMT>]
```

### Arguments

| Argument | Type | Required | Description |
|----------|------|----------|-------------|
| `path` | `PathBuf` | Yes | Path to the job template file |
| `--step` | `Option<String>` | No | Print summary for this Step or loop only (not a loop's body Step) |
| `-p`, `--parameter` | `Vec<String>` | No | Job parameters (`Key=Value`, `file://path`, or inline JSON) |
| `--extensions` | `Option<String>` | No | Comma-separated extension names |
| `--output` | `String` | No | Output format: `human-readable` (default), `json`, `yaml` |

### SummaryArgs Struct

```rust
#[derive(Args)]
pub struct SummaryArgs {
    pub path: PathBuf,
    #[arg(long)]
    pub step: Option<String>,
    #[arg(short = 'p', long = "parameter")]
    pub parameters: Vec<String>,
    #[arg(long = "extensions")]
    pub extensions: Option<String>,
    #[arg(long = "output", value_parser = ["human-readable", "json", "yaml"], default_value = "human-readable")]
    pub output: String,
}
```

## Execution Pipeline

```
execute(args)
  │
  ├── Read and parse template (same pattern as check: common::read_input_file)
  ├── Resolve extensions list
  ├── decode_job_template()
  │
  ├── Preserve parameter definition order from template
  │   └── param_order: Vec<String> from job_template.parameter_definitions_list()
  │
  ├── Parse CLI parameters via run::parse_cli_parameters()
  ├── Resolve job_template_dir and current_working_dir
  ├── preprocess_job_parameters() → param_values
  ├── create_job() → Job
  │     context: job_template.default_validation_context() with
  │     common::caller_limits() layered on — the same limits the decode
  │     ran under (see run.md § Session Configuration / check.md § Caller-Limits Policy)
  │
  └── Dispatch on --step
      ├── Some(step_name) → output_step_summary(&job, step_name, output_format)
      └── None → output_job_summary(&job, &param_order, output_format)
```

## Parameter Definition Order

The summary preserves the order in which parameters are defined in the template file. This
is important for human-readable output — users expect to see parameters in the order they
wrote them, not sorted alphabetically or in hash map iteration order.

The implementation captures the order from `job_template.parameter_definitions_list()` before
job creation (which stores parameters in a `HashMap`), then uses that order vector when
iterating over `job.parameters` for display.

## Task Count Calculation

`step_total_tasks()` is `job::Step::tasks_known()`: the number of Tasks with chunking
disabled (so `CHUNK[INT]` parameters count individual Tasks, not chunks), 1 for a Step
with no parameter space, 0 for a skipped Step, and `None` for a pending Step whose Tasks
are determined at run time. When `tasks_known()` has no count for a bound Step, its count
overflowed, and `step_total_tasks()` reports that error.

```rust
fn step_total_tasks(step: &job::Step) -> Result<Option<u64>, String> {
    if let Some(n) = step.tasks_known() {
        return Ok(Some(n));
    }
    match (&step.binding, &step.parameter_space) {
        (job::StepBinding::Bound, Some(ps)) => {
            StepParameterSpaceIterator::new_with_chunk_override(ps, Some(1))
                .map(|it| Some(it.len()))
                .map_err(|e| format!("Step '{}': {e}", step.name))
        }
        _ => Ok(None),
    }
}
```

The total sums the Steps whose Tasks are known. A loop's Tasks are known only once it ends
(RFC 0013 §Iterations), so a loop counts as determined at run time, unless it is skipped
or its first `while` is false at Job creation (0 Tasks).

Task counts are `u64`, the type of `StepParameterSpaceIterator::len`, so they are exact on
every target, including one where `usize` is 32 bits. The job summary's total is their
checked sum: when it passes `u64::MAX` (two steps of 2^63 tasks) the command fails with
`Job '<name>' has more than 18446744073709551615 tasks in total, so the total cannot be
reported.` rather than printing a wrapped total. A single step can have more than
`u64::MAX` tasks too (a chunked step: the job has its chunks, but counting one task per value
overflows); the summary then fails with `Step '<name>': Validation error: Total parameter
space size overflow: the product of parameter dimensions is too large.` rather than reporting
a count.

## Job Summary Output

`output_job_summary()` collects and displays:

1. **Parameters** — Name, type, and resolved value for each job parameter, in template
   definition order.
2. **Totals** — Total steps, total tasks (sum across all steps), total environments
   (root + step environments).
3. **Per-step details** — Name, description, task count, task parameter definitions
   (name and type), environment count, dependency count.
4. **Environments** — Root (job-level) and step-level environments with names and parent
   context.

### Human-Readable Format

```
--- Summary for 'MyJob' ---

Parameters:
  - Frames (INT): 1-100
  - OutputDir (PATH): /output

Total steps: 2
Total tasks: 100
Total environments: 1

--- Steps in 'MyJob' ---

1. 'Render' (100 total Tasks)
  Task parameters:
    - Frame (INT)
  1 environments

2. 'Encode' (1 total Tasks)
  1 dependencies
```

### JSON Format

The JSON output includes a `status` and `message` field for consistency with the Python
CLI's `OpenJDCliResult` pattern, plus structured data for all summary fields. Step
parameter definitions are arrays of `{"name", "type"}` objects. Optional fields
(`description`, `environments`, `dependencies`) are omitted when empty.

For a `CONTROL_FLOW` template, each step entry also has `state` and, for a pending Step,
`binding` (see below), and `total_tasks` is `null` for a Step whose Tasks are unknown.
`tasks_determined_at_run_time: true` is added when any Step's are, and `job_outputs` when
the template defines any. A disabled Environment's entry has `enabled: false`.

### YAML Format

The YAML output contains the same structured data as JSON, serialized via
`serde_yaml::to_string()`. Both formats share the same `serde_json::Value` construction
code, so they are always in sync.

## Step Summary Output

`output_step_summary()` displays details for a single step:

- Total tasks, total task parameters, total environments
- Dependencies (step names)
- Parameter definitions (name and type)
- Environments (name, parent step, description)

The step is looked up by name in `job.steps`, and then in `job.loops` (see
[Loops](#loops)). If the step name doesn't match any step, an error is returned with the
job name for context.

## CONTROL_FLOW

For a template that lists `CONTROL_FLOW`:

* **Pending Steps.** A Step that is pending inputs is shown as `N. 'C' (pending inputs;
  Tasks determined at run time)`, or `(240 total Tasks; pending inputs)` once its Tasks are
  known, followed by one line per group from `common::binding_lines()`, which the run log
  uses too:

  ```
  3. 'C' (pending inputs; Tasks determined at run time)
    Enabled: yes
    Tasks: determined at run time from Steps.P.Output.N
    Host requirements: known
  ```

  `Enabled:` is `yes` or `determined at run time from <symbols>`. `Tasks:` is `<N> (known)`,
  `<N> (known; Step pending inputs for <symbols>)` when another group still waits (the A11
  case: a `let` read only by the script waits on an upstream output), `determined at run
  time from <symbols>` (with ` and enableIf` when the Step's `enableIf` is undecided too),
  or `determined at run time (waiting for enableIf)`. `Host requirements:` is `known` or
  as for Tasks. In JSON, `binding` has `enablement`, `tasks`, `host_requirements`, and
  `remaining`, each `{"known": true}` or `{"known": false, "waits_on": [...],
  "waits_for_enable_if": bool}`, from `job::Step::binding_report()`.
* **Skipped Steps** are shown as `N. 'S' (SKIPPED, 0 total Tasks)`, and with `state:
  "SKIPPED"` in JSON. Every Step has a `state` there: `READY`, `PENDING_INPUTS`, or
  `SKIPPED`.
* **Totals.** `Total tasks: N + (determined at run time)` when some Step's Tasks are
  unknown, where `N` counts every Step whose Tasks are known, pending or not.
* **Disabled Environments** are listed with `(from 'P', disabled by its enableIf)`.
* **Job outputs** are listed after the Environments, with their types and units, and
  either the value `create_job` evaluated or the `Steps.*` symbols each waits on that are
  not concrete at creation (`common::concrete_at_creation`, decided from the Steps'
  bindings without building any value), with each description indented under its item:

  ```
  --- Job outputs in 'Fan' ---
    - Frames (LIST[INT]): pending; waits for Steps.C.TaskParam.F
    - Total (INT, px): pending; waits for Steps.C.Output.Done
      Total pixels
  ```

  In JSON, `job_outputs` is a list of `{name, type, unit?, description?, state, value?,
  waits_on?}` with `state` `VALUE`, `NULL`, or `PENDING`.

`--step` reports the same for one Step: `Total tasks: determined at run time` (or `N
(SKIPPED)`), then the group lines, and `state` and `binding` in JSON.

### Loops

A loop (RFC 0013, plan L8) is listed in template order among the Steps, as `N. 'Refine'
(loop; Tasks determined at run time)` (`loop; pending inputs; Tasks determined at run
time`, `loop; SKIPPED, 0 total Tasks`, or `loop; 0 iterations, 0 total Tasks` when its
first `while` is false), followed by:

```
1. 'Refine' (loop; Tasks determined at run time)
  Condition: until
  maxIterations: 6
  Loop variables:
    - Samples (INT)
    - Frames (LIST[INT])
  Loop outputs:
    - FinalSamples (INT, LAST)
    - NoisyFrameCounts (LIST[INT], APPEND)
  Body Steps (Tasks determined at run time in each iteration):
    - 'RenderPass'
      Task parameters:
        - Frame (INT)
    - 'CheckPass'
      1 dependencies
```

A pending loop shows its group lines (`common::loop_binding_lines`, as in the run log)
before `Condition:`, in place of the `maxIterations:` line; a skipped loop shows
`maxIterations: not evaluated (the loop is skipped)`. Each loop output shows its type,
`collect`, and unit. A body Step shows its Task parameters, Step Environment count, and
dependency count, from its template, since no instance exists before the loop runs. Its
Step Environments are listed under Environments as `Name (from body Step 'S' of loop
'L')`.

`Total steps` counts each top-level Step and each body Step once (RFC 0013 §Limits item
3), and `Total loops: N` follows it when the Job has loops (`total_loops` in JSON). In
JSON, a loop's entry in `steps` is `{name, kind: "loop", description?, total_tasks, state,
binding?, condition, max_iterations?, variables?, outputs?, body, dependencies?}`, where
`total_tasks` is `null` unless the loop has 0 Tasks, `binding` has `enablement`, `bound`,
and `start` as a Step's groups are reported, `outputs` is a list of `{name, type, collect,
unit?}`, and `body` a list of `{name, parameter_definitions?, environments?,
dependencies?}`.

`--step L` naming a loop prints `--- Summary for loop 'L' in Job 'J' ---`, `State: READY
(loop; Tasks determined at run time)`, the same lines, and its dependencies. `--step`
naming a body Step fails with `Step 'Q' is a body Step of loop 'L'; summarize the loop
with --step L.`

## Internal Types

These private structs organize the collected summary data:

```rust
struct StepInfo {
    name: String,
    description: Option<String>,
    total_tasks: Option<u64>,            // None while determined at run time
    task_params: Vec<(String, String)>,  // (name, type_name)
    envs: Vec<String>,
    deps: Vec<String>,
    state: Option<&'static str>,         // CONTROL_FLOW only
    binding_lines: Vec<String>,          // a pending Step's group lines
    binding_json: Option<serde_json::Value>,
    loop_info: Option<LoopInfo>,         // for a loop's entry
}

struct LoopInfo {                        // see Loops
    name: String,
    description: Option<String>,
    state: &'static str,
    condition: &'static str,             // "while" or "until"
    max_iterations: Option<u32>,         // once known
    zero_iterations: bool,               // the first while is false at Job creation
    variables: Vec<(String, String)>,    // (name, type)
    outputs: Vec<LoopOutputInfo>,        // name, type, collect, unit
    body: Vec<BodyStepInfo>,             // name, task params, Step Environments, deps
    deps: Vec<String>,
    binding_lines: Vec<String>,          // a pending loop's group lines
    binding_json: Option<serde_json::Value>,
}

struct ParamInfo {
    name: String,
    param_type: String,
    value: String,
}

struct EnvInfo {
    name: String,
    description: Option<String>,
    parent: String,
    parent_loop: Option<String>,         // a body Step's Step Environment
    enabled: bool,
}

struct JobOutputInfo {
    name: String,
    output_type: String,
    unit: Option<String>,
    description: Option<String>,
    value: Option<job::JobOutputValue>,  // when create_job evaluated it
    waits_on: Vec<String>,               // Steps.* symbols not yet concrete
}
```

These are display-oriented — they hold string representations ready for output, not the
original model types. Task parameters are sorted alphabetically by name for deterministic
output.

## Task Parameter Type Names

Task parameter names and types come from the Job Template's step definitions
(`TaskParameterDefinition::task_param_type().as_spec_str()`), not from the job's
materialized parameter space, so a `CONTROL_FLOW` Step that is pending inputs still lists
them. The names are:

| Type | Display Name |
|------|-------------|
| `Int` | `INT` |
| `Float` | `FLOAT` |
| `String` | `STRING` |
| `Path` | `PATH` |
| `ChunkInt` | `CHUNK[INT]` |

## Differences from Python CLI

| Aspect | Python | Rust |
|--------|--------|------|
| Result type | `OpenJDJobSummaryResult` / `OpenJDStepSummaryResult` dataclasses | Inline output in `output_*_summary()` functions |
| Output decorator | `@print_cli_result` handles format dispatch | Manual `match` on output format string |
| JSON/YAML parity | Identical structured output in both | Identical — same value, different serializer |
| Parameter display | Shows `description` field | Shows `value` field |

The Python CLI's summary shows parameter descriptions (from the template definition) while
the Rust CLI shows resolved parameter values. Both are useful — the Python approach helps
users understand what parameters mean, while the Rust approach shows what values will be
used for job execution.
