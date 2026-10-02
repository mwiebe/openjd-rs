# Service Session (RFC 0009)

## Overview

A **Service Session** runs the actions of one `<Service>` (RFC 0009, the `SERVICE`
extension) on the service host. It is implemented by `ServiceSession` in
`service_session.rs`, a sibling of `Session` that composes one: the `Session`
owns the working directory, the Environment stack, cumulative environment
variables, path mapping, redaction, the cross-user helper, and cleanup; the
`ServiceSession` adds the `Service.*` symbol scope, Service `variables`,
`onEnter`'s retained `openjd_env` changes, the background `onRun` driver with
its readiness check, the second action slot in which `onReadinessCheck` runs
concurrently with `onRun` (`COMMAND` readiness), the `onWrapService*` hook
dispatch, and the constraint-7 teardown.

Normative references: RFC 0009 "Modifications to How Jobs Are Run" (Service
lifecycle constraints, "Services run inside Environments", "Failure and
restart"), `<ServiceActions>` incl. "Concurrency with `onRun`" (rules 1–6),
`<ServiceReadinessCheck>`, the `<EnvironmentActions>` modification
(`onWrapService*`, "RFC 0008's rules extend to the new hooks" 1–6,
`WrappedService.*`), "Environment variables within a Service", and the
`openjd_service_ready` message; wiki *Template Schemas* §4.3 rule 6, §4.3.1,
§9.6, §9.6.1; wiki *How-Jobs-Are-Run* § Services.

### What this runtime decides, and what it leaves to the caller

The runtime implements everything that happens *inside* one Service Session.
The scheduler's decisions stay with the caller (`openjd-cli` or a worker
agent):

| Runtime (this crate) | Caller |
|---|---|
| Enter `SERVICE`-scoped Environments and the Service's `serviceEnvironments`, run `onEnter`, launch `onRun`, probe readiness, report exit, relaunch, `onExit`, exit Environments, cleanup | Port allocation and `bindAddress`/`connectAddress` choice |
| Detect instance failures: readiness timeout, `onRun` exit before READY, `onRun` exit at any time | Restart decision (`restartPolicy`, relaunch vs. new Session), Task gating, multi-Service ordering |
| Cancel `onRun` with its own `cancelation` method on request and at `end()` | When to cancel (scope complete, readiness timed out, …) |
| Run `onReadinessCheck` concurrently with `onRun` under the rules of §9.6.1; run the `onWrapService*` hooks of the entered wrapping Environment in place of the Service's actions | Validate the Environment stack (single wrap layer, hook set matches `runScope`, §9.7) |

## Why a sibling type, not a mode of `Session`

`Session`'s state machine runs one action at a time behind `&mut self`, and
every caller-facing operation (`enter_environment`, `run_task`, …) awaits the
action to completion. A Service's `onRun` is the opposite: it is launched and
*not* awaited, and while it runs the caller must be able to observe readiness,
observe exit, cancel it, and end the Session. Threading that through `Session`
would have meant a second state machine inside the first. Composition keeps
`Session` unchanged for Task Sessions and gives Service Sessions their own
small, explicit lifecycle.

The crate-internal seams `ServiceSession` uses on `Session` are `pub(crate)`:
`build_symbol_table`, `materialize_path_mapping`, `resolve_env_var_value`,
`evaluate_env_vars`, `live_session_env_vars`, `embedded_files`,
`new_runner_base` / `restore_runner_base` (a fully configured
`ScriptRunnerBase`, carrying the cross-user helper), `cancel_fields` (the
Session's *main* cancel slot, so one `SessionCancelHandle` cancels whichever
action — Environment or Service — is running in it), `spawn_detached_helper`
/ `new_detached_runner_base` (a runner for the *second* slot, with its own
cross-user helper), `wrap_hook_base_symtab` / `service_wrap_hooks` and the
shared `seed_wrapped_action_symbols` (wrap hook dispatch), `callback_arc`,
`library_arc`, `limits`, `add_redacted_values`.

### Two action slots

`Session` runs one action at a time, and its cancel state is one slot
(`ActionCancelSlot`, wrapped by `CancelFields`): a token, a time-limit
channel, and the running action's declared NOTIFY_THEN_TERMINATE grace, which
a `SessionCancelHandle` delivers a cancel to. A Service Session's `onEnter`,
`onRun`, `onExit`, and Environment actions all run in that **main slot** —
`ServiceSession::cancel_handle()` / `cancel_run()` target it.

