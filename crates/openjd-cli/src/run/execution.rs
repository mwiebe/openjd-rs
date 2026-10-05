// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Execution phases for the `openjd run` command.

use super::*;
use openjd_model::template::EnvironmentTemplate;
use openjd_model::AttachedEnvironmentTemplate;
use std::collections::BTreeSet;

pub(super) async fn execute(args: RunArgs) -> Result<(), RunError> {
    if args.verbose {
        log::set_max_level(log::LevelFilter::Debug);
    }

    let started_at = Instant::now();
    let _ = crate::SESSION_START.set(started_at);
    let _ = crate::TIMESTAMP_FORMAT.set(args.timestamp_format.clone());

    let prepared = prepare_run(&args)?;
    let selection = prepare_selection(&args, &prepared.job)?;
    validate_wrap_environment_stacks(&prepared.job, &selection.steps_to_run)?;
    let base_symtab = openjd_model::build_symbol_table(&prepared.param_values)
        .map_err(|e| format!("Failed to build the job parameter symbol table: {e}"))?;

    let cancel_token = CancellationToken::new();
    let session = create_session(&args, &prepared, cancel_token.clone())?;
    let services = ServiceManager::new(services::ServiceRunConfig {
        job_parameter_values: prepared.param_values.clone(),
        path_mapping_rules: (!prepared.path_rules.is_empty()).then(|| prepared.path_rules.clone()),
        retain_working_dir: args.preserve,
        profile: prepared.revision_profile.clone(),
        attached_profiles: prepared.attached_profiles.clone(),
        requirement_bindings: prepared.requirement_bindings.clone(),
        cancel_token: cancel_token.clone(),
        limits: openjd_sessions::SessionLimits::from(&crate::common::caller_limits()),
    });
    let interrupted = install_signal_handler(cancel_token.clone());
    let mut ctx = RunContext {
        session,
        entered_envs: Vec::new(),
        interrupted,
        started_at,
        tasks_run: 0,
        session_failed: false,
        services,
        failed_services: Vec::new(),
        step_env_baseline: 0,
        base_symtab: SerializedSymbolTable::from_symtab(&base_symtab),
        cancel_token,
        preserved_working_dirs: Vec::new(),
    };

    println!("{}\tSession start", ctx.timestamp());
    println!("{}\tRunning job '{}'", ctx.timestamp(), prepared.job.name);
    let execution_result = run_workload(&mut ctx, &args, &prepared, &selection).await;

    // Once a Session exists, errors must not bypass environment cleanup —
    // nor Service teardown (RFC 0009 constraint 6: a Service whose scope
    // completes has its Session ended whatever its state), in reference
    // order (constraint 4), after the Task Session has left their
    // Environments.
    ctx.exit_environments_down_to(0).await;
    ctx.services.stop_all().await;
    ctx.record_interruption();
    if let Err(e) = execution_result {
        if !args.preserve {
            ctx.session.cleanup();
        }
        return Err(e);
    }

    report_result(&mut ctx, &args, &prepared.job);
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

    // The second stage of the submission (RFC 0009, Template Schemas
    // §1.2.2): the --environment templates' Services become external
    // Services placed before the Job Template's services (each stamped
    // with its document — an external `Cache` and the Job Template's
    // `Cache` are two Services), their Environments are placed before its
    // jobEnvironments, each requiresServices entry is matched to exactly
    // one attached Service, and the submission-time wrapper check runs
    // against the combined Job. Each template is labeled with its path in
    // error messages and in the run log.
    let labels: Vec<String> = args
        .environments
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    let attached: Vec<AttachedEnvironmentTemplate<'_>> = env_templates
        .iter()
        .zip(&labels)
        .map(|(template, label)| AttachedEnvironmentTemplate::new(template).with_label(label))
        .collect();
    let applied = openjd_model::apply_environment_templates(
        &job,
        &attached,
        &param_values,
        &crate::common::caller_limits(),
    )
    .map_err(|e| format!("{e}\n\n{}", crate::help::format_help(&job_template, path)))?;
    let environment_documents = applied.combined_environment_documents(&job);
    let requirement_bindings = applied.requirement_bindings.clone();
    let job = applied.into_combined_job(job);
    // Each attached template keeps its own profile: its Environment and
    // Services are evaluated at run time under the extensions *it* declares
    // (RFC 0009 "Environment Template": a Job Template need not list the
    // extensions the Environment Templates applied to it use, and vice
    // versa).
    let attached_profiles = env_templates
        .iter()
        .map(EnvironmentTemplate::profile)
        .collect();

    Ok(PreparedRun {
        job,
        environment_documents,
        requirement_bindings,
        param_values,
        path_rules,
        revision_profile,
        attached_profiles,
    })
}

