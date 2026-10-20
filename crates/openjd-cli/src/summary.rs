// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `openjd summary` command — print summary information about a Job Template.

use clap::Args;
use openjd_model::job;
use openjd_model::template::parse;
use std::path::PathBuf;

#[derive(Args)]
pub struct SummaryArgs {
    /// Path to the job template file
    pub path: PathBuf,

    /// Print information about this Step only
    #[arg(long)]
    pub step: Option<String>,

    /// Job parameters (Key=Value, file://path, or inline JSON)
    #[arg(short = 'p', long = "job-param", alias = "parameter")]
    pub parameters: Vec<String>,

    /// Extensions to support (comma-separated). Empty string disables all.
    #[arg(long = "extensions")]
    pub extensions: Option<String>,

    /// Environment template files
    #[arg(long = "environment", alias = "env")]
    pub environments: Vec<PathBuf>,

    /// How to format the command's output
    #[arg(long = "output", value_parser = ["human-readable", "json", "yaml"], default_value = "human-readable")]
    pub output: String,
}

pub fn execute(args: SummaryArgs) -> Result<(), Box<dyn std::error::Error>> {
    let path = &args.path;
    let content = crate::common::read_input_file(path)?;
    let template_value = parse::document_string_to_object(
        &content,
        crate::common::document_type(path),
        &crate::common::caller_limits(),
    )?;

    let exts = crate::common::parse_extensions(&args.extensions)?;
    let supported_exts: Vec<&str> = exts.iter().map(|s| s.as_str()).collect();

    let job_template = parse::decode_job_template(
        template_value,
        Some(&supported_exts),
        &crate::common::caller_limits(),
    )?;

    // Preserve template parameter definition order
    let param_order: Vec<String> = job_template
        .parameter_definitions_list()
        .iter()
        .map(|p| p.name().to_string())
        .collect();
    let param_descriptions: std::collections::HashMap<&str, &str> = job_template
        .parameter_definitions_list()
        .iter()
        .filter_map(|p| p.description().map(|d| (p.name(), d)))
        .collect();

    // Load environment templates
    let mut env_templates = Vec::new();
    for env_path in &args.environments {
        let env_content = std::fs::read_to_string(env_path)?;
        let env_value = parse::document_string_to_object(
            &env_content,
            crate::common::document_type(env_path),
            &crate::common::caller_limits(),
        )?;
        env_templates.push(parse::decode_environment_template(
            env_value,
            Some(&supported_exts),
            &crate::common::caller_limits(),
        )?);
    }

    // Parse parameters
    let input_values = crate::run::parse_cli_parameters(&args.parameters)?;
    let job_template_dir = crate::run::strip_extended_prefix(
        std::fs::canonicalize(path)?
            .parent()
            .unwrap_or_else(|| std::path::Path::new(".")),
    );
    let current_working_dir = crate::run::strip_extended_prefix(&std::env::current_dir()?);
    let param_values = openjd_model::preprocess_job_parameters(
        &job_template,
        &input_values,
        &env_templates,
        &openjd_model::PathParameterOptions {
            job_template_dir: job_template_dir.to_str().unwrap_or("."),
            current_working_dir: current_working_dir.to_str().unwrap_or("."),
            path_format: openjd_expr::path_mapping::PathFormat::host(),
            allow_template_dir_walk_up: false,
            allow_uri_path_values: true,
        },
    )?;

    // Derive the context from the template itself — `create_job`
    // requires the context's revision/extensions to cover the
    // template's — and carry the CLI's caller limits so job creation
    // enforces the same caps the decode ran under.
    let ctx = job_template
        .default_validation_context()
        .with_caller_limits(crate::common::caller_limits());

    let the_job = openjd_model::create_job(&job_template, &param_values, &ctx)?;
    let control_flow = job_template
        .profile()
        .has_extension(openjd_model::types::ModelExtension::ControlFlow);
    // Task parameter names and types come from the template, so a pending
    // Step, whose parameter space is not materialized, still lists them.
    let task_params: std::collections::HashMap<String, Vec<(String, String)>> = job_template
        .step_templates()
        .map(|r| r.step)
        .map(|step| {
            let mut v: Vec<(String, String)> = step
                .parameter_space
                .iter()
                .flat_map(|ps| &ps.task_parameter_definitions)
                .map(|tp| {
                    (
                        tp.name().to_string(),
                        tp.task_param_type().as_spec_str().to_string(),
                    )
                })
                .collect();
            v.sort_by(|a, b| a.0.cmp(&b.0));
            (step.name.clone(), v)
        })
        .collect();

    if let Some(step_name) = &args.step {
        if let Some(l) = the_job.loops.iter().find(|l| l.name == *step_name) {
            let info = loop_info(&job_template, l, &task_params);
            let result = LoopSummaryResult {
                job_name: the_job.name.clone(),
                info,
            };
            crate::common::print_cli_result(&result, &args.output);
            return Ok(());
        }
        if let Some(l) = the_job
            .loops
            .iter()
            .find(|l| l.body.iter().any(|b| b == step_name))
        {
            return Err(format!(
                "Step '{step_name}' is a body Step of loop '{}'; summarize the loop with --step {}.",
                l.name, l.name
            )
            .into());
        }
        output_step_summary(
            &the_job,
            step_name,
            &task_params,
            control_flow,
            &args.output,
        )
    } else {
        output_job_summary(
            &job_template,
            &the_job,
            &param_order,
            &param_descriptions,
            &task_params,
            control_flow,
            &args.output,
        )
    }
}

