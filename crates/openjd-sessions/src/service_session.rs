// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Service Session runtime — RFC 0009 (`SERVICE` extension).
//!
//! A [`ServiceSession`] runs the actions of one `<Service>` on the service
//! host: it enters the Environments of the Service's scope whose `runScope`
//! includes `SERVICE`, runs the Service's `onEnter`, launches `onRun` in the
//! background, applies the health check (`TCP_CONNECT`, `STDOUT`, or
//! `COMMAND` — the latter running `onHealthCheck` concurrently with
//! `onRun` under the rules of RFC 0009 "Concurrency with `onRun`") in its
//! two phases — probing for readiness until the instance is READY, then
//! probing for health until `failureThreshold` consecutive failures make it
//! UNHEALTHY, an instance failure that cancels `onRun` — reports `onRun`'s
//! exit, allows `onRun` to be relaunched within the same Session,
//! and ends per *How Jobs Are Run* "Service lifecycle" constraint 7 (cancel
//! the running action, run `onExit`, exit the Environments in reverse,
//! delete the working directory). When the entered stack contains a
//! wrapping Environment (`WRAP_ACTIONS`) whose `runScope` includes
//! `SERVICE`, its `onWrapService*` hooks run in place of the Service's
//! actions.
//!
//! It composes a [`Session`] — which owns the working directory, the
//! Environment stack, cumulative environment variables, path mapping, the
//! cross-user helper, and cleanup — and adds the Service-specific pieces: the
//! `Service.*` symbol scope, Service `variables`, `onEnter`'s retained
//! `openjd_env` changes, the background `onRun` driver, the health check,
//! and the second action slot `onHealthCheck` runs in.
//!
//! See `specs/sessions/service-session.md`.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use openjd_expr::function_library::FunctionLibrary;
use openjd_model::job::service_symbols::{build_service_symbol_table, ServiceEndpoints};
use openjd_model::job::{
    Action, Environment, RunScope, Service, ServiceHealthCheck, ServicePortProtocol,
};
use openjd_model::symbol_table::SymbolTable;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::action::{ActionMessage, ActionState};
use crate::action_status::ActionStatus;
use crate::embedded_files::{EmbeddedFiles, EmbeddedFilesScope};
use crate::error::SessionError;
use crate::logging::{LogContent, LogTag};
use crate::runner::ScriptRunnerBase;
use crate::session::{
    declared_terminate_delay, normalize_env_key, seed_wrapped_action_symbols, ActionCancelSlot,
    ActionStatusFields, EnvVarChanges, Session, SessionCancelHandle, SessionConfig, SharedCallback,
    WrapLibraries, WrappedContext,
};
use crate::subprocess::SubprocessResult;
use crate::{session_log, session_tagged_log};

/// Default `timeout` of a Service's `onExit` (Template Schemas §5 defaults
/// table: 300 seconds, like an Environment's `onExit`).
pub const SERVICE_EXIT_DEFAULT_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Default `timeout` of a Service's `onHealthCheck` — one invocation
/// (RFC 0009 `<ServiceActions>` defaults table: 30 seconds).
pub const SERVICE_HEALTH_CHECK_DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Default `notifyPeriodInSeconds` for every Service action (Template
/// Schemas §5.3.2: 120 for a `<StepActions>` `onRun`, 30 otherwise).
const SERVICE_DEFAULT_NOTIFY_PERIOD: Duration = Duration::from_secs(30);

/// Bound on one `TCP_CONNECT` attempt so an unresponsive address cannot
/// stall the probe loop past the ready timeout (or, after READY, past the
/// health interval).
const TCP_PROBE_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

/// Configuration for a [`ServiceSession`].
pub struct ServiceSessionConfig {
    /// The underlying Session configuration: session id, job parameter
    /// values, path mapping rules, session user, callback, limits, …
    /// exactly as for a Session that runs Tasks.
    pub session: SessionConfig,
    /// The Service whose actions this Session runs.
    pub service: Service,
    /// The Job's `jobEnvironments`, in the order a Task Session enters them
    /// (never a Step's `stepEnvironments`: a Service belongs to no Step).
    /// Only those whose effective `runScope` includes `SERVICE` are entered;
    /// the rest are skipped with a log line.
    pub environments: Vec<Environment>,
    /// The document profile of each entry of
    /// [`environments`](Self::environments), index for index, for an
    /// Environment that comes from a document other than the Service's
    /// (Template Schemas §1.2 item 3: an extension applies to the document
    /// that lists it). `Some(profile)` enters that Environment through
    /// [`Session::enter_environment_with_profile`]; `None`, or an index past
    /// the end of this list, enters it with the Session's own profile —
    /// [`session`](Self::session)`.profile`, which is the profile of the
    /// Service's **own** document (the Job Template for a `services` entry,
    /// the attached Environment Template for an external Service) and also
    /// governs the Service's actions,
    /// `variables`, `let` bindings, and embedded files. An empty list means
    /// every scope Environment shares the Service's document.
    pub environment_profiles: Vec<Option<openjd_model::ModelProfile>>,
    /// The caller's endpoint assignment for every port the Service
    /// declares (`Service.<own>.<port>.*`, including `bindAddress`). Port
    /// allocation policy is the caller's: a single-host runner uses
    /// loopback for both addresses.
    pub endpoints: ServiceEndpoints,
    /// The endpoints of every Service this Service may reference —
    /// Services earlier in the start order (RFC 0009 "The `Service.*`
    /// scope"). Seeded without `bindAddress`.
    pub in_scope_endpoints: Vec<ServiceEndpoints>,
}

/// Lifecycle phase of a [`ServiceSession`].
///
/// ```
/// use openjd_sessions::ServiceSessionState;
///
/// assert_eq!(format!("{}", ServiceSessionState::Running), "RUNNING");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceSessionState {
    /// Constructed; nothing has run. [`ServiceSession::enter`] is next.
    Created,
    /// Environments entered and `onEnter` (if any) succeeded; no `onRun`
    /// is running. [`ServiceSession::launch`] is allowed.
    Entered,
    /// An `onRun` instance is running (launched and not yet exited).
    Running,
    /// The most recent `onRun` has exited. [`ServiceSession::launch`]
    /// (relaunch) or [`ServiceSession::end`] are allowed.
    Exited,
    /// Entering an Environment or running `onEnter` failed (a *start
    /// failure*). Only [`ServiceSession::end`] is allowed.
    StartFailed,
    /// [`ServiceSession::end`] has completed.
    Ended,
}

impl std::fmt::Display for ServiceSessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Created => write!(f, "CREATED"),
            Self::Entered => write!(f, "ENTERED"),
            Self::Running => write!(f, "RUNNING"),
            Self::Exited => write!(f, "EXITED"),
            Self::StartFailed => write!(f, "START_FAILED"),
            Self::Ended => write!(f, "ENDED"),
        }
    }
}

/// Health of the current (or most recent) `onRun` instance, as the
/// Service's health check (RFC 0009 `<ServiceHealthCheck>`) decides it in
/// its two phases: readiness before the first successful probe, health
/// after.
///
/// The three failure variants are the *instance failures* of RFC 0009
/// "Failure and restart" that this runtime can observe; the restart decision
/// is the caller's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceHealth {
    /// `onRun` is running and no probe has succeeded yet (phase 1).
    Pending,
    /// The instance is READY: a probe passed while `onRun` was running, and
    /// fewer than `failureThreshold` consecutive probes have failed since
    /// (phase 2). For a `STDOUT` check, `message` is the text of the first
    /// `openjd_service_ready` line.
    Ready {
        /// The informational `openjd_service_ready` message, if any.
        message: Option<String>,
        /// Consecutive failed health probes since the last success — below
        /// `failureThreshold`, else the instance is [`Unhealthy`](Self::Unhealthy).
        /// Always 0 for a `STDOUT` check without `healthIntervalSeconds`.
        failed_probes: u64,
    },
    /// `readinessTimeoutSeconds` elapsed (measured from launch) before a probe
    /// passed. `onRun` may still be running; the caller cancels it with
    /// [`ServiceSession::cancel_run`] before relaunching or ending.
    TimedOut,
    /// `onRun` exited before a probe passed.
    ExitedBeforeReady,
    /// `failureThreshold` consecutive health probes failed after READY. The
    /// runtime has canceled `onRun` with its cancelation method; the caller
    /// awaits the exit ([`ServiceSession::wait_exit`], whose
    /// [`ServiceRunExit::unhealthy`] carries the same detail) and takes the
    /// restart decision.
    Unhealthy(ServiceUnhealthy),
}

impl ServiceHealth {
    /// `true` once the readiness phase has reached a decision: anything but
    /// [`Pending`](Self::Pending). [`ServiceSession::wait_ready`] returns on
    /// the first such value.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::Pending)
    }

    /// `true` iff the instance is READY.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }
}

/// Why an instance became UNHEALTHY (RFC 0009 `<ServiceHealthCheck>` item
/// 6): `failureThreshold` consecutive health probes failed after READY.
///
/// ```
/// use openjd_sessions::ServiceUnhealthy;
///
/// let u = ServiceUnhealthy {
///     failed_probes: 2,
///     failure_threshold: 2,
///     last_failure: "onHealthCheck exit code: 1".into(),
/// };
/// assert_eq!(
///     u.to_string(),
///     "2 consecutive health probes failed (failureThreshold: 2); last probe: onHealthCheck exit code: 1"
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceUnhealthy {
    /// The consecutive failures observed — equal to `failure_threshold`.
    pub failed_probes: u64,
    /// The Service's effective `failureThreshold`.
    pub failure_threshold: u64,
    /// What the last failed probe reported (a refused connection, the
    /// `onHealthCheck` exit code or timeout, a missed heartbeat).
    pub last_failure: String,
}

impl std::fmt::Display for ServiceUnhealthy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} consecutive health probes failed (failureThreshold: {}); last probe: {}",
            self.failed_probes, self.failure_threshold, self.last_failure
        )
    }
}

/// How an `onRun` instance ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRunExit {
    /// `Success` (exit 0), `Failed` (non-zero exit, `openjd_fail`, or the
    /// process could not be started), `Canceled` (canceled through this
    /// runtime — by the caller, or by the health check on UNHEALTHY), or
    /// `Timeout` (the action's own `timeout` expired — an instance failure
    /// per Template Schemas §5 note 3).
    pub state: ActionState,
    /// The process exit code, when the process ran and reported one.
    pub exit_code: Option<i32>,
    /// `true` when the exit was requested through [`ServiceSession::cancel_run`],
    /// [`ServiceSession::end`], or the cancel handle — i.e. not an instance
    /// failure under RFC 0009 "Failure and restart". `false` for an exit the
    /// health check forced on UNHEALTHY (see [`unhealthy`](Self::unhealthy)),
    /// which is one.
    pub canceled: bool,
    /// `Some` when the health check canceled `onRun` because the instance
    /// became UNHEALTHY (RFC 0009 lifecycle constraint 11, "Failure and
    /// restart"). Set even when the caller's own cancelation raced it, in
    /// which case `canceled` is also `true` and the exit is not a failure.
    pub unhealthy: Option<ServiceUnhealthy>,
    /// The `openjd_fail` message, or the reason the action could not run.
    pub fail_message: Option<String>,
    /// `onRun`'s captured output, when `SessionConfig::debug_collect_stdout`
    /// is set; empty otherwise.
    pub stdout: String,
}

/// The health check to apply to a launched instance, with the
/// `TCP_CONNECT` targets resolved against the endpoint assignment.
#[derive(Debug, Clone)]
struct HealthPlan {
    probe: ProbePlan,
    /// `readinessTimeoutSeconds`, measured from launch.
    readiness_timeout: Duration,
    /// `healthIntervalSeconds`: the pause between the end of one probe and
    /// the start of the next after READY. `None` for a `STDOUT` check
    /// without a heartbeat: the instance is not monitored after READY.
    health_interval: Option<Duration>,
    /// `failureThreshold`.
    failure_threshold: u64,
}

/// The probe mechanism of a [`HealthPlan`].
#[derive(Debug, Clone)]
enum ProbePlan {
    TcpConnect {
        /// `(port name, address to connect to, port)` for each probed port.
        targets: Vec<(String, String, u16)>,
        /// `readinessIntervalSeconds`.
        readiness_interval: Duration,
    },
    Stdout,
    Command {
        /// `readinessIntervalSeconds`: the pause between the end of one
        /// `onHealthCheck` invocation and the start of the next before READY.
        readiness_interval: Duration,
    },
}

