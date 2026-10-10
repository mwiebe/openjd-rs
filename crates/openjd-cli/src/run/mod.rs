// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `openjd run` command — run a job template locally.

mod execution;
mod params;
mod result;
mod service_ports;
mod services;

pub use params::parse_cli_parameters;

use clap::Args;
use openjd_expr::SerializedSymbolTable;
use openjd_model::job::service_symbols::build_service_symbol_table;
use openjd_model::job::{Document, Environment, Job, RunScope, ServiceScope, Step};
use openjd_model::template::parse;
use openjd_model::types::{JobParameterValues, ModelProfile, TaskParameterSet};
use openjd_model::{RequirementBinding, StepDependencyGraph};
use openjd_sessions::action::ActionState;
use openjd_sessions::path_mapping::PathMappingRule;
use openjd_sessions::session::Session;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

use params::*;
use result::RunResult;
use services::{merge_scopes, ServiceFailure, ServiceManager, Visibility};

type RunError = Box<dyn std::error::Error>;
type EnvSymtab = Option<SerializedSymbolTable>;

struct PreparedRun {
    /// The combined Job: `apply_environment_templates` has placed the
    /// `--environment` templates' Services before the Job Template's
    /// `services` and their Environments before its `jobEnvironments`.
    job: Job,
    /// The document of each entry of `job.job_environments`, index for
    /// index: the attached Environments carry their template, the Job
    /// Template's own `Document::JobTemplate`. Decides which document's
    /// `Service.*` symbols each Job Environment sees, and (through
    /// [`profile_for`](Self::profile_for)) which document's extension
    /// profile its format strings are evaluated with.
    environment_documents: Vec<Document>,
    /// The attached Service each `requiresServices` entry of the Job
    /// Template was bound to at submission (Template Schemas §1.2.2 item
    /// 2).
    requirement_bindings: Vec<RequirementBinding>,
    param_values: JobParameterValues,
    path_rules: Vec<PathMappingRule>,
    /// The Job Template's revision + extensions profile: the Task Session's
    /// profile, and that of every entity the Job Template declares.
    revision_profile: ModelProfile,
    /// The profile of each `--environment` template, by attachment index
    /// (the index in `Document::EnvironmentTemplate`). An extension applies
    /// to the document that lists it (Template Schemas §1.2 item 3), so an
    /// attached Environment's strings — and an external Service's — are
    /// evaluated under their own template's profile, not the Job's.
    attached_profiles: Vec<ModelProfile>,
}

impl PreparedRun {
    /// The extension profile of the document `document`: the Job Template's
    /// for [`Document::JobTemplate`], the attached template's for an
    /// [`Document::EnvironmentTemplate`].
    fn profile_for(&self, document: &Document) -> &ModelProfile {
        profile_for(&self.revision_profile, &self.attached_profiles, document)
    }
}

/// The profile of `document` given the Job Template's `job_profile` and the
/// attachments' `attached_profiles` (by attachment index). Shared by the
/// Task Session side ([`PreparedRun`]) and the Service side
/// (`ServiceRunConfig`).
fn profile_for<'a>(
    job_profile: &'a ModelProfile,
    attached_profiles: &'a [ModelProfile],
    document: &Document,
) -> &'a ModelProfile {
    match document {
        Document::JobTemplate => job_profile,
        Document::EnvironmentTemplate { index, .. } => {
            attached_profiles.get(*index).unwrap_or(job_profile)
        }
    }
}

struct RunSelection {
    selected_step_idx: Option<usize>,
    explicit_task_params: Option<Vec<HashMap<String, String>>>,
    steps_to_run: Vec<usize>,
}

#[derive(Clone)]
struct EnteredEnvironment {
    identifier: String,
    /// The Environment as entered, so it can be re-entered when the
    /// `Service.*` endpoints it may reference change.
    env: Environment,
    /// The document that declares the Environment: the only document whose
    /// Services it may reference (plus the bound required Services, for the
    /// Job Template's).
    document: Document,
    /// The Step whose `stepEnvironments` this is, if any: its Task Session
    /// sees the inline Services whose scope includes the Step.
    step: Option<String>,
    /// That document's extension profile when it is not the Job Template's
    /// (see `RunContext::enter_environment`), kept for re-entry.
    profile: Option<ModelProfile>,
    /// The symbol table the caller supplied (before `Service.*` symbols were
    /// layered on), the input to a re-entry.
    symtab: EnvSymtab,
    /// The symbol table the Environment was entered with — `symtab` with the
    /// `Service.*` symbols of its document's READY Services layered on — so
    /// its `onExit` resolves the same `Service.*` values its `onEnter` and
    /// `variables` did.
    resolved: EnvSymtab,
}

