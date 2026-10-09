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

    // With `--environment`, summarize the combined Job as `run` would see
    // it: the attached Environments and Services, and each
    // `requiresServices` entry bound to the attachment that satisfies it
    // (RFC 0009 §1.2.2; a requirement no attachment satisfies is the same
    // error `run` reports). Without, the Job Template alone is summarized
    // and a requirement is shown unsatisfied.
    let mut bindings: Vec<(String, String)> = Vec::new();
    let the_job = if env_templates.is_empty() {
        the_job
    } else {
        let labels: Vec<String> = args
            .environments
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        let attached: Vec<openjd_model::AttachedEnvironmentTemplate<'_>> = env_templates
            .iter()
            .zip(&labels)
            .map(|(template, label)| {
                openjd_model::AttachedEnvironmentTemplate::new(template).with_label(label)
            })
            .collect();
        let applied = openjd_model::apply_environment_templates(
            &the_job,
            &attached,
            &param_values,
            &crate::common::caller_limits(),
        )?;
        bindings = applied
            .requirement_bindings
            .iter()
            .map(|b| (b.requirement.clone(), b.document.to_string()))
            .collect();
        applied.into_combined_job(the_job)
    };

    if let Some(step_name) = &args.step {
        output_step_summary(&the_job, step_name, &args.output)
    } else {
        output_job_summary(
            &the_job,
            &param_order,
            &param_descriptions,
            &bindings,
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

/// A Step's `dependencies` as written, each classified as a Step or, with
/// `SERVICE`, a Service (`service:<name>`).
fn step_dependencies(step: &job::Step, service_active: bool) -> Vec<DepInfo> {
    step.dependencies
        .iter()
        .flatten()
        .map(|d| match d.target(service_active) {
            job::DependencyTarget::Step(name) => DepInfo {
                name: name.to_string(),
                is_service: false,
            },
            job::DependencyTarget::Service(name) => DepInfo {
                name: name.to_string(),
                is_service: true,
            },
        })
        .collect()
}

/// `main (TCP), ingest (UDP)` / `main (TCP, port 6379)` — a Service's or a
/// requirement's ports for display.
fn describe_ports<'a>(
    ports: impl Iterator<Item = (&'a str, job::ServicePortProtocol, Option<u16>)>,
) -> Vec<PortInfo> {
    ports
        .map(|(name, protocol, port)| PortInfo {
            name: name.to_string(),
            protocol: protocol.to_string(),
            port,
        })
        .collect()
}

/// The Job's Services (RFC 0009), each with its scope, ports, health check
/// type and restart policy, in `job.services` order.
fn service_summaries(job: &job::Job) -> Vec<ServiceInfo> {
    job.services
        .iter()
        .flatten()
        .map(|svc| ServiceInfo {
            name: svc.name.clone(),
            description: svc.description.clone(),
            document: (!svc.document.is_job_template()).then(|| svc.document.to_string()),
            scope: svc.scope.to_string(),
            ports: describe_ports(
                svc.ports
                    .iter()
                    .map(|p| (p.name.as_str(), p.protocol, p.port)),
            ),
            health_check: svc.health_check.type_name().to_string(),
            max_attempts: svc.restart_policy.max_attempts,
            completed_tasks: svc.restart_policy.completed_tasks.as_str().to_string(),
            deps: svc
                .dependencies
                .iter()
                .flatten()
                .map(|d| match d.target(true) {
                    job::DependencyTarget::Step(name) => DepInfo {
                        name: name.to_string(),
                        is_service: false,
                    },
                    job::DependencyTarget::Service(name) => DepInfo {
                        name: name.to_string(),
                        is_service: true,
                    },
                })
                .collect(),
        })
        .collect()
}

/// The Job Template's `requiresServices` (RFC 0009 §9.8), each with its
/// ports and, when an attached Environment Template (`--environment`)
/// declares the Service, the document that satisfies it.
fn requirement_summaries(job: &job::Job, bindings: &[(String, String)]) -> Vec<RequirementInfo> {
    job.requires_services
        .iter()
        .flatten()
        .map(|req| RequirementInfo {
            name: req.name.clone(),
            ports: describe_ports(
                req.ports
                    .iter()
                    .map(|p| (p.name.as_str(), p.protocol, None)),
            ),
            satisfied_by: bindings
                .iter()
                .find(|(name, _)| *name == req.name)
                .map(|(_, doc)| doc.clone()),
        })
        .collect()
}

fn output_job_summary(
    job: &job::Job,
    param_order: &[String],
    param_descriptions: &std::collections::HashMap<&str, &str>,
    bindings: &[(String, String)],
    output_format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let service_active = job.services.is_some() || job.requires_services.is_some();
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
                    });
                }
            }
            StepInfo {
                name: s.name.clone(),
                description: s.description.clone(),
                total_tasks,
                task_params,
                envs,
                deps: step_dependencies(s, service_active),
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
        services: service_summaries(job),
        requirements: requirement_summaries(job, bindings),
    };
    crate::common::print_cli_result(&result, output_format);
    Ok(())
}