impl HealthPlan {
    fn type_name(&self) -> &'static str {
        match self.probe {
            ProbePlan::TcpConnect { .. } => "TCP_CONNECT",
            ProbePlan::Stdout => "STDOUT",
            ProbePlan::Command { .. } => "COMMAND",
        }
    }

    /// The pause between probes before READY; `None` for `STDOUT`, whose
    /// ready line arrives when it arrives.
    fn readiness_interval(&self) -> Option<Duration> {
        match self.probe {
            ProbePlan::TcpConnect {
                readiness_interval, ..
            }
            | ProbePlan::Command { readiness_interval } => Some(readiness_interval),
            ProbePlan::Stdout => None,
        }
    }

    /// One line describing the plan for the launch log.
    fn describe(&self) -> String {
        let mut s = format!(
            "Health check: {} (readinessTimeoutSeconds {}",
            self.type_name(),
            self.readiness_timeout.as_secs()
        );
        if let Some(i) = self.readiness_interval() {
            s.push_str(&format!(", readinessIntervalSeconds {}", i.as_secs()));
        }
        match self.health_interval {
            Some(i) => s.push_str(&format!(
                ", healthIntervalSeconds {}, failureThreshold {})",
                i.as_secs(),
                self.failure_threshold
            )),
            None => s.push_str(", no heartbeat after READY)"),
        }
        s
    }
}

/// The four actions of a Service, as the runtime dispatches them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceActionKind {
    Enter,
    Run,
    HealthCheck,
    Exit,
}

impl ServiceActionKind {
    /// The `<ServiceActions>` property name.
    fn name(self) -> &'static str {
        match self {
            Self::Enter => "onEnter",
            Self::Run => "onRun",
            Self::HealthCheck => "onHealthCheck",
            Self::Exit => "onExit",
        }
    }

    /// The `<EnvironmentActions>` hook that runs in its place in a wrapped
    /// Service Session (RFC 0009 `<EnvironmentActions>`).
    fn hook_name(self) -> &'static str {
        match self {
            Self::Enter => "onWrapServiceEnter",
            Self::Run => "onWrapServiceRun",
            Self::HealthCheck => "onWrapServiceHealthCheck",
            Self::Exit => "onWrapServiceExit",
        }
    }
}

/// A Service action resolved for one run: either the Service's own action
/// with the Service's symbol table, or — when the entered stack contains a
/// wrapping Environment whose `runScope` includes `SERVICE` and which
/// defines the corresponding hook — that hook with the hook's own scope
/// (`WrappedAction.*`, `WrappedService.*`, the wrapping Environment's
/// symbols).
struct ResolvedAction {
    /// The name of the action that actually runs: the `<ServiceActions>`
    /// name, or the hook's name when wrapped. Used in log lines and as the
    /// log attribution tag of `onHealthCheck`.
    name: &'static str,
    action: Action,
    symtab: Box<SymbolTable>,
    /// The function library the action's command, args, timeout, and
    /// cancelation resolve with: the Session's (the Service's document's)
    /// for the Service's own action, the wrapping Environment's document's
    /// for a hook.
    library: Arc<FunctionLibrary>,
}

/// Everything the background `onRun` driver owns.
struct RunDriverInputs {
    session_id: String,
    service_name: String,
    /// The Session's log tag (`SessionConfig::log_tag`), prefixed to the
    /// health probe lines so a consumer merging Sessions' logs can show a
    /// Service's probe timeline attributed.
    session_tag: Option<String>,
    runner: ScriptRunnerBase,
    /// The name of the action that runs as `onRun` (`onRun`, or
    /// `onWrapServiceRun` when wrapped).
    action_name: &'static str,
    action: Action,
    symtab: Box<SymbolTable>,
    library: Arc<FunctionLibrary>,
    env_vars: HashMap<String, Option<String>>,
    plan: HealthPlan,
    status: Arc<Mutex<ActionStatusFields>>,
    callback: Option<SharedCallback>,
    health_tx: watch::Sender<ServiceHealth>,
    exit_tx: watch::Sender<Option<ServiceRunExit>>,
    message_tx: mpsc::UnboundedSender<ActionMessage>,
    message_rx: mpsc::UnboundedReceiver<ActionMessage>,
    cancel_requested: Arc<AtomicBool>,
    /// Cancels `onRun` with its own cancelation method when the instance
    /// becomes UNHEALTHY (lifecycle constraint 11).
    cancel_handle: SessionCancelHandle,
    /// The `onHealthCheck` driver's inputs, for a `COMMAND` health check.
    /// The `onRun` driver spawns it, requests one invocation per probe,
    /// decides READY and UNHEALTHY from the results, and stops it
    /// (`check_stop`) when probing ends: on the ready timeout, on
    /// UNHEALTHY, and when `onRun` exits.
    check: Option<CheckDriverInputs>,
    check_stop: CancellationToken,
}

/// Everything the `onHealthCheck` driver owns — the second action slot of a
/// Service Session (RFC 0009 "Concurrency with `onRun`").
struct CheckDriverInputs {
    session_id: String,
    service_name: String,
    /// The Session's log tag (`SessionConfig::log_tag`), prefixed to every
    /// process-control line of the driver ahead of the action tag.
    session_tag: Option<String>,
    /// A runner with its own cross-user helper (when the Session is
    /// cross-user) and `action_tag` set to `action_name`, so every log
    /// record of the check is attributed to it (rule 3).
    runner: ScriptRunnerBase,
    /// `onHealthCheck`, or `onWrapServiceHealthCheck` when wrapped.
    action_name: &'static str,
    action: Action,
    symtab: Box<SymbolTable>,
    library: Arc<FunctionLibrary>,
    env_vars: HashMap<String, Option<String>>,
    /// The check's own cancel slot: the invocation in flight is canceled
    /// through it with the action's own cancelation method.
    slot: ActionCancelSlot,
    /// The cancel-pipe writer and auth token of the check's own cross-user
    /// helper, for the slot's cancel handle.
    handle_route: Option<(std::fs::File, String)>,
    /// Parent of each invocation's cancel token (a child of the caller's
    /// external token, when one was given).
    parent_token: CancellationToken,
}

/// What the `onHealthCheck` driver hands back when it stops.
struct CheckDriverOutput {
    /// Values from `openjd_redacted_env` lines on the check's stdout: the
    /// directive is ignored (rule 2) but the value is still redacted from
    /// every later line of the Session.
    redacted_values: Vec<String>,
}

/// One probe request to the `onHealthCheck` driver: the sender the
/// invocation's result is reported on.
type ProbeRequest = tokio::sync::oneshot::Sender<ProbeResult>;

/// The result of one health-check probe, whichever mechanism produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProbeResult {
    /// The probe succeeded.
    Ok,
    /// The probe failed; the text says how, for the log and for
    /// [`ServiceUnhealthy::last_failure`].
    Failed(String),
}

/// What the driver hands back when `onRun` has exited.
struct RunDriverOutput {
    runner: ScriptRunnerBase,
    /// Values from `openjd_redacted_env` lines on `onRun`'s stdout. The
    /// env-var effect is ignored (RFC 0009), but the value is still redacted
    /// from every later line of the Session.
    redacted_values: Vec<String>,
}

/// The current (or most recent) launched `onRun` instance.
struct RunInstance {
    health_rx: watch::Receiver<ServiceHealth>,
    exit_rx: watch::Receiver<Option<ServiceRunExit>>,
    join: Option<tokio::task::JoinHandle<RunDriverOutput>>,
    cancel_requested: Arc<AtomicBool>,
    /// Stops the `onHealthCheck` driver (if any); canceled by the `onRun`
    /// driver itself in normal operation, and by `Drop` if the Service
    /// Session is dropped without `end()`.
    check_stop: CancellationToken,
}

/// A Session that runs one Service (RFC 0009).
///
/// Typical use by a scheduler or single-host runner:
///
/// 1. [`ServiceSession::with_config`] — allocate the working directory.
/// 2. [`ServiceSession::enter`] — enter the scope's `SERVICE` Environments
///    and run `onEnter`. An error is a *start failure*.
/// 3. [`ServiceSession::launch`] — launch `onRun` in the background and start
///    the health check (for `COMMAND`, the concurrent `onHealthCheck`
///    invocations).
/// 4. [`ServiceSession::wait_ready`] — await READY, or an instance failure.
/// 5. Observe the instance with [`ServiceSession::wait_exit`] /
///    [`ServiceSession::exit_watch`] / [`ServiceSession::health_watch`] —
///    the health check keeps probing and, on UNHEALTHY, cancels `onRun`
///    itself; stop it with [`ServiceSession::cancel_run`]; relaunch with
///    [`ServiceSession::launch`] once it has exited (constraint 5).
/// 6. [`ServiceSession::end`] — constraint 7 teardown. Always call it, in
///    every state except `Ended`.
///
/// [`ServiceSession::start`] bundles steps 2–4.
///
/// When the entered Environments include a wrapping Environment
/// (`WRAP_ACTIONS`) whose `runScope` includes `SERVICE`, its
/// `onWrapServiceEnter` / `onWrapServiceRun` / `onWrapServiceHealthCheck`
/// / `onWrapServiceExit` run in place of the Service's actions, each only
/// when the Service defines the corresponding action (RFC 0009
/// `<EnvironmentActions>`). The hooks see `WrappedAction.*` as RFC 0008
/// defines them, plus `WrappedService.Name` / `.PortNames` / `.Ports` /
/// `.BindAddresses` / `.Protocols`.
pub struct ServiceSession {
    session: Session,
    service: Service,
    environments: Vec<Environment>,
    /// See [`ServiceSessionConfig::environment_profiles`].
    environment_profiles: Vec<Option<openjd_model::ModelProfile>>,
    endpoints: ServiceEndpoints,
    in_scope_endpoints: Vec<ServiceEndpoints>,
    state: ServiceSessionState,
    /// Whether any action of the Service (`onEnter` or `onRun`) has run —
    /// constraint 7's condition for running `onExit`.
    any_action_ran: bool,
    /// The Service's resolved symbol table: `Param.*`, `Session.*`,
    /// `Service.*`, `Service.File.*`, `<ServiceScript>.let`. Built once in
    /// [`enter`](Self::enter); constant for the Session's lifetime.
    symtab: Option<Box<SymbolTable>>,
    /// Service `variables`, resolved at service start (normalized keys).
    service_vars: HashMap<String, String>,
    /// `openjd_env` / `openjd_redacted_env` / `openjd_unset_env` changes
    /// from `onEnter`, retained for every later action of the Session.
    on_enter_changes: EnvVarChanges,
    /// Status of the Service's own current/most recent action (`onEnter`,
    /// `onRun`, `onExit`). Environment actions report through
    /// [`Session::action_status`] instead.
    status: Arc<Mutex<ActionStatusFields>>,
    run: Option<RunInstance>,
    /// Number of `onRun` launches so far in this Session.
    launch_count: u32,
    /// The wrapping Environment's embedded files (`Env.File.*` in its
    /// `onWrapService*` hooks), allocated and written once per Service
    /// Session on the first hook that runs and re-registered for every later
    /// hook — never rewritten, so a hook running concurrently with
    /// `onWrapServiceRun` cannot modify a file it was given (RFC 0009
    /// "Concurrency with `onRun`" rule 1).
    wrap_hook_files: Option<EmbeddedFiles>,
}

