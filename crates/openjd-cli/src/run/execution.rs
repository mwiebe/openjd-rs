// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Execution phases for the `openjd run` command.

use super::simulator::{Begin, Simulator};
use super::*;

pub(super) async fn execute(args: RunArgs) -> Result<(), RunError> {
    if args.verbose {
        log::set_max_level(log::LevelFilter::Debug);
    }

    let started_at = Instant::now();
    let _ = crate::SESSION_START.set(started_at);
    let _ = crate::TIMESTAMP_FORMAT.set(args.timestamp_format.clone());

    let prepared = prepare_run(&args)?;
    let selection = prepare_selection(&args, &prepared.job, &prepared.job_template)?;
    let mut sim = prepare_simulator(&args, &prepared, &selection)?;
    validate_wrap_environment_stacks(
        &prepared.job,
        &prepared.env_templates,
        &selection.steps_to_run,
    )?;
    let env_template_symtab = openjd_model::build_symbol_table(&prepared.param_values)
        .map_err(|e| format!("Failed to build environment template symbol table: {e}"))?;

    let cancel_token = CancellationToken::new();
    let session = create_session(&args, &prepared, cancel_token.clone())?;
    let working_dir = session.working_directory().to_path_buf();
    let interrupted = install_signal_handler(cancel_token);
    let mut ctx = RunContext {
        session,
        entered_envs: Vec::new(),
        interrupted,
        started_at,
        tasks_run: 0,
        session_failed: false,
    };

    println!("{}\tSession start", ctx.timestamp());
    println!("{}\tRunning job '{}'", ctx.timestamp(), prepared.job.name);
    let execution_result = run_workload(
        &mut ctx,
        &args,
        &mut sim,
        &prepared.env_templates,
        &env_template_symtab,
        &selection,
    )
    .await;

    // Once a Session exists, errors must not bypass environment cleanup.
    ctx.exit_environments_down_to(0).await;
    ctx.record_interruption();
    if let Err(e) = execution_result {
        if !args.preserve {
            ctx.session.cleanup();
        }
        return Err(e);
    }

    report_result(&mut ctx, &args, &prepared, &sim, &working_dir);
    if ctx.session_failed {
        std::process::exit(1);
    }
    Ok(())
}

fn prepare_run(args: &RunArgs) -> Result<PreparedRun, RunError> {
    let path = &args.path;
    let content = crate::common::read_input_file(path)?;
    let template_value = parse::document_string_to_object(
        &content,
        crate::common::document_type(path),
        &crate::common::caller_limits(),
    )?;
    let exts = crate::common::parse_extensions(&args.extensions)?;
    let supported_exts: Vec<&str> = exts.iter().map(String::as_str).collect();
    let job_template = parse::decode_job_template(
        template_value,
        Some(&supported_exts),
        &crate::common::caller_limits(),
    )?;

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

    let input_values = parse_cli_parameters(&args.parameters)?;
    let path_rules = load_path_mapping_rules(&args.path_mapping_rules)?;
    let canonical_path = std::fs::canonicalize(path)?;
    let job_template_dir = strip_extended_prefix(
        canonical_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new(".")),
    );
    let current_working_dir = strip_extended_prefix(&std::env::current_dir()?);
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
    )
    .map_err(|e| format!("{e}\n\n{}", crate::help::format_help(&job_template, path)))?;

    // Derive the profile and context from the template itself —
    // `create_job` requires the context's revision/extensions to cover
    // the template's — and carry the CLI's caller limits so job
    // creation enforces the same caps the decode ran under.
    let revision_profile = job_template.profile();
    let revision_ctx = job_template
        .default_validation_context()
        .with_caller_limits(crate::common::caller_limits());
    let job = openjd_model::create_job(&job_template, &param_values, &revision_ctx)
        .map_err(|e| format!("{e}\n\n{}", crate::help::format_help(&job_template, path)))?;

    Ok(PreparedRun {
        job_template,
        validation_ctx: revision_ctx,
        job,
        env_templates,
        param_values,
        path_rules,
        revision_profile,
    })
}