fn output_step_summary(
    job: &job::Job,
    step_name: &str,
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
                })
                .collect()
        })
        .unwrap_or_default();
    let service_active = job.services.is_some() || job.requires_services.is_some();
    let deps = step_dependencies(step, service_active);
    // The Services whose scope includes this Step: those it lists, those
    // reached through them, and every Job-wide one (RFC 0009 §9.1).
    let services: Vec<String> = job
        .services
        .iter()
        .flatten()
        .filter(|svc| svc.scope.contains(step_name))
        .map(|svc| svc.name.clone())
        .collect();

    let result = StepSummaryResult {
        job_name: job.name.clone(),
        step_name: step_name.to_string(),
        total_tasks,
        task_params,
        envs,
        deps,
        services,
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
    total_tasks: usize,
    total_envs: usize,
    services: Vec<ServiceInfo>,
    requirements: Vec<RequirementInfo>,
}

impl DepInfo {
    /// `{"step_name": …}` or `{"service_name": …}`.
    fn to_json(&self) -> serde_json::Value {
        if self.is_service {
            serde_json::json!({"service_name": self.name})
        } else {
            serde_json::json!({"step_name": self.name})
        }
    }

    /// `'Prepare'` / `Service 'Cache'`, for human-readable lists.
    fn display(&self) -> String {
        if self.is_service {
            format!("Service '{}'", self.name)
        } else {
            format!("'{}'", self.name)
        }
    }
}

impl PortInfo {
    fn to_json(&self) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        m.insert("name".into(), self.name.clone().into());
        m.insert("protocol".into(), self.protocol.clone().into());
        if let Some(port) = self.port {
            m.insert("port".into(), port.into());
        }
        serde_json::Value::Object(m)
    }

    /// `main (TCP)` / `main (TCP, port 6379)`.
    fn display(&self) -> String {
        match self.port {
            Some(port) => format!("{} ({}, port {port})", self.name, self.protocol),
            None => format!("{} ({})", self.name, self.protocol),
        }
    }
}

/// `n dependencies`, and — when any names a Service — `(k Step, m
/// Service)` or `(all Services)`, for a Step's line in the Job summary.
fn describe_dependency_counts(deps: &[DepInfo]) -> String {
    let services = deps.iter().filter(|d| d.is_service).count();
    let steps = deps.len() - services;
    match (steps, services) {
        (_, 0) => format!("{} dependencies", deps.len()),
        (0, _) => format!("{} dependencies (all Services)", deps.len()),
        _ => format!(
            "{} dependencies ({steps} Step, {services} Service)",
            deps.len()
        ),
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
                    let services: Vec<&str> = s
                        .deps
                        .iter()
                        .filter(|d| d.is_service)
                        .map(|d| d.name.as_str())
                        .collect();
                    if !services.is_empty() {
                        m.insert("service_dependencies".into(), serde_json::json!(services));
                    }
                }
                serde_json::Value::Object(m)
            })
            .collect();
        obj.insert("steps".into(), steps_json.into());
        if !self.services.is_empty() {
            obj.insert(
                "services".into(),
                serde_json::json!(self
                    .services
                    .iter()
                    .map(ServiceInfo::to_json)
                    .collect::<Vec<_>>()),
            );
        }
        if !self.requirements.is_empty() {
            obj.insert(
                "requires_services".into(),
                serde_json::json!(self
                    .requirements
                    .iter()
                    .map(|r| {
                        let mut m = serde_json::Map::new();
                        m.insert("name".into(), r.name.clone().into());
                        m.insert(
                            "ports".into(),
                            serde_json::json!(r
                                .ports
                                .iter()
                                .map(PortInfo::to_json)
                                .collect::<Vec<_>>()),
                        );
                        if let Some(doc) = &r.satisfied_by {
                            m.insert("satisfied_by".into(), doc.clone().into());
                        }
                        serde_json::Value::Object(m)
                    })
                    .collect::<Vec<_>>()),
            );
        }
        serde_json::Value::Object(obj)
    }
}

