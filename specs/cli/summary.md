# summary Command

## Purpose

`openjd summary <path>` prints summary information about a job template: its parameters,
steps, task counts, environments, and dependencies. It can summarize the entire job or a
single step. The command instantiates the job (resolving parameters and creating the job
object) to produce accurate task counts that reflect parameter space expansion.

## Interface

```
openjd summary <PATH> [--step <STEP>] [-p <KEY=VALUE>]... [--extensions <EXT>]
    [--environment <PATH>]... [--output <FMT>]
```

### Arguments

| Argument | Type | Required | Description |
|----------|------|----------|-------------|
| `path` | `PathBuf` | Yes | Path to the job template file |
| `--step` | `Option<String>` | No | Print summary for this step only |
| `-p`, `--parameter` | `Vec<String>` | No | Job parameters (`Key=Value`, `file://path`, or inline JSON) |
| `--extensions` | `Option<String>` | No | Comma-separated extension names |
| `--environment` (alias `--env`) | `Vec<PathBuf>` | No | Environment Template files, attached in the order given, as for `openjd run` |
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
    #[arg(long = "environment", alias = "env")]
    pub environments: Vec<PathBuf>,
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
  ├── Read and decode the --environment templates
  │
  ├── Preserve parameter definition order from template
  │   └── param_order: Vec<String> from job_template.parameter_definitions_list()
  │
  ├── Parse CLI parameters via run::parse_cli_parameters()
  ├── Resolve job_template_dir and current_working_dir
  ├── preprocess_job_parameters() → param_values (merging the Environment
  │     Templates' Job Parameters)
  ├── create_job_with_environment_templates() with the --environment
  │     templates, as `openjd run` does → Job; print its warnings to stderr.
  │     context: common::submission_context(&job_template), the same one
  │     `openjd run` uses: the caller limits the decode ran under and the
  │     CLI's submission path policy (see run.md § Session Configuration /
  │     check.md § Caller-Limits Policy). Each Step's
  │     environment_parameters covers the attached Environments. The
  │     Job's job_environments are filtered back to the Job Template's own
  │     (by CreatedJob::environment_documents), so the environment listings
  │     and counts are unchanged by --environment
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
   (name and type), environment count, dependency count, and (`ENV_PARAMS`) the Step's
   effective Environment parameter values: for each Environment its Sessions enter that
   declares parameters, in the order its Sessions enter them (attached Environments first,
   then the Job Template's), the Environment's name and declaring document
   (`JobTemplate`, or the `--environment` path) and each parameter's name, type, and value
   before path mapping, in parameter-name order, with the value in the display form `Param.*`
   values use (`common::environment_parameter_entries`; a list is `[1, 2]` or
   `["/a", "/b"]`; a RANGE_EXPR value in canonical form, so `1,2,3` shows as `1-3`).
   With `ENV_PARAMS` (see below for which documents), also the Step's **Environment set** and its
   **combined host requirements**: `Session environments:` lists every Environment its
   Sessions enter, in the order they enter them (the combined Job Environments the Step
   enters, `job::Step::enters_environment`, attached ones first, each with its declaring
   document; then its Step Environments, marked `stepEnvironments`; `(none)` when there are
   none), computed from the combined Job before its Environment listing is filtered;
   `Disabled environments:` lists, in the same form, the Environments its Sessions would
   otherwise have entered that their own `enableIf` (Template Schemas §4 item 6) disabled for
   the Step (`CreatedJob::disabled_environments`: combined Job Environments in Job order with
   their document, then Step Environments marked `stepEnvironments`), and is omitted when
   there are none; and
   `Host requirements:` lists `job::Step::host_requirements`, which job creation has combined
   from the Step's own and its entered Environments' (Template Schemas §3.3.3), one line per
   requirement (`name: min 1, max 2`, `name: anyOf a, b`, `name: allOf a, b`), omitted when the
   Step has none. Both are shown when the Job Template or any `--environment` template lists
   `ENV_PARAMS` (an attached Environment's `activation` and `hostRequirements` apply when its own
   document lists it); when no document does, neither is shown, so such a summary is unchanged.
4. **Environments** — Root (job-level) and step-level environments with names and parent
   context.

The environment totals, each Step's environment count, and the Environments listing come from
the created Job, so a Step Environment whose `enableIf` (`ENV_PARAMS`, Template Schemas §4
item 6) is `false` for its Step is not counted or listed there (job creation drops it from
`job::Step::step_environments`), while a Job Environment disabled for some or every Step is,
since the Job keeps it; each Step's `Disabled environments:` (item 3 above) names both.

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

With `ENV_PARAMS` values (`openjd summary video-sfm-composite.yaml --environment
conda-env-params.yaml`):

```
2. 'SolveSfM' (1 total Tasks)
  1 dependencies
  Session environments:
    - Conda (conda-env-params.yaml)
  Host requirements:
    - amount.worker.gpu: min 1
  Environment parameters:
    Conda (conda-env-params.yaml):
      - Channels (STRING): conda-forge
      - Packages (STRING): colmap=3.11
```

### JSON Format

The JSON output includes a `status` and `message` field for consistency with the Python
CLI's `OpenJDCliResult` pattern, plus structured data for all summary fields. Step
parameter definitions are arrays of `{"name", "type"}` objects. Optional fields
(`description`, `environments`, `dependencies`, `environment_parameters`) are omitted when
empty. `environment_parameters` is an array of `{"environment", "document", "values"}`
objects whose `values` are `{"name", "type", "value"}` objects, with the same strings as the
human-readable form. With `ENV_PARAMS`, each Step also has `session_environments`, an array
of `{"name", "source"}` objects (`source` is the declaring document, or `stepEnvironments`),
when an `enableIf` disabled any, `disabled_environments` in the same form,
and, when it has any, `host_requirements`, `{"amounts": [{"name", "min"?, "max"?}],
"attributes": [{"name", "anyOf"?, "allOf"?}]}` with each list present when non-empty and
amounts as numbers.

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
- With `ENV_PARAMS`, the Step's Environment set, the Environments an `enableIf` disabled for
  it, and its combined host requirements, as in the job summary, under `Session
  environments:`, `Disabled environments:`, and `Host requirements:` headings and in the
  JSON `session_environments`, `disabled_environments`, and `host_requirements` fields
- Environment parameters (`ENV_PARAMS`), as in the job summary, under an
  `Environment parameters:` heading and in the JSON `environment_parameters` field

The step is looked up by name in `job.steps`. If the step name doesn't match any step,
an error is returned with the job name for context.

## Internal Types

Six private structs organize the collected summary data:

```rust
struct StepInfo<'a> {
    name: String,
    description: Option<String>,
    total_tasks: usize,
    task_params: Vec<(String, String)>,  // (name, type_name)
    envs: Vec<String>,
    deps: Vec<String>,
    env_params: Vec<EnvParamInfo>,
    env_params_details: Option<EnvParamsDetails<'a>>, // Some with ENV_PARAMS
}

struct EnvParamsDetails<'a> {
    session_envs: &'a [SessionEnvInfo],               // the Step's Environment set
    host_requirements: Option<&'a job::HostRequirements>, // combined (§3.3.3)
}

struct SessionEnvInfo {
    name: String,
    source: String,                      // the declaring document, or `stepEnvironments`
}

struct EnvParamInfo {
    environment: String,
    document: String,                    // `JobTemplate` or the --environment path
    values: Vec<(String, String, String)>, // (name, TYPE, display value), in name order
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
}
```

These are display-oriented — they hold string representations ready for output, except that
`EnvParamsDetails` borrows the Step's `job::HostRequirements`, which it formats when
written. Task parameters are sorted alphabetically by name for deterministic
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