fn prepare_selection(
    args: &RunArgs,
    job: &Job,
    job_template: &openjd_model::template::JobTemplate,
) -> Result<RunSelection, RunError> {
    let selected_step_idx = if let Some(step_name) = &args.step {
        Some(
            job.steps
                .iter()
                .position(|step| step.name == *step_name)
                .ok_or_else(|| {
                    format!(
                        "No Step with name '{}' is defined in the given Job Template.",
                        step_name
                    )
                })?,
        )
    } else if job.steps.len() == 1 {
        Some(0)
    } else {
        if !args.task_params.is_empty() || args.tasks.is_some() {
            return Err(format!(
                "Providing task parameters requires a specified step or a job with a single step.\n{} steps: {:?}.",
                job.steps.len(),
                job.steps.iter().map(|step| &step.name).collect::<Vec<_>>()
            )
            .into());
        }
        None
    };

    let explicit_task_params = if !args.task_params.is_empty() {
        Some(vec![parse_task_params(&args.task_params)?])
    } else if let Some(tasks_arg) = &args.tasks {
        Some(parse_tasks_arg(tasks_arg)?)
    } else {
        None
    };
    if explicit_task_params.is_some() {
        let selected_idx = selected_step_idx.ok_or(
            "Providing task parameters requires a specified step or a job with a single step.",
        )?;
        let selected = &job.steps[selected_idx];
        let option = if args.task_params.is_empty() {
            "--tasks"
        } else {
            "--task-param"
        };
        if matches!(selected.binding, openjd_model::job::StepBinding::Skipped) {
            return Err(format!(
                "Step '{}' is skipped by its enableIf; {option} cannot be used.",
                selected.name
            )
            .into());
        }
        // Whether the Step has a parameterSpace is fixed by the template,
        // even for a Step pending inputs, whose space is not materialized.
        let has_space = job_template
            .steps
            .get(selected_idx)
            .map_or(selected.parameter_space.is_some(), |t| {
                t.parameter_space.is_some()
            });
        if !has_space {
            return Err(no_parameter_space(&selected.name, option).into());
        }
    }
    let steps_to_run = if let Some(selected_idx) = selected_step_idx {
        if args.run_dependencies {
            resolve_step_dependencies(job, selected_idx)
        } else {
            vec![selected_idx]
        }
    } else {
        StepDependencyGraph::new(job)?.topo_sorted()?
    };

    Ok(RunSelection {
        selected_step_idx,
        explicit_task_params,
        steps_to_run,
    })
}

fn no_parameter_space(step: &str, option: &str) -> String {
    format!("Step '{step}' does not define a parameterSpace; {option} cannot be used.")
}

/// The scheduler simulator for the run, with the `--step-inputs` values given
/// to a Step run alone, and every `Steps.*` value it reads checked to be
/// concrete before the Session starts.
fn prepare_simulator(
    args: &RunArgs,
    prepared: &PreparedRun,
    selection: &RunSelection,
) -> Result<Simulator, RunError> {
    let alone = selection
        .selected_step_idx
        .filter(|_| !args.run_dependencies);
    let step_inputs = match (&args.step_inputs, alone) {
        (None, _) => None,
        (Some(arg), Some(_)) => Some(parse_step_inputs(arg)?),
        (Some(_), None) => return Err(
            "--step-inputs requires --step (or a Job with one Step) without --run-dependencies."
                .into(),
        ),
    };
    let mut sim = Simulator::new(
        prepared.job_template.clone(),
        prepared.param_values.clone(),
        prepared.validation_ctx.clone(),
        prepared.job.clone(),
        &selection.steps_to_run,
    )?;
    if let Some(target) = alone {
        sim.give_step_inputs(target, step_inputs.as_ref())?;
    }
    Ok(sim)
}

