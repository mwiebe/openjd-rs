# run Command

## Purpose

`openjd run <path>` executes a job template locally in an OpenJD session. It is the
most complex command in the CLI, orchestrating the full session lifecycle: template parsing,
parameter resolution, job creation, environment enter/exit, task iteration with adaptive
chunking, embedded file materialization, let binding evaluation, and structured result output.
With the `SERVICE` extension (RFC 0009) it is also the scheduler for the Job's Services: it
allocates their endpoints, runs each in its own Service Session, gates Tasks on their
readiness, applies their restart policies, and stops them when their scope completes (see
[Services](#services-rfc-0009)).

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
| `--environment` | `Vec<PathBuf>` | No | — | Additional environment template files |
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
  │   ├── create_job() → Job (from the Job Template alone)
  │   └── apply_environment_templates().into_combined_job() → the combined Job:
  │       external Services ahead of the Job Template's services, attached
  │       Environments ahead of jobEnvironments, requirement bindings kept
  │       (RFC 0009 / Template Schemas §1.2.2)
  │
  ├── 2. SELECTION AND PREFLIGHT
  │   ├── Resolve step selection (explicit / auto-select / all)
  │   ├── Parse explicit task params and require a parameter space
  │   ├── Determine step execution order (the Step entries of Step dependencies,
  │   │   plus the Step entries of a Service's `dependencies` folded into the
  │   │   Steps in its scope; `service:` entries are the readiness gate's)
  │   └── Validate RFC 0008's single-wrap-layer rule for every selected stack
  │       (jobEnvironments + one Step's stepEnvironments, which a Service
  │       Session's stack is a subset of), across the combined Job's documents
  │
  ├── 3. SESSION CREATION
  │   ├── Create SessionConfig from parameters, path rules, and model profile
  │   ├── Session::with_config() — the Task Session
  │   └── ServiceManager::new() — the scheduler side of RFC 0009 (no Session yet)
  │
  ├── 4. JOB-WIDE SERVICES AND ENVIRONMENT ENTRY
  │   ├── Register every Service of the combined Job; activate those whose
  │   │   scope is every Step (unless no Task will run)
  │   ├── Readiness gate: start every active Service, wait until READY
  │   └── Enter job environments (attached, then the template's own) in the
  │       Task Session — skipping those whose runScope excludes TASK
  │
  ├── 5. STEP EXECUTION
  │   └── For each step not yet completed:
  │       ├── Activate every Service whose scope includes the Step (unless no
  │       │   Task of it will run), once the Steps it lists have completed
  │       ├── Readiness gate: start every active Service, wait until READY
  │       ├── Enter step environments
  │       ├── Execute tasks (explicit, lazy iteration, or no-param single task),
  │       │   each behind the readiness gate and watched for Service instance failures
  │       ├── Exit step environments (reverse order)
  │       └── Stop every Service whose scope has no remaining Step (dependency order)
  │       A completedTasks: RERUN relaunch returns every Step in the Service's
  │       scope, and every Step depending on one of them, to pending: the loop
  │       resumes over the uncompleted Steps in a new Task Session.
  │
  ├── 6. ENVIRONMENT EXIT AND SERVICE STOP
  │   ├── Exit every entered environment (LIFO), including any whose
  │   │   enter action failed (cleanup guarantee)
  │   └── Stop every Service still running (a Service before any it
  │       depends on), whatever its state
  │
  └── 7. RESULTS
      ├── Treat any observed interruption as a failed run
      ├── Capture run duration (before filesystem cleanup)
      ├── Print session summary (format depends on --output), naming any
      │   FAILED Service
      ├── Cleanup session (unless --preserve)
      └── Exit with code 1 if any action failed or a Service failed its scope
```

The environment ledger is manipulated only through `RunContext` methods.
Each step unwinds to its pre-step baseline before propagating an error, and
the outer execution phase always unwinds the remaining environments and
stops every Service. Thus a `Session::run_task()` error cannot bypass
environment cleanup or Service teardown.

Selection and RFC 0008 preflight happen before session creation. Invalid runs
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
(dependencies before dependents) via recursive DFS. With `SERVICE` (`Job::service_active()`)
a `dependsOn: "service:<Name>"` entry names a Service, not a Step
(`StepDependency::target(service_active).step()` is `None`), and is skipped; the
Service it names is reached through its scope instead (see [Services](#services-rfc-0009)).

Both orderings are computed over `with_implied_step_dependencies(job)`: a copy of the
Job in which every Step in the scope of a Service that lists Steps in its `dependencies`
(RFC 0009 §9 item 4) also depends on those Steps (`job::Service::depends_on_steps()`) —
its Tasks wait for the Service, which waits for those Steps. A Service's scope already
includes the Steps that reach it through other Services (§9.1 rule 2), so a chain
`Use -> service:Front -> service:Back -> Prepare` folds `Prepare` into `Use` through
Back's scope; `service:` entries are not folded. A cycle this introduces is reported
with the step-graph message plus `(Step dependencies include those implied by Services'
dependencies)`.

## Task Execution Modes

Within a selected step, tasks execute in one of three modes:

### No Parameter Space

If the step has no `parameterSpace`, a single task runs with no task parameters.
Supplying `--task-param` or `--tasks` for such a step is rejected during preflight.

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
3. Pass the readiness gate (every active Service READY — a no-op without
   Services), then call `Session::run_task()`; the sessions crate builds symbols
   and materializes the script
4. Check the result and stop on failure; a Task canceled for a `RERUN` relaunch
   ends the iteration so the returned Steps can start over

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

Environment template environments (from `--environment` files) reach the run as
the leading entries of the combined Job's `job_environments`:
`openjd_model::apply_environment_templates()` converts each attached template's
`environment` with a symbol table built from the preprocessed parameter values
(the merged `Param.*`/`RawParam.*` of every template in the submission) frozen
into its `resolved_symtab`, which the session's RFC 0008 wrap-hook dispatch merges
into hook scope — a wrap hook resolved against a step's symbol table could not
otherwise see the environment template's own parameters — and
`into_combined_job()` places them ahead of the Job Template's own
`jobEnvironments`. The same call instantiates the templates' `services` as
external Services (see [Services](#services-rfc-0009)), each stamped with its
document, and runs the one submission-time check of RFC 0009 (a wrapping
Environment from a document that does not declare `SERVICE` with a Service in its
scope), reporting it like any other job-creation error with the template's path
as the document label. `PreparedRun::environment_documents`
(`AppliedEnvironmentTemplates::combined_environment_documents`) records, index
for index with the combined `job_environments`, which document each Environment
came from. The CLI enters nothing itself from the template objects.

**Per-document extension profiles.** An extension applies to the document that
lists it (Template Schemas §1.2 item 3; RFC 0009 "Environment Template": a Job
Template need not list the extensions the Environment Templates applied to it
use, and vice versa). The Task Session's `SessionConfig::profile` is the Job
Template's, so an attached Environment is entered through
`Session::enter_environment_with_profile(env, symtab, None, None,
Some(profile))` with **its own template's** profile — `PreparedRun::attached_profiles`
(each `--environment` template's `EnvironmentTemplate::profile()`, by attachment
index) looked up through `PreparedRun::profile_for(document)` — and the Job
Template's own Environments with `None` (the Session's). The profile is kept on the
`EnteredEnvironment` so a re-entry after a Service's endpoints change uses it
again. Without this, the queue use case of RFC 0009 failed at run time
(`Failed to resolve env var 'KV_ADDR': Unknown function: 'join_host_port'`) whenever
the Job Template did not itself declare `SERVICE`: the attached Environment's
`join_host_port` was looked up in the Job Template's library. The same rule applies
to Service Sessions (see [Services](#services-rfc-0009) "Documents and profiles").

The profile also decides whether `openjd_redacted_env` is honored: an Environment's
`onEnter` / `onExit` sets a redacted variable iff **its own** document declares
`REDACTED_ENV_VARS` (or has a revision newer than 2023-09); a Task's `onRun` and a
Service's actions iff their document does. When it is not, the runtime still redacts
the value from the log but sets nothing, and — so the redacted `NAME=********` line is
not mistaken for a set variable — logs a WARN line in the action's output, which the
run log shows next to that line:

```
0:00:00.017	Received openjd_redacted_env for 'SECRET' but the REDACTED_ENV_VARS extension is not declared; the variable is not set.
0:00:00.017	openjd_redacted_env: SECRET=********
```

The line names the variable only (the value never appears) and is emitted once per
directive, by the sessions runtime's `ActionFilter`
(`specs/sessions/action-filter.md`); inside a Service Session it is tagged like the
rest of that Session's output (`[Service Vault] Received openjd_redacted_env for
'SECRET' …`). This was stumble S6 of the exploratory report (`08-secret.yaml`, whose
Service `onEnter` generated a token the `onRun` then could not find; `08c` for a plain
Environment): the fix is to add `REDACTED_ENV_VARS` to that document's `extensions`.
Tests: `test_service_on_enter_redacted_env_without_the_extension_warns`,
sessions `redacted_env_without_the_extension_warns_in_the_environments_output`.

The `EnteredEnvironment` also keeps the symbol table the Environment was actually
entered with (`resolved`: the caller's table with the document's READY `Service.*`
endpoints layered on), and `exit_environments_down_to` passes it to
`Session::exit_environment`, so an `onExit` that references `Service.*` resolves the
same values its `onEnter` and `variables` did instead of failing teardown with an
undefined variable.

Step environments receive the step's symbol table (`step_symtab`) for format string
resolution. Job and template environments receive `None` for the step symbol table.
When Services are in scope, `RunContext::task_symtab` layers the endpoints of the
Environment's **own document's** Services onto whichever table it resolves against
(see [Task Sessions see the endpoints](#task-sessions-see-the-endpoints)).

An Environment whose `runScope` excludes `TASK` (RFC 0009 `<Environment>`) is not
entered in the Task Session: the run logs `Skipping Environment '<name>': its runScope
does not include TASK` and records nothing for it. (Such an Environment is entered in
Service Sessions instead.)

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

## Services (RFC 0009)

`openjd run` is the scheduler and the only host, so it takes the scheduler's side of
the split described in `specs/sessions/service-session.md`: everything *inside* one
Service Session is the `openjd_sessions::ServiceSession` runtime's; the decisions
below are the CLI's, in `run/services.rs` (`ServiceManager`) and
`run/service_ports.rs` (`PortAllocator`). Normative references: RFC 0009
"Modifications to How Jobs Are Run" (lifecycle constraints 1–10, "Failure and
restart"), wiki *How Jobs Are Run* § Services, and §1.2.2 "Services from Environment
Templates".

### Submission

After `create_job`, `apply_environment_templates` folds the `--environment` templates
into the Job: `job.services` is the external Services (attachment order, then each
template's `services` order) followed by the Job Template's `services`, and
`job.job_environments` the attached Environments followed by the template's own. Its
`requirement_bindings` — the attached Service each `requiresServices` entry matched —
are kept in `PreparedRun` and handed to the `ServiceManager`. A requirement that no
attachment provides, that two provide, or whose provider lacks a port or carries it
with another protocol is rejected before any Session exists, with the model's
`Submission` error naming `JobTemplate -> requiresServices[i]` and the cause (see
`specs/model/validation.md`, "Submission-time checks").

**Dependencies and scope.** A Step's or a Service's `dependencies` list Steps and
Services in one list: `dependsOn: "service:<Name>"` names a Service, any other string a
Step (Template Schemas §3.2). A Step's Tasks are scheduled once the Steps it lists have
completed and the Services it lists are READY; a Service starts once the Steps it lists
have completed and the Services it lists are READY, and is stopped before any Service
it lists. Every Service of the combined Job carries the scope job creation computed for
it from those dependencies (`job::Service::scope`, Template Schemas §9.1): every Step
for an external Service or a Service a Job Environment references; otherwise the Steps
that list `service:<Name>`, together with the scope of every Service that lists it,
transitively. (Validation rejects a Job Template Service whose scope is empty.) The
manager registers all of them once and *activates* each when the run reaches its
scope: a Service whose scope is every Step before the Task Session enters the Job's
Environments; one scoped to some Steps when the first of them is about to run — after
every Step it lists (`job::Service::depends_on_steps()`) has completed (a dependency
outside the selection counts as completed, as a Step's own dependencies do under
`--step`). A Step that lists `service:X` is in X's scope, so its Tasks wait for X
through the readiness gate; Steps outside a Service's scope never wait on it. A Service is stopped once no Step still to run is in
its scope, and returns to idle; a `RERUN` that returns one of its Steps to the queue
activates it again, in a new Service Session (lifecycle constraint 9).

**Inline Services shadow external ones** (Template Schemas §1.2.2 item 3). An
external Service may be named like a Service of the Job Template or of another
attachment; the submission is rejected for it only when a requirement names that name
(the ambiguity above). The manager identifies every Service by `ServiceKey {
document, name }` — `job::Service::document` is `Document::JobTemplate` for the Job
Template's own Services and the attachment (by index, labeled with its path) for an
external one — so two `Cache`s are two Services with two Sessions, two sets of ports,
and two readiness verdicts. A name is looked up across documents in exactly one place:

- A `service:<Name>` entry in a Service's `dependencies`
  (`job::Service::depends_on_services()`) names a Service of the same document — or,
  in a Job Template Service, a required external Service, which resolves to the
  attached Service its requirement was bound to (`ServiceKey::of_binding`) unless the
  Job Template declares that name itself. Each is keyed accordingly in
  `Managed::depends_on_services` for the start-ordering waves, the stop order, and
  restarting dependents.
- A Service Session's in-scope endpoints (constraint 2) are the READY Services it
  depends on, so keyed. A same-named Service from another document is never seeded, so
  its symbols cannot collide.
- The Task Session sees, for a Task of Step `S` or `S`'s Step Environments, the Job
  Template's READY Services whose scope includes `S`; for the Job Template's own Job
  Environments, every READY inline Service; and in both cases the attached Services
  bound to the Job Template's requirements (`Service.<requirement>.*` is the bound
  Service's endpoints). For an attached Environment, that attachment's Services only
  (`RunContext::task_symtab(…, document, step)` with
  `ServiceManager::task_scope_endpoints(document, step)`). The RFC's queue-cache
  Environment therefore publishes the queue's `Cache` through `VALKEY_HOST` /
  `VALKEY_PORT` while the Job Template's Tasks resolve `Service.Cache.*` to their own —
  or, with `requiresServices: [{name: Cache, …}]` and no inline `Cache`, to the queue's.
- Log lines, the failure summary, and `failed_services` name an external Service
  with its document: `Service 'Cache' (from queue-cache.yaml)` (the path as given
  to `--environment`); the Job Template's own stay `Service 'Cache'`. See
  [Output](#output).

**Documents and profiles.** Each document's strings are evaluated under its own
extensions (Template Schemas §1.2 item 3). A Service Session's `SessionConfig::profile`
is the profile of the Service's own document — `ServiceRunConfig::profile_for(&service.document)`:
the Job Template's `profile` for its `services`, the attachment's entry of
`attached_profiles` for an external Service — so an external Service's actions,
`variables`, `let` and embedded files use its template's `SERVICE` / `EXPR` functions
whatever the Job Template declares. The Environments a Service Session enters (the
combined `job_environments`; never a Step's `stepEnvironments`, since a Service belongs
to no Step) each come from a document; `ServiceManager::register` takes
`PreparedRun::environment_documents` and fills `ServiceSessionConfig::environment_profiles`
with `Some(profile_for(doc))` for every Job Environment whose document differs from the
Service's (`None` for those sharing it). The Job Template's `jobEnvironments` entered in
an external Service's Session thus keep the Job's profile, and an attached Environment
entered in a Job Template Service's Session keeps its template's.

Template validation (per document) and `apply_environment_templates` (across documents)
between them enforce §9.9 item 6 — a wrapping Environment whose `runScope` includes
`SERVICE` defines all four `onWrapService*` hooks — so the CLI's wrap preflight only
re-checks RFC 0008's single-layer rule over the combined stacks.

### Endpoint allocation (constraint 1)

`PortAllocator` (`run/service_ports.rs`) is the **local runner's** policy: every
Service binds and is reached on the loopback interface. For each declared port it
binds `127.0.0.1:0` with a socket of the port's `protocol` — a `TcpListener` for a
TCP port, a `UdpSocket` for a UDP port (§9.3 item 3) — to find a free port in that
protocol's space (or binds the requested `port` once, the same way, to confirm it is
available), releases the socket, and records `(protocol, number)` so that no two
Services of the run ever receive the same port of one protocol — numbers are never
reused within a run, even after a Service Session ends, so a Task that resolved an old
endpoint cannot reach the wrong Service. TCP and UDP are independent spaces: a TCP
port and a UDP port may be allocated, or requested, with the same number.
`bindAddress` and `connectAddress` are both `127.0.0.1`, as bare addresses (RFC 0009
"Address forms"; templates join an address and a port with `join_host_port`). A
requested port that is already allocated in its protocol or cannot be bound (`Service
'A' port 'dgram' requests UDP port 8125, which is not available on 127.0.0.1: …`) is
a *start failure* of the Service. Endpoints are allocated when the Service Session is
opened, before any of its actions runs; a new Service Session gets new ports.

### Start ordering (constraints 2, 10)

Services are started by the **readiness gate** (`ServiceManager::gate`), which runs
before the Task Session enters the Job's Environments, before each Step's
Environments, and before every Task. The gate starts the *active* Services that are
idle, in *waves*: a Service may start when every Service in its
`depends_on_services` is READY — the `service:<Name>` entries of its `dependencies`,
each keyed with the Service's document (or the bound attached Service, for a
requirement), never from list position, which carries no meaning — and Services that
do not depend on one another start concurrently, each on its own tokio task. A
Service's Session is seeded with the endpoints of exactly the READY Services it depends
on (the "in scope" set of RFC 0009 "The `Service.*` scope"; validation ensures a
Service references `Service.X.*` only for an X it lists), so a depended-on Service's
endpoint is always known at the dependent Session's start. If no active idle Service
can start and none is starting, the gate fails the run with `cannot order the start of
Services <names>: each depends on a Service that is not READY and is not starting`.

A Service whose scope is every Step is activated at job start, but only if some
selected Step will run at least one Task (constraint 10 — a `--tasks '[]'` selection
runs none, and the run logs `Not starting the N Service(s) whose scope is every Step:
no Task of this Job will run`); a Service scoped to some Steps is activated when the
first Step of its scope is about to run, only if that Step will run a Task (`Not
starting Service 'X', Service 'Y' for Step '<name>': no Task of this Step will run`),
and only after every Step it lists has completed (§9 item 4; the Step ordering
guarantees it, see [Step Selection Logic](#step-selection-logic) — should it not, the
activation fails with `Service 'X' (scope: …) cannot start before Step '<name>': it
depends on Step(s) 'D' which have not completed`). A Service
Session enters the Job's Environments, skipping those whose effective `runScope`
excludes `SERVICE` — the runtime does the skipping and logs it under the Service's tag.

### Task gating (constraint 3)

No Task runs until the gate reports every active Service READY. A Service scoped to
Steps is activated and started when the first Step of its scope is reached in
dependency order (the existing topological loop), before the Task Session enters the
Step's Environments. The order within a Step is therefore: the Step's Services start →
step Environments entered → Tasks → step Environments exited → Services whose scope is
now complete stopped. The RFC does not couple the Step's Environment exits (which
belong to the Task Session) to the Services' stop; this runner exits the Environments
first so that a TASK-scoped Environment's `onExit` can still reach the Service,
mirroring the Job level (Job Environments exit, then the Job-wide Services stop) and the
RFC's execution-order example. A Service whose scope spans several Steps keeps its
Session — and its state — across them (the RFC's coordinator example).

### Task Sessions see the endpoints

`RunContext::task_symtab` builds the symbol table a Task Session action resolves
against: the caller's table (the Step's `resolved_symtab` for a Task or a step
Environment, an Environment's own `resolved_symtab` for a job Environment, or the
submission's `Param.*` table when neither exists) with
`openjd_model::job::service_symbols::build_service_symbol_table(in_scope, None)`
appended — the `Service.<name>.<port>.port` and `.connectAddress` of every READY
Service in scope of the resolving entity: for the Job Template's entities
(`Document::JobTemplate`), the inline Services whose scope includes the Step (a Task,
a step Environment — `EnteredEnvironment::step`) or every inline Service (the Job
Template's own job Environments), plus the attached Services bound to its
`requiresServices` under the requirement's name; for an `--environment` template's
Environment, that attachment's own Services (recorded in
`EnteredEnvironment::document` so a re-entry uses the same scope); never
`bindAddress` (§7.3.1 scope rows). The two serialized tables are
concatenated entry-for-entry, so no path value is re-interpreted. Without Services
in scope the caller's table is passed unchanged (the pre-RFC-0009 behavior,
including `None` for job Environments).

### Failure and restart

While a Task runs, `RunContext::run_task` selects between the Task and
`ServiceManager::wait_instance_failure`, which resolves when a READY Service suffers
an instance failure: its `onRun` exits other than by cancelation, or its health check
declares it **UNHEALTHY** (`failureThreshold` consecutive failed probes after READY —
RFC 0009 `<ServiceHealthCheck>`; the Service Session reports it on its health watch
and, per lifecycle constraint 11, has already canceled `onRun` with its cancelation
method). Between Tasks, the gate polls the same health and exit channels. An exit
observed after the scope completed is never seen (nothing watches after the last
Task), so it is not a failure — the RFC's stop race.

On an instance failure the Service becomes UNREADY (an UNHEALTHY one is logged as
`is UNHEALTHY: <reason>` first) and the restart decision runs on a background task
(`start_or_recover`), so a `KEEP` relaunch proceeds while the Task continues:

- `completedTasks: RERUN` — the running Task is canceled through the Task Session's
  cancel handle (its own `cancelation` method), logged as `Task canceled; it returns
  to the queue (not a Task failure)` and not counted in `tasks_run` or as a failure.
  The Step's remaining Tasks are abandoned and, per "`RERUN` and Step dependencies",
  every selected Step in the Service's scope returns to pending, and with it every
  selected Step that depends on one of them, directly or transitively
  (`returned_steps`): `run_workload` resumes over the Steps no longer marked
  completed, in the original order, re-running their completed Tasks — **only once the
  Service is actually relaunched**: after the canceled Task's Step unwinds,
  `run_workload` runs the readiness gate before announcing the requeue, so the restart
  decision (and the relaunch) has settled. If the Service's attempts are exhausted it
  is FAILED instead, the gate records the failure, and the run fails without printing
  `Returning every completed Task …` or `New Task Session for the requeued Tasks` (the
  exploratory report's stumble S10). Otherwise the requeue is logged (`Returning every
  completed Task of Step(s) 'A', 'B' to the queue: a Service with completedTasks: RERUN
  (scope: Steps A, B) was relaunched`, followed by `; dependent Step(s) 'C' return to
  pending` when a Step outside the scope is returned) and the requeued Tasks run in a
  **new Task Session** (`RunContext::replace_task_session`): a canceled action leaves a
  Session ending-only ("Brittle Sessions", `specs/sessions/session.md`), and a
  scheduler would form new Sessions for requeued Tasks anyway. The new Session
  re-enters the Job's Environments. A Service that had been stopped because its scope
  completed, and whose scope includes a returned Step, is activated again when that
  Step is next reached and starts in a new Service Session (constraint 9); several
  `RERUN` Services failing during one Task return the union of their scopes.
- `completedTasks: KEEP` — the Task continues; if it fails on its own that is an
  ordinary Task failure, which the local runner (having no Task retry) treats as it
  always has: the run fails. Completed Tasks stand.

Then, if relaunches so far `< restartPolicy.maxAttempts`, the Service is relaunched
(consuming one attempt): `ServiceSession::launch()` again in the same Session after an
exit of a READY instance, an UNHEALTHY verdict (the relaunch first awaits the exit of
the `onRun` the Session canceled — "Failure and restart" step 2, constraint 5), or a
ready timeout (which first cancels `onRun` and awaits its exit — step 2), but in a
**new Service Session**
(new working directory, new ports, Environments re-entered, `onEnter` re-run) when the
failure may be a port conflict — `onRun` exited before becoming READY — or was a
start failure (requested port unavailable, Environment `onEnter` failed, Service
`onEnter` failed, Session could not be created). Otherwise the Service is FAILED: its
Session is ended, the failure is recorded with its name and scope, and the scope
fails — every Step in it, and with it the Job when the scope is every Step; in the
local runner either fails the run (no further Task runs, exit code 1).

A relaunch that began a new Service Session changed the Service's endpoints. RFC 0009
only guarantees a depended-on Service's endpoint at the dependent Session's start, so
the runner then (`restart_dependents`, at the next gate) restarts every READY Service
that depends on the replaced one, transitively, in reverse registration order —
logging `Service 'Front' depends on a Service that began a new Service Session;
restarting it with the new endpoints`, ending their Sessions and starting them again
with the new in-scope endpoints, without consuming any of their attempts — and exits and re-enters the Task Session's Environments that may have
captured the old value (all of them for a Service whose scope is every Step, the
Step's for one scoped to Steps), logging `Re-entering N Environment(s): a Service they
may reference has new endpoints`. Tasks resolve `Service.*` per Task, so they see the new value without
further action.

Not implemented: suspension (constraint 10's `KEEP`-only pause) — a single-process
runner never pauses a Job; relocation and host loss do not arise with one host.

### Stopping (constraints 4, 6, 7)

A Service is stopped when its scope completes — no Step still to run is in it
(`ServiceManager::stop_completed_scopes`, after each Step, with the Steps that remain),
or the run ends, fails, or is interrupted (`stop_all`, after the Task Session has
exited the Job's Environments). A set of Services is stopped (`stop_set`) in reverse
topological order of their dependency graph: a Service before any Service it depends
on, Services that do not depend on one another in reverse registration order. A
Service whose scope is every Step is therefore stopped only at the end of the run,
after every Service scoped to Steps; a Service two Steps list is stopped after the
second of them, before any later Step runs. Stopping is `ServiceSession::end()`: cancel `onRun`
with its own `cancelation` method, `onExit` (if any action ran), Environments exited
in reverse, working directory deleted (kept under `--preserve`, with its path
logged). A background start or relaunch in flight is cut short through the Service's
stop token (the Service's running action is canceled through its cancel handle) and
its Session ended the same way; the stopped Service returns to idle, with a fresh
token, so a later `RERUN` can start it again. A Service whose Session never started is
skipped; a FAILED Service's Session was ended when it failed.

An interruption (Ctrl-C) cancels the running Task through the shared cancellation
token as before, and the Services through the manager: a Service Session does **not**
share the run's token — a token canceled during teardown would also cancel `onExit`
and the Environment exits, which constraint 7 wants run — so the manager cancels a
running `onEnter`/`onRun` through the Session's cancel handle and then ends the
Session normally.

### Output

Service lifecycle is logged on the run's timeline with the same banners the
Environments use (`Starting Service: <name>` / `Stopping Service: <name>`) and one
line per event, each naming the Service and its scope — `(scope: every Step)`,
`(scope: Step Render)`, or `(scope: Steps Gather, Scatter)` (sorted Step names), the
`Display` of `job::ServiceScope`: endpoints
(`Service 'Cache' (scope: every Step) endpoints: main -> 127.0.0.1:41235`; a UDP port's
address is suffixed `/udp` — `ingest -> 127.0.0.1:50780/udp` — and a TCP port's is
unsuffixed), `onRun launched
(launch N in this Session); health check: <TYPE> (readinessTimeoutSeconds N[,
readinessIntervalSeconds N], healthIntervalSeconds N, failureThreshold N)` (or `…, no
heartbeat after READY)` for a `STDOUT` check without one), `is READY[: <message>]`,
`is UNHEALTHY: N consecutive health probes failed (failureThreshold: N); last probe:
<what failed>`, `is UNREADY: <reason> (completedTasks: <policy>)`, `Relaunching Service
'<name>' onRun in its Service Session (relaunch N of M): <reason>` / `… in a new
Service Session …`, `is FAILED: <reason>; N of M relaunch(es) used
(restartPolicy.maxAttempts)`, and `stopped`. The `<reason>` of a failure is one of
`failed to start: …`, `onRun exited before becoming READY (exit code: N[; <fail
message>])`, `did not become READY within readinessTimeoutSeconds`, `onRun exited while the
scope still had work (…)`, or `instance UNHEALTHY: <the UNHEALTHY detail>`. The
Service's subprocess output streams through the session logger like a Task's, with the
`[onHealthCheck]` tag the runtime adds — including the runtime's
WARN line when the Service's `onEnter` uses `openjd_redacted_env` without the
document declaring `REDACTED_ENV_VARS` (see [Environment
Lifecycle](#environment-lifecycle) "Per-document extension profiles").

**Attribution inside a Service Session.** Once Tasks run, a Service's `onRun` output
interleaves with Task output in the single run log, and the sessions runtime enters a
Service Session's Environments and runs its actions internally, where the CLI cannot
print banners of its own. Both are solved with the Session's log tag
(`SessionConfig::log_tag`, `specs/sessions/logging.md`): every Service Session is
created with `log_tag = "Service <name>"` — `"Service <name> (from <document>)"` for an
external Service — so **every line the Service Session logs is prefixed `[Service
<name>] `**: its Environments' `onEnter` / `onExit` output, `onEnter`'s, `onRun`'s,
`onExit`'s, the `Output:` headers, and the process-control lines. The concurrent
`onHealthCheck`'s lines keep the RFC 0009 rule-3 action tag *after* the Service
tag: `[Service Files] [onHealthCheck] CHECK_OK`. A tagged Session's section
banners collapse to one tagged line each, and the CLI's `SessionLogger` prints
`BANNER` records that carry a session tag (it still drops the untagged Task
Session's, whose banners the CLI prints itself), so a Service Session's phases read:

```
--------- Starting Service: Files                        ← the CLI's banner (4 lines)
Service 'Files' (scope: every Step) endpoints: main -> 127.0.0.1:41235
[Service Files] --------- Starting Service: Files
[Service Files] Skipping Environment 'Client': its runScope does not include SERVICE
[Service Files] --------- Entering Environment: Shared
[Service Files] Output:
[Service Files] Shared.onEnter running in a Session
[Service Files] --------- Service onEnter: Files
[Service Files] Output:
[Service Files] --------- Service onRun: Files (launch 1)
Service 'Files' onRun launched (launch 1 in this Session); health check: TCP_CONNECT (readinessTimeoutSeconds 300, readinessIntervalSeconds 1, healthIntervalSeconds 30, failureThreshold 3)
[Service Files] Output:
Service 'Files' is READY
…
--------- Stopping Service: Files                        ← the CLI's banner (4 lines)
[Service Files] --------- Ending Service: Files
[Service Files] --------- Service onExit: Files
[Service Files] --------- Exiting Environment: Shared
Service 'Files' stopped
```

The CLI's own event lines (`Service 'Files' …`) are not tagged: they are the run's
narrative, not the Session's output. The Task Session carries no tag, so Task and
Task-Session Environment output is unchanged.

A FAILED Service is also
reported on stderr (`ERROR: Service '<name>' (scope: <scope>) failed: <reason>`), in
the summary (`Failed Service: <name> (scope: <scope>): <reason>`; the result message
becomes `Service '<name>' (scope: <scope>) failed: <reason>`), and as `failed_services`
(`name`, `scope` — the scope text, `every Step` / `Step A` / `Steps A, B` — and
`reason`) in the JSON/YAML result. The exit code is 1.

An **external Service** (one from an `--environment` template) is named with its
document everywhere a Service of the Job Template is named alone, so two
same-named Services are told apart: the banners read `Starting Service: Cache (from
queue-cache.yaml)` / `Stopping Service: Cache (from queue-cache.yaml)`, every event
line `Service 'Cache' (from queue-cache.yaml) …`, the stderr and summary lines
`Service 'Cache' (from queue-cache.yaml) (scope: every Step) failed: …` and `Failed
Service: Cache (from queue-cache.yaml) (scope: every Step): …`, and the
`failed_services` entry gains
a `document` key holding the template's path as given on the command line (absent
for the Job Template's own Services). The document is the label the CLI passes to
`apply_environment_templates`, so it matches the submission-time error paths.

## Script Runtime Delegation

The CLI passes the instantiated `StepScript`, task parameter values, and the step's
serialized symbol table to `Session::run_task()`. The sessions crate owns script-level
`let` evaluation, embedded-file allocation and materialization, action format-string
resolution, subprocess execution, and path-mapping symbol materialization. The CLI only
selects and sequences tasks.

## Host-Context Function Library

The CLI passes the instantiated job's `ModelProfile` and path-mapping rules through
`SessionConfig`. `Session::with_config()` derives the matching expression profile and
host-context function library used by the Job Template's environment and task actions.
An attached Environment Template's Environment carries its own profile into the
Session (`enter_environment_with_profile`), and each Service Session's `SessionConfig`
carries the profile of the Service's own document; see
[Environment Lifecycle](#environment-lifecycle) and [Services](#services-rfc-0009).

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
| `profile` | `ModelProfile` built from the job template's declared extensions (the Task Session; a Service Session gets its own document's — `ServiceRunConfig::profile_for`) |
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
Tasks run: 100
```

### JSON

```json
{
  "status": "success",
  "message": "Session ended successfully",
  "job_name": "MyJob",
  "step_name": "Render",
  "duration": 42.123,
  "tasks_run": 100
}
```

When a Service failed its scope (RFC 0009) the result additionally carries
`"failed_services": [{"name": "Cache", "scope": "Job", "reason": "..."}]` (the
`scope` is `Job` or `Step '<name>'`; an external Service's entry also carries
`"document": "<path given to --environment>"`, since Service names are scoped to
their document), the `status` is `error`, and the `message` names the first failed
Service; the human-readable output appends a `Failed Service: <name>[ (from
<document>)] (<scope> scope): <reason>` line per Service.

### YAML

```yaml
status: success
message: Session ended successfully
job_name: MyJob
step_name: Render
duration: 42.123
tasks_run: 100
```

## Error Handling and Exit Codes

- Exit code 0: All actions completed successfully
- Exit code 1: Any action failed, a Service became FAILED (RFC 0009), setup failed,
  or interruption was observed

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
| Environment conversion | Implicit (Python sessions accept template types) | `apply_environment_templates().into_combined_job()` folds attached templates into the Job |
| Services (RFC 0009) | Not implemented | `ServiceManager` orchestrates Service Sessions (see [Services](#services-rfc-0009)) |
| Callback | `LocalSession._action_callback()` handles states | No callback; result checked after each await |