impl ServiceSession {
    /// Create the Service Session: allocate its working directory (and
    /// cross-user helper, if any) like [`Session::with_config`], and check
    /// that the endpoint assignment covers every declared port.
    ///
    /// # Errors
    ///
    /// Any [`Session::with_config`] error; [`SessionError::ServicePortUnassigned`]
    /// when a declared or probed port has no endpoint; [`SessionError::Runtime`]
    /// when the endpoint assignment names a different Service, an endpoint's
    /// protocol differs from its port's declared `protocol`, a `TCP_CONNECT`
    /// check names a UDP port, or the health check type is `COMMAND` and
    /// the Service defines no `onHealthCheck` (model validation forbids
    /// the last two combinations).
    pub fn with_config(config: ServiceSessionConfig) -> Result<Self, SessionError> {
        let ServiceSessionConfig {
            session,
            service,
            environments,
            environment_profiles,
            mut endpoints,
            in_scope_endpoints,
        } = config;

        if endpoints.name != service.name {
            return Err(SessionError::Runtime(format!(
                "Service '{}' was given the endpoint assignment of Service '{}'",
                service.name, endpoints.name
            )));
        }
        for port in &service.ports {
            let Some((_, endpoint)) = endpoints.ports.iter().find(|(n, _)| *n == port.name) else {
                return Err(SessionError::ServicePortUnassigned {
                    name: service.name.clone(),
                    port: port.name.clone(),
                });
            };
            // The number was allocated in the declared protocol's space
            // (§9.2 item 3); an assignment in the other space is the
            // scheduler's error, not the Service's.
            if endpoint.protocol != port.protocol {
                return Err(SessionError::Runtime(format!(
                    "Service '{}' port '{}' is declared {} but its endpoint assignment is {}",
                    service.name, port.name, port.protocol, endpoint.protocol
                )));
            }
        }
        if let ServiceHealthCheck::TcpConnect { ports, .. } = &service.health_check {
            for p in ports {
                let Some((_, endpoint)) = endpoints.ports.iter().find(|(n, _)| n == p) else {
                    return Err(SessionError::ServicePortUnassigned {
                        name: service.name.clone(),
                        port: p.clone(),
                    });
                };
                // Model validation restricts TCP_CONNECT to TCP ports
                // (§9.3 item 2).
                if endpoint.protocol != ServicePortProtocol::Tcp {
                    return Err(SessionError::Runtime(format!(
                        "Service '{}': TCP_CONNECT health check names port '{p}', whose \
                         protocol is {}; only TCP ports can be probed",
                        service.name, endpoint.protocol
                    )));
                }
            }
        }
        if matches!(service.health_check, ServiceHealthCheck::Command { .. })
            && service.script.actions.on_health_check.is_none()
        {
            return Err(SessionError::Runtime(format!(
                "Service '{}': health check type is COMMAND but onHealthCheck is not defined",
                service.name
            )));
        }
        // `WrappedService.PortNames` / `.Ports` / `.BindAddresses` /
        // `.Protocols` are parallel lists in *declaration* order (Template
        // Schemas §4.3.1): order the assignment by the Service's `ports`
        // once, here.
        let mut ordered = Vec::with_capacity(endpoints.ports.len());
        for port in &service.ports {
            if let Some(i) = endpoints.ports.iter().position(|(n, _)| *n == port.name) {
                ordered.push(endpoints.ports.remove(i));
            }
        }
        ordered.append(&mut endpoints.ports);
        endpoints.ports = ordered;

        let session = Session::with_config(session)?;
        Ok(Self {
            session,
            service,
            environments,
            environment_profiles,
            endpoints,
            in_scope_endpoints,
            state: ServiceSessionState::Created,
            any_action_ran: false,
            symtab: None,
            service_vars: HashMap::new(),
            on_enter_changes: HashMap::new(),
            status: Arc::new(Mutex::new(ActionStatusFields::new())),
            run: None,
            launch_count: 0,
            wrap_hook_files: None,
        })
    }

    // --- Accessors ---

    /// The underlying Session (working directory, entered Environments,
    /// redaction, the status of Environment actions, …).
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// The Service this Session runs.
    pub fn service(&self) -> &Service {
        &self.service
    }

    /// The Service's own endpoint assignment.
    pub fn endpoints(&self) -> &ServiceEndpoints {
        &self.endpoints
    }

    /// The lifecycle phase.
    pub fn state(&self) -> ServiceSessionState {
        self.state
    }

    /// Status of the Service's current or most recent action (`onEnter`,
    /// `onRun`, or `onExit`), or `None` before any has started.
    pub fn action_status(&self) -> Option<ActionStatus> {
        self.lock_status().snapshot()
    }

    /// Number of times `onRun` has been launched in this Session.
    pub fn launch_count(&self) -> u32 {
        self.launch_count
    }

    /// Health of the current/most recent `onRun` instance; `None` before
    /// the first launch.
    pub fn health(&self) -> Option<ServiceHealth> {
        self.run.as_ref().map(|r| r.health_rx.borrow().clone())
    }

    /// How the most recent `onRun` instance exited; `None` before the first
    /// launch or while it is still running.
    pub fn run_exit(&self) -> Option<ServiceRunExit> {
        self.run.as_ref().and_then(|r| r.exit_rx.borrow().clone())
    }

    /// A receiver that tracks the current instance's health: `Pending`,
    /// then `Ready` (re-sent whenever its `failed_probes` count changes) or
    /// one of the readiness failures, and `Unhealthy` once
    /// `failureThreshold` consecutive probes fail after READY. Available
    /// after [`launch`](Self::launch); each launch installs a fresh channel.
    pub fn health_watch(&self) -> Option<watch::Receiver<ServiceHealth>> {
        self.run.as_ref().map(|r| r.health_rx.clone())
    }

    /// A receiver that becomes `Some` when the current instance's `onRun`
    /// exits — the asynchronous exit notification a scheduler selects on.
    /// Available after [`launch`](Self::launch); each launch installs a
    /// fresh channel.
    pub fn exit_watch(&self) -> Option<watch::Receiver<Option<ServiceRunExit>>> {
        self.run.as_ref().map(|r| r.exit_rx.clone())
    }

    /// A thread-safe handle that cancels whichever action of this Session is
    /// running in its main slot — `onEnter`, `onRun`, `onExit`, or an
    /// Environment action — with its own cancelation method. See
    /// [`SessionCancelHandle`]. A concurrent `onHealthCheck` invocation
    /// runs in its own slot and is canceled only by the runtime (when
    /// `onRun` exits, the health check stops probing, or the Session ends).
    pub fn cancel_handle(&self) -> SessionCancelHandle {
        self.session.cancel_handle()
    }

    // --- Lifecycle ---

    /// Enter the scope's Environments whose `runScope` includes `SERVICE`,
    /// in order; resolve the Service's symbol table, embedded files,
    /// `<ServiceScript>.let`, and `variables`; then run `onEnter` if defined.
    ///
    /// # Errors
    ///
    /// Any failure is a *start failure* (RFC 0009 "Failure and restart"):
    /// the state becomes [`ServiceSessionState::StartFailed`] and only
    /// [`end`](Self::end) may follow. An Environment `onEnter` failure
    /// surfaces as [`SessionError::EnvironmentScriptFailed`]; a Service
    /// `onEnter` failure as [`SessionError::ServiceScriptFailed`].
    pub async fn enter(&mut self) -> Result<(), SessionError> {
        self.require_state(&[ServiceSessionState::Created])?;
        match self.enter_inner().await {
            Ok(()) => {
                self.state = ServiceSessionState::Entered;
                Ok(())
            }
            Err(e) => {
                session_log!(
                    error,
                    self.session.session_id(),
                    LogContent::EXCEPTION_INFO,
                    "Service '{}' failed to start: {e}",
                    self.service.name
                );
                self.state = ServiceSessionState::StartFailed;
                Err(e)
            }
        }
    }

    async fn enter_inner(&mut self) -> Result<(), SessionError> {
        let sid = self.session.session_id().to_string();
        self.session
            .log_banner(&format!("Starting Service: {}", self.service.name));

        // RFC 0009 "Services run inside Environments": enter the scope's
        // Environments, in order, skipping those whose runScope excludes
        // SERVICE. The Environments cannot reference Service.* (validation
        // forbids it), so they resolve against the plain Session scope —
        // each with its own document's profile when it has one.
        let environments = std::mem::take(&mut self.environments);
        for (i, env) in environments.iter().enumerate() {
            if !env.runs_in(RunScope::Service) {
                crate::logging::log_session_note_tagged(
                    &sid,
                    self.session.log_tag(),
                    &format!(
                        "Skipping Environment '{}': its runScope does not include SERVICE",
                        env.name
                    ),
                );
                continue;
            }
            let profile = self.environment_profiles.get(i).and_then(Option::as_ref);
            let entered = self
                .session
                .enter_environment_with_profile(
                    env,
                    env.resolved_symtab.as_ref(),
                    None,
                    None,
                    profile,
                )
                .await;
            if let Err(e) = entered {
                self.environments = environments;
                return Err(e);
            }
        }
        self.environments = environments;

        // The Service's symbol table: Param.*/RawParam.*/Job.Name/Step.Name
        // (from its resolved_symtab), Session.WorkingDirectory, the
        // Service.* endpoints in scope (own ports with bindAddress), path
        // mapping (Session.HasPathMappingRules / PathMappingRulesFile),
        // Service.File.*, then <ServiceScript>.let — in the runners' order:
        // allocate file paths → evaluate lets → write file contents.
        let mut symtab = self
            .session
            .build_symbol_table(None, self.service.resolved_symtab.as_ref())?;
        // The Service.* endpoints in scope: the Service's own ports with
        // bindAddress, and the port / connectAddress of every Service it may
        // reference.
        let service_symbols =
            build_service_symbol_table(&self.in_scope_endpoints, Some(&self.endpoints)).map_err(
                |e| SessionError::Runtime(format!("Failed to seed Service.* symbols: {e}")),
            )?;
        symtab.merge_from(&service_symbols);
        self.session.materialize_path_mapping(&mut symtab)?;

        let library = self.session.library_arc();
        let script = &self.service.script;
        let mut embedded = None;
        if let Some(files) = script.embedded_files.as_deref() {
            let mut ef = self.session.embedded_files(EmbeddedFilesScope::Service);
            ef.allocate_file_paths(files, &mut symtab)?;
            embedded = Some(ef);
        }
        if let Some(bindings) = script.let_bindings.as_deref() {
            let limits = self.session.limits();
            symtab = crate::let_bindings::evaluate_let_bindings(
                bindings,
                &symtab,
                Some(&library),
                openjd_expr::PathFormat::host(),
                limits.max_eval_memory_bytes,
                limits.max_eval_operations,
            )
            .map_err(|e| SessionError::FormatString {
                context: "let bindings".into(),
                reason: e.to_string(),
            })?;
        }
        if let Some(ef) = embedded {
            // Written once: a Service Session's format-string values are
            // constant for its lifetime, so the content never changes
            // (RFC 0009 "Concurrency with onRun" rule 1 allows not rewriting
            // an unchanged file, which also keeps a later concurrent action
            // from rewriting a script onRun is still reading).
            ef.write_file_contents(&symtab, Some(&library))?;
        }

        // Service `variables`: resolved once, at service start, with the
        // same checks as an Environment's (§4.4.2).
        let mut service_vars = HashMap::new();
        if let Some(vars) = &self.service.variables {
            for (key, fmt_str) in vars {
                let value =
                    self.session
                        .resolve_env_var_value(key, fmt_str, &symtab, Some(&library))?;
                service_vars.insert(normalize_env_key(key), value);
            }
        }
        self.service_vars = service_vars;
        self.symtab = Some(Box::new(symtab));

        if let Some(on_enter) = self.resolve_action(ServiceActionKind::Enter)? {
            self.any_action_ran = true;
            self.session
                .log_banner(&format!("Service onEnter: {}", self.service.name));
            let result = self.run_foreground_action(&on_enter, None, true).await?;
            if result.state != ActionState::Success {
                let fail_message = self.lock_status().fail_message.clone();
                return Err(SessionError::ServiceScriptFailed {
                    name: self.service.name.clone(),
                    action: "onEnter".into(),
                    reason: failure_reason(&result, fail_message.as_deref()),
                });
            }
        }
        Ok(())
    }

