// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Service orchestration for `openjd run` (RFC 0009 "Modifications to How
//! Jobs Are Run", wiki *How Jobs Are Run* § Services).
//!
//! `openjd run` is both the scheduler and the only host, so this module
//! takes the scheduler's side of the split that
//! `specs/sessions/service-session.md` describes: it allocates endpoints
//! ([`super::service_ports`]), decides when each Service Session starts
//! (ordering constraints 2 and 9: after every entry of the Service's
//! `dependencies` is satisfied — its Steps completed, its Services READY),
//! gates Tasks on readiness (constraint 3), watches for instance failures
//! while Tasks run — an `onRun` exit, or an UNHEALTHY verdict from the
//! Session's health check — and applies the restart policy ("Failure and
//! restart"), and stops Services when their scope completes (constraints 4,
//! 6, 7). The Service Sessions themselves are
//! `openjd_sessions::ServiceSession`s.
//!
//! Every Service of the combined Job is registered once, with the scope job
//! creation computed for it from the template's `dependencies` lists
//! ([`Service::scope`], Template Schemas §9.1): a Service whose scope is
//! every Step is started before the Task Session enters the Job's
//! Environments and stopped at the end of the run; one scoped to some Steps
//! is *activated* when the first of them is about to run — after every Step
//! in its `dependencies` has completed — and stopped once no Step in its
//! scope remains to run. Steps outside a Service's scope never wait on it. A
//! Service stopped because its scope completed returns to idle and is
//! started again, in a new Service Session, if a `RERUN` returns a Step of
//! its scope to the queue.
//!
//! Services are keyed by [`ServiceKey`] — the document that declares a
//! Service plus its name (Template Schemas §1.2.2 item 3: Service names are
//! per document, so an inline Service and an attached one may share a name,
//! and two attachments may both declare a `Cache`). A Service Session is seeded with the endpoints of the
//! Services it lists with the `service` key in its `dependencies` — of its
//! own document, or the attached Service bound to the Job Template's
//! `externalServices` entry of that name for an inline Service — and a Task
//! Session with the inline Services whose scope includes its Step and the
//! bound required Services.
//!
//! When a Service begins a new Service Session (RFC 0009 "Dependents of a
//! relaunched Service" — locally after a start failure or an `onRun` exit
//! before READY, which may be a port conflict), every Service that lists
//! it, directly or transitively, holds endpoint values that are no longer
//! valid: each is stopped, dependents before the Services they list
//! (constraint 4), and started again in a new Service Session once every
//! Service it lists is READY. The stop consumes none of the dependent's
//! relaunch attempts, and its own `completedTasks` applies to its scope —
//! the completed Tasks of its scope are returned under `RERUN` and kept
//! under `KEEP`; a dependent that gives none (allowed only with
//! `maxRestarts` 0) is treated as `RERUN`. A relaunch within the same
//! Service Session keeps the same ports and requires nothing of
//! dependents.
//!
//! See `specs/cli/run.md` § Services for the orchestration rules.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use openjd_expr::SerializedSymbolTable;
use openjd_model::job::service_symbols::{build_service_symbol_table, ServiceEndpoints};
use openjd_model::job::{
    CompletedTasksPolicy, Document, Environment, EnvironmentParameterValues, Job, Service,
    ServiceScope,
};
use openjd_model::types::{JobParameterValues, ModelProfile};
use openjd_model::RequirementBinding;
use openjd_sessions::path_mapping::PathMappingRule;
use openjd_sessions::session::SessionConfig;
use openjd_sessions::{
    ServiceHealth, ServiceRunExit, ServiceSession, ServiceSessionConfig, ServiceSessionState,
    ServiceUnhealthy, SessionLimits,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::service_ports::{describe_endpoints, PortAllocator};
use super::RunError;

/// What identifies one Service of the combined Job: the document that
/// declares it and its name (Template Schemas §1.2.2 item 3). Two Services
/// with the same name from different documents are different Services.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ServiceKey {
    pub document: Document,
    pub name: String,
}

impl ServiceKey {
    pub(super) fn of(service: &Service) -> Self {
        Self {
            document: service.document.clone(),
            name: service.name.clone(),
        }
    }

    /// The Service `name` names from inside a Service of `document`: a
    /// `service: <name>` dependency on an inline Service never crosses a
    /// document boundary.
    fn sibling(document: &Document, name: &str) -> Self {
        Self {
            document: document.clone(),
            name: name.to_string(),
        }
    }

    /// The attached Service a requirement was bound to.
    fn of_binding(binding: &RequirementBinding) -> Self {
        Self {
            document: binding.document.clone(),
            name: binding.service.clone(),
        }
    }

    /// `(from <doc>)` for an external Service, nothing for the Job
    /// Template's own — what disambiguates same-named Services in the log.
    pub(super) fn origin_suffix(&self) -> String {
        origin_suffix(&self.document)
    }
}

/// Which `Service.*` values a Task Session action may see (RFC 0009 "The
/// `Service.*` scope"): the entity's kind decides the rule
/// [`ServiceManager::task_scope_endpoints`] applies.
#[derive(Clone, Copy)]
pub(super) enum Visibility<'a> {
    /// A Task of the named Step, or one of its Step Environments, which
    /// follow the Step's `dependencies`: the Services whose scope includes
    /// the Step, plus every bound requirement.
    Step(&'a str),
    /// A Job Environment — the Job Template's own or an attached one —
    /// which has a `dependencies` list of its own: exactly the Services it
    /// lists.
    Environment(&'a Environment),
}

/// `Service 'X'`, or `Service 'X' (from <doc>)` for an external Service.
impl std::fmt::Display for ServiceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Service '{}'{}", self.name, self.origin_suffix())
    }
}

/// `" (from <doc>)"` when `document` is an attached Environment Template,
/// else empty.
pub(super) fn origin_suffix(document: &Document) -> String {
    if document.is_job_template() {
        String::new()
    } else {
        format!(" (from {document})")
    }
}

/// `(scope: every Step)` / `(scope: Steps A, B)` — how every log line names
/// a Service's scope.
pub(super) fn scope_label(scope: &ServiceScope) -> String {
    format!("(scope: {scope})")
}

/// A Service that became FAILED: its scope fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ServiceFailure {
    pub name: String,
    /// The document declaring the Service; `JobTemplate` for the Job
    /// Template's own.
    pub document: Document,
    pub scope: ServiceScope,
    pub reason: String,
}

impl ServiceFailure {
    fn key(&self) -> ServiceKey {
        ServiceKey {
            document: self.document.clone(),
            name: self.name.clone(),
        }
    }
}

impl std::fmt::Display for ServiceFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} failed: {}",
            self.key(),
            scope_label(&self.scope),
            self.reason
        )
    }
}

/// The union of two scopes: the Steps whose completed Tasks a set of `RERUN`
/// relaunches return to the queue.
pub(super) fn merge_scopes(a: Option<ServiceScope>, b: &ServiceScope) -> ServiceScope {
    match (a, b) {
        (None, b) => b.clone(),
        (Some(ServiceScope::AllSteps), _) | (_, ServiceScope::AllSteps) => ServiceScope::AllSteps,
        (Some(ServiceScope::Steps { steps: mut a }), ServiceScope::Steps { steps: b }) => {
            a.extend(b.iter().cloned());
            ServiceScope::Steps { steps: a }
        }
    }
}

/// The run-wide inputs every Service Session shares with the Task Session.
pub(super) struct ServiceRunConfig {
    pub job_parameter_values: JobParameterValues,
    pub path_mapping_rules: Option<Vec<PathMappingRule>>,
    pub retain_working_dir: bool,
    /// The Job Template's profile: the profile of its `services` and of its
    /// own Environments.
    pub profile: ModelProfile,
    /// The `--environment` templates' profiles, by attachment index. An
    /// external Service's Session runs under its own template's profile,
    /// and every Environment a Service Session enters is evaluated under
    /// the profile of the document that declares it (Template Schemas §1.2
    /// item 3).
    pub attached_profiles: Vec<ModelProfile>,
    /// The attached Service each `externalServices` entry of the Job
    /// Template was bound to at submission (§1.2.2 item 2). The Job
    /// Template's Tasks, Environments and inline Services see
    /// `Service.<requirement>.*` as that Service's endpoints.
    pub requirement_bindings: Vec<RequirementBinding>,
    /// `Param.*` / `RawParam.*` for the whole submission: the base of a Job
    /// Environment's symbol table, when it has no `resolved_symtab` of its
    /// own, onto which a Service Session layers the `Service.*` symbols of
    /// the Services that Environment lists.
    pub base_symtab: SerializedSymbolTable,
    /// The run's interruption token. A Service Session does not share it
    /// (a token canceled while a Session is being torn down would cancel
    /// its `onExit` and Environment exits too, which constraint 7 wants
    /// run); instead the manager watches a child of it while a Service
    /// starts and cancels the running action through the Session's cancel
    /// handle, then ends the Session as for a scope completion.
    pub cancel_token: CancellationToken,
    pub limits: SessionLimits,
}

impl ServiceRunConfig {
    /// The profile of the document `document` (see [`super::profile_for`]).
    fn profile_for(&self, document: &Document) -> &ModelProfile {
        super::profile_for(&self.profile, &self.attached_profiles, document)
    }

    /// The bound Service of the requirement named `name`, if any.
    fn binding(&self, name: &str) -> Option<&RequirementBinding> {
        self.requirement_bindings
            .iter()
            .find(|b| b.requirement == name)
    }
}

/// What the readiness gate found.
#[derive(Debug, Default)]
pub(super) struct GateOutcome {
    /// A Service became FAILED; its scope fails.
    pub failure: Option<ServiceFailure>,
    /// Services with `completedTasks: RERUN` failed since the last gate, or
    /// were stopped and started again because a Service they list began a
    /// new Service Session (`RERUN`, or no `completedTasks` given): the
    /// completed Tasks of every Step in the union of their scopes return to
    /// the queue.
    pub rerun: Option<ServiceScope>,
    /// A Service whose scope is every Step began a new Service Session (new endpoints) since the last gate: every
    /// Environment the Task Session entered may hold stale `Service.*`
    /// values.
    pub job_wide_endpoints_changed: bool,
    /// As above, for a Service scoped to some Steps: the current Step's
    /// Environments may hold stale values.
    pub step_endpoints_changed: bool,
}

/// An instance failure or start failure, as RFC 0009 "Failure and restart"
/// classifies them for the restart decision.
#[derive(Debug)]
enum FailureKind {
    /// A start failure: a requested port is unavailable, the Session could
    /// not be created, an Environment `onEnter` failed, or `onEnter`
    /// failed. Always begins a new Service Session.
    Start(String),
    /// `onRun` exited before a health-check probe passed — possibly a port
    /// conflict, so the relaunch begins a new Service Session (new ports).
    ExitedBeforeReady(Box<ServiceRunExit>),
    /// `readinessTimeoutSeconds` elapsed while `onRun` kept running (it has
    /// been canceled and has exited by the time this is reported).
    ReadyTimedOut,
    /// `onRun` exited after READY while the scope still had work.
    Exited(Box<ServiceRunExit>),
    /// The instance became UNHEALTHY: `failureThreshold` consecutive health
    /// probes failed after READY. The runtime has canceled `onRun`; it has
    /// exited by the time the restart decision is taken.
    Unhealthy(ServiceUnhealthy),
    /// `onRun` reported with `openjd_fail` that it cannot serve (RFC 0009
    /// `<ServiceActions>`): an instance failure in itself. The runtime has
    /// canceled `onRun`, as on UNHEALTHY; the message is the reason. Whether
    /// the instance was READY does not matter: the Service said so itself,
    /// so this is not a suspected port conflict and the relaunch stays in
    /// the Session.
    Reported(String),
}