`onReadinessCheck` is the one action that runs *while* another runs, so it
gets a **second slot** of its own: its own `ActionCancelSlot`, its own
`ScriptRunnerBase` (built by `Session::new_detached_runner_base`, which does
not take the Session's cross-user helper), its own cross-user helper when the
Session is cross-user (`Session::spawn_detached_helper` — the helper protocol
runs one command at a time, so each concurrent action needs its own helper
process, spawned from the same helper binary and shut down with the runner),
and its own log attribution tag. The runtime alone cancels through the second
slot (when `onRun` exits, the readiness decision is made, or the Session
ends); the caller's handle never reaches it. The shared slot stays shared for
everything that runs one at a time, and only the concurrent action gets a
separate one.

## ServiceSessionState

```rust
pub enum ServiceSessionState {
    Created,      // constructed; enter() next
    Entered,      // Environments entered, onEnter succeeded; launch() allowed
    Running,      // an onRun instance is running
    Exited,       // the most recent onRun has exited; launch() (relaunch) or end()
    StartFailed,  // an Environment onEnter or the Service onEnter failed; only end()
    Ended,        // end() completed
}
```

```
Created ──enter()──► Entered ──launch()──► Running ──(onRun exits)──► Exited
   │                    │                     │                          │
   │ (failure)          │                     │ cancel_run() → exit      │ launch() ──► Running
   ▼                    │                     │                          │
StartFailed             └──────── end() ──────┴──────────────────────────┴──► Ended
```

`Running → Exited` happens when the caller next observes the exit
(`wait_exit`, `launch`, `end`): the background driver publishes the exit on a
watch channel, and the `ServiceSession` reclaims the driver — restoring the
cross-user helper and recording any redacted values — at that point. A
wrong-state call returns `SessionError::InvalidServiceState { expected, current }`
with the message `Service Session must be in ENTERED or EXITED state, current:
RUNNING` (same shape as `InvalidState`).

## Construction

`ServiceSession::with_config(ServiceSessionConfig)`:

```rust
pub struct ServiceSessionConfig {
    pub session: SessionConfig,                 // as for a Task Session; `profile` is the Service's document's
    pub service: job::Service,
    pub environments: Vec<job::Environment>,    // the scope's Environments, in entry order
    pub environment_profiles: Vec<Option<ModelProfile>>, // per entry of `environments`: its document's profile when not the Service's
    pub endpoints: ServiceEndpoints,            // own ports: port, bindAddress, connectAddress
    pub in_scope_endpoints: Vec<ServiceEndpoints>, // earlier Services (no bindAddress)
}
```

**Documents and profiles.** `session.profile` is the profile of the Service's
*own* document (Template Schemas §1.2 item 3: an extension applies to the
document that lists it): the Job Template's for a `jobServices` /
`stepServices` entry, the attached Environment Template's for an external
Service. It governs the Service's actions, `variables`, `<ServiceScript>.let`,
embedded files, and its `serviceEnvironments` (which share its document).
The scope's Environments may come from other documents — an external
Service's Session enters the Job Template's `jobEnvironments`; a Job Template
Service's Session enters the attached Environments — so
`environment_profiles[i]` carries `Some(profile)` for `environments[i]` when
its document is not the Service's, and `enter()` enters it through
`Session::enter_environment_with_profile`. `None`, or an index past the end
of the list, enters with the Session's profile. The CLI fills this from each
Job Environment's document (`PreparedRun::environment_documents`) and the
profiles of the Job Template and the `--environment` templates. A wrapping
scope Environment from another document resolves its `onWrapService*` hooks
with its own document's library (`ServiceWrapHooks::library`) while
`WrappedAction.*` is resolved with the Service's (see "Wrap hooks").

Checks, before any directory is created:

- `endpoints.name == service.name`, else `SessionError::Runtime("Service 'svc'
  was given the endpoint assignment of Service 'other'")`.
- Every declared port, and every port a `TCP_CONNECT` check names, has an
  endpoint: else `SessionError::ServicePortUnassigned { name, port }` —
  `Service 'svc' port 'metrics' has no endpoint assignment`.
- Each endpoint's `protocol` is the declared port's `protocol` (§9.2 item 3;
  the number was allocated in that protocol's space): else
  `SessionError::Runtime("Service 'svc' port 'ingest' is declared UDP but its
  endpoint assignment is TCP")`. And every port a `TCP_CONNECT` check names
  is TCP (§9.3 item 2): else `SessionError::Runtime("Service 'svc':
  TCP_CONNECT readiness check names port 'ingest', whose protocol is UDP;
  only TCP ports can be probed")` — model validation already forbids this.
- A `COMMAND` readiness check comes with an `onReadinessCheck`: else
  `SessionError::Runtime("Service 'svc': readiness check type is COMMAND but
  onReadinessCheck is not defined")` (model validation already forbids this
  combination, §9.7 item 4).

The endpoint assignment's ports are then re-ordered into the Service's
`ports` declaration order, so `WrappedService.PortNames` / `.Ports` /
`.BindAddresses` / `.Protocols` are parallel lists in declaration order
(§4.3.1).

Then `Session::with_config(config.session)` runs — same working directory,
sticky-bit, cross-user helper, and host-info logging as any Session.

## `enter()` — opening the Session (constraints 1, 2 are the caller's; this is "Starting a Service")

Banner `Starting Service: <name>`. Any error leaves the state `StartFailed`
(a *start failure*), logs `Service '<name>' failed to start: <error>`, and
returns it; `end()` is still required.

1. **The scope's Environments** (RFC 0009 "Services run inside
   Environments"). For each configured Environment in order: if
   `!env.runs_in(RunScope::Service)`, log `Skipping Environment '<name>': its
   runScope does not include SERVICE` and continue; otherwise
   `Session::enter_environment_with_profile(env, env.resolved_symtab, None,
   None, environment_profiles[i])` — its own document's library when it has
   one, else the Session's. Job and Step Environments cannot reference
   `Service.*` (model validation), so they resolve against the plain Session
   scope. An Environment `onEnter` failure
   returns `SessionError::EnvironmentScriptFailed`; the Environment counts as
   entered and is exited by `end()`, exactly as in a Task Session.
1a. **The Service's `serviceEnvironments`** (§9 item 5), after all of the
   scope's and before `onEnter`: for each `service.service_environments`
   entry in order, `Session::enter_environment(env, Some(symtab), None,
   None)` where `symtab` (`service_environment_symtab`) is the entry's own
   `resolved_symtab` (`Param.*`, `RawParam.*`, `Job.Name`, for a Step Service
   `Step.Name` and the step-level `let`, the `<Service>.let` values) with the
   `Service.*` endpoint table of step 2 folded in — the declaring Service's
   own ports **including `bindAddress`** and the `port` / `connectAddress` of
   the Services in `in_scope_endpoints`: the same table the Service's own
   script uses, which a Job or Step Environment never gets. Because the
   argument is folded onto the stored Environment, its `onExit` and (for a
   wrapping Service Environment) its hooks' own scope see `Service.*` too.
   `run_scope` is never consulted (it is `None`; the effective scope is
   `[SERVICE]`). They join the same entered stack as the scope's
   Environments, so `end()` exits them first, in reverse; their `variables`
   and `openjd_env` exports layer on top of the scope Environments' (later
   Environments take precedence); and a wrapping Service Environment is found
   by `Session::service_wrap_hooks` like any entered wrapper (see "Wrap
   hooks"). A Service Environment `onEnter` failure is a start failure exactly
   as a scope Environment's: `SessionError::EnvironmentScriptFailed`, state
   `StartFailed`, `onExit` not run, every entered Environment exited by
   `end()`; the caller's restart policy treats it like any start failure.
2. **Symbol table**, built once and kept for the Session's lifetime:
   `Session::build_symbol_table(None, service.resolved_symtab)` (`Param.*`,
   `RawParam.*`, `Job.Name`, `Step.Name`, `<Service>.let` values, with PATH
   parameters re-mapped for this host, plus `Session.WorkingDirectory`), then
   `openjd_model::job::service_symbols::build_service_symbol_table(in_scope,
   Some(own))` merged in (own ports with `bindAddress`; earlier Services
   without), then `materialize_path_mapping` (`Session.HasPathMappingRules`,
   `Session.PathMappingRulesFile` — the rules file is written to the working
   directory), then `Service.File.*` (embedded file paths allocated under
   `EmbeddedFilesScope::Service`), then `<ServiceScript>.let` evaluated, then
   the embedded files' contents written.

   Embedded files are written **once**, here, and never again in the
   Session — not before `onRun`, not before any `onReadinessCheck`
   invocation, not on relaunch, not before `onExit`. This is how rule 1 of
   "Concurrency with `onRun`" (§9.6.1 item 1) is satisfied: a Service
   Session's format-string values are constant for its lifetime, so the
   content never changes, and the rule explicitly allows leaving an
   unchanged file in place; since nothing is ever rewritten, no action can
   modify a file a still-running action was given. `Service.File.<name>`
   therefore resolves to the same path in every action.
3. **Service `variables`**, resolved once against that table with
   `Session::resolve_env_var_value` — the same `string` target type, NUL
   rejection, and 2048-character cap (§4.4.2) as an Environment's.
4. **`onEnter`**, if defined (or `onWrapServiceEnter` in its place — see
   "Wrap hooks"): an ordinary foreground action (no default timeout; its
   `timeout` and `cancelation` apply; cancelable through `cancel_handle()`).
   Banner `Service onEnter: <name>`. A non-`Success` result is
   `SessionError::ServiceScriptFailed { name, action: "onEnter", reason }`
   with reason `exit code: N` / `canceled` / `timed out`, followed by
   `; openjd_fail: <message>` when the action emitted one (RFC 0009
   `<ServiceActions>`: "the message accompanies it"). The same shape is
   used for an `onExit` failure in `end()`.

## Environment variables of a Service action

`service_env_vars()` layers, lowest precedence first:

1. `Session::evaluate_env_vars(None)` — the process environment,
   `OPENJD_SESSION_WORKING_DIR`, and the entered Environments' `variables` and
   `openjd_env` / `openjd_unset_env` changes, in entry order (a later
   Environment overrides an earlier one, so a Service Environment overrides
   the scope's Environments);
2. the Service's `variables`;
3. `onEnter`'s `openjd_env` / `openjd_redacted_env` (when redaction is enabled
   by the profile) / `openjd_unset_env` changes.

This is the precedence RFC 0009 "Services run inside Environments" states
(scope Environments < Service Environments < Service `variables` < `onEnter`). The map is recomputed for each
action, so every `onRun` instance (including relaunches) and `onExit` see
`onEnter`'s changes — they are retained across relaunches because `onEnter`
is not re-run (constraint 5 / "Failure and restart" step 3.2).

The same map is given to `onReadinessCheck`, which therefore sees
`onEnter`'s variables too (RFC 0009: "every instance of *onRun*,
*onReadinessCheck*, and *onExit*").

Messages honored per action ("Environment variables within a Service", the
wiki's message table, and §9.6.1 rule 2):

| Message | `onEnter` | `onRun` | `onReadinessCheck` | `onExit` |
|---|---|---|---|---|
| `openjd_status` / `openjd_progress` / `openjd_fail` | honored | honored | ignored (logged) | honored |
| `openjd_env` / `openjd_redacted_env` / `openjd_unset_env` | honored | ignored (logged) | ignored (logged) | ignored (logged) |
| malformed env command (`CancelMarkFailed`) | cancels + fails the action | ignored (logged) | ignored (logged) | ignored (logged) |
| `openjd_service_ready` | ignored (logged) | honored iff type is `STDOUT` | ignored (logged) | ignored (logged) |

"Ignored (logged)" is one `info` line, e.g. `Ignoring openjd_env from Service
'svc' onRun: environment variable messages are honored only from onEnter`, or
for the check `[onReadinessCheck] Ignoring openjd_fail from Service 'svc'
onReadinessCheck: messages on the readiness check's stdout are not honored`.
The value of an ignored `openjd_redacted_env` is still added to the Session's
redaction set — the directive's effect is ignored, not its secrecy. Nothing a
Service sets is ever propagated to the entities in its scope: the `Session`'s
own `created_env_vars` are untouched by Service actions. The check's
`openjd_fail` does not even affect the check's own result: its result is its
exit status (rule 2), so an invocation that prints `openjd_fail` and exits 0
is READY. Under wrapping, the action name in these lines is the hook's
(`onWrapServiceRun`, …).

## `launch()` — the background `onRun` driver

Allowed in `Entered` and `Exited`. Banner `Service onRun: <name> (launch N)`
and `Readiness check: <TYPE> (timeout Ns)`. Steps:

1. Reclaim the previous driver, if any (restores the cross-user helper).
2. Resolve the action to run as `onRun` (the Service's, or
   `onWrapServiceRun`) and the readiness plan (below). For `COMMAND`,
   resolve the check action (the Service's `onReadinessCheck`, or
   `onWrapServiceReadinessCheck`) and build its second-slot runner
   (`prepare_readiness_check`: detached helper, detached runner base with
   `action_tag` = the check action's name, a fresh `ActionCancelSlot`). A
   failure here — a wrap hook's scope not resolving, or the detached
   helper not spawning — is returned from `launch()` with the state
   unchanged.
3. Register a fresh cancel token in the Session's main cancel slot and
   record the action's declared NOTIFY_THEN_TERMINATE grace (default 30 s —
   Template Schemas §5.3.2 "30 otherwise"), so `cancel_handle()` /
   `cancel_run()` deliver the right cancel, including over the cross-user
   helper pipe.
4. Build a `ScriptRunnerBase` via `Session::new_runner_base` (this moves the
   cross-user helper into the runner).
5. Reset the Service action status to `Running` and notify the callback.
6. `tokio::spawn(drive_run(..))`, passing the runner, the cloned symbol table,
   the resolved environment, the plan, the status, the callback, and fresh
   `watch` channels for readiness and exit. The `run_subprocess` future is
   `Send` on every platform (the vestigial Windows `HANDLE` it used to hold
   was removed), which is what makes spawning possible.

`drive_run` is one `tokio::select!` loop, `biased` toward messages. For a
`COMMAND` check it first spawns the check driver (`drive_readiness_check`, a
second task) with a `check_stop` token and a one-shot report channel:

```
loop select! {
    msg    = message_rx.recv()                            → apply (status/progress/fail; service_ready; env lines ignored)
    report = check_rx.recv(),  if check active            → READY if pending (onRun still running); stop the check
    r      = onRun future                                 → exit observed; stop the check
    ok     = TCP probe future, if pending && TCP_CONNECT  → READY, or re-arm the probe after 1 s
    _      = readiness deadline, if pending               → TimedOut; stop the check
}
```

After the exit, remaining messages are drained with the same handler, except
that an `openjd_service_ready` drained after the exit cannot make the instance
READY ("`onRun` exit wins"), and a check report is no longer received. The
driver awaits the check driver (so an invocation in flight is canceled and
reaped before the exit is published — and so before `end()` can run
`onExit`, rule 5), merges its redacted values, then: if readiness is still
pending it becomes `ExitedBeforeReady`; it logs `Service '<name>' onRun
exited: <state> (exit code: N)[, canceled by the runtime]`, finishes the
status, notifies the callback, publishes the `ServiceRunExit`, and returns
the runner.

`onRun` runs with **no default timeout**; a declared `timeout` is measured
from launch and its expiry is reported as `state: Timeout` (Template Schemas
§5 note 3: an instance failure).

### Readiness

```rust
pub enum ServiceReadiness {
    Pending,
    Ready { message: Option<String> },   // STDOUT: the openjd_service_ready text
    TimedOut,                            // timeoutSeconds elapsed; onRun may still run
    ExitedBeforeReady,                   // onRun exited first
}
```

The timeout (`timeoutSeconds`, model default 300) is measured from launch.

- **`TCP_CONNECT`** — for each probed port (the check's `ports`, which the
  model restricts to TCP ports and defaults to every declared TCP port — a
  UDP port of a mixed Service is never probed, §9 item 7), the probe address
  is the loopback
  address of the same family when `bindAddress` is a wildcard (`0.0.0.0` →
  `127.0.0.1`, `::` → `::1`), otherwise `bindAddress` itself (an IP literal or
  a hostname). One probe round connects to every target in turn with a 1 s
  per-connect bound and closes immediately; READY when every connection
  succeeds. The first round starts immediately after launch; each failed round
  re-arms 1 s later. READY requires `onRun` still running: the probe arm is
  disabled once the exit is observed.
- **`STDOUT`** — READY on the first `openjd_service_ready: <message>` line on
  `onRun`'s stdout (same `openjd_<kind>: <payload>` syntax as every message;
  `ActionFilter` parses it to `ActionMessage::ServiceReady`). Later lines have
  no effect; the line is still echoed/logged like any directive. Under
  `TCP_CONNECT` and `COMMAND` the line is logged as ignored (`Ignoring
  openjd_service_ready from Service 'svc' onRun: its readiness check type is
  COMMAND`).
- **`COMMAND`** — `onReadinessCheck` is run by the check driver, in the
  second slot, concurrently with `onRun`. The first invocation begins as soon
  as `onRun` is launched (the driver is spawned together with the `onRun`
  future); each later one begins `intervalSeconds` (model default 5) after
  the previous one ends. One invocation is bounded by the action's own
  `timeout`, default `SERVICE_READINESS_CHECK_DEFAULT_TIMEOUT` (30 s): on
  overrun the process is terminated and the invocation counts as not ready.
  Any exit status other than 0, a timeout, or a command that cannot be run
  (`not ready (failed to run: …)`) is "not yet ready" — never a Service
  failure. Exit status 0 is reported to the `onRun` driver, which makes the
  instance READY iff `onRun` is still running and readiness is still pending
  (`Ready { message: None }`); the check driver then returns, so the action
  never runs again (rule 4). The readiness `timeoutSeconds` (default 300)
  is the `onRun` driver's deadline and runs continuously, including while an
  invocation is in progress. Readiness is not a liveness check.

  Per-invocation log lines (all tagged, see "Logging"): `Service 'svc'
  readiness check invocation N`, then one of `Readiness check invocation N:
  ready (exit code: 0)` / `not ready (exit code: 1)` / `not ready (exceeded
  its timeout)` / `not ready (failed to run: <error>)`, or `Canceling
  readiness check invocation N: its result will be discarded`.

READY is logged as `Service '<name>' is READY[: <message>]`; a timeout as
`Service '<name>' did not become READY within Ns (<TYPE> readiness check)`
(error level); an exit before READY as `Service '<name>' onRun exited before
becoming READY`.

On `TimedOut` the runtime does **not** cancel `onRun`: the RFC's restart
decision ("cancels `onRun` if it is still running … and waits for it to exit")
belongs to the scheduler, which calls `cancel_run` then `wait_exit`. It does
stop the check driver: a `COMMAND` invocation in flight at the deadline is
canceled, and no further invocation starts.

### Concurrency with `onRun` (RFC 0009 §9.6.1), rule by rule

1. **Embedded files** — written once at `enter()`, never rewritten (see
   `enter()` step 2). A wrapping Environment's embedded files (`Env.File.*`
   in its hooks) are likewise allocated and written once per Service
   Session, on the first `onWrapService*` hook that runs, and only
   re-registered for later hooks (`ServiceSession::wrap_hook_files`); their
   `data` may reference the wrapping Environment's `Param.*`, `let`s,
   `Session.*`, and `WrappedService.*` (constant for the Session), but not
   `WrappedAction.*`, which differs per hook — a file is never rewritten
   while `onWrapServiceRun` may be reading it.
2. **Stdout** — every `ActionMessage` from the check is logged and ignored
   (`log_check_message_ignored`); the check's result is `exit_code == 0`,
   not `SubprocessResult::state`, so an `openjd_fail` line cannot turn an
   exit-0 invocation into "not ready". The Service's `ActionStatus` is
   written by `onEnter`, `onRun`, `onExit` only.
3. **Log attribution** — see "Logging".
4. **At most one invocation at a time** — structural: the check driver is one
   sequential loop; it exists only between `launch()` and the `onRun` exit
   (so never while `onEnter` or `onExit` runs, which happen outside that
   window and in the main slot); it returns after reporting success, so
   nothing runs after READY.
5. **`onRun` exit wins** — a success report is only honored while the
   `onRun` future is still pending (the `check_rx` arm is disabled once the
   exit is observed, and the report channel is not drained afterwards); on
   exit the `onRun` driver cancels `check_stop`, the check driver cancels the
   in-flight invocation through its own slot (`SessionCancelHandle::cancel(None,
   false)` — the action's own cancelation method, full declared grace) and
   discards the result; the `onRun` driver awaits the check driver before
   publishing the exit. `end()` cancels `onRun`, awaits that exit (and thus
   the check), then runs `onExit`.
6. **Wrap hooks** — `onWrapServiceReadinessCheck` runs in the second slot
   while `onWrapServiceRun` runs in the main slot, exactly as the unwrapped
   pair does. Wrap scripts must tolerate this (RFC); nothing in the runtime
   serializes them.

The check SHOULD be read-only with respect to the working directory it shares
with `onRun` (RFC `<ServiceActions>` item 3). This is advice to template
authors; the runtime cannot enforce it.

### Exit

```rust
pub struct ServiceRunExit {
    pub state: ActionState,         // Success | Failed | Canceled | Timeout
    pub exit_code: Option<i32>,
    pub canceled: bool,             // requested through this runtime
    pub fail_message: Option<String>,
    pub stdout: String,             // when debug_collect_stdout
}
```

`canceled` is true when the cancel was requested through `cancel_run`,
`end`, or the cancel handle, or the runner reported `Canceled` — i.e. the
exit is *not* an instance failure under "Failure and restart". A command
that cannot be started, or a format string that fails to resolve, is a
`Failed` exit with `exit_code: None` and the error as `fail_message`
(instance failure, not a `launch()` error). As for every other action in this
crate, an `openjd_fail` line makes the action `Failed` regardless of exit
status and supplies `fail_message`.

Observation API: `wait_ready()` / `readiness()` / `readiness_watch()`,
`wait_exit()` / `run_exit()` / `exit_watch()`, `action_status()` (the Service
action's `ActionStatus`, also delivered to the `SessionConfig` callback), and
`launch_count()`.

## `cancel_run(time_limit)`

Cancels the running `onRun` with the action's `cancelation` method —
`NOTIFY_THEN_TERMINATE` grace capped at `time_limit`, `Some(0)` terminates
immediately — through `Session::cancel_handle()`, and marks the pending exit
`canceled`. Returns `false` when no `onRun` is running (state ≠ `Running`).
`cancel_handle()` returns the same `SessionCancelHandle`, which also cancels a
running `onEnter`, `onExit`, or Environment action from another task.

## Wrap hooks (`WRAP_ACTIONS` + `SERVICE`)

`Session::service_wrap_hooks()` returns the innermost entered Environment
that defines any wrap hook (`Session::active_wrap_env`, RFC 0008's single
layer), provided its `runScope` includes `SERVICE` — an Environment whose
`runScope` excludes `SERVICE` (e.g. a `[TASK]` wrapper) is never entered in
a Service Session in the first place (`enter()` step 1), so it is skipped
entirely. The entered stack is the scope's `SERVICE`-scoped Environments
followed by the Service's `serviceEnvironments` (step 1a), so a wrapping
Service Environment — `run_scope` `None`, effective `[SERVICE]`, defining
`onWrapEnvEnter`, `onWrapEnvExit`, and the four `onWrapService*` hooks — is
found exactly as a wrapping Job or Step Environment is; the single-layer rule
spans both parts of the stack (`Session::enter_environment` rejects a second
wrapper with `MultipleWrapEnvironments`). When such an Environment is
present, `ServiceSession::resolve_action(kind)` substitutes the hook for the
Service's action:

| Service action | Hook | Runs when |
|---|---|---|
| `onEnter` | `onWrapServiceEnter` | the Service defines `onEnter` |
| `onRun` | `onWrapServiceRun` | always (every Service defines `onRun`) |
| `onReadinessCheck` | `onWrapServiceReadinessCheck` | readiness type is `COMMAND` (so the Service defines `onReadinessCheck`) |
| `onExit` | `onWrapServiceExit` | the Service defines `onExit` and any Service action ran |

("Nothing to replace", RFC rule 2 / §4.3 rule 6.) A hook that is defined but
whose Service action is not never runs; a Service action whose hook the
wrapping Environment does not define runs unwrapped (the CLI's validation
ensures a `SERVICE`-scoped wrapper defines all four). The log line `Service
'svc' onRun: running onWrapServiceRun of wrapping Environment 'W' in its
place` records each substitution.

The hook's scope is built as for RFC 0008's hooks, through the shared
`seed_wrapped_action_symbols`: `Session::wrap_hook_base_symtab()` (the
Session's base table with path mapping materialized), the wrapping
Environment's `Env.File.*` (see rule 1 above), its frozen `resolved_symtab`
and script `let` bindings, then `WrappedAction.Command` / `.Args` /
`.Environment` / `.Timeout` / `.Cancelation.Mode` /
`.Cancelation.NotifyPeriodInSeconds` — resolved against the **Service's**
own symbol table (`Param.*`, `Service.*`, `Service.File.*`, its `let`s) and
with the Service's document library, so a wrapper-defined name never leaks
into the wrapped command, while the wrapping Environment's `let`s, embedded
files, and the hook's own command/args/timeout/cancelation resolve with
*its* document's library (`WrapLibraries { inner, hook }`) — and
`WrappedService.Name` / `.PortNames` / `.Ports` / `.BindAddresses` /
`.Protocols` (`"TCP"` / `"UDP"` per port;
`openjd_model::job::service_symbols::add_wrapped_service_symbols`, parallel
lists in port declaration order). `WrappedAction.Environment` is the
session-defined environment the wrapped action would have run with: the
entered Environments' `variables` and `openjd_env` exports (RFC 0008), then
the Service's `variables`, then `onEnter`'s `openjd_env` / `openjd_unset_env`
changes (`ServiceSession::wrapped_env_vars`) — host-inherited variables
excluded. `WrappedAction.Cancelation.NotifyPeriodInSeconds` applies the
30-second default (§5.3.2 "30 otherwise"). The hook runs in the Service
action's process environment (`service_env_vars()`), with the Service
action's default timeout (none for `onEnter`/`onRun`, 30 s for the check,
300 s for `onExit`) unless it declares its own, and with its own
`cancelation`.

Stdout scanning is on the wrap script (RFC rule 4): `openjd_env` from a
wrapped `onEnter` and `openjd_service_ready` from a wrapped `onRun` are
honored when the wrapper forwards the wrapped process's stdout; the wrapped
check's exit status is the hook's exit status. Failure mapping is the
wrapped action's (rule 5): a failed `onWrapServiceEnter` is a start failure
(`ServiceScriptFailed { action: "onEnter" }`), an `onWrapServiceRun` exit is
an instance exit, a failed `onWrapServiceExit` is an `onExit` failure, and
an `onWrapServiceReadinessCheck` invocation's status has the check's
meaning. The wrapping Environment's own `onEnter` / `onExit` are never
wrapped, and inner Environments entered in the Service Session — Service
Environments after a wrapping one in the list, or every Service Environment
when the wrapper is a scope Environment — are wrapped by its `onWrapEnvEnter`
/ `onWrapEnvExit` exactly as in a Task Session — that is
`Session::enter_environment` / `exit_environment`'s existing behavior, which
`enter()` and `end()` call unchanged. A wrapping Service Environment's hooks
have the declaring Service's `Service.*` scope (including `bindAddress`)
through its folded `resolved_symtab`, in addition to `WrappedService.*`.

## Relaunch (constraint 5, "Failure and restart" 3.2)

`launch()` in `Exited` relaunches `onRun` in the same Session: same working
directory and endpoints, `onEnter` not re-run, its environment-variable changes
retained, embedded files already in place, a fresh readiness check (for
`COMMAND`, a fresh check driver and invocation count). Constraint
5 is enforced structurally: `launch()` in `Running` is
`InvalidServiceState`. Whether to relaunch here or open a new Session (new
ports) is the caller's decision.

## `end()` — constraint 7

Allowed in every state but `Ended`. Banner `Ending Service: <name>`. Every
step runs regardless of earlier failures and the first error is returned once
teardown is complete:

1. If `Running`: `cancel_run(None)` (the action's own method, full declared
   grace), then await the exit — which includes the cancelation of any
   `onReadinessCheck` invocation in flight (rule 5: canceled before `onExit`).
2. If any action of the Service has run (`onEnter` ran, or `onRun` was ever
   launched) and `onExit` is defined: run it (or `onWrapServiceExit`) in the
   foreground with the 300 s default timeout (banner `Service onExit:
   <name>`). A non-`Success` result
   is `SessionError::ServiceScriptFailed { action: "onExit", .. }` —
   reported, but per RFC 0009 it does not change the outcome of the scope.
   `onExit` does not run after an Environment `onEnter` start failure, since
   no Service action ran.
3. Exit the entered Environments in reverse order
   (`Session::exit_environment(id, None, false, None)`): the Service's
   `serviceEnvironments` first (last entered), then the scope's.
4. `Session::cleanup()` — the working directory is deleted (unless
   `retain_working_dir`); the cross-user helper is shut down.

State becomes `Ended`. Dropping a `ServiceSession` without `end()` logs a
warning, cancels the check driver's stop token, aborts the `onRun` driver
task (which detaches but does not stop the `onRun` process beyond what the
`Session`'s `Drop` does), and relies on `Session`'s `Drop` for the working
directory.

`start()` is `enter()` → `launch()` → `wait_ready()`, returning the terminal
readiness.

## Logging and attribution (rule 3)

Service Sessions use the same `session_log!` records (session id,
`LogContent`, timestamp) and the same per-action banners as Task Sessions.
`onEnter`, `onRun`, and `onExit` run one at a time in the main slot, so their
banners delimit their output as in any Session, and `onRun`'s lines stay
**untagged** — it is the Service's main stream and the only one present once
the instance is READY.

`onReadinessCheck` interleaves with `onRun`, so every record about it is
attributed to it in both forms RFC 0009 describes:

- **structured**: the record carries the key-value field `openjd_action =
  "<action name>"` next to `session_id` / `openjd_log_content` /
  `openjd_timestamp_usec`;
- **plain text**: the message is prefixed with `[<action name>] `, e.g.
  `[onReadinessCheck] connection refused`.

Both come from the `session_action_log!` macro (`logging.rs`), the tagged
sibling of `session_log!`. The tag is carried by `ScriptRunnerBase::action_tag`
→ `ActionFilter::action_tag`, and `run_subprocess` / `run_via_helper` use it
for every record they emit about the action: `Running command …`, `Command
started as pid: …`, `Output:`, each `COMMAND_OUTPUT` line, cancel/timeout
notices, `Process exit code: …`. The runner's `Phase: Running action`
subsection banner is replaced by one tagged line for a tagged action (banners
would interleave). The check driver's own lines (invocation start/outcome,
ignored messages, cancelation) are tagged the same way. The tag is the name
of the action that actually ran: `onReadinessCheck`, or
`onWrapServiceReadinessCheck` when wrapped (RFC rule 3 applies to the wrap
script's output too). The tag is added by the runtime; the Service's
processes do not prefix their own output. Redaction applies to the line
before the prefix is added.

Not implemented (RFC MAY): collapsing the output of invocations that
succeed. Lines are logged as they stream, before the exit status is known;
buffering them per invocation would be a separate change.

**Session tag (merged logs).** The above assumes one log stream per Session.
A single-host runner that merges every Session's log into one — `openjd run`
— sets `SessionConfig::log_tag` (the CLI: `Service <name>`, with `(from
<document>)` for an external Service), and then *every* record of the Service
Session carries it ahead of any action tag: `[Service Files] line` for
`onRun`, `onEnter`, `onExit` and the entered Environments' actions, `[Service
Files] [onReadinessCheck] line` for the check (the action tag's rule-3
meaning is unchanged), and the Session's section banners — `Starting Service:
…`, `Entering Environment: …`, `Service onEnter: …`, `Service onRun: … (launch
N)`, `Ending Service: …`, `Service onExit: …`, `Exiting Environment: …` —
collapse to one tagged `BANNER` line each (`Session::log_banner`). The check
driver passes the Session's tag into its `LogTag` alongside the action name.
See `specs/sessions/logging.md` "LogTag and session_tagged_log!". Without a
tag nothing changes.

## Not covered by this runtime (by design)

- Port allocation, `bindAddress`/`connectAddress` selection, and the restart
  policy are inputs/decisions of the caller.
- Validation of the Environment stack given to the Service Session — the
  single-layer rule (checked by `Session::enter_environment` as in any
  Session), and that a `SERVICE`-scoped wrapping Environment defines all four
  `onWrapService*` hooks (§9.7 item 6) — is the CLI's / scheduler's.
- Collapsing successful `onReadinessCheck` output (RFC MAY) — see "Logging".
- Host loss (constraint 8) is a scheduler concern; nothing here waits on a
  lost host because nothing here runs there.
