// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `openjd run` command — run a job template locally.

mod execution;
mod params;
mod result;

pub use params::parse_cli_parameters;

use clap::Args;
use openjd_model::job::{Document, Environment, EnvironmentParameterValues, Job, Step};
use openjd_model::template::parse;
use openjd_model::types::{JobParameterValues, ModelProfile, TaskParameterSet};
use openjd_model::StepDependencyGraph;
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

type RunError = Box<dyn std::error::Error>;
type EnvSymtab = Option<openjd_expr::SerializedSymbolTable>;

struct PreparedRun {
    /// The Job combined with the attached Environment Templates: their
    /// Environments lead `job_environments`, and each Step's
    /// `environment_parameters` covers them.
    job: Job,
    /// The document of each entry of `job.job_environments`, index for
    /// index.
    environment_documents: Vec<Document>,
    param_values: JobParameterValues,
    path_rules: Vec<PathMappingRule>,
    revision_profile: ModelProfile,
}

struct RunSelection {
    selected_step_idx: Option<usize>,
    explicit_task_params: Option<Vec<HashMap<String, String>>>,
    steps_to_run: Vec<usize>,
}

struct EnteredEnvironment {
    identifier: String,
    name: String,
    symtab: EnvSymtab,
    /// Which Job Environment this is, and the values it was entered with;
    /// `None` for a Step Environment.
    job_environment: Option<EnteredJobEnvironment>,
}

/// The identity of an entered Job Environment and the Environment
/// parameter values the Session entered it with (Template Schemas §3.2.2).
struct EnteredJobEnvironment {
    /// Index into the combined Job's `job_environments`, which names the
    /// Environment by `(document, name)`.
    index: usize,
    values: Option<EnvironmentParameterValues>,
}

impl EnteredJobEnvironment {
    /// Whether a Step that enters `target` can keep this entry: the same
    /// Job Environment, entered with values equal to the Step's
    /// ([`EnvironmentParameterValues::values_equal`]), or without values on
    /// both sides.
    fn matches(&self, target: &JobEnvironmentTarget<'_>) -> bool {
        self.index == target.index
            && match (&self.values, target.values) {
                (None, None) => true,
                (Some(entered), Some(wanted)) => entered.values_equal(wanted),
                _ => false,
            }
    }
}

/// A Job Environment a Step enters, with the Step's values for it.
struct JobEnvironmentTarget<'a> {
    /// Index into the combined Job's `job_environments`.
    index: usize,
    env: &'a Environment,
    values: Option<&'a EnvironmentParameterValues>,
}

struct RunContext {
    session: Session,
    entered_envs: Vec<EnteredEnvironment>,
    interrupted: Arc<AtomicBool>,
    started_at: Instant,
    tasks_run: usize,
    session_failed: bool,
    /// The Session's working directory, once the Session has ended with
    /// `--preserve`.
    preserved_dir: Option<PathBuf>,
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

    /// End the Session: keep its working directory when `preserve`
    /// (recording it for the result), else remove it.
    fn end_session(&mut self, preserve: bool) {
        if preserve {
            self.preserved_dir = Some(self.session.working_directory().to_path_buf());
        } else {
            self.session.cleanup();
        }
    }

    /// Print the preserved working directory, for a run that ends with an
    /// error before its result is reported.
    fn print_preserved_dir(&self) {
        if let Some(dir) = &self.preserved_dir {
            println!("Working directory preserved at: {}", dir.display());
        }
    }