fn validate_wrap_environment_stacks(
    job: &Job,
    env_templates: &[EnvironmentTemplate],
    steps_to_run: &[usize],
) -> Result<(), RunError> {
    // A disabled Environment is not entered, so its hooks do not count.
    let has_wrap_hook = |env: &Environment| {
        env.enabled
            && env
                .script
                .as_ref()
                .is_some_and(|script| script.actions.has_any_wrap_hook())
    };
    let mut base_wrap_envs: Vec<&str> = env_templates
        .iter()
        .filter(|template| {
            template
                .environment
                .script
                .as_ref()
                .is_some_and(|script| script.actions.has_any_wrap_hook())
        })
        .map(|template| template.environment.name.as_str())
        .collect();
    if let Some(job_envs) = &job.job_environments {
        base_wrap_envs.extend(
            job_envs
                .iter()
                .filter(|env| has_wrap_hook(env))
                .map(|env| env.name.as_str()),
        );
    }
    reject_multiple_wrap_environments(&base_wrap_envs)?;

    for &step_idx in steps_to_run {
        let mut stack_wrap_envs = base_wrap_envs.clone();
        if let Some(step_envs) = &job.steps[step_idx].step_environments {
            stack_wrap_envs.extend(
                step_envs
                    .iter()
                    .filter(|env| has_wrap_hook(env))
                    .map(|env| env.name.as_str()),
            );
        }
        reject_multiple_wrap_environments(&stack_wrap_envs)?;
    }
    Ok(())
}

fn reject_multiple_wrap_environments(names: &[&str]) -> Result<(), RunError> {
    if names.len() <= 1 {
        return Ok(());
    }
    Err(format!(
        "RFC 0008: a session may have at most one Environment defining wrap hooks \
         (onWrapEnvEnter / onWrapTaskRun / onWrapEnvExit). Found {}: {}.",
        names.len(),
        names.join(", ")
    )
    .into())
}

fn create_session(
    args: &RunArgs,
    prepared: &PreparedRun,
    cancel_token: CancellationToken,
) -> Result<Session, RunError> {
    let session_config = openjd_sessions::session::SessionConfig {
        session_id: format!("cli-{}", std::process::id()),
        job_parameter_values: prepared.param_values.clone(),
        path_mapping_rules: (!prepared.path_rules.is_empty()).then(|| prepared.path_rules.clone()),
        retain_working_dir: args.preserve,
        callback: None,
        os_env_vars: None,
        session_root_directory: None,
        user: None,
        profile: Some(prepared.revision_profile.clone()),
        cancel_token: Some(cancel_token),
        sticky_bit_policy: Default::default(),
        debug_collect_stdout: false,
        echo_openjd_directives: true,
        // `revision_profile` is the Job Template's own profile, so this is
        // whether the template lists CONTROL_FLOW.
        control_flow: prepared
            .revision_profile
            .has_extension(openjd_model::types::ModelExtension::ControlFlow),
        output_callback: None,
        // Run-time mirror of the CLI's caller-limits policy: derived from
        // the same `common::caller_limits()` value the decode/creation
        // stages use, so the two cannot drift.
        limits: openjd_sessions::SessionLimits::from(&crate::common::caller_limits()),
    };
    Session::with_config(session_config)
        .map_err(|e| format!("Failed to create session: {e}").into())
}

