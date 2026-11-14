// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Execution phases for the `openjd run` command.

use super::simulator::{Begin, IterationEnd, LoopBegin, Node, Simulator, Slot};
use super::*;

pub(super) async fn execute(args: RunArgs) -> Result<(), RunError> {
    if args.verbose {
        log::set_max_level(log::LevelFilter::Debug);
    }

    let started_at = Instant::now();
    let _ = crate::SESSION_START.set(started_at);
    let _ = crate::TIMESTAMP_FORMAT.set(args.timestamp_format.clone());

    let prepared = prepare_run(&args)?;
    // Job creation has checked the RFC 0008 single-layer rule over every
    // Step's Environment set, attached Environments included; with
    // CONTROL_FLOW, step input binding checks it for a Step whose
    // Environments are decided later.
    let selection = prepare_selection(&args, &prepared.job, &prepared.job_template)?;
    let mut sim = prepare_simulator(&args, &prepared, &selection)?;

    let cancel_token = CancellationToken::new();
    let session = create_session(&args, &prepared, cancel_token.clone())?;
    let interrupted = install_signal_handler(cancel_token);
    let mut ctx = RunContext {
        session,
        entered_envs: Vec::new(),
        interrupted,
        started_at,
        tasks_run: 0,
        session_failed: false,
        preserved_dir: None,
    };

    println!("{}\tSession start", ctx.timestamp());
    println!("{}\tRunning job '{}'", ctx.timestamp(), prepared.job.name);
    let execution_result = run_workload(&mut ctx, &args, &prepared, &mut sim, &selection).await;

    // Once a Session exists, errors must not bypass environment cleanup.
    ctx.exit_environments_down_to(0).await;
    ctx.record_interruption();
    if let Err(e) = execution_result {
        ctx.end_session(args.preserve);
        ctx.print_preserved_dir();
        return Err(e);
    }

    report_result(&mut ctx, &args, &prepared, &sim);
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

    let env_templates =
        crate::common::read_environment_templates(&args.environments, &supported_exts)?;

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
    let revision_ctx = crate::common::submission_context(&job_template);

    // Attach the --environment templates, in the order given (Template
    // Schemas §3.2.1): job creation resolves `environment:` entries that
    // name no Job Environment, fixes each Step's Environment set, and
    // computes every Step's values for the attached Environments it
    // enters. The combined Job lists the attached Environments first, so a
    // Session enters them first.
    let labels = crate::common::environment_labels(&args.environments);
    let attached = crate::common::attach(&env_templates, &labels);
    let created = openjd_model::create_job_with_environment_templates(
        &job_template,
        &attached,
        &param_values,
        &revision_ctx,
    )
    .map_err(|e| format!("{e}\n\n{}", crate::help::format_help(&job_template, path)))?;
    crate::common::print_submission_warnings(&created.warnings);

    Ok(PreparedRun {
        job_template,
        validation_ctx: revision_ctx,
        job: created.job,
        environment_documents: created.environment_documents,
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
    // The top-level entries, Steps and loops, by name.
    let entry = |name: &str| {
        job.steps
            .iter()
            .position(|step| step.name == name)
            .map(Node::Step)
            .or_else(|| {
                job.loops
                    .iter()
                    .position(|l| l.name == name)
                    .map(Node::Loop)
            })
    };
    // In template order.
    let graph = StepDependencyGraph::new(job)?;
    let entry_names: Vec<&str> = (0..graph.node_count())
        .filter_map(|n| graph.node(n))
        .map(|n| n.name.as_str())
        .collect();
    let selected = if let Some(step_name) = &args.step {
        match entry(step_name) {
            Some(node) => Some(node),
            None => {
                // RFC 0013 §Implementation Impact: a body Step runs only
                // within its loop's iterations.
                if let Some(l) = job
                    .loops
                    .iter()
                    .find(|l| l.body.iter().any(|b| b == step_name))
                {
                    return Err(format!(
                        "Step '{step_name}' is a body Step of loop '{}', so it runs only within the loop's iterations. Use --step {} to run the whole loop.",
                        l.name, l.name
                    )
                    .into());
                }
                return Err(format!(
                    "No Step with name '{}' is defined in the given Job Template.",
                    step_name
                )
                .into());
            }
        }
    } else if entry_names.len() == 1 {
        Some(if job.steps.len() == 1 {
            Node::Step(0)
        } else {
            Node::Loop(0)
        })
    } else {
        if !args.task_params.is_empty() || args.tasks.is_some() {
            return Err(format!(
                "Providing task parameters requires a specified step or a job with a single step.\n{} steps: {:?}.",
                entry_names.len(),
                entry_names
            )
            .into());
        }
        None
    };

    let option = if args.task_params.is_empty() {
        "--tasks"
    } else {
        "--task-param"
    };
    if let Some(Node::Loop(li)) = selected {
        // RFC 0013 §Implementation Impact: a loop has no Task parameters or
        // Tasks of its own.
        let name = &job.loops[li].name;
        if !args.task_params.is_empty() || args.tasks.is_some() {
            return Err(format!(
                "Loop '{name}' has no Task parameters or Tasks of its own; {option} cannot be used."
            )
            .into());
        }
        if args.step.is_some() && args.maximum_tasks.is_some() {
            return Err(format!(
                "Loop '{name}' has no Tasks of its own; --maximum-tasks cannot be used with --step naming a loop. Without --step, --maximum-tasks applies to each body Step instance."
            )
            .into());
        }
    }

    let explicit_task_params = if !args.task_params.is_empty() {
        Some(vec![parse_task_params(&args.task_params)?])
    } else if let Some(tasks_arg) = &args.tasks {
        Some(parse_tasks_arg(tasks_arg)?)
    } else {
        None
    };
    if explicit_task_params.is_some() {
        let Some(Node::Step(selected_idx)) = selected else {
            return Err(
                "Providing task parameters requires a specified step or a job with a single step."
                    .into(),
            );
        };
        let selected = &job.steps[selected_idx];
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
            .top_level_steps()
            .nth(selected_idx)
            .map_or(selected.parameter_space.is_some(), |t| {
                t.parameter_space.is_some()
            });
        if !has_space {
            return Err(no_parameter_space(&selected.name, option).into());
        }
    }
    let to_run = match selected {
        Some(node) if args.run_dependencies => resolve_step_dependencies(job, node),
        Some(node) => vec![node],
        None => graph
            .topo_sorted()?
            .into_iter()
            .map(|i| {
                let n = graph.node(i).expect("a node of the graph");
                match n.kind {
                    openjd_model::step_dependency_graph::StepNodeKind::Step => {
                        Node::Step(n.step_index)
                    }
                    openjd_model::step_dependency_graph::StepNodeKind::Loop => {
                        Node::Loop(n.step_index)
                    }
                }
            })
            .collect(),
    };

    Ok(RunSelection {
        selected,
        explicit_task_params,
        to_run,
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
    let alone = selection.selected.filter(|_| !args.run_dependencies);
    let step_inputs = match (&args.step_inputs, alone) {
        (None, _) => None,
        (Some(arg), Some(_)) => Some(parse_step_inputs(arg)?),
        (Some(_), None) => return Err(
            "--step-inputs requires --step (or a Job with one Step or loop) without --run-dependencies."
                .into(),
        ),
    };
    let mut sim = Simulator::new(
        prepared.job_template.clone(),
        prepared.param_values.clone(),
        prepared.validation_ctx.clone(),
        prepared.job.clone(),
        &selection.to_run,
    )?;
    if let Some(target) = alone {
        sim.give_step_inputs(target, step_inputs.as_ref())?;
    }
    Ok(sim)
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

/// Run the selected Steps, in order, in the run's one Session. Before each
/// Step, move the Session's Job Environments to the ones the Step enters
/// ([`job_environment_targets`]): exit the entered ones, in the reverse of
/// the order the Session entered them, down to the longest prefix the Step
/// also enters with equal values ([`EnteredJobEnvironment::matches`]), then
/// enter the rest of the Step's in order (How Jobs Are Run, Environment Parameters). A Job
/// without `activation: ON_REQUEST`, Environment `enableIf`, disabled
/// `environment:` entries, or Environment parameter values gives every
/// Step the same targets, so the
/// Session enters each Job Environment once, as before `ENV_PARAMS`.
async fn run_workload(
    ctx: &mut RunContext,
    args: &RunArgs,
    prepared: &PreparedRun,
    sim: &mut Simulator,
    selection: &RunSelection,
) -> Result<(), RunError> {
    sim.report_initial();
    // `--step-inputs` values can bind the Step run alone, and a job output
    // can read them.
    if sim.settle()? {
        ctx.session_failed = true;
    }
    for &node in &selection.to_run {
        if ctx.is_stopping() {
            break;
        }
        match node {
            Node::Step(step_idx) => {
                let explicit = selection
                    .explicit_task_params
                    .as_deref()
                    .filter(|_| selection.selected == Some(node));
                match sim.begin_step(step_idx, explicit.is_some())? {
                    Begin::Failed => {
                        ctx.session_failed = true;
                        break;
                    }
                    Begin::NotRun => {}
                    Begin::Run(step) => {
                        let slot = Slot::Top(step_idx);
                        ctx.move_to_job_environments(&job_environment_targets(
                            &sim.job,
                            &prepared.environment_documents,
                            &step,
                        ))
                        .await;
                        if ctx.is_stopping() {
                            sim.finish_step(slot, true, false)?;
                            break;
                        }
                        let ran_all =
                            execute_step(ctx, args, &step, &step.name, slot, explicit, sim).await?;
                        if ctx.session_failed && ctx.interrupted.load(Ordering::SeqCst) {
                            sim.set_reason(slot, "the run was interrupted");
                        }
                        sim.finish_step(slot, ctx.session_failed, ran_all)?;
                    }
                }
            }
            Node::Loop(li) => match sim.begin_loop(li)? {
                LoopBegin::Failed => {
                    ctx.session_failed = true;
                    break;
                }
                LoopBegin::NotRun => {}
                LoopBegin::Run => run_loop(ctx, args, prepared, sim, li).await?,
            },
        }
        if !ctx.is_stopping() && sim.settle()? {
            ctx.session_failed = true;
        }
    }
    sim.block_unreachable(ctx.interrupted.load(Ordering::SeqCst));
    Ok(())
}

/// The Job Environments of the combined Job (the attached ones, then the
/// Job Template's own) that `step` enters ([`Step::enters_environment`],
/// Template Schemas §4 item 4 `activation`, item 6 `enableIf`, and item 7
/// `exclusiveGroup`), in Job
/// order, each with the Step's Environment parameter values for it, if any.
fn job_environment_targets<'a>(
    job: &'a Job,
    environment_documents: &'a [Document],
    step: &'a Step,
) -> Vec<JobEnvironmentTarget<'a>> {
    let step_values = step.environment_parameters.as_deref().unwrap_or(&[]);
    job.job_environments
        .iter()
        .flatten()
        .zip(environment_documents)
        .enumerate()
        .filter(|(_, (env, document))| step.enters_environment(document, &env.name))
        .map(|(index, (env, document))| JobEnvironmentTarget {
            index,
            env,
            // Keyed by (document, name): an attached Environment may share
            // its name with a Job Environment (§3.2.1 item 4).
            values: step_values
                .iter()
                .find(|v| &v.document == document && v.environment == env.name),
            // An Environment Template has not opted in to CONTROL_FLOW, so a
            // directive in its actions is ordinary output.
            origin: if document.is_job_template() {
                EnvironmentOrigin::JobTemplate
            } else {
                EnvironmentOrigin::EnvironmentTemplate
            },
        })
        .collect()
}

/// Run the iterations of loop `li`, one after another (RFC 0013
/// §Iterations, plan L8): start each, run its body Step instances in body
/// dependency order through the same path as a top-level Step, and evaluate
/// its boundary, until the loop succeeds, fails, or is blocked by an
/// incomplete instance.
async fn run_loop(
    ctx: &mut RunContext,
    args: &RunArgs,
    prepared: &PreparedRun,
    sim: &mut Simulator,
    li: usize,
) -> Result<(), RunError> {
    println!(
        "{}\tRunning loop '{}'",
        ctx.timestamp(),
        sim.job.loops[li].name
    );
    let order = sim.body_order(li)?;
    loop {
        if ctx.is_stopping() {
            sim.stop_loop(li, "the run stopped before its iteration completed");
            return Ok(());
        }
        let started = match sim.start_next_iteration(li)? {
            None => sim.settle_iteration()?,
            end => end,
        };
        match started {
            None => {}
            Some(IterationEnd::Failed) => {
                ctx.session_failed = true;
                return Ok(());
            }
            Some(_) => return Ok(()),
        }
        for &j in &order {
            if ctx.is_stopping() || !sim.is_running(li) {
                break;
            }
            let step = match sim.begin_instance(j)? {
                Begin::NotRun => continue,
                Begin::Failed => {
                    ctx.session_failed = true;
                    return Ok(());
                }
                Begin::Run(step) => step,
            };
            let slot = Slot::Body(j);
            let name = sim.instance_name(j);
            // Step input binding has checked the RFC 0008 single-layer rule
            // for the instance (RFC 0011 `<Environment>` item 6).
            ctx.move_to_job_environments(&job_environment_targets(
                &sim.job,
                &prepared.environment_documents,
                &step,
            ))
            .await;
            if ctx.is_stopping() {
                sim.stop_loop(li, "the run stopped before its iteration completed");
                return Ok(());
            }
            let ran_all = execute_step(ctx, args, &step, &name, slot, None, sim).await?;
            if ctx.session_failed && ctx.interrupted.load(Ordering::SeqCst) {
                sim.set_reason(slot, "the run was interrupted");
            }
            sim.finish_step(slot, ctx.session_failed, ran_all)?;
            if ctx.session_failed {
                return Ok(());
            }
            match sim.settle_iteration()? {
                None => {}
                Some(IterationEnd::Failed) => {
                    ctx.session_failed = true;
                    return Ok(());
                }
                Some(_) => return Ok(()),
            }
        }
        if !sim.is_running(li) {
            return Ok(());
        }
        if ctx.is_stopping() {
            sim.stop_loop(li, "the run stopped before its iteration completed");
            return Ok(());
        }
        match sim.end_iteration(li)? {
            IterationEnd::Continue => {}
            IterationEnd::Succeeded | IterationEnd::Blocked => return Ok(()),
            IterationEnd::Failed => {
                ctx.session_failed = true;
                return Ok(());
            }
        }
    }
}

/// Run `step` (`name` in the log: `<Loop>[<N>].<Step>` for a body Step
/// instance): enter its enabled Step Environments, run its Tasks (those of
/// `explicit` when given), and exit them. Returns whether every one of its
/// Tasks ran.
async fn execute_step(
    ctx: &mut RunContext,
    args: &RunArgs,
    step: &Step,
    name: &str,
    slot: Slot,
    explicit: Option<&[HashMap<String, String>]>,
    sim: &mut Simulator,
) -> Result<bool, RunError> {
    println!("{}\tRunning step '{name}'", ctx.timestamp());
    let environment_baseline = ctx.entered_envs.len();
    if let Some(step_envs) = &step.step_environments {
        for env in step_envs {
            if ctx.is_stopping() {
                break;
            }
            // A Step Environment whose enableIf is false is left out of the
            // Step (RFC 0011 `<Environment>` item 4).
            ctx.enter_environment(
                env,
                &env.name,
                step.resolved_symtab.clone(),
                None,
                None,
                EnvironmentOrigin::JobTemplate,
                &step.path_inputs,
            )
            .await;
        }
    }

    let result = if ctx.session_failed {
        Ok(false)
    } else {
        execute_step_tasks(ctx, args, step, name, slot, explicit, sim).await
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
    name: &str,
    slot: Slot,
    explicit: Option<&[HashMap<String, String>]>,
    sim: &mut Simulator,
) -> Result<bool, RunError> {
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
            sim.record_action(slot, None, run.outputs);
        }
        return Ok(true);
    };

    let mut iter = openjd_model::StepParameterSpaceIterator::new(parameter_space)?;
    if let Some(task_param_sets) = explicit {
        return execute_explicit_tasks(
            ctx,
            step,
            slot,
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
            println!("{}\tStep '{name}' has 0 tasks", ctx.timestamp());
        }
        return Ok(true);
    }
    let maximum_tasks = args.maximum_tasks.unwrap_or(-1);
    execute_parameter_space_tasks(ctx, maximum_tasks, step, slot, &mut iter, sim).await
}

/// Run the Tasks given with `--tasks` or `--task-param`, returning whether
/// they ran every Task of the Step: as many distinct Tasks as it has, all
/// run, counting each `CHUNK[INT]` value of a chunk as one Task.
async fn execute_explicit_tasks(
    ctx: &mut RunContext,
    step: &Step,
    slot: Slot,
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
            sim.record_action(slot, Some(values), run.outputs);
        }
    }
    let all_given = ran == typed_sets.len();
    Ok(all_given && Some(distinct.len() as u64) == step.tasks_known())
}

