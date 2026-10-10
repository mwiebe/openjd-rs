# summary Command

## Purpose

`openjd summary <path>` prints summary information about a job template: its parameters,
steps, task counts, environments, dependencies, and — with the `SERVICE` extension (RFC 0009)
— its Services and required Services. It can summarize the entire job or a
single step. The command instantiates the job (resolving parameters and creating the job
object) to produce accurate task counts that reflect parameter space expansion. With
`--environment`, the attached Environment Templates are applied as `run` applies them, so the
summary is of the combined Job: attached Environments and Services appear, and each
`requiresServices` entry is shown with the attachment that satisfies it (an unsatisfied
requirement is the same error `run` reports).

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
  ├── With --environment: apply_environment_templates() → combined Job +
  │     requirement bindings (requirement name → attachment label)
  │
  └── Dispatch on --step
      ├── Some(step_name) → output_step_summary(&job, step_name, output_format)
      └── None → output_job_summary(&job, &param_order, &bindings, output_format)
```

## Parameter Definition Order

The summary preserves the order in which parameters are defined in the template file. This
is important for human-readable output — users expect to see parameters in the order they
wrote them, not sorted alphabetically or in hash map iteration order.

The implementation captures the order from `job_template.parameter_definitions_list()` before
job creation (which stores parameters in a `HashMap`), then uses that order vector when
iterating over `job.parameters` for display.

## Task Count Calculation

`step_total_tasks()` computes the total number of tasks for a step by constructing a
`StepParameterSpaceIterator` with a chunk override of 1. The chunk override ensures that
chunked parameters (CHUNK[INT]) are counted as individual tasks rather than chunks, giving
the true task count.

```rust
fn step_total_tasks(step: &job::Step) -> usize {
    match &step.parameter_space {
        None => 1,
        Some(ps) => {
            match StepParameterSpaceIterator::new_with_chunk_override(ps, Some(1)) {
                Ok(it) => it.len(),
                Err(_) => 1,
            }
        }
    }
}
```

Steps with no parameter space have exactly 1 task (the implicit single task).

## Job Summary Output

`output_job_summary()` collects and displays:

1. **Parameters** — Name, type, and resolved value for each job parameter, in template
   definition order.
2. **Totals** — Total steps, total tasks (sum across all steps), total environments
   (root + step environments).
3. **Per-step details** — Name, description, task count, task parameter definitions
   (name and type), environment count, dependency count. With `SERVICE`, a count that
   includes a Service dependency reads `3 dependencies (1 Step, 2 Service)` (or `1
   dependencies (all Services)`) and is followed by `Services: 'Coord', 'Cache'`; a
   Step-only count stays `N dependencies`.
4. **Services** (RFC 0009) — each `services` entry (the Job Template's own, then with
   `--environment` the attached ones, named `<name> (from <document>)`): scope (the
   `Display` of `job::ServiceScope`: `every Step` / `Step Work` / `Steps A, B`),
   description, `Ports: api (TCP), metrics (TCP, port 9100)` (protocol, and the pinned
   number when the template gives one), `Health check: <TYPE>`, `Restart policy:
   maxAttempts N, completedTasks KEEP|RERUN|none` (`none` when the template gave none,
   which it may only with `maxAttempts` 0; JSON `"completed_tasks": null`), and
   `Dependencies: 'Prepare', Service 'Back'` (a `dependsOn` entry as `'Name'`, a `service`
   entry as `Service 'Name'`) when it lists any.
5. **Required Services** (RFC 0009 §9.8) — each `requiresServices` entry with its ports
   and either `— satisfied by <document>` (the attachment `apply_environment_templates`
   bound it to) or `— not satisfied: attach an Environment Template that declares it with
   --environment`.
6. **Environments** — Root (job-level) and step-level environments with names and parent
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

With Services:

```
2. 'Work' (1 total Tasks)
  3 dependencies (1 Step, 2 Service)
    Services: 'Coord', 'Cache'


--- Services in 'ServiceJob' ---
  - Coord (scope: Step Work)
    Hands out work items.
    Ports: api (TCP), metrics (TCP, port 9100)
    Health check: STDOUT
    Restart policy: maxAttempts 2, completedTasks KEEP
    Dependencies: 'Prepare'

--- Required Services in 'ServiceJob' ---
  - Cache (ports: main (TCP), stats (UDP)) — not satisfied: attach an Environment Template that declares it with --environment
```

### JSON Format

The JSON output includes a `status` and `message` field for consistency with the Python
CLI's `OpenJDCliResult` pattern, plus structured data for all summary fields. Step
parameter definitions are arrays of `{"name", "type"}` objects. Optional fields
(`description`, `environments`, `dependencies`) are omitted when empty. With `SERVICE`: a
step with Service dependencies also carries `service_dependencies` (the Service names, in
list order; `dependencies` stays the total count); the root carries `services` — each
`{"name", "description"?, "document"?, "scope", "ports": [{"name", "protocol", "port"?}],
"health_check", "restart_policy": {"max_attempts", "completed_tasks"}, "dependencies"?:
[{"step_name"} | {"service_name"}]}` — and `requires_services` — each `{"name", "ports":
[{"name", "protocol"}], "satisfied_by"?}` — when the Job has any.

### YAML Format

The YAML output contains the same structured data as JSON, serialized via
`serde_yaml::to_string()`. Both formats share the same `serde_json::Value` construction
code, so they are always in sync.

## Step Summary Output

`output_step_summary()` displays details for a single step:

- Total tasks, total task parameters, total environments
- Dependencies — `'Prepare'` for a Step, `Service 'Coord'` for a Service (JSON:
  `{"step_name"}` / `{"service_name"}` entries)
- Services in scope — the Services whose computed scope includes the Step (RFC 0009 §9.1:
  those it lists, those reached through them, and every Job-wide one; JSON `services`)
- Parameter definitions (name and type)
- Environments (name, parent step, description)

The step is looked up by name in `job.steps`. If the step name doesn't match any step,
an error is returned with the job name for context.

## Internal Types

Private structs organize the collected summary data:

```rust
struct StepInfo {
    name: String,
    description: Option<String>,
    total_tasks: usize,
    task_params: Vec<(String, String)>,  // (name, type_name)
    envs: Vec<String>,
    deps: Vec<DepInfo>,
}

struct DepInfo { name: String, is_service: bool }           // one `dependencies` entry: dependsOn (false) or service (true)
struct PortInfo { name: String, protocol: String, port: Option<u16> }
struct ServiceInfo {                                        // one `services` entry
    name: String, description: Option<String>, document: Option<String>, scope: String,
    ports: Vec<PortInfo>, health_check: String, max_attempts: u64, completed_tasks: Option<String>,
    deps: Vec<DepInfo>,
}
struct RequirementInfo { name: String, ports: Vec<PortInfo>, satisfied_by: Option<String> }

struct ParamInfo {
    name: String,
    param_type: String,
    value: String,
}

struct EnvInfo {
    name: String,
    description: Option<String>,
    parent: String,
}
```

These are display-oriented — they hold string representations ready for output, not the
original model types. Task parameters are sorted alphabetically by name for deterministic
output.

## Task Parameter Type Names

`task_param_type_name()` maps `job::TaskParameter` variants to their specification string
names:

| Variant | Display Name |
|---------|-------------|
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