/// How one Task ended, from the run loop's point of view.
enum TaskRun {
    /// The Task ran to an exit; the duration is in seconds.
    Completed(f64),
    /// A Service with `completedTasks: RERUN` failed: the Task was canceled
    /// (not a Task failure) and the completed Tasks of every Step in the
    /// scope return to the queue.
    Rerun(ServiceScope),
    /// The run is stopping (a Service failed, or an interruption); no Task
    /// ran.
    Aborted,
}

struct RunContext {
    session: Session,
    entered_envs: Vec<EnteredEnvironment>,
    interrupted: Arc<AtomicBool>,
    started_at: Instant,
    tasks_run: usize,
    session_failed: bool,
    services: ServiceManager,
    failed_services: Vec<ServiceFailure>,
    /// `entered_envs.len()` before the current Step's Environments were
    /// entered; the level a Step-scoped Service's endpoint change refreshes
    /// down to.
    step_env_baseline: usize,
    /// `Param.*` / `RawParam.*` for the whole submission, the base of an
    /// Environment's symbol table when it has no `resolved_symtab` of its
    /// own and `Service.*` symbols must be layered on.
    base_symtab: SerializedSymbolTable,
    /// The run's interruption token, shared by every Session of the run.
    cancel_token: CancellationToken,
    /// Working directories of Task Sessions replaced after a RERUN, kept
    /// under `--preserve` and reported with the result.
    preserved_working_dirs: Vec<PathBuf>,
}

impl RunContext {
    fn timestamp(&self) -> String {
        crate::format_log_timestamp()
    }

    fn print_action_banner(&self, label: &str) {
        println!("{}\t", self.timestamp());
        self.print_banner(label);
    }

    fn print_banner(&self, label: &str) {
        println!(
            "{}\t==============================================",
            self.timestamp()
        );
        println!("{}\t--------- {label}", self.timestamp());
        println!(
            "{}\t==============================================",
            self.timestamp()
        );
    }

    fn is_stopping(&self) -> bool {
        self.session_failed || self.interrupted.load(Ordering::SeqCst)
    }

    /// The symbol table a Task Session action resolves against: `base` (or
    /// `fallback`, or the submission's `Param.*` table) with the
    /// `Service.<name>.<port>.port` / `.connectAddress` of every READY
    /// Service the entity may see layered on (RFC 0009 "The `Service.*`
    /// scope"; Template Schemas §1.2.2 items 2–3): for a Task or Step
    /// Environment ([`Visibility::Step`]) the Job Template's inline Services
    /// whose scope includes the Step and the attached Services bound to its
    /// `requiresServices`; for a Job Environment
    /// ([`Visibility::Environment`]) the Services of `document` — and, for
    /// the Job Template's own, the bound requirements — it lists in its
    /// `dependencies`. Without Services in scope, `base` unchanged — the
    /// pre-RFC-0009 behavior.
    fn task_symtab(
        &self,
        base: Option<&SerializedSymbolTable>,
        fallback: Option<&SerializedSymbolTable>,
        document: &Document,
        visibility: Visibility<'_>,
    ) -> Result<EnvSymtab, RunError> {
        let in_scope = self.services.task_scope_endpoints(document, visibility);
        if in_scope.is_empty() {
            return Ok(base.cloned());
        }
        let service_table = build_service_symbol_table(&in_scope, None)
            .map_err(|e| format!("Failed to build the Service.* symbol table: {e}"))?;
        let service_entries = SerializedSymbolTable::from_symtab(&service_table)
            .as_value()
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut entries = base
            .or(fallback)
            .unwrap_or(&self.base_symtab)
            .as_value()
            .as_array()
            .cloned()
            .unwrap_or_default();
        entries.extend(service_entries);
        Ok(Some(SerializedSymbolTable::from_value(
            serde_json::Value::Array(entries),
        )))
    }