fn install_signal_handler(cancel_token: CancellationToken) -> Arc<AtomicBool> {
    let interrupted = Arc::new(AtomicBool::new(false));
    let handler_interrupted = interrupted.clone();
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            let mut sigint =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                    .expect("failed to install SIGINT handler");
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("failed to install SIGTERM handler");
            tokio::select! {
                _ = sigint.recv() => {}
                _ = sigterm.recv() => {}
            }
        }
        #[cfg(windows)]
        {
            // Ctrl+Break is the only console event another process can
            // target at this process group alone (CTRL_C_EVENT is
            // broadcast to every process on the console), so listening
            // for it gives worker tooling a graceful interruption channel.
            let mut ctrl_c =
                tokio::signal::windows::ctrl_c().expect("failed to install Ctrl+C handler");
            let mut ctrl_break =
                tokio::signal::windows::ctrl_break().expect("failed to install Ctrl+Break handler");
            tokio::select! {
                _ = ctrl_c.recv() => {}
                _ = ctrl_break.recv() => {}
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        handler_interrupted.store(true, Ordering::SeqCst);
        cancel_token.cancel();
    });
    interrupted
}

async fn run_workload(
    ctx: &mut RunContext,
    args: &RunArgs,
    sim: &mut Simulator,
    env_templates: &[EnvironmentTemplate],
    env_template_symtab: &openjd_expr::SymbolTable,
    selection: &RunSelection,
) -> Result<(), RunError> {
    sim.report_initial();
    // `--step-inputs` values can bind the Step run alone, and a job output
    // can read them.
    if sim.settle()? {
        ctx.session_failed = true;
    }
    for template in env_templates {
        if ctx.is_stopping() {
            break;
        }
        let env = openjd_model::convert_environment_with_symtab(
            &template.environment,
            Some(env_template_symtab),
        );
        ctx.enter_environment(
            &env,
            &template.environment.name,
            None,
            openjd_sessions::session::EnvironmentOrigin::EnvironmentTemplate,
            &[],
        )
        .await;
    }
    if let Some(job_envs) = &sim.job.job_environments {
        for env in job_envs {
            if ctx.is_stopping() {
                break;
            }
            if !env.enabled {
                println!(
                    "{}\tJob Environment '{}' is disabled by its enableIf; not entering it",
                    ctx.timestamp(),
                    env.name
                );
                continue;
            }
            ctx.enter_environment(
                env,
                &env.name,
                None,
                openjd_sessions::session::EnvironmentOrigin::JobTemplate,
                &[],
            )
            .await;
        }
    }

    for &step_idx in &selection.steps_to_run {
        if ctx.is_stopping() {
            break;
        }
        let explicit_tasks = selection.explicit_task_params.is_some()
            && selection.selected_step_idx == Some(step_idx);
        match sim.begin_step(step_idx, explicit_tasks)? {
            Begin::Failed => {
                ctx.session_failed = true;
                break;
            }
            Begin::NotRun => {}
            Begin::Run(step) => {
                let ran_all = execute_step(ctx, args, &step, step_idx, selection, sim).await?;
                if ctx.session_failed && ctx.interrupted.load(Ordering::SeqCst) {
                    sim.set_reason(step_idx, "the run was interrupted");
                }
                sim.finish_step(step_idx, ctx.session_failed, ran_all)?;
            }
        }
        if !ctx.is_stopping() && sim.settle()? {
            ctx.session_failed = true;
        }
    }
    sim.block_unreachable(ctx.interrupted.load(Ordering::SeqCst));
    Ok(())
}

async fn execute_step(
    ctx: &mut RunContext,
    args: &RunArgs,
    step: &Step,
    step_idx: usize,
    selection: &RunSelection,
    sim: &mut Simulator,
) -> Result<bool, RunError> {
    println!("{}\tRunning step '{}'", ctx.timestamp(), step.name);
    let environment_baseline = ctx.entered_envs.len();
    if let Some(step_envs) = &step.step_environments {
        for env in step_envs {
            if ctx.is_stopping() {
                break;
            }
            if !env.enabled {
                println!(
                    "{}\tStep Environment '{}' is disabled by its enableIf; not entering it",
                    ctx.timestamp(),
                    env.name
                );
                continue;
            }
            ctx.enter_environment(
                env,
                &env.name,
                step.resolved_symtab.clone(),
                openjd_sessions::session::EnvironmentOrigin::JobTemplate,
                &step.path_inputs,
            )
            .await;
        }
    }

    let result = if ctx.session_failed {
        Ok(false)
    } else {
        execute_step_tasks(ctx, args, step, step_idx, selection, sim).await
    };
    // This unwind happens before propagating task setup/runtime errors.
    ctx.exit_environments_down_to(environment_baseline).await;
    result
}

