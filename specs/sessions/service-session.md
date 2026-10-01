# Service Session (RFC 0009)

## Overview

A **Service Session** runs the actions of one `<Service>` (RFC 0009, the `SERVICE`
extension) on the service host. It is implemented by `ServiceSession` in
`service_session.rs`, a sibling of `Session` that composes one: the `Session`
owns the working directory, the Environment stack, cumulative environment
variables, path mapping, redaction, the cross-user helper, and cleanup; the
`ServiceSession` adds the `Service.*` symbol scope, Service `variables`,
`onEnter`'s retained `openjd_env` changes, the background `onRun` driver with
its readiness check, and the constraint-7 teardown.

Normative references: RFC 0009 "Modifications to How Jobs Are Run" (Service
lifecycle constraints, "Services run inside Environments", "Failure and
restart"), `<ServiceActions>`, `<ServiceReadinessCheck>`, "Environment
variables within a Service", and the `openjd_service_ready` message; wiki
*How-Jobs-Are-Run* § Services.

### What this runtime decides, and what it leaves to the caller

The runtime implements everything that happens *inside* one Service Session.
The scheduler's decisions stay with the caller (`openjd-cli` or a worker
agent):

| Runtime (this crate) | Caller |
|---|---|
| Enter `SERVICE`-scoped Environments, run `onEnter`, launch `onRun`, probe readiness, report exit, relaunch, `onExit`, exit Environments, cleanup | Port allocation and `bindAddress`/`connectAddress` choice |
| Detect instance failures: readiness timeout, `onRun` exit before READY, `onRun` exit at any time | Restart decision (`restartPolicy`, relaunch vs. new Session), Task gating, multi-Service ordering |
| Cancel `onRun` with its own `cancelation` method on request and at `end()` | When to cancel (scope complete, readiness timed out, …) |