async fn execute_parameter_space_tasks(
    ctx: &mut RunContext,
    maximum_tasks: i64,
    step: &Step,
    slot: Slot,
    iter: &mut openjd_model::StepParameterSpaceIterator,
    sim: &mut Simulator,
) -> Result<bool, RunError> {
    let is_adaptive = iter.chunks_adaptive();
    let chunks_param_name = iter.chunks_parameter_name().map(String::from);
    let target_runtime_seconds = adaptive_target_runtime(step, is_adaptive);
    let mut completed_task_count = 0u64;
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
            sim.record_action(slot, Some(&task_values), run.outputs);
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
            completed_task_count = completed_task_count.saturating_add(chunk_items);
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
    current_chunk_size: u64,
    completed_task_count: u64,
    completed_task_duration: f64,
    target_runtime_seconds: f64,
) -> Option<u64> {
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

    // A float-to-int `as` saturates (NaN becomes 0), so this cannot wrap.
    Some((adaptive_chunk_size as u64).max(1))
}

fn adjust_adaptive_chunk_size(
    ctx: &RunContext,
    iter: &mut openjd_model::StepParameterSpaceIterator,
    completed_task_count: u64,
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

fn report_result(ctx: &mut RunContext, args: &RunArgs, prepared: &PreparedRun, sim: &Simulator) {
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
    ctx.end_session(args.preserve);
    let preserved_msg = ctx
        .preserved_dir
        .as_ref()
        .map(|dir| format!("\nWorking directory preserved at: {}", dir.display()))
        .unwrap_or_default();
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

    #[test]
    fn adaptive_chunk_size_is_kept_beyond_2_pow_32() {
        // 16 tasks in 2^-28 s is 2^-32 s per task, so a 1 s target asks
        // for exactly 2^32 tasks per chunk (powers of two keep the float
        // math exact). A 32-bit usize would truncate that to 0, then 1.
        assert_eq!(
            calculate_adaptive_chunk_size(1, 16, 2f64.powi(-28), 1.0),
            Some(1 << 32)
        );
        // Larger sizes saturate at u64::MAX rather than wrap.
        assert_eq!(
            calculate_adaptive_chunk_size(1, 16, 2f64.powi(-28), 1e30),
            Some(u64::MAX)
        );
    }
}