/// RFC 0013's name for a loop's state at Job creation.
fn loop_state(job_loop: &job::Loop) -> &'static str {
    match job_loop.binding {
        job::LoopBinding::Bound(_) => "READY",
        job::LoopBinding::PendingInputs(_) => "PENDING_INPUTS",
        job::LoopBinding::Skipped => "SKIPPED",
    }
}

/// A pending loop's binding report as JSON, as [`binding_json`] is a
/// Step's.
fn loop_binding_json(job_loop: &job::Loop) -> Option<serde_json::Value> {
    let report = job_loop.binding_report()?;
    Some(serde_json::json!({
        "enablement": group_json(&report.enablement),
        "bound": group_json(&report.bound),
        "start": group_json(&report.start),
    }))
}

/// What the summary shows of loop `l` (plan L8, Reporting): its condition,
/// its bound, its variables and outputs, and its body Steps, whose Tasks are
/// determined at run time in each iteration.
fn loop_info(
    jt: &openjd_model::template::JobTemplate,
    l: &job::Loop,
    task_params: &std::collections::HashMap<String, Vec<(String, String)>>,
) -> LoopInfo {
    let body_templates: Vec<&openjd_model::template::StepTemplate> = jt
        .steps
        .get(l.entry_index)
        .and_then(|e| e.as_loop())
        .map(|t| t.body().iter().collect())
        .unwrap_or_default();
    let body = body_templates
        .iter()
        .map(|st| BodyStepInfo {
            name: st.name.clone(),
            task_params: task_params.get(&st.name).cloned().unwrap_or_default(),
            envs: st
                .step_environments
                .iter()
                .flatten()
                .map(|e| EnvInfo {
                    name: e.name.clone(),
                    description: e.description.as_ref().map(|d| d.0.clone()),
                    parent: st.name.clone(),
                    parent_loop: Some(l.name.clone()),
                    enabled: true,
                })
                .collect(),
            deps: st
                .dependencies
                .iter()
                .flatten()
                .map(|d| d.depends_on.clone())
                .collect(),
        })
        .collect();
    let max_iterations = match &l.binding {
        job::LoopBinding::Bound(b) => Some(b.max_iterations),
        job::LoopBinding::PendingInputs(p) => match p.bound {
            job::GroupState::Known(n) => Some(n),
            job::GroupState::Waiting { .. } => None,
        },
        job::LoopBinding::Skipped => None,
    };
    let zero_iterations = matches!(&l.binding, job::LoopBinding::Bound(b) if !b.has_iterations());
    LoopInfo {
        name: l.name.clone(),
        description: l.description.clone(),
        state: loop_state(l),
        condition: match l.condition {
            job::LoopCondition::While => "while",
            job::LoopCondition::Until => "until",
        },
        max_iterations,
        zero_iterations,
        variables: l
            .variables
            .iter()
            .map(|v| (v.name.clone(), v.param_type.as_spec_str().to_string()))
            .collect(),
        outputs: l
            .output_variables
            .iter()
            .map(|o| LoopOutputInfo {
                name: o.name.clone(),
                output_type: o.param_type.as_spec_str().to_string(),
                collect: o.collect.as_spec_str(),
                unit: o.unit.clone(),
            })
            .collect(),
        body,
        deps: l
            .dependencies
            .iter()
            .flatten()
            .map(|d| d.depends_on.clone())
            .collect(),
        binding_lines: crate::common::loop_binding_lines(l),
        binding_json: loop_binding_json(l),
    }
}

/// A Step's Task count, exact on every target (a `u64`, like
/// `StepParameterSpaceIterator::len`), or `None` while it is determined at
/// run time (a pending Step whose Tasks are not known). Fails, naming the
/// Step, when the count of a bound Step cannot be computed: a chunked Step
/// can have more than `u64::MAX` Tasks, which counting one Task per value
/// overflows.
fn step_total_tasks(step: &job::Step) -> Result<Option<u64>, String> {
    if let Some(n) = step.tasks_known() {
        return Ok(Some(n));
    }
    match (&step.binding, &step.parameter_space) {
        (job::StepBinding::Bound, Some(ps)) => {
            openjd_model::StepParameterSpaceIterator::new_with_chunk_override(ps, Some(1))
                .map(|it| Some(it.len()))
                .map_err(|e| format!("Step '{}': {e}", step.name))
        }
        _ => Ok(None),
    }
}

/// RFC 0011's name for a `CONTROL_FLOW` Step's state at Job creation.
fn step_state(step: &job::Step) -> &'static str {
    match step.binding {
        job::StepBinding::Bound => "READY",
        job::StepBinding::PendingInputs(_) => "PENDING_INPUTS",
        job::StepBinding::Skipped => "SKIPPED",
    }
}

