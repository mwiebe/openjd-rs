# summary Command

## Purpose

`openjd summary <path>` prints summary information about a job template: its parameters,
steps, task counts, environments, and dependencies. It can summarize the entire job or a
single step. The command instantiates the job (resolving parameters and creating the job
object) to produce accurate task counts that reflect parameter space expansion.

For a `CONTROL_FLOW` Job, `create_job` may leave a Step skipped or pending inputs, and
`summary` reports what it knows of each (plan C2, RFC 0011 §Implementation strategy), as
described in [CONTROL_FLOW](#control_flow) below. A job output that fails at creation
fails `create_job`, so `summary` reports that error.

## Interface

```
openjd summary <PATH> [--step <STEP>] [-p <KEY=VALUE>]... [--extensions <EXT>] [--output <FMT>]
```

### Arguments

| Argument | Type | Required | Description |
|----------|------|----------|-------------|
| `path` | `PathBuf` | Yes | Path to the job template file |
| `--step` | `Option<String>` | No | Print summary for this step only |
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
are determined at run time.

```rust
fn step_total_tasks(step: &job::Step) -> Option<usize> {
    step.tasks_known().map(|n| n as usize)
}
```

The total sums the Steps whose Tasks are known.

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

The step is looked up by name in `job.steps`. If the step name doesn't match any step,
an error is returned with the job name for context.

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

## Internal Types

Four private structs organize the collected summary data:

```rust
struct StepInfo {
    name: String,
    description: Option<String>,
    total_tasks: Option<usize>,          // None while determined at run time
    task_params: Vec<(String, String)>,  // (name, type_name)
    envs: Vec<String>,
    deps: Vec<String>,
    state: Option<&'static str>,         // CONTROL_FLOW only
    binding_lines: Vec<String>,          // a pending Step's group lines
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