impl FailureKind {
    fn requires_new_session(&self) -> bool {
        matches!(self, Self::Start(_) | Self::ExitedBeforeReady(_))
    }

    fn describe(&self) -> String {
        match self {
            Self::Start(reason) => format!("failed to start: {reason}"),
            Self::ExitedBeforeReady(exit) => {
                format!(
                    "onRun exited before becoming READY ({})",
                    describe_exit(exit)
                )
            }
            Self::ReadyTimedOut => {
                "did not become READY within readinessTimeoutSeconds".to_string()
            }
            Self::Exited(exit) => format!(
                "onRun exited while the scope still had work ({})",
                describe_exit(exit)
            ),
            Self::Unhealthy(unhealthy) => format!("instance UNHEALTHY: {unhealthy}"),
            Self::Reported(message) => format!("onRun reported openjd_fail: {message}"),
        }
    }
}

/// `<TYPE>[ on <port>, <port>] (readinessTimeoutSeconds N[, readinessIntervalSeconds N][, healthIntervalSeconds N, failureThreshold N])`
/// — the effective health check, for the launch log line.
fn describe_health_check(check: &openjd_model::job::ServiceHealthCheck) -> String {
    let mut s = check.type_name().to_string();
    if let openjd_model::job::ServiceHealthCheck::TcpConnect { ports, .. } = check {
        s.push_str(&format!(" on {}", ports.join(", ")));
    }
    s.push_str(&format!(
        " (readinessTimeoutSeconds {}",
        check.readiness_timeout_seconds()
    ));
    if let Some(i) = check.readiness_interval_seconds() {
        s.push_str(&format!(", readinessIntervalSeconds {i}"));
    }
    match check.health_interval_seconds() {
        Some(i) => s.push_str(&format!(
            ", healthIntervalSeconds {i}, failureThreshold {})",
            check.failure_threshold()
        )),
        None => s.push_str(", no heartbeat after READY)"),
    }
    s
}

/// `Service 'X' onRun (launch N) canceled; exit code: 143` — how the
/// instance the runtime canceled (after UNHEALTHY or a readiness timeout)
/// ended, logged before the relaunch or FAILED line so the log shows the
/// old instance gone before the new one starts (RFC 0009 "Failure and
/// restart" step 2, constraint 5). `exited` instead of `canceled` when the
/// exit was the process's own.
fn log_run_exit(label: &str, launch: u32, exit: &ServiceRunExit) {
    let how = if exit.canceled || exit.state == openjd_sessions::ActionState::Canceled {
        "canceled"
    } else {
        "exited"
    };
    log_line(format!(
        "{label} onRun (launch {launch}) {how}; {}",
        describe_exit(exit)
    ));
}

fn describe_exit(exit: &ServiceRunExit) -> String {
    let mut s = match exit.exit_code {
        Some(code) => format!("exit code: {code}"),
        None if exit.state == openjd_sessions::ActionState::Canceled => {
            "exit code: N/A".to_string()
        }
        None => format!("{}", exit.state).to_lowercase(),
    };
    if let Some(msg) = &exit.fail_message {
        s.push_str(&format!("; {msg}"));
    }
    s
}

/// The part of a Service's state that a background task owns while it
/// starts or recovers the Service.
struct Instance {
    service: Service,
    /// `Service 'X'` or `Service 'X' (from <doc>)`, for every log line.
    label: String,
    /// `(scope: …)`, for every log line.
    scope_label: String,
    /// The Job Environments the Service's Sessions enter, in entry order
    /// (`job::Service::environments`, RFC 0009 "Services run inside
    /// Environments").
    environments: Vec<Environment>,
    /// The profile of each entry of `environments` whose document is not
    /// the Service's own (`None` for those that share it), index for index
    /// — `ServiceSessionConfig::environment_profiles`.
    environment_profiles: Vec<Option<ModelProfile>>,
    /// The Service's own effective values for each entry of
    /// `environments`, index for index (`job::Service::environment_parameters`).
    environment_parameters: Vec<Option<EnvironmentParameterValues>>,
    /// Endpoints of the Services this one lists in its `dependencies` that
    /// were READY when this Session was started: Services of its own
    /// document, and the bound required Services for an inline Service.
    in_scope: Vec<ServiceEndpoints>,
    /// For each entry of `environments`, the endpoints of the Services it
    /// lists, READY when this Session was started: the Session enters the
    /// Environment with them, as a Task Session does.
    environment_endpoints: Vec<Vec<ServiceEndpoints>>,
    /// Whether the Service depends on any Service, by listing it or by
    /// entering a Job Environment that lists it: a new Session must then
    /// wait at the gate for those to be READY (constraint 2).
    has_service_dependencies: bool,
    endpoints: Option<ServiceEndpoints>,
    session: Option<ServiceSession>,
    /// Relaunches consumed so far (RFC 0009 `restartPolicy.maxRestarts`).
    relaunches: u64,
    /// `--preserve`: the Session's working directory is kept and its path
    /// reported when the Service stops.
    retain_working_dir: bool,
}

/// How a background start/recovery ended.
enum Outcome {
    /// The Service is READY. `replaced` is set when a new Service Session
    /// was begun along the way (new endpoints).
    Ready { replaced: bool },
    /// The Service is FAILED (attempts exhausted). Its Session has ended.
    Failed(String),
    /// The Service is stopping; the Service's Session is returned for
    /// `end()`.
    Stopped,
    /// A new-Session relaunch of a Service that lists other Services: its
    /// old Session has ended and `relaunches` attempts are consumed. The
    /// gate returns it to idle and `start_pending` restarts it once its
    /// dependencies are READY (constraint 2), with their current endpoints.
    Regate { relaunches: u64 },
}

type Background = JoinHandle<(Instance, Outcome)>;

enum State {
    /// Not started, or stopped because its scope completed. Started by the
    /// gate when active.
    Idle,
    /// A background task is starting or relaunching the Service.
    Busy(Background),
    /// READY; the Session is held here and watched for an exit.
    Ready(Box<Instance>),
    /// FAILED; the Session has ended.
    Failed,
}

struct Managed {
    key: ServiceKey,
    service: Service,
    scope: ServiceScope,
    /// The Service's `completedTasks`; `None` only when its `maxRestarts`
    /// is 0, so a relaunch never happens (an instance failure fails the
    /// scope) and the value matters only for a dependent restart, where it
    /// reads as `RERUN`.
    policy: Option<CompletedTasksPolicy>,
    /// The Services this one depends on (§9 item 4, `<StepDependency>`
    /// constraint 3): those it lists with the `service` key in its
    /// `dependencies` — Services of the same document, plus the bound
    /// required Services an inline Service lists — and those listed by a
    /// Job Environment its Sessions enter. It starts after each is READY and
    /// is stopped before any of them.
    depends_on_services: BTreeSet<ServiceKey>,
    /// The Services it lists itself, whose endpoints its own actions see.
    listed_services: Vec<ServiceKey>,
    /// Set while the Service is idle because a Service it lists (`Some`,
    /// named) began a new Service Session; the gate starts it again once
    /// its dependencies are READY and logs that it is starting again.
    restart_after: Option<ServiceKey>,
    /// The Job Environments its Sessions enter, with their profiles, the
    /// Service's values for them, and the Services each lists.
    environments: Vec<Environment>,
    environment_profiles: Vec<Option<ModelProfile>>,
    environment_parameters: Vec<Option<EnvironmentParameterValues>>,
    environment_listed: Vec<Vec<ServiceKey>>,
    /// Whether a Step in the Service's scope is pending in the current
    /// round: the gate starts active idle Services and leaves inactive ones
    /// idle (constraint 9: a Service whose scope schedules no Task need
    /// not start).
    active: bool,
    /// Cancels background work when the Service is stopped. Replaced when
    /// the Service is started again from idle.
    stop: CancellationToken,
    state: State,
    /// Set when a relaunch that began a new Service Session (new endpoints)
    /// was promoted to `Ready` by the in-Task watcher before any gate saw
    /// it; the next gate consumes it as it would the `replaced` flag of a
    /// `Ready` outcome it awaited itself — also when the instance has
    /// failed again by then and the gate awaits a later relaunch of it.
    pending_replaced: bool,
    /// The relaunch count to carry into the next Session when a Service that
    /// lists other Services was sent back to the gate for a new-Session
    /// relaunch (`Outcome::Regate`): the gate restarts it from idle once its
    /// dependencies are READY (constraint 2), re-seeding its in-scope
    /// endpoints, instead of relaunching in the background with stale ones.
    pending_relaunches: u64,
    /// Set alongside `pending_relaunches` for a `Regate`: the restart the
    /// gate performs opens a NEW Service Session (new ports), so the gate
    /// must treat its `Ready` as `replaced` — restarting this Service's own
    /// dependents and re-entering the Environments that list it, exactly as
    /// a `Ready { replaced: true }` it had awaited directly (constraint 8).
    pending_new_session: bool,
    /// Set once the in-Task watcher has reported this Service's FAILED
    /// verdict ([`InTaskEvent::Failed`]), so that it is reported once: the
    /// verdict itself stays in the `Busy` handle for the next gate.
    failed_noticed: bool,
}

impl Managed {
    /// A fresh [`Instance`] for a new Service Session (constraint 8: new
    /// host selection, new ports, new working directory).
    fn new_instance(&self, retain_working_dir: bool) -> Instance {
        Instance {
            service: self.service.clone(),
            label: self.key.to_string(),
            scope_label: scope_label(&self.scope),
            environments: self.environments.clone(),
            environment_profiles: self.environment_profiles.clone(),
            environment_parameters: self.environment_parameters.clone(),
            in_scope: Vec::new(),
            environment_endpoints: Vec::new(),
            has_service_dependencies: !self.depends_on_services.is_empty(),
            endpoints: None,
            session: None,
            // Carried from a `Regate` so a Service sent back to the gate for
            // a new-Session relaunch keeps its consumed attempts.
            relaunches: self.pending_relaunches,
            retain_working_dir,
        }
    }

    fn is_job_wide(&self) -> bool {
        self.scope.is_all_steps()
    }

    /// Whether this Service is active, scoped to some Steps, and none of
    /// them is in `remaining`: its scope completes (constraint 6).
    fn scope_completes(&self, remaining: &BTreeSet<String>) -> bool {
        self.active
            && !self.is_job_wide()
            && self
                .scope
                .step_names()
                .is_some_and(|steps| steps.is_disjoint(remaining))
    }
}

struct Shared {
    config: ServiceRunConfig,
    ports: Mutex<PortAllocator>,
    session_counter: AtomicU32,
    /// Signaled (one permit, `notify_one`) each time a background start or
    /// relaunch finishes, so the in-Task watcher can re-scan the `Busy`
    /// Services and promote one that became READY.
    settled: tokio::sync::Notify,
    /// The task IDs of the background starts and relaunches whose future
    /// has returned, recorded before `settled` is signaled. The signal can
    /// wake the watcher before the task itself has finished, so
    /// `JoinHandle::is_finished` may still be false at the re-scan, and no
    /// second signal follows; the re-scan treats a task recorded here as
    /// finished instead.
    settled_tasks: Mutex<HashSet<tokio::task::Id>>,
}