    /// Launch `onRun` in the background and begin the health check.
    /// Returns as soon as the process has been handed to the background
    /// driver; observe it with [`wait_ready`](Self::wait_ready),
    /// [`wait_exit`](Self::wait_exit), and the watch receivers.
    ///
    /// The health check runs in two phases (RFC 0009 `<ServiceHealthCheck>`).
    /// Before READY the first probe runs as soon as `onRun` is launched and
    /// one more `readinessIntervalSeconds` after each failure, until one
    /// succeeds while `onRun` is running (READY), `readinessTimeoutSeconds`
    /// elapses, or `onRun` exits. After READY a probe runs every
    /// `healthIntervalSeconds`; `failureThreshold` consecutive failures make
    /// the instance UNHEALTHY, on which the runtime cancels `onRun` with its
    /// cancelation method (constraint 11) and stops probing; a success
    /// resets the count. For `TCP_CONNECT` and `COMMAND`, an interval is
    /// measured from the end of the previous probe: a `TCP_CONNECT` probe
    /// connects to every listed port; a `COMMAND` probe is one
    /// `onHealthCheck` invocation (sequential, each bounded by the action's
    /// `timeout`, default 30 s), run by the check driver this also starts.
    /// For `STDOUT` the first `openjd_service_ready` line is the readiness
    /// probe; afterwards, only when `healthIntervalSeconds` is given, the
    /// runtime arms a deadline `healthIntervalSeconds` after the later of
    /// READY and the most recent `openjd_service_ready` line — a line before
    /// the deadline is a successful probe and re-arms it, and a deadline
    /// that passes without a line is a failed probe and arms the next.
    ///
    /// Allowed in [`Entered`](ServiceSessionState::Entered) (first launch)
    /// and [`Exited`](ServiceSessionState::Exited) (relaunch within the same
    /// Session: same working directory and ports, `onEnter` not re-run, its
    /// environment variables retained — constraint 5 guarantees the previous
    /// `onRun` has exited).
    ///
    /// # Errors
    ///
    /// [`SessionError::InvalidServiceState`] in any other state. A failure
    /// to build a wrap hook's scope (`WrappedAction.*` resolution, the
    /// wrapping Environment's `let` bindings or embedded files), or to spawn
    /// the health check's cross-user helper, is returned without changing
    /// the state.
    pub async fn launch(&mut self) -> Result<(), SessionError> {
        self.require_state(&[ServiceSessionState::Entered, ServiceSessionState::Exited])?;
        // Reclaim the previous instance's runner (and cross-user helper).
        self.reclaim_run().await;

        let sid = self.session.session_id().to_string();
        let run = self
            .resolve_action(ServiceActionKind::Run)?
            .expect("every Service defines onRun");
        let library = run.library.clone();
        let env_vars = self.service_env_vars();
        let plan = self.health_plan()?;
        let check_stop = CancellationToken::new();
        let check = match &plan.probe {
            ProbePlan::Command { .. } => Some(self.prepare_health_check(&env_vars)?),
            _ => None,
        };

        self.launch_count += 1;
        self.any_action_ran = true;
        self.session.log_banner(&format!(
            "Service onRun: {} (launch {})",
            self.service.name, self.launch_count
        ));
        session_log!(
            info,
            &sid,
            LogContent::PROCESS_CONTROL,
            "{}",
            plan.describe()
        );

        let cancel_token = self.session.action_cancel_token();
        let (cancel_tx, cancel_rx) = watch::channel(None);
        self.session
            .cancel_fields()
            .set_action(cancel_token.clone(), cancel_tx);
        self.session
            .cancel_fields()
            .set_terminate_delay(declared_terminate_delay(
                &run.action.cancelation,
                &run.symtab,
                Some(&library),
                self.session.limits(),
                SERVICE_DEFAULT_NOTIFY_PERIOD,
            ));
        let runner = self.session.new_runner_base(cancel_token, cancel_rx);
        let cancel_handle = self.session.cancel_handle();

        {
            let mut status = self.lock_status();
            status.reset();
        }
        self.notify_callback();

        let (message_tx, message_rx) = mpsc::unbounded_channel();
        let (health_tx, health_rx) = watch::channel(ServiceHealth::Pending);
        let (exit_tx, exit_rx) = watch::channel(None);
        let cancel_requested = Arc::new(AtomicBool::new(false));

        let inputs = RunDriverInputs {
            session_id: sid,
            service_name: self.service.name.clone(),
            session_tag: self.session.log_tag().map(str::to_string),
            runner,
            action_name: run.name,
            action: run.action,
            symtab: run.symtab,
            library,
            env_vars,
            plan,
            status: self.status.clone(),
            callback: self.session.callback_arc(),
            health_tx,
            exit_tx,
            message_tx,
            message_rx,
            cancel_requested: cancel_requested.clone(),
            cancel_handle,
            check,
            check_stop: check_stop.clone(),
        };
        let join = tokio::spawn(drive_run(inputs));
        self.run = Some(RunInstance {
            health_rx,
            exit_rx,
            join: Some(join),
            cancel_requested,
            check_stop,
        });
        self.state = ServiceSessionState::Running;
        Ok(())
    }

    /// Resolve `onHealthCheck` (or `onWrapServiceHealthCheck`) and
    /// build its runner: the second action slot, with its own cancel slot,
    /// its own cross-user helper when the Session is cross-user, and its log
    /// attribution tag.
    fn prepare_health_check(
        &mut self,
        env_vars: &HashMap<String, Option<String>>,
    ) -> Result<CheckDriverInputs, SessionError> {
        let check = self
            .resolve_action(ServiceActionKind::HealthCheck)?
            .ok_or_else(|| {
                SessionError::Runtime(format!(
                    "Service '{}': health check type is COMMAND but onHealthCheck is not defined",
                    self.service.name
                ))
            })?;
        let helper = self.session.spawn_detached_helper()?;
        let parent_token = self.session.action_cancel_token();
        let (_placeholder_tx, placeholder_rx) = watch::channel(None);
        let (mut runner, handle_route) = self.session.new_detached_runner_base(
            parent_token.child_token(),
            placeholder_rx,
            helper,
        );
        runner.action_tag = Some(check.name.to_string());
        Ok(CheckDriverInputs {
            session_id: self.session.session_id().to_string(),
            service_name: self.service.name.clone(),
            session_tag: self.session.log_tag().map(str::to_string),
            runner,
            action_name: check.name,
            action: check.action,
            symtab: check.symtab,
            library: check.library,
            env_vars: env_vars.clone(),
            slot: ActionCancelSlot::new(),
            handle_route,
            parent_token,
        })
    }

    /// [`enter`](Self::enter), [`launch`](Self::launch), then
    /// [`wait_ready`](Self::wait_ready): the whole "start a Service"
    /// sequence of *How Jobs Are Run*. The returned health is either
    /// `Ready` or one of the readiness-failure variants; a start failure is
    /// an `Err`.
    ///
    /// # Errors
    ///
    /// As [`enter`](Self::enter) and [`launch`](Self::launch).
    pub async fn start(&mut self) -> Result<ServiceHealth, SessionError> {
        self.enter().await?;
        self.launch().await?;
        self.wait_ready().await
    }

    /// Wait until the current instance's readiness phase is decided —
    /// READY, or one of the readiness failures — and return that
    /// [`ServiceHealth`]. Returns immediately once it is decided (including
    /// `Unhealthy`, for an instance that was READY and has since failed).
    ///
    /// # Errors
    ///
    /// [`SessionError::InvalidServiceState`] if `onRun` has never been
    /// launched.
    pub async fn wait_ready(&self) -> Result<ServiceHealth, SessionError> {
        let mut rx = self.require_run()?.health_rx.clone();
        loop {
            let current = rx.borrow_and_update().clone();
            if current.is_terminal() {
                return Ok(current);
            }
            if rx.changed().await.is_err() {
                // The driver is gone without a terminal value: it exited.
                return Ok(ServiceHealth::ExitedBeforeReady);
            }
        }
    }

    /// Wait until the current instance's `onRun` has exited and return how.
    /// Returns immediately once it has.
    ///
    /// # Errors
    ///
    /// [`SessionError::InvalidServiceState`] if `onRun` has never been
    /// launched.
    pub async fn wait_exit(&mut self) -> Result<ServiceRunExit, SessionError> {
        let mut rx = self.require_run()?.exit_rx.clone();
        let exit = loop {
            if let Some(exit) = rx.borrow_and_update().clone() {
                break exit;
            }
            if rx.changed().await.is_err() {
                break ServiceRunExit {
                    state: ActionState::Failed,
                    exit_code: None,
                    canceled: false,
                    unhealthy: None,
                    fail_message: Some("onRun driver ended without reporting an exit".into()),
                    stdout: String::new(),
                };
            }
        };
        self.reclaim_run().await;
        Ok(exit)
    }

    /// Cancel the running `onRun` with the action's `cancelation` method
    /// (`NOTIFY_THEN_TERMINATE` grace capped at `time_limit`; `Some(0)`
    /// terminates immediately). The resulting exit reports
    /// `canceled: true`. Returns `false` when no `onRun` is running.
    pub fn cancel_run(&self, time_limit: Option<Duration>) -> bool {
        if self.state != ServiceSessionState::Running {
            return false;
        }
        let Some(run) = self.run.as_ref() else {
            return false;
        };
        run.cancel_requested.store(true, Ordering::SeqCst);
        session_log!(
            info,
            self.session.session_id(),
            LogContent::PROCESS_CONTROL,
            "Canceling Service '{}' onRun",
            self.service.name
        );
        self.session.cancel_handle().cancel(time_limit, false)
    }

