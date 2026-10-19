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

    if let Some(step_name) = &args.step {
        output_step_summary(&the_job, step_name, control_flow, &args.output)
    } else {
        output_job_summary(
            &the_job,
            &param_order,
            &param_descriptions,
            control_flow,
            &args.output,
        )
    }
}

fn task_param_type_name(tp: &job::TaskParameter) -> &'static str {
    match tp {
        job::TaskParameter::Int { .. } => "INT",
        job::TaskParameter::Float { .. } => "FLOAT",
        job::TaskParameter::String { .. } => "STRING",
        job::TaskParameter::Path { .. } => "PATH",
        job::TaskParameter::ChunkInt { .. } => "CHUNK[INT]",
    }
}

/// The Step's Task count, `None` while it is determined at run time (a
/// pending Step whose Tasks are not known).
fn step_total_tasks(step: &job::Step) -> Option<usize> {
    step.tasks_known().map(|n| n as usize)
}

/// The RFC's name for a `CONTROL_FLOW` Step's state at Job creation.
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
    let group = |g: &job::GroupReport| match g {
        job::GroupReport::Known => serde_json::json!({"known": true}),
        job::GroupReport::Waiting { on, for_enablement } => serde_json::json!({
            "known": false,
            "waits_on": on,
            "waits_for_enable_if": for_enablement,
        }),
    };
    Some(serde_json::json!({
        "enablement": group(&report.enablement),
        "tasks": group(&report.tasks),
        "host_requirements": group(&report.host_requirements),
        "remaining": group(&report.remaining),
    }))
}

/// What is known of each job output at Job creation.
fn job_output_infos(job: &job::Job) -> Result<Vec<JobOutputInfo>, Box<dyn std::error::Error>> {
    let inputs = crate::common::creation_inputs(job)?;
    Ok(job
        .outputs
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
                .filter(|s| !inputs.is_concrete(s))
                .collect(),
        })
        .collect())
}

fn output_job_summary(
    job: &job::Job,
    param_order: &[String],
    param_descriptions: &std::collections::HashMap<&str, &str>,
    control_flow: bool,
    output_format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Collect step summaries
    let mut step_envs_info: Vec<EnvInfo> = Vec::new();
    let step_summaries: Vec<StepInfo> = job
        .steps
        .iter()
        .map(|s| {
            let total_tasks = step_total_tasks(s);
            let task_params: Vec<(String, String)> = s
                .parameter_space
                .as_ref()
                .map(|ps| {
                    let mut v: Vec<_> = ps
                        .task_parameter_definitions
                        .iter()
                        .map(|(name, tp)| (name.clone(), task_param_type_name(tp).to_string()))
                        .collect();
                    v.sort_by(|a, b| a.0.cmp(&b.0));
                    v
                })
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
                        enabled: e.enabled,
                    });
                }
            }
            let deps: Vec<String> = s
                .dependencies
                .as_ref()
                .map(|deps| deps.iter().map(|d| d.depends_on.clone()).collect())
                .unwrap_or_default();
            StepInfo {
                name: s.name.clone(),
                description: s.description.clone(),
                total_tasks,
                task_params,
                envs,
                deps,
                state: control_flow.then(|| step_state(s)),
                binding_lines: crate::common::binding_lines(s),
                binding_json: binding_json(s),
            }
        })
        .collect();

    let total_tasks: usize = step_summaries.iter().filter_map(|s| s.total_tasks).sum();
    let tasks_at_run_time = step_summaries.iter().any(|s| s.total_tasks.is_none());
    let step_envs: usize = step_summaries.iter().map(|s| s.envs.len()).sum();
    let root_envs: Vec<EnvInfo> = job
        .job_environments
        .as_ref()
        .map(|envs| {
            envs.iter()
                .map(|e| EnvInfo {
                    name: e.name.clone(),
                    description: e.description.clone(),
                    parent: "root".into(),
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
        job_outputs: job_output_infos(job)?,
    };
    crate::common::print_cli_result(&result, output_format);
    Ok(())
}

fn output_step_summary(
    job: &job::Job,
    step_name: &str,
    control_flow: bool,
    output_format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let step = job
        .steps
        .iter()
        .find(|s| s.name == step_name)
        .ok_or_else(|| format!("Step '{step_name}' does not exist in Job '{}'.", job.name))?;

    let total_tasks = step_total_tasks(step);
    let task_params: Vec<(String, String)> = step
        .parameter_space
        .as_ref()
        .map(|ps| {
            let mut v: Vec<_> = ps
                .task_parameter_definitions
                .iter()
                .map(|(name, tp)| (name.clone(), task_param_type_name(tp).to_string()))
                .collect();
            v.sort_by(|a, b| a.0.cmp(&b.0));
            v
        })
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
    total_tasks: usize,
    /// Whether some Step's Tasks are determined at run time.
    tasks_at_run_time: bool,
    total_envs: usize,
    job_outputs: Vec<JobOutputInfo>,
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
        obj.insert("total_steps".into(), self.step_summaries.len().into());
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
        writeln!(f, "Total steps: {}", self.step_summaries.len())?;
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
                    writeln!(f, " {d}")?;
                }
            }
        }
        Ok(())
    }
}

struct StepSummaryResult {
    job_name: String,
    step_name: String,
    total_tasks: Option<usize>,
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
    total_tasks: Option<usize>,
    task_params: Vec<(String, String)>,
    envs: Vec<String>,
    deps: Vec<String>,
    /// The Step's state, for a template that lists `CONTROL_FLOW`.
    state: Option<&'static str>,
    /// What is known of a pending Step (`common::binding_lines`).
    binding_lines: Vec<String>,
    binding_json: Option<serde_json::Value>,
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

struct EnvInfo {
    name: String,
    description: Option<String>,
    parent: String,
    /// `false` for an Environment whose `enableIf` is `false`.
    enabled: bool,
}

impl EnvInfo {
    /// `Name (from 'Parent')`, marked when disabled.
    fn heading(&self) -> String {
        if self.enabled {
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
        if !self.enabled {
            value["enabled"] = false.into();
        }
        value
    }
}
