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
    // template's — and carry the CLI's caller limits, as `openjd run`
    // does.
    let ctx = crate::common::submission_context(&job_template);

    // Attach the --environment templates as `openjd run` does, so each
    // Step shows the effective Environment parameter values it would run
    // with (`ENV_PARAMS`, Template Schemas §3.2.2). The environment
    // listings stay those of the Job Template.
    let labels = crate::common::environment_labels(&args.environments);
    let attached = crate::common::attach(&env_templates, &labels);
    // Without `--environment` the Job Template alone is summarized: an
    // `externalServices` entry (RFC 0009) is shown unsatisfied rather than
    // rejected, as no submission attaches its provider.
    let created = if attached.is_empty() {
        openjd_model::create_job_details(&job_template, &param_values, &ctx)?
    } else {
        openjd_model::create_job_with_environment_templates(
            &job_template,
            &attached,
            &param_values,
            &ctx,
        )?
    };
    crate::common::print_submission_warnings(&created.warnings);
    let mut the_job = created.job;
    // With `ENV_PARAMS`, each Step's Environment set (Template Schemas §4
    // item 4), from the combined Job before its listing is filtered.
    // The Environments an `enableIf` disabled for each Step (§4 item 6),
    // or an earlier one of their `exclusiveGroup` superseded (§4 item 7),
    // each with which, the ones it declined, and its optional entries
    // that named nothing, are listed too.
    let session_envs: Vec<StepEnvSets> = if lists_env_params(&the_job, &env_templates) {
        the_job
            .steps
            .iter()
            .zip(&created.environment_statuses)
            .zip(&created.discarded_host_requirements)
            .map(|((s, statuses), discarded)| StepEnvSets {
                entered: session_env_infos(&the_job, &created.environment_documents, s),
                disabled: disabled_env_infos(statuses),
                declined: declined_env_infos(statuses),
                not_found: statuses
                    .not_found
                    .iter()
                    .map(|n| (n.dependency_index, n.environment.clone()))
                    .collect(),
                discarded: discarded.iter().map(DiscardedInfo::from).collect(),
            })
            .collect()
    } else {
        Vec::new()
    };
    // Each Service's Environment set (RFC 0009 "Services run inside
    // Environments"), index for index with `job.services`, likewise from
    // the combined Job: a Service selects Environments exactly as a Step
    // does, so its disabled, superseded and declined Environments and its
    // optional entries that named nothing are listed as a Step's are, with
    // its discarded host requirements (§3.3).
    let no_statuses = openjd_model::StepEnvironmentStatuses::default();
    let service_envs: Vec<StepEnvSets> = the_job
        .services
        .iter()
        .flatten()
        .enumerate()
        .map(|(k, svc)| {
            let statuses = created
                .service_environment_statuses
                .get(k)
                .unwrap_or(&no_statuses);
            (svc, statuses, k)
        })
        .map(|(svc, statuses, k)| StepEnvSets {
            entered: service_session_env_infos(&the_job, &created.environment_documents, svc),
            disabled: disabled_env_infos(statuses),
            declined: declined_env_infos(statuses),
            not_found: statuses
                .not_found
                .iter()
                .map(|n| (n.dependency_index, n.environment.clone()))
                .collect(),
            discarded: created
                .discarded_service_host_requirements
                .get(k)
                .into_iter()
                .flatten()
                .map(DiscardedInfo::from)
                .collect(),
        })
        .collect();
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

    // With `--environment`, the Services are summarized as `run` would see
    // them: the attached Services, and each `externalServices` entry bound
    // to the attachment that satisfies it (RFC 0009 §1.2.2; a requirement no
    // attachment satisfies is the same error `run` reports). Without, the
    // Job Template alone is summarized and a requirement is shown
    // unsatisfied. The Environment listings stay the Job Template's (above).
    let bindings: Vec<(String, String)> = created
        .requirement_bindings
        .iter()
        .map(|b| (b.requirement.clone(), b.document.to_string()))
        .collect();

    if let Some(step_name) = &args.step {
        output_step_summary(&the_job, &session_envs, step_name, &args.output)
    } else {
        output_job_summary(
            &the_job,
            &session_envs,
            &service_envs,
            &param_order,
            &param_descriptions,
            &bindings,
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

/// A Step's (or, with `SERVICE`, a Service's) Environment set, the
/// Environments an `enableIf` or an `exclusiveGroup` took out of it, the
/// ones it declined, and its discarded host requirements.
struct StepEnvSets {
    /// The Environments its Sessions enter, in the order they enter them.
    entered: Vec<SessionEnvInfo>,
    /// The Environments its Sessions would otherwise have entered that an
    /// `enableIf` on the Environment disabled for it (Template Schemas §4
    /// item 6), or an earlier one of their `exclusiveGroup` superseded (§4
    /// item 7), each with which: Job Environments in Job order, then Step
    /// Environments.
    disabled: Vec<DisabledEnvInfo>,
    /// The `BY_DEFAULT` Environments a disabled `environment:` entry of the
    /// Step declined (§3.2 item 3.2), in Job order.
    declined: Vec<SessionEnvInfo>,
    /// Its enabled `required: false` entries that named no Environment, as
    /// (index in `dependencies`, name given), in `dependencies` order.
    not_found: Vec<(usize, String)>,
    /// The host requirements that resolved to nothing for it and were
    /// discarded (Template Schemas §3.3): its own first, then its
    /// Environments'.
    discarded: Vec<DiscardedInfo>,
}

/// One discarded host requirement of a Step or Service (Template Schemas
/// §3.3).
struct DiscardedInfo {
    /// `amount` or `attribute`.
    kind: &'static str,
    /// The resolved capability name.
    name: String,
    /// The Environment whose `hostRequirements` held it; `None` for the
    /// Step's (or Service's) own.
    environment: Option<SessionEnvInfo>,
}

impl From<&openjd_model::DiscardedHostRequirement> for DiscardedInfo {
    fn from(d: &openjd_model::DiscardedHostRequirement) -> Self {
        Self {
            kind: match d.kind {
                openjd_model::HostRequirementKind::Amount => "amount",
                openjd_model::HostRequirementKind::Attribute => "attribute",
            },
            name: d.name.clone(),
            environment: d.environment.as_ref().map(|r| SessionEnvInfo {
                name: r.environment.clone(),
                source: r.document.to_string(),
            }),
        }
    }
}

/// An Environment disabled for a Step, with `disabled` (by its `enableIf`)
/// or `superseded` (by an earlier one of its `exclusiveGroup`).
struct DisabledEnvInfo {
    env: SessionEnvInfo,
    status: &'static str,
    /// Its `exclusiveGroup`, if it has one.
    exclusive_group: Option<String>,
    /// For a superseded one, the member of its group the Step enters.
    superseded_by: Option<SessionEnvInfo>,
}

/// The [`SessionEnvInfo`] of one item of a Step's statuses: its declaring
/// document, or `stepEnvironments` for the Step's own.
fn status_env_info(s: &openjd_model::StepEnvironmentStatus) -> SessionEnvInfo {
    SessionEnvInfo {
        name: s.name.clone(),
        source: match s.scope {
            openjd_model::EnvironmentScope::Step => "stepEnvironments".to_string(),
            openjd_model::EnvironmentScope::Attached | openjd_model::EnvironmentScope::Job => {
                s.document.to_string()
            }
        },
    }
}

/// The Environments disabled or superseded for a Step, in Job order (Job
/// Environments, then Step Environments), each with which.
fn disabled_env_infos(statuses: &openjd_model::StepEnvironmentStatuses) -> Vec<DisabledEnvInfo> {
    statuses
        .environments
        .iter()
        .filter_map(|s| {
            let status = match s.status {
                openjd_model::EnvironmentStatus::Disabled => "disabled",
                openjd_model::EnvironmentStatus::Superseded => "superseded",
                openjd_model::EnvironmentStatus::Entered
                | openjd_model::EnvironmentStatus::Declined => return None,
            };
            Some(DisabledEnvInfo {
                env: status_env_info(s),
                status,
                exclusive_group: s.exclusive_group.clone(),
                superseded_by: s.superseded_by.as_ref().map(|w| SessionEnvInfo {
                    name: w.name.clone(),
                    source: w.document.to_string(),
                }),
            })
        })
        .collect()
}

/// The Environments a Step declined, in Job order.
fn declined_env_infos(statuses: &openjd_model::StepEnvironmentStatuses) -> Vec<SessionEnvInfo> {
    statuses
        .environments
        .iter()
        .filter(|s| s.status == openjd_model::EnvironmentStatus::Declined)
        .map(status_env_info)
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

/// The Job Environments `service`'s Sessions enter, in order (attached
/// first, then the Job Template's; `documents` describes
/// `job.job_environments` index for index). A Service enters no Step
/// Environment.
fn service_session_env_infos(
    job: &job::Job,
    documents: &[job::Document],
    service: &job::Service,
) -> Vec<SessionEnvInfo> {
    job.job_environments
        .iter()
        .flatten()
        .zip(documents)
        .filter(|(env, doc)| service.enters_environment(doc, &env.name))
        .map(|(env, doc)| SessionEnvInfo {
            name: env.name.clone(),
            source: doc.to_string(),
        })
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

/// A step's task count, exact on every target (a `u64`, like
/// `StepParameterSpaceIterator::len`). Fails, naming the step, when the
/// count cannot be computed: a chunked step can have more than `u64::MAX`
/// tasks, which counting one task per value overflows.
fn step_total_tasks(step: &job::Step) -> Result<u64, String> {
    match &step.parameter_space {
        None => Ok(1),
        Some(ps) => openjd_model::StepParameterSpaceIterator::new_with_chunk_override(ps, Some(1))
            .map(|it| it.len())
            .map_err(|e| format!("Step '{}': {e}", step.name)),
    }
}

/// A `dependencies` list as written, each entry a Step (`dependsOn`) or,
/// with `SERVICE`, a Service (`service`).
fn dependency_infos(deps: Option<&[job::StepDependency]>) -> Vec<DepInfo> {
    deps.into_iter()
        .flatten()
        .filter_map(|d| match d.target()? {
            job::DependencyTarget::Step(name, _) => Some(DepInfo {
                name: name.to_string(),
                is_service: false,
            }),
            job::DependencyTarget::Service(name) => Some(DepInfo {
                name: name.to_string(),
                is_service: true,
            }),
            // A Job's dependencies hold no `environment:` entry (job
            // creation resolves those into the Step's Environments).
            job::DependencyTarget::Environment(_) => None,
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
/// type, restart policy, the Job Environments its Sessions enter
/// (`session_envs`, index for index), its combined host requirements
/// (RFC 0009 `<Service>` item 5), and the ones discarded (§3.3), in
/// `job.services` order.
fn service_summaries<'a>(
    job: &'a job::Job,
    session_envs: &'a [StepEnvSets],
) -> Vec<ServiceInfo<'a>> {
    job.services
        .iter()
        .flatten()
        .zip(session_envs)
        .map(|(svc, envs)| ServiceInfo {
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
            max_restarts: svc.restart_policy.max_restarts,
            completed_tasks: svc
                .restart_policy
                .completed_tasks
                .map(|p| p.as_str().to_string()),
            deps: dependency_infos(svc.dependencies.as_deref()),
            env_params: env_param_infos_of(svc.environment_parameters.as_deref()),
            env_params_details: EnvParamsDetails {
                session_envs: &envs.entered,
                disabled_envs: &envs.disabled,
                declined_envs: &envs.declined,
                not_found: &envs.not_found,
                host_requirements: svc.host_requirements.as_ref(),
                discarded: &envs.discarded,
                owner: "Service",
            },
        })
        .collect()
}

/// The Job Template's `externalServices` (RFC 0009 §9.8), each with its
/// ports and, when an attached Environment Template (`--environment`)
/// declares the Service, the document that satisfies it.
fn requirement_summaries(job: &job::Job, bindings: &[(String, String)]) -> Vec<RequirementInfo> {
    job.external_services
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
    session_envs: &[StepEnvSets],
    service_envs: &[StepEnvSets],
    param_order: &[String],
    param_descriptions: &std::collections::HashMap<&str, &str>,
    bindings: &[(String, String)],
    output_format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Collect step summaries
    let mut step_envs_info: Vec<EnvInfo> = Vec::new();
    let mut session_envs = session_envs.iter();
    let step_summaries: Vec<StepInfo> = job
        .steps
        .iter()
        .map(|s| {
            let total_tasks = step_total_tasks(s)?;
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
            Ok(StepInfo {
                name: s.name.clone(),
                description: s.description.clone(),
                total_tasks,
                task_params,
                envs,
                deps: dependency_infos(s.dependencies.as_deref()),
                env_params: env_param_infos(s),
                env_params_details: session_envs.next().map(|envs| EnvParamsDetails {
                    session_envs: &envs.entered,
                    disabled_envs: &envs.disabled,
                    declined_envs: &envs.declined,
                    not_found: &envs.not_found,
                    host_requirements: s.host_requirements.as_ref(),
                    discarded: &envs.discarded,
                    owner: "Step",
                }),
            })
        })
        .collect::<Result<_, String>>()?;

    // Each step's count is an exact u64, but their sum can pass u64::MAX
    // (two steps of 2^63 tasks). Fail rather than print a wrapped total.
    let total_tasks = step_summaries
        .iter()
        .try_fold(0u64, |acc, s| acc.checked_add(s.total_tasks))
        .ok_or_else(|| {
            format!(
                "Job '{}' has more than {} tasks in total, so the total cannot be reported.",
                job.name,
                u64::MAX
            )
        })?;
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
        services: service_summaries(job, service_envs),
        requirements: requirement_summaries(job, bindings),
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

    let total_tasks = step_total_tasks(step)?;
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
    let deps = dependency_infos(step.dependencies.as_deref());
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
        env_params: env_param_infos(step),
        env_params_details: session_envs.get(step_index).map(|envs| EnvParamsDetails {
            session_envs: &envs.entered,
            disabled_envs: &envs.disabled,
            declined_envs: &envs.declined,
            not_found: &envs.not_found,
            host_requirements: step.host_requirements.as_ref(),
            discarded: &envs.discarded,
            owner: "Step",
        }),
        services,
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
    total_tasks: u64,
    total_envs: usize,
    services: Vec<ServiceInfo<'a>>,
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
                "external_services".into(),
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

impl ServiceInfo<'_> {
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
                "max_restarts": self.max_restarts,
                "completed_tasks": self.completed_tasks,
            }),
        );
        if !self.deps.is_empty() {
            m.insert(
                "dependencies".into(),
                serde_json::json!(self.deps.iter().map(DepInfo::to_json).collect::<Vec<_>>()),
            );
        }
        self.env_params_details.insert_json(&mut m);
        if !self.env_params.is_empty() {
            m.insert(
                "environment_parameters".into(),
                env_params_json(&self.env_params),
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
            "    Restart policy: maxRestarts {}, completedTasks {}",
            self.max_restarts,
            self.completed_tasks.as_deref().unwrap_or("none")
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
        self.env_params_details.write(f, "    ")?;
        write_env_params(f, &self.env_params, "    ")
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
            if let Some(d) = &s.env_params_details {
                d.write(f, "  ")?;
            }
            write_env_params(f, &s.env_params, "  ")?;
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

struct StepSummaryResult<'a> {
    job_name: String,
    step_name: String,
    total_tasks: u64,
    task_params: Vec<(String, String)>,
    envs: Vec<EnvInfo>,
    deps: Vec<DepInfo>,
    env_params: Vec<EnvParamInfo>,
    env_params_details: Option<EnvParamsDetails<'a>>,
    /// The Services whose scope includes the Step (RFC 0009 §9.1).
    services: Vec<String>,
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
    total_tasks: u64,
    task_params: Vec<(String, String)>,
    envs: Vec<String>,
    deps: Vec<DepInfo>,
    env_params: Vec<EnvParamInfo>,
    /// `Some` when the Job Template or an attached Environment Template
    /// lists `ENV_PARAMS`.
    env_params_details: Option<EnvParamsDetails<'a>>,
}

/// What the summary adds for a Step when the Job Template or an attached
/// Environment Template lists `ENV_PARAMS`: the Environments its Sessions
/// enter, the ones an `enableIf` disabled or an `exclusiveGroup`
/// superseded for it (Template Schemas §4 items 6 and 7), the ones it
/// declined (§3.2 item 3.2), its `required: false` entries that named
/// nothing (§3.2.1 item 2), its combined host requirements (§3.3.3), and
/// the requirements that resolved to nothing and were discarded (§3.3).
struct EnvParamsDetails<'a> {
    session_envs: &'a [SessionEnvInfo],
    disabled_envs: &'a [DisabledEnvInfo],
    declined_envs: &'a [SessionEnvInfo],
    not_found: &'a [(usize, String)],
    host_requirements: Option<&'a job::HostRequirements>,
    discarded: &'a [DiscardedInfo],
    /// `Step` or `Service`: what the text form calls the entity whose own
    /// requirement was discarded.
    owner: &'static str,
}

impl EnvParamsDetails<'_> {
    /// Add `session_environments` and, when there are any,
    /// `disabled_environments` (each with its `status`),
    /// `declined_environments`, `optional_entries_not_found`,
    /// `host_requirements`, and `discarded_host_requirements` to `m`.
    fn insert_json(&self, m: &mut serde_json::Map<String, serde_json::Value>) {
        let list = |envs: &[SessionEnvInfo]| {
            serde_json::json!(envs
                .iter()
                .map(|e| serde_json::json!({"name": e.name, "source": e.source}))
                .collect::<Vec<_>>())
        };
        m.insert("session_environments".into(), list(self.session_envs));
        if !self.disabled_envs.is_empty() {
            m.insert(
                "disabled_environments".into(),
                serde_json::json!(self
                    .disabled_envs
                    .iter()
                    .map(|d| {
                        let mut o = serde_json::json!({
                            "name": d.env.name,
                            "source": d.env.source,
                            "status": d.status,
                        });
                        if let Some(group) = &d.exclusive_group {
                            o["exclusive_group"] = serde_json::json!(group);
                        }
                        if let Some(w) = &d.superseded_by {
                            o["superseded_by"] =
                                serde_json::json!({"name": w.name, "source": w.source});
                        }
                        o
                    })
                    .collect::<Vec<_>>()),
            );
        }
        if !self.declined_envs.is_empty() {
            m.insert("declined_environments".into(), list(self.declined_envs));
        }
        if !self.not_found.is_empty() {
            m.insert(
                "optional_entries_not_found".into(),
                serde_json::json!(self
                    .not_found
                    .iter()
                    .map(|(i, name)| serde_json::json!({
                        "dependency_index": i,
                        "environment": name,
                    }))
                    .collect::<Vec<_>>()),
            );
        }
        if let Some(hr) = self.host_requirements {
            m.insert("host_requirements".into(), host_requirements_json(hr));
        }
        if !self.discarded.is_empty() {
            m.insert(
                "discarded_host_requirements".into(),
                serde_json::json!(self
                    .discarded
                    .iter()
                    .map(|d| {
                        let mut o = serde_json::json!({"kind": d.kind, "name": d.name});
                        if let Some(e) = &d.environment {
                            o["environment"] =
                                serde_json::json!({"name": e.name, "source": e.source});
                        }
                        o
                    })
                    .collect::<Vec<_>>()),
            );
        }
    }

    /// `Session environments:` with one `- Name (source)` line each (or
    /// `(none)`), then, when there are any, `Disabled environments:` in the
    /// same form with `: disabled` or `: superseded by Name (source)` after
    /// each,
    /// `Declined environments:` in the same form, and `Optional entries not
    /// found:` with one `- dependencies[i]: Name` line each, then `Host
    /// requirements:` with one line per requirement,
    /// then, when there are any, `Discarded host requirements:` with one
    /// `- kind name (Step)` (or `(Service)`, by `owner`) or `- kind name
    /// (Environment Name, source)` line each, indented by `indent`.
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
            for d in self.disabled_envs {
                write!(
                    f,
                    "{indent}  - {} ({}): {}",
                    d.env.name, d.env.source, d.status
                )?;
                if let Some(w) = &d.superseded_by {
                    write!(f, " by {} ({})", w.name, w.source)?;
                }
                writeln!(f)?;
            }
        }
        if !self.declined_envs.is_empty() {
            writeln!(f, "{indent}Declined environments:")?;
            for e in self.declined_envs {
                writeln!(f, "{indent}  - {} ({})", e.name, e.source)?;
            }
        }
        if !self.not_found.is_empty() {
            writeln!(f, "{indent}Optional entries not found:")?;
            for (i, name) in self.not_found {
                writeln!(f, "{indent}  - dependencies[{i}]: {name}")?;
            }
        }
        if let Some(hr) = self.host_requirements {
            writeln!(f, "{indent}Host requirements:")?;
            for line in host_requirement_lines(hr) {
                writeln!(f, "{indent}  - {line}")?;
            }
        }
        if !self.discarded.is_empty() {
            writeln!(f, "{indent}Discarded host requirements:")?;
            for d in self.discarded {
                match &d.environment {
                    None => writeln!(f, "{indent}  - {} {} ({})", d.kind, d.name, self.owner)?,
                    Some(e) => writeln!(
                        f,
                        "{indent}  - {} {} (Environment {}, {})",
                        d.kind, d.name, e.name, e.source
                    )?,
                }
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
    env_param_infos_of(step.environment_parameters.as_deref())
}

/// The effective Environment parameter values `values` of a Step or (RFC
/// 0009 `<Service>` item 4) a Service, in the order its Sessions enter the
/// Environments.
fn env_param_infos_of(values: Option<&[job::EnvironmentParameterValues]>) -> Vec<EnvParamInfo> {
    values
        .into_iter()
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
struct ServiceInfo<'a> {
    name: String,
    description: Option<String>,
    /// `Some(<document>)` for an external Service from an attached
    /// Environment Template.
    document: Option<String>,
    scope: String,
    ports: Vec<PortInfo>,
    health_check: String,
    max_restarts: u64,
    /// `KEEP` / `RERUN`, or `None` when the template gave none (allowed
    /// only with `maxRestarts` 0).
    completed_tasks: Option<String>,
    deps: Vec<DepInfo>,
    /// Its effective Environment parameter values (RFC 0009 `<Service>`
    /// item 4), shown as for a Step.
    env_params: Vec<EnvParamInfo>,
    /// The Job Environments its Sessions enter, the ones disabled,
    /// superseded or declined for it, its optional entries that named
    /// nothing, its combined host requirements (RFC 0009 `<Service>` item
    /// 5), and the ones discarded (§3.3), shown as for a Step.
    env_params_details: EnvParamsDetails<'a>,
}

/// One `externalServices` entry (RFC 0009 §9.8).
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