/// A pending Step's binding report as JSON: for each group, whether it is
/// known and, if not, the `Steps.*` symbols it waits on and whether it
/// waits for the Step's `enableIf`.
fn binding_json(step: &job::Step) -> Option<serde_json::Value> {
    let report = step.binding_report()?;
    Some(serde_json::json!({
        "enablement": group_json(&report.enablement),
        "tasks": group_json(&report.tasks),
        "host_requirements": group_json(&report.host_requirements),
        "remaining": group_json(&report.remaining),
    }))
}

/// One group of a binding report as JSON.
fn group_json(g: &job::GroupReport) -> serde_json::Value {
    match g {
        job::GroupReport::Known => serde_json::json!({"known": true}),
        job::GroupReport::Waiting { on, for_enablement } => serde_json::json!({
            "known": false,
            "waits_on": on,
            "waits_for_enable_if": for_enablement,
        }),
    }
}

/// What is known of each job output at Job creation.
fn job_output_infos(job: &job::Job) -> Vec<JobOutputInfo> {
    job.outputs
        .iter()
        .flatten()
        .enumerate()
        .map(|(i, o)| JobOutputInfo {
            name: o.declaration.name.clone(),
            output_type: o.declaration.param_type.as_spec_str().to_string(),
            unit: o.declaration.unit.clone(),
            description: o.description.clone(),
            value: o.evaluated.clone(),
            waits_on: openjd_model::job_output_inputs(job, i)
                .into_iter()
                .filter(|s| !crate::common::concrete_at_creation(job, s))
                .collect(),
        })
        .collect()
}

fn output_job_summary(
    jt: &openjd_model::template::JobTemplate,
    job: &job::Job,
    param_order: &[String],
    param_descriptions: &std::collections::HashMap<&str, &str>,
    template_task_params: &std::collections::HashMap<String, Vec<(String, String)>>,
    control_flow: bool,
    output_format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Collect step summaries
    let mut step_envs_info: Vec<EnvInfo> = Vec::new();
    let graph = openjd_model::StepDependencyGraph::new(job)?;
    let step_summaries: Vec<StepInfo> = (0..graph.node_count())
        .filter_map(|n| graph.node(n))
        .map(|node| {
            if node.kind == openjd_model::step_dependency_graph::StepNodeKind::Loop {
                let l = &job.loops[node.step_index];
                let info = loop_info(jt, l, template_task_params);
                for b in &info.body {
                    step_envs_info.extend(b.envs.iter().cloned());
                }
                return Ok(StepInfo {
                    name: l.name.clone(),
                    description: l.description.clone(),
                    total_tasks: info.total_tasks(),
                    task_params: Vec::new(),
                    envs: Vec::new(),
                    deps: info.deps.clone(),
                    state: control_flow.then(|| loop_state(l)),
                    binding_lines: Vec::new(),
                    binding_json: None,
                    loop_info: Some(info),
                });
            }
            let s = &job.steps[node.step_index];
            let total_tasks = step_total_tasks(s)?;
            let task_params = template_task_params
                .get(&s.name)
                .cloned()
                .unwrap_or_default();
            let envs: Vec<String> = s
                .step_environments
                .as_ref()
                .map(|envs| envs.iter().map(|e| e.name.clone()).collect())
                .unwrap_or_default();
            if let Some(envs) = &s.step_environments {
                for e in envs {
                    step_envs_info.push(EnvInfo {
                        name: e.name.clone(),
                        description: e.description.clone(),
                        parent: s.name.clone(),
                        parent_loop: None,
                        enabled: e.enabled,
                    });
                }
            }
            let deps: Vec<String> = s
                .dependencies
                .as_ref()
                .map(|deps| deps.iter().map(|d| d.depends_on.clone()).collect())
                .unwrap_or_default();
            Ok(StepInfo {
                name: s.name.clone(),
                description: s.description.clone(),
                total_tasks,
                task_params,
                envs,
                deps,
                state: control_flow.then(|| step_state(s)),
                binding_lines: crate::common::binding_lines(s),
                binding_json: binding_json(s),
                loop_info: None,
            })
        })
        .collect::<Result<_, String>>()?;

    // Each step's count is an exact u64, but their sum can pass u64::MAX
    // (two steps of 2^63 tasks). Fail rather than print a wrapped total.
    let total_tasks = step_summaries
        .iter()
        .filter_map(|s| s.total_tasks)
        .try_fold(0u64, |acc, n| acc.checked_add(n))
        .ok_or_else(|| {
            format!(
                "Job '{}' has more than {} tasks in total, so the total cannot be reported.",
                job.name,
                u64::MAX
            )
        })?;
    let tasks_at_run_time = step_summaries.iter().any(|s| s.total_tasks.is_none());
    let step_envs = step_envs_info.len();
    let root_envs: Vec<EnvInfo> = job
        .job_environments
        .as_ref()
        .map(|envs| {
            envs.iter()
                .map(|e| EnvInfo {
                    name: e.name.clone(),
                    description: e.description.clone(),
                    parent: "root".into(),
                    parent_loop: None,
                    enabled: e.enabled,
                })
                .collect()
        })
        .unwrap_or_default();
    let total_envs = root_envs.len() + step_envs;

    // Collect parameter summaries in template definition order
    let params: Vec<ParamInfo> = param_order
        .iter()
        .filter_map(|name| {
            job.parameters.get(name).map(|p| ParamInfo {
                name: name.clone(),
                description: param_descriptions.get(name.as_str()).map(|s| s.to_string()),
                param_type: p.param_type.as_spec_str().to_string(),
                value: p.value.to_display_string(),
            })
        })
        .collect();

    let result = JobSummaryResult {
        name: job.name.clone(),
        params,
        step_summaries,
        root_envs,
        step_envs: step_envs_info,
        total_tasks,
        tasks_at_run_time,
        total_envs,
        job_outputs: job_output_infos(job),
    };
    crate::common::print_cli_result(&result, output_format);
    Ok(())
}