/// Run the Step's Tasks, returning whether every one of them ran (not so
/// after `--maximum-tasks`, or for Tasks given with `--tasks` or
/// `--task-param`).
async fn execute_step_tasks(
    ctx: &mut RunContext,
    args: &RunArgs,
    step: &Step,
    step_idx: usize,
    selection: &RunSelection,
    sim: &mut Simulator,
) -> Result<bool, RunError> {
    let explicit = selection
        .explicit_task_params
        .as_deref()
        .filter(|_| selection.selected_step_idx == Some(step_idx));
    let Some(parameter_space) = &step.parameter_space else {
        if explicit.is_some() {
            let option = if args.task_params.is_empty() {
                "--tasks"
            } else {
                "--task-param"
            };
            return Err(no_parameter_space(&step.name, option).into());
        }
        ctx.print_banner("Running Task");
        let run = ctx.run_task(step, None).await?;
        if run.succeeded {
            sim.record_action(step_idx, None, run.outputs);
        }
        return Ok(true);
    };

    let mut iter = openjd_model::StepParameterSpaceIterator::new(parameter_space)?;
    if let Some(task_param_sets) = explicit {
        return execute_explicit_tasks(
            ctx,
            step,
            step_idx,
            parameter_space,
            task_param_sets,
            &iter,
            sim,
        )
        .await;
    }
    // `iter.len()` is not the Task count of an adaptively chunked space.
    if step.tasks_known() == Some(0) {
        // Printed only for CONTROL_FLOW, so other templates' output is
        // unchanged.
        if sim.control_flow() {
            println!("{}\tStep '{}' has 0 tasks", ctx.timestamp(), step.name);
        }
        return Ok(true);
    }
    execute_parameter_space_tasks(ctx, args.maximum_tasks, step, step_idx, &mut iter, sim).await
}

/// Run the Tasks given with `--tasks` or `--task-param`, returning whether
/// they ran every Task of the Step: as many distinct Tasks as it has, all
/// run, counting each `CHUNK[INT]` value of a chunk as one Task.
async fn execute_explicit_tasks(
    ctx: &mut RunContext,
    step: &Step,
    step_idx: usize,
    parameter_space: &openjd_model::job::StepParameterSpace,
    task_param_sets: &[HashMap<String, String>],
    iter: &openjd_model::StepParameterSpaceIterator,
    sim: &mut Simulator,
) -> Result<bool, RunError> {
    let mut typed_sets = Vec::with_capacity(task_param_sets.len());
    for (index, params) in task_param_sets.iter().enumerate() {
        let values = coerce_task_params(params, parameter_space)?;
        iter.validate_containment(&values)
            .map_err(|e| format!("Task parameter set {index}: {e}"))?;
        typed_sets.push(values);
    }

    let chunk_param = iter.chunks_parameter_name().map(str::to_string);
    let mut distinct = std::collections::HashSet::new();
    let mut ran = 0;
    for (params, values) in task_param_sets.iter().zip(&typed_sets) {
        if ctx.is_stopping() {
            break;
        }
        // A FLOAT is compared by number, not spelling.
        let mut key: Vec<String> = values
            .iter()
            .filter(|(name, _)| Some(name.as_str()) != chunk_param.as_deref())
            .map(|(name, v)| match &v.value {
                openjd_expr::ExprValue::Float(f) => {
                    format!("{name}={:#x}", (f.value() + 0.0).to_bits())
                }
                other => format!("{name}={}", other.to_display_string()),
            })
            .collect();
        key.sort_unstable();
        // A chunk names one Task per CHUNK[INT] value.
        match chunk_param.as_deref().and_then(|c| values.get(c)) {
            Some(chunk) => {
                let chunk_values: Vec<i64> = match &chunk.value {
                    openjd_expr::ExprValue::RangeExpr(r) => r.iter().collect(),
                    openjd_expr::ExprValue::Int(i) => vec![*i],
                    _ => Vec::new(),
                };
                for value in chunk_values {
                    distinct.insert((key.clone(), Some(value)));
                }
            }
            None => {
                distinct.insert((key, None));
            }
        }
        ran += 1;
        ctx.print_banner("Running Task");
        println!("{}\tParameter values:", ctx.timestamp());
        for (name, value) in params {
            println!("{}\t{name} = {value}", ctx.timestamp());
        }
        let run = ctx.run_task(step, Some(values)).await?;
        if run.succeeded {
            sim.record_action(step_idx, Some(values), run.outputs);
        }
    }
    let all_given = ran == typed_sets.len();
    Ok(all_given && Some(distinct.len() as u64) == step.tasks_known())
}

