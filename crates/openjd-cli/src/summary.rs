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

    /// Environment Template files, attached in the order given, as for
    /// `openjd run` (repeatable)
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

    let env_templates =
        crate::common::read_environment_templates(&args.environments, &supported_exts)?;

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
    // template's — and carry the CLI's caller limits and submission path
    // policy, as `openjd run` does.
    let ctx = crate::common::submission_context(&job_template);

    // Attach the --environment templates as `openjd run` does, so each
    // Step shows the effective Environment parameter values it would run
    // with (`ENV_PARAMS`, Template Schemas §3.2.2). The environment
    // listings stay those of the Job Template.
    let labels = crate::common::environment_labels(&args.environments);
    let attached = crate::common::attach(&env_templates, &labels);
    let created = openjd_model::create_job_with_environment_templates(
        &job_template,
        &attached,
        &param_values,
        &ctx,
    )?;
    crate::common::print_submission_warnings(&created.warnings);
    let mut the_job = created.job;
    // With `ENV_PARAMS`, each Step's Environment set (Template Schemas §4
    // item 4), from the combined Job before its listing is filtered.
    // The Environments an `enableIf` disabled for each Step (§4 item 6)
    // are listed too.
    let session_envs: Vec<StepEnvSets> = if lists_env_params(&the_job, &env_templates) {
        the_job
            .steps
            .iter()
            .zip(&created.disabled_environments)
            .map(|(s, disabled)| StepEnvSets {
                entered: session_env_infos(&the_job, &created.environment_documents, s),
                disabled: disabled_env_infos(disabled),
            })
            .collect()
    } else {
        Vec::new()
    };
    let own: Vec<_> = the_job
        .job_environments
        .take()
        .into_iter()
        .flatten()
        .zip(&created.environment_documents)
        .filter(|(_, doc)| doc.is_job_template())
        .map(|(env, _)| env)
        .collect();
    the_job.job_environments = (!own.is_empty()).then_some(own);

    if let Some(step_name) = &args.step {
        output_step_summary(&the_job, &session_envs, step_name, &args.output)
    } else {
        output_job_summary(
            &the_job,
            &session_envs,
            &param_order,
            &param_descriptions,
            &args.output,
        )
    }
}

/// Whether the Job Template or an attached Environment Template lists
/// `ENV_PARAMS`, which decides whether the summary shows each Step's
/// Environment set and combined host requirements. An attached
/// Environment's `activation` and `hostRequirements` apply when its own
/// document lists the extension, so it is enough for one document to;
/// without it in any, the summary is unchanged.
fn lists_env_params(
    job: &job::Job,
    env_templates: &[openjd_model::template::EnvironmentTemplate],
) -> bool {
    job.extensions
        .iter()
        .flatten()
        .any(|e| *e == openjd_model::ModelExtension::EnvParams)
        || env_templates.iter().any(|t| {
            t.profile()
                .has_extension(openjd_model::ModelExtension::EnvParams)
        })
}

/// A Step's Environment set, and the Environments an `enableIf` took out
/// of it.
struct StepEnvSets {
    /// The Environments its Sessions enter, in the order they enter them.
    entered: Vec<SessionEnvInfo>,
    /// The Environments its Sessions would otherwise have entered that an
    /// `enableIf` on the Environment disabled for it (Template Schemas §4
    /// item 6): Job Environments in Job order, then Step Environments.
    disabled: Vec<SessionEnvInfo>,
}

/// The [`SessionEnvInfo`]s of `disabled`, Job Environments first.
fn disabled_env_infos(disabled: &openjd_model::StepDisabledEnvironments) -> Vec<SessionEnvInfo> {
    disabled
        .environments
        .iter()
        .map(|r| SessionEnvInfo {
            name: r.environment.clone(),
            source: r.document.to_string(),
        })
        .chain(
            disabled
                .step_environments
                .iter()
                .map(|name| SessionEnvInfo {
                    name: name.clone(),
                    source: "stepEnvironments".to_string(),
                }),
        )
        .collect()
}

/// One Environment a Step's Sessions enter, in the order they enter it.
struct SessionEnvInfo {
    name: String,
    /// The declaring document (`JobTemplate` or the `--environment`
    /// path), or `stepEnvironments` for the Step's own.
    source: String,
}

