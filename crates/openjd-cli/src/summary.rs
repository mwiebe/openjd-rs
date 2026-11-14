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

    /// Print information about this Step or loop only (not a loop's body Step)
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
    // The Environments an `enableIf` disabled for each Step (§4 item 6),
    // or an earlier one of their `exclusiveGroup` superseded (§4 item 7),
    // each with which, the ones it declined, and its optional entries
    // that named nothing, are listed too.
    // With `CONTROL_FLOW`, a skipped Step has no Environment set, and one
    // whose `enableIf` is decided at run time makes its Environment
    // selection then (RFC 0011 `<Environment>` item 3), so neither is
    // listed.
    let session_envs: Vec<Option<StepEnvSets>> = if lists_env_params(&the_job, &env_templates) {
        the_job
            .steps
            .iter()
            .zip(&created.environment_statuses)
            .zip(&created.discarded_host_requirements)
            .map(|((s, statuses), discarded)| {
                environments_decided(s).then(|| StepEnvSets {
                    entered: session_env_infos(
                        &job_template,
                        &the_job,
                        &created.environment_documents,
                        s,
                    ),
                    disabled: disabled_env_infos(statuses),
                    declined: declined_env_infos(statuses),
                    not_found: statuses
                        .not_found
                        .iter()
                        .map(|n| (n.dependency_index, n.environment.clone()))
                        .collect(),
                    discarded: discarded.iter().map(DiscardedInfo::from).collect(),
                })
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
            &session_envs,
            step_name,
            &task_params,
            control_flow,
            &args.output,
        )
    } else {
        output_job_summary(
            &job_template,
            &the_job,
            &session_envs,
            &task_params,
            control_flow,
            &args.output,
        )
    }
}

/// Whether `step`'s Environments are decided: not for a skipped Step, nor
/// for one whose `enableIf` is not decided yet (`CONTROL_FLOW`).
fn environments_decided(step: &job::Step) -> bool {
    match &step.binding {
        job::StepBinding::Bound => true,
        job::StepBinding::PendingInputs(p) => p.enablement.is_known(),
        job::StepBinding::Skipped => false,
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

/// A Step's Environment set, and the Environments an `enableIf` or an
/// `exclusiveGroup` took out of it.
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

/// One discarded host requirement of a Step (Template Schemas §3.3).
struct DiscardedInfo {
    /// `amount` or `attribute`.
    kind: &'static str,
    /// The resolved capability name.
    name: String,
    /// The Environment whose `hostRequirements` held it; `None` for the
    /// Step's own.
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
/// Step Environments. With `CONTROL_FLOW`, a Step Environment whose
/// `enableIf` the Step's remaining group has not decided yet (RFC 0011
/// `<Environment>` item 4) is marked as entered only if its `enableIf` is
/// `true`.
fn session_env_infos(
    jt: &openjd_model::template::JobTemplate,
    job: &job::Job,
    documents: &[job::Document],
    step: &job::Step,
) -> Vec<SessionEnvInfo> {
    let undecided =
        matches!(&step.binding, job::StepBinding::PendingInputs(p) if !p.remaining.is_known());
    let template_envs: Vec<&openjd_model::template::Environment> = jt
        .step_templates()
        .find(|r| r.body_index.is_none() && r.step.name == step.name)
        .into_iter()
        .flat_map(|r| r.step.step_environments.iter().flatten())
        .collect();
    let has_enable_if = |name: &str| {
        template_envs
            .iter()
            .any(|e| e.name == name && e.enable_if.is_some())
    };
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
                    source: if undecided && has_enable_if(&env.name) {
                        "stepEnvironments, if its enableIf is true".to_string()
                    } else {
                        "stepEnvironments".to_string()
                    },
                }),
        )
        .collect()
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
                })
                .collect(),
            deps: st
                .dependencies
                .iter()
                .flatten()
                .filter_map(|d| d.step().map(str::to_string))
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
    session_envs: &[Option<StepEnvSets>],
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
                    env_params: Vec::new(),
                    env_params_details: None,
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
                env_params: env_param_infos(s),
                env_params_details: session_envs
                    .get(node.step_index)
                    .and_then(Option::as_ref)
                    .map(|envs| EnvParamsDetails {
                        session_envs: &envs.entered,
                        disabled_envs: &envs.disabled,
                        declined_envs: &envs.declined,
                        not_found: &envs.not_found,
                        host_requirements: s.host_requirements.as_ref(),
                        discarded: &envs.discarded,
                    }),
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
                })
                .collect()
        })
        .unwrap_or_default();
    let total_envs = root_envs.len() + step_envs;

    // Collect parameter summaries in template definition order
    // Preserve template parameter definition order
    let param_order: Vec<String> = jt
        .parameter_definitions_list()
        .iter()
        .map(|p| p.name().to_string())
        .collect();
    let param_descriptions: std::collections::HashMap<&str, &str> = jt
        .parameter_definitions_list()
        .iter()
        .filter_map(|p| p.description().map(|d| (p.name(), d)))
        .collect();
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
    session_envs: &[Option<StepEnvSets>],
    step_name: &str,
    template_task_params: &std::collections::HashMap<String, Vec<(String, String)>>,
    control_flow: bool,
    output_format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let step_index = job
        .steps
        .iter()
        .position(|s| s.name == step_name)
        .ok_or_else(|| format!("Step '{step_name}' does not exist in Job '{}'.", job.name))?;
    let step = &job.steps[step_index];

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
        env_params_details: session_envs
            .get(step_index)
            .and_then(Option::as_ref)
            .map(|envs| EnvParamsDetails {
                session_envs: &envs.entered,
                disabled_envs: &envs.disabled,
                declined_envs: &envs.declined,
                not_found: &envs.not_found,
                host_requirements: step.host_requirements.as_ref(),
                discarded: &envs.discarded,
            }),
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

