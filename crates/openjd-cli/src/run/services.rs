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
//! (ordering constraints 2 and 10), gates Tasks on readiness (constraint
//! 3), watches for instance failures while Tasks run — an `onRun` exit, or
//! an UNHEALTHY verdict from the Session's health check — and applies the
//! restart policy ("Failure and restart"), and stops Services when their
//! scope completes (constraints 4, 6, 7). The Service Sessions themselves
//! are `openjd_sessions::ServiceSession`s.
//!
//! Services are keyed by [`ServiceKey`] — the document that declares a
//! Service plus its name (Template Schemas §1.2.2 item 2: names are scoped
//! to their document, so an external Service may be named like one of the
//! Job Template's). A Service Session is seeded with the `Service.*`
//! endpoints of its own document only, and a `Service.*` reference inside a
//! Service resolves to a Service of the same document.
//!
//! See `specs/cli/run.md` § Services for the orchestration rules.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use openjd_model::job::service_symbols::{referenced_service_names, ServiceEndpoints};
use openjd_model::job::{CompletedTasksPolicy, Document, Environment, Job, Service, Step};
use openjd_model::types::{JobParameterValues, ModelProfile};
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

/// Which scope a Service belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ServiceScope {
    /// A Job Service (`jobServices`, including external Services from
    /// `--environment` templates): the scope is the whole Job.
    Job,
    /// A Step Service (`stepServices`) of the named Step.
    Step(String),
}

impl std::fmt::Display for ServiceScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Job => write!(f, "Job"),
            Self::Step(name) => write!(f, "Step '{name}'"),
        }
    }
}

/// What identifies one Service of the combined Job: the document that
/// declares it and its name (Template Schemas §1.2.2 item 2). Two Services
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
    /// `Service.<name>.*` reference never crosses a document boundary.
    fn sibling(document: &Document, name: &str) -> Self {
        Self {
            document: document.clone(),
            name: name.to_string(),
        }
    }

    /// `(from <doc>)` for an external Service, nothing for the Job
    /// Template's own — what disambiguates same-named Services in the log.
    pub(super) fn origin_suffix(&self) -> String {
        origin_suffix(&self.document)
    }
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
            "{} ({} scope) failed: {}",
            self.key(),
            self.scope,
            self.reason
        )
    }
}

/// How far a `completedTasks: RERUN` relaunch reaches: the Step's Tasks
/// (a Step Service) or every Task of the Job (a Job Service).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum RerunScope {
    Step,
    Job,
}

/// The run-wide inputs every Service Session shares with the Task Session.
pub(super) struct ServiceRunConfig {
    pub job_parameter_values: JobParameterValues,
    pub path_mapping_rules: Option<Vec<PathMappingRule>>,
    pub retain_working_dir: bool,
    /// The Job Template's profile: the profile of its `jobServices` /
    /// `stepServices` and of its own Environments.
    pub profile: ModelProfile,
    /// The `--environment` templates' profiles, by attachment index. An
    /// external Service's Session runs under its own template's profile,
    /// and every Environment a Service Session enters is evaluated under
    /// the profile of the document that declares it (Template Schemas §1.2
    /// item 3).
    pub attached_profiles: Vec<ModelProfile>,
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
}

/// What the readiness gate found.
#[derive(Debug, Default)]
pub(super) struct GateOutcome {
    /// A Service became FAILED; its scope fails.
    pub failure: Option<ServiceFailure>,
    /// A Service with `completedTasks: RERUN` failed since the last gate, so
    /// the completed Tasks of its scope return to the queue.
    pub rerun: Option<RerunScope>,
    /// A Job Service began a new Service Session (new endpoints) since the
    /// last gate: Environments the Task Session entered may hold stale
    /// `Service.*` values.
    pub job_endpoints_changed: bool,
    /// As above, for a Step Service of the current Step.
    pub step_endpoints_changed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    Job,
    Step,
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
    ExitedBeforeReady(ServiceRunExit),
    /// `readinessTimeoutSeconds` elapsed while `onRun` kept running (it has
    /// been canceled and has exited by the time this is reported).
    ReadyTimedOut,
    /// `onRun` exited after READY while the scope still had work.
    Exited(ServiceRunExit),
    /// The instance became UNHEALTHY: `failureThreshold` consecutive health
    /// probes failed after READY. The runtime has canceled `onRun`; it has
    /// exited by the time the restart decision is taken.
    Unhealthy(ServiceUnhealthy),
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
        }
    }
}