async fn execute_parameter_space_tasks(
    ctx: &mut RunContext,
    maximum_tasks: i64,
    step: &Step,
    step_idx: usize,
    iter: &mut openjd_model::StepParameterSpaceIterator,
    sim: &mut Simulator,
) -> Result<bool, RunError> {
    let is_adaptive = iter.chunks_adaptive();
    let chunks_param_name = iter.chunks_parameter_name().map(String::from);
    let target_runtime_seconds = adaptive_target_runtime(step, is_adaptive);
    let mut completed_task_count = 0usize;
    let mut completed_task_duration = 0.0;
    let mut remaining_tasks = if maximum_tasks > 0 {
        maximum_tasks
    } else {
        i64::MAX
    };

    let mut exhausted = false;
    while remaining_tasks > 0 && !ctx.is_stopping() {
        let Some(task_params) = iter.next() else {
            exhausted = true;
            break;
        };
        ctx.print_banner("Running Task");
        println!("{}\tParameter values:", ctx.timestamp());
        for (name, value) in &task_params {
            println!(
                "{}\t{}({}) = {}",
                ctx.timestamp(),
                name,
                value.param_type.as_spec_str(),
                value.value.to_display_string()
            );
        }
        let task_values: TaskParameterSet = task_params
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let run = ctx.run_task(step, Some(&task_values)).await?;
        if run.succeeded {
            sim.record_action(step_idx, Some(&task_values), run.outputs);
        }
        let task_duration = run.duration;
        remaining_tasks -= 1;

        if is_adaptive && !ctx.is_stopping() {
            let chunk_items = chunks_param_name
                .as_ref()
                .and_then(|name| task_params.get(name))
                .map(|value| match &value.value {
                    openjd_expr::ExprValue::RangeExpr(range) => range.len(),
                    _ => 1,
                })
                .unwrap_or(1);
            completed_task_count += chunk_items;
            completed_task_duration += task_duration;
            adjust_adaptive_chunk_size(
                ctx,
                iter,
                completed_task_count,
                completed_task_duration,
                target_runtime_seconds,
            );
        }
    }
    // A run stopped by --maximum-tasks may still have run every Task.
    if !exhausted && remaining_tasks == 0 && !ctx.is_stopping() {
        exhausted = iter.next().is_none();
    }
    Ok(exhausted)
}

fn adaptive_target_runtime(step: &Step, is_adaptive: bool) -> f64 {
    if !is_adaptive {
        return 0.0;
    }
    step.parameter_space
        .as_ref()
        .and_then(|space| {
            space
                .task_parameter_definitions
                .values()
                .find_map(|definition| match definition {
                    openjd_model::job::TaskParameter::ChunkInt { chunks, .. } => {
                        chunks.target_runtime_seconds.map(|seconds| seconds as f64)
                    }
                    _ => None,
                })
        })
        .unwrap_or(0.0)
}