    /// End the Service Session per constraint 7: cancel a running `onRun`
    /// and wait for it to exit; run `onExit` (default timeout 300 s) if it
    /// is defined and any action of the Service has run; exit the entered
    /// Environments in reverse order; delete the working directory.
    ///
    /// Every step runs regardless of earlier failures; the first error is
    /// returned once teardown is complete. An `onExit` failure is
    /// [`SessionError::ServiceScriptFailed`] — reported, but per RFC 0009 it
    /// does not change the outcome of the scope.
    ///
    /// # Errors
    ///
    /// [`SessionError::InvalidServiceState`] when already ended; otherwise
    /// the first teardown failure.
    pub async fn end(&mut self) -> Result<(), SessionError> {
        if self.state == ServiceSessionState::Ended {
            return Err(SessionError::InvalidServiceState {
                expected: vec![
                    ServiceSessionState::Created,
                    ServiceSessionState::Entered,
                    ServiceSessionState::Running,
                    ServiceSessionState::Exited,
                    ServiceSessionState::StartFailed,
                ],
                current: self.state,
            });
        }
        self.session
            .log_banner(&format!("Ending Service: {}", self.service.name));
        let mut first_error: Option<SessionError> = None;

        if self.state == ServiceSessionState::Running {
            self.cancel_run(None);
            if let Err(e) = self.wait_exit().await {
                first_error.get_or_insert(e);
            }
        }
        self.reclaim_run().await;

        if self.any_action_ran {
            match self.resolve_action(ServiceActionKind::Exit) {
                Ok(Some(on_exit)) => {
                    self.session
                        .log_banner(&format!("Service onExit: {}", self.service.name));
                    match self
                        .run_foreground_action(&on_exit, Some(SERVICE_EXIT_DEFAULT_TIMEOUT), false)
                        .await
                    {
                        Ok(result) if result.state == ActionState::Success => {}
                        Ok(result) => {
                            let fail_message = self.lock_status().fail_message.clone();
                            first_error.get_or_insert(SessionError::ServiceScriptFailed {
                                name: self.service.name.clone(),
                                action: "onExit".into(),
                                reason: failure_reason(&result, fail_message.as_deref()),
                            });
                        }
                        Err(e) => {
                            first_error.get_or_insert(e);
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }

        // Exit the Environments in reverse (a failed onEnter still counts
        // as entered, exactly as in a Task Session).
        let entered: Vec<_> = self.session.environments_entered().to_vec();
        for id in entered.iter().rev() {
            if let Err(e) = self.session.exit_environment(id, None, false, None).await {
                first_error.get_or_insert(e);
            }
        }

        self.session.cleanup();
        self.state = ServiceSessionState::Ended;
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    // --- Internals ---

    fn require_state(&self, expected: &[ServiceSessionState]) -> Result<(), SessionError> {
        if expected.contains(&self.state) {
            Ok(())
        } else {
            Err(SessionError::InvalidServiceState {
                expected: expected.to_vec(),
                current: self.state,
            })
        }
    }

    fn require_run(&self) -> Result<&RunInstance, SessionError> {
        self.run.as_ref().ok_or(SessionError::InvalidServiceState {
            expected: vec![ServiceSessionState::Running, ServiceSessionState::Exited],
            current: self.state,
        })
    }

    fn lock_status(&self) -> std::sync::MutexGuard<'_, ActionStatusFields> {
        self.status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn notify_callback(&self) {
        if let Some(cb) = self.session.callback_arc() {
            if let Some(status) = self.action_status() {
                cb(self.session.session_id(), &status);
            }
        }
    }

    /// Await the background driver once its `onRun` has exited, restoring
    /// the runner (and cross-user helper) to the Session and recording the
    /// redacted values it collected. No-op when nothing is outstanding.
    async fn reclaim_run(&mut self) {
        let Some(run) = self.run.as_mut() else {
            return;
        };
        let Some(join) = run.join.take() else {
            return;
        };
        match join.await {
            Ok(output) => {
                self.session.restore_runner_base(output.runner);
                self.session.add_redacted_values(output.redacted_values);
            }
            Err(e) => {
                session_log!(
                    error,
                    self.session.session_id(),
                    LogContent::EXCEPTION_INFO,
                    "Service '{}' onRun driver task failed: {e}",
                    self.service.name
                );
            }
        }
        self.session.cancel_fields().reset();
        if self.state == ServiceSessionState::Running {
            self.state = ServiceSessionState::Exited;
        }
    }

    /// The process environment of a Service action: the Session's
    /// cumulative variables (process env, `OPENJD_SESSION_WORKING_DIR`, the
    /// entered Environments' `variables` and `openjd_env` changes), then the
    /// Service's `variables`, then `onEnter`'s changes — lowest to highest
    /// precedence (RFC 0009 "Services run inside Environments").
    fn service_env_vars(&self) -> HashMap<String, Option<String>> {
        let mut env = self.session.evaluate_env_vars(None);
        for (k, v) in &self.service_vars {
            env.insert(k.clone(), Some(v.clone()));
        }
        for (k, v) in &self.on_enter_changes {
            env.insert(k.clone(), v.clone());
        }
        env
    }

    /// Resolve the health check against the endpoint assignment.
    fn health_plan(&self) -> Result<HealthPlan, SessionError> {
        let check = &self.service.health_check;
        let probe = match check {
            ServiceHealthCheck::TcpConnect {
                ports,
                readiness_interval_seconds,
                ..
            } => {
                let mut targets = Vec::with_capacity(ports.len());
                for port_name in ports {
                    let (_, endpoint) = self
                        .endpoints
                        .ports
                        .iter()
                        .find(|(n, _)| n == port_name)
                        .ok_or_else(|| SessionError::ServicePortUnassigned {
                        name: self.service.name.clone(),
                        port: port_name.clone(),
                    })?;
                    targets.push((
                        port_name.clone(),
                        probe_address(&endpoint.bind_address),
                        endpoint.port,
                    ));
                }
                ProbePlan::TcpConnect {
                    targets,
                    readiness_interval: Duration::from_secs(*readiness_interval_seconds),
                }
            }
            ServiceHealthCheck::Stdout { .. } => ProbePlan::Stdout,
            ServiceHealthCheck::Command {
                readiness_interval_seconds,
                ..
            } => ProbePlan::Command {
                readiness_interval: Duration::from_secs(*readiness_interval_seconds),
            },
        };
        Ok(HealthPlan {
            probe,
            readiness_timeout: Duration::from_secs(check.readiness_timeout_seconds()),
            health_interval: check.health_interval_seconds().map(Duration::from_secs),
            failure_threshold: check.failure_threshold(),
        })
    }

    /// The Service action to run for `kind`: `None` when the Service does
    /// not define it (`onEnter`, `onHealthCheck`, `onExit` are optional;
    /// `onRun` always resolves). When the entered stack contains a wrapping
    /// Environment whose `runScope` includes `SERVICE` and which defines the
    /// corresponding `onWrapService*` hook, the hook is returned instead,
    /// with its scope built as for RFC 0008's hooks: the Session's base
    /// scope, the wrapping Environment's `Env.File.*`, its frozen symbol
    /// table and `let` bindings, then `WrappedAction.*` (resolved against
    /// the Service's own scope) and `WrappedService.*`. A hook never runs
    /// for an action the Service does not define ("nothing to replace").
    fn resolve_action(
        &mut self,
        kind: ServiceActionKind,
    ) -> Result<Option<ResolvedAction>, SessionError> {
        let actions = &self.service.script.actions;
        let inner = match kind {
            ServiceActionKind::Enter => actions.on_enter.as_ref(),
            ServiceActionKind::Run => Some(&actions.on_run),
            ServiceActionKind::HealthCheck => actions.on_health_check.as_ref(),
            ServiceActionKind::Exit => actions.on_exit.as_ref(),
        };
        let Some(inner) = inner.cloned() else {
            return Ok(None);
        };
        let inner_symtab = self
            .symtab
            .clone()
            .expect("symtab is built before any Service action runs");

        let hooks = self.session.service_wrap_hooks();
        let hook = hooks.as_ref().and_then(|h| match kind {
            ServiceActionKind::Enter => h.on_enter.clone(),
            ServiceActionKind::Run => h.on_run.clone(),
            ServiceActionKind::HealthCheck => h.on_health_check.clone(),
            ServiceActionKind::Exit => h.on_exit.clone(),
        });
        let (Some(hooks), Some(hook)) = (hooks, hook) else {
            return Ok(Some(ResolvedAction {
                name: kind.name(),
                action: inner,
                symtab: inner_symtab,
                library: self.session.library_arc(),
            }));
        };

        let sid = self.session.session_id().to_string();
        session_log!(
            info,
            &sid,
            LogContent::PROCESS_CONTROL,
            "Service '{}' {}: running {} of wrapping Environment '{}' in its place",
            self.service.name,
            kind.name(),
            kind.hook_name(),
            hooks.scope.name
        );
        let library = self.session.library_arc();
        let mut hook_symtab = self.session.wrap_hook_base_symtab()?;

        // The wrapping Environment's embedded files: paths registered before
        // the seed so its `let` bindings can reference `Env.File.*`; contents
        // written once per Service Session (rule 1 — see `wrap_hook_files`).
        let mut first_use = false;
        match &self.wrap_hook_files {
            Some(cached) => cached.register_file_paths(&mut hook_symtab)?,
            None => {
                if let Some(files) = hooks.embedded_files.as_deref().filter(|f| !f.is_empty()) {
                    let mut ef = self.session.embedded_files(EmbeddedFilesScope::Env);
                    ef.allocate_file_paths(files, &mut hook_symtab)?;
                    self.wrap_hook_files = Some(ef);
                    first_use = true;
                }
            }
        }
        // The Service's own strings (WrappedAction.*) resolve with the
        // Session's library — the Service's document's; the hook and the
        // wrapping Environment's scope with that Environment's document's.
        seed_wrapped_action_symbols(
            &mut hook_symtab,
            &hooks.scope,
            &inner_symtab,
            &inner,
            WrappedContext::Service(&self.endpoints),
            &self.wrapped_env_vars(),
            WrapLibraries {
                inner: Some(&library),
                hook: Some(&hooks.library),
            },
            self.session.limits(),
            &format!("Service {}", kind.name()),
        )?;
        if first_use {
            if let Some(ef) = &self.wrap_hook_files {
                ef.write_file_contents(&hook_symtab, Some(&hooks.library))?;
            }
        }
        Ok(Some(ResolvedAction {
            name: kind.hook_name(),
            action: hook,
            symtab: Box::new(hook_symtab),
            library: hooks.library,
        }))
    }

    /// The session-defined variables a Service's wrapped action would have
    /// run with, for `WrappedAction.Environment`: the entered Environments'
    /// `variables` and `openjd_env` exports (as for every RFC 0008 hook),
    /// then the Service's `variables`, then `onEnter`'s `openjd_env` /
    /// `openjd_unset_env` changes — the same layering as the process
    /// environment of the action itself. Host-inherited variables are
    /// excluded, as RFC 0008 requires.
    fn wrapped_env_vars(&self) -> HashMap<String, String> {
        let mut env = self.session.live_session_env_vars();
        for (k, v) in &self.service_vars {
            env.insert(k.clone(), v.clone());
        }
        for (k, v) in &self.on_enter_changes {
            match v {
                Some(v) => {
                    env.insert(k.clone(), v.clone());
                }
                None => {
                    env.remove(k);
                }
            }
        }
        env
    }

    /// Run `onEnter` or `onExit` (or the hook wrapping it) to completion in
    /// the foreground, processing its `openjd_*` messages as they arrive.
    /// `honor_env_messages` is true for `onEnter` only (RFC 0009
    /// "Environment variables within a Service"); from `onExit` they are
    /// ignored.
    async fn run_foreground_action(
        &mut self,
        resolved: &ResolvedAction,
        default_timeout: Option<Duration>,
        honor_env_messages: bool,
    ) -> Result<SubprocessResult, SessionError> {
        let ResolvedAction {
            name: phase,
            action,
            symtab,
            library,
        } = resolved;
        let env_vars = self.service_env_vars();

        let cancel_token = self.session.action_cancel_token();
        let (cancel_tx, cancel_rx) = watch::channel(None);
        self.session
            .cancel_fields()
            .set_action(cancel_token.clone(), cancel_tx);
        self.session
            .cancel_fields()
            .set_terminate_delay(declared_terminate_delay(
                &action.cancelation,
                symtab,
                Some(library),
                self.session.limits(),
                SERVICE_DEFAULT_NOTIFY_PERIOD,
            ));
        let mut runner = self.session.new_runner_base(cancel_token, cancel_rx);
        self.lock_status().reset();
        self.notify_callback();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let redactions_enabled = self.session.redactions_are_enabled();
        let result = {
            let run_fut = Box::pin(runner.run_action(
                action,
                symtab,
                Some(library),
                &env_vars,
                tx,
                default_timeout,
                SERVICE_DEFAULT_NOTIFY_PERIOD,
            ));
            tokio::pin!(run_fut);
            let mut result = None;
            loop {
                tokio::select! {
                    biased;
                    msg = rx.recv(), if result.is_none() => {
                        if let Some(msg) = msg {
                            self.apply_foreground_message(msg, phase, honor_env_messages, redactions_enabled);
                        }
                    }
                    r = &mut run_fut, if result.is_none() => {
                        result = Some(r);
                    }
                    else => break,
                }
                if result.is_some() {
                    while let Ok(msg) = rx.try_recv() {
                        self.apply_foreground_message(
                            msg,
                            phase,
                            honor_env_messages,
                            redactions_enabled,
                        );
                    }
                    break;
                }
            }
            result.expect("loop guarantees result is Some")
        };
        self.session.restore_runner_base(runner);
        self.session.cancel_fields().reset();

        let result = match result {
            Ok(r) => r,
            Err(e) => {
                session_log!(
                    error,
                    self.session.session_id(),
                    LogContent::EXCEPTION_INFO,
                    "Service '{}' {phase} failed to run: {e}",
                    self.service.name
                );
                {
                    let mut status = self.lock_status();
                    status.fail_message = Some(e.to_string());
                    status.finish(ActionState::Failed, None);
                }
                self.notify_callback();
                return Err(e);
            }
        };
        self.lock_status().finish(result.state, result.exit_code);
        self.notify_callback();
        Ok(result)
    }

    fn apply_foreground_message(
        &mut self,
        msg: ActionMessage,
        phase: &str,
        honor_env_messages: bool,
        redactions_enabled: bool,
    ) {
        let sid = self.session.session_id().to_string();
        match msg {
            ActionMessage::Progress(v) => self.lock_status().progress = Some(v),
            ActionMessage::Status(s) => self.lock_status().status_message = Some(s),
            ActionMessage::Fail(s) => self.lock_status().fail_message = Some(s),
            ActionMessage::SetEnv { name, value } => {
                if honor_env_messages {
                    self.on_enter_changes
                        .insert(normalize_env_key(&name), Some(value));
                } else {
                    log_env_message_ignored(&sid, &self.service.name, phase, "openjd_env");
                }
            }
            ActionMessage::UnsetEnv { name } => {
                if honor_env_messages {
                    self.on_enter_changes.insert(normalize_env_key(&name), None);
                } else {
                    log_env_message_ignored(&sid, &self.service.name, phase, "openjd_unset_env");
                }
            }
            ActionMessage::RedactedEnv { name, value } => {
                if honor_env_messages {
                    if redactions_enabled {
                        self.on_enter_changes
                            .insert(normalize_env_key(&name), Some(value.clone()));
                    }
                } else {
                    log_env_message_ignored(&sid, &self.service.name, phase, "openjd_redacted_env");
                }
                self.session.add_redacted_values([value]);
            }
            ActionMessage::ServiceReady(_) => {
                session_log!(
                    info,
                    &sid,
                    LogContent::PROCESS_CONTROL,
                    "Ignoring openjd_service_ready from Service '{}' {phase}: it is honored only from onRun",
                    self.service.name
                );
            }
            ActionMessage::CancelMarkFailed { fail_message } => {
                if honor_env_messages {
                    self.lock_status().fail_message = Some(fail_message);
                    self.session.cancel_handle().cancel(None, true);
                } else {
                    log_env_message_ignored(
                        &sid,
                        &self.service.name,
                        phase,
                        "malformed openjd env",
                    );
                }
            }
        }
        self.notify_callback();
    }
}

impl Drop for ServiceSession {
    fn drop(&mut self) {
        if self.state != ServiceSessionState::Ended {
            // The inner Session's Drop removes the working directory; the
            // background onRun, if any, is detached and keeps running until
            // its own cancel token (a child of the caller's, if given) fires.
            log::warn!(
                target: "openjd.sessions",
                "ServiceSession for Service '{}' was dropped without calling end(). \
                 A running onRun is not stopped and onExit does not run.",
                self.service.name
            );
            if let Some(run) = self.run.as_ref() {
                // Stop the onHealthCheck driver (it is owned by the onRun
                // driver task, which is aborted next).
                run.check_stop.cancel();
                if let Some(join) = run.join.as_ref() {
                    join.abort();
                }
            }
        }
    }
}

fn format_exit_code(code: Option<i32>) -> String {
    match code {
        Some(c) => format!("exit code: {c}"),
        None => "exit code: N/A".to_string(),
    }
}

/// The `reason` of a [`SessionError::ServiceScriptFailed`] for a finished
/// foreground action. An `openjd_fail` message the action emitted accompanies
/// the reason (RFC 0009 `<ServiceActions>`: "the message accompanies it").
fn failure_reason(result: &SubprocessResult, fail_message: Option<&str>) -> String {
    let base = match result.state {
        ActionState::Canceled => "canceled".to_string(),
        ActionState::Timeout => "timed out".to_string(),
        _ => format_exit_code(result.exit_code),
    };
    match fail_message {
        Some(msg) if !msg.is_empty() => format!("{base}; openjd_fail: {msg}"),
        _ => base,
    }
}

fn log_env_message_ignored(session_id: &str, service: &str, phase: &str, what: &str) {
    session_log!(
        info,
        session_id,
        LogContent::PROCESS_CONTROL,
        "Ignoring {what} from Service '{service}' {phase}: environment variable messages are honored only from onEnter"
    );
}

/// The address a `TCP_CONNECT` probe connects to for a port bound at
/// `bind_address`: the loopback address of the same family when the bind
/// address is a wildcard (`0.0.0.0` / `::`, which cannot be connected to on
/// every operating system), otherwise the bind address itself.
fn probe_address(bind_address: &str) -> String {
    match bind_address.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) if ip.is_unspecified() => "127.0.0.1".to_string(),
        Ok(IpAddr::V6(ip)) if ip.is_unspecified() => "::1".to_string(),
        _ => bind_address.to_string(),
    }
}

/// One `TCP_CONNECT` probe: connect to every target and close immediately.
/// `Ok` iff every connection succeeded; otherwise the first port that could
/// not be connected to, and why.
async fn tcp_probe(targets: &[(String, String, u16)]) -> ProbeResult {
    for (name, address, port) in targets {
        let connect = tokio::net::TcpStream::connect((address.as_str(), *port));
        match tokio::time::timeout(TCP_PROBE_CONNECT_TIMEOUT, connect).await {
            Ok(Ok(_stream)) => {}
            Ok(Err(e)) => {
                return ProbeResult::Failed(format!(
                    "TCP connect to port '{name}' ({address}:{port}) failed: {e}"
                ));
            }
            Err(_) => {
                return ProbeResult::Failed(format!(
                    "TCP connect to port '{name}' ({address}:{port}) timed out after {}s",
                    TCP_PROBE_CONNECT_TIMEOUT.as_secs()
                ));
            }
        }
    }
    ProbeResult::Ok
}

/// The health check's view of one `onRun` instance: which phase it is in
/// and how many consecutive probes have failed in phase 2. Owned by the
/// `onRun` driver, which also applies `onRun`'s `openjd_*` messages through
/// it (the `STDOUT` probe *is* a message).
struct HealthTracker<'a> {
    session_id: &'a str,
    service_name: &'a str,
    /// The Session's log tag, for the per-probe lines.
    tag: LogTag<'a>,
    /// The name of the action running as `onRun` (`onRun`, or
    /// `onWrapServiceRun` when wrapped), for log lines.
    action_name: &'a str,
    /// The health check type, for log lines.
    check_type: &'static str,
    /// Whether `openjd_service_ready` is honored (type `STDOUT`).
    stdout_check: bool,
    failure_threshold: u64,
    status: &'a Arc<Mutex<ActionStatusFields>>,
    callback: Option<&'a SharedCallback>,
    health_tx: &'a watch::Sender<ServiceHealth>,
    phase: HealthPhase,
    /// Values from `openjd_redacted_env` lines; see [`RunDriverOutput`].
    redacted_values: Vec<String>,
}

/// Where the health check stands for the current instance.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HealthPhase {
    /// Phase 1: no probe has succeeded yet.
    Pending,
    /// Phase 2: READY, counting consecutive failures.
    Ready { failed_probes: u64 },
    /// Probing has stopped: the ready timeout elapsed, the instance became
    /// UNHEALTHY, or `onRun` exited.
    Stopped,
}

impl HealthTracker<'_> {
    fn lock_status(&self) -> std::sync::MutexGuard<'_, ActionStatusFields> {
        self.status.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn notify(&self) {
        if let Some(cb) = self.callback {
            if let Some(snapshot) = self.lock_status().snapshot() {
                cb(self.session_id, &snapshot);
            }
        }
    }