fn output_step_summary(
    job: &job::Job,
    step_name: &str,
    template_task_params: &std::collections::HashMap<String, Vec<(String, String)>>,
    control_flow: bool,
    output_format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let step = job
        .steps
        .iter()
        .find(|s| s.name == step_name)
        .ok_or_else(|| format!("Step '{step_name}' does not exist in Job '{}'.", job.name))?;

    let total_tasks = step_total_tasks(step)?;
    let task_params = template_task_params
        .get(&step.name)
        .cloned()
        .unwrap_or_default();
    let envs: Vec<EnvInfo> = step
        .step_environments
        .as_ref()
        .map(|envs| {
            envs.iter()
                .map(|e| EnvInfo {
                    name: e.name.clone(),
                    description: e.description.clone(),
                    parent: step.name.clone(),
                    parent_loop: None,
                    enabled: e.enabled,
                })
                .collect()
        })
        .unwrap_or_default();
    let deps: Vec<String> = step
        .dependencies
        .as_ref()
        .map(|deps| deps.iter().map(|d| d.depends_on.clone()).collect())
        .unwrap_or_default();

    let result = StepSummaryResult {
        job_name: job.name.clone(),
        step_name: step_name.to_string(),
        total_tasks,
        task_params,
        envs,
        deps,
        state: control_flow.then(|| step_state(step)),
        binding_lines: crate::common::binding_lines(step),
        binding_json: binding_json(step),
    };
    crate::common::print_cli_result(&result, output_format);
    Ok(())
}

// --- Result types ---

use crate::common::CliResult;
use std::fmt;

struct JobSummaryResult {
    name: String,
    params: Vec<ParamInfo>,
    step_summaries: Vec<StepInfo>,
    root_envs: Vec<EnvInfo>,
    step_envs: Vec<EnvInfo>,
    /// The Tasks of every Step whose Tasks are known.
    total_tasks: u64,
    /// Whether some Step's Tasks are determined at run time.
    tasks_at_run_time: bool,
    total_envs: usize,
    job_outputs: Vec<JobOutputInfo>,
}

impl JobSummaryResult {
    /// The Steps of the template: each top-level Step and each body Step,
    /// which counts once (RFC 0013 §Limits item 3).
    fn total_steps(&self) -> usize {
        self.step_summaries
            .iter()
            .map(|s| s.loop_info.as_ref().map_or(1, |l| l.body.len()))
            .sum()
    }

    fn loop_count(&self) -> usize {
        self.step_summaries
            .iter()
            .filter(|s| s.loop_info.is_some())
            .count()
    }
}