impl ServiceInfo {
    fn to_json(&self) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        m.insert("name".into(), self.name.clone().into());
        if let Some(d) = &self.description {
            m.insert("description".into(), d.clone().into());
        }
        if let Some(doc) = &self.document {
            m.insert("document".into(), doc.clone().into());
        }
        m.insert("scope".into(), self.scope.clone().into());
        m.insert(
            "ports".into(),
            serde_json::json!(self.ports.iter().map(PortInfo::to_json).collect::<Vec<_>>()),
        );
        m.insert("health_check".into(), self.health_check.clone().into());
        m.insert(
            "restart_policy".into(),
            serde_json::json!({
                "max_attempts": self.max_attempts,
                "completed_tasks": self.completed_tasks,
            }),
        );
        if !self.deps.is_empty() {
            m.insert(
                "dependencies".into(),
                serde_json::json!(self.deps.iter().map(DepInfo::to_json).collect::<Vec<_>>()),
            );
        }
        serde_json::Value::Object(m)
    }

    /// The lines under `--- Services in '<job>' ---`.
    fn write_human(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let origin = self
            .document
            .as_deref()
            .map(|d| format!(" (from {d})"))
            .unwrap_or_default();
        writeln!(f, "  - {}{origin} (scope: {})", self.name, self.scope)?;
        if let Some(d) = &self.description {
            writeln!(f, "    {d}")?;
        }
        writeln!(
            f,
            "    Ports: {}",
            self.ports
                .iter()
                .map(PortInfo::display)
                .collect::<Vec<_>>()
                .join(", ")
        )?;
        writeln!(f, "    Health check: {}", self.health_check)?;
        writeln!(
            f,
            "    Restart policy: maxAttempts {}, completedTasks {}",
            self.max_attempts, self.completed_tasks
        )?;
        if !self.deps.is_empty() {
            writeln!(
                f,
                "    Dependencies: {}",
                self.deps
                    .iter()
                    .map(DepInfo::display)
                    .collect::<Vec<_>>()
                    .join(", ")
            )?;
        }
        Ok(())
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
                writeln!(f, "  {}", describe_dependency_counts(&s.deps))?;
                let services: Vec<String> = s
                    .deps
                    .iter()
                    .filter(|d| d.is_service)
                    .map(|d| format!("'{}'", d.name))
                    .collect();
                if !services.is_empty() {
                    writeln!(f, "    Services: {}", services.join(", "))?;
                }
            }
            writeln!(f)?;
        }
        if !self.services.is_empty() {
            writeln!(f)?;
            writeln!(f, "--- Services in '{}' ---", self.name)?;
            for svc in &self.services {
                svc.write_human(f)?;
            }
        }
        if !self.requirements.is_empty() {
            writeln!(f)?;
            writeln!(f, "--- Required Services in '{}' ---", self.name)?;
            for req in &self.requirements {
                write!(
                    f,
                    "  - {} (ports: {})",
                    req.name,
                    req.ports
                        .iter()
                        .map(PortInfo::display)
                        .collect::<Vec<_>>()
                        .join(", ")
                )?;
                match &req.satisfied_by {
                    Some(doc) => writeln!(f, " — satisfied by {doc}")?,
                    None => writeln!(
                        f,
                        " — not satisfied: attach an Environment Template that declares it \
                         with --environment"
                    )?,
                }
            }
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

struct StepSummaryResult {
    job_name: String,
    step_name: String,
    total_tasks: usize,
    task_params: Vec<(String, String)>,
    envs: Vec<EnvInfo>,
    deps: Vec<DepInfo>,
    /// The Services whose scope includes the Step (RFC 0009 §9.1).
    services: Vec<String>,
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
        obj.insert("total_parameters".into(), self.task_params.len().into());
        obj.insert("total_environments".into(), self.envs.len().into());
        if !self.deps.is_empty() {
            obj.insert(
                "dependencies".into(),
                serde_json::json!(self.deps.iter().map(DepInfo::to_json).collect::<Vec<_>>()),
            );
        }
        if !self.services.is_empty() {
            obj.insert("services".into(), serde_json::json!(self.services));
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
        writeln!(f, "Total tasks: {}", self.total_tasks)?;
        writeln!(f, "Total task parameters: {}", self.task_params.len())?;
        writeln!(f, "Total environments: {}", self.envs.len())?;
        if !self.deps.is_empty() {
            writeln!(f)?;
            writeln!(f, "Dependencies ({}):", self.deps.len())?;
            for d in &self.deps {
                writeln!(f, "- {}", d.display())?;
            }
        }
        if !self.services.is_empty() {
            writeln!(f)?;
            writeln!(f, "Services in scope ({}):", self.services.len())?;
            for s in &self.services {
                writeln!(f, "- '{s}'")?;
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
        Ok(())
    }
}

// --- Data types ---

struct StepInfo {
    name: String,
    description: Option<String>,
    total_tasks: usize,
    task_params: Vec<(String, String)>,
    envs: Vec<String>,
    deps: Vec<DepInfo>,
}

/// One `dependencies` entry: a Step, or (RFC 0009) a Service.
struct DepInfo {
    name: String,
    is_service: bool,
}

/// One port of a Service or a requirement.
struct PortInfo {
    name: String,
    protocol: String,
    /// The pinned number, when the template gives one.
    port: Option<u16>,
}

/// One Service of the Job (RFC 0009 §9).
struct ServiceInfo {
    name: String,
    description: Option<String>,
    /// `Some(<document>)` for an external Service from an attached
    /// Environment Template.
    document: Option<String>,
    scope: String,
    ports: Vec<PortInfo>,
    health_check: String,
    max_attempts: u64,
    completed_tasks: String,
    deps: Vec<DepInfo>,
}

/// One `requiresServices` entry (RFC 0009 §9.8).
struct RequirementInfo {
    name: String,
    ports: Vec<PortInfo>,
    /// The attached Environment Template whose Service satisfies it, when
    /// `--environment` attached one.
    satisfied_by: Option<String>,
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