    fn is_pending(&self) -> bool {
        self.phase == HealthPhase::Pending
    }

    fn is_ready(&self) -> bool {
        matches!(self.phase, HealthPhase::Ready { .. })
    }

    /// Phase 1 → 2: the first successful probe while `onRun` runs.
    fn set_ready(&mut self, message: Option<String>) {
        self.phase = HealthPhase::Ready { failed_probes: 0 };
        session_log!(
            info,
            self.session_id,
            LogContent::PROCESS_CONTROL,
            "Service '{}' is READY{}",
            self.service_name,
            message
                .as_deref()
                .map(|m| format!(": {m}"))
                .unwrap_or_default()
        );
        let _ = self.health_tx.send(ServiceHealth::Ready {
            message,
            failed_probes: 0,
        });
    }

    /// Phase 1 ended without READY: `readinessTimeoutSeconds` elapsed.
    fn set_timed_out(&mut self, readiness_timeout: Duration) {
        self.phase = HealthPhase::Stopped;
        session_log!(
            error,
            self.session_id,
            LogContent::PROCESS_CONTROL,
            "Service '{}' did not become READY within {}s ({} health check)",
            self.service_name,
            readiness_timeout.as_secs(),
            self.check_type
        );
        let _ = self.health_tx.send(ServiceHealth::TimedOut);
    }

    /// `onRun` exited while phase 1 was undecided.
    fn set_exited_before_ready(&mut self) {
        self.phase = HealthPhase::Stopped;
        session_log!(
            error,
            self.session_id,
            LogContent::PROCESS_CONTROL,
            "Service '{}' {} exited before becoming READY",
            self.service_name,
            self.action_name
        );
        let _ = self.health_tx.send(ServiceHealth::ExitedBeforeReady);
    }

    /// Apply a phase-2 probe result. Returns `Some` when the instance has
    /// just become UNHEALTHY (`failureThreshold` reached); the caller stops
    /// probing and cancels `onRun`.
    fn apply_health_probe(&mut self, result: ProbeResult) -> Option<ServiceUnhealthy> {
        let HealthPhase::Ready { failed_probes } = &mut self.phase else {
            return None;
        };
        match result {
            ProbeResult::Ok => {
                if *failed_probes > 0 {
                    session_tagged_log!(
                        info,
                        self.session_id,
                        self.tag,
                        LogContent::PROCESS_CONTROL,
                        "Service '{}' health probe ok; failure count reset from {}",
                        self.service_name,
                        failed_probes
                    );
                    *failed_probes = 0;
                    self.publish_ready_count();
                } else {
                    // Every passing probe, at DEBUG: a TCP_CONNECT
                    // connection, a COMMAND exit 0, a STDOUT heartbeat line.
                    // A consumer that wants the probe timeline (`openjd run
                    // --verbose`) prints these; the default log does not.
                    session_tagged_log!(
                        debug,
                        self.session_id,
                        self.tag,
                        LogContent::PROCESS_CONTROL,
                        "Service '{}' health probe ok",
                        self.service_name
                    );
                }
                None
            }
            ProbeResult::Failed(detail) => {
                *failed_probes += 1;
                let count = *failed_probes;
                if count >= self.failure_threshold {
                    let unhealthy = ServiceUnhealthy {
                        failed_probes: count,
                        failure_threshold: self.failure_threshold,
                        last_failure: detail,
                    };
                    session_log!(
                        error,
                        self.session_id,
                        LogContent::PROCESS_CONTROL,
                        "Service '{}' is UNHEALTHY: {unhealthy}",
                        self.service_name
                    );
                    self.phase = HealthPhase::Stopped;
                    let _ = self
                        .health_tx
                        .send(ServiceHealth::Unhealthy(unhealthy.clone()));
                    Some(unhealthy)
                } else {
                    session_tagged_log!(
                        warn,
                        self.session_id,
                        self.tag,
                        LogContent::PROCESS_CONTROL,
                        "Service '{}' health probe failed ({count} of {}): {detail}",
                        self.service_name,
                        self.failure_threshold
                    );
                    self.publish_ready_count();
                    None
                }
            }
        }
    }

    /// Re-send `Ready` with the current failure count, keeping the message.
    fn publish_ready_count(&self) {
        let HealthPhase::Ready { failed_probes } = self.phase else {
            return;
        };
        self.health_tx.send_modify(|h| {
            if let ServiceHealth::Ready {
                failed_probes: count,
                ..
            } = h
            {
                *count = failed_probes;
            }
        });
    }

    /// Stop probing because `onRun` exited (constraint 11): a phase-1
    /// instance has failed; a phase-2 instance is simply no longer probed.
    fn stop_for_exit(&mut self) {
        if self.is_pending() {
            self.set_exited_before_ready();
        } else {
            self.phase = HealthPhase::Stopped;
        }
    }

    /// Apply one `onRun` message. `running` is false for messages drained
    /// after `onRun` exited, which cannot make the instance READY or count
    /// as a heartbeat. Returns `true` when the message was an honored
    /// `openjd_service_ready` line (the `STDOUT` probe), so the driver can
    /// reset its heartbeat timer.
    fn apply(&mut self, msg: ActionMessage, running: bool) -> bool {
        let mut heartbeat = false;
        match msg {
            ActionMessage::Progress(v) => self.lock_status().progress = Some(v),
            ActionMessage::Status(s) => self.lock_status().status_message = Some(s),
            ActionMessage::Fail(s) => self.lock_status().fail_message = Some(s),
            ActionMessage::ServiceReady(message) => {
                if !self.stdout_check {
                    session_log!(
                        info,
                        self.session_id,
                        LogContent::PROCESS_CONTROL,
                        "Ignoring openjd_service_ready from Service '{}' {}: its health check type is {}",
                        self.service_name,
                        self.action_name,
                        self.check_type
                    );
                } else if running {
                    if self.is_pending() {
                        self.set_ready(Some(message));
                    } else if self.is_ready() {
                        // After READY the line is a heartbeat (when the
                        // check gives healthIntervalSeconds) and has no
                        // other effect.
                        self.apply_health_probe(ProbeResult::Ok);
                    }
                    heartbeat = true;
                }
            }
            ActionMessage::SetEnv { .. } => {
                log_env_message_ignored(
                    self.session_id,
                    self.service_name,
                    self.action_name,
                    "openjd_env",
                );
            }
            ActionMessage::UnsetEnv { .. } => {
                log_env_message_ignored(
                    self.session_id,
                    self.service_name,
                    self.action_name,
                    "openjd_unset_env",
                );
            }
            ActionMessage::RedactedEnv { value, .. } => {
                log_env_message_ignored(
                    self.session_id,
                    self.service_name,
                    self.action_name,
                    "openjd_redacted_env",
                );
                self.redacted_values.push(value);
            }
            ActionMessage::CancelMarkFailed { .. } => {
                log_env_message_ignored(
                    self.session_id,
                    self.service_name,
                    self.action_name,
                    "malformed openjd env",
                );
            }
        }
        self.notify();
        heartbeat
    }
}

