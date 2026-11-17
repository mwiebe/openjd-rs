# summary Command

## Purpose

`openjd summary <path>` prints summary information about a job template: its parameters,
steps, task counts, environments, dependencies, and — with the `SERVICE` extension (RFC 0009)
— its Services and required Services. It can summarize the entire job or a
single step. The command instantiates the job (resolving parameters and creating the job
object) to produce accurate task counts that reflect parameter space expansion. With
`--environment`, the attached Environment Templates are applied as `run` applies them, so the
summary is of the combined Job: attached Environments and Services appear, and each
`externalServices` entry is shown with the attachment that satisfies it (an unsatisfied
requirement is the same error `run` reports).

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
  │     templates, as `openjd run` does (create_job_details() without any, so
  │     an `externalServices` entry is shown unsatisfied rather than
  │     rejected) → Job; print its warnings to stderr.
  │     context: common::submission_context(&job_template), the same one
  │     `openjd run` uses: the caller limits the decode ran under (see
  │     run.md § Session Configuration / check.md § Caller-Limits
  │     Policy). Each Step's
  │     environment_parameters covers the attached Environments. The
  │     Job's job_environments are filtered back to the Job Template's own
  │     (by CreatedJob::environment_documents), so the environment listings
  │     and counts are unchanged by --environment.
  │     The Job's services keep the attached ones (RFC 0009), and
  │     CreatedJob::requirement_bindings give each externalServices entry's
  │     provider (requirement name → attachment label)
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
fn step_total_tasks(step: &job::Step) -> Result<u64, String> {
    match &step.parameter_space {
        None => Ok(1),
        Some(ps) => StepParameterSpaceIterator::new_with_chunk_override(ps, Some(1))
            .map(|it| it.len())
            .map_err(|e| format!("Step '{}': {e}", step.name)),
    }
}
```

Steps with no parameter space have exactly 1 task (the implicit single task).

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
   the Step, or that an earlier Environment of their `exclusiveGroup` superseded (§4 item 7;
   with the Conda and Docker samples both attached to the video sample, whose Steps request
   both and give Conda packages, the one attached second;
   the `Disabled` and `Superseded` items of `CreatedJob::environment_statuses`: combined Job
   Environments in Job order with their document, then Step Environments marked
   `stepEnvironments`), each line ending `: disabled` or, for a superseded one,
   `: superseded by Name (document)`, naming the member of its group the Step enters
   (`StepEnvironmentStatus::superseded_by`), and is
   omitted when there are none. An Environment with `activation: ALWAYS` is never listed: it
   cannot be disabled, and an `enableIf` that is `false` for a Step makes job creation, and so
   `summary`, fail with an error naming the Step and the Environment (Template Schemas §3.2.2
   item 6). A `BY_DEFAULT` Environment the Step declines with a disabled `environment:`
   entry (§3.2 item 3.2) is not listed there, since its own `enableIf` is not evaluated for
   the Step; `Declined environments:` lists those, in the same form without a suffix (the
   `Declined` items), and is omitted when there are none. `Optional entries not found:` lists
   the Step's enabled `required: false` entries that named no Environment (§3.2.1 item 2,
   `StepEnvironmentStatuses::not_found`), one `- dependencies[i]: Name` line each, and is
   omitted when there are none; each also prints its warning on stderr, as for every
   subcommand; and
   `Host requirements:` lists `job::Step::host_requirements`, which job creation has combined
   from the Step's own and its entered Environments' (Template Schemas §3.3.3), one line per
   requirement (`name: min 1, max 2`, `name: anyOf a, b`, `name: allOf a, b`), omitted when the
   Step has none; then `Discarded host requirements:` lists the requirements that resolved to
   nothing for the Step and were discarded (Template Schemas §3.3,
   `CreatedJob::discarded_host_requirements`), the Step's own first, one line each:
   `amount amount.worker.gpu (Step)` for the Step's own, or `amount amount.worker.gpu
   (Environment Docker, docker-env-params.yaml)` with the Environment and its document,
   omitted when there are none. These are shown when the Job Template or any `--environment` template lists
   `ENV_PARAMS` (an attached Environment's `activation` and `hostRequirements` apply when its own
   document lists it); when no document does, none of them is shown, so such a summary is unchanged.
   With `SERVICE`, a count that
   includes a Service dependency reads `3 dependencies (1 Step, 2 Service)` (or `1
   dependencies (all Services)`) and is followed by `Services: 'Coord', 'Cache'`; a
   Step-only count stays `N dependencies`.
4. **Services** (RFC 0009) — each `services` entry (the Job Template's own, then with
   `--environment` the attached ones, named `<name> (from <document>)`): scope (the
   `Display` of `job::ServiceScope`: `every Step` / `Step Work` / `Steps A, B`),
   description, `Ports: api (TCP), metrics (TCP, port 9100)` (protocol, and the pinned
   number when the template gives one), `Health check: <TYPE>`, `Restart policy:
   maxRestarts N, completedTasks KEEP|RERUN|none` (`none` when the template gave none,
   which it may only with `maxRestarts` 0; JSON `"completed_tasks": null`), and
   `Dependencies: 'Prepare', Service 'Back'` (a `dependsOn` entry as `'Name'`, a `service`
   entry as `Service 'Name'`) when it lists any. Then, as for a Step and in the same forms
   indented under the Service: `Session environments:` lists the combined Job Environments
   its Sessions enter (`job::Service::enters_environment`, RFC 0009 "Services run inside
   Environments", which chooses them exactly as for a Step: every `ALWAYS` one, every
   `BY_DEFAULT` one it does not decline, and each `ON_REQUEST` one it requests, less any
   whose `enableIf` is false for it or that an earlier one of its `exclusiveGroup`
   supersedes; an attached Service chooses only among its own document's and the earlier
   attachments' Environments), attached ones first,
   each with its declaring document, or `(none)`; `Disabled environments:`, `Declined
   environments:` and `Optional entries not found:` list the Service's
   `CreatedJob::service_environment_statuses` in the Step's forms, each omitted when it is
   empty (an attached Service, which requests nothing, has no optional entries);
   `Host requirements:` lists
   `job::Service::host_requirements`, which job creation has combined from the Service's own
   and its entered Environments' (RFC 0009 `<Service>` item 5, Template Schemas §3.3.3),
   omitted when it has none; `Discarded host requirements:` lists
   `CreatedJob::discarded_service_host_requirements` for the Service, in the Step's form
   with `(Service)` for its own, omitted when there are none; and `Environment parameters:` lists the Service's effective
   values (`job::Service::environment_parameters`, RFC 0009 `<Service>` item 4), omitted when
   there are none. In the JSON form a Service gets the same keys as a Step:
   `disabled_environments` (each with its `status`, `exclusive_group` and, for a superseded
   one, `superseded_by`), `declined_environments` and `optional_entries_not_found`, each
   omitted when it is empty.
5. **Required Services** (RFC 0009 §9.8) — each `externalServices` entry with its ports
   and either `— satisfied by <document>` (the attachment `create_job_with_environment_templates`
   bound it to) or `— not satisfied: attach an Environment Template that declares it with
   --environment`.
6. **Environments** — Root (job-level) and step-level environments with names and parent
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
    Restart policy: maxRestarts 2, completedTasks KEEP
    Dependencies: 'Prepare'

--- Required Services in 'ServiceJob' ---
  - Cache (ports: main (TCP), stats (UDP)) — not satisfied: attach an Environment Template that declares it with --environment
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
when an `enableIf` disabled or an `exclusiveGroup` superseded any, `disabled_environments`,
`{"name", "source", "status"}` objects with `status` `disabled` or `superseded`, plus
`"exclusive_group"` when the Environment has one and, on a superseded one, `"superseded_by"`,
`{"name", "source"}` of the group member the Step enters, when it
declined any, `declined_environments` in the `session_environments` form, when an optional
entry named nothing, `optional_entries_not_found`, `{"dependency_index", "environment"}`
objects,
and, when it has any, `host_requirements`, `{"amounts": [{"name", "min"?, "max"?}],
"attributes": [{"name", "anyOf"?, "allOf"?}]}` with each list present when non-empty and
amounts as numbers, and, when any were discarded, `discarded_host_requirements`, an array of
`{"kind", "name", "environment"?}` objects (`kind` is `amount` or `attribute`;
`environment`, `{"name", "source"}`, is absent for the Step's own).
With `SERVICE`: a
step with Service dependencies also carries `service_dependencies` (the Service names, in
list order; `dependencies` stays the total count); the root carries `services` — each
`{"name", "description"?, "document"?, "scope", "ports": [{"name", "protocol", "port"?}],
"health_check", "restart_policy": {"max_restarts", "completed_tasks"}, "dependencies"?:
[{"step_name"} | {"service_name"}], "session_environments", "host_requirements"?,
"discarded_host_requirements"?, "environment_parameters"?}`, the last four in a Step's forms
(`session_environments` always present, possibly empty; the others when non-empty) — and
`external_services` — each `{"name", "ports": [{"name", "protocol"}], "satisfied_by"?}` — when
the Job has any.

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
- With `ENV_PARAMS`, the Step's Environment set, the Environments an `enableIf` disabled or
  an `exclusiveGroup` superseded for it, the ones it declined, its optional entries that named
  nothing, its combined host requirements, and its discarded host requirements, as in the job
  summary, under `Session environments:`, `Disabled environments:`, `Declined environments:`,
  `Optional entries not found:`, `Host requirements:`, and `Discarded host requirements:`
  headings and in the JSON `session_environments`, `disabled_environments`,
  `declined_environments`, `optional_entries_not_found`, `host_requirements`, and
  `discarded_host_requirements` fields
- Environment parameters (`ENV_PARAMS`), as in the job summary, under an
  `Environment parameters:` heading and in the JSON `environment_parameters` field

The step is looked up by name in `job.steps`. If the step name doesn't match any step,
an error is returned with the job name for context.

## Internal Types

These private structs organize the collected summary data (the result types that print
it, `JobSummaryResult` and `StepSummaryResult`, are not shown):

```rust
struct StepEnvSets {                     // per Step with ENV_PARAMS, and per Service
    entered: Vec<SessionEnvInfo>,        // the Environments its Sessions enter
    disabled: Vec<DisabledEnvInfo>,      // disabled by an enableIf or superseded (§4 items 6, 7)
    declined: Vec<SessionEnvInfo>,       // declined by a disabled entry (§3.2 item 3.2)
    not_found: Vec<(usize, String)>,     // optional entries that named nothing (§3.2.1 item 2)
    discarded: Vec<DiscardedInfo>,       // resolved to nothing (§3.3)
}

