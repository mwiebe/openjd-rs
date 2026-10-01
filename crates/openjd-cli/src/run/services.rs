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
//! 3), watches for instance failures while Tasks run and applies the
//! restart policy ("Failure and restart"), and stops Services when their
//! scope completes (constraints 4, 6, 7). The Service Sessions themselves
//! are `openjd_sessions::ServiceSession`s.
//!
//! See `specs/cli/run.md` § Services for the orchestration rules.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use openjd_model::job::service_symbols::{referenced_service_names, ServiceEndpoints};
use openjd_model::job::{CompletedTasksPolicy, Environment, Job, Service, Step};
use openjd_model::types::{JobParameterValues, ModelProfile};
use openjd_sessions::path_mapping::PathMappingRule;
use openjd_sessions::session::SessionConfig;
use openjd_sessions::{
    ServiceReadiness, ServiceRunExit, ServiceSession, ServiceSessionConfig, ServiceSessionState,
    SessionLimits,
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

/// A Service that became FAILED: its scope fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ServiceFailure {
    pub name: String,
    pub scope: ServiceScope,
    pub reason: String,
}

impl std::fmt::Display for ServiceFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Service '{}' ({} scope) failed: {}",
            self.name, self.scope, self.reason
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
    pub profile: ModelProfile,
    /// The run's interruption token. A Service Session does not share it
    /// (a token canceled while a Session is being torn down would cancel
    /// its `onExit` and Environment exits too, which constraint 7 wants
    /// run); instead the manager watches a child of it while a Service
    /// starts and cancels the running action through the Session's cancel
    /// handle, then ends the Session as for a scope completion.
    pub cancel_token: CancellationToken,
    pub limits: SessionLimits,
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
    /// `onRun` exited before the readiness check passed — possibly a port
    /// conflict, so the relaunch begins a new Service Session (new ports).
    ExitedBeforeReady(ServiceRunExit),
    /// The readiness check timed out while `onRun` kept running (it has
    /// been canceled and has exited by the time this is reported).
    ReadinessTimedOut,
    /// `onRun` exited after READY while the scope still had work.
    Exited(ServiceRunExit),
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
            Self::ReadinessTimedOut => "readiness check timed out".to_string(),
            Self::Exited(exit) => format!(
                "onRun exited while the scope still had work ({})",
                describe_exit(exit)
            ),
        }
    }
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
    /// The Environments the Service Session enters (those whose `runScope`
    /// includes `SERVICE`), in entry order.
    environments: Vec<Environment>,
    /// Endpoints of the Services earlier in the start order that were READY
    /// when this Session was started.
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
    name: String,
    policy: CompletedTasksPolicy,
    /// Names of the Services this one references through `Service.*`.
    references: BTreeSet<String>,
    state: State,
}

struct Shared {
    config: ServiceRunConfig,
    ports: Mutex<PortAllocator>,
    session_counter: AtomicU32,
}

/// An `onRun` exit observed on a READY Service.
pub(super) struct Detected {
    group: Group,
    idx: usize,
    exit: ServiceRunExit,
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
    pub(super) fn set_job_services(&mut self, job: &Job) {
        if !self.job.is_empty() {
            return;
        }
        let envs: Vec<Environment> = job.job_environments.clone().unwrap_or_default();
        for service in job.job_services.iter().flatten() {
            self.job.push(managed(
                service,
                ServiceScope::Job,
                envs.clone(),
                self.shared.config.retain_working_dir,
            ));
        }
    }