impl CliResult for JobSummaryResult {
    fn to_json_value(&self) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        obj.insert("status".into(), "success".into());
        obj.insert(
            "message".into(),
            format!("Summary for '{}'", self.name).into(),
        );
        obj.insert("name".into(), self.name.clone().into());
        if !self.params.is_empty() {
            obj.insert(
                "parameter_definitions".into(),
                serde_json::json!(self
                    .params
                    .iter()
                    .map(|p| {
                        let mut m = serde_json::Map::new();
                        m.insert("name".into(), p.name.clone().into());
                        if let Some(d) = &p.description {
                            m.insert("description".into(), d.clone().into());
                        }
                        m.insert("type".into(), p.param_type.clone().into());
                        m.insert("value".into(), p.value.clone().into());
                        serde_json::Value::Object(m)
                    })
                    .collect::<Vec<_>>()),
            );
        }
        obj.insert("total_steps".into(), self.total_steps().into());
        let loops = self.loop_count();
        if loops > 0 {
            obj.insert("total_loops".into(), loops.into());
        }
        obj.insert(
            "total_tasks".into(),
            serde_json::Value::Number(self.total_tasks.into()),
        );
        if self.tasks_at_run_time {
            obj.insert("tasks_determined_at_run_time".into(), true.into());
        }
        if self.total_envs > 0 {
            obj.insert("total_environments".into(), self.total_envs.into());
        }
        if !self.root_envs.is_empty() {
            obj.insert(
                "root_environments".into(),
                serde_json::json!(self
                    .root_envs
                    .iter()
                    .map(EnvInfo::to_json)
                    .collect::<Vec<_>>()),
            );
        }
        let steps_json: Vec<serde_json::Value> = self
            .step_summaries
            .iter()
            .map(|s| {
                if let Some(info) = &s.loop_info {
                    return info.to_json();
                }
                let mut m = serde_json::Map::new();
                m.insert("name".into(), s.name.clone().into());
                if let Some(d) = &s.description {
                    m.insert("description".into(), d.clone().into());
                }
                m.insert("total_tasks".into(), s.total_tasks.into());
                if let Some(state) = s.state {
                    m.insert("state".into(), state.into());
                }
                if let Some(binding) = &s.binding_json {
                    m.insert("binding".into(), binding.clone());
                }
                if !s.task_params.is_empty() {
                    m.insert(
                        "parameter_definitions".into(),
                        serde_json::json!(s
                            .task_params
                            .iter()
                            .map(|(n, t)| serde_json::json!({"name": n, "type": t}))
                            .collect::<Vec<_>>()),
                    );
                }
                if !s.envs.is_empty() {
                    m.insert("environments".into(), s.envs.len().into());
                }
                if !s.deps.is_empty() {
                    m.insert("dependencies".into(), s.deps.len().into());
                }
                serde_json::Value::Object(m)
            })
            .collect();
        obj.insert("steps".into(), steps_json.into());
        if !self.job_outputs.is_empty() {
            obj.insert(
                "job_outputs".into(),
                self.job_outputs
                    .iter()
                    .map(JobOutputInfo::to_json)
                    .collect::<Vec<_>>()
                    .into(),
            );
        }
        serde_json::Value::Object(obj)
    }
}

impl fmt::Display for JobSummaryResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f)?;
        writeln!(f, "--- Summary for '{}' ---", self.name)?;
        if !self.params.is_empty() {
            writeln!(f)?;
            writeln!(f, "Parameters:")?;
            for p in &self.params {
                if p.value.is_empty() {
                    writeln!(f, "  - {} ({})", p.name, p.param_type)?;
                } else {
                    writeln!(f, "  - {} ({}): {}", p.name, p.param_type, p.value)?;
                }
            }
        }
        writeln!(f)?;
        writeln!(f, "Total steps: {}", self.total_steps())?;
        let loops = self.loop_count();
        if loops > 0 {
            writeln!(f, "Total loops: {loops}")?;
        }
        if self.tasks_at_run_time {
            writeln!(
                f,
                "Total tasks: {} + (determined at run time)",
                self.total_tasks
            )?;
        } else {
            writeln!(f, "Total tasks: {}", self.total_tasks)?;
        }
        writeln!(f, "Total environments: {}", self.total_envs)?;
        writeln!(f)?;
        writeln!(f, "--- Steps in '{}' ---", self.name)?;
        writeln!(f)?;
        for (i, s) in self.step_summaries.iter().enumerate() {
            if let Some(info) = &s.loop_info {
                writeln!(f, "{}. '{}' ({})", i + 1, s.name, info.heading())?;
                info.write_details(f, "  ")?;
                writeln!(f)?;
                continue;
            }
            writeln!(f, "{}. '{}' ({})", i + 1, s.name, s.heading())?;
            for line in &s.binding_lines {
                writeln!(f, "  {line}")?;
            }
            if !s.task_params.is_empty() {
                writeln!(f, "  Task parameters:")?;
                for (name, tp) in &s.task_params {
                    writeln!(f, "    - {name} ({tp})")?;
                }
            }
            if !s.envs.is_empty() {
                writeln!(f, "  {} environments", s.envs.len())?;
            }
            if !s.deps.is_empty() {
                writeln!(f, "  {} dependencies", s.deps.len())?;
            }
            writeln!(f)?;
        }
        if self.total_envs > 0 {
            writeln!(f)?;
            writeln!(f, "--- Environments in '{}' ---", self.name)?;
            for e in self.root_envs.iter().chain(&self.step_envs) {
                write!(f, "  - {}", e.heading())?;
                if let Some(d) = &e.description {
                    write!(f, "\n {d}")?;
                }
                writeln!(f)?;
            }
        }
        if !self.job_outputs.is_empty() {
            writeln!(f)?;
            writeln!(f, "--- Job outputs in '{}' ---", self.name)?;
            for o in &self.job_outputs {
                writeln!(f, "  - {}", o.line())?;
                if let Some(d) = &o.description {
                    writeln!(f, "    {d}")?;
                }
            }
        }
        Ok(())
    }
}

struct StepSummaryResult {
    job_name: String,
    step_name: String,
    total_tasks: Option<u64>,
    task_params: Vec<(String, String)>,
    envs: Vec<EnvInfo>,
    deps: Vec<String>,
    state: Option<&'static str>,
    binding_lines: Vec<String>,
    binding_json: Option<serde_json::Value>,
}