type ProbeFuture = std::pin::Pin<Box<dyn std::future::Future<Output = ProbeResult> + Send>>;

/// Start one probe of the plan's mechanism: a `TCP_CONNECT` round, or one
/// `onHealthCheck` invocation requested from the check driver (whose result
/// is `Failed` if the driver has stopped). Never called for `STDOUT`.
fn start_probe(
    probe: &ProbePlan,
    request_tx: Option<&mpsc::UnboundedSender<ProbeRequest>>,
) -> ProbeFuture {
    match probe {
        ProbePlan::TcpConnect { targets, .. } => {
            let t = targets.clone();
            Box::pin(async move { tcp_probe(&t).await })
        }
        ProbePlan::Command { .. } => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let sent = request_tx.is_some_and(|req| req.send(tx).is_ok());
            Box::pin(async move {
                if !sent {
                    return ProbeResult::Failed("the health check driver has stopped".into());
                }
                rx.await.unwrap_or_else(|_| {
                    ProbeResult::Failed("the health check invocation was canceled".into())
                })
            })
        }
        ProbePlan::Stdout => Box::pin(async {
            ProbeResult::Failed("a STDOUT health check has no probe to run".into())
        }),
    }
}

/// The background driver of one `onRun` instance: runs the action, applies
/// its `openjd_*` messages, runs the health check against it in both
/// phases (spawning and stopping the `onHealthCheck` driver for a `COMMAND`
/// check), cancels `onRun` on UNHEALTHY, and publishes health and exit.
async fn drive_run(inputs: RunDriverInputs) -> RunDriverOutput {
    let RunDriverInputs {
        session_id,
        service_name,
        session_tag,
        mut runner,
        action_name,
        action,
        symtab,
        library,
        env_vars,
        plan,
        status,
        callback,
        health_tx,
        exit_tx,
        message_tx,
        mut message_rx,
        cancel_requested,
        cancel_handle,
        check,
        check_stop,
    } = inputs;
    let lock_status = || status.lock().unwrap_or_else(|p| p.into_inner());
    let notify = || {
        if let Some(cb) = &callback {
            if let Some(snapshot) = lock_status().snapshot() {
                cb(&session_id, &snapshot);
            }
        }
    };

    // The ready timeout is measured from launch (RFC 0009 §9.3 item 4)
    // and runs continuously, including while a probe is in progress.
    let deadline = tokio::time::sleep(plan.readiness_timeout);
    tokio::pin!(deadline);
    let is_stdout = matches!(plan.probe, ProbePlan::Stdout);

    // COMMAND: the onHealthCheck driver runs in its own task and performs
    // one invocation per request on `request_tx`, reporting the result on
    // the request's oneshot. It stops when `check_stop` fires (probing
    // ended, or onRun exited — rules 4 and 5).
    let (request_tx, request_rx) = mpsc::unbounded_channel::<ProbeRequest>();
    let check_join = check.map(|inputs| {
        let stop = check_stop.clone();
        tokio::spawn(drive_health_check(inputs, stop, request_rx))
    });
    let request_tx = check_join.is_some().then_some(&request_tx);

    let tag = LogTag {
        session: session_tag.as_deref(),
        action: None,
    };
    let mut tracker = HealthTracker {
        session_id: &session_id,
        service_name: &service_name,
        tag,
        action_name,
        check_type: plan.type_name(),
        stdout_check: is_stdout,
        failure_threshold: plan.failure_threshold,
        status: &status,
        callback: callback.as_ref(),
        health_tx: &health_tx,
        phase: HealthPhase::Pending,
        redacted_values: Vec::new(),
    };

    // The probe schedule. For TCP_CONNECT and COMMAND: `probe` is the
    // round in flight, `next_probe` the pause before the next one (the
    // first probe runs as soon as onRun is launched, i.e. now; each later
    // one an interval after the previous ends). For STDOUT after READY with
    // healthIntervalSeconds: `heartbeat` is the deadline armed
    // healthIntervalSeconds after the later of READY and the most recent
    // openjd_service_ready line — a line before it is a successful probe
    // and re-arms it, and a deadline that passes without a line is a failed
    // probe and arms the next (RFC 0009 `<ServiceHealthCheck>`).
    let mut probe: Option<ProbeFuture> = (!is_stdout).then(|| start_probe(&plan.probe, request_tx));
    let mut next_probe: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = None;
    let mut heartbeat: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = None;
    let mut unhealthy: Option<ServiceUnhealthy> = None;

    let result = {
        let run_fut = runner.run_action(
            &action,
            &symtab,
            Some(&library),
            &env_vars,
            message_tx,
            None,
            SERVICE_DEFAULT_NOTIFY_PERIOD,
        );
        tokio::pin!(run_fut);
        let mut result = None;
        loop {
            tokio::select! {
                biased;
                msg = message_rx.recv(), if result.is_none() => {
                    let Some(msg) = msg else { continue };
                    let was_pending = tracker.is_pending();
                    if tracker.apply(msg, true) {
                        // A STDOUT probe: READY on the first line; a
                        // heartbeat afterwards. Either arms the deadline
                        // anew when the check gives an interval.
                        if let Some(interval) = plan.health_interval {
                            if tracker.is_ready() {
                                heartbeat = Some(Box::pin(tokio::time::sleep(interval)));
                            }
                        } else if was_pending {
                            // No heartbeat: the instance is not monitored
                            // after READY (its health is that onRun runs).
                            tracker.phase = HealthPhase::Stopped;
                        }
                    }
                }
                r = &mut run_fut, if result.is_none() => {
                    result = Some(r);
                }
                outcome = async { probe.as_mut().expect("guarded").await }, if probe.is_some() && result.is_none() => {
                    probe = None;
                    if tracker.is_pending() {
                        match outcome {
                            ProbeResult::Ok => {
                                tracker.set_ready(None);
                                // Phase 2 begins: the next probe is a health
                                // probe, healthIntervalSeconds from now.
                                match plan.health_interval {
                                    Some(i) => next_probe = Some(Box::pin(tokio::time::sleep(i))),
                                    None => tracker.phase = HealthPhase::Stopped,
                                }
                            }
                            ProbeResult::Failed(detail) => {
                                session_tagged_log!(
                                    info,
                                    &session_id,
                                    tag,
                                    LogContent::PROCESS_CONTROL,
                                    "Service '{}' is not yet READY: {detail}",
                                    service_name
                                );
                                let i = plan.readiness_interval().expect("TCP_CONNECT or COMMAND");
                                next_probe = Some(Box::pin(tokio::time::sleep(i)));
                            }
                        }
                    } else if tracker.is_ready() {
                        match tracker.apply_health_probe(outcome) {
                            Some(u) => {
                                // Constraint 11: an UNHEALTHY instance is
                                // stopped as at scope end — onRun canceled
                                // with its cancelation method; probing ends.
                                unhealthy = Some(u);
                                check_stop.cancel();
                                session_log!(
                                    info,
                                    &session_id,
                                    LogContent::PROCESS_CONTROL,
                                    "Canceling Service '{}' {action_name}: the instance is UNHEALTHY",
                                    service_name
                                );
                                cancel_handle.cancel(None, false);
                            }
                            None => {
                                let i = plan.health_interval.expect("phase 2 implies an interval");
                                next_probe = Some(Box::pin(tokio::time::sleep(i)));
                            }
                        }
                    }
                    // Stopped: a result that raced the stop is discarded.
                }
                () = async { next_probe.as_mut().expect("guarded").await }, if next_probe.is_some() && result.is_none() => {
                    next_probe = None;
                    if tracker.phase != HealthPhase::Stopped {
                        probe = Some(start_probe(&plan.probe, request_tx));
                    }
                }
                () = async { heartbeat.as_mut().expect("guarded").await }, if heartbeat.is_some() && result.is_none() => {
                    heartbeat = None;
                    if tracker.is_ready() {
                        let interval = plan.health_interval.expect("a heartbeat implies an interval");
                        let missed = ProbeResult::Failed(format!(
                            "no openjd_service_ready line within {}s",
                            interval.as_secs()
                        ));
                        match tracker.apply_health_probe(missed) {
                            Some(u) => {
                                unhealthy = Some(u);
                                session_log!(
                                    info,
                                    &session_id,
                                    LogContent::PROCESS_CONTROL,
                                    "Canceling Service '{}' {action_name}: the instance is UNHEALTHY",
                                    service_name
                                );
                                cancel_handle.cancel(None, false);
                            }
                            None => heartbeat = Some(Box::pin(tokio::time::sleep(interval))),
                        }
                    }
                }
                _ = &mut deadline, if tracker.is_pending() && result.is_none() => {
                    tracker.set_timed_out(plan.readiness_timeout);
                    // An invocation in flight is canceled: the decision is
                    // terminal and the check never runs again.
                    probe = None;
                    next_probe = None;
                    check_stop.cancel();
                }
                else => break,
            }
            if result.is_some() {
                // "onRun exit wins" (rule 5, constraint 11): the check is
                // stopped — an invocation in flight is canceled with its own
                // cancelation method and its result discarded — and messages
                // that raced the exit are still applied, but neither a
                // readiness line nor a probe success can make the instance
                // READY or keep it READY now.
                check_stop.cancel();
                drop(probe.take());
                drop(next_probe.take());
                drop(heartbeat.take());
                while let Ok(msg) = message_rx.try_recv() {
                    tracker.apply(msg, false);
                }
                break;
            }
        }
        result.expect("loop guarantees result is Some")
    };

    let exit = match result {
        Ok(r) => {
            let canceled = cancel_requested.load(Ordering::SeqCst)
                || (r.state == ActionState::Canceled && unhealthy.is_none());
            let fail_message = lock_status().fail_message.clone();
            ServiceRunExit {
                state: r.state,
                exit_code: r.exit_code,
                canceled,
                unhealthy: unhealthy.clone(),
                fail_message,
                stdout: r.stdout,
            }
        }
        Err(e) => {
            session_log!(
                error,
                &session_id,
                LogContent::EXCEPTION_INFO,
                "Service '{}' {action_name} failed to run: {e}",
                service_name
            );
            lock_status().fail_message = Some(e.to_string());
            ServiceRunExit {
                state: ActionState::Failed,
                exit_code: None,
                canceled: cancel_requested.load(Ordering::SeqCst),
                unhealthy: unhealthy.clone(),
                fail_message: Some(e.to_string()),
                stdout: String::new(),
            }
        }
    };
    tracker.stop_for_exit();
    let HealthTracker {
        mut redacted_values,
        ..
    } = tracker;

    // The check driver stops before the exit is published (and so before
    // `end()` can run onExit — rule 5): await it here.
    if let Some(join) = check_join {
        match join.await {
            Ok(out) => redacted_values.extend(out.redacted_values),
            Err(e) => session_log!(
                error,
                &session_id,
                LogContent::EXCEPTION_INFO,
                "Service '{}' onHealthCheck driver task failed: {e}",
                service_name
            ),
        }
    }

    session_log!(
        info,
        &session_id,
        LogContent::PROCESS_CONTROL,
        "Service '{}' {action_name} exited: {} ({}){}",
        service_name,
        exit.state,
        format_exit_code(exit.exit_code),
        if exit.unhealthy.is_some() {
            ", canceled by the runtime: UNHEALTHY"
        } else if exit.canceled {
            ", canceled by the runtime"
        } else {
            ""
        }
    );
    lock_status().finish(exit.state, exit.exit_code);
    notify();
    let _ = exit_tx.send(Some(exit));

    RunDriverOutput {
        runner,
        redacted_values,
    }
}