    /// Make the Session's entered Job Environments exactly `targets`, in
    /// order (Template Schemas §3.2.2): exit entered Environments, from the
    /// most recently entered, until what remains is a prefix of `targets`
    /// whose every entry [`matches`](EnteredJobEnvironment::matches), then
    /// enter the rest of `targets` in order. Called between Steps, when no
    /// Step Environment is entered. Exits go through
    /// [`exit_environments_down_to`](Self::exit_environments_down_to), so
    /// an Environment entered under a wrapper is exited through its
    /// `onWrapEnvExit` while the wrapper is still entered.
    async fn move_to_job_environments(&mut self, targets: &[JobEnvironmentTarget<'_>]) {
        let kept = self
            .entered_envs
            .iter()
            .zip(targets)
            .take_while(|(entered, target)| {
                entered
                    .job_environment
                    .as_ref()
                    .is_some_and(|job_env| job_env.matches(target))
            })
            .count();
        self.exit_environments_down_to(kept).await;
        for target in &targets[kept..] {
            if self.is_stopping() {
                break;
            }
            // The Environment's own frozen table carries the symbols job
            // creation resolved for it (`Job.Name`, `Job.EnvNames`, and an
            // attached Environment's own `Param.*`); a Session builds an
            // Environment's scope from the table it is given.
            self.enter_environment(
                target.env,
                &target.env.name,
                target.env.resolved_symtab.clone(),
                target.values,
                Some(target.index),
            )
            .await;
        }
    }

    /// Enter `env`, with `values` as its Environment parameter values
    /// (`ENV_PARAMS`) when it declares parameters. The values are printed
    /// after the banner, as given (before path mapping). `job_index` is the
    /// Environment's index in the combined Job's `job_environments`, or
    /// `None` for a Step Environment.
    async fn enter_environment(
        &mut self,
        env: &Environment,
        name: &str,
        symtab: EnvSymtab,
        values: Option<&EnvironmentParameterValues>,
        job_index: Option<usize>,
    ) {
        let job_environment = job_index.map(|index| EnteredJobEnvironment {
            index,
            values: values.cloned(),
        });
        self.print_action_banner(&format!("Entering Environment: {name}"));
        if let Some(values) = values {
            println!("{}\tEnvironment parameter values:", self.timestamp());
            for (name, ty, value) in crate::common::environment_parameter_entries(values) {
                println!("{}\t{name}({ty}) = {value}", self.timestamp());
            }
        }
        match self
            .session
            .enter_environment_with_parameters(env, symtab.as_ref(), values, None, None)
            .await
        {
            Ok((identifier, _output)) => self.entered_envs.push(EnteredEnvironment {
                identifier,
                name: name.to_string(),
                symtab,
                job_environment,
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
                            name: name.to_string(),
                            symtab,
                            job_environment,
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
            self.print_action_banner(&format!("Exiting Environment: {}", entered.name));
            if let Err(e) = self
                .session
                .exit_environment(&entered.identifier, entered.symtab.as_ref(), true, None)
                .await
            {
                eprintln!("ERROR: Environment teardown failed: {e}");
                self.session_failed = true;
            }
        }
    }

    async fn run_task(
        &mut self,
        step: &Step,
        task_values: Option<&TaskParameterSet>,
    ) -> Result<f64, RunError> {
        let task_start = Instant::now();
        let result = self
            .session
            .run_task(
                &step.name,
                &step.script,
                task_values,
                step.resolved_symtab.as_ref(),
                None,
            )
            .await
            .map_err(|e| format!("Step '{}': {e}", step.name))?;
        let task_duration = task_start.elapsed().as_secs_f64();

        println!(
            "{}\tProcess exited with code: {}",
            self.timestamp(),
            result.exit_code.unwrap_or(-1)
        );
        self.tasks_run += 1;
        if result.state != ActionState::Success {
            self.session_failed = true;
        }
        Ok(task_duration)
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
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
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

    /// Environment Template files, attached in the order given; their
    /// Environments are entered before the Job's own (repeatable)
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

#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub async fn execute(args: RunArgs) -> Result<(), RunError> {
    execution::execute(args).await
}

/// Resolve step dependencies transitively, returning indices in execution order.
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
                if let Some(&dep_idx) = name_to_idx.get(&dep.depends_on) {
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
            preserved_dir: None,
        };

        ctx.record_interruption();
        assert!(!ctx.session_failed);

        interrupted.store(true, Ordering::SeqCst);
        ctx.record_interruption();
        assert!(ctx.session_failed);
        ctx.session.cleanup();
    }
}