    /// Enter `env`, declared by `document`, in the Task Session unless its
    /// effective `runScope` excludes `TASK` (RFC 0009 `<Environment>`),
    /// layering the `Service.*` symbols in scope (see
    /// [`task_symtab`](Self::task_symtab); `step` names the Step for a Step
    /// Environment, which follows the Step's `dependencies` — a Job
    /// Environment, `step` `None`, follows its own) onto `symtab` (or the
    /// Environment's own `resolved_symtab`). `profile` is that document's
    /// extension profile:
    /// `Some` for an attached Environment Template, whose strings are then
    /// evaluated under its own extensions rather than the Job Template's
    /// (the Task Session's profile); `None` for the Job Template's own
    /// Environments.
    async fn enter_environment(
        &mut self,
        env: &Environment,
        symtab: EnvSymtab,
        document: Document,
        profile: Option<&ModelProfile>,
        step: Option<&str>,
    ) {
        if !env.runs_in(RunScope::Task) {
            println!(
                "{}\tSkipping Environment '{}': its runScope does not include TASK",
                self.timestamp(),
                env.name
            );
            return;
        }
        let visibility = step.map_or(Visibility::Environment(env), Visibility::Step);
        let resolved = match self.task_symtab(
            symtab.as_ref(),
            env.resolved_symtab.as_ref(),
            &document,
            visibility,
        ) {
            Ok(resolved) => resolved,
            Err(e) => {
                eprintln!("ERROR: Environment setup failed: {e}");
                self.session_failed = true;
                return;
            }
        };
        self.print_action_banner(&format!("Entering Environment: {}", env.name));
        match self
            .session
            .enter_environment_with_profile(env, resolved.as_ref(), None, None, profile)
            .await
        {
            Ok((identifier, _stdout)) => self.entered_envs.push(EnteredEnvironment {
                identifier,
                env: env.clone(),
                document,
                step: step.map(str::to_string),
                profile: profile.cloned(),
                symtab,
                resolved,
            }),
            Err(e) => {
                eprintln!("ERROR: Environment setup failed: {e}");
                if let Some(code) = self.session.action_status().and_then(|s| s.exit_code) {
                    println!("{}\tProcess exited with code: {code}", self.timestamp());
                }

                // Failed enter actions remain on the Session stack and must
                // still be exited. Rejections before entry do not.
                if let Some(identifier) = self.session.environments_entered().last() {
                    if !self
                        .entered_envs
                        .iter()
                        .any(|entered| &entered.identifier == identifier)
                    {
                        self.entered_envs.push(EnteredEnvironment {
                            identifier: identifier.clone(),
                            env: env.clone(),
                            document,
                            step: step.map(str::to_string),
                            profile: profile.cloned(),
                            symtab,
                            resolved,
                        });
                    }
                }
                self.session_failed = true;
            }
        }
    }

    async fn exit_environments_down_to(&mut self, baseline: usize) {
        while self.entered_envs.len() > baseline {
            let entered = self
                .entered_envs
                .pop()
                .expect("environment count checked by loop guard");
            self.print_action_banner(&format!("Exiting Environment: {}", entered.env.name));
            if let Err(e) = self
                .session
                .exit_environment(&entered.identifier, entered.resolved.as_ref(), true, None)
                .await
            {
                eprintln!("ERROR: Environment teardown failed: {e}");
                self.session_failed = true;
            }
        }
    }

    /// Exit the Environments above `baseline` and enter them again, so that
    /// their `Service.*` values are re-resolved against the current
    /// endpoints after a Service began a new Service Session.
    async fn refresh_environments_from(&mut self, baseline: usize) {
        if self.entered_envs.len() <= baseline {
            return;
        }
        println!(
            "{}\tRe-entering {} Environment(s): a Service they may reference has new endpoints",
            self.timestamp(),
            self.entered_envs.len() - baseline
        );
        let to_reenter: Vec<EnteredEnvironment> = self.entered_envs[baseline..].to_vec();
        self.exit_environments_down_to(baseline).await;
        for e in to_reenter {
            if self.is_stopping() {
                break;
            }
            self.enter_environment(
                &e.env,
                e.symtab,
                e.document,
                e.profile.as_ref(),
                e.step.as_deref(),
            )
            .await;
        }
    }