/// The driver of the `onHealthCheck` invocations of one `onRun` instance
/// (RFC 0009 `<ServiceHealthCheck>` `COMMAND`, "Concurrency with `onRun`"):
/// one invocation per request on `request_rx`, each bounded by the action's
/// `timeout` (default 30 s, canceled on overrun, a failed probe), its result
/// — exit 0 is a successful probe, anything else a failed one — reported on
/// the request's channel. Invocations are sequential by construction: the
/// `onRun` driver requests the next only after receiving the previous
/// result and waiting the phase's interval. The driver returns when `stop`
/// fires; an invocation in flight then is canceled through the check's own
/// cancel slot with its own cancelation method, and its result is discarded.
/// No `openjd_*` message on the check's stdout is honored (rule 2): each is
/// logged and ignored. Every log record of the check is tagged with the
/// action name (rule 3) through the runner's `action_tag`.
async fn drive_health_check(
    inputs: CheckDriverInputs,
    stop: CancellationToken,
    mut request_rx: mpsc::UnboundedReceiver<ProbeRequest>,
) -> CheckDriverOutput {
    let CheckDriverInputs {
        session_id,
        service_name,
        session_tag,
        mut runner,
        action_name,
        action,
        symtab,
        library,
        env_vars,
        slot,
        handle_route,
        parent_token,
    } = inputs;
    let tag = LogTag {
        session: session_tag.as_deref(),
        action: Some(action_name),
    };
    let (route_writer, route_token) = match handle_route {
        Some((w, t)) => (Some(w), Some(t)),
        None => (None, None),
    };
    let mut redacted_values = Vec::new();
    let mut invocation: u32 = 0;

    loop {
        let reply = tokio::select! {
            biased;
            _ = stop.cancelled() => break,
            req = request_rx.recv() => match req {
                Some(reply) => reply,
                None => break,
            },
        };
        invocation += 1;
        session_tagged_log!(
            info,
            &session_id,
            tag,
            LogContent::PROCESS_CONTROL,
            "Service '{service_name}' health check invocation {invocation}"
        );

        // Each invocation gets a fresh cancel token in the check's own slot.
        let token = parent_token.child_token();
        let (cancel_tx, cancel_rx) = watch::channel(None);
        slot.set_action(token.clone(), cancel_tx);
        slot.set_terminate_delay(declared_terminate_delay(
            &action.cancelation,
            &symtab,
            Some(&library),
            &runner.limits,
            SERVICE_DEFAULT_NOTIFY_PERIOD,
        ));
        runner.cancel_token = token;
        runner.cancel_request_rx = Some(cancel_rx);
        let handle = slot.handle(
            route_writer.as_ref().and_then(|w| w.try_clone().ok()),
            route_token.clone(),
        );

        let (message_tx, mut message_rx) = mpsc::unbounded_channel();
        let result = {
            let run_fut = runner.run_action(
                &action,
                &symtab,
                Some(&library),
                &env_vars,
                message_tx,
                Some(SERVICE_HEALTH_CHECK_DEFAULT_TIMEOUT),
                SERVICE_DEFAULT_NOTIFY_PERIOD,
            );
            tokio::pin!(run_fut);
            let mut result = None;
            let mut cancel_sent = false;
            loop {
                tokio::select! {
                    biased;
                    msg = message_rx.recv(), if result.is_none() => {
                        let Some(msg) = msg else { continue };
                        log_check_message_ignored(&session_id, &service_name, tag, &msg, &mut redacted_values);
                    }
                    _ = stop.cancelled(), if !cancel_sent && result.is_none() => {
                        cancel_sent = true;
                        session_tagged_log!(
                            info,
                            &session_id,
                            tag,
                            LogContent::PROCESS_CONTROL,
                            "Canceling health check invocation {invocation}: its result will be discarded"
                        );
                        handle.cancel(None, false);
                    }
                    r = &mut run_fut, if result.is_none() => {
                        result = Some(r);
                    }
                    else => break,
                }
                if result.is_some() {
                    while let Ok(msg) = message_rx.try_recv() {
                        log_check_message_ignored(
                            &session_id,
                            &service_name,
                            tag,
                            &msg,
                            &mut redacted_values,
                        );
                    }
                    break;
                }
            }
            result.expect("loop guarantees result is Some")
        };
        slot.reset();

        if stop.is_cancelled() {
            // Rule 5 / constraint 11: canceled by the runtime (onRun exited,
            // probing ended, or the Session is ending). The result is
            // discarded; the dropped `reply` tells the requester so.
            break;
        }
        let outcome = match result {
            Ok(r) if r.state == ActionState::Timeout => {
                ProbeResult::Failed(format!("{action_name} exceeded its timeout"))
            }
            Ok(r) if r.state != ActionState::Canceled && r.exit_code == Some(0) => {
                // Its result is its exit status (rule 2): exit 0 is a
                // successful probe even if the output carried an
                // openjd_fail line.
                ProbeResult::Ok
            }
            Ok(r) => {
                ProbeResult::Failed(format!("{action_name} {}", format_exit_code(r.exit_code)))
            }
            Err(e) => ProbeResult::Failed(format!("{action_name} failed to run: {e}")),
        };
        session_tagged_log!(
            info,
            &session_id,
            tag,
            LogContent::PROCESS_CONTROL,
            "Health check invocation {invocation}: {}",
            match &outcome {
                ProbeResult::Ok => "succeeded (exit code: 0)".to_string(),
                ProbeResult::Failed(detail) => format!("failed ({detail})"),
            }
        );
        // The requester may have moved on (probing stopped); nothing to do
        // then.
        let _ = reply.send(outcome);
    }

    // The check's own cross-user helper (if any) is done: shut it down
    // cleanly rather than leaving it to `Drop`'s kill.
    if let Some(helper) = runner.helper.as_mut() {
        helper.shutdown();
    }
    CheckDriverOutput { redacted_values }
}

/// Rule 2: log one line for an `openjd_*` message on `onHealthCheck`'s
/// stdout and ignore it. The value of an `openjd_redacted_env` is still
/// collected for redaction — the directive's effect is ignored, not its
/// secrecy.
fn log_check_message_ignored(
    session_id: &str,
    service_name: &str,
    tag: LogTag<'_>,
    msg: &ActionMessage,
    redacted_values: &mut Vec<String>,
) {
    let action_name = tag.action.unwrap_or("onHealthCheck");
    let what = match msg {
        ActionMessage::Progress(_) => "openjd_progress",
        ActionMessage::Status(_) => "openjd_status",
        ActionMessage::Fail(_) => "openjd_fail",
        ActionMessage::SetEnv { .. } => "openjd_env",
        ActionMessage::UnsetEnv { .. } => "openjd_unset_env",
        ActionMessage::RedactedEnv { value, .. } => {
            redacted_values.push(value.clone());
            "openjd_redacted_env"
        }
        ActionMessage::ServiceReady(_) => "openjd_service_ready",
        ActionMessage::CancelMarkFailed { .. } => "malformed openjd env",
    };
    session_tagged_log!(
        info,
        session_id,
        tag,
        LogContent::PROCESS_CONTROL,
        "Ignoring {what} from Service '{service_name}' {action_name}: messages on the health check's stdout are not honored"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_address_maps_wildcards_to_loopback() {
        assert_eq!(probe_address("0.0.0.0"), "127.0.0.1");
        assert_eq!(probe_address("::"), "::1");
        assert_eq!(probe_address("127.0.0.1"), "127.0.0.1");
        assert_eq!(probe_address("::1"), "::1");
        assert_eq!(probe_address("10.1.2.3"), "10.1.2.3");
        assert_eq!(
            probe_address("svc.example.internal"),
            "svc.example.internal"
        );
    }

    #[test]
    fn health_predicates() {
        let ready = ServiceHealth::Ready {
            message: None,
            failed_probes: 0,
        };
        assert!(!ServiceHealth::Pending.is_terminal());
        assert!(ready.is_terminal());
        assert!(ready.is_ready());
        assert!(ServiceHealth::TimedOut.is_terminal());
        assert!(!ServiceHealth::TimedOut.is_ready());
        assert!(ServiceHealth::ExitedBeforeReady.is_terminal());
        let unhealthy = ServiceHealth::Unhealthy(ServiceUnhealthy {
            failed_probes: 3,
            failure_threshold: 3,
            last_failure: "x".into(),
        });
        assert!(unhealthy.is_terminal());
        assert!(!unhealthy.is_ready());
    }

    #[test]
    fn health_plan_names_intervals_and_description() {
        let cmd = HealthPlan {
            probe: ProbePlan::Command {
                readiness_interval: Duration::from_secs(5),
            },
            readiness_timeout: Duration::from_secs(7),
            health_interval: Some(Duration::from_secs(30)),
            failure_threshold: 3,
        };
        assert_eq!(cmd.type_name(), "COMMAND");
        assert_eq!(cmd.readiness_interval(), Some(Duration::from_secs(5)));
        assert_eq!(
            cmd.describe(),
            "Health check: COMMAND (readinessTimeoutSeconds 7, readinessIntervalSeconds 5, \
             healthIntervalSeconds 30, failureThreshold 3)"
        );
        let out = HealthPlan {
            probe: ProbePlan::Stdout,
            readiness_timeout: Duration::from_secs(9),
            health_interval: None,
            failure_threshold: 3,
        };
        assert_eq!(out.type_name(), "STDOUT");
        assert_eq!(out.readiness_interval(), None);
        assert_eq!(
            out.describe(),
            "Health check: STDOUT (readinessTimeoutSeconds 9, no heartbeat after READY)"
        );
        let beat = HealthPlan {
            health_interval: Some(Duration::from_secs(15)),
            failure_threshold: 2,
            ..out
        };
        assert_eq!(
            beat.describe(),
            "Health check: STDOUT (readinessTimeoutSeconds 9, healthIntervalSeconds 15, \
             failureThreshold 2)"
        );
        let tcp = HealthPlan {
            probe: ProbePlan::TcpConnect {
                targets: vec![],
                readiness_interval: Duration::from_secs(1),
            },
            readiness_timeout: Duration::from_secs(11),
            health_interval: Some(Duration::from_secs(30)),
            failure_threshold: 3,
        };
        assert_eq!(tcp.type_name(), "TCP_CONNECT");
        assert_eq!(tcp.readiness_interval(), Some(Duration::from_secs(1)));
    }

    #[test]
    fn unhealthy_display() {
        let u = ServiceUnhealthy {
            failed_probes: 3,
            failure_threshold: 3,
            last_failure: "TCP connect to port 'main' (127.0.0.1:1) failed: refused".into(),
        };
        assert_eq!(
            u.to_string(),
            "3 consecutive health probes failed (failureThreshold: 3); last probe: TCP connect \
             to port 'main' (127.0.0.1:1) failed: refused"
        );
    }

    #[test]
    fn state_display() {
        assert_eq!(ServiceSessionState::Created.to_string(), "CREATED");
        assert_eq!(ServiceSessionState::Entered.to_string(), "ENTERED");
        assert_eq!(ServiceSessionState::Running.to_string(), "RUNNING");
        assert_eq!(ServiceSessionState::Exited.to_string(), "EXITED");
        assert_eq!(ServiceSessionState::StartFailed.to_string(), "START_FAILED");
        assert_eq!(ServiceSessionState::Ended.to_string(), "ENDED");
    }

    #[tokio::test]
    async fn tcp_probe_succeeds_only_when_all_listen() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let open = ("a".to_string(), "127.0.0.1".to_string(), port);
        assert_eq!(
            tcp_probe(std::slice::from_ref(&open)).await,
            ProbeResult::Ok
        );
        // A second, closed port fails the round and is named.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed_port = closed.local_addr().unwrap().port();
        drop(closed);
        match tcp_probe(&[
            open,
            ("b".to_string(), "127.0.0.1".to_string(), closed_port),
        ])
        .await
        {
            ProbeResult::Failed(detail) => assert!(
                detail.starts_with(&format!(
                    "TCP connect to port 'b' (127.0.0.1:{closed_port}) failed: "
                )),
                "{detail}"
            ),
            ProbeResult::Ok => panic!("closed port probed ok"),
        }
    }
}