impl CliResult for StepSummaryResult {
    fn to_json_value(&self) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        obj.insert("status".into(), "success".into());
        obj.insert(
            "message".into(),
            format!(
                "Summary for Step '{}' in Job '{}'",
                self.step_name, self.job_name
            )
            .into(),
        );
        obj.insert("job_name".into(), self.job_name.clone().into());
        obj.insert("step_name".into(), self.step_name.clone().into());
        obj.insert("total_tasks".into(), self.total_tasks.into());
        if let Some(state) = self.state {
            obj.insert("state".into(), state.into());
        }
        if let Some(binding) = &self.binding_json {
            obj.insert("binding".into(), binding.clone());
        }
        obj.insert("total_parameters".into(), self.task_params.len().into());
        obj.insert("total_environments".into(), self.envs.len().into());
        if !self.deps.is_empty() {
            obj.insert(
                "dependencies".into(),
                serde_json::json!(self
                    .deps
                    .iter()
                    .map(|d| serde_json::json!({"step_name": d}))
                    .collect::<Vec<_>>()),
            );
        }
        if !self.task_params.is_empty() {
            obj.insert(
                "parameter_definitions".into(),
                serde_json::json!(self
                    .task_params
                    .iter()
                    .map(|(n, t)| serde_json::json!({"name": n, "type": t}))
                    .collect::<Vec<_>>()),
            );
        }
        serde_json::Value::Object(obj)
    }
}

impl fmt::Display for StepSummaryResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f)?;
        writeln!(
            f,
            "--- Summary for Step '{}' in Job '{}' ---",
            self.step_name, self.job_name
        )?;
        writeln!(f)?;
        match (self.total_tasks, self.state) {
            (Some(n), Some("SKIPPED")) => writeln!(f, "Total tasks: {n} (SKIPPED)")?,
            (Some(n), _) => writeln!(f, "Total tasks: {n}")?,
            (None, _) => writeln!(f, "Total tasks: determined at run time")?,
        }
        for line in &self.binding_lines {
            writeln!(f, "{line}")?;
        }
        writeln!(f, "Total task parameters: {}", self.task_params.len())?;
        writeln!(f, "Total environments: {}", self.envs.len())?;
        if !self.deps.is_empty() {
            writeln!(f)?;
            writeln!(f, "Dependencies ({}):", self.deps.len())?;
            for d in &self.deps {
                writeln!(f, "- '{d}'")?;
            }
        }
        if !self.task_params.is_empty() {
            writeln!(f)?;
            writeln!(f, "Parameters:")?;
            for (name, tp) in &self.task_params {
                writeln!(f, "- {name} ({tp})")?;
            }
        }
        if !self.envs.is_empty() {
            writeln!(f)?;
            writeln!(f, "Environments:")?;
            for e in &self.envs {
                write!(f, "- {}", e.heading())?;
                if let Some(d) = &e.description {
                    write!(f, "\n {d}")?;
                }
                writeln!(f)?;
            }
        }
        Ok(())
    }
}

// --- Data types ---

struct StepInfo {
    name: String,
    description: Option<String>,
    /// `None` while the Step's Tasks are determined at run time.
    total_tasks: Option<u64>,
    task_params: Vec<(String, String)>,
    envs: Vec<String>,
    deps: Vec<String>,
    /// The Step's state, for a template that lists `CONTROL_FLOW`.
    state: Option<&'static str>,
    /// What is known of a pending Step (`common::binding_lines`).
    binding_lines: Vec<String>,
    binding_json: Option<serde_json::Value>,
    /// For a loop, what the summary shows of it.
    loop_info: Option<LoopInfo>,
}

/// A loop in the summary (RFC 0013 §Iterations: a tool SHOULD show a loop's
/// bound and its body).
struct LoopInfo {
    name: String,
    description: Option<String>,
    state: &'static str,
    /// `while` or `until`.
    condition: &'static str,
    /// `maxIterations`, once known.
    max_iterations: Option<u32>,
    /// Whether its first `while` is false at Job creation.
    zero_iterations: bool,
    /// Each loop variable's name and type.
    variables: Vec<(String, String)>,
    outputs: Vec<LoopOutputInfo>,
    body: Vec<BodyStepInfo>,
    deps: Vec<String>,
    /// What is known of a pending loop (`common::loop_binding_lines`).
    binding_lines: Vec<String>,
    binding_json: Option<serde_json::Value>,
}

/// A loop output in the summary.
struct LoopOutputInfo {
    name: String,
    output_type: String,
    collect: &'static str,
    unit: Option<String>,
}

/// A body Step in the summary.
struct BodyStepInfo {
    name: String,
    task_params: Vec<(String, String)>,
    envs: Vec<EnvInfo>,
    deps: Vec<String>,
}

impl LoopInfo {
    /// The loop's Task count at Job creation: 0 when it is skipped or its
    /// first `while` is false, and `None` otherwise, since it is not known
    /// until the loop ends (RFC 0013 §Iterations).
    fn total_tasks(&self) -> Option<u64> {
        (self.state == "SKIPPED" || self.zero_iterations).then_some(0)
    }