    /// RFC 0009 ordering constraint 3, the readiness gate: start every
    /// active Service that is not yet READY and wait for it. Records a
    /// FAILED Service as a failed run and refreshes the Task Session's
    /// Environments when a Service's endpoints changed. Returns the Steps
    /// whose completed Tasks a `RERUN` relaunch returned to the queue.
    async fn gate_services(&mut self) -> Result<Option<ServiceScope>, RunError> {
        if !self.services.any_registered() {
            return Ok(None);
        }
        let outcome = self.services.gate().await?;
        if let Some(failure) = outcome.failure {
            self.record_service_failure(failure);
            return Ok(None);
        }
        if outcome.job_wide_endpoints_changed {
            self.refresh_environments_from(0).await;
        } else if outcome.step_endpoints_changed {
            let baseline = self.step_env_baseline;
            self.refresh_environments_from(baseline).await;
        }
        Ok(outcome.rerun)
    }

    /// Replace the Task Session after a `RERUN`: the old Session (ending-only
    /// after its canceled Task, with every Environment already exited) is
    /// cleaned up unless preserved, and `replacement` runs the requeued
    /// Tasks.
    fn replace_task_session(&mut self, replacement: Session, preserve: bool) {
        debug_assert!(self.entered_envs.is_empty());
        let mut old = std::mem::replace(&mut self.session, replacement);
        if preserve {
            self.preserved_working_dirs
                .push(old.working_directory().to_path_buf());
        } else {
            old.cleanup();
        }
        println!(
            "{}\tNew Task Session for the requeued Tasks: {}",
            self.timestamp(),
            self.session.working_directory().display()
        );
    }

    fn record_service_failure(&mut self, failure: ServiceFailure) {
        self.failed_services.push(failure);
        self.session_failed = true;
    }

    /// Run one Task of `step` after the readiness gate, watching the READY
    /// Services' `onRun` for an exit while it runs (RFC 0009 "Failure and
    /// restart" step 1): under `RERUN` the Task is canceled and requeued;
    /// under `KEEP` it continues while the Service relaunches.
    ///
    /// `param_lines` are printed under the "Running Task" banner (one
    /// `name = value` line per Task parameter).
    async fn run_task(
        &mut self,
        step: &Step,
        task_values: Option<&TaskParameterSet>,
        param_lines: &[String],
    ) -> Result<TaskRun, RunError> {
        if let Some(scope) = self.gate_services().await? {
            return Ok(TaskRun::Rerun(scope));
        }
        if self.is_stopping() {
            return Ok(TaskRun::Aborted);
        }
        // A Task belongs to the Job Template: it sees the inline Services
        // whose scope includes its Step and the bound required Services.
        let symtab = self.task_symtab(
            step.resolved_symtab.as_ref(),
            None,
            &Document::JobTemplate,
            Visibility::Step(&step.name),
        )?;
        self.print_banner("Running Task");
        if !param_lines.is_empty() {
            println!("{}\tParameter values:", self.timestamp());
            for line in param_lines {
                println!("{}\t{line}", self.timestamp());
            }
        }
        let cancel = self.session.cancel_handle();
        let task_start = Instant::now();
        let mut rerun: Option<ServiceScope> = None;
        let result = {
            let Self {
                session, services, ..
            } = self;
            let mut task = Box::pin(session.run_task(
                &step.name,
                &step.script,
                task_values,
                symtab.as_ref(),
                None,
            ));
            loop {
                let detected = tokio::select! {
                    biased;
                    r = &mut task => break r,
                    d = services.wait_instance_failure() => d,
                };
                let (policy, scope) = services.begin_recovery(detected);
                if policy == Some(openjd_model::job::CompletedTasksPolicy::Rerun) {
                    println!(
                        "{}\tCanceling the running Task of Step '{}': a Service with \
                         completedTasks: RERUN is UNREADY; the Task returns to the queue",
                        crate::format_log_timestamp(),
                        step.name
                    );
                    cancel.cancel(None, false);
                    rerun = Some(merge_scopes(rerun.take(), &scope));
                    break task.await;
                }
            }
        };
        let result = result.map_err(|e| format!("Step '{}': {e}", step.name))?;
        let task_duration = task_start.elapsed().as_secs_f64();

        if let Some(scope) = rerun {
            println!(
                "{}\tTask canceled; it returns to the queue (not a Task failure)",
                self.timestamp()
            );
            return Ok(TaskRun::Rerun(scope));
        }
        println!(
            "{}\tProcess exited with code: {}",
            self.timestamp(),
            result.exit_code.unwrap_or(-1)
        );
        self.tasks_run += 1;
        if result.state != ActionState::Success {
            self.session_failed = true;
        }
        Ok(TaskRun::Completed(task_duration))
    }