Not yet implemented (deferred to a later milestone): the `COMMAND` readiness
type and `onReadinessCheck` (RFC 0009 "Concurrency with `onRun`"), and the
`onWrapService*` wrap hooks. `ServiceSession::with_config` rejects a Service
whose readiness check type is `COMMAND`.

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
`evaluate_env_vars`, `embedded_files`, `new_runner_base` /
`restore_runner_base` (a fully configured `ScriptRunnerBase`, carrying the
cross-user helper), `cancel_fields` (the shared per-action cancel slot, so one
`SessionCancelHandle` cancels whichever action — Environment or Service — is
running), `callback_arc`, `library_arc`, `limits`, `add_redacted_values`.

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
    pub session: SessionConfig,                 // as for a Task Session
    pub service: job::Service,
    pub environments: Vec<job::Environment>,    // the scope's Environments, in entry order
    pub endpoints: ServiceEndpoints,            // own ports: port, bindAddress, connectAddress
    pub in_scope_endpoints: Vec<ServiceEndpoints>, // earlier Services (no bindAddress)
}
```

Checks, before any directory is created:

- `endpoints.name == service.name`, else `SessionError::Runtime("Service 'svc'
  was given the endpoint assignment of Service 'other'")`.
- Every declared port, and every port a `TCP_CONNECT` check names, has an
  endpoint: else `SessionError::ServicePortUnassigned { name, port }` —
  `Service 'svc' port 'metrics' has no endpoint assignment`.
- The readiness type is not `COMMAND`: else `SessionError::Runtime("Service
  'svc': the COMMAND readiness check type is not supported by this runtime
  yet")`.

Then `Session::with_config(config.session)` runs — same working directory,
sticky-bit, cross-user helper, and host-info logging as any Session.

## `enter()` — opening the Session (constraints 1, 2 are the caller's; this is "Starting a Service")

Banner `Starting Service: <name>`. Any error leaves the state `StartFailed`
(a *start failure*), logs `Service '<name>' failed to start: <error>`, and
returns it; `end()` is still required.

1. **Environments** (RFC 0009 "Services run inside Environments"). For each
   configured Environment in order: if `!env.runs_in(RunScope::Service)`, log
   `Skipping Environment '<name>': its runScope does not include SERVICE` and
   continue; otherwise `Session::enter_environment(env,
   env.resolved_symtab, None, None)`. Environments cannot reference `Service.*`
   (model validation), so they resolve against the plain Session scope. An
   Environment `onEnter` failure returns `SessionError::EnvironmentScriptFailed`;
   the Environment counts as entered and is exited by `end()`, exactly as in a
   Task Session.
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

   Embedded files are written **once**. A Service Session's format-string
   values are constant for its lifetime, so the content never changes; RFC
   0009 "Concurrency with `onRun`" rule 1 explicitly allows not rewriting an
   unchanged file, and writing once also means a later concurrent action
   (milestone 7's `onReadinessCheck`) can never rewrite a script `onRun` is
   reading.
3. **Service `variables`**, resolved once against that table with
   `Session::resolve_env_var_value` — the same `string` target type, NUL
   rejection, and 2048-character cap (§4.4.2) as an Environment's.
4. **`onEnter`**, if defined: an ordinary foreground action (no default
   timeout; its `timeout` and `cancelation` apply; cancelable through
   `cancel_handle()`). Banner `Service onEnter: <name>`. A non-`Success`
   result is `SessionError::ServiceScriptFailed { name, action: "onEnter",
   reason }` with reason `exit code: N` / `canceled` / `timed out`.

## Environment variables of a Service action

`service_env_vars()` layers, lowest precedence first:

1. `Session::evaluate_env_vars(None)` — the process environment,
   `OPENJD_SESSION_WORKING_DIR`, and the entered Environments' `variables` and
   `openjd_env` / `openjd_unset_env` changes;
2. the Service's `variables`;
3. `onEnter`'s `openjd_env` / `openjd_redacted_env` (when redaction is enabled
   by the profile) / `openjd_unset_env` changes.

This is the precedence RFC 0009 "Services run inside Environments" states
(Environment < Service `variables` < `onEnter`). The map is recomputed for each
action, so every `onRun` instance (including relaunches) and `onExit` see
`onEnter`'s changes — they are retained across relaunches because `onEnter`
is not re-run (constraint 5 / "Failure and restart" step 3.2).

Messages honored per action ("Environment variables within a Service" and
the wiki's message table):

| Message | `onEnter` | `onRun` | `onExit` |
|---|---|---|---|
| `openjd_status` / `openjd_progress` / `openjd_fail` | honored | honored | honored |
| `openjd_env` / `openjd_redacted_env` / `openjd_unset_env` | honored | ignored (logged) | ignored (logged) |
| malformed env command (`CancelMarkFailed`) | cancels + fails the action | ignored (logged) | ignored (logged) |
| `openjd_service_ready` | ignored (logged) | honored iff type is `STDOUT` | ignored (logged) |

"Ignored (logged)" is one `info` line, e.g. `Ignoring openjd_env from Service
'svc' onRun: environment variable messages are honored only from onEnter`. The
value of an ignored `openjd_redacted_env` is still added to the Session's
redaction set — the directive's effect is ignored, not its secrecy. Nothing a
Service sets is ever propagated to the entities in its scope: the `Session`'s
own `created_env_vars` are untouched by Service actions.

## `launch()` — the background `onRun` driver

Allowed in `Entered` and `Exited`. Banner `Service onRun: <name> (launch N)`
and `Readiness check: <TYPE> (timeout Ns)`. Steps:

1. Reclaim the previous driver, if any (restores the cross-user helper).
2. Resolve the readiness plan (below).
3. Register a fresh cancel token in the Session's shared cancel slot and
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

`drive_run` is one `tokio::select!` loop, `biased` toward messages:

```
loop select! {
    msg  = message_rx.recv()                           → apply (status/progress/fail; service_ready; env lines ignored)
    r    = onRun future                                → exit observed
    ok   = TCP probe future,  if pending && TCP_CONNECT → READY, or re-arm the probe after 1 s
    _    = readiness deadline, if pending              → TimedOut
}
```

After the exit, remaining messages are drained with the same handler, except
that an `openjd_service_ready` drained after the exit cannot make the instance
READY ("`onRun` exit wins"). If readiness is still pending at exit it becomes
`ExitedBeforeReady`. The driver then logs `Service '<name>' onRun exited:
<state> (exit code: N)[, canceled by the runtime]`, finishes the status,
notifies the callback, publishes the `ServiceRunExit`, and returns the runner.

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
  model defaults to every declared port), the probe address is the loopback
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
  `TCP_CONNECT` the line is logged as ignored.

READY is logged as `Service '<name>' is READY[: <message>]`; a timeout as
`Service '<name>' did not become READY within Ns (<TYPE> readiness check)`
(error level); an exit before READY as `Service '<name>' onRun exited before
becoming READY`.

On `TimedOut` the runtime does **not** cancel `onRun`: the RFC's restart
decision ("cancels `onRun` if it is still running … and waits for it to exit")
belongs to the scheduler, which calls `cancel_run` then `wait_exit`.

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

## Relaunch (constraint 5, "Failure and restart" 3.2)

`launch()` in `Exited` relaunches `onRun` in the same Session: same working
directory and endpoints, `onEnter` not re-run, its environment-variable changes
retained, embedded files already in place, a fresh readiness check. Constraint
5 is enforced structurally: `launch()` in `Running` is
`InvalidServiceState`. Whether to relaunch here or open a new Session (new
ports) is the caller's decision.

## `end()` — constraint 7

Allowed in every state but `Ended`. Banner `Ending Service: <name>`. Every
step runs regardless of earlier failures and the first error is returned once
teardown is complete:

1. If `Running`: `cancel_run(None)` (the action's own method, full declared
   grace), then await the exit.
2. If any action of the Service has run (`onEnter` ran, or `onRun` was ever
   launched) and `onExit` is defined: run it in the foreground with the 300 s
   default timeout (banner `Service onExit: <name>`). A non-`Success` result
   is `SessionError::ServiceScriptFailed { action: "onExit", .. }` —
   reported, but per RFC 0009 it does not change the outcome of the scope.
   `onExit` does not run after an Environment `onEnter` start failure, since
   no Service action ran.
3. Exit the entered Environments in reverse order
   (`Session::exit_environment(id, None, false, None)`).
4. `Session::cleanup()` — the working directory is deleted (unless
   `retain_working_dir`); the cross-user helper is shut down.

State becomes `Ended`. Dropping a `ServiceSession` without `end()` logs a
warning, aborts the driver task (which detaches but does not stop the
`onRun` process beyond what the `Session`'s `Drop` does), and relies on
`Session`'s `Drop` for the working directory.

`start()` is `enter()` → `launch()` → `wait_ready()`, returning the terminal
readiness.

## Logging

Service Sessions use the same `session_log!` records (session id,
`LogContent`) and the same per-action banners as Task Sessions. In this
milestone exactly one Service action runs at a time, so banners suffice for
attribution; the log-attribution rule of "Concurrency with `onRun`" (rule 3)
applies once `onReadinessCheck` is implemented.

## Not covered by this runtime (by design or deferred)

- Port allocation, `bindAddress`/`connectAddress` selection, and the restart
  policy are inputs/decisions of the caller.
- `COMMAND` readiness / `onReadinessCheck` and the concurrency rules of RFC
  0009 §9.6.1 — deferred; `with_config` rejects `COMMAND`.
- `onWrapService*` hooks (`WRAP_ACTIONS` + `SERVICE`) — deferred. A wrapping
  Environment entered in a Service Session has its `onWrapEnvEnter` /
  `onWrapEnvExit` applied to the *inner Environments* as in any Session (that
  is `Session`'s existing behavior), but the Service's own actions run
  unwrapped.
- Host loss (constraint 8) is a scheduler concern; nothing here waits on a
  lost host because nothing here runs there.