impl Shared {
    /// Run a Service's background start or relaunch, signaling `settled`
    /// when it finishes.
    fn spawn_background(
        self: &Arc<Self>,
        fut: impl Future<Output = (Instance, Outcome)> + Send + 'static,
    ) -> JoinHandle<(Instance, Outcome)> {
        let shared = Arc::clone(self);
        tokio::spawn(async move {
            let out = fut.await;
            shared
                .settled_tasks
                .lock()
                .expect("settled_tasks lock")
                .insert(tokio::task::id());
            shared.settled.notify_one();
            out
        })
    }

    /// Whether the background task `handle` has finished, or its future has
    /// returned and it is about to (see [`settled_tasks`](Self::settled_tasks)).
    fn background_settled(&self, handle: &Background) -> bool {
        handle.is_finished()
            || self
                .settled_tasks
                .lock()
                .expect("settled_tasks lock")
                .contains(&handle.id())
    }

    /// Forget `handle`'s task once its output has been taken.
    fn forget_background(&self, id: tokio::task::Id) {
        self.settled_tasks
            .lock()
            .expect("settled_tasks lock")
            .remove(&id);
    }
}

/// An instance failure observed on a READY Service (RFC 0009 "Failure and
/// restart"): its `onRun` exited other than by the runner's cancelation, or
/// its health check declared it UNHEALTHY (on which the Service Session has
/// already canceled `onRun`).
/// What the in-Task watcher ([`ServiceManager::wait_in_task_event`])
/// observed.
pub(super) enum InTaskEvent {
    /// A READY instance failed; the caller begins its recovery.
    Instance(Detected),
    /// A Service became FAILED, with this scope: a running Task in it fails
    /// (RFC 0009 "Failure and restart" step 4).
    Failed(ServiceScope),
}

pub(super) struct Detected {
    idx: usize,
    failure: InstanceFailure,
}

enum InstanceFailure {
    Exit(ServiceRunExit),
    Unhealthy(ServiceUnhealthy),
    /// `onRun` reported `openjd_fail`; the runtime canceled it.
    Reported(String),
}

impl InstanceFailure {
    /// Classify an `onRun` exit the Session reported: UNHEALTHY when the
    /// health check forced it, a reported failure when `onRun`'s own
    /// `openjd_fail` did, an ordinary exit otherwise.
    fn from_exit(exit: ServiceRunExit) -> Self {
        if let Some(u) = exit.unhealthy {
            return Self::Unhealthy(u);
        }
        match exit.reported_failure {
            Some(message) => Self::Reported(message),
            None => Self::Exit(exit),
        }
    }

    fn into_kind(self) -> FailureKind {
        match self {
            Self::Exit(exit) => FailureKind::Exited(Box::new(exit)),
            Self::Unhealthy(u) => FailureKind::Unhealthy(u),
            Self::Reported(message) => FailureKind::Reported(message),
        }
    }
}

/// The scheduler side of RFC 0009 for one `openjd run`.
pub(super) struct ServiceManager {
    shared: Arc<Shared>,
    services: Vec<Managed>,
}

fn log_line(msg: impl std::fmt::Display) {
    println!("{}\t{msg}", crate::format_log_timestamp());
}

fn log_banner(label: &str) {
    let ts = crate::format_log_timestamp();
    println!("{ts}\t");
    println!("{ts}\t==============================================");
    println!("{ts}\t--------- {label}");
    println!("{ts}\t==============================================");
}

impl ServiceManager {
    pub(super) fn new(config: ServiceRunConfig) -> Self {
        Self {
            shared: Arc::new(Shared {
                config,
                ports: Mutex::new(PortAllocator::default()),
                session_counter: AtomicU32::new(0),
                settled: tokio::sync::Notify::new(),
                settled_tasks: Mutex::new(HashSet::new()),
            }),
            services: Vec::new(),
        }
    }

    /// Register every Service of the combined Job (`job.services`: external
    /// Services then the Job Template's own), none started or active. Each
    /// Service's Sessions enter the Job Environments job creation decided
    /// for it (`job::Service::environments`), with its own effective values
    /// (`job::Service::environment_parameters`); `environment_documents`
    /// is the document of each entry of `job.job_environments`, index for
    /// index. A Service depends on the Services it lists and on those a Job
    /// Environment its Sessions enter lists (RFC 0009 `<StepDependency>`
    /// constraint 3).
    pub(super) fn register(&mut self, job: &Job, environment_documents: &[Document]) {
        if !self.services.is_empty() {
            return;
        }
        let envs: Vec<Environment> = job.job_environments.clone().unwrap_or_default();
        // A `service: <name>` entry written in `document` names a Service of
        // that document — or, in the Job Template, a required external
        // Service bound at submission.
        let resolve = |document: &Document, name: &str| match (
            document.is_job_template(),
            self.shared.config.binding(name),
        ) {
            (true, Some(binding)) if !service_declared(job, name) => {
                ServiceKey::of_binding(binding)
            }
            _ => ServiceKey::sibling(document, name),
        };
        let mut registered = Vec::new();
        for service in job.services.iter().flatten() {
            let key = ServiceKey::of(service);
            let entered: Vec<usize> = envs
                .iter()
                .zip(environment_documents)
                .enumerate()
                .filter(|(_, (env, doc))| service.enters_environment(doc, &env.name))
                .map(|(e, _)| e)
                .collect();
            let documents: Vec<Document> = entered
                .iter()
                .map(|e| environment_documents[*e].clone())
                .collect();
            let profiles = self.environment_profiles(service, &documents);
            let environment_parameters = entered
                .iter()
                .map(|e| {
                    service
                        .environment_parameters
                        .iter()
                        .flatten()
                        .find(|v| {
                            v.document == environment_documents[*e]
                                && v.environment == envs[*e].name
                        })
                        .cloned()
                })
                .collect();
            let environment_listed: Vec<Vec<ServiceKey>> = entered
                .iter()
                .map(|e| {
                    envs[*e]
                        .depends_on_services()
                        .map(|name| resolve(&environment_documents[*e], name))
                        .collect()
                })
                .collect();
            let listed_services: Vec<ServiceKey> = service
                .depends_on_services()
                .map(|name| resolve(&key.document, name))
                .collect();
            let depends_on_services = listed_services
                .iter()
                .chain(environment_listed.iter().flatten())
                .filter(|k| **k != key)
                .cloned()
                .collect();
            registered.push(Managed {
                key,
                service: service.clone(),
                scope: service.scope.clone(),
                policy: service.restart_policy.completed_tasks,
                depends_on_services,
                listed_services,
                restart_after: None,
                environments: entered.iter().map(|e| envs[*e].clone()).collect(),
                environment_profiles: profiles,
                environment_parameters,
                environment_listed,
                active: false,
                stop: self.shared.config.cancel_token.child_token(),
                state: State::Idle,
                pending_replaced: false,
                pending_relaunches: 0,
                pending_new_session: false,
                failed_noticed: false,
            });
        }
        self.services = registered;
    }

    /// The `environment_profiles` of `service`'s Session for the Job
    /// Environments declared by `environment_documents`: `Some(profile)` for
    /// each from a document other than the Service's own, `None` for those
    /// sharing it (the Session's own profile — the Service's document's —
    /// applies).
    fn environment_profiles(
        &self,
        service: &Service,
        environment_documents: &[Document],
    ) -> Vec<Option<ModelProfile>> {
        environment_documents
            .iter()
            .map(|doc| {
                (*doc != service.document).then(|| self.shared.config.profile_for(doc).clone())
            })
            .collect()
    }

    /// Check and reserve the requested ports of `services` before any
    /// Service starts ([`PortAllocator::reserve_requested`]).
    pub(super) fn reserve_requested_ports(&self, services: &[Service]) -> Result<(), String> {
        self.shared
            .ports
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .reserve_requested(services)
    }

    /// `true` when any Service is registered.
    pub(super) fn any_registered(&self) -> bool {
        !self.services.is_empty()
    }

    /// The number of registered Services whose scope is every Step
    /// (including external Services).
    pub(super) fn job_wide_count(&self) -> usize {
        self.services.iter().filter(|m| m.is_job_wide()).count()
    }

    /// Activate every Service whose scope is every Step (constraint 9: the
    /// Job will schedule a Task), so the next gate starts them. Called
    /// before the Task Session enters the Job's Environments, which may
    /// reference them.
    pub(super) fn activate_job_wide(&mut self) {
        for m in self.services.iter_mut().filter(|m| m.is_job_wide()) {
            m.active = true;
        }
    }