    fn record_interruption(&mut self) {
        if self.interrupted.load(Ordering::SeqCst) {
            println!("{}\tInterruption signal received.", self.timestamp());
            self.session_failed = true;
        }
    }
}

/// Strip the `\\?\` extended-length path prefix that Rust's `canonicalize()` and
/// `current_dir()` add on Windows. Most tools (bash, Python, etc.) don't understand it.
pub fn strip_extended_prefix(path: &std::path::Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        path.to_path_buf()
    }
}

#[derive(Args)]
pub struct RunArgs {
    /// Path to the job template file
    pub path: PathBuf,

    /// The name of the Step to run (if omitted, runs all steps; auto-selects if only one step)
    #[arg(long)]
    pub step: Option<String>,

    /// Job parameters (Key=Value, file://path, or inline JSON '{"Key": "Value"}')
    #[arg(short = 'p', long = "job-param", alias = "parameter")]
    pub parameters: Vec<String>,

    /// Run a single task with explicit parameter values (PARAM=VALUE, repeatable).
    /// Mutually exclusive with --tasks and --maximum-tasks.
    #[arg(long = "task-param", short = 't', action = clap::ArgAction::Append, conflicts_with_all = ["tasks", "maximum_tasks"])]
    pub task_params: Vec<String>,

    /// Run specific tasks from a JSON/YAML file (file://path) or inline JSON array.
    /// Mutually exclusive with --task-param and --maximum-tasks.
    #[arg(long = "tasks", conflicts_with_all = ["task_params", "maximum_tasks"])]
    pub tasks: Option<String>,

    /// Environment template files
    #[arg(long = "environment", alias = "env")]
    pub environments: Vec<PathBuf>,

    /// Path mapping rules (file://path or inline JSON). Must have version 'pathmapping-1.0'.
    #[arg(long = "path-mapping-rules")]
    pub path_mapping_rules: Option<String>,

    /// Run dependency steps before the target step
    #[arg(long = "run-dependencies")]
    pub run_dependencies: bool,

    /// Do not run dependency steps (default)
    #[arg(long = "no-run-dependencies")]
    pub no_run_dependencies: bool,

    /// Maximum number of tasks to run (-1 for all).
    /// Mutually exclusive with --task-param and --tasks.
    #[arg(long = "maximum-tasks", default_value = "-1")]
    pub maximum_tasks: i64,

    /// Extensions to support (comma-separated or repeated). Empty string disables all.
    #[arg(long = "extensions")]
    pub extensions: Option<String>,

    /// Preserve session working directory after completion
    #[arg(long)]
    pub preserve: bool,

    /// Enable verbose logging while running the Session
    #[arg(long)]
    pub verbose: bool,

    /// How to format log output timestamps
    #[arg(long = "timestamp-format", value_parser = ["relative", "local", "utc"], default_value = "relative")]
    pub timestamp_format: String,

    /// How to format the command's output
    #[arg(long = "output", value_parser = ["human-readable", "json", "yaml"], default_value = "human-readable")]
    pub output: String,
}

pub async fn execute(args: RunArgs) -> Result<(), RunError> {
    execution::execute(args).await
}