struct DisabledEnvInfo {
    env: SessionEnvInfo,
    status: &'static str,                // `disabled` or `superseded`
    exclusive_group: Option<String>,     // its exclusiveGroup
    superseded_by: Option<SessionEnvInfo>, // for a superseded one, the group member entered
}

struct StepInfo<'a> {
    name: String,
    description: Option<String>,
    total_tasks: u64,
    task_params: Vec<(String, String)>,  // (name, type_name)
    envs: Vec<String>,
    deps: Vec<DepInfo>,
    env_params: Vec<EnvParamInfo>,
    env_params_details: Option<EnvParamsDetails<'a>>, // Some with ENV_PARAMS
}

struct EnvParamsDetails<'a> {
    session_envs: &'a [SessionEnvInfo],               // the Step's Environment set
    disabled_envs: &'a [DisabledEnvInfo],             // disabled by an enableIf or superseded (§4 items 6, 7)
    declined_envs: &'a [SessionEnvInfo],              // declined by a disabled entry (§3.2 item 3.2)
    not_found: &'a [(usize, String)],                 // optional entries that named nothing
    host_requirements: Option<&'a job::HostRequirements>, // combined (§3.3.3)
    discarded: &'a [DiscardedInfo],                   // resolved to nothing (§3.3)
    owner: &'static str,                              // `Step` or `Service`, for an own discard
}

struct DiscardedInfo {
    kind: &'static str,                  // `amount` or `attribute`
    name: String,                        // the resolved capability name
    environment: Option<SessionEnvInfo>, // `None` for the Step's own
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

struct DepInfo { name: String, is_service: bool }           // one `dependencies` entry: dependsOn (false) or service (true)
struct PortInfo { name: String, protocol: String, port: Option<u16> }
struct ServiceInfo<'a> {                                    // one `services` entry
    name: String, description: Option<String>, document: Option<String>, scope: String,
    ports: Vec<PortInfo>, health_check: String, max_restarts: u64, completed_tasks: Option<String>,
    deps: Vec<DepInfo>, env_params: Vec<EnvParamInfo>,
    env_params_details: EnvParamsDetails<'a>,               // as for a Step
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
