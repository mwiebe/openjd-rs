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
//! (ordering constraints 2 and 10: after every entry of the Service's
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
//! Service plus its name (Template Schemas §1.2.2 item 3: an inline Service
//! shadows an external one of the same name, and two attachments may both
//! declare a `Cache`). A Service Session is seeded with the endpoints of the
//! Services it lists with the `service` key in its `dependencies` — of its
//! own document, or the attached Service bound to the Job Template's
//! `requiresServices` entry of that name for an inline Service — and a Task
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
//! `maxAttempts` 0) is treated as `RERUN`. A relaunch within the same
//! Service Session keeps the same ports and requires nothing of
//! dependents.
//!
//! See `specs/cli/run.md` § Services for the orchestration rules.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use openjd_model::job::service_symbols::ServiceEndpoints;
use openjd_model::job::{CompletedTasksPolicy, Document, Environment, Job, Service, ServiceScope};
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
    /// The attached Service each `requiresServices` entry of the Job
    /// Template was bound to at submission (§1.2.2 item 2). The Job
    /// Template's Tasks, Environments and inline Services see
    /// `Service.<requirement>.*` as that Service's endpoints.
    pub requirement_bindings: Vec<RequirementBinding>,
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
    /// A Service whose scope is every Step (or an external Service) began a
    /// new Service Session (new endpoints) since the last gate: every
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
    /// The Job's Environments, in entry order; the Session enters those
    /// whose `runScope` includes `SERVICE`.
    environments: Vec<Environment>,
    /// The profile of each entry of `environments` whose document is not
    /// the Service's own (`None` for those that share it), index for index
    /// — `ServiceSessionConfig::environment_profiles`.
    environment_profiles: Vec<Option<ModelProfile>>,
    /// Endpoints of the Services this one depends on that were READY when
    /// this Session was started: Services of its own document, and the
    /// bound required Services for an inline Service.
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
    /// The Service is stopping; the Service's Session is returned for
    /// `end()`.
    Stopped,
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
    /// The Service's `completedTasks`; `None` only when its `maxAttempts`
    /// is 0, so a relaunch never happens (an instance failure fails the
    /// scope) and the value matters only for a dependent restart, where it
    /// reads as `RERUN`.
    policy: Option<CompletedTasksPolicy>,
    /// The Services this one lists with the `service` key in its
    /// `dependencies` (§9 item 4): Services of the same document, plus the
    /// bound required Services an inline Service lists. It starts after each
    /// is READY and is stopped before any of them.
    depends_on_services: BTreeSet<ServiceKey>,
    /// Set while the Service is idle because a Service it lists (`Some`,
    /// named) began a new Service Session; the gate starts it again once
    /// its dependencies are READY and logs that it is starting again.
    restart_after: Option<ServiceKey>,
    /// The Job's Environments (every one; the Session skips those whose
    /// `runScope` excludes `SERVICE`).
    environments: Vec<Environment>,
    environment_profiles: Vec<Option<ModelProfile>>,
    /// Whether a Step in the Service's scope is pending in the current
    /// round: the gate starts active idle Services and leaves inactive ones
    /// idle (constraint 10: a Service whose scope schedules no Task need
    /// not start).
    active: bool,
    /// Cancels background work when the Service is stopped. Replaced when
    /// the Service is started again from idle.
    stop: CancellationToken,
    state: State,
}

impl Managed {
    /// A fresh [`Instance`] for a new Service Session (constraint 9: new
    /// host selection, new ports, new working directory).
    fn new_instance(&self, retain_working_dir: bool) -> Instance {
        Instance {
            service: self.service.clone(),
            label: self.key.to_string(),
            scope_label: scope_label(&self.scope),
            environments: self.environments.clone(),
            environment_profiles: self.environment_profiles.clone(),
            in_scope: Vec::new(),
            endpoints: None,
            session: None,
            relaunches: 0,
            retain_working_dir,
        }
    }