    /// Activate every Service whose scope includes `step`, so the next gate
    /// starts those not yet READY (a Service stopped because its scope had
    /// completed starts again in a new Session, constraint 8). A Service that
    /// lists Steps in its `dependencies` is activated only once every listed
    /// Step has completed (`completed`) — a listed Step outside the run's
    /// selection (`selected`) counts as completed, as a Step's own
    /// dependencies do under `--step` — and an error names a Service whose
    /// Step dependency is selected but not yet completed, which the Step
    /// ordering prevents. (Its Service dependencies are the gate's: it
    /// starts once they are READY, constraint 2.) Returns the names of the
    /// Services activated.
    pub(super) fn activate_for_step(
        &mut self,
        step: &str,
        completed: &BTreeSet<String>,
        selected: &BTreeSet<String>,
    ) -> Result<Vec<String>, RunError> {
        let mut activated = Vec::new();
        for m in self.services.iter_mut() {
            if m.active || !m.scope.contains(step) {
                continue;
            }
            let unmet: Vec<&str> = m
                .service
                .depends_on_steps()
                .filter(|d| selected.contains(*d) && !completed.contains(*d))
                .collect();
            // A Step dependency outside the selection (`--step` without
            // `--run-dependencies`) is taken as completed, as a Step's own
            // dependencies are; say so, since whatever that Step would have
            // produced for the Service is not there.
            let skipped: Vec<String> = m
                .service
                .depends_on_steps()
                .filter(|d| !selected.contains(*d))
                .map(|d| format!("'{d}'"))
                .collect();
            if !skipped.is_empty() {
                log_line(format!(
                    "{} depends on Step(s) {}, which {} not being run (--step without \
                     --run-dependencies); starting it as if {} had completed",
                    m.key,
                    skipped.join(", "),
                    if skipped.len() == 1 { "is" } else { "are" },
                    if skipped.len() == 1 { "it" } else { "they" },
                ));
            }
            if !unmet.is_empty() {
                return Err(format!(
                    "{} {} cannot start before Step '{step}': it depends on Step(s) {} which \
                     have not completed",
                    m.key,
                    scope_label(&m.scope),
                    unmet
                        .iter()
                        .map(|d| format!("'{d}'"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
                .into());
            }
            m.active = true;
            activated.push(m.key.name.clone());
        }
        Ok(activated)
    }

    /// The names of the registered Services whose scope includes `step` and
    /// which are not active (will be started for it).
    pub(super) fn inactive_for_step(&self, step: &str) -> Vec<String> {
        self.services
            .iter()
            .filter(|m| !m.active && m.scope.contains(step))
            .map(|m| m.key.to_string())
            .collect()
    }

    /// The endpoints an entity declared by `document` may reference in the
    /// Task Session (`port` and `connectAddress`; `bindAddress` is never
    /// seeded here), per `visibility`:
    ///
    /// - [`Visibility::Step`] — a Task of the Step, or one of its Step
    ///   Environments: the READY inline Services of the Job Template whose
    ///   scope includes the Step, and the READY attached Services bound to
    ///   the Job Template's `externalServices` — seeded by scope, which is
    ///   every Step, not by visibility: a Step that does not list
    ///   `service: <requirement>` gets the symbols too, but template
    ///   validation has already rejected any reference from it (Template
    ///   Schemas §9 scope rule 3, §9.8 item 2);
    /// - [`Visibility::Environment`] — a Job Environment, the Job Template's
    ///   own or an attached one: the READY Services of `document` it lists in
    ///   its `dependencies` and, for the Job Template's own, the READY
    ///   attached Services bound to the requirements it lists (Template
    ///   Schemas §4 item 3, §9 scope rules 4–5, §1.2.2 item 3). A Job
    ///   Environment that lists an inline Service has put every Step in its
    ///   scope, so the Service is READY before the Environment is entered.
    pub(super) fn task_scope_endpoints(
        &self,
        document: &Document,
        visibility: Visibility<'_>,
    ) -> Vec<ServiceEndpoints> {
        let mut out: Vec<ServiceEndpoints> = self
            .services
            .iter()
            .filter(|m| m.key.document == *document)
            .filter(|m| match visibility {
                Visibility::Step(step) => m.scope.contains(step),
                Visibility::Environment(env) => env.depends_on_service(&m.key.name),
            })
            .filter_map(|m| match &m.state {
                State::Ready(inst) => inst.endpoints.clone(),
                _ => None,
            })
            .collect();
        if document.is_job_template() {
            for binding in &self.shared.config.requirement_bindings {
                let listed = match visibility {
                    Visibility::Step(_) => true,
                    Visibility::Environment(env) => env.depends_on_service(&binding.requirement),
                };
                if !listed {
                    continue;
                }
                let key = ServiceKey::of_binding(binding);
                if let Some(endpoints) = self.ready_endpoints_of(&key) {
                    out.push(endpoints);
                }
            }
        }
        out
    }

    /// Whether the Job Environment `env`, declared by `document`, lists a
    /// Service whose scope is not every Step (directly, or for the Job
    /// Template's own, through a bound `externalServices` requirement): its
    /// `Service.*` values change when such a Service starts for a Step, so
    /// a Step-scoped endpoint change re-enters it.
    pub(super) fn lists_step_scoped(&self, document: &Document, env: &Environment) -> bool {
        self.lists_any(document, env, |m| !m.is_job_wide())
    }

    /// Whether `env`, declared by `document`, lists a Service whose scope
    /// completes once no Step of `remaining` is left to run
    /// ([`stop_completed_scopes`](Self::stop_completed_scopes)): the Task
    /// Session exits such an Environment before the Service stops
    /// (constraint 6).
    pub(super) fn lists_completing(
        &self,
        document: &Document,
        env: &Environment,
        remaining: &BTreeSet<String>,
    ) -> bool {
        self.lists_any(document, env, |m| m.scope_completes(remaining))
    }

    /// Whether `env`, declared by `document`, lists a Service `pred` holds
    /// for: one of its own document's, or for the Job Template's, the
    /// attached Service bound to a requirement it lists.
    fn lists_any(
        &self,
        document: &Document,
        env: &Environment,
        pred: impl Fn(&Managed) -> bool,
    ) -> bool {
        let holds = |key: &ServiceKey| self.services.iter().any(|m| m.key == *key && pred(m));
        self.services
            .iter()
            .any(|m| m.key.document == *document && env.depends_on_service(&m.key.name) && pred(m))
            || (document.is_job_template()
                && self.shared.config.requirement_bindings.iter().any(|b| {
                    env.depends_on_service(&b.requirement) && holds(&ServiceKey::of_binding(b))
                }))
    }

    fn ready_endpoints_of(&self, key: &ServiceKey) -> Option<ServiceEndpoints> {
        self.services
            .iter()
            .find(|m| m.key == *key)
            .and_then(|m| match &m.state {
                State::Ready(inst) => inst.endpoints.clone(),
                _ => None,
            })
    }

    /// The readiness gate (constraint 3), run before every Task: observe
    /// any instance failure since the last gate and begin the restart decision
    /// for it; await every background start/relaunch; stop the Services
    /// that depend on one whose Session was replaced, to start them again;
    /// start every active Service not yet started, in waves of Services
    /// whose Service dependencies are READY (constraint 2); repeat until
    /// every active Service is READY or FAILED.
    pub(super) async fn gate(&mut self) -> Result<GateOutcome, RunError> {
        let mut outcome = GateOutcome::default();
        loop {
            // The run is stopping: nothing more is started or awaited here —
            // `stop_all` tears the Services down. Terminating the loop
            // explicitly on cancellation is what keeps a Service left `Idle`
            // but `active` (e.g. a dependent whose upstream was `Stopped`
            // mid-relaunch) from either erroring or spinning the loop, since
            // the per-round predicates below (`any_busy` / `any_startable` /
            // `any_pending_replaced`) know nothing of cancellation.
            if self.shared.config.cancel_token.is_cancelled() {
                break;
            }
            for detected in self.poll_failures() {
                let (policy, scope) = self.begin_recovery(detected);
                if policy == Some(CompletedTasksPolicy::Rerun) {
                    outcome.rerun = Some(merge_scopes(outcome.rerun.take(), &scope));
                }
            }
            if !self.any_busy() && !self.any_startable() && !self.any_pending_replaced() {
                break;
            }
            let settled = self.await_busy(&mut outcome).await?;
            if outcome.failure.is_some() {
                break;
            }
            self.restart_dependents(&settled, &mut outcome).await;
            self.start_pending(&mut outcome).await?;
            if outcome.failure.is_some() {
                break;
            }
        }
        Ok(outcome)
    }

    /// Constraint 6: stop every active Service whose scope contains none of
    /// `remaining` (the Steps still to run), in dependency order — a Service
    /// before any it depends on (constraint 4) — and return each to idle.
    /// A Service whose scope is every Step is stopped only by
    /// [`stop_all`](Self::stop_all).
    ///
    /// Returns the FAILED verdicts that were decided during the scope's last
    /// Task and that no readiness gate ran to collect (see
    /// [`stop_set`](Self::stop_set)), so the caller fails the scope
    /// ("Failure and restart" step 4). A relaunch still undecided is cut
    /// short, not awaited: the scope has completed, so its outcome is no
    /// longer a failure.
    pub(super) async fn stop_completed_scopes(
        &mut self,
        remaining: &BTreeSet<String>,
    ) -> Result<Vec<ServiceFailure>, RunError> {
        let done: BTreeSet<ServiceKey> = self
            .services
            .iter()
            .filter(|m| m.scope_completes(remaining))
            .map(|m| m.key.clone())
            .collect();
        self.stop_set(&done).await
    }

    /// Signal scope completion to every active Service whose scope contains
    /// none of `remaining` (constraint 6): cancel its stop token so that a
    /// relaunch in flight returns `Stopped` rather than deciding FAILED.
    /// Called the moment the scope's last Task returns, before the Task
    /// Session exits its Environments, so that an exit or readiness timeout
    /// observed during those exits is not a failure ("an exit observed after
    /// the scope has completed is not a failure"). The Session teardown
    /// itself follows in [`stop_completed_scopes`](Self::stop_completed_scopes).
    pub(super) fn complete_scopes(&mut self, remaining: &BTreeSet<String>) {
        for m in self
            .services
            .iter()
            .filter(|m| m.scope_completes(remaining))
        {
            m.stop.cancel();
        }
    }

    /// Signal completion of every scope, as [`complete_scopes`](Self::complete_scopes)
    /// does for one Step's: the run's last Task has returned.
    pub(super) fn complete_all_scopes(&mut self) {
        for m in self.services.iter().filter(|m| m.active) {
            m.stop.cancel();
        }
    }

    /// Stop everything, in dependency order (constraint 4), after the Task
    /// Session has exited the Job's Environments. Returns the FAILED
    /// verdicts decided during the run's last Task that no gate collected,
    /// as [`stop_completed_scopes`](Self::stop_completed_scopes) does: a
    /// job-wide Service whose attempts ran out during that Task fails the
    /// Job.
    pub(super) async fn stop_all(&mut self) -> Result<Vec<ServiceFailure>, RunError> {
        let all: BTreeSet<ServiceKey> = self.services.iter().map(|m| m.key.clone()).collect();
        self.stop_set(&all).await
    }

    /// Stop the Services in `keys`: cancel their background work and end
    /// their Sessions, each before any Service it depends on (constraint 4;
    /// see [`stop_order`](Self::stop_order)).
    ///
    /// A background task that had already decided FAILED when its stop token
    /// was canceled — and may still have been running `onExit` — returns
    /// `Outcome::Failed` rather than `Stopped`; those verdicts are collected
    /// and returned, since no gate will run to see them. A task cut short
    /// before deciding returns `Stopped`, and is not a failure.
    async fn stop_set(
        &mut self,
        keys: &BTreeSet<ServiceKey>,
    ) -> Result<Vec<ServiceFailure>, RunError> {
        let mut failures = Vec::new();
        if keys.is_empty() {
            return Ok(failures);
        }
        for m in self.services.iter().filter(|m| keys.contains(&m.key)) {
            m.stop.cancel();
        }
        // A background task that panicked is an internal error, but every
        // other Service is still stopped (in order) before it is reported.
        let mut first_error = None;
        for idx in self.stop_order(keys) {
            let m = &mut self.services[idx];
            match stop_managed(m, &self.shared).await {
                Ok(Some(reason)) => failures.push(ServiceFailure {
                    name: m.key.name.clone(),
                    document: m.key.document.clone(),
                    scope: m.scope.clone(),
                    reason,
                }),
                Ok(None) => {}
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(failures),
        }
    }

    /// The order in which to stop the Services in `keys` (constraint 4):
    /// each before any Service it depends on — the reverse topological order
    /// of the dependency graph restricted to `keys`, in waves of Services no
    /// other pending Service depends on; registration order breaks ties,
    /// latest first.
    fn stop_order(&self, keys: &BTreeSet<ServiceKey>) -> Vec<usize> {
        let mut order = Vec::with_capacity(keys.len());
        let mut pending: BTreeSet<ServiceKey> = keys.clone();
        while !pending.is_empty() {
            let depended_on_by_pending: BTreeSet<ServiceKey> = self
                .services
                .iter()
                .filter(|m| pending.contains(&m.key))
                .flat_map(|m| m.depends_on_services.iter().cloned())
                .collect();
            let mut wave: Vec<usize> = self
                .services
                .iter()
                .enumerate()
                .filter(|(_, m)| {
                    pending.contains(&m.key) && !depended_on_by_pending.contains(&m.key)
                })
                .map(|(i, _)| i)
                .collect();
            if wave.is_empty() {
                // Cannot happen for an acyclic dependency graph; take the
                // rest in reverse registration order rather than spin.
                wave = self
                    .services
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| pending.contains(&m.key))
                    .map(|(i, _)| i)
                    .collect();
            }
            wave.reverse();
            for idx in wave {
                pending.remove(&self.services[idx].key);
                order.push(idx);
            }
        }
        order
    }

    /// Resolve when, while a Task runs, a READY Service suffers an instance
    /// failure (its health check declares it UNHEALTHY, or its `onRun` exits
    /// other than by cancelation), or a Service's background recovery
    /// decides FAILED. Pending forever when neither can happen.
    pub(super) async fn wait_in_task_event(&mut self) -> InTaskEvent {
        loop {
            // A relaunch that became READY while the Task ran is still
            // `Busy` (its READY outcome waits in the JoinHandle for a gate
            // that will not run until the Task ends). Promote it so that
            // its exit is watched like any READY instance's.
            if let Some(scope) = self.promote_finished_relaunches().await {
                return InTaskEvent::Failed(scope);
            }
            if let Some(d) = self.wait_ready_instance_failure().await {
                return InTaskEvent::Instance(d);
            }
        }
    }

    /// Take every `Busy` background task that has finished with
    /// `Outcome::Ready` and hold its instance as `State::Ready`, so that it
    /// is watched. `Failed` and `Stopped` outcomes are left in place for the
    /// next gate (or scope end) to collect; the scope of the first `Failed`
    /// one not yet reported is returned, so that the running Task can be
    /// failed if it falls in it (RFC 0009 "Failure and restart" step 4).
    async fn promote_finished_relaunches(&mut self) -> Option<ServiceScope> {
        let mut failed: Option<ServiceScope> = None;
        let retain = self.shared.config.retain_working_dir;
        let shared = Arc::clone(&self.shared);
        for m in self.services.iter_mut() {
            // The handle is awaited in place (`&mut JoinHandle` is a
            // Future) and the state replaced only once its output is in
            // hand: this future runs inside the Task's `select!` and may be
            // dropped at any await, after which the handle — still `Busy`,
            // its output untaken — is awaited by the next gate as before.
            // A settled task whose output is not yet stored is awaited
            // too: it finishes at once.
            let result = match &mut m.state {
                State::Busy(handle) if shared.background_settled(handle) => {
                    let id = handle.id();
                    let result = handle.await;
                    shared.forget_background(id);
                    result
                }
                _ => continue,
            };
            match result {
                Ok((inst, Outcome::Ready { replaced })) => {
                    // A new-Session relaunch changed the endpoints; the
                    // gate's dependents check runs next time it is reached.
                    m.pending_replaced |= replaced;
                    m.state = State::Ready(Box::new(inst));
                }
                Ok((inst, outcome)) => {
                    if matches!(outcome, Outcome::Failed(_)) && !m.failed_noticed {
                        m.failed_noticed = true;
                        failed.get_or_insert_with(|| m.scope.clone());
                    }
                    // Re-spawn a trivial task carrying the finished outcome,
                    // so the gate's `await_busy` sees exactly what it would
                    // have.
                    m.state = State::Busy(tokio::spawn(async move { (inst, outcome) }));
                }
                Err(e) => {
                    // The task panicked (an internal error): its instance is
                    // gone with it. Hand the next gate a FAILED outcome, as
                    // `await_busy` would have reported had it awaited the
                    // panic itself, so the run fails rather than carrying on
                    // without the Service.
                    let reason = format!("background task failed: {e}");
                    log_line(format!(
                        "{} {} is FAILED: {reason}",
                        m.key,
                        scope_label(&m.scope)
                    ));
                    let inst = m.new_instance(retain);
                    m.state =
                        State::Busy(tokio::spawn(async move { (inst, Outcome::Failed(reason)) }));
                    m.failed_noticed = true;
                    failed.get_or_insert_with(|| m.scope.clone());
                }
            }
        }
        failed
    }

    /// Wait for a READY instance to exit or become UNHEALTHY, or for a
    /// background start or relaunch to finish (returning `None`, so the
    /// caller re-scans and re-arms).
    async fn wait_ready_instance_failure(&self) -> Option<Detected> {
        let mut watchers: Vec<Pin<Box<dyn Future<Output = Option<Detected>> + Send>>> = Vec::new();
        if self.any_busy() {
            // `notify_one` stores a permit when nothing waits, so a finish
            // between the caller's scan and this await is not missed; a
            // stale permit costs one extra scan.
            let shared = Arc::clone(&self.shared);
            watchers.push(Box::pin(async move {
                shared.settled.notified().await;
                None
            }));
        }
        for (idx, m) in self.services.iter().enumerate() {
            let State::Ready(inst) = &m.state else {
                continue;
            };
            let Some(session) = inst.session.as_ref() else {
                continue;
            };
            let (Some(mut exit_rx), Some(mut health_rx)) =
                (session.exit_watch(), session.health_watch())
            else {
                continue;
            };
            watchers.push(Box::pin(async move {
                loop {
                    // UNHEALTHY — and a reported openjd_fail — are published
                    // as soon as the Session decides them, before the
                    // canceled onRun has exited: the instance is UNREADY
                    // now, and no further Task is gated on it.
                    match health_rx.borrow_and_update().clone() {
                        ServiceHealth::Unhealthy(u) => {
                            return Some(Detected {
                                idx,
                                failure: InstanceFailure::Unhealthy(u),
                            });
                        }
                        ServiceHealth::Failed { message } => {
                            return Some(Detected {
                                idx,
                                failure: InstanceFailure::Reported(message),
                            });
                        }
                        _ => {}
                    }
                    let current = exit_rx.borrow_and_update().clone();
                    match current {
                        Some(exit) if exit.canceled => std::future::pending::<()>().await,
                        Some(exit) => {
                            return Some(Detected {
                                idx,
                                failure: InstanceFailure::from_exit(exit),
                            })
                        }
                        None => {}
                    }
                    tokio::select! {
                        r = exit_rx.changed() => {
                            if r.is_err() {
                                return Some(Detected {
                                    idx,
                                    failure: InstanceFailure::Exit(driver_lost_exit()),
                                });
                            }
                        }
                        r = health_rx.changed() => {
                            if r.is_err() {
                                // The driver is gone; the exit watcher
                                // reports what happened.
                                health_rx.mark_unchanged();
                                if exit_rx.changed().await.is_err() {
                                    return Some(Detected {
                                        idx,
                                        failure: InstanceFailure::Exit(driver_lost_exit()),
                                    });
                                }
                            }
                        }
                    }
                }
            }));
        }
        if watchers.is_empty() {
            std::future::pending::<()>().await;
        }
        let (detected, _, _) = futures_util::future::select_all(watchers).await;
        detected
    }

    /// Observe the restart decision for an instance failure (RFC 0009
    /// "Failure and restart"): the Service becomes UNREADY (an UNHEALTHY
    /// instance once its canceled `onRun` has exited, which the background
    /// relaunch awaits) and its relaunch (or FAILED verdict) proceeds in the
    /// background. Returns the Service's `completedTasks` policy (`None`
    /// when it gave none, which only a Service with `maxRestarts` 0 may: no
    /// relaunch follows, and the failure fails the scope) and its scope, so
    /// the caller can cancel and requeue Tasks under `RERUN`.
    pub(super) fn begin_recovery(
        &mut self,
        detected: Detected,
    ) -> (Option<CompletedTasksPolicy>, ServiceScope) {
        let Detected { idx, failure } = detected;
        let m = &mut self.services[idx];
        let policy = m.policy;
        let scope = m.scope.clone();
        if let State::Ready(inst) = std::mem::replace(&mut m.state, State::Failed) {
            if let InstanceFailure::Unhealthy(u) = &failure {
                log_line(format!(
                    "{} {} is UNHEALTHY: {u}",
                    inst.label, inst.scope_label
                ));
            }
            let kind = failure.into_kind();
            log_line(format!(
                "{} {} is UNREADY: {} ({})",
                inst.label,
                inst.scope_label,
                kind.describe(),
                describe_policy(policy)
            ));
            m.state = State::Busy(self.shared.spawn_background(start_or_recover(
                *inst,
                Some(kind),
                self.shared.clone(),
                m.stop.clone(),
            )));
        }
        (policy, scope)
    }

    fn poll_failures(&self) -> Vec<Detected> {
        let mut out = Vec::new();
        for (idx, m) in self.services.iter().enumerate() {
            let State::Ready(inst) = &m.state else {
                continue;
            };
            let Some(session) = inst.session.as_ref() else {
                continue;
            };
            match session.health_watch().map(|rx| rx.borrow().clone()) {
                Some(ServiceHealth::Unhealthy(u)) => {
                    out.push(Detected {
                        idx,
                        failure: InstanceFailure::Unhealthy(u),
                    });
                    continue;
                }
                Some(ServiceHealth::Failed { message }) => {
                    out.push(Detected {
                        idx,
                        failure: InstanceFailure::Reported(message),
                    });
                    continue;
                }
                _ => {}
            }
            let Some(rx) = session.exit_watch() else {
                continue;
            };
            let current = rx.borrow().clone();
            if let Some(exit) = current {
                if !exit.canceled {
                    out.push(Detected {
                        idx,
                        failure: InstanceFailure::from_exit(exit),
                    });
                }
            }
        }
        out
    }

    /// A READY instance the in-Task watcher promoted from a new-Session
    /// relaunch, whose replaced endpoints no gate has yet acted on.
    fn any_pending_replaced(&self) -> bool {
        self.services.iter().any(|m| m.pending_replaced)
    }

    fn any_busy(&self) -> bool {
        self.services
            .iter()
            .any(|m| matches!(m.state, State::Busy(_)))
    }

    /// An active Service that is idle: the gate has something to start.
    fn any_startable(&self) -> bool {
        self.services
            .iter()
            .any(|m| m.active && matches!(m.state, State::Idle))
    }

    /// Await every background task. Records FAILED Services on `outcome`
    /// (the first as its `failure`) and returns the keys of the Services
    /// that became READY in a new Session.
    async fn await_busy(&mut self, outcome: &mut GateOutcome) -> Result<Vec<ServiceKey>, RunError> {
        let mut replaced = Vec::new();
        for m in self.services.iter_mut() {
            // A relaunch the in-Task watcher promoted to READY began a new
            // Session this gate has not yet accounted for. The instance may
            // since have failed again and be relaunching (`Busy`, possibly
            // in the same Session): its dependents still hold the endpoints
            // of the Session before, so the flag carries over to whatever
            // outcome is awaited below. A Service no longer running (its
            // scope completed, or FAILED) has nothing to restart.
            let promoted = std::mem::take(&mut m.pending_replaced);
            if promoted && matches!(m.state, State::Ready(_)) {
                replaced.push(m.key.clone());
                if m.is_job_wide() {
                    outcome.job_wide_endpoints_changed = true;
                } else {
                    outcome.step_endpoints_changed = true;
                }
            }
            if !matches!(m.state, State::Busy(_)) {
                continue;
            }
            let State::Busy(handle) = std::mem::replace(&mut m.state, State::Failed) else {
                unreachable!("checked above");
            };
            let id = handle.id();
            let joined = handle.await;
            self.shared.forget_background(id);
            let (inst, result) =
                joined.map_err(|e| format!("{}: background task failed: {e}", m.key))?;
            match result {
                Outcome::Ready { replaced: r } => {
                    // `r`: this Session began anew along the way.
                    // `promoted`: an in-Task promotion already flagged it.
                    // `pending_new_session`: a `Regate` restart, whose new
                    // ports the gate must now propagate.
                    let new_session = r || promoted || m.pending_new_session;
                    m.pending_new_session = false;
                    if new_session {
                        replaced.push(m.key.clone());
                        if m.is_job_wide() {
                            outcome.job_wide_endpoints_changed = true;
                        } else {
                            outcome.step_endpoints_changed = true;
                        }
                    }
                    m.state = State::Ready(Box::new(inst));
                }
                Outcome::Failed(reason) => {
                    let failure = ServiceFailure {
                        name: m.key.name.clone(),
                        document: m.key.document.clone(),
                        scope: m.scope.clone(),
                        reason,
                    };
                    // Reported once in the log (the `is FAILED:` line
                    // above) and once in the Results; not echoed here.
                    outcome.failure.get_or_insert(failure);
                    m.state = State::Failed;
                }
                Outcome::Stopped => {
                    let mut inst = inst;
                    end_instance(&mut inst).await;
                    m.state = State::Idle;
                    m.active = false;
                }
                Outcome::Regate { relaunches } => {
                    // The old Session has ended; restart from idle once the
                    // Services it lists are READY (constraint 2), carrying
                    // the consumed attempts. `start_pending`, run next in
                    // this gate loop, waits for the dependencies and
                    // re-seeds the new Session's in-scope endpoints. The
                    // restart opens a NEW Session (new ports), so flag it so
                    // this Service's own dependents are restarted when it is
                    // READY again (constraint 8).
                    m.pending_relaunches = relaunches;
                    m.pending_new_session = true;
                    m.state = State::Idle;
                }
            }
        }
        Ok(replaced)
    }

    /// RFC 0009 "Dependents of a relaunched Service": every Service that
    /// lists one of `replaced` — a Service that began a new Service Session
    /// — directly or transitively, and has a Session (READY, or starting),
    /// holds endpoint values that are no longer valid. Each is stopped, in
    /// the order of constraint 4 (dependents before the Services they
    /// list), and returned to idle still active, so the gate starts it
    /// again in a new Service Session once every Service it lists is READY.
    /// This consumes none of the dependent's relaunch attempts, and its
    /// `completedTasks` applies to its own scope: under `RERUN` — or when
    /// it gave none — the completed Tasks of its scope are recorded on
    /// `outcome` to return to the queue; under `KEEP` they stand.
    async fn restart_dependents(&mut self, replaced: &[ServiceKey], outcome: &mut GateOutcome) {
        if replaced.is_empty() {
            return;
        }
        // The dependents, each with the Service it lists whose Session was
        // replaced (or, transitively, the stale Service it lists).
        let mut stale: BTreeMap<ServiceKey, ServiceKey> = BTreeMap::new();
        loop {
            let before = stale.len();
            for m in &self.services {
                if replaced.contains(&m.key) || stale.contains_key(&m.key) {
                    continue;
                }
                // A dependent that has a Session is READY here: both callers
                // run `restart_dependents` straight after `await_busy`, which
                // has drained every `Busy`.
                if !matches!(m.state, State::Ready(_)) {
                    continue;
                }
                let cause = m
                    .depends_on_services
                    .iter()
                    .find(|d| replaced.contains(d))
                    .or_else(|| m.depends_on_services.iter().find(|d| stale.contains_key(d)));
                if let Some(cause) = cause {
                    stale.insert(m.key.clone(), cause.clone());
                }
            }
            if stale.len() == before {
                break;
            }
        }
        if stale.is_empty() {
            return;
        }
        let keys: BTreeSet<ServiceKey> = stale.keys().cloned().collect();
        for idx in self.stop_order(&keys) {
            let shared = self.shared.clone();
            let m = &mut self.services[idx];
            let cause = &stale[&m.key];
            let policy = m.policy.unwrap_or(CompletedTasksPolicy::Rerun);
            log_line(format!(
                "{} {} is stopping: {cause} began a new Service Session ({}; no relaunch \
                 attempt consumed)",
                m.key,
                scope_label(&m.scope),
                describe_dependent_restart_policy(m.policy)
            ));
            if policy == CompletedTasksPolicy::Rerun {
                outcome.rerun = Some(merge_scopes(outcome.rerun.take(), &m.scope));
            }
            m.stop.cancel();
            // READY by the filter above; its Session is ended and the
            // Service returns to idle, to be started again once `cause` is
            // READY (`start_pending`).
            if let State::Ready(mut inst) = std::mem::replace(&mut m.state, State::Idle) {
                end_instance(&mut inst).await;
            }
            m.stop = shared.config.cancel_token.child_token();
            m.restart_after = Some(cause.clone());
        }
    }

    /// Start every active idle Service, in waves of Services whose Service
    /// dependencies are all READY (constraint 2). Each wave starts
    /// concurrently; the next begins when the wave has settled.
    async fn start_pending(&mut self, outcome: &mut GateOutcome) -> Result<(), RunError> {
        // The run is stopping: start nothing (and do not log a restart for a
        // Service that will not start). `gate` breaks on the same condition;
        // `stop_all` tears the Services down.
        if self.shared.config.cancel_token.is_cancelled() {
            return Ok(());
        }
        loop {
            let ready: BTreeMap<ServiceKey, ServiceEndpoints> = self
                .services
                .iter()
                .filter_map(|m| match &m.state {
                    State::Ready(inst) => inst.endpoints.clone().map(|e| (m.key.clone(), e)),
                    _ => None,
                })
                .collect();
            let known: BTreeSet<ServiceKey> = self.services.iter().map(|m| m.key.clone()).collect();
            let mut spawned = false;
            let mut blocked = Vec::new();
            for idx in 0..self.services.len() {
                let m = &self.services[idx];
                if !m.active || !matches!(m.state, State::Idle) {
                    continue;
                }
                let waiting_on: Vec<&ServiceKey> = m
                    .depends_on_services
                    .iter()
                    .filter(|r| known.contains(*r) && !ready.contains_key(*r))
                    .collect();
                if !waiting_on.is_empty() {
                    blocked.push(m.key.to_string());
                    continue;
                }
                // Constraint 2: the Services it depends on are READY; the
                // endpoints of those it lists seed its own actions, and those
                // of the Services each of its Environments lists seed that
                // Environment.
                let in_scope: Vec<ServiceEndpoints> = m
                    .listed_services
                    .iter()
                    .filter_map(|r| ready.get(r).cloned())
                    .collect();
                let environment_endpoints: Vec<Vec<ServiceEndpoints>> = m
                    .environment_listed
                    .iter()
                    .map(|keys| keys.iter().filter_map(|r| ready.get(r).cloned()).collect())
                    .collect();
                let retain = self.shared.config.retain_working_dir;
                let m = &mut self.services[idx];
                if m.stop.is_cancelled() {
                    m.stop = self.shared.config.cancel_token.child_token();
                }
                if let Some(cause) = m.restart_after.take() {
                    log_line(format!(
                        "{} {} is starting again in a new Service Session: {cause} is READY",
                        m.key,
                        scope_label(&m.scope)
                    ));
                }
                let mut inst = m.new_instance(retain);
                // Consumed once; a later start of this Service (e.g. a RERUN
                // re-activation) begins with a fresh counter.
                m.pending_relaunches = 0;
                inst.in_scope = in_scope;
                inst.environment_endpoints = environment_endpoints;
                m.state = State::Busy(self.shared.spawn_background(start_or_recover(
                    inst,
                    None,
                    self.shared.clone(),
                    m.stop.clone(),
                )));
                spawned = true;
            }
            if !spawned {
                if !blocked.is_empty()
                    && !self.any_busy()
                    && !self.shared.config.cancel_token.is_cancelled()
                {
                    return Err(format!(
                        "cannot order the start of Services {}: each depends on a Service that \
                         is not READY and is not starting",
                        blocked.join(", ")
                    )
                    .into());
                }
                return Ok(());
            }
            let replaced = self.await_busy(outcome).await?;
            if outcome.failure.is_some() {
                return Ok(());
            }
            self.restart_dependents(&replaced, outcome).await;
        }
    }
}

/// True when the Job Template declares an inline Service named `name` (its
/// own `Service.<name>.*` references mean that one; an attached Service of
/// the same name is a different Service, Template Schemas §1.2.2 item 3).
fn service_declared(job: &Job, name: &str) -> bool {
    job.services
        .iter()
        .flatten()
        .any(|s| s.document.is_job_template() && s.name == name)
}

fn policy_name(policy: CompletedTasksPolicy) -> &'static str {
    match policy {
        CompletedTasksPolicy::Keep => "KEEP",
        CompletedTasksPolicy::Rerun => "RERUN",
    }
}

/// `completedTasks: KEEP`, or, for a Service that gave none (so its
/// `maxRestarts` is 0 and a failure fails the scope), `completedTasks:
/// none; maxRestarts 0`.
fn describe_policy(policy: Option<CompletedTasksPolicy>) -> String {
    match policy {
        Some(p) => format!("completedTasks: {}", policy_name(p)),
        None => "completedTasks: none; maxRestarts 0, so the failure fails the scope".to_string(),
    }
}

/// How a dependent's `completedTasks` reads when it is stopped and started
/// again because a Service it lists began a new Service Session: as given,
/// or `RERUN` when none was.
fn describe_dependent_restart_policy(policy: Option<CompletedTasksPolicy>) -> String {
    match policy {
        Some(p) => format!("completedTasks: {}", policy_name(p)),
        None => "completedTasks omitted, read as RERUN".to_string(),
    }
}

fn driver_lost_exit() -> ServiceRunExit {
    ServiceRunExit {
        state: openjd_sessions::ActionState::Failed,
        exit_code: None,
        canceled: false,
        unhealthy: None,
        reported_failure: None,
        fail_message: Some("onRun driver ended without reporting an exit".into()),
        stdout: String::new(),
    }
}

/// End `m`'s Session, if one is live, awaiting (and thereby cutting short,
/// through its stop token) any background start or relaunch first, and
/// return the Service to idle. Returns the reason when the background task
/// had already decided FAILED: that verdict stands (the failure was observed
/// while the scope had work) and the Service is left FAILED.
async fn stop_managed(m: &mut Managed, shared: &Shared) -> Result<Option<String>, RunError> {
    let mut failed = None;
    // A stopped Service's dependents are stopped with it (or already were).
    m.pending_replaced = false;
    match std::mem::replace(&mut m.state, State::Idle) {
        State::Busy(handle) => {
            let id = handle.id();
            let joined = handle.await;
            shared.forget_background(id);
            let (mut inst, outcome) =
                joined.map_err(|e| format!("{}: background task failed: {e}", m.key))?;
            match outcome {
                Outcome::Failed(reason) => {
                    // `start_or_recover` ended the Session before returning.
                    failed = Some(reason);
                    m.state = State::Failed;
                }
                Outcome::Ready { .. } | Outcome::Stopped | Outcome::Regate { .. } => {
                    end_instance(&mut inst).await
                }
            }
        }
        State::Ready(mut inst) => end_instance(&mut inst).await,
        State::Idle | State::Failed => {}
    }
    m.active = false;
    m.pending_relaunches = 0;
    m.pending_new_session = false;
    m.stop = shared.config.cancel_token.child_token();
    Ok(failed)
}

/// Constraint 7: end the Service Session, if one is live.
async fn end_instance(inst: &mut Instance) {
    let Some(mut session) = inst.session.take() else {
        return;
    };
    log_banner(&format!(
        "Stopping Service: {}{}",
        inst.service.name,
        origin_suffix(&inst.service.document)
    ));
    let working_dir = session.session().working_directory().to_path_buf();
    if let Err(e) = session.end().await {
        eprintln!("ERROR: {} teardown reported an error: {e}", inst.label);
    }
    let mut msg = format!("{} stopped", inst.label);
    if inst.retain_working_dir {
        msg.push_str(&format!(
            "; working directory preserved at: {}",
            working_dir.display()
        ));
    }
    log_line(msg);
}

/// The Session configuration of `service`'s Service Session. Its profile is
/// that of the Service's own document: an external Service's actions,
/// `variables`, `let` bindings and embedded files are evaluated under its
/// Environment Template's extensions, a Job
/// Template Service's under the Job Template's. Its `log_tag` — `Service
/// <name>`, plus `(from <document>)` for an external Service — prefixes
/// every line the Session logs (`[Service Files] …`), so a Service's output
/// stays attributable once it interleaves with Task output in the single
/// run log, and its section banners (`Entering Environment: …`, `Service
/// onEnter: …`) appear as single tagged lines.
fn session_config(shared: &Shared, service: &Service) -> SessionConfig {
    let n = shared.session_counter.fetch_add(1, Ordering::SeqCst);
    SessionConfig {
        session_id: format!("cli-{}-svc-{}-{n}", std::process::id(), service.name),
        job_parameter_values: shared.config.job_parameter_values.clone(),
        path_mapping_rules: shared.config.path_mapping_rules.clone(),
        retain_working_dir: shared.config.retain_working_dir,
        callback: None,
        os_env_vars: None,
        session_root_directory: None,
        user: None,
        profile: Some(shared.config.profile_for(&service.document).clone()),
        cancel_token: None,
        sticky_bit_policy: Default::default(),
        debug_collect_stdout: false,
        echo_openjd_directives: true,
        log_tag: Some(format!(
            "Service {}{}",
            service.name,
            origin_suffix(&service.document)
        )),
        limits: shared.config.limits,
    }
}

/// `Job Environment(s): A, B` or `no Job Environment`: which of the Job's
/// Environments a Service Session enters (RFC 0009 "Services run inside
/// Environments"), for the run log.
fn describe_entered_environments(environments: &[Environment]) -> String {
    if environments.is_empty() {
        return "no Job Environment".to_string();
    }
    let names: Vec<&str> = environments.iter().map(|e| e.name.as_str()).collect();
    format!("Job Environment(s): {}", names.join(", "))
}

/// The Environments `inst`'s Session enters, each with the `Service.*`
/// symbols of the READY Services it lists layered onto its symbol table (or
/// onto `base`, the submission's `Param.*` table, when it has none), as a
/// Task Session enters them: an Environment's format strings may reference
/// the Services it lists (RFC 0009 "The `Service.*` scope" item 4), and the
/// Service waited for them to be READY before starting.
fn environments_with_endpoints(
    inst: &Instance,
    base: &SerializedSymbolTable,
) -> Result<Vec<Environment>, String> {
    inst.environments
        .iter()
        .enumerate()
        .map(|(i, env)| {
            let endpoints = inst
                .environment_endpoints
                .get(i)
                .map_or(&[][..], Vec::as_slice);
            if endpoints.is_empty() {
                return Ok(env.clone());
            }
            let table = build_service_symbol_table(endpoints, None)
                .map_err(|e| format!("Failed to build the Service.* symbol table: {e}"))?;
            let mut entries = env
                .resolved_symtab
                .as_ref()
                .unwrap_or(base)
                .as_value()
                .as_array()
                .cloned()
                .unwrap_or_default();
            entries.extend(
                SerializedSymbolTable::from_symtab(&table)
                    .as_value()
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
            );
            let mut env = env.clone();
            env.resolved_symtab = Some(SerializedSymbolTable::from_value(
                serde_json::Value::Array(entries),
            ));
            Ok(env)
        })
        .collect()
}

/// Start the Service (when `first` is `None`) or apply the restart decision
/// to `first` and relaunch, until it is READY, FAILED, or the Service is
/// stopping. Runs in a background task so Services start concurrently and a
/// `KEEP` relaunch proceeds while Tasks continue.
async fn start_or_recover(
    mut inst: Instance,
    first: Option<FailureKind>,
    shared: Arc<Shared>,
    stop: CancellationToken,
) -> (Instance, Outcome) {
    let mut kind = first;
    let mut replaced = false;
    loop {
        if stop.is_cancelled() {
            return (inst, Outcome::Stopped);
        }
        if let Some(k) = kind.take() {
            // Step 2: an UNHEALTHY instance's onRun was canceled by the
            // runtime; its exit is awaited — and logged — before the
            // restart decision, so the log shows the old instance gone
            // before the relaunch or FAILED line (constraint 5).
            if let Some(session) = inst.session.as_mut() {
                if session.state() == ServiceSessionState::Running {
                    let launch = session.launch_count();
                    if let Ok(exit) = session.wait_exit().await {
                        log_run_exit(&inst.label, launch, &exit);
                    }
                }
            }
            // RFC 0009 "Failure and restart" step 3/4.
            let max_restarts = inst.service.restart_policy.max_restarts;
            if inst.relaunches >= max_restarts {
                let reason = format!(
                    "{}; {} of {} relaunch(es) used (restartPolicy.maxRestarts)",
                    k.describe(),
                    inst.relaunches,
                    max_restarts
                );
                log_line(format!(
                    "{} {} is FAILED: {reason}",
                    inst.label, inst.scope_label
                ));
                end_instance(&mut inst).await;
                return (inst, Outcome::Failed(reason));
            }
            inst.relaunches += 1;
            if k.requires_new_session() {
                replaced = true;
                log_line(format!(
                    "Relaunching {} in a new Service Session (relaunch {} of {}): {}",
                    inst.label,
                    inst.relaunches,
                    max_restarts,
                    k.describe()
                ));
                let relaunches = inst.relaunches;
                end_instance(&mut inst).await;
                // A Service that lists other Services must start only after
                // they are READY (constraint 2) and with their current
                // endpoints. This background task cannot see their state, so
                // hand the new Session back to the gate: it restarts the
                // Service from idle once its dependencies are READY,
                // re-seeding `in_scope`. (A Service with no dependencies has
                // nothing to wait for and relaunches here as before.)
                if inst.has_service_dependencies {
                    return (inst, Outcome::Regate { relaunches });
                }
            } else {
                log_line(format!(
                    "Relaunching {} onRun in its Service Session (relaunch {} of {}): {}",
                    inst.label,
                    inst.relaunches,
                    max_restarts,
                    k.describe()
                ));
            }
        }

        match launch_until_ready(&mut inst, &shared, &stop).await {
            Ok(true) => return (inst, Outcome::Ready { replaced }),
            Ok(false) => return (inst, Outcome::Stopped),
            Err(k) => {
                if stop.is_cancelled() {
                    return (inst, Outcome::Stopped);
                }
                log_line(format!(
                    "{} {} is UNREADY: {} ({})",
                    inst.label,
                    inst.scope_label,
                    k.describe(),
                    describe_policy(inst.service.restart_policy.completed_tasks)
                ));
                kind = Some(k);
            }
        }
    }
}

/// Open a Service Session if none is live (allocate endpoints, enter), then
/// launch `onRun` and wait for the readiness verdict. `Ok(true)`: READY.
/// `Ok(false)`: the Service is stopping. `Err`: the failure to decide on.
async fn launch_until_ready(
    inst: &mut Instance,
    shared: &Shared,
    stop: &CancellationToken,
) -> Result<bool, FailureKind> {
    if inst.session.is_none() {
        let endpoints = shared
            .ports
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .allocate(&inst.service)
            .map_err(FailureKind::Start)?;
        log_banner(&format!(
            "Starting Service: {}{}",
            inst.service.name,
            origin_suffix(&inst.service.document)
        ));
        log_line(format!(
            "{} {} endpoints: {}",
            inst.label,
            inst.scope_label,
            describe_endpoints(&endpoints)
        ));
        log_line(format!(
            "{} Session enters {}",
            inst.label,
            describe_entered_environments(&inst.environments)
        ));
        inst.endpoints = Some(endpoints.clone());
        let environments = environments_with_endpoints(inst, &shared.config.base_symtab)
            .map_err(FailureKind::Start)?;
        let mut session = ServiceSession::with_config(ServiceSessionConfig {
            session: session_config(shared, &inst.service),
            service: inst.service.clone(),
            environments,
            environment_profiles: inst.environment_profiles.clone(),
            environment_parameters: inst.environment_parameters.clone(),
            endpoints,
            in_scope_endpoints: inst.in_scope.clone(),
        })
        .map_err(|e| FailureKind::Start(e.to_string()))?;
        log_line(format!(
            "{} working directory: {}",
            inst.label,
            session.session().working_directory().display()
        ));
        // Interruption while an Environment's onEnter or the Service's
        // onEnter runs: cancel it with its own cancelation method and let
        // enter() report the (start) failure; the stop token then ends
        // the attempt without consuming a relaunch.
        let cancel = session.cancel_handle();
        let entered = {
            let enter = session.enter();
            tokio::pin!(enter);
            tokio::select! {
                r = &mut enter => r,
                _ = stop.cancelled() => {
                    cancel.cancel(None, false);
                    enter.await
                }
            }
        };
        inst.session = Some(session);
        entered.map_err(|e| FailureKind::Start(e.to_string()))?;
    }
    let label = inst.label.clone();
    let session = inst
        .session
        .as_mut()
        .expect("a Service Session is live at this point");
    if session.state() == ServiceSessionState::Running {
        // The exit was observed on the watch channel — or, for an UNHEALTHY
        // instance, is on its way: the Session canceled onRun and the
        // restart decision waits for it (RFC 0009 "Failure and restart"
        // step 2, constraint 5). The Session moves to EXITED (and reclaims
        // its driver) when the exit is awaited here.
        let launch = session.launch_count();
        if let Ok(exit) = session.wait_exit().await {
            log_run_exit(&label, launch, &exit);
        }
    }
    if stop.is_cancelled() {
        return Ok(false);
    }
    session
        .launch()
        .await
        .map_err(|e| FailureKind::Start(format!("could not launch onRun: {e}")))?;
    log_line(format!(
        "{} onRun launched (launch {} in this Session); health check: {}",
        inst.label,
        session.launch_count(),
        describe_health_check(&inst.service.health_check)
    ));
    let health = tokio::select! {
        r = session.wait_ready() => r,
        _ = stop.cancelled() => return Ok(false),
    };
    match health {
        Ok(ServiceHealth::Ready { message, .. }) => {
            let suffix = message.map(|m| format!(": {m}")).unwrap_or_default();
            log_line(format!("{} is READY{suffix}", inst.label));
            Ok(true)
        }
        Ok(ServiceHealth::TimedOut) => {
            // RFC 0009 "Failure and restart" step 2: cancel onRun and wait
            // for it to exit before deciding.
            let launch = session.launch_count();
            session.cancel_run(None);
            if let Ok(exit) = session.wait_exit().await {
                log_run_exit(&label, launch, &exit);
            }
            Err(FailureKind::ReadyTimedOut)
        }
        Ok(ServiceHealth::Unhealthy(u)) => {
            // READY and UNHEALTHY before this task observed either: the
            // Session has canceled onRun; wait for it (step 2).
            log_line(format!(
                "{} {} is UNHEALTHY: {u}",
                inst.label, inst.scope_label
            ));
            let launch = session.launch_count();
            if let Ok(exit) = session.wait_exit().await {
                log_run_exit(&label, launch, &exit);
            }
            Err(FailureKind::Unhealthy(u))
        }
        Ok(ServiceHealth::Failed { message }) => {
            // onRun said so itself, before READY: the Session has canceled
            // it; wait for the exit (step 2). Not a suspected port conflict,
            // so the relaunch stays in the Session.
            let launch = session.launch_count();
            if let Ok(exit) = session.wait_exit().await {
                log_run_exit(&label, launch, &exit);
            }
            Err(FailureKind::Reported(message))
        }
        Ok(ServiceHealth::ExitedBeforeReady | ServiceHealth::Pending) => {
            let exit = session
                .wait_exit()
                .await
                .unwrap_or_else(|_| driver_lost_exit());
            if exit.canceled {
                Ok(false)
            } else {
                // A reported failure never reaches this arm: `Failed` is
                // published before the exit and is terminal, so `wait_ready`
                // returned it above.
                Err(FailureKind::ExitedBeforeReady(Box::new(exit)))
            }
        }
        Err(e) => Err(FailureKind::Start(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A manager with the one Service `X` of a minimal Job Template
    /// registered (scope: Step A), started by nothing.
    fn manager_with_one_service() -> ServiceManager {
        let template = serde_json::json!({
            "specificationVersion": "jobtemplate-2023-09",
            "extensions": ["SERVICE", "ENV_PARAMS", "EXPR"],
            "name": "J",
            "services": [{
                "name": "X",
                "ports": [{"name": "main"}],
                "healthCheck": {"type": "STDOUT"},
                "script": {"actions": {"onRun": {"command": "run"}}}
            }],
            "steps": [{
                "name": "A",
                "dependencies": [{"service": "X"}],
                "script": {"actions": {"onRun": {"command": "run"}}}
            }]
        });
        let jt = openjd_model::decode_job_template(
            template,
            Some(&["SERVICE", "ENV_PARAMS", "EXPR"]),
            &openjd_model::CallerLimits::default(),
        )
        .unwrap();
        let job = openjd_model::create_job(
            &jt,
            &JobParameterValues::new(),
            &jt.default_validation_context(),
        )
        .unwrap();
        let mut manager = ServiceManager::new(ServiceRunConfig {
            job_parameter_values: JobParameterValues::new(),
            path_mapping_rules: None,
            retain_working_dir: false,
            profile: ModelProfile::default(),
            attached_profiles: Vec::new(),
            requirement_bindings: Vec::new(),
            base_symtab: SerializedSymbolTable::from_symtab(&openjd_expr::SymbolTable::new()),
            cancel_token: CancellationToken::new(),
            limits: Default::default(),
        });
        manager.register(&job, &[]);
        manager
    }

    /// The in-Task watcher can be woken by `settled` before the background
    /// task that signaled it has finished. A task whose future has returned
    /// (recorded in `settled_tasks`) is taken as finished, so its FAILED
    /// verdict is reported now rather than waiting for a second signal
    /// that never comes.
    #[tokio::test]
    async fn a_settled_background_task_not_yet_finished_is_promoted() {
        let mut manager = manager_with_one_service();
        let inst = manager.services[0].new_instance(false);
        let (release, wait) = tokio::sync::oneshot::channel::<()>();
        let handle: Background = tokio::spawn(async move {
            let _ = wait.await;
            (inst, Outcome::Failed("injected".into()))
        });
        manager
            .shared
            .settled_tasks
            .lock()
            .unwrap()
            .insert(handle.id());
        assert!(!handle.is_finished());
        manager.services[0].state = State::Busy(handle);
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            let _ = release.send(());
        });
        assert_eq!(
            manager.promote_finished_relaunches().await,
            Some(ServiceScope::steps(["A"]))
        );
        assert!(manager.shared.settled_tasks.lock().unwrap().is_empty());
    }

    /// A background start or relaunch that is still running is neither
    /// settled nor finished: the re-scan leaves it `Busy` and returns at
    /// once, so the in-Task watcher goes on watching the READY Services
    /// instead of waiting for it.
    #[tokio::test]
    async fn a_running_background_task_is_left_busy() {
        use futures_util::FutureExt;
        let mut manager = manager_with_one_service();
        let inst = manager.services[0].new_instance(false);
        let (_release, wait) = tokio::sync::oneshot::channel::<()>();
        manager.services[0].state = State::Busy(tokio::spawn(async move {
            let _ = wait.await;
            (inst, Outcome::Failed("injected".into()))
        }));
        assert_eq!(
            manager.promote_finished_relaunches().now_or_never(),
            Some(None)
        );
        assert!(matches!(manager.services[0].state, State::Busy(_)));
        assert!(!manager.services[0].failed_noticed);
    }

    /// A background task that panicked (an internal error) is reported as
    /// a FAILED Service by the in-Task watcher, once, and left for the next
    /// gate as a FAILED outcome rather than a lost instance.
    #[tokio::test]
    async fn a_panicked_background_task_is_promoted_as_failed() {
        let mut manager = manager_with_one_service();
        let handle: Background = tokio::spawn(async { panic!("injected") });
        while !handle.is_finished() {
            tokio::task::yield_now().await;
        }
        manager.services[0].state = State::Busy(handle);
        assert_eq!(
            manager.promote_finished_relaunches().await,
            Some(ServiceScope::steps(["A"]))
        );
        assert!(manager.services[0].failed_noticed);
        // Reported once: a second pass leaves the FAILED outcome in place.
        while matches!(&manager.services[0].state, State::Busy(h) if !h.is_finished()) {
            tokio::task::yield_now().await;
        }
        assert_eq!(manager.promote_finished_relaunches().await, None);
        let State::Busy(handle) = std::mem::replace(&mut manager.services[0].state, State::Idle)
        else {
            panic!("the FAILED outcome is held for the next gate");
        };
        let (_, outcome) = handle.await.unwrap();
        let Outcome::Failed(reason) = outcome else {
            panic!("expected a FAILED outcome");
        };
        assert!(
            reason.starts_with("background task failed: ") && reason.contains("panicked"),
            "{reason}"
        );
    }

    #[test]
    fn merge_scopes_unions_steps_and_absorbs_into_all() {
        let a = ServiceScope::steps(["A"]);
        let b = ServiceScope::steps(["B"]);
        assert_eq!(merge_scopes(None, &a), a);
        assert_eq!(
            merge_scopes(Some(a.clone()), &b),
            ServiceScope::steps(["A", "B"])
        );
        assert_eq!(
            merge_scopes(Some(a.clone()), &ServiceScope::AllSteps),
            ServiceScope::AllSteps
        );
        assert_eq!(
            merge_scopes(Some(ServiceScope::AllSteps), &b),
            ServiceScope::AllSteps
        );
        assert_eq!(scope_label(&ServiceScope::AllSteps), "(scope: every Step)");
        assert_eq!(
            scope_label(&ServiceScope::steps(["B", "A"])),
            "(scope: Steps A, B)"
        );
    }

    #[test]
    fn failure_kind_new_session_policy_and_descriptions() {
        let exit = ServiceRunExit {
            state: openjd_sessions::ActionState::Failed,
            exit_code: Some(3),
            canceled: false,
            unhealthy: None,
            reported_failure: None,
            fail_message: Some("boom".into()),
            stdout: String::new(),
        };
        let start = FailureKind::Start("port 1 unavailable".into());
        let before = FailureKind::ExitedBeforeReady(Box::new(exit.clone()));
        let timed_out = FailureKind::ReadyTimedOut;
        let exited = FailureKind::Exited(Box::new(exit.clone()));
        let unhealthy = FailureKind::Unhealthy(ServiceUnhealthy {
            failed_probes: 2,
            failure_threshold: 2,
            last_failure: "onHealthCheck exit code: 1".into(),
        });
        assert_eq!(
            describe_policy(Some(CompletedTasksPolicy::Keep)),
            "completedTasks: KEEP"
        );
        assert_eq!(
            describe_policy(None),
            "completedTasks: none; maxRestarts 0, so the failure fails the scope"
        );
        assert_eq!(
            describe_dependent_restart_policy(Some(CompletedTasksPolicy::Rerun)),
            "completedTasks: RERUN"
        );
        assert_eq!(
            describe_dependent_restart_policy(None),
            "completedTasks omitted, read as RERUN"
        );
        assert!(start.requires_new_session());
        assert!(before.requires_new_session());
        assert!(!timed_out.requires_new_session());
        assert!(!exited.requires_new_session());
        assert!(!unhealthy.requires_new_session());
        assert_eq!(start.describe(), "failed to start: port 1 unavailable");
        assert_eq!(
            before.describe(),
            "onRun exited before becoming READY (exit code: 3; boom)"
        );
        assert_eq!(
            timed_out.describe(),
            "did not become READY within readinessTimeoutSeconds"
        );
        assert_eq!(
            exited.describe(),
            "onRun exited while the scope still had work (exit code: 3; boom)"
        );
        assert_eq!(
            unhealthy.describe(),
            "instance UNHEALTHY: 2 consecutive health probes failed (failureThreshold: 2); last \
             probe: onHealthCheck exit code: 1"
        );
        // An exit the health check forced is classified UNHEALTHY; the
        // runner's Canceled state does not make it a caller cancelation.
        let forced = ServiceRunExit {
            state: openjd_sessions::ActionState::Canceled,
            exit_code: None,
            unhealthy: Some(ServiceUnhealthy {
                failed_probes: 3,
                failure_threshold: 3,
                last_failure: "no openjd_service_ready line within 5s".into(),
            }),
            fail_message: None,
            ..exit.clone()
        };
        assert!(matches!(
            InstanceFailure::from_exit(forced).into_kind(),
            FailureKind::Unhealthy(u) if u.failed_probes == 3
        ));
        // An exit the runtime forced because onRun reported openjd_fail is
        // a reported failure: relaunched in the same Session, the message
        // is the reason.
        let reported = ServiceRunExit {
            state: openjd_sessions::ActionState::Canceled,
            exit_code: None,
            reported_failure: Some("store corrupt".into()),
            fail_message: Some("store corrupt".into()),
            ..exit
        };
        let kind = InstanceFailure::from_exit(reported).into_kind();
        assert!(matches!(&kind, FailureKind::Reported(m) if m == "store corrupt"));
        assert!(!kind.requires_new_session());
        assert_eq!(kind.describe(), "onRun reported openjd_fail: store corrupt");
        assert_eq!(
            describe_exit(&driver_lost_exit()),
            "failed; onRun driver ended without reporting an exit"
        );
    }

    #[test]
    fn service_failure_display_names_scope() {
        let f = ServiceFailure {
            name: "Cache".into(),
            document: Document::JobTemplate,
            scope: ServiceScope::steps(["Render"]),
            reason: "did not become READY within readinessTimeoutSeconds; 1 of 1 relaunch(es) used"
                .into(),
        };
        assert_eq!(
            f.to_string(),
            "Service 'Cache' (scope: Step Render) failed: did not become READY within \
             readinessTimeoutSeconds; 1 of 1 relaunch(es) used"
        );

        // An external Service names its document.
        let f = ServiceFailure {
            document: Document::environment_template(0, Some("queue-cache.yaml")),
            scope: ServiceScope::AllSteps,
            ..f
        };
        assert_eq!(
            f.to_string(),
            "Service 'Cache' (from queue-cache.yaml) (scope: every Step) failed: did not become \
             READY within readinessTimeoutSeconds; 1 of 1 relaunch(es) used"
        );
    }

    #[test]
    fn service_keys_distinguish_documents_not_names() {
        let own = ServiceKey {
            document: Document::JobTemplate,
            name: "Cache".into(),
        };
        let external = ServiceKey {
            document: Document::environment_template(1, None),
            name: "Cache".into(),
        };
        assert_ne!(own, external);
        assert_eq!(own.to_string(), "Service 'Cache'");
        assert_eq!(
            external.to_string(),
            "Service 'Cache' (from EnvironmentTemplate[1])"
        );
        // A `service:` dependency written inside the external Service stays
        // in its document.
        assert_eq!(
            ServiceKey::sibling(&external.document, "Store"),
            ServiceKey {
                document: external.document.clone(),
                name: "Store".into()
            }
        );
        // A requirement resolves to the attached Service it was bound to.
        let binding = RequirementBinding {
            requirement: "Cache".into(),
            document: external.document.clone(),
            service: "Cache".into(),
        };
        assert_eq!(ServiceKey::of_binding(&binding), external);
        assert_eq!(origin_suffix(&Document::JobTemplate), "");
    }
}