fn prepare_selection(args: &RunArgs, job: &Job) -> Result<RunSelection, RunError> {
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
        if job.steps[selected_idx].parameter_space.is_none() {
            let option = if args.task_params.is_empty() {
                "--tasks"
            } else {
                "--task-param"
            };
            return Err(format!(
                "Step '{}' does not define a parameterSpace; {option} cannot be used.",
                job.steps[selected_idx].name
            )
            .into());
        }
    }
    // Order the Steps so that a Step in the scope of a Service with
    // `dependencies` runs after the Steps the Service depends on (RFC 0009
    // §9 item 4), in addition to its own dependencies.
    let ordered = with_implied_step_dependencies(job);
    let steps_to_run = if let Some(selected_idx) = selected_step_idx {
        if args.run_dependencies {
            resolve_step_dependencies(&ordered, selected_idx)
        } else {
            vec![selected_idx]
        }
    } else {
        StepDependencyGraph::new(&ordered)
            .and_then(|g| g.topo_sorted())
            .map_err(|e| {
                format!("{e}\n(Step dependencies include those implied by Services' dependencies)")
            })?
    };

    Ok(RunSelection {
        selected_step_idx,
        explicit_task_params,
        steps_to_run,
    })
}

/// RFC 0008's single-wrap-layer rule over every Session stack this run
/// builds. The combined Job's `jobEnvironments` already hold the
/// `--environment` templates' Environments (in attachment order) ahead of
/// the Job Template's own. A Service Session enters the same stacks (minus
/// Environments whose `runScope` excludes `SERVICE`), so the check covers
/// them too. The model enforces the rule per document at template
/// validation; this covers the combined Job, where an `--environment`
/// template's wrapper meets the Job Template's. That a `SERVICE`-scoped
/// wrapper defines all four `onWrapService*` hooks (§9.7 item 6) is enforced
/// by the model — per document at template validation, and across documents
/// by `apply_environment_templates` — and is not repeated here.
fn validate_wrap_environment_stacks(job: &Job, steps_to_run: &[usize]) -> Result<(), RunError> {
    let has_wrap_hook = |env: &Environment| {
        env.script
            .as_ref()
            .is_some_and(|script| script.actions.has_any_wrap_hook())
    };
    let base_wrap_envs: Vec<&str> = job
        .job_environments
        .iter()
        .flatten()
        .filter(|env| has_wrap_hook(env))
        .map(|env| env.name.as_str())
        .collect();
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
        log_tag: None,
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

/// Whether `step` will run at least one Task under this selection — RFC
/// 0009 constraint 10 lets the runner decline to start a Service whose scope
/// schedules no Task (a `--tasks '[]'` selection, say).
fn step_runs_tasks(step_idx: usize, selection: &RunSelection) -> bool {
    if selection.selected_step_idx == Some(step_idx) {
        if let Some(explicit) = &selection.explicit_task_params {
            return !explicit.is_empty();
        }
    }
    // A parameter space always yields at least one Task, and a Step
    // without one runs exactly one; `--maximum-tasks 0` means "no limit".
    true
}

/// How a Step's attempt ended.
enum StepOutcome {
    /// The Step's Tasks are done (or the run is stopping).
    Done,
    /// A Service with `completedTasks: RERUN` failed: the completed Tasks
    /// of every Step in its scope return to the queue.
    Rerun(ServiceScope),
}

/// How a Step's Task iteration ended.
enum TasksOutcome {
    Done,
    Rerun(ServiceScope),
}

/// The Steps a `RERUN` returns to pending (RFC 0009 "`RERUN` and Step
/// dependencies"): every selected Step in `scope`, and every selected Step
/// that depends on one of them, directly or transitively.
fn returned_steps(job: &Job, scope: &ServiceScope, selected: &[usize]) -> BTreeSet<String> {
    let mut returned: BTreeSet<String> = selected
        .iter()
        .map(|&i| &job.steps[i].name)
        .filter(|name| scope.contains(name))
        .cloned()
        .collect();
    loop {
        let before = returned.len();
        for &i in selected {
            let step = &job.steps[i];
            if step
                .dependencies
                .iter()
                .flatten()
                .any(|d| returned.contains(&d.depends_on))
            {
                returned.insert(step.name.clone());
            }
        }
        if returned.len() == before {
            return returned;
        }
    }
}

/// `'A'`, or `'A', 'B'`, for log lines.
fn quoted_list<'a>(names: impl Iterator<Item = &'a String>) -> String {
    names
        .map(|n| format!("'{n}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

async fn run_workload(
    ctx: &mut RunContext,
    args: &RunArgs,
    prepared: &PreparedRun,
    selection: &RunSelection,
) -> Result<(), RunError> {
    let job = &prepared.job;
    let steps_to_run = &selection.steps_to_run;
    let selected: BTreeSet<String> = steps_to_run
        .iter()
        .map(|&i| job.steps[i].name.clone())
        .collect();
    // Every Service of the combined Job is registered; those whose scope is
    // every Step are activated now (RFC 0009 constraint 10: unless the
    // selection schedules no Task), the rest when the first Step of their
    // scope is about to run.
    ctx.services.register(job, &prepared.environment_documents);
    let job_runs_tasks = steps_to_run
        .iter()
        .any(|&idx| step_runs_tasks(idx, selection));
    let job_wide_count = ctx.services.job_wide_count();
    if job_runs_tasks {
        ctx.services.activate_job_wide();
    } else if job_wide_count > 0 {
        println!(
            "{}\tNot starting the {job_wide_count} Service(s) whose scope is every Step: no Task \
             of this Job will run",
            ctx.timestamp()
        );
    }

    // The Steps whose Tasks have completed in this run; a RERUN removes the
    // Steps it returns to the queue.
    let mut completed: BTreeSet<String> = BTreeSet::new();
    loop {
        // Constraints 2–3: Job-wide Services are READY before any Task;
        // here, before the Task Session even enters the Job's Environments
        // (a TASK-scoped one may reference their endpoints).
        if !ctx.is_stopping() {
            ctx.gate_services().await?;
        }
        for (env, document) in job
            .job_environments
            .iter()
            .flatten()
            .zip(&prepared.environment_documents)
        {
            if ctx.is_stopping() {
                break;
            }
            // An attached Environment is evaluated under its own template's
            // profile; the Job Template's own under the Session's.
            let profile = (!document.is_job_template()).then(|| prepared.profile_for(document));
            ctx.enter_environment(env, None, document.clone(), profile, None)
                .await;
        }

        let mut rerun: Option<ServiceScope> = None;
        for (pos, &step_idx) in steps_to_run.iter().enumerate() {
            let step = &job.steps[step_idx];
            if completed.contains(&step.name) || ctx.is_stopping() {
                continue;
            }
            // The Steps still to run after this one: a Service whose scope
            // contains none of them is stopped when this Step completes.
            let remaining: BTreeSet<String> = steps_to_run[pos + 1..]
                .iter()
                .map(|&i| job.steps[i].name.clone())
                .filter(|name| !completed.contains(name))
                .collect();
            match execute_step(
                ctx, args, step, step_idx, selection, &completed, &selected, &remaining,
            )
            .await?
            {
                StepOutcome::Done => {
                    completed.insert(step.name.clone());
                }
                StepOutcome::Rerun(scope) => {
                    rerun = Some(scope);
                    break;
                }
            }
        }
        ctx.exit_environments_down_to(0).await;
        let Some(scope) = rerun.filter(|_| !ctx.is_stopping()) else {
            return Ok(());
        };
        // The requeue happens only if the Service is actually relaunched:
        // await the restart decision (and the relaunch) first. A Service
        // whose attempts are exhausted is FAILED instead, the gate records
        // the failure, and the Job fails without pretending to requeue
        // anything.
        ctx.gate_services().await?;
        if ctx.is_stopping() {
            return Ok(());
        }
        // "RERUN and Step dependencies": every Step in the scope returns to
        // pending, and with it every Step that depends on one of them;
        // dependencies are resolved again from scratch. A Service that was
        // stopped because its scope completed, and whose scope includes a
        // returned Step, starts again in a new Service Session when that
        // Step is next activated.
        let returned = returned_steps(job, &scope, steps_to_run);
        let in_scope: Vec<&String> = returned.iter().filter(|n| scope.contains(n)).collect();
        let dependents: Vec<&String> = returned.iter().filter(|n| !scope.contains(n)).collect();
        let mut msg = format!(
            "Returning every completed Task of Step(s) {} to the queue: a Service with \
             completedTasks: RERUN {} was relaunched",
            quoted_list(in_scope.into_iter()),
            services::scope_label(&scope)
        );
        if !dependents.is_empty() {
            msg.push_str(&format!(
                "; dependent Step(s) {} return to pending",
                quoted_list(dependents.into_iter())
            ));
        }
        println!("{}\t{msg}", ctx.timestamp());
        for name in &returned {
            completed.remove(name);
        }
        // The requeued Tasks run in a new Task Session: a canceled action
        // leaves a Session ending-only (see specs/sessions/session.md
        // "Brittle Sessions"), and a scheduler would form new Sessions for
        // requeued Tasks anyway. The new Session re-enters the Job's
        // Environments, so a TASK-scoped one re-resolves the Service's
        // (possibly new) endpoints.
        let replacement = create_session(args, prepared, ctx.cancel_token.clone())?;
        ctx.replace_task_session(replacement, args.preserve);
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_step(
    ctx: &mut RunContext,
    args: &RunArgs,
    step: &Step,
    step_idx: usize,
    selection: &RunSelection,
    completed: &BTreeSet<String>,
    selected: &BTreeSet<String>,
    remaining: &BTreeSet<String>,
) -> Result<StepOutcome, RunError> {
    println!("{}\tRunning step '{}'", ctx.timestamp(), step.name);
    let runs_tasks = step_runs_tasks(step_idx, selection);
    // A Service scoped to this Step (among others) is UNREADY from the time
    // the Step is about to run and its own `dependencies` have completed
    // (constraint 3 gates the Step's Tasks on it). It is activated here and
    // started by the gate, before the Task Session enters the Step's
    // Environments. A Step re-running its Tasks after a RERUN finds its
    // Services still active, or starts them again if their scope had
    // completed.
    if runs_tasks && !ctx.is_stopping() {
        ctx.services
            .activate_for_step(&step.name, completed, selected)?;
        ctx.gate_services().await?;
    } else if !runs_tasks {
        let inactive = ctx.services.inactive_for_step(&step.name);
        if !inactive.is_empty() {
            println!(
                "{}\tNot starting {} for Step '{}': no Task of this Step will run",
                ctx.timestamp(),
                inactive.join(", "),
                step.name
            );
        }
    }
    let environment_baseline = ctx.entered_envs.len();
    ctx.step_env_baseline = environment_baseline;
    for env in step.step_environments.iter().flatten() {
        if ctx.is_stopping() {
            break;
        }
        ctx.enter_environment(
            env,
            step.resolved_symtab.clone(),
            Document::JobTemplate,
            None,
            Some(&step.name),
        )
        .await;
    }

    let result = if ctx.session_failed {
        Ok(TasksOutcome::Done)
    } else {
        execute_step_tasks(ctx, args, step, step_idx, selection).await
    };
    // This unwind happens before propagating task setup/runtime errors.
    ctx.exit_environments_down_to(environment_baseline).await;
    match result? {
        TasksOutcome::Rerun(scope) if !ctx.is_stopping() => {
            // The Services stay registered (the failed one is relaunching);
            // the returned Steps run again from the start.
            Ok(StepOutcome::Rerun(scope))
        }
        TasksOutcome::Done | TasksOutcome::Rerun(_) => {
            // Constraint 6: a Service whose scope has no Step left to run
            // is stopped.
            ctx.services.stop_completed_scopes(remaining).await;
            Ok(StepOutcome::Done)
        }
    }
}

async fn execute_step_tasks(
    ctx: &mut RunContext,
    args: &RunArgs,
    step: &Step,
    step_idx: usize,
    selection: &RunSelection,
) -> Result<TasksOutcome, RunError> {
    let Some(parameter_space) = &step.parameter_space else {
        return Ok(match ctx.run_task(step, None, &[]).await? {
            TaskRun::Rerun(scope) => TasksOutcome::Rerun(scope),
            TaskRun::Completed(_) | TaskRun::Aborted => TasksOutcome::Done,
        });
    };

    let mut iter = openjd_model::StepParameterSpaceIterator::new(parameter_space)?;
    if selection.selected_step_idx == Some(step_idx) {
        if let Some(task_param_sets) = selection.explicit_task_params.as_deref() {
            return execute_explicit_tasks(ctx, step, parameter_space, task_param_sets, &iter)
                .await;
        }
    }
    execute_parameter_space_tasks(ctx, args.maximum_tasks, step, &mut iter).await
}

async fn execute_explicit_tasks(
    ctx: &mut RunContext,
    step: &Step,
    parameter_space: &openjd_model::job::StepParameterSpace,
    task_param_sets: &[HashMap<String, String>],
    iter: &openjd_model::StepParameterSpaceIterator,
) -> Result<TasksOutcome, RunError> {
    let mut typed_sets = Vec::with_capacity(task_param_sets.len());
    for (index, params) in task_param_sets.iter().enumerate() {
        let values = coerce_task_params(params, parameter_space)?;
        iter.validate_containment(&values)
            .map_err(|e| format!("Task parameter set {index}: {e}"))?;
        typed_sets.push(values);
    }

    for (params, values) in task_param_sets.iter().zip(&typed_sets) {
        if ctx.is_stopping() {
            break;
        }
        let lines: Vec<String> = params
            .iter()
            .map(|(name, value)| format!("{name} = {value}"))
            .collect();
        match ctx.run_task(step, Some(values), &lines).await? {
            TaskRun::Completed(_) => {}
            TaskRun::Rerun(scope) => return Ok(TasksOutcome::Rerun(scope)),
            TaskRun::Aborted => break,
        }
    }
    Ok(TasksOutcome::Done)
}

async fn execute_parameter_space_tasks(
    ctx: &mut RunContext,
    maximum_tasks: i64,
    step: &Step,
    iter: &mut openjd_model::StepParameterSpaceIterator,
) -> Result<TasksOutcome, RunError> {
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

    while remaining_tasks > 0 && !ctx.is_stopping() {
        let Some(task_params) = iter.next() else {
            break;
        };
        let lines: Vec<String> = task_params
            .iter()
            .map(|(name, value)| {
                format!(
                    "{}({}) = {}",
                    name,
                    value.param_type.as_spec_str(),
                    value.value.to_display_string()
                )
            })
            .collect();
        let task_values: TaskParameterSet = task_params
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let task_duration = match ctx.run_task(step, Some(&task_values), &lines).await? {
            TaskRun::Completed(duration) => duration,
            TaskRun::Rerun(scope) => return Ok(TasksOutcome::Rerun(scope)),
            TaskRun::Aborted => break,
        };
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
    Ok(TasksOutcome::Done)
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

fn report_result(ctx: &mut RunContext, args: &RunArgs, job: &Job) {
    let working_dir = ctx.session.working_directory().to_path_buf();
    println!("{}\t", ctx.timestamp());
    for failure in &ctx.failed_services {
        println!("{}\t{failure}", ctx.timestamp());
    }
    if ctx.session_failed {
        println!("{}\tSession ended with errors.", ctx.timestamp());
    } else {
        println!("{}\tAll actions completed successfully!", ctx.timestamp());
    }
    println!("{}\tLocal session ended.", ctx.timestamp());

    let duration = ctx.started_at.elapsed().as_secs_f64();
    let preserved_msg = if args.preserve {
        ctx.preserved_working_dirs.push(working_dir);
        ctx.preserved_working_dirs
            .iter()
            .map(|dir| format!("\nWorking directory preserved at: {}", dir.display()))
            .collect::<String>()
    } else {
        ctx.session.cleanup();
        String::new()
    };
    let (status, message) = if let Some(failure) = ctx.failed_services.first() {
        ("error", format!("{failure}{preserved_msg}"))
    } else if ctx.session_failed {
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
        failed_services: ctx
            .failed_services
            .iter()
            .map(|f| result::FailedService {
                name: f.name.clone(),
                document: (!f.document.is_job_template()).then(|| f.document.to_string()),
                scope: f.scope.to_string(),
                reason: f.reason.clone(),
            })
            .collect(),
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