    /// The parenthesized part of the loop's line in the human-readable
    /// summary.
    fn heading(&self) -> String {
        match self.state {
            "SKIPPED" => "loop; SKIPPED, 0 total Tasks".to_string(),
            "PENDING_INPUTS" => "loop; pending inputs; Tasks determined at run time".to_string(),
            _ if self.zero_iterations => "loop; 0 iterations, 0 total Tasks".to_string(),
            _ => "loop; Tasks determined at run time".to_string(),
        }
    }

    /// The loop's lines under its heading, each after `indent`.
    fn write_details(&self, f: &mut fmt::Formatter<'_>, indent: &str) -> fmt::Result {
        for line in &self.binding_lines {
            writeln!(f, "{indent}{line}")?;
        }
        writeln!(f, "{indent}Condition: {}", self.condition)?;
        if self.binding_lines.is_empty() {
            match self.max_iterations {
                Some(n) => writeln!(f, "{indent}maxIterations: {n}")?,
                None => writeln!(
                    f,
                    "{indent}maxIterations: not evaluated (the loop is skipped)"
                )?,
            }
        }
        if !self.variables.is_empty() {
            writeln!(f, "{indent}Loop variables:")?;
            for (name, ty) in &self.variables {
                writeln!(f, "{indent}  - {name} ({ty})")?;
            }
        }
        if !self.outputs.is_empty() {
            writeln!(f, "{indent}Loop outputs:")?;
            for o in &self.outputs {
                match &o.unit {
                    Some(unit) => writeln!(
                        f,
                        "{indent}  - {} ({}, {}, {unit})",
                        o.name, o.output_type, o.collect
                    )?,
                    None => writeln!(
                        f,
                        "{indent}  - {} ({}, {})",
                        o.name, o.output_type, o.collect
                    )?,
                }
            }
        }
        writeln!(
            f,
            "{indent}Body Steps (Tasks determined at run time in each iteration):"
        )?;
        for b in &self.body {
            writeln!(f, "{indent}  - '{}'", b.name)?;
            if !b.task_params.is_empty() {
                writeln!(f, "{indent}    Task parameters:")?;
                for (name, tp) in &b.task_params {
                    writeln!(f, "{indent}      - {name} ({tp})")?;
                }
            }
            if !b.envs.is_empty() {
                writeln!(f, "{indent}    {} environments", b.envs.len())?;
            }
            if !b.deps.is_empty() {
                writeln!(f, "{indent}    {} dependencies", b.deps.len())?;
            }
        }
        if !self.deps.is_empty() {
            writeln!(f, "{indent}{} dependencies", self.deps.len())?;
        }
        Ok(())
    }

    fn to_json(&self) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        m.insert("name".into(), self.name.clone().into());
        m.insert("kind".into(), "loop".into());
        if let Some(d) = &self.description {
            m.insert("description".into(), d.clone().into());
        }
        m.insert("total_tasks".into(), self.total_tasks().into());
        m.insert("state".into(), self.state.into());
        if let Some(binding) = &self.binding_json {
            m.insert("binding".into(), binding.clone());
        }
        m.insert("condition".into(), self.condition.into());
        if let Some(n) = self.max_iterations {
            m.insert("max_iterations".into(), n.into());
        }
        if !self.variables.is_empty() {
            m.insert(
                "variables".into(),
                serde_json::json!(self
                    .variables
                    .iter()
                    .map(|(n, t)| serde_json::json!({"name": n, "type": t}))
                    .collect::<Vec<_>>()),
            );
        }
        if !self.outputs.is_empty() {
            m.insert(
                "outputs".into(),
                serde_json::json!(self
                    .outputs
                    .iter()
                    .map(|o| {
                        let mut v = serde_json::json!({
                            "name": o.name, "type": o.output_type, "collect": o.collect,
                        });
                        if let Some(unit) = &o.unit {
                            v["unit"] = unit.clone().into();
                        }
                        v
                    })
                    .collect::<Vec<_>>()),
            );
        }
        m.insert(
            "body".into(),
            serde_json::json!(self
                .body
                .iter()
                .map(|b| {
                    let mut v = serde_json::Map::new();
                    v.insert("name".into(), b.name.clone().into());
                    if !b.task_params.is_empty() {
                        v.insert(
                            "parameter_definitions".into(),
                            serde_json::json!(b
                                .task_params
                                .iter()
                                .map(|(n, t)| serde_json::json!({"name": n, "type": t}))
                                .collect::<Vec<_>>()),
                        );
                    }
                    if !b.envs.is_empty() {
                        v.insert("environments".into(), b.envs.len().into());
                    }
                    if !b.deps.is_empty() {
                        v.insert("dependencies".into(), b.deps.len().into());
                    }
                    serde_json::Value::Object(v)
                })
                .collect::<Vec<_>>()),
        );
        if !self.deps.is_empty() {
            m.insert("dependencies".into(), self.deps.len().into());
        }
        serde_json::Value::Object(m)
    }
}

/// `openjd summary --step <loop>`.
struct LoopSummaryResult {
    job_name: String,
    info: LoopInfo,
}