/// The Environments `step`'s Sessions enter, in order: the combined Job
/// Environments it enters (attached first, then the Job Template's;
/// `documents` describes `job.job_environments` index for index), then its
/// Step Environments.
fn session_env_infos(
    job: &job::Job,
    documents: &[job::Document],
    step: &job::Step,
) -> Vec<SessionEnvInfo> {
    job.job_environments
        .iter()
        .flatten()
        .zip(documents)
        .filter(|(env, doc)| step.enters_environment(doc, &env.name))
        .map(|(env, doc)| SessionEnvInfo {
            name: env.name.clone(),
            source: doc.to_string(),
        })
        .chain(
            step.step_environments
                .iter()
                .flatten()
                .map(|env| SessionEnvInfo {
                    name: env.name.clone(),
                    source: "stepEnvironments".to_string(),
                }),
        )
        .collect()
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

fn step_total_tasks(step: &job::Step) -> usize {
    match &step.parameter_space {
        None => 1,
        Some(ps) => {
            match openjd_model::StepParameterSpaceIterator::new_with_chunk_override(ps, Some(1)) {
                Ok(it) => it.len(),
                Err(_) => 1,
            }
        }
    }
}

fn output_job_summary(
    job: &job::Job,
    session_envs: &[StepEnvSets],
    param_order: &[String],
    param_descriptions: &std::collections::HashMap<&str, &str>,
    output_format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Collect step summaries
    let mut step_envs_info: Vec<EnvInfo> = Vec::new();
    let mut session_envs = session_envs.iter();
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
                env_params: env_param_infos(s),
                env_params_details: session_envs.next().map(|envs| EnvParamsDetails {
                    session_envs: &envs.entered,
                    disabled_envs: &envs.disabled,
                    host_requirements: s.host_requirements.as_ref(),
                }),
            }
        })
        .collect();

    let total_tasks: usize = step_summaries.iter().map(|s| s.total_tasks).sum();
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
        total_envs,
    };
    crate::common::print_cli_result(&result, output_format);
    Ok(())
}

fn output_step_summary(
    job: &job::Job,
    session_envs: &[StepEnvSets],
    step_name: &str,
    output_format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let step_index = job
        .steps
        .iter()
        .position(|s| s.name == step_name)
        .ok_or_else(|| format!("Step '{step_name}' does not exist in Job '{}'.", job.name))?;
    let step = &job.steps[step_index];

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
        env_params: env_param_infos(step),
        env_params_details: session_envs.get(step_index).map(|envs| EnvParamsDetails {
            session_envs: &envs.entered,
            disabled_envs: &envs.disabled,
            host_requirements: step.host_requirements.as_ref(),
        }),
    };
    crate::common::print_cli_result(&result, output_format);
    Ok(())
}

// --- Result types ---

use crate::common::CliResult;
use std::fmt;

struct JobSummaryResult<'a> {
    name: String,
    params: Vec<ParamInfo>,
    step_summaries: Vec<StepInfo<'a>>,
    root_envs: Vec<EnvInfo>,
    step_envs: Vec<EnvInfo>,
    total_tasks: usize,
    total_envs: usize,
}

impl CliResult for JobSummaryResult<'_> {
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
        if self.total_envs > 0 {
            obj.insert("total_environments".into(), self.total_envs.into());
        }
        if !self.root_envs.is_empty() {
            obj.insert(
                "root_environments".into(),
                serde_json::json!(self
                    .root_envs
                    .iter()
                    .map(|e| serde_json::json!({"name": e.name, "parent": e.parent}))
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
                if !s.env_params.is_empty() {
                    m.insert(
                        "environment_parameters".into(),
                        env_params_json(&s.env_params),
                    );
                }
                if let Some(d) = &s.env_params_details {
                    d.insert_json(&mut m);
                }
                serde_json::Value::Object(m)
            })
            .collect();
        obj.insert("steps".into(), steps_json.into());
        serde_json::Value::Object(obj)
    }
}

impl fmt::Display for JobSummaryResult<'_> {
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
        writeln!(f, "Total tasks: {}", self.total_tasks)?;
        writeln!(f, "Total environments: {}", self.total_envs)?;
        writeln!(f)?;
        writeln!(f, "--- Steps in '{}' ---", self.name)?;
        writeln!(f)?;
        for (i, s) in self.step_summaries.iter().enumerate() {
            writeln!(f, "{}. '{}' ({} total Tasks)", i + 1, s.name, s.total_tasks)?;
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
            if let Some(d) = &s.env_params_details {
                d.write(f, "  ")?;
            }
            write_env_params(f, &s.env_params, "  ")?;
            writeln!(f)?;
        }
        if self.total_envs > 0 {
            writeln!(f)?;
            writeln!(f, "--- Environments in '{}' ---", self.name)?;
            for e in &self.root_envs {
                write!(f, "  - {} (from '{}')", e.name, e.parent)?;
                if let Some(d) = &e.description {
                    write!(f, "\n {d}")?;
                }
                writeln!(f)?;
            }
            for e in &self.step_envs {
                write!(f, "  - {} (from '{}')", e.name, e.parent)?;
                if let Some(d) = &e.description {
                    write!(f, "\n {d}")?;
                }
                writeln!(f)?;
            }
        }
        Ok(())
    }
}