struct JobSummaryResult<'a> {
    name: String,
    params: Vec<ParamInfo>,
    step_summaries: Vec<StepInfo<'a>>,
    root_envs: Vec<EnvInfo>,
    step_envs: Vec<EnvInfo>,
    /// The Tasks of every Step whose Tasks are known.
    total_tasks: u64,
    /// Whether some Step's Tasks are determined at run time.
    tasks_at_run_time: bool,
    total_envs: usize,
    job_outputs: Vec<JobOutputInfo>,
}

impl JobSummaryResult<'_> {
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
                info.write_details(f, "  ", true)?;
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
            if let Some(d) = &s.env_params_details {
                d.write(f, "  ")?;
            }
            write_env_params(f, &s.env_params, "  ")?;
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

struct StepSummaryResult<'a> {
    job_name: String,
    step_name: String,
    total_tasks: Option<u64>,
    task_params: Vec<(String, String)>,
    envs: Vec<EnvInfo>,
    deps: Vec<String>,
    env_params: Vec<EnvParamInfo>,
    env_params_details: Option<EnvParamsDetails<'a>>,
    state: Option<&'static str>,
    binding_lines: Vec<String>,
    binding_json: Option<serde_json::Value>,
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
    /// `None` while the Step's Tasks are determined at run time.
    total_tasks: Option<u64>,
    task_params: Vec<(String, String)>,
    envs: Vec<String>,
    deps: Vec<String>,
    env_params: Vec<EnvParamInfo>,
    /// `Some` when the Job Template or an attached Environment Template
    /// lists `ENV_PARAMS`.
    env_params_details: Option<EnvParamsDetails<'a>>,
    /// The Step's state, for a template that lists `CONTROL_FLOW`.
    state: Option<&'static str>,
    /// What is known of a pending Step (`common::binding_lines`).
    binding_lines: Vec<String>,
    binding_json: Option<serde_json::Value>,
    /// For a loop, what the summary shows of it.
    loop_info: Option<LoopInfo>,
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
    /// `- kind name (Step)` or `- kind name (Environment Name, source)` line
    /// each, indented by `indent`.
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
                    None => writeln!(f, "{indent}  - {} {} (Step)", d.kind, d.name)?,
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

    /// The loop's lines under its heading, each after `indent`, ending with
    /// its dependency count when `count_deps`.
    fn write_details(
        &self,
        f: &mut fmt::Formatter<'_>,
        indent: &str,
        count_deps: bool,
    ) -> fmt::Result {
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
        if count_deps && !self.deps.is_empty() {
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
        // The dependencies are listed by name below.
        self.info.write_details(f, "", false)?;
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

impl StepInfo<'_> {
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
}

impl EnvInfo {
    /// `Name (from 'Parent')`.
    fn heading(&self) -> String {
        if let Some(l) = &self.parent_loop {
            format!(
                "{} (from body Step '{}' of loop '{l}')",
                self.name, self.parent
            )
        } else {
            format!("{} (from '{}')", self.name, self.parent)
        }
    }

    fn to_json(&self) -> serde_json::Value {
        // Only root Environments are listed in JSON, so `parent_loop` is
        // never set here.
        serde_json::json!({"name": self.name, "parent": self.parent})
    }
}