impl CliResult for LoopSummaryResult {
    fn to_json_value(&self) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        obj.insert("status".into(), "success".into());
        obj.insert(
            "message".into(),
            format!(
                "Summary for loop '{}' in Job '{}'",
                self.info.name, self.job_name
            )
            .into(),
        );
        obj.insert("job_name".into(), self.job_name.clone().into());
        obj.insert("step_name".into(), self.info.name.clone().into());
        if let serde_json::Value::Object(m) = self.info.to_json() {
            for (k, v) in m {
                if k != "name" {
                    obj.insert(k, v);
                }
            }
        }
        if !self.info.deps.is_empty() {
            obj.insert(
                "dependencies".into(),
                serde_json::json!(self
                    .info
                    .deps
                    .iter()
                    .map(|d| serde_json::json!({"step_name": d}))
                    .collect::<Vec<_>>()),
            );
        }
        serde_json::Value::Object(obj)
    }
}

impl fmt::Display for LoopSummaryResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f)?;
        writeln!(
            f,
            "--- Summary for loop '{}' in Job '{}' ---",
            self.info.name, self.job_name
        )?;
        writeln!(f)?;
        writeln!(f, "State: {} ({})", self.info.state, self.info.heading())?;
        self.info.write_details(f, "")?;
        if !self.info.deps.is_empty() {
            writeln!(f)?;
            writeln!(f, "Dependencies ({}):", self.info.deps.len())?;
            for d in &self.info.deps {
                writeln!(f, "- '{d}'")?;
            }
        }
        Ok(())
    }
}

impl StepInfo {
    /// The parenthesized part of the Step's line in the human-readable
    /// summary.
    fn heading(&self) -> String {
        match (self.state, self.total_tasks) {
            (Some("SKIPPED"), _) => "SKIPPED, 0 total Tasks".to_string(),
            (Some("PENDING_INPUTS"), Some(n)) => format!("{n} total Tasks; pending inputs"),
            (Some("PENDING_INPUTS"), None) => {
                "pending inputs; Tasks determined at run time".to_string()
            }
            (_, n) => format!("{} total Tasks", n.unwrap_or(0)),
        }
    }
}

/// A job output in the summary.
struct JobOutputInfo {
    name: String,
    output_type: String,
    unit: Option<String>,
    description: Option<String>,
    /// Its value, when `create_job` evaluated it.
    value: Option<job::JobOutputValue>,
    /// The `Steps.*` symbols it waits on that are not concrete yet.
    waits_on: Vec<String>,
}

impl JobOutputInfo {
    /// `Name (TYPE, unit): value`, or `...: pending; waits for <symbols>`.
    fn line(&self) -> String {
        let kind = match &self.unit {
            Some(unit) => format!("{}, {unit}", self.output_type),
            None => self.output_type.clone(),
        };
        let value = match &self.value {
            Some(job::JobOutputValue::Value(v)) => crate::common::display_value(v),
            Some(job::JobOutputValue::Null) => "null (no value)".to_string(),
            None => format!("pending; waits for {}", self.waits_on.join(", ")),
        };
        format!("{} ({kind}): {value}", self.name)
    }

    fn to_json(&self) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        m.insert("name".into(), self.name.clone().into());
        m.insert("type".into(), self.output_type.clone().into());
        if let Some(unit) = &self.unit {
            m.insert("unit".into(), unit.clone().into());
        }
        if let Some(d) = &self.description {
            m.insert("description".into(), d.clone().into());
        }
        match &self.value {
            Some(job::JobOutputValue::Value(v)) => {
                m.insert("state".into(), "VALUE".into());
                m.insert("value".into(), crate::common::expr_value_to_json(v));
            }
            Some(job::JobOutputValue::Null) => {
                m.insert("state".into(), "NULL".into());
            }
            None => {
                m.insert("state".into(), "PENDING".into());
                m.insert("waits_on".into(), self.waits_on.clone().into());
            }
        }
        serde_json::Value::Object(m)
    }
}

struct ParamInfo {
    name: String,
    description: Option<String>,
    param_type: String,
    value: String,
}

#[derive(Clone)]
struct EnvInfo {
    name: String,
    description: Option<String>,
    parent: String,
    /// The loop of a body Step's Step Environment.
    parent_loop: Option<String>,
    /// `false` for an Environment whose `enableIf` is `false`.
    enabled: bool,
}

impl EnvInfo {
    /// `Name (from 'Parent')`, marked when disabled.
    fn heading(&self) -> String {
        if let Some(l) = &self.parent_loop {
            format!(
                "{} (from body Step '{}' of loop '{l}')",
                self.name, self.parent
            )
        } else if self.enabled {
            format!("{} (from '{}')", self.name, self.parent)
        } else {
            format!(
                "{} (from '{}', disabled by its enableIf)",
                self.name, self.parent
            )
        }
    }

    fn to_json(&self) -> serde_json::Value {
        let mut value = serde_json::json!({"name": self.name, "parent": self.parent});
        if let Some(l) = &self.parent_loop {
            value["loop"] = l.clone().into();
        }
        if !self.enabled {
            value["enabled"] = false.into();
        }
        value
    }
}