struct StepSummaryResult<'a> {
    job_name: String,
    step_name: String,
    total_tasks: usize,
    task_params: Vec<(String, String)>,
    envs: Vec<EnvInfo>,
    deps: Vec<String>,
    env_params: Vec<EnvParamInfo>,
    env_params_details: Option<EnvParamsDetails<'a>>,
}

impl CliResult for StepSummaryResult<'_> {
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
        if !self.env_params.is_empty() {
            obj.insert(
                "environment_parameters".into(),
                env_params_json(&self.env_params),
            );
        }
        if let Some(d) = &self.env_params_details {
            d.insert_json(&mut obj);
        }
        serde_json::Value::Object(obj)
    }
}

impl fmt::Display for StepSummaryResult<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f)?;
        writeln!(
            f,
            "--- Summary for Step '{}' in Job '{}' ---",
            self.step_name, self.job_name
        )?;
        writeln!(f)?;
        writeln!(f, "Total tasks: {}", self.total_tasks)?;
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
                write!(f, "- {} (from '{}')", e.name, e.parent)?;
                if let Some(d) = &e.description {
                    write!(f, "\n {d}")?;
                }
                writeln!(f)?;
            }
        }
        if let Some(d) = &self.env_params_details {
            writeln!(f)?;
            d.write(f, "")?;
        }
        if !self.env_params.is_empty() {
            writeln!(f)?;
            write_env_params(f, &self.env_params, "")?;
        }
        Ok(())
    }
}

// --- Data types ---

struct StepInfo<'a> {
    name: String,
    description: Option<String>,
    total_tasks: usize,
    task_params: Vec<(String, String)>,
    envs: Vec<String>,
    deps: Vec<String>,
    env_params: Vec<EnvParamInfo>,
    /// `Some` when the Job Template or an attached Environment Template
    /// lists `ENV_PARAMS`.
    env_params_details: Option<EnvParamsDetails<'a>>,
}

/// What the summary adds for a Step when the Job Template or an attached
/// Environment Template lists `ENV_PARAMS`: the Environments its Sessions
/// enter, the ones an `enableIf` disabled for it (Template Schemas §4 item
/// 6), and its combined host requirements (§3.3.3).
struct EnvParamsDetails<'a> {
    session_envs: &'a [SessionEnvInfo],
    disabled_envs: &'a [SessionEnvInfo],
    host_requirements: Option<&'a job::HostRequirements>,
}

impl EnvParamsDetails<'_> {
    /// Add `session_environments` and, when there are any,
    /// `disabled_environments` and `host_requirements` to `m`.
    fn insert_json(&self, m: &mut serde_json::Map<String, serde_json::Value>) {
        let list = |envs: &[SessionEnvInfo]| {
            serde_json::json!(envs
                .iter()
                .map(|e| serde_json::json!({"name": e.name, "source": e.source}))
                .collect::<Vec<_>>())
        };
        m.insert("session_environments".into(), list(self.session_envs));
        if !self.disabled_envs.is_empty() {
            m.insert("disabled_environments".into(), list(self.disabled_envs));
        }
        if let Some(hr) = self.host_requirements {
            m.insert("host_requirements".into(), host_requirements_json(hr));
        }
    }

    /// `Session environments:` with one `- Name (source)` line each (or
    /// `(none)`), then, when there are any, `Disabled environments:` in the
    /// same form, then `Host requirements:` with one line per requirement,
    /// indented by `indent`.
    fn write(&self, f: &mut fmt::Formatter<'_>, indent: &str) -> fmt::Result {
        writeln!(f, "{indent}Session environments:")?;
        if self.session_envs.is_empty() {
            writeln!(f, "{indent}  (none)")?;
        }
        for e in self.session_envs {
            writeln!(f, "{indent}  - {} ({})", e.name, e.source)?;
        }
        if !self.disabled_envs.is_empty() {
            writeln!(f, "{indent}Disabled environments:")?;
            for e in self.disabled_envs {
                writeln!(f, "{indent}  - {} ({})", e.name, e.source)?;
            }
        }
        if let Some(hr) = self.host_requirements {
            writeln!(f, "{indent}Host requirements:")?;
            for line in host_requirement_lines(hr) {
                writeln!(f, "{indent}  - {line}")?;
            }
        }
        Ok(())
    }
}