/// The Job with each Service's Step `dependencies` folded into its Steps'
/// (RFC 0009 §9 item 4): a Step in the scope of a Service that depends on
/// Step `D` cannot run before `D` has completed, since its Tasks wait for
/// the Service and the Service waits for `D`. A Service's scope already
/// includes the Steps that reach it through other Services (§9.1 rule 2),
/// so a chain `Use -> Service Front -> Service Back -> Prepare` folds
/// `Prepare` into `Use` through Back's scope. The result orders the run so
/// the implied edges hold; the Job's own `steps` are not changed otherwise.
/// A Service whose scope is every Step lists no Step (validation rejects
/// it), so nothing is folded for it; `service` entries are the Service
/// gate's concern and are not folded.
fn with_implied_step_dependencies(job: &Job) -> Job {
    let mut ordered = job.clone();
    for service in job.services.iter().flatten() {
        let Some(scope) = service.scope.step_names() else {
            continue;
        };
        let step_deps: Vec<&str> = service.depends_on_steps().collect();
        if step_deps.is_empty() {
            continue;
        }
        for step in ordered.steps.iter_mut().filter(|s| scope.contains(&s.name)) {
            let existing = step.dependencies.get_or_insert_with(Vec::new);
            for dep in &step_deps {
                if *dep != step.name && !existing.iter().any(|d| d.step() == Some(dep)) {
                    existing.push(openjd_model::job::StepDependency::on_step(*dep));
                }
            }
        }
    }
    ordered
}

/// Resolve step dependencies transitively, returning indices in execution
/// order. A `service` entry (RFC 0009) names a Service, not a Step, and is
/// skipped.
fn resolve_step_dependencies(job: &openjd_model::job::Job, target_idx: usize) -> Vec<usize> {
    let step_name_to_idx: HashMap<String, usize> = job
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.clone(), i))
        .collect();
    let mut visited = std::collections::HashSet::new();
    let mut order = Vec::new();
    fn visit(
        job: &openjd_model::job::Job,
        idx: usize,
        name_to_idx: &HashMap<String, usize>,
        visited: &mut std::collections::HashSet<usize>,
        order: &mut Vec<usize>,
    ) {
        if !visited.insert(idx) {
            return;
        }
        if let Some(deps) = &job.steps[idx].dependencies {
            for dep in deps {
                let Some(step) = dep.step() else {
                    continue;
                };
                if let Some(&dep_idx) = name_to_idx.get(step) {
                    visit(job, dep_idx, name_to_idx, visited, order);
                }
            }
        }
        order.push(idx);
    }
    visit(job, target_idx, &step_name_to_idx, &mut visited, &mut order);
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_interruption_marks_run_failed() {
        let root = tempfile::tempdir().unwrap();
        let session = Session::with_config(openjd_sessions::session::SessionConfig {
            session_id: "interrupt-test".to_string(),
            job_parameter_values: HashMap::new(),
            path_mapping_rules: None,
            retain_working_dir: false,
            callback: None,
            os_env_vars: None,
            session_root_directory: Some(root.path().to_path_buf()),
            user: None,
            profile: None,
            cancel_token: None,
            sticky_bit_policy: Default::default(),
            debug_collect_stdout: false,
            echo_openjd_directives: true,
            log_tag: None,
            limits: Default::default(),
        })
        .unwrap();
        let interrupted = Arc::new(AtomicBool::new(false));
        let mut ctx = RunContext {
            session,
            entered_envs: Vec::new(),
            interrupted: interrupted.clone(),
            started_at: Instant::now(),
            tasks_run: 0,
            session_failed: false,
            services: ServiceManager::new(services::ServiceRunConfig {
                job_parameter_values: HashMap::new(),
                path_mapping_rules: None,
                retain_working_dir: false,
                profile: openjd_model::ModelProfile::default(),
                attached_profiles: Vec::new(),
                requirement_bindings: Vec::new(),
                cancel_token: CancellationToken::new(),
                limits: Default::default(),
            }),
            failed_services: Vec::new(),
            step_env_baseline: 0,
            base_symtab: SerializedSymbolTable::from_symtab(&openjd_expr::SymbolTable::new()),
            cancel_token: CancellationToken::new(),
            preserved_working_dirs: Vec::new(),
        };

        ctx.record_interruption();
        assert!(!ctx.session_failed);

        interrupted.store(true, Ordering::SeqCst);
        ctx.record_interruption();
        assert!(ctx.session_failed);
        ctx.session.cleanup();
    }
}