    fn is_job_wide(&self) -> bool {
        self.scope.is_all_steps()
    }
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
            }),
            services: Vec::new(),
        }
    }

    /// Register every Service of the combined Job (`job.services`: external
    /// Services then the Job Template's own), none started or active. Their
    /// Sessions enter the Job's Environments (those whose `runScope`
    /// includes `SERVICE`); `environment_documents` is the document of each
    /// entry of `job.job_environments`, index for index.
    pub(super) fn register(&mut self, job: &Job, environment_documents: &[Document]) {
        if !self.services.is_empty() {
            return;
        }
        let envs: Vec<Environment> = job.job_environments.clone().unwrap_or_default();
        for service in job.services.iter().flatten() {
            let key = ServiceKey::of(service);
            let profiles = self.environment_profiles(service, environment_documents);
            // A `service: <name>` dependency of this Service names a Service
            // of the same document — or, in the Job Template, a required
            // external Service bound at submission.
            let depends_on_services = service
                .depends_on_services()
                .map(|name| {
                    match (
                        key.document.is_job_template(),
                        self.shared.config.binding(name),
                    ) {
                        (true, Some(binding)) if !service_declared(job, name) => {
                            ServiceKey::of_binding(binding)
                        }
                        _ => ServiceKey::sibling(&key.document, name),
                    }
                })
                .collect();
            self.services.push(Managed {
                key,
                service: service.clone(),
                scope: service.scope.clone(),
                policy: service.restart_policy.completed_tasks,
                depends_on_services,
                restart_after: None,
                environments: envs.clone(),
                environment_profiles: profiles,
                active: false,
                stop: self.shared.config.cancel_token.child_token(),
                state: State::Idle,
            });
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
        !self.services.is_empty()
    }

    /// The number of registered Services whose scope is every Step
    /// (including external Services).
    pub(super) fn job_wide_count(&self) -> usize {
        self.services.iter().filter(|m| m.is_job_wide()).count()
    }

    /// Activate every Service whose scope is every Step (constraint 10: the
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
    /// completed starts again in a new Session, constraint 9). A Service that
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
    ///   the Job Template's `requiresServices` — seeded by scope, which is
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
            for detected in self.poll_failures() {
                let (policy, scope) = self.begin_recovery(detected);
                if policy == Some(CompletedTasksPolicy::Rerun) {
                    outcome.rerun = Some(merge_scopes(outcome.rerun.take(), &scope));
                }
            }
            if !self.any_busy() && !self.any_startable() {
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
    pub(super) async fn stop_completed_scopes(&mut self, remaining: &BTreeSet<String>) {
        let done: BTreeSet<ServiceKey> = self
            .services
            .iter()
            .filter(|m| m.active && !m.is_job_wide())
            .filter(|m| {
                m.scope
                    .step_names()
                    .is_some_and(|steps| steps.is_disjoint(remaining))
            })
            .map(|m| m.key.clone())
            .collect();
        self.stop_set(&done).await;
    }

    /// Stop everything, in dependency order (constraint 4), after the Task
    /// Session has exited the Job's Environments.
    pub(super) async fn stop_all(&mut self) {
        let all: BTreeSet<ServiceKey> = self.services.iter().map(|m| m.key.clone()).collect();
        self.stop_set(&all).await;
    }

    /// Stop the Services in `keys`: cancel their background work and end
    /// their Sessions, each before any Service it depends on (constraint 4;
    /// see [`stop_order`](Self::stop_order)).
    async fn stop_set(&mut self, keys: &BTreeSet<ServiceKey>) {
        if keys.is_empty() {
            return;
        }
        for m in self.services.iter().filter(|m| keys.contains(&m.key)) {
            m.stop.cancel();
        }
        for idx in self.stop_order(keys) {
            stop_managed(&mut self.services[idx], &self.shared).await;
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

    /// Resolve when a READY Service suffers an instance failure while a Task
    /// runs: its health check declares it UNHEALTHY, or its `onRun` exits
    /// other than by cancelation. Pending forever when no Service is READY.
    pub(super) async fn wait_instance_failure(&self) -> Detected {
        let mut watchers: Vec<Pin<Box<dyn Future<Output = Detected> + Send>>> = Vec::new();
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
                    // UNHEALTHY is reported as soon as the health check
                    // decides it, before the canceled onRun has exited.
                    if let ServiceHealth::Unhealthy(u) = health_rx.borrow_and_update().clone() {
                        return Detected {
                            idx,
                            failure: InstanceFailure::Unhealthy(u),
                        };
                    }
                    let current = exit_rx.borrow_and_update().clone();
                    match current {
                        Some(exit) if exit.canceled => std::future::pending::<()>().await,
                        Some(exit) => {
                            return Detected {
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
    /// when it gave none, which only a Service with `maxAttempts` 0 may: no
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
            m.state = State::Busy(tokio::spawn(start_or_recover(
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
            if let Some(ServiceHealth::Unhealthy(u)) =
                session.health_watch().map(|rx| rx.borrow().clone())
            {
                out.push(Detected {
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
                        idx,
                        failure: InstanceFailure::from_exit(exit),
                    });
                }
            }
        }
        out
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
                if !matches!(m.state, State::Ready(_) | State::Busy(_)) {
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
            match std::mem::replace(&mut m.state, State::Idle) {
                State::Busy(handle) => match handle.await {
                    Ok((mut inst, _)) => end_instance(&mut inst).await,
                    Err(e) => eprintln!("ERROR: {}: background task failed: {e}", m.key),
                },
                State::Ready(mut inst) => end_instance(&mut inst).await,
                State::Idle | State::Failed => {}
            }
            m.stop = shared.config.cancel_token.child_token();
            m.restart_after = Some(cause.clone());
        }
    }

    /// Start every active idle Service, in waves of Services whose Service
    /// dependencies are all READY (constraint 2). Each wave starts
    /// concurrently; the next begins when the wave has settled.
    async fn start_pending(&mut self, outcome: &mut GateOutcome) -> Result<(), RunError> {
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
                // Constraint 2: the Services it depends on are READY; their
                // endpoints seed the Session.
                let in_scope: Vec<ServiceEndpoints> = m
                    .depends_on_services
                    .iter()
                    .filter_map(|r| ready.get(r).cloned())
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
                inst.in_scope = in_scope;
                m.state = State::Busy(tokio::spawn(start_or_recover(
                    inst,
                    None,
                    self.shared.clone(),
                    m.stop.clone(),
                )));
                spawned = true;
            }
            if !spawned {
                if !blocked.is_empty() && !self.any_busy() {
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

/// True when the Job Template declares an inline Service named `name` (which
/// shadows an attached Service of the same name, Template Schemas §1.2.2
/// item 3).
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
/// `maxAttempts` is 0 and a failure fails the scope), `completedTasks:
/// none; maxAttempts 0`.
fn describe_policy(policy: Option<CompletedTasksPolicy>) -> String {
    match policy {
        Some(p) => format!("completedTasks: {}", policy_name(p)),
        None => "completedTasks: none; maxAttempts 0, so the failure fails the scope".to_string(),
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
        fail_message: Some("onRun driver ended without reporting an exit".into()),
        stdout: String::new(),
    }
}

/// End `m`'s Session, if one is live, awaiting (and thereby cutting short,
/// through its stop token) any background start or relaunch first, and
/// return the Service to idle.
async fn stop_managed(m: &mut Managed, shared: &Shared) {
    match std::mem::replace(&mut m.state, State::Idle) {
        State::Busy(handle) => match handle.await {
            Ok((mut inst, _)) => end_instance(&mut inst).await,
            Err(e) => eprintln!("ERROR: {}: background task failed: {e}", m.key),
        },
        State::Ready(mut inst) => end_instance(&mut inst).await,
        State::Idle | State::Failed => {}
    }
    m.active = false;
    m.stop = shared.config.cancel_token.child_token();
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
            let max_attempts = inst.service.restart_policy.max_attempts;
            if inst.relaunches >= max_attempts {
                let reason = format!(
                    "{}; {} of {} relaunch(es) used (restartPolicy.maxAttempts)",
                    k.describe(),
                    inst.relaunches,
                    max_attempts
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
                    "{} {} is UNREADY: {}",
                    inst.label,
                    inst.scope_label,
                    k.describe()
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
        assert_eq!(
            describe_policy(Some(CompletedTasksPolicy::Keep)),
            "completedTasks: KEEP"
        );
        assert_eq!(
            describe_policy(None),
            "completedTasks: none; maxAttempts 0, so the failure fails the scope"
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