fn calculate_adaptive_chunk_size(
    current_chunk_size: usize,
    completed_task_count: usize,
    completed_task_duration: f64,
    target_runtime_seconds: f64,
) -> Option<usize> {
    if completed_task_count == 0
        || completed_task_duration <= 0.0
        || !completed_task_duration.is_finite()
    {
        return None;
    }

    let duration_per_task = completed_task_duration / completed_task_count as f64;
    let mut adaptive_chunk_size = target_runtime_seconds / duration_per_task;
    if completed_task_count < 10 && adaptive_chunk_size > current_chunk_size as f64 {
        adaptive_chunk_size = 0.75 * current_chunk_size as f64 + 0.25 * adaptive_chunk_size;
    }

    Some((adaptive_chunk_size as usize).max(1))
}

fn adjust_adaptive_chunk_size(
    ctx: &RunContext,
    iter: &mut openjd_model::StepParameterSpaceIterator,
    completed_task_count: usize,
    completed_task_duration: f64,
    target_runtime_seconds: f64,
) {
    let current_chunk_size = iter.chunks_default_task_count().unwrap_or(1);
    let Some(adaptive_chunk_size) = calculate_adaptive_chunk_size(
        current_chunk_size,
        completed_task_count,
        completed_task_duration,
        target_runtime_seconds,
    ) else {
        return;
    };

    if Some(adaptive_chunk_size) != iter.chunks_default_task_count() {
        println!(
            "{}\tAdjusting chunk size to {adaptive_chunk_size}",
            ctx.timestamp()
        );
        iter.set_chunks_default_task_count(adaptive_chunk_size);
    }
}

fn report_result(
    ctx: &mut RunContext,
    args: &RunArgs,
    prepared: &PreparedRun,
    sim: &Simulator,
    working_dir: &std::path::Path,
) {
    let job = &prepared.job;
    sim.report_not_evaluated();
    println!("{}\t", ctx.timestamp());
    if ctx.session_failed {
        println!("{}\tSession ended with errors.", ctx.timestamp());
    } else {
        println!("{}\tAll actions completed successfully!", ctx.timestamp());
    }
    println!("{}\tLocal session ended.", ctx.timestamp());

    let duration = ctx.started_at.elapsed().as_secs_f64();
    let preserved_msg = if args.preserve {
        format!(
            "\nWorking directory preserved at: {}",
            working_dir.display()
        )
    } else {
        ctx.session.cleanup();
        String::new()
    };
    let (status, message) = if ctx.session_failed {
        (
            "error",
            format!("Session ended with errors; see Task logs for details{preserved_msg}"),
        )
    } else {
        (
            "success",
            format!("Session ended successfully{preserved_msg}"),
        )
    };
    let result = RunResult {
        status: status.to_string(),
        message,
        job_name: job.name.clone(),
        step_name: args.step.clone(),
        duration,
        chunks_run: ctx.tasks_run,
        control_flow: prepared
            .revision_profile
            .has_extension(openjd_model::types::ModelExtension::ControlFlow)
            .then(|| result::ControlFlowResult {
                steps: sim.steps_json(),
                job_outputs: sim.job_outputs_json(),
                job_output_lines: sim.job_output_lines(),
                not_evaluated: sim.not_evaluated(),
            }),
    };
    crate::common::print_cli_result(&result, &args.output);
}

#[cfg(test)]
mod tests {
    use super::calculate_adaptive_chunk_size;

    #[test]
    fn zero_duration_defers_adaptive_chunk_adjustment() {
        assert_eq!(calculate_adaptive_chunk_size(1, 1, 0.0, 60.0), None);
    }

    #[test]
    fn measurable_duration_preserves_early_sample_blending() {
        assert_eq!(calculate_adaptive_chunk_size(1, 1, 1.0, 9.0), Some(3));
    }
}