    /// Register `step`'s Services, not yet started. Their Sessions enter the
    /// Job's Environments followed by the Step's. A no-op while the current
    /// Step's Services are registered (a Step re-running its Tasks keeps
    /// them).
    pub(super) fn set_step_services(&mut self, job: &Job, step: &Step) {
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
            self.step.push(managed(
                service,
                ServiceScope::Step(step.name.clone()),
                envs.clone(),
                self.shared.config.retain_working_dir,
            ));
        }
    }

    /// `true` when any Service is registered.
    pub(super) fn any_registered(&self) -> bool {
        !self.job.is_empty() || !self.step.is_empty()
    }

    /// The endpoints every Task Session of the current Step may reference:
    /// the Job Services' and the Step's Services' (`port` and
    /// `connectAddress`; `bindAddress` is never seeded here).
    pub(super) fn task_scope_endpoints(&self) -> Vec<ServiceEndpoints> {
        self.job
            .iter()
            .chain(self.step.iter())
            .filter_map(|m| match &m.state {
                State::Ready(inst) => inst.endpoints.clone(),
                _ => None,
            })
            .collect()
    }

    /// The readiness gate (constraint 3), run before every Task: observe
    /// any `onRun` exit since the last gate and begin the restart decision
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

    /// Resolve when the `onRun` of a READY Service exits other than by
    /// cancelation — an instance failure while a Task runs. Pending forever
    /// when no Service is READY.
    pub(super) async fn wait_instance_failure(&self) -> Detected {
        let mut watchers: Vec<Pin<Box<dyn Future<Output = Detected> + Send>>> = Vec::new();
        for (group, list) in [(Group::Job, &self.job), (Group::Step, &self.step)] {
            for (idx, m) in list.iter().enumerate() {
                let State::Ready(inst) = &m.state else {
                    continue;
                };
                let Some(mut rx) = inst.session.as_ref().and_then(|s| s.exit_watch()) else {
                    continue;
                };
                watchers.push(Box::pin(async move {
                    loop {
                        let current = rx.borrow_and_update().clone();
                        match current {
                            Some(exit) if exit.canceled => std::future::pending::<()>().await,
                            Some(exit) => return Detected { group, idx, exit },
                            None => {}
                        }
                        if rx.changed().await.is_err() {
                            return Detected {
                                group,
                                idx,
                                exit: driver_lost_exit(),
                            };
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

    /// Observe the restart decision for an `onRun` exit (RFC 0009 "Failure
    /// and restart"): the Service becomes UNREADY and its relaunch (or
    /// FAILED verdict) proceeds in the background. Returns the Service's
    /// `completedTasks` policy and the scope a `RERUN` reaches, so the
    /// caller can cancel and requeue Tasks.
    pub(super) fn begin_recovery(
        &mut self,
        detected: Detected,
    ) -> (CompletedTasksPolicy, RerunScope) {
        let Detected { group, idx, exit } = detected;
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
            let kind = FailureKind::Exited(exit);
            log_line(format!(
                "Service '{}' ({} scope) is UNREADY: {} (completedTasks: {})",
                inst.service.name,
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
                let Some(rx) = inst.session.as_ref().and_then(|s| s.exit_watch()) else {
                    continue;
                };
                let current = rx.borrow().clone();
                if let Some(exit) = current {
                    if !exit.canceled {
                        out.push(Detected { group, idx, exit });
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
    /// (the first as its `failure`) and returns the names of the Services
    /// that became READY in a new Session.
    async fn await_busy(&mut self, outcome: &mut GateOutcome) -> Result<Vec<String>, RunError> {
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
                    .map_err(|e| format!("Service '{}': background task failed: {e}", m.name))?;
                match result {
                    Outcome::Ready { replaced: r } => {
                        if r {
                            replaced.push(m.name.clone());
                            match group {
                                Group::Job => outcome.job_endpoints_changed = true,
                                Group::Step => outcome.step_endpoints_changed = true,
                            }
                        }
                        m.state = State::Ready(inst);
                    }
                    Outcome::Failed(reason) => {
                        let failure = ServiceFailure {
                            name: m.name.clone(),
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
    async fn restart_dependents(&mut self, replaced: &[String]) {
        if replaced.is_empty() {
            return;
        }
        let mut stale: BTreeSet<String> = replaced.iter().cloned().collect();
        loop {
            let before = stale.len();
            for m in self.job.iter().chain(self.step.iter()) {
                if matches!(m.state, State::Ready(_)) && !m.references.is_disjoint(&stale) {
                    stale.insert(m.name.clone());
                }
            }
            if stale.len() == before {
                break;
            }
        }
        for list in [&mut self.step, &mut self.job] {
            for m in list.iter_mut().rev() {
                if replaced.contains(&m.name) || !stale.contains(&m.name) {
                    continue;
                }
                if let State::Ready(mut inst) = std::mem::replace(&mut m.state, State::Failed) {
                    log_line(format!(
                        "Service '{}' references a Service that began a new Service Session; \
                         restarting it with the new endpoints",
                        m.name
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
            let ready: BTreeSet<String> = self
                .job
                .iter()
                .chain(self.step.iter())
                .filter(|m| matches!(m.state, State::Ready(_)))
                .map(|m| m.name.clone())
                .collect();
            let known: BTreeSet<String> = self
                .job
                .iter()
                .chain(self.step.iter())
                .map(|m| m.name.clone())
                .collect();
            let mut spawned = false;
            let mut blocked = Vec::new();
            for (group, stop) in [
                (Group::Job, self.job_stop.clone()),
                (Group::Step, self.step_stop.clone()),
            ] {
                let job_endpoints: Vec<ServiceEndpoints> = ready_endpoints(&self.job, None);
                let list = match group {
                    Group::Job => &mut self.job,
                    Group::Step => &mut self.step,
                };
                for idx in 0..list.len() {
                    if !matches!(list[idx].state, State::Pending(_)) {
                        continue;
                    }
                    let waiting_on: Vec<&String> = list[idx]
                        .references
                        .iter()
                        .filter(|r| known.contains(*r) && !ready.contains(*r))
                        .collect();
                    if !waiting_on.is_empty() {
                        blocked.push(list[idx].name.clone());
                        continue;
                    }
                    // Constraint 2: in scope are the Services earlier in the
                    // start order — every Job Service for a Step Service,
                    // plus the earlier entries of this list.
                    let mut in_scope = match group {
                        Group::Job => Vec::new(),
                        Group::Step => job_endpoints.clone(),
                    };
                    in_scope.extend(ready_endpoints(list, Some(idx)));
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
    retain_working_dir: bool,
) -> Managed {
    Managed {
        name: service.name.clone(),
        policy: service.restart_policy.completed_tasks,
        references: referenced_service_names(service),
        state: State::Pending(Instance {
            service: service.clone(),
            scope,
            environments,
            in_scope: Vec::new(),
            endpoints: None,
            session: None,
            relaunches: 0,
            retain_working_dir,
        }),
    }
}

/// Endpoints of the READY Services of `list`, restricted to indices before
/// `before` when given.
fn ready_endpoints(list: &[Managed], before: Option<usize>) -> Vec<ServiceEndpoints> {
    list.iter()
        .enumerate()
        .take(before.unwrap_or(list.len()))
        .filter_map(|(_, m)| match &m.state {
            State::Ready(inst) => inst.endpoints.clone(),
            _ => None,
        })
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
                Err(e) => eprintln!("ERROR: Service '{}': background task failed: {e}", m.name),
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
    log_banner(&format!("Stopping Service: {}", inst.service.name));
    let working_dir = session.session().working_directory().to_path_buf();
    if let Err(e) = session.end().await {
        eprintln!(
            "ERROR: Service '{}' teardown reported an error: {e}",
            inst.service.name
        );
    }
    let mut msg = format!("Service '{}' stopped", inst.service.name);
    if inst.retain_working_dir {
        msg.push_str(&format!(
            "; working directory preserved at: {}",
            working_dir.display()
        ));
    }
    log_line(msg);
}

fn session_config(shared: &Shared, service_name: &str) -> SessionConfig {
    let n = shared.session_counter.fetch_add(1, Ordering::SeqCst);
    SessionConfig {
        session_id: format!("cli-{}-svc-{service_name}-{n}", std::process::id()),
        job_parameter_values: shared.config.job_parameter_values.clone(),
        path_mapping_rules: shared.config.path_mapping_rules.clone(),
        retain_working_dir: shared.config.retain_working_dir,
        callback: None,
        os_env_vars: None,
        session_root_directory: None,
        user: None,
        profile: Some(shared.config.profile.clone()),
        cancel_token: None,
        sticky_bit_policy: Default::default(),
        debug_collect_stdout: false,
        echo_openjd_directives: true,
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
                    "Service '{}' ({} scope) is FAILED: {reason}",
                    inst.service.name, inst.scope
                ));
                end_instance(&mut inst).await;
                return (inst, Outcome::Failed(reason));
            }
            inst.relaunches += 1;
            if k.requires_new_session() {
                replaced = true;
                log_line(format!(
                    "Relaunching Service '{}' in a new Service Session (relaunch {} of {}): {}",
                    inst.service.name,
                    inst.relaunches,
                    max_attempts,
                    k.describe()
                ));
                end_instance(&mut inst).await;
            } else {
                log_line(format!(
                    "Relaunching Service '{}' onRun in its Service Session (relaunch {} of {}): {}",
                    inst.service.name,
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
                    "Service '{}' ({} scope) is UNREADY: {}",
                    inst.service.name,
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
        log_banner(&format!("Starting Service: {}", inst.service.name));
        log_line(format!(
            "Service '{}' ({} scope) endpoints: {}",
            inst.service.name,
            inst.scope,
            describe_endpoints(&endpoints)
        ));
        inst.endpoints = Some(endpoints.clone());
        let mut session = ServiceSession::with_config(ServiceSessionConfig {
            session: session_config(shared, &inst.service.name),
            service: inst.service.clone(),
            environments: inst.environments.clone(),
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
        // The exit was observed on the watch channel; the Session moves to
        // EXITED (and reclaims its driver) when the exit is awaited here.
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
        "Service '{}' onRun launched (launch {} in this Session); readiness check: {}",
        inst.service.name,
        session.launch_count(),
        inst.service.readiness_check.type_name()
    ));
    let readiness = tokio::select! {
        r = session.wait_ready() => r,
        _ = stop.cancelled() => return Ok(false),
    };
    match readiness {
        Ok(ServiceReadiness::Ready { message }) => {
            let suffix = message.map(|m| format!(": {m}")).unwrap_or_default();
            log_line(format!("Service '{}' is READY{suffix}", inst.service.name));
            Ok(true)
        }
        Ok(ServiceReadiness::TimedOut) => {
            // RFC 0009 "Failure and restart" step 2: cancel onRun and wait
            // for it to exit before deciding.
            session.cancel_run(None);
            let _ = session.wait_exit().await;
            Err(FailureKind::ReadinessTimedOut)
        }
        Ok(ServiceReadiness::ExitedBeforeReady | ServiceReadiness::Pending) => {
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
            fail_message: Some("boom".into()),
            stdout: String::new(),
        };
        let start = FailureKind::Start("port 1 unavailable".into());
        let before = FailureKind::ExitedBeforeReady(exit.clone());
        let timed_out = FailureKind::ReadinessTimedOut;
        let exited = FailureKind::Exited(exit);
        assert!(start.requires_new_session());
        assert!(before.requires_new_session());
        assert!(!timed_out.requires_new_session());
        assert!(!exited.requires_new_session());
        assert_eq!(start.describe(), "failed to start: port 1 unavailable");
        assert_eq!(
            before.describe(),
            "onRun exited before becoming READY (exit code: 3; boom)"
        );
        assert_eq!(timed_out.describe(), "readiness check timed out");
        assert_eq!(
            exited.describe(),
            "onRun exited while the scope still had work (exit code: 3; boom)"
        );
        assert_eq!(
            describe_exit(&driver_lost_exit()),
            "failed; onRun driver ended without reporting an exit"
        );
    }

    #[test]
    fn service_failure_display_names_scope() {
        let f = ServiceFailure {
            name: "Cache".into(),
            scope: ServiceScope::Step("Render".into()),
            reason: "readiness check timed out; 1 of 1 relaunch(es) used".into(),
        };
        assert_eq!(
            f.to_string(),
            "Service 'Cache' (Step 'Render' scope) failed: readiness check timed out; 1 of 1 \
             relaunch(es) used"
        );
        assert_eq!(ServiceScope::Job.to_string(), "Job");
    }
}
