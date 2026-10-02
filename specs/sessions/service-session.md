# Service Session (RFC 0009)

## Overview

A **Service Session** runs the actions of one `<Service>` (RFC 0009, the `SERVICE`
extension) on the service host. It is implemented by `ServiceSession` in
`service_session.rs`, a sibling of `Session` that composes one: the `Session`
owns the working directory, the Environment stack, cumulative environment
variables, path mapping, redaction, the cross-user helper, and cleanup; the
`ServiceSession` adds the `Service.*` symbol scope, Service `variables`,
`onEnter`'s retained `openjd_env` changes, the background `onRun` driver with
its two-phase health check (readiness, then health monitoring until
UNHEALTHY), the second action slot in which `onHealthCheck` runs
concurrently with `onRun` (`COMMAND` probes), the `onWrapService*` hook
dispatch, and the constraint-7 teardown.

Normative references: RFC 0009 "Modifications to How Jobs Are Run" (Service
lifecycle constraints, "Services run inside Environments", "Failure and
restart"), `<ServiceActions>` incl. "Concurrency with `onRun`" (rules 1–6),
`<ServiceHealthCheck>`, the `<EnvironmentActions>` modification
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
| Enter `SERVICE`-scoped Environments, run `onEnter`, launch `onRun`, probe readiness then health, report exit, relaunch, `onExit`, exit Environments, cleanup | Port allocation and `bindAddress`/`connectAddress` choice |
| Detect instance failures: ready timeout, `onRun` exit before READY, `onRun` exit at any time, UNHEALTHY (`failureThreshold` consecutive failed probes after READY) | Restart decision (`restartPolicy`, relaunch vs. new Session), Task gating, multi-Service ordering |
| Cancel `onRun` with its own `cancelation` method on request, at `end()`, and on UNHEALTHY (constraint 11) | When to cancel otherwise (scope complete, ready timeout, …) |
| Run `onHealthCheck` concurrently with `onRun` under the rules of §9.6.1; run the `onWrapService*` hooks of the entered wrapping Environment in place of the Service's actions | Validate the Environment stack (single wrap layer, hook set matches `runScope`, §9.7) |

## Why a sibling type, not a mode of `Session`

`Session`'s state machine runs one action at a time behind `&mut self`, and
every caller-facing operation (`enter_environment`, `run_task`, …) awaits the
action to completion. A Service's `onRun` is the opposite: it is launched and
*not* awaited, and while it runs the caller must be able to observe health,
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

`onHealthCheck` is the one action that runs *while* another runs, so it
gets a **second slot** of its own: its own `ActionCancelSlot`, its own
`ScriptRunnerBase` (built by `Session::new_detached_runner_base`, which does
not take the Session's cross-user helper), its own cross-user helper when the
Session is cross-user (`Session::spawn_detached_helper` — the helper protocol
runs one command at a time, so each concurrent action needs its own helper
process, spawned from the same helper binary and shut down with the runner),
and its own log attribution tag. The runtime alone cancels through the second
slot (when `onRun` exits, probing stops, or the Session
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
Service. It governs the Service's actions, `variables`, `<ServiceScript>.let`, and
embedded files.
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
  TCP_CONNECT health check names port 'ingest', whose protocol is UDP;
  only TCP ports can be probed")` — model validation already forbids this.
- A `COMMAND` health check comes with an `onHealthCheck`: else
  `SessionError::Runtime("Service 'svc': health check type is COMMAND but
  onHealthCheck is not defined")` (model validation already forbids this
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
   Session — not before `onRun`, not before any `onHealthCheck`
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
   Environment overrides an earlier one);
2. the Service's `variables`;
3. `onEnter`'s `openjd_env` / `openjd_redacted_env` (when redaction is enabled
   by the profile) / `openjd_unset_env` changes.

This is the precedence RFC 0009 "Services run inside Environments" states
(Environments < Service `variables` < `onEnter`). The map is recomputed for each
action, so every `onRun` instance (including relaunches) and `onExit` see
`onEnter`'s changes — they are retained across relaunches because `onEnter`
is not re-run (constraint 5 / "Failure and restart" step 3.2).

The same map is given to `onHealthCheck`, which therefore sees
`onEnter`'s variables too (RFC 0009: "every instance of *onRun*,
*onHealthCheck*, and *onExit*").

Messages honored per action ("Environment variables within a Service", the
wiki's message table, and §9.6.1 rule 2):

| Message | `onEnter` | `onRun` | `onHealthCheck` | `onExit` |
|---|---|---|---|---|
| `openjd_status` / `openjd_progress` / `openjd_fail` | honored | honored | ignored (logged) | honored |
| `openjd_env` / `openjd_redacted_env` / `openjd_unset_env` | honored | ignored (logged) | ignored (logged) | ignored (logged) |
| malformed env command (`CancelMarkFailed`) | cancels + fails the action | ignored (logged) | ignored (logged) | ignored (logged) |
| `openjd_service_ready` | ignored (logged) | honored iff type is `STDOUT` | ignored (logged) | ignored (logged) |

"Ignored (logged)" is one `info` line, e.g. `Ignoring openjd_env from Service
'svc' onRun: environment variable messages are honored only from onEnter`, or
for the check `[onHealthCheck] Ignoring openjd_fail from Service 'svc'
onHealthCheck: messages on the health check's stdout are not honored`.
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
and `Health check: <TYPE> (readinessTimeoutSeconds N[, readinessIntervalSeconds
N], healthIntervalSeconds N, failureThreshold N)` — or, for a `STDOUT` check
without a heartbeat, `Health check: STDOUT (readinessTimeoutSeconds N, no
heartbeat after READY)`. Steps:

1. Reclaim the previous driver, if any (restores the cross-user helper).
2. Resolve the action to run as `onRun` (the Service's, or
   `onWrapServiceRun`) and the health plan (below). For `COMMAND`,
   resolve the check action (the Service's `onHealthCheck`, or
   `onWrapServiceHealthCheck`) and build its second-slot runner
   (`prepare_health_check`: detached helper, detached runner base with
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
   `watch` channels for health and exit, and the Session's cancel handle
   (for canceling `onRun` on UNHEALTHY). The `run_subprocess` future is
   `Send` on every platform (the vestigial Windows `HANDLE` it used to hold
   was removed), which is what makes spawning possible.

`drive_run` is one `tokio::select!` loop, `biased` toward messages, around a
`HealthTracker` (phase: `Pending` → `Ready { failed_probes }` → `Stopped`).
For a `COMMAND` check it first spawns the check driver (`drive_health_check`,
a second task) with a `check_stop` token and a request channel: each probe is
one request carrying a `oneshot` for its `ProbeResult` (`Ok` or `Failed(why)`),
so invocations are sequential by construction. A `TCP_CONNECT` probe is a
`tcp_probe` future; a `STDOUT` check has no probe future — its probe is the
`openjd_service_ready` message, with a heartbeat timer after READY:

```
loop select! {
    msg   = message_rx.recv()                        → apply (status/progress/fail; service_ready = STDOUT probe; env lines ignored)
    r     = onRun future                             → exit observed; stop everything
    res   = probe in flight, if any                  → Pending: Ok → READY, arm next after healthIntervalSeconds (or Stopped if none);
                                                       Failed → "not yet READY: why", arm next after readinessIntervalSeconds
                                                       Ready: apply_health_probe → reset / "failed (n of t)" / UNHEALTHY → cancel onRun, stop the check
    ()    = next-probe timer, if armed               → start the next probe (unless Stopped)
    ()    = heartbeat timer, if armed (STDOUT)       → one failed probe ("no openjd_service_ready line within Ns"); re-arm, or UNHEALTHY → cancel onRun
    _     = ready deadline, if Pending               → TimedOut; stop the check
}
```

After the exit, remaining messages are drained with the same handler, except
that an `openjd_service_ready` drained after the exit cannot make the instance
READY or count as a heartbeat ("`onRun` exit wins", constraint 11), and a
probe result is no longer received. The driver awaits the check driver (so an
invocation in flight is canceled and reaped before the exit is published —
and so before `end()` can run `onExit`, rule 5), merges its redacted values,
then: if readiness is still pending it becomes `ExitedBeforeReady`; it logs
`Service '<name>' onRun exited: <state> (exit code: N)[, canceled by the
runtime[: UNHEALTHY]]`, finishes the status, notifies the callback, publishes
the `ServiceRunExit`, and returns the runner.

`onRun` runs with **no default timeout**; a declared `timeout` is measured
from launch and its expiry is reported as `state: Timeout` (Template Schemas
§5 note 3: an instance failure).

### Health

```rust
pub enum ServiceHealth {
    Pending,                                               // phase 1: no probe has passed
    Ready { message: Option<String>, failed_probes: u64 }, // phase 2; message = STDOUT ready text; failed_probes < failureThreshold
    TimedOut,                                              // readinessTimeoutSeconds elapsed; onRun may still run
    ExitedBeforeReady,                                     // onRun exited first
    Unhealthy(ServiceUnhealthy),                           // failureThreshold reached; onRun canceled by the runtime
}

pub struct ServiceUnhealthy {
    pub failed_probes: u64,       // == failure_threshold
    pub failure_threshold: u64,
    pub last_failure: String,     // what the last failed probe reported
}
// Display: "3 consecutive health probes failed (failureThreshold: 3); last probe: <last_failure>"
```

`is_terminal()` is true for everything but `Pending` (it is what
`wait_ready()` returns on); `is_ready()` for `Ready` only. The watch channel
re-sends `Ready` whenever `failed_probes` changes, so a scheduler can log
each below-threshold failure if it wants to.

One probe mechanism, two phases (RFC `<ServiceHealthCheck>`). **Phase 1
(readiness):** the first probe starts as soon as `onRun` is launched; each
failed probe logs `Service '<name>' is not yet READY: <why>` and the next
starts `readinessIntervalSeconds` (model default 1 for `TCP_CONNECT`, 5 for
`COMMAND`) after it ends; the first success makes the instance READY (`Ready
{ failed_probes: 0 }`) iff `onRun` is still running. `readinessTimeoutSeconds`
(model default 300) is measured from launch and runs continuously, including
while a probe is in progress. **Phase 2 (health):** a probe starts
`healthIntervalSeconds` (model default 30) after the previous one ends. A
failure increments `failed_probes` and logs `Service '<name>' health probe
failed (n of t): <why>` (warn); a success resets it, logging `Service
'<name>' health probe succeeded; failure count reset from n` when it was
non-zero. When `failed_probes` reaches `failureThreshold` (model default 3)
the instance is UNHEALTHY: `Service '<name>' is UNHEALTHY: <ServiceUnhealthy>`
(error), `Unhealthy` is published, probing stops (the check driver is
stopped), and the runtime cancels `onRun` with its own cancelation method
through the Session's cancel handle (`Canceling Service '<name>' onRun: the
instance is UNHEALTHY`) — lifecycle constraint 11, "stopped the same way an
instance is stopped at scope end". The resulting `ServiceRunExit` has
`unhealthy: Some(..)` and `canceled: false`: UNHEALTHY is an instance failure,
and the caller takes the restart decision once the exit arrives. A probe
result that arrives after probing stopped is discarded.

- **`TCP_CONNECT`** — for each probed port (the check's `ports`, which the
  model restricts to TCP ports and defaults to every declared TCP port — a
  UDP port of a mixed Service is never probed, §9 item 6), the probe address
  is the loopback address of the same family when `bindAddress` is a
  wildcard (`0.0.0.0` → `127.0.0.1`, `::` → `::1`), otherwise `bindAddress`
  itself (an IP literal or a hostname). One probe connects to every target in
  turn with a 1 s per-connect bound and closes immediately; it succeeds when
  every connection does, else it fails naming the first port that did not
  (`TCP connect to port 'main' (127.0.0.1:4100) failed: Connection refused`
  / `… timed out after 1s`). The same probe serves both phases.
- **`STDOUT`** — the probe is an `openjd_service_ready: <message>` line on
  `onRun`'s stdout (same `openjd_<kind>: <payload>` syntax as every message;
  `ActionFilter` parses it to `ActionMessage::ServiceReady`). The first
  makes the instance READY (`Ready { message: Some(text), .. }`). Afterwards,
  when the check gives `healthIntervalSeconds`, the line is a heartbeat: a
  timer of that length is (re)started by READY and by every later line, and
  its expiry is one failed probe (`no openjd_service_ready line within Ns`)
  that re-arms the timer; a line after a miss is a successful probe that
  resets the count. Without `healthIntervalSeconds` the instance is not
  monitored after READY (`HealthPhase::Stopped`; its health is that `onRun`
  runs) and later lines have no effect — they are still echoed/logged like
  any directive. Under `TCP_CONNECT` and `COMMAND` the line is logged as
  ignored (`Ignoring openjd_service_ready from Service 'svc' onRun: its
  health check type is COMMAND`). `readinessIntervalSeconds` does not exist
  for this type.
- **`COMMAND`** — a probe is one `onHealthCheck` invocation, run by the check
  driver in the second slot, concurrently with `onRun`, on request from the
  `onRun` driver. One invocation is bounded by the action's own `timeout`,
  default `SERVICE_HEALTH_CHECK_DEFAULT_TIMEOUT` (30 s): on overrun the
  process is terminated and the probe fails (`onHealthCheck exceeded its
  timeout`); any exit status other than 0 (`onHealthCheck exit code: 1`) or
  a command that cannot be run (`onHealthCheck failed to run: …`) fails the
  probe; exit status 0 succeeds regardless of the output (rule 2). No single
  failure is a Service failure. The ready deadline is the `onRun` driver's,
  not the check driver's.

  Per-invocation log lines (all tagged, see "Logging"): `Service 'svc'
  health check invocation N`, then `Health check invocation N: succeeded
  (exit code: 0)` / `failed (onHealthCheck exit code: 1)` / `failed
  (onHealthCheck exceeded its timeout)` / `failed (onHealthCheck failed to
  run: <error>)`, or `Canceling health check invocation N: its result will
  be discarded`.

READY is logged as `Service '<name>' is READY[: <message>]`; a ready timeout
as `Service '<name>' did not become READY within Ns (<TYPE> health check)`
(error level); an exit before READY as `Service '<name>' onRun exited before
becoming READY`.

On `TimedOut` the runtime does **not** cancel `onRun`: the RFC's restart
decision ("cancels `onRun` if it is still running … and waits for it to exit")
belongs to the scheduler, which calls `cancel_run` then `wait_exit`. It does
stop the check driver: a `COMMAND` invocation in flight at the deadline is
canceled, and no further invocation starts. On `Unhealthy` the runtime
*does* cancel `onRun` (constraint 11 says how an UNHEALTHY instance is
stopped); the scheduler only awaits the exit.

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
   exit-0 invocation into a failed probe. The Service's `ActionStatus` is
   written by `onEnter`, `onRun`, `onExit` only.
3. **Log attribution** — see "Logging".
4. **At most one invocation at a time** — structural: the check driver is one
   sequential loop that runs one invocation per request, and the `onRun`
   driver requests the next only after receiving the previous result and
   waiting the phase's interval; it exists only between `launch()` and the
   `onRun` exit (so never while `onEnter` or `onExit` runs, which happen
   outside that window and in the main slot). Invocations continue for as
   long as `onRun` runs: every `readinessIntervalSeconds` before READY and
   every `healthIntervalSeconds` after, until UNHEALTHY stops them.
5. **`onRun` exit wins** (and constraint 11) — a probe result is only
   honored while the `onRun` future is still pending (the probe arm is
   disabled once the exit is observed and the in-flight future dropped); on
   exit the `onRun` driver cancels `check_stop`, the check driver cancels the
   in-flight invocation through its own slot (`SessionCancelHandle::cancel(None,
   false)` — the action's own cancelation method, full declared grace) and
   discards the result (the dropped `oneshot` tells the requester so); the
   `onRun` driver awaits the check driver before publishing the exit. This
   holds whether the instance was READY or not: an exit after READY is an
   ordinary instance failure, never UNHEALTHY. `end()` cancels `onRun`,
   awaits that exit (and thus the check), then runs `onExit`.
6. **Wrap hooks** — `onWrapServiceHealthCheck` runs in the second slot
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
    pub canceled: bool,             // requested by the caller through this runtime
    pub unhealthy: Option<ServiceUnhealthy>, // Some: the health check canceled onRun (UNHEALTHY)
    pub fail_message: Option<String>,
    pub stdout: String,             // when debug_collect_stdout
}
```

`canceled` is true when the cancel was requested through `cancel_run`,
`end`, or the cancel handle, or the runner reported `Canceled` other than
because the health check canceled it — i.e. the exit is *not* an instance
failure under "Failure and restart". An UNHEALTHY exit has `unhealthy:
Some(..)` and `canceled: false` (it *is* an instance failure); when the
caller's own cancel races the health check's, both are set and the exit is
not a failure. A command
that cannot be started, or a format string that fails to resolve, is a
`Failed` exit with `exit_code: None` and the error as `fail_message`
(instance failure, not a `launch()` error). As for every other action in this
crate, an `openjd_fail` line makes the action `Failed` regardless of exit
status and supplies `fail_message`.

Observation API: `wait_ready()` / `health()` / `health_watch()`,
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
entirely. When such an Environment is present, `ServiceSession::resolve_action(kind)`
substitutes the hook for the Service's action:

| Service action | Hook | Runs when |
|---|---|---|
| `onEnter` | `onWrapServiceEnter` | the Service defines `onEnter` |
| `onRun` | `onWrapServiceRun` | always (every Service defines `onRun`) |
| `onHealthCheck` | `onWrapServiceHealthCheck` | health check type is `COMMAND` (so the Service defines `onHealthCheck`) |
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
an `onWrapServiceHealthCheck` invocation's status has the check's
meaning. The wrapping Environment's own `onEnter` / `onExit` are never
wrapped, and inner Environments entered in the Service Session (the scope's
Environments after a wrapping one) are wrapped by its `onWrapEnvEnter` /
`onWrapEnvExit` exactly as in a Task Session — that is
`Session::enter_environment` / `exit_environment`'s existing behavior, which
`enter()` and `end()` call unchanged.

## Relaunch (constraint 5, "Failure and restart" 3.2)

`launch()` in `Exited` relaunches `onRun` in the same Session: same working
directory and endpoints, `onEnter` not re-run, its environment-variable changes
retained, embedded files already in place, a fresh health check (for
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
   `onHealthCheck` invocation in flight (rule 5: canceled before `onExit`).
2. If any action of the Service has run (`onEnter` ran, or `onRun` was ever
   launched) and `onExit` is defined: run it (or `onWrapServiceExit`) in the
   foreground with the 300 s default timeout (banner `Service onExit:
   <name>`). A non-`Success` result
   is `SessionError::ServiceScriptFailed { action: "onExit", .. }` —
   reported, but per RFC 0009 it does not change the outcome of the scope.
   `onExit` does not run after an Environment `onEnter` start failure, since
   no Service action ran.
3. Exit the entered Environments in reverse order
   (`Session::exit_environment(id, None, false, None)`).
4. `Session::cleanup()` — the working directory is deleted (unless
   `retain_working_dir`); the cross-user helper is shut down.

State becomes `Ended`. Dropping a `ServiceSession` without `end()` logs a
warning, cancels the check driver's stop token, aborts the `onRun` driver
task (which detaches but does not stop the `onRun` process beyond what the
`Session`'s `Drop` does), and relies on `Session`'s `Drop` for the working
directory.

`start()` is `enter()` → `launch()` → `wait_ready()`, returning the readiness
decision.

## Logging and attribution (rule 3)

Service Sessions use the same `session_log!` records (session id,
`LogContent`, timestamp) and the same per-action banners as Task Sessions.
`onEnter`, `onRun`, and `onExit` run one at a time in the main slot, so their
banners delimit their output as in any Session, and `onRun`'s lines stay
**untagged** — it is the Service's main stream and the only one present once
the instance is READY.

`onHealthCheck` interleaves with `onRun`, so every record about it is
attributed to it in both forms RFC 0009 describes:

- **structured**: the record carries the key-value field `openjd_action =
  "<action name>"` next to `session_id` / `openjd_log_content` /
  `openjd_timestamp_usec`;
- **plain text**: the message is prefixed with `[<action name>] `, e.g.
  `[onHealthCheck] connection refused`.

Both come from the `session_action_log!` macro (`logging.rs`), the tagged
sibling of `session_log!`. The tag is carried by `ScriptRunnerBase::action_tag`
→ `ActionFilter::action_tag`, and `run_subprocess` / `run_via_helper` use it
for every record they emit about the action: `Running command …`, `Command
started as pid: …`, `Output:`, each `COMMAND_OUTPUT` line, cancel/timeout
notices, `Process exit code: …`. The runner's `Phase: Running action`
subsection banner is replaced by one tagged line for a tagged action (banners
would interleave). The check driver's own lines (invocation start/outcome,
ignored messages, cancelation) are tagged the same way. The tag is the name
of the action that actually ran: `onHealthCheck`, or
`onWrapServiceHealthCheck` when wrapped (RFC rule 3 applies to the wrap
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
Files] [onHealthCheck] line` for the check (the action tag's rule-3
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
- Collapsing successful `onHealthCheck` output (RFC MAY) — see "Logging".
- Host loss (constraint 8) is a scheduler concern; nothing here waits on a
  lost host because nothing here runs there.
