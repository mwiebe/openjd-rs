// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `openjd run` command — run a job template locally.

mod execution;
mod params;
mod result;
mod simulator;

pub use params::parse_cli_parameters;

use clap::Args;
use openjd_model::job::{Environment, Job, Step};
use openjd_model::template::parse;
use openjd_model::template::EnvironmentTemplate;
use openjd_model::types::{JobParameterValues, ModelProfile, TaskParameterSet};
use openjd_model::StepDependencyGraph;
use openjd_sessions::action::ActionState;
use openjd_sessions::path_mapping::PathMappingRule;
use openjd_sessions::session::{EnvironmentOptions, EnvironmentOrigin, RunTaskOptions, Session};
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
    job_template: openjd_model::template::JobTemplate,
    validation_ctx: openjd_model::types::ValidationContext,
    job: Job,
    env_templates: Vec<EnvironmentTemplate>,
    param_values: JobParameterValues,
    path_rules: Vec<PathMappingRule>,
    revision_profile: ModelProfile,
}

struct RunSelection {
    /// The Step or loop `--step` names, or the Job's only top-level entry.
    selected: Option<simulator::Node>,
    explicit_task_params: Option<Vec<HashMap<String, String>>>,
    /// The top-level Steps and loops to run, in dependency order.
    to_run: Vec<simulator::Node>,
}

struct EnteredEnvironment {
    identifier: String,
    name: String,
    symtab: EnvSymtab,
    /// Passed again when the Environment is exited.
    origin: EnvironmentOrigin,
    /// A Step Environment's Step's path-typed `Steps.*` inputs, passed again
    /// on exit.
    path_inputs: Vec<openjd_model::job::PathInput>,
}

/// The result of one Action that ran a Task (or a chunk of Tasks).
struct TaskRun {
    duration: f64,
    succeeded: bool,
    /// The values its Tasks reported, when the Step declares output
    /// variables and the Action succeeded.
    outputs: Option<openjd_sessions::action::ActionOutputs>,
}

struct RunContext {
    session: Session,
    entered_envs: Vec<EnteredEnvironment>,
    interrupted: Arc<AtomicBool>,
    started_at: Instant,
    tasks_run: usize,
    session_failed: bool,
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

    async fn enter_environment(
        &mut self,
        env: &Environment,
        name: &str,
        symtab: EnvSymtab,
        origin: EnvironmentOrigin,
        path_inputs: &[openjd_model::job::PathInput],
    ) {
        self.print_action_banner(&format!("Entering Environment: {name}"));
        let options = EnvironmentOptions {
            origin,
            path_inputs,
        };
        match self
            .session
            .enter_environment(env, symtab.as_ref(), None, None, &options)
            .await
        {
            Ok(identifier) => self.entered_envs.push(EnteredEnvironment {
                identifier,
                name: name.to_string(),
                symtab,
                origin,
                path_inputs: path_inputs.to_vec(),
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
                            origin,
                            path_inputs: path_inputs.to_vec(),
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
                .exit_environment(
                    &entered.identifier,
                    entered.symtab.as_ref(),
                    true,
                    None,
                    &EnvironmentOptions {
                        origin: entered.origin,
                        path_inputs: &entered.path_inputs,
                    },
                )
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
    ) -> Result<TaskRun, RunError> {
        let task_start = Instant::now();
        let result = self
            .session
            .run_task(
                &step.name,
                &step.script,
                task_values,
                step.resolved_symtab.as_ref(),
                None,
                &RunTaskOptions {
                    output_variables: step.output_variables.as_deref(),
                    path_inputs: &step.path_inputs,
                },
            )
            .await
            .map_err(|e| format!("Step '{}': {e}", step.name))?;
        let duration = task_start.elapsed().as_secs_f64();

        println!(
            "{}\tProcess exited with code: {}",
            self.timestamp(),
            result.exit_code.unwrap_or(-1)
        );
        self.tasks_run += 1;
        let succeeded = result.state == ActionState::Success;
        if !succeeded {
            self.session_failed = true;
        }
        Ok(TaskRun {
            duration,
            succeeded,
            outputs: result.outputs,
        })
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

    /// Environment template files
    #[arg(long = "environment", alias = "env")]
    pub environments: Vec<PathBuf>,

    /// Path mapping rules (file://path or inline JSON). Must have version 'pathmapping-1.0'.
    #[arg(long = "path-mapping-rules")]
    pub path_mapping_rules: Option<String>,

    /// CONTROL_FLOW: the Steps.* output values that the Step given with --step reads, for
    /// running it without --run-dependencies (file://path, or inline JSON
    /// '{"Steps.P.Output.V": value}'; a list with one value per Task for a Step with a
    /// parameterSpace).
    #[arg(long = "step-inputs")]
    pub step_inputs: Option<String>,

    /// Run dependency steps before the target step
    #[arg(long = "run-dependencies")]
    pub run_dependencies: bool,

    /// Do not run dependency steps (default)
    #[arg(long = "no-run-dependencies")]
    pub no_run_dependencies: bool,

    /// Maximum number of tasks to run per Step (-1 for all; the default). For a loop,
    /// applies to each body Step instance; an error with --step naming a loop.
    /// Mutually exclusive with --task-param and --tasks.
    #[arg(long = "maximum-tasks", allow_negative_numbers = true)]
    pub maximum_tasks: Option<i64>,

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

/// Resolve the dependencies of top-level entry `target` (a Step or loop)
/// transitively, returning them and `target` in execution order.
fn resolve_step_dependencies(
    job: &openjd_model::job::Job,
    target: simulator::Node,
) -> Vec<simulator::Node> {
    use simulator::Node;
    let mut name_to_node: HashMap<&str, Node> = job
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.as_str(), Node::Step(i)))
        .collect();
    name_to_node.extend(
        job.loops
            .iter()
            .enumerate()
            .map(|(i, l)| (l.name.as_str(), Node::Loop(i))),
    );
    let deps_of = |node: Node| match node {
        Node::Step(i) => job.steps[i].dependencies.as_ref(),
        Node::Loop(i) => job.loops[i].dependencies.as_ref(),
    };
    let mut visited = std::collections::HashSet::new();
    let mut order = Vec::new();
    fn visit<'a>(
        node: Node,
        deps_of: &dyn Fn(Node) -> Option<&'a Vec<openjd_model::job::StepDependency>>,
        name_to_node: &HashMap<&str, Node>,
        visited: &mut std::collections::HashSet<Node>,
        order: &mut Vec<Node>,
    ) {
        if !visited.insert(node) {
            return;
        }
        for dep in deps_of(node).into_iter().flatten() {
            if let Some(&dep_node) = name_to_node.get(dep.depends_on.as_str()) {
                visit(dep_node, deps_of, name_to_node, visited, order);
            }
        }
        order.push(node);
    }
    visit(target, &deps_of, &name_to_node, &mut visited, &mut order);
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
            control_flow: false,
            output_callback: None,
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
        };

        ctx.record_interruption();
        assert!(!ctx.session_failed);

        interrupted.store(true, Ordering::SeqCst);
        ctx.record_interruption();
        assert!(ctx.session_failed);
        ctx.session.cleanup();
    }
}