/// One line per requirement of `hr`: `name: min 1, max 2` for an amount,
/// `name: anyOf a, b` or `name: allOf a, b` for an attribute.
fn host_requirement_lines(hr: &job::HostRequirements) -> Vec<String> {
    let mut lines = Vec::new();
    for a in hr.amounts.iter().flatten() {
        let bounds: Vec<String> = [("min", a.min), ("max", a.max)]
            .iter()
            .filter_map(|(k, v)| v.map(|v| format!("{k} {v}")))
            .collect();
        lines.push(format!("{}: {}", a.name, bounds.join(", ")));
    }
    for a in hr.attributes.iter().flatten() {
        for (k, values) in [("anyOf", &a.any_of), ("allOf", &a.all_of)] {
            if let Some(values) = values {
                lines.push(format!("{}: {k} {}", a.name, values.join(", ")));
            }
        }
    }
    lines
}

/// `hr` as `{"amounts": [{name, min?, max?}], "attributes": [{name,
/// anyOf?, allOf?}]}`, each list present when non-empty.
fn host_requirements_json(hr: &job::HostRequirements) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    if let Some(amounts) = hr.amounts.as_ref().filter(|a| !a.is_empty()) {
        m.insert(
            "amounts".into(),
            amounts
                .iter()
                .map(|a| {
                    let mut o = serde_json::Map::new();
                    o.insert("name".into(), a.name.clone().into());
                    if let Some(v) = a.min {
                        o.insert("min".into(), v.into());
                    }
                    if let Some(v) = a.max {
                        o.insert("max".into(), v.into());
                    }
                    serde_json::Value::Object(o)
                })
                .collect(),
        );
    }
    if let Some(attributes) = hr.attributes.as_ref().filter(|a| !a.is_empty()) {
        m.insert(
            "attributes".into(),
            attributes
                .iter()
                .map(|a| {
                    let mut o = serde_json::Map::new();
                    o.insert("name".into(), a.name.clone().into());
                    if let Some(v) = &a.any_of {
                        o.insert("anyOf".into(), v.clone().into());
                    }
                    if let Some(v) = &a.all_of {
                        o.insert("allOf".into(), v.clone().into());
                    }
                    serde_json::Value::Object(o)
                })
                .collect(),
        );
    }
    serde_json::Value::Object(m)
}

/// One Environment's effective parameter values for a Step (`ENV_PARAMS`).
struct EnvParamInfo {
    environment: String,
    /// The declaring document: `JobTemplate`, or the `--environment` path.
    document: String,
    /// `(name, type, value)`, from [`crate::common::environment_parameter_entries`].
    values: Vec<(String, String, String)>,
}

/// A Step's effective Environment parameter values, in the order its
/// Sessions enter the Environments.
fn env_param_infos(step: &job::Step) -> Vec<EnvParamInfo> {
    step.environment_parameters
        .iter()
        .flatten()
        .map(|v| EnvParamInfo {
            environment: v.environment.clone(),
            document: v.document.to_string(),
            values: crate::common::environment_parameter_entries(v),
        })
        .collect()
}

fn env_params_json(infos: &[EnvParamInfo]) -> serde_json::Value {
    serde_json::json!(infos
        .iter()
        .map(|e| serde_json::json!({
            "environment": e.environment,
            "document": e.document,
            "values": e
                .values
                .iter()
                .map(|(n, t, v)| serde_json::json!({"name": n, "type": t, "value": v}))
                .collect::<Vec<_>>(),
        }))
        .collect::<Vec<_>>())
}

/// `Environment parameters:` then, per Environment, `Name (document):` and
/// one `- name (TYPE): value` line per parameter, indented by `indent`.
fn write_env_params(
    f: &mut fmt::Formatter<'_>,
    infos: &[EnvParamInfo],
    indent: &str,
) -> fmt::Result {
    if infos.is_empty() {
        return Ok(());
    }
    writeln!(f, "{indent}Environment parameters:")?;
    for e in infos {
        writeln!(f, "{indent}  {} ({}):", e.environment, e.document)?;
        for (name, ty, value) in &e.values {
            writeln!(f, "{indent}    - {name} ({ty}): {value}")?;
        }
    }
    Ok(())
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
}