/// `<TYPE> (readinessTimeoutSeconds N[, readinessIntervalSeconds N][, healthIntervalSeconds N, failureThreshold N])`
/// — the effective health check, for the launch log line.
fn describe_health_check(check: &openjd_model::job::ServiceHealthCheck) -> String {
    let mut s = format!(
        "{} (readinessTimeoutSeconds {}",
        check.type_name(),
        check.readiness_timeout_seconds()
    );
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

fn describe_exit(exit: &ServiceRunExit) -> String {
    let mut s = match exit.exit_code {
        Some(code) => format!("exit code: {code}"),
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
    scope: ServiceScope,
    /// `Service 'X'` or `Service 'X' (from <doc>)`, for every log line.
    label: String,
    /// The Environments the Service Session enters (those whose `runScope`
    /// includes `SERVICE`), in entry order.
    environments: Vec<Environment>,
    /// The profile of each entry of `environments` whose document is not
    /// the Service's own (`None` for those that share it), index for index
    /// — `ServiceSessionConfig::environment_profiles`.
    environment_profiles: Vec<Option<ModelProfile>>,
    /// Endpoints of the Services earlier in the start order, **of this
    /// Service's own document**, that were READY when this Session was
    /// started (a `Service.*` reference resolves within its document).
    in_scope: Vec<ServiceEndpoints>,
    endpoints: Option<ServiceEndpoints>,
    session: Option<ServiceSession>,
    /// Relaunches consumed so far (RFC 0009 `restartPolicy.maxAttempts`).
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
    /// The run is stopping; the Service's Session is returned for `end()`.
    Stopped,
}

type Background = JoinHandle<(Instance, Outcome)>;

enum State {
    /// Not started (or returned to pending to be started again with new
    /// in-scope endpoints).
    Pending(Instance),
    /// A background task is starting or relaunching the Service.
    Busy(Background),
    /// READY; the Session is held here and watched for an exit.
    Ready(Instance),
    /// FAILED; the Session has ended.
    Failed,
    /// Ended because its scope completed or the run stopped.
    Stopped,
}

struct Managed {
    key: ServiceKey,
    policy: CompletedTasksPolicy,
    /// The Services this one references through `Service.*` — each a
    /// Service of the same document, by construction of the scope rules.
    references: BTreeSet<ServiceKey>,
    state: State,
}

struct Shared {
    config: ServiceRunConfig,
    ports: Mutex<PortAllocator>,
    session_counter: AtomicU32,
}

/// An instance failure observed on a READY Service (RFC 0009 "Failure and
/// restart"): its `onRun` exited other than by the runner's cancelation, or
/// its health check declared it UNHEALTHY (on which the Service Session has
/// already canceled `onRun`).
pub(super) struct Detected {
    group: Group,
    idx: usize,
    failure: InstanceFailure,
}

enum InstanceFailure {
    Exit(ServiceRunExit),
    Unhealthy(ServiceUnhealthy),
}

impl InstanceFailure {
    /// Classify an `onRun` exit the Session reported: UNHEALTHY when the
    /// health check forced it, an ordinary exit otherwise.
    fn from_exit(exit: ServiceRunExit) -> Self {
        match exit.unhealthy {
            Some(u) => Self::Unhealthy(u),
            None => Self::Exit(exit),
        }
    }

    fn into_kind(self) -> FailureKind {
        match self {
            Self::Exit(exit) => FailureKind::Exited(exit),
            Self::Unhealthy(u) => FailureKind::Unhealthy(u),
        }
    }
}

/// The scheduler side of RFC 0009 for one `openjd run`.
pub(super) struct ServiceManager {
    shared: Arc<Shared>,
    job: Vec<Managed>,
    step: Vec<Managed>,
    /// Cancels background work on Job Services when they are stopped.
    job_stop: CancellationToken,
    /// Cancels background work on the current Step's Services when they
    /// are stopped. Replaced for every Step.
    step_stop: CancellationToken,
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
        let job_stop = config.cancel_token.child_token();
        let step_stop = config.cancel_token.child_token();
        Self {
            shared: Arc::new(Shared {
                config,
                ports: Mutex::new(PortAllocator::default()),
                session_counter: AtomicU32::new(0),
            }),
            job: Vec::new(),
            step: Vec::new(),
            job_stop,
            step_stop,
        }
    }

    /// Register the Job's Services (the combined `jobServices`: external
    /// Services then the Job Template's own), not yet started. Their
    /// Sessions enter the Job's Environments.
    ///
    /// `environment_documents` is the document of each entry of
    /// `job.job_environments`, index for index.
    pub(super) fn set_job_services(&mut self, job: &Job, environment_documents: &[Document]) {
        if !self.job.is_empty() {
            return;
        }
        let envs: Vec<Environment> = job.job_environments.clone().unwrap_or_default();
        for service in job.job_services.iter().flatten() {
            let profiles = self.environment_profiles(service, environment_documents);
            self.job.push(managed(
                service,
                ServiceScope::Job,
                envs.clone(),
                profiles,
                self.shared.config.retain_working_dir,
            ));
        }
    }

    /// Register `step`'s Services, not yet started. Their Sessions enter the
    /// Job's Environments followed by the Step's. A no-op while the current
    /// Step's Services are registered (a Step re-running its Tasks keeps
    /// them). `environment_documents` is as for
    /// [`set_job_services`](Self::set_job_services); a Step's Environments
    /// belong to the Job Template.
    pub(super) fn set_step_services(
        &mut self,
        job: &Job,
        environment_documents: &[Document],
        step: &Step,
    ) {
        if !self.step.is_empty() {
            return;
        }
        let Some(services) = &step.step_services else {
            return;
        };
        let mut envs: Vec<Environment> = job.job_environments.clone().unwrap_or_default();
        envs.extend(step.step_environments.iter().flatten().cloned());
        self.step_stop = self.shared.config.cancel_token.child_token();
        for service in services {
            // Step Environments are the Job Template's: they share a Step
            // Service's document, so they need no profile of their own.
            let profiles = self.environment_profiles(service, environment_documents);
            self.step.push(managed(
                service,
                ServiceScope::Step(step.name.clone()),
                envs.clone(),
                profiles,
                self.shared.config.retain_working_dir,
            ));
        }
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

    /// `true` when any Service is registered.
    pub(super) fn any_registered(&self) -> bool {
        !self.job.is_empty() || !self.step.is_empty()
    }

    /// The endpoints an entity declared by `document` may reference in the
    /// Task Session: the READY Job Services' and the Step's Services' of
    /// **that document** (`port` and `connectAddress`; `bindAddress` is
    /// never seeded here). A Task, or a Job Template Environment, passes
    /// `Document::JobTemplate` and sees the Job Template's Services alone;
    /// an attached Environment passes its own document and sees the
    /// external Services it was declared with (Template Schemas §1.2.2
    /// item 2).
    pub(super) fn task_scope_endpoints(&self, document: &Document) -> Vec<ServiceEndpoints> {
        self.job
            .iter()
            .chain(self.step.iter())
            .filter(|m| m.key.document == *document)
            .filter_map(|m| match &m.state {
                State::Ready(inst) => inst.endpoints.clone(),
                _ => None,
            })
            .collect()
    }

    /// The readiness gate (constraint 3), run before every Task: observe
    /// any instance failure since the last gate and begin the restart decision
    /// for it; await every background start/relaunch; restart the Services
    /// that reference one whose Session was replaced; start every Service
    /// not yet started, in waves of Services whose referenced Services are
    /// READY (constraint 2); repeat until every registered Service is READY,
    /// FAILED, or stopped.
    pub(super) async fn gate(&mut self) -> Result<GateOutcome, RunError> {
        let mut outcome = GateOutcome::default();
        loop {
            for detected in self.poll_failures() {
                let (policy, scope) = self.begin_recovery(detected);
                if policy == CompletedTasksPolicy::Rerun {
                    outcome.rerun = outcome.rerun.max(Some(scope));
                }
            }
            if !self.any_busy() && !self.any_pending() {
                break;
            }
            let settled = self.await_busy(&mut outcome).await?;
            if outcome.failure.is_some() {
                break;
            }
            self.restart_dependents(&settled).await;
            self.start_pending(&mut outcome).await?;
            if outcome.failure.is_some() {
                break;
            }
        }
        Ok(outcome)
    }

    /// Stop the current Step's Services (constraints 4, 6, 7): cancel
    /// background work, end every Session in reverse start order.
    pub(super) async fn stop_step_services(&mut self) {
        self.step_stop.cancel();
        stop_group(&mut self.step).await;
        self.step.clear();
    }

    /// Stop the Job Services in reverse start order. Call after every Step
    /// Service has stopped (constraint 4) and the Task Session has exited
    /// the Job's Environments.
    pub(super) async fn stop_job_services(&mut self) {
        self.job_stop.cancel();
        stop_group(&mut self.job).await;
    }

    /// Stop everything: the Step's Services, then the Job's.
    pub(super) async fn stop_all(&mut self) {
        self.stop_step_services().await;
        self.stop_job_services().await;
    }

    /// Resolve when a READY Service suffers an instance failure while a Task
    /// runs: its health check declares it UNHEALTHY, or its `onRun` exits
    /// other than by cancelation. Pending forever when no Service is READY.
    pub(super) async fn wait_instance_failure(&self) -> Detected {
        let mut watchers: Vec<Pin<Box<dyn Future<Output = Detected> + Send>>> = Vec::new();
        for (group, list) in [(Group::Job, &self.job), (Group::Step, &self.step)] {
            for (idx, m) in list.iter().enumerate() {
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
                        // UNHEALTHY is reported as soon as the health check
                        // decides it, before the canceled onRun has exited.
                        if let ServiceHealth::Unhealthy(u) = health_rx.borrow_and_update().clone() {
                            return Detected {
                                group,
                                idx,
                                failure: InstanceFailure::Unhealthy(u),
                            };
                        }
                        let current = exit_rx.borrow_and_update().clone();
                        match current {
                            Some(exit) if exit.canceled => std::future::pending::<()>().await,
                            Some(exit) => {
                                return Detected {
                                    group,
                                    idx,
                                    failure: InstanceFailure::from_exit(exit),
                                }
                            }
                            None => {}
                        }
                        tokio::select! {
                            r = exit_rx.changed() => {
                                if r.is_err() {
                                    return Detected {
                                        group,
                                        idx,
                                        failure: InstanceFailure::Exit(driver_lost_exit()),
                                    };
                                }
                            }
                            r = health_rx.changed() => {
                                if r.is_err() {
                                    // The driver is gone; the exit watcher
                                    // reports what happened.
                                    health_rx.mark_unchanged();
                                    if exit_rx.changed().await.is_err() {
                                        return Detected {
                                            group,
                                            idx,
                                            failure: InstanceFailure::Exit(driver_lost_exit()),
                                        };
                                    }
                                }
                            }
                        }
                    }
                }));
            }
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
    /// background. Returns the Service's `completedTasks` policy and the
    /// scope a `RERUN` reaches, so the caller can cancel and requeue Tasks.
    pub(super) fn begin_recovery(
        &mut self,
        detected: Detected,
    ) -> (CompletedTasksPolicy, RerunScope) {
        let Detected {
            group,
            idx,
            failure,
        } = detected;
        let stop = match group {
            Group::Job => self.job_stop.clone(),
            Group::Step => self.step_stop.clone(),
        };
        let list = match group {
            Group::Job => &mut self.job,
            Group::Step => &mut self.step,
        };
        let m = &mut list[idx];
        let policy = m.policy;
        let rerun_scope = match group {
            Group::Job => RerunScope::Job,
            Group::Step => RerunScope::Step,
        };
        if let State::Ready(inst) = std::mem::replace(&mut m.state, State::Failed) {
            if let InstanceFailure::Unhealthy(u) = &failure {
                log_line(format!(
                    "{} ({} scope) is UNHEALTHY: {u}",
                    inst.label, inst.scope
                ));
            }
            let kind = failure.into_kind();
            log_line(format!(
                "{} ({} scope) is UNREADY: {} (completedTasks: {})",
                inst.label,
                inst.scope,
                kind.describe(),
                policy_name(policy)
            ));
            m.state = State::Busy(tokio::spawn(start_or_recover(
                inst,
                Some(kind),
                self.shared.clone(),
                stop,
            )));
        }
        (policy, rerun_scope)
    }

    fn poll_failures(&self) -> Vec<Detected> {
        let mut out = Vec::new();
        for (group, list) in [(Group::Job, &self.job), (Group::Step, &self.step)] {
            for (idx, m) in list.iter().enumerate() {
                let State::Ready(inst) = &m.state else {
                    continue;
                };
                let Some(session) = inst.session.as_ref() else {
                    continue;
                };
                if let Some(ServiceHealth::Unhealthy(u)) =
                    session.health_watch().map(|rx| rx.borrow().clone())
                {
                    out.push(Detected {
                        group,
                        idx,
                        failure: InstanceFailure::Unhealthy(u),
                    });
                    continue;
                }
                let Some(rx) = session.exit_watch() else {
                    continue;
                };
                let current = rx.borrow().clone();
                if let Some(exit) = current {
                    if !exit.canceled {
                        out.push(Detected {
                            group,
                            idx,
                            failure: InstanceFailure::from_exit(exit),
                        });
                    }
                }
            }
        }
        out
    }

    fn any_busy(&self) -> bool {
        self.job
            .iter()
            .chain(self.step.iter())
            .any(|m| matches!(m.state, State::Busy(_)))
    }

    fn any_pending(&self) -> bool {
        self.job
            .iter()
            .chain(self.step.iter())
            .any(|m| matches!(m.state, State::Pending(_)))
    }

    /// Await every background task. Records FAILED Services on `outcome`
    /// (the first as its `failure`) and returns the keys of the Services
    /// that became READY in a new Session.
    async fn await_busy(&mut self, outcome: &mut GateOutcome) -> Result<Vec<ServiceKey>, RunError> {
        let mut replaced = Vec::new();
        for (group, list) in [(Group::Job, &mut self.job), (Group::Step, &mut self.step)] {
            for m in list.iter_mut() {
                if !matches!(m.state, State::Busy(_)) {
                    continue;
                }
                let State::Busy(handle) = std::mem::replace(&mut m.state, State::Failed) else {
                    unreachable!("checked above");
                };
                let (inst, result) = handle
                    .await
                    .map_err(|e| format!("{}: background task failed: {e}", m.key))?;
                match result {
                    Outcome::Ready { replaced: r } => {
                        if r {
                            replaced.push(m.key.clone());
                            match group {
                                Group::Job => outcome.job_endpoints_changed = true,
                                Group::Step => outcome.step_endpoints_changed = true,
                            }
                        }
                        m.state = State::Ready(inst);
                    }
                    Outcome::Failed(reason) => {
                        let failure = ServiceFailure {
                            name: m.key.name.clone(),
                            document: m.key.document.clone(),
                            scope: inst.scope.clone(),
                            reason,
                        };
                        eprintln!("ERROR: {failure}");
                        outcome.failure.get_or_insert(failure);
                        m.state = State::Failed;
                    }
                    Outcome::Stopped => {
                        let mut inst = inst;
                        end_instance(&mut inst).await;
                        m.state = State::Stopped;
                    }
                }
            }
        }
        Ok(replaced)
    }

    /// Return every READY Service that (transitively) references one of
    /// `replaced` to pending, ending its Session: its `Service.*` values for
    /// the replaced Service are stale, and RFC 0009 constraint 2 only
    /// guarantees a referenced Service's endpoint at the referencing
    /// Session's start. Not a failure of the dependent: no attempt is
    /// consumed. Stopped in reverse start order (constraint 4).
    async fn restart_dependents(&mut self, replaced: &[ServiceKey]) {
        if replaced.is_empty() {
            return;
        }
        let mut stale: BTreeSet<ServiceKey> = replaced.iter().cloned().collect();
        loop {
            let before = stale.len();
            for m in self.job.iter().chain(self.step.iter()) {
                if matches!(m.state, State::Ready(_)) && !m.references.is_disjoint(&stale) {
                    stale.insert(m.key.clone());
                }
            }
            if stale.len() == before {
                break;
            }
        }
        for list in [&mut self.step, &mut self.job] {
            for m in list.iter_mut().rev() {
                if replaced.contains(&m.key) || !stale.contains(&m.key) {
                    continue;
                }
                if let State::Ready(mut inst) = std::mem::replace(&mut m.state, State::Failed) {
                    log_line(format!(
                        "{} references a Service that began a new Service Session; \
                         restarting it with the new endpoints",
                        m.key
                    ));
                    end_instance(&mut inst).await;
                    m.state = State::Pending(inst);
                }
            }
        }
    }

    /// Start every pending Service: Job Services first, then the Step's, in
    /// waves of Services whose referenced Services are all READY. Each wave
    /// starts concurrently; the next begins when the wave has settled.
    async fn start_pending(&mut self, outcome: &mut GateOutcome) -> Result<(), RunError> {
        loop {
            let ready: BTreeSet<ServiceKey> = self
                .job
                .iter()
                .chain(self.step.iter())
                .filter(|m| matches!(m.state, State::Ready(_)))
                .map(|m| m.key.clone())
                .collect();
            let known: BTreeSet<ServiceKey> = self
                .job
                .iter()
                .chain(self.step.iter())
                .map(|m| m.key.clone())
                .collect();
            let mut spawned = false;
            let mut blocked = Vec::new();
            for (group, stop) in [
                (Group::Job, self.job_stop.clone()),
                (Group::Step, self.step_stop.clone()),
            ] {
                // The READY Job Services' endpoints, with their documents,
                // for seeding a Step Service's Session.
                let job_snapshot = ready_snapshot(&self.job);
                let list = match group {
                    Group::Job => &mut self.job,
                    Group::Step => &mut self.step,
                };
                for idx in 0..list.len() {
                    if !matches!(list[idx].state, State::Pending(_)) {
                        continue;
                    }
                    let waiting_on: Vec<&ServiceKey> = list[idx]
                        .references
                        .iter()
                        .filter(|r| known.contains(*r) && !ready.contains(*r))
                        .collect();
                    if !waiting_on.is_empty() {
                        blocked.push(list[idx].key.to_string());
                        continue;
                    }
                    // Constraint 2: in scope are the Services earlier in the
                    // start order — every Job Service for a Step Service,
                    // plus the earlier entries of this list — restricted to
                    // the Service's own document (§1.2.2 item 2: a
                    // `Service.*` reference resolves within its document,
                    // and two documents may both declare a `Cache`).
                    let document = list[idx].key.document.clone();
                    let mut in_scope = match group {
                        Group::Job => Vec::new(),
                        Group::Step => snapshot_endpoints(&job_snapshot, &document),
                    };
                    in_scope.extend(ready_endpoints(list, Some(idx), &document));
                    let State::Pending(mut inst) =
                        std::mem::replace(&mut list[idx].state, State::Failed)
                    else {
                        unreachable!("checked above");
                    };
                    inst.in_scope = in_scope;
                    list[idx].state = State::Busy(tokio::spawn(start_or_recover(
                        inst,
                        None,
                        self.shared.clone(),
                        stop.clone(),
                    )));
                    spawned = true;
                }
            }
            if !spawned {
                if !blocked.is_empty() && !self.any_busy() {
                    return Err(format!(
                        "cannot order the start of Services {}: each references a Service that \
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
            self.restart_dependents(&replaced).await;
        }
    }
}

fn managed(
    service: &Service,
    scope: ServiceScope,
    environments: Vec<Environment>,
    environment_profiles: Vec<Option<ModelProfile>>,
    retain_working_dir: bool,
) -> Managed {
    let key = ServiceKey::of(service);
    Managed {
        // A `Service.<name>.*` reference inside this Service names a
        // Service of the same document (template validation seeds only
        // the document's own Services), so each referenced name is keyed
        // with this Service's document.
        references: referenced_service_names(service)
            .iter()
            .map(|name| ServiceKey::sibling(&key.document, name))
            .collect(),
        key: key.clone(),
        policy: service.restart_policy.completed_tasks,
        state: State::Pending(Instance {
            service: service.clone(),
            scope,
            label: key.to_string(),
            environments,
            environment_profiles,
            in_scope: Vec::new(),
            endpoints: None,
            session: None,
            relaunches: 0,
            retain_working_dir,
        }),
    }
}

/// Endpoints of the READY Services of `list` declared by `document`,
/// restricted to indices before `before` when given.
fn ready_endpoints(
    list: &[Managed],
    before: Option<usize>,
    document: &Document,
) -> Vec<ServiceEndpoints> {
    list.iter()
        .take(before.unwrap_or(list.len()))
        .filter(|m| m.key.document == *document)
        .filter_map(|m| match &m.state {
            State::Ready(inst) => inst.endpoints.clone(),
            _ => None,
        })
        .collect()
}

/// The READY Services' endpoints of `list` with their documents — a
/// snapshot usable while another list is borrowed mutably.
fn ready_snapshot(list: &[Managed]) -> Vec<(Document, ServiceEndpoints)> {
    list.iter()
        .filter_map(|m| match &m.state {
            State::Ready(inst) => inst.endpoints.clone().map(|e| (m.key.document.clone(), e)),
            _ => None,
        })
        .collect()
}

/// The endpoints of `snapshot` declared by `document`.
fn snapshot_endpoints(
    snapshot: &[(Document, ServiceEndpoints)],
    document: &Document,
) -> Vec<ServiceEndpoints> {
    snapshot
        .iter()
        .filter(|(d, _)| d == document)
        .map(|(_, e)| e.clone())
        .collect()
}

fn policy_name(policy: CompletedTasksPolicy) -> &'static str {
    match policy {
        CompletedTasksPolicy::Keep => "KEEP",
        CompletedTasksPolicy::Rerun => "RERUN",
    }
}

fn driver_lost_exit() -> ServiceRunExit {
    ServiceRunExit {
        state: openjd_sessions::ActionState::Failed,
        exit_code: None,
        canceled: false,
        unhealthy: None,
        fail_message: Some("onRun driver ended without reporting an exit".into()),
        stdout: String::new(),
    }
}

/// End every Session of `list` in reverse start order, awaiting (and
/// thereby cutting short, through the group's stop token) any background
/// start or relaunch first.
async fn stop_group(list: &mut [Managed]) {
    for m in list.iter_mut().rev() {
        match std::mem::replace(&mut m.state, State::Stopped) {
            State::Busy(handle) => match handle.await {
                Ok((mut inst, _)) => end_instance(&mut inst).await,
                Err(e) => eprintln!("ERROR: {}: background task failed: {e}", m.key),
            },
            State::Ready(mut inst) | State::Pending(mut inst) => end_instance(&mut inst).await,
            State::Failed | State::Stopped => {}
        }
    }
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

/// Start the Service (when `first` is `None`) or apply the restart decision
/// to `first` and relaunch, until it is READY, FAILED, or the group is
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
            // RFC 0009 "Failure and restart" step 3/4.
            let max_attempts = inst.service.restart_policy.max_attempts;
            if inst.relaunches >= max_attempts {
                let reason = format!(
                    "{}; {} of {} relaunch(es) used (restartPolicy.maxAttempts)",
                    k.describe(),
                    inst.relaunches,
                    max_attempts
                );
                log_line(format!(
                    "{} ({} scope) is FAILED: {reason}",
                    inst.label, inst.scope
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
                    max_attempts,
                    k.describe()
                ));
                end_instance(&mut inst).await;
            } else {
                log_line(format!(
                    "Relaunching {} onRun in its Service Session (relaunch {} of {}): {}",
                    inst.label,
                    inst.relaunches,
                    max_attempts,
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
                    "{} ({} scope) is UNREADY: {}",
                    inst.label,
                    inst.scope,
                    k.describe()
                ));
                kind = Some(k);
            }
        }
    }
}

/// Open a Service Session if none is live (allocate endpoints, enter), then
/// launch `onRun` and wait for the readiness verdict. `Ok(true)`: READY.
/// `Ok(false)`: the group is stopping. `Err`: the failure to decide on.
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
            "{} ({} scope) endpoints: {}",
            inst.label,
            inst.scope,
            describe_endpoints(&endpoints)
        ));
        inst.endpoints = Some(endpoints.clone());
        let mut session = ServiceSession::with_config(ServiceSessionConfig {
            session: session_config(shared, &inst.service),
            service: inst.service.clone(),
            environments: inst.environments.clone(),
            environment_profiles: inst.environment_profiles.clone(),
            endpoints,
            in_scope_endpoints: inst.in_scope.clone(),
        })
        .map_err(|e| FailureKind::Start(e.to_string()))?;
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
        let _ = session.wait_exit().await;
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
            session.cancel_run(None);
            let _ = session.wait_exit().await;
            Err(FailureKind::ReadyTimedOut)
        }
        Ok(ServiceHealth::Unhealthy(u)) => {
            // READY and UNHEALTHY before this task observed either: the
            // Session has canceled onRun; wait for it (step 2).
            log_line(format!(
                "{} ({} scope) is UNHEALTHY: {u}",
                inst.label, inst.scope
            ));
            let _ = session.wait_exit().await;
            Err(FailureKind::Unhealthy(u))
        }
        Ok(ServiceHealth::ExitedBeforeReady | ServiceHealth::Pending) => {
            let exit = session
                .wait_exit()
                .await
                .unwrap_or_else(|_| driver_lost_exit());
            if exit.canceled {
                Ok(false)
            } else {
                Err(FailureKind::ExitedBeforeReady(exit))
            }
        }
        Err(e) => Err(FailureKind::Start(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rerun_scope_orders_job_above_step() {
        assert!(RerunScope::Job > RerunScope::Step);
        assert_eq!(
            Some(RerunScope::Step).max(Some(RerunScope::Job)),
            Some(RerunScope::Job)
        );
        assert_eq!(None.max(Some(RerunScope::Step)), Some(RerunScope::Step));
    }

    #[test]
    fn failure_kind_new_session_policy_and_descriptions() {
        let exit = ServiceRunExit {
            state: openjd_sessions::ActionState::Failed,
            exit_code: Some(3),
            canceled: false,
            unhealthy: None,
            fail_message: Some("boom".into()),
            stdout: String::new(),
        };
        let start = FailureKind::Start("port 1 unavailable".into());
        let before = FailureKind::ExitedBeforeReady(exit.clone());
        let timed_out = FailureKind::ReadyTimedOut;
        let exited = FailureKind::Exited(exit.clone());
        let unhealthy = FailureKind::Unhealthy(ServiceUnhealthy {
            failed_probes: 2,
            failure_threshold: 2,
            last_failure: "onHealthCheck exit code: 1".into(),
        });
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
            ..exit
        };
        assert!(matches!(
            InstanceFailure::from_exit(forced).into_kind(),
            FailureKind::Unhealthy(u) if u.failed_probes == 3
        ));
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
            scope: ServiceScope::Step("Render".into()),
            reason: "did not become READY within readinessTimeoutSeconds; 1 of 1 relaunch(es) used"
                .into(),
        };
        assert_eq!(
            f.to_string(),
            "Service 'Cache' (Step 'Render' scope) failed: did not become READY within \
             readinessTimeoutSeconds; 1 of 1 relaunch(es) used"
        );
        assert_eq!(ServiceScope::Job.to_string(), "Job");

        // An external Service names its document.
        let f = ServiceFailure {
            document: Document::environment_template(0, Some("queue-cache.yaml")),
            scope: ServiceScope::Job,
            ..f
        };
        assert_eq!(
            f.to_string(),
            "Service 'Cache' (from queue-cache.yaml) (Job scope) failed: did not become READY \
             within readinessTimeoutSeconds; 1 of 1 relaunch(es) used"
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
        // A reference made from inside the external Service stays in its
        // document.
        assert_eq!(
            ServiceKey::sibling(&external.document, "Store"),
            ServiceKey {
                document: external.document.clone(),
                name: "Store".into()
            }
        );
        assert_eq!(origin_suffix(&Document::JobTemplate), "");
    }
}
