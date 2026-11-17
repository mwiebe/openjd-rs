// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Step and environment instantiation — converting template types to job types.

use openjd_expr::format_string::copy_symbol_value;
use openjd_expr::path_mapping::PathFormat;
use openjd_expr::symbol_table::SymbolTable;

use crate::error::{
    path_field, path_index, path_to_string, ModelError, PathElement, ValidationErrors,
};
use crate::job;
use crate::template;
use crate::template::validate_v2023_09::EffectiveLimits;
use openjd_expr::ExpressionError;

use super::ranges;

/// The job-wide inputs every instantiation step shares: the context's
/// extensions and limits, the caller budgets, and the template's Services
/// and requirements (whose `Service.*` symbols are in scope for the Steps,
/// Services, and Job Environments that list them).
#[derive(Clone, Copy)]
pub(super) struct InstantiateCtx<'a> {
    pub(super) has_expr: bool,
    /// Whether the template itself enables EXPR, which decides how a numeric
    /// range list element resolves
    /// (`TaskParameterType::range_list_item_target`).
    pub(super) template_expr: bool,
    pub(super) limits: &'a EffectiveLimits,
    pub(super) ctx: &'a crate::types::ValidationContext,
    pub(super) budgets: super::EvalBudgets,
    /// The document's `services`, in declaration order (empty when none).
    pub(super) services: &'a [template::Service],
    /// A Job Template's `externalServices` (empty for an Environment
    /// Template, or when none).
    pub(super) requirements: &'a [template::ServiceRequirement],
}

/// What [`instantiate_service`] needs to resolve a Job Template Service's
/// `environment:` entries (RFC 0009 `<StepDependency>` item 3): the
/// Service's index in the Job Template's `services`, the number of Steps
/// (its errors are filed after theirs, [`super::env_params::Who::index`]),
/// and the Job Environments. An Environment Template's Service has none.
#[derive(Clone, Copy)]
pub(super) struct ServiceRequester<'r, 'a> {
    pub(super) index: usize,
    pub(super) step_count: usize,
    pub(super) job_envs: &'r super::env_params::JobEnvironments<'a>,
}

/// Instantiate a StepTemplate into a Step.
///
/// The step's carried-forward resolved-value checks — and its `let`
/// bindings, step-level and script/environment-level — report into
/// `check_errors`, which `create_job` threads through every step and job
/// environment and converts into one `ModelValidation` error at the end
/// (whole-template aggregation, as pass 8 reports). Resolution failures
/// that leave the step uninstantiable — host requirements, the
/// parameter space — are returned as `Err` and abort job creation
/// immediately.
///
/// Returns the Step with what job creation decided about its Environments
/// for the submission stage. `job::Step::environments` is left `None`
/// until the submission stage fixes it.
///
/// Returns `Ok(None)` when a step-level `let` binding failed: the
/// failed names are bound `Unresolved` so the step's carried-forward
/// checks still run (as at pass 8), but host requirements and the
/// parameter space need the concrete values, so they are skipped. The
/// failure is already recorded in `check_errors`, so job creation fails.
#[allow(clippy::too_many_arguments)]
pub(super) fn instantiate_step<'a>(
    st: &template::StepTemplate,
    symtab: &SymbolTable,
    icx: InstantiateCtx<'_>,
    step_index: usize,
    job_envs: &super::env_params::JobEnvironments<'a>,
    lists_env_params: bool,
    check_errors: &mut ValidationErrors,
    staged: &mut super::staged::StagedErrors,
) -> Result<Option<(job::Step, super::env_params::StepSelection<'a>)>, ModelError> {
    let InstantiateCtx {
        has_expr,
        template_expr,
        limits,
        ctx,
        budgets,
        services,
        requirements,
    } = icx;
    let mut step_symtab = symtab.clone();

    let step_name = st.name.clone();
    let step_path = [
        PathElement::Field("steps".to_string()),
        PathElement::Index(step_index),
    ];

    if has_expr {
        step_symtab.set(
            "Step.Name",
            openjd_expr::ExprValue::String(step_name.clone()),
        )?;
    }

    // Evaluate step-level let bindings (TEMPLATE scope — no PATH Param.*,
    // no host context), reported at `steps[i] -> let[j]` like pass 8.
    let mut step_let_failed = false;
    if has_expr {
        if let Some(bindings) = &st.let_bindings {
            let before = check_errors.len();
            crate::template::validate_v2023_09::format_strings::check_carried_forward_let_bindings(
                bindings,
                &path_field(&step_path, "let"),
                &mut step_symtab,
                ctx,
                openjd_expr::HostContext::None,
                check_errors,
            );
            step_let_failed = check_errors.len() > before;
        }
    }

    let script_template = st.resolve_syntax_sugar();
    let script = script_template.as_ref().map(convert_step_script);
    // When the script came from a SimpleAction (§8), diagnostics on the
    // desugared form must point at the field the author wrote.
    let sugar_kind = if st.script.is_none() {
        st.simple_actions().next().map(|(kind, _)| kind)
    } else {
        None
    };

    // The Services whose `Service.*` endpoints this Step's Task Sessions
    // may reference (RFC 0009 §9 scope rule 3, §9.8 item 2): the inline and
    // required Services the Step lists with the `service` key in its
    // `dependencies`.
    let in_scope_services =
        || crate::template::listed_services(st.dependencies.as_deref(), services);
    let listed_requirements =
        || crate::template::listed_requirements(st.dependencies.as_deref(), requirements);

    // Re-run the carried-forward format-string resolved-value checks —
    // the script's `let` bindings, the `onRun` action's
    // `command`/`args`/`timeout`/`cancelation`, and each embedded file's
    // `data` — against check symbol tables where job parameters are
    // bound to real values. A violation template validation could only
    // lower-bound is decidable here: fail at submission, not on every
    // worker. These are exactly the checks pass 8 applies — to the same
    // desugared form for a SimpleAction step, with the same path remap
    // back onto the authored field.
    if let Some(s) = &script_template {
        // A SimpleAction's checks run into a scratch collection rooted at
        // the desugared `script` node and are re-rooted afterwards.
        let mut scratch = ValidationErrors::default();
        let (script_path, target): (Vec<PathElement>, &mut ValidationErrors) = match sugar_kind {
            Some(_) => (Vec::new(), &mut scratch),
            None => (path_field(&step_path, "script"), &mut *check_errors),
        };
        // Check symbol table for the step script's carried-forward
        // (session/task-scope) format strings: `step_symtab`'s concrete
        // values plus `Unresolved` placeholders for everything only a
        // session can bind, with the script-level `let` bindings
        // evaluated in (reported at `<script> -> let[j]`).
        let cst = build_task_check_symtab(
            st,
            s,
            &step_symtab,
            has_expr,
            ctx,
            &script_path,
            target,
            in_scope_services(),
            listed_requirements(),
        )?;
        crate::template::validate_v2023_09::format_strings::check_carried_forward_step_script(
            s,
            &cst,
            &step_symtab,
            ctx,
            &script_path,
            target,
        );
        if let Some(kind) = sugar_kind {
            let sa_path = path_field(&step_path, kind.field_name());
            check_errors.extend_remapped(scratch, |rel| {
                let mut p = sa_path.clone();
                p.extend(kind.remap_desugared_path(rel));
                p
            });
        }
    }

    // The same checks for this step's environments (session scope):
    // `let` bindings, `variables` values, every action's
    // `command`/`args`/`timeout`/`cancelation`, embedded-file `data`. A
    // Step Environment sees the `Service.*` endpoints its Step lists (RFC
    // 0009: a Step Environment follows its Step's dependencies and is
    // entered only by Task Sessions).
    if let Some(envs) = &st.step_environments {
        for (j, env) in envs.iter().enumerate() {
            let env_path = path_index(&path_field(&step_path, "stepEnvironments"), j);
            let env_symtab = build_env_check_symtab(
                env,
                &step_symtab,
                has_expr,
                ctx,
                &env_path,
                check_errors,
                in_scope_services(),
                listed_requirements(),
            )?;
            crate::template::validate_v2023_09::format_strings::check_carried_forward_environment(
                env,
                &env_symtab,
                &step_symtab,
                ctx,
                limits.max_env_var_value_len,
                &env_path,
                check_errors,
            );
        }
    }

    // Host requirements and the parameter space are resolved for real
    // and need the step-level `let` values; with one of those failed
    // (already reported) the step cannot be instantiated.
    if step_let_failed {
        return Ok(None);
    }

    // With ENV_PARAMS, requirements that resolve to nothing are discarded,
    // and a Step left with none has none (§3.3).
    let (host_requirements, discarded_host_requirements) = match &st.host_requirements {
        None => (None, Vec::new()),
        Some(hr) => {
            let resolved = super::host_requirements::resolve_host_requirements(
                hr,
                &step_symtab,
                ctx,
                &super::host_requirements::Site::step(step_index, &st.name),
                budgets,
                lists_env_params,
            )?;
            let discarded = resolved
                .discarded
                .into_iter()
                .map(|(kind, name)| super::DiscardedHostRequirement {
                    environment: None,
                    kind: kind.into(),
                    name,
                })
                .collect();
            (resolved.requirements, discarded)
        }
    };

    let parameter_space = st
        .parameter_space
        .as_ref()
        .map(|ps| ranges::resolve_parameter_space(ps, &step_symtab, template_expr, limits, budgets))
        .transpose()?;

    // Validate the resolved parameter space (e.g. association length mismatches)
    if let Some(ref ps) = parameter_space {
        let _ = crate::job::step_param_space::StepParameterSpaceIterator::new(ps)?;
    }

    // Step entries (`dependsOn` and its ENV_PARAMS synonym `step`, §3.2
    // item 2, each written `dependsOn`) and SERVICE `service` entries become
    // job dependencies. An `environment:` entry (ENV_PARAMS) does not delay
    // scheduling (§3.2 item 3), so it has no place in the dependency graph;
    // a list left empty by the filter becomes `None`, as for a Step with no
    // dependencies.
    let dependencies = st
        .dependencies
        .as_ref()
        .map(|deps| {
            deps.iter()
                .filter(|d| d.step().is_some() || d.service().is_some())
                .map(job::StepDependency::from)
                .collect::<Vec<_>>()
        })
        .filter(|deps| !deps.is_empty());

    // ENV_PARAMS: `enableIf`, the Job Environments the Step's Sessions
    // enter and its effective values for them, and the entries for
    // external Environments frozen for submission (§3.2.2, §7.4.1).
    let entries = super::env_params::resolve_step_entries(
        &super::env_params::Requester::step(st, step_index),
        &step_symtab,
        job_envs,
        super::env_params::ResolvePolicy::new(ctx),
        staged,
    );

    // §4 item 6: a Step Environment whose `enableIf` is `false` is not
    // entered by the Step's Sessions, so it is left out of the Job; with
    // none left the Step has none (plan 3 Q7).
    let step_environments: Option<Vec<job::Environment>> = st
        .step_environments
        .as_ref()
        .map(|envs| {
            envs.iter()
                .zip(&entries.step_environments_enabled)
                .filter(|(_, enabled)| **enabled)
                .map(|(env, _)| convert_environment(env))
                .collect::<Vec<_>>()
        })
        .filter(|envs| !envs.is_empty() || entries.step_environments_enabled.iter().all(|e| *e));

    let script = script.ok_or_else(|| {
        ModelError::DecodeValidation("Step must have a script or SimpleAction".to_string())
    })?;
    let filtered_symtab = filter_symtab_for_step(
        &step_symtab,
        Some(&script),
        &step_environments,
        st.let_bindings.as_deref(),
    );

    Ok(Some((
        job::Step {
            name: step_name,
            description: st.description.as_ref().map(|d| d.0.clone()),
            script,
            step_environments,
            parameter_space,
            host_requirements,
            dependencies,
            resolved_symtab: Some(openjd_expr::SerializedSymbolTable::from_symtab(
                &filtered_symtab,
            )),
            // Set by the submission stage, which decides the Environments.
            environment_parameters: None,
            environments: None,
        },
        super::env_params::StepSelection {
            discarded_host_requirements,
            ..entries.selection
        },
    )))
}

// ── RFC 0009 Services ─────────────────────────────────────────────────

/// Instantiate one `<Service>` (Template Schemas §9) at `path`
/// (`services[k]` of the Job Template or of an attached Environment
/// Template).
///
/// `base` is the job-creation symbol table (`Param.*`, `RawParam.*`,
/// `Job.Name`). `in_scope` is every Service whose `Service.<name>.<port>.port`
/// / `.connectAddress` the Service's host-resolved fields may reference: the
/// Services of its own document it lists in its `dependencies` (§9 scope
/// rule 2); the Service itself is seeded separately, with `bindAddress`, and
/// the `requirements` of a Job Template it lists are seeded as well
/// (`listed_requirements`, §9.8 item 2). `scope` is the
/// §9.1 result for the Service (for an external Service, over the combined
/// Job).
///
/// Job-creation-stage fields resolve here: `<Service>.let` (template scope,
/// like `<StepTemplate>.let`), the numeric `@fmtstring` fields with their
/// §9 ranges and defaults, and `hostRequirements`. `variables` and `script`
/// are `@fmtstring[host]`: they are carried forward unresolved and re-checked
/// against a check table where `Param.*` have real values, like an
/// Environment's.
///
/// Like [`instantiate_step`], the Service's checks report into
/// `check_errors` — the collection `create_job` (or
/// `apply_environment_templates`) threads through the whole document and
/// converts into one `ModelValidation` error — in pass 8's order: `let`,
/// `hostRequirements`, the numeric fields, then the script's `let`,
/// `variables`, actions, embedded files. A numeric field that resolves out
/// of range, a `maxRestarts` without `completedTasks`, and a failed `let`
/// binding are recorded there, and the Service is returned as `Ok(None)`
/// (the remaining checks still run, with the failed names bound
/// `Unresolved`); only a resolution failure — `hostRequirements`, a numeric
/// field that does not resolve to `int?` — is returned as `Err` and aborts
/// job creation alone.
///
/// Returns the Service; for a Job Template's Service (`requester`), what
/// job creation decided about its Environments, as for a Step; and its own
/// host requirements that resolved to nothing and were discarded (§3.3), in
/// written order with amounts first.
#[allow(clippy::too_many_arguments)]
pub(super) fn instantiate_service<'s, 'a>(
    svc: &template::Service,
    base: &SymbolTable,
    icx: InstantiateCtx<'_>,
    path: &[PathElement],
    in_scope: impl Iterator<Item = &'s template::Service>,
    scope: job::ServiceScope,
    requester: Option<ServiceRequester<'_, 'a>>,
    check_errors: &mut ValidationErrors,
    staged: &mut super::staged::StagedErrors,
) -> Result<Option<InstantiatedService<'a>>, ModelError> {
    let InstantiateCtx {
        has_expr,
        limits,
        ctx,
        budgets,
        requirements,
        ..
    } = icx;
    let before = check_errors.len();
    let mut service_symtab = base.clone();
    if has_expr {
        if let Some(bindings) = &svc.let_bindings {
            crate::template::validate_v2023_09::format_strings::check_carried_forward_let_bindings(
                bindings,
                &path_field(path, "let"),
                &mut service_symtab,
                ctx,
                openjd_expr::HostContext::None,
                check_errors,
            );
        }
    }
    let let_failed = check_errors.len() > before;

    // The carried-forward (host-resolved) fields are re-checked with the
    // job parameters bound whatever happened above — `variables`, every
    // action (`command`/`args`, and `timeout`/`cancelation` in the
    // `<Service>.let` scope), embedded-file `data`, after evaluating
    // `<ServiceScript>.let` into the check table.
    let check_fields = |service_symtab: &SymbolTable, check_errors: &mut ValidationErrors| {
        let check_symtab = build_service_check_symtab(
            svc,
            service_symtab,
            has_expr,
            ctx,
            path,
            in_scope,
            crate::template::listed_requirements(svc.dependencies.as_deref(), requirements),
            check_errors,
        )?;
        crate::template::validate_v2023_09::format_strings::check_carried_forward_service(
            svc,
            &check_symtab,
            service_symtab,
            ctx,
            limits.max_env_var_value_len,
            path,
            check_errors,
        );
        Ok::<(), ModelError>(())
    };

    // The job-creation-stage fields need the concrete `let` values.
    if let_failed {
        check_fields(&service_symtab, check_errors)?;
        return Ok(None);
    }

    // With ENV_PARAMS, which SERVICE requires, requirements that resolve to
    // nothing are discarded, and a Service left with none has none (§3.3),
    // as for a Step.
    let (host_requirements, discarded_host_requirements) = match &svc.host_requirements {
        None => (None, Vec::new()),
        Some(hr) => {
            let resolved = super::host_requirements::resolve_host_requirements(
                hr,
                &service_symtab,
                ctx,
                &super::host_requirements::Site::owner(path, &format!("Service '{}'", svc.name)),
                budgets,
                ctx.profile
                    .has_extension(crate::types::ModelExtension::EnvParams),
            )?;
            let discarded = resolved
                .discarded
                .into_iter()
                .map(|(kind, name)| super::DiscardedHostRequirement {
                    environment: None,
                    kind: kind.into(),
                    name,
                })
                .collect::<Vec<_>>();
            (resolved.requirements, discarded)
        }
    };

    // §9.5.1 numeric `@fmtstring` fields: target type `int?`,
    // a `null` resolution meaning "not provided" (the default applies), and
    // a non-null value satisfying the field's range — a violation is
    // recorded in `check_errors` at the field, as pass 8 reports it.
    let ports_path = path_field(path, "ports");
    let ports = svc
        .ports
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let port = p
                .port
                .as_ref()
                .map(|fs| {
                    resolve_service_int(
                        fs,
                        &service_symtab,
                        budgets,
                        &path_field(&path_index(&ports_path, i), "port"),
                        1..=65535,
                        "must be between 1 and 65535.",
                        check_errors,
                    )
                })
                .transpose()?
                .flatten()
                // Range-checked above (1..=65535), so the narrowing cannot
                // fail; were the range ever widened by mistake, failing loudly
                // beats silently clamping to 65535.
                .map(|v| u16::try_from(v).expect("port range-checked to 1..=65535"));
            Ok(job::ServicePort {
                name: p.name.clone(),
                port,
                protocol: p.protocol,
            })
        })
        .collect::<Result<Vec<_>, ModelError>>()?;
    check_duplicate_port_numbers(&ports, &ports_path, check_errors);

    // §9.3 <ServiceHealthCheck>: the four numeric fields resolve alike;
    // the defaults differ by type (readinessIntervalSeconds 1 for
    // TCP_CONNECT and 5 for COMMAND; healthIntervalSeconds 30, or no
    // heartbeat for STDOUT).
    let hc_path = path_field(path, "healthCheck");
    let declared = svc.health_check();
    let mut resolve_field = |name: &'static str,
                             value: Option<&openjd_expr::FormatString>,
                             default: u64|
     -> Result<u64, ModelError> {
        resolve_service_u64(
            value,
            &service_symtab,
            budgets,
            &path_field(&hc_path, name),
            1..=i64::MAX,
            "must be > 0.",
            default,
            check_errors,
        )
    };
    let readiness_timeout_seconds = resolve_field(
        "readinessTimeoutSeconds",
        declared.readiness_timeout_seconds(),
        template::ServiceHealthCheck::DEFAULT_READY_TIMEOUT_SECONDS,
    )?;
    let failure_threshold = resolve_field(
        "failureThreshold",
        declared.failure_threshold(),
        template::ServiceHealthCheck::DEFAULT_FAILURE_THRESHOLD,
    )?;
    let health_check = match &declared {
        template::ServiceHealthCheck::TcpConnect {
            ports: probed,
            readiness_interval_seconds,
            health_interval_seconds,
            ..
        } => job::ServiceHealthCheck::TcpConnect {
            // §9 item 7 / §9.4 item 2: the default port set is every TCP
            // port; validation has rejected a Service with none.
            ports: probed
                .clone()
                .unwrap_or_else(|| svc.tcp_port_names().map(str::to_string).collect()),
            readiness_interval_seconds: resolve_field(
                "readinessIntervalSeconds",
                readiness_interval_seconds.as_ref(),
                template::ServiceHealthCheck::DEFAULT_TCP_CONNECT_READINESS_INTERVAL_SECONDS,
            )?,
            readiness_timeout_seconds,
            health_interval_seconds: resolve_field(
                "healthIntervalSeconds",
                health_interval_seconds.as_ref(),
                template::ServiceHealthCheck::DEFAULT_HEALTH_INTERVAL_SECONDS,
            )?,
            failure_threshold,
        },
        template::ServiceHealthCheck::Command {
            readiness_interval_seconds,
            health_interval_seconds,
            ..
        } => job::ServiceHealthCheck::Command {
            readiness_interval_seconds: resolve_field(
                "readinessIntervalSeconds",
                readiness_interval_seconds.as_ref(),
                template::ServiceHealthCheck::DEFAULT_COMMAND_READINESS_INTERVAL_SECONDS,
            )?,
            readiness_timeout_seconds,
            health_interval_seconds: resolve_field(
                "healthIntervalSeconds",
                health_interval_seconds.as_ref(),
                template::ServiceHealthCheck::DEFAULT_HEALTH_INTERVAL_SECONDS,
            )?,
            failure_threshold,
        },
        template::ServiceHealthCheck::Stdout {
            health_interval_seconds,
            ..
        } => job::ServiceHealthCheck::Stdout {
            readiness_timeout_seconds,
            // No default: a `null` resolution, like an absent field, means
            // no heartbeat is expected (§9.4 item 5).
            health_interval_seconds: health_interval_seconds
                .as_ref()
                .map(|fs| {
                    resolve_service_int(
                        fs,
                        &service_symtab,
                        budgets,
                        &path_field(&hc_path, "healthIntervalSeconds"),
                        1..=i64::MAX,
                        "must be > 0.",
                        check_errors,
                    )
                })
                .transpose()?
                .flatten()
                .map(i64::unsigned_abs),
            failure_threshold,
        },
    };

    let policy = svc.restart_policy();
    let policy_path = path_field(path, "restartPolicy");
    let max_restarts = resolve_service_u64(
        policy.max_restarts.as_ref(),
        &service_symtab,
        budgets,
        &path_field(&policy_path, "maxRestarts"),
        0..=i64::MAX,
        "must be >= 0.",
        // DEFAULT_MAX_RESTARTS is 0, which fits every unsigned width.
        template::ServiceRestartPolicy::DEFAULT_MAX_RESTARTS.unsigned_abs(),
        check_errors,
    )?;
    // §9.5 item 2: a resolved maxRestarts > 0 needs
    // completedTasks (a literal one was checked at template validation).
    if max_restarts > 0 && policy.completed_tasks.is_none() {
        check_errors.add(
            &policy_path,
            crate::template::validate_v2023_09::service::completed_tasks_required_message(
                i64::try_from(max_restarts).unwrap_or(i64::MAX),
            ),
        );
    }
    let restart_policy = job::ServiceRestartPolicy {
        max_restarts,
        completed_tasks: policy.completed_tasks,
    };

    check_fields(&service_symtab, check_errors)?;

    // ENV_PARAMS, for a Job Template's Service (RFC 0009 `<StepDependency>`
    // item 3): `enableIf`, the Job Environments its Sessions enter and its
    // effective values for them, and the entries for external Environments
    // frozen for submission, exactly as for a Step (§3.2.2, §7.4.1), in the
    // Service's scope: the Job Parameters, `Job.Name`, `Job.EnvNames`, and
    // its `let` bindings. Its errors are filed under §7.4.1's steps, after
    // every Step's.
    let entries = requester.map(|r| {
        super::env_params::resolve_step_entries(
            &super::env_params::Requester::service(svc, r.index, r.step_count),
            &service_symtab,
            r.job_envs,
            super::env_params::ResolvePolicy::new(ctx),
            staged,
        )
    });
    // A recorded job-creation-stage violation leaves the Service without
    // its resolved values; job creation fails on the collection.
    if check_errors.len() > before {
        return Ok(None);
    }
    let selection = entries.map(|e| e.selection);

    let converted = job::Service {
        name: svc.name.clone(),
        description: svc.description.as_ref().map(|d| d.0.clone()),
        // A Service of the Job Template until the submission stage
        // stamps an external Service with its attachment.
        document: job::Document::JobTemplate,
        scope,
        // Step and Service entries; an `environment:` entry (ENV_PARAMS)
        // orders nothing (§3.2 constraint 3), so it has no place here, as
        // for a Step.
        dependencies: svc
            .dependencies
            .as_ref()
            .map(|deps| {
                deps.iter()
                    .filter(|d| d.step().is_some() || d.service().is_some())
                    .map(job::StepDependency::from)
                    .collect::<Vec<_>>()
            })
            .filter(|deps| !deps.is_empty()),
        host_requirements,
        // Both set by the submission stage, which decides the
        // Environments.
        environment_parameters: None,
        environments: None,
        ports,
        health_check,
        restart_policy,
        variables: svc.variables.clone(),
        script: job::ServiceScript {
            let_bindings: svc.script.let_bindings.clone(),
            actions: job::ServiceActions {
                on_enter: svc.script.actions.on_enter.as_ref().map(convert_action),
                on_run: convert_action(&svc.script.actions.on_run),
                on_health_check: svc
                    .script
                    .actions
                    .on_health_check
                    .as_ref()
                    .map(convert_action),
                on_exit: svc.script.actions.on_exit.as_ref().map(convert_action),
            },
            embedded_files: svc
                .script
                .embedded_files
                .as_ref()
                .map(|files| files.iter().map(convert_embedded_file).collect()),
        },
        resolved_symtab: None,
    };
    let filtered = filter_symtab_for_service(&converted, &service_symtab);
    Ok(Some((
        job::Service {
            resolved_symtab: Some(openjd_expr::SerializedSymbolTable::from_symtab(&filtered)),
            ..converted
        },
        selection,
        discarded_host_requirements,
    )))
}

/// What [`instantiate_service`] returns for a Service it builds: the
/// Service, its Environment decisions (a Job Template's Service only), and
/// its own discarded host requirements.
pub(super) type InstantiatedService<'a> = (
    job::Service,
    Option<super::env_params::StepSelection<'a>>,
    Vec<super::DiscardedHostRequirement>,
);

/// §9 item 6.4: no two ports of the same `protocol` share a
/// resolved `port` number. Template validation applies the same rule to
/// literal numbers; this catches the format-string forms once resolved,
/// reporting the later port with the same message, at `ports[i] -> port`.
fn check_duplicate_port_numbers(
    ports: &[job::ServicePort],
    ports_path: &[PathElement],
    errors: &mut ValidationErrors,
) {
    for (i, later) in ports.iter().enumerate() {
        let Some(number) = later.port else {
            continue;
        };
        if let Some(earlier) = ports[..i]
            .iter()
            .find(|p| p.protocol == later.protocol && p.port == Some(number))
        {
            errors.add(
                &path_field(&path_index(ports_path, i), "port"),
                format!(
                    "{} port {number} is also used by port '{}'; two ports with the same \
                     protocol must not have the same port number.",
                    later.protocol, earlier.name
                ),
            );
        }
    }
}

/// Resolve one of a Service's numeric `@fmtstring` fields (`port`,
/// `timeoutSeconds`, `intervalSeconds`, `maxRestarts`) with target type
/// `int?` (RFC 0009 §9.5.1): `Ok(None)` when a whole-field expression
/// resolves to `null` — the field is treated as not provided — otherwise
/// the integer, which must lie in `range`. A value outside it (`range_msg`)
/// or a multi-segment string that does not parse as an integer is recorded
/// in `errors` at `path` — matching the raw-text check for the literal form
/// of the same field — and returned as `Ok(None)`, so the caller keeps
/// checking and discards the Service afterwards. A multi-segment string
/// concatenates and parses with surrounding whitespace tolerated, as
/// `<Action>.timeout` does. `Err` only when the format string does not
/// resolve to `int?` at all.
#[allow(clippy::too_many_arguments)]
fn resolve_service_int(
    fs: &openjd_expr::FormatString,
    symtab: &SymbolTable,
    budgets: super::EvalBudgets,
    path: &[PathElement],
    range: std::ops::RangeInclusive<i64>,
    range_msg: &str,
    errors: &mut ValidationErrors,
) -> Result<Option<i64>, ModelError> {
    let target = openjd_expr::ExprType::union(vec![
        openjd_expr::ExprType::INT,
        openjd_expr::ExprType::NULLTYPE,
    ]);
    let resolved = fs
        .resolve_with(symtab, &budgets.fs_options().with_target_type(&target))
        .map_err(|e| ModelError::FormatStringError {
            message: format!("{}: {e}", path_to_string(path)),
            input: Some(fs.raw().to_string()),
            start: None,
            end: None,
            source: Some(e),
        })?;
    let value = match resolved {
        openjd_expr::ExprValue::Null => return Ok(None),
        openjd_expr::ExprValue::Int(v) => v,
        other => match other.to_display_string().trim().parse::<i64>() {
            Ok(v) => v,
            Err(_) => {
                errors.add(path, "must be an integer.");
                return Ok(None);
            }
        },
    };
    if !range.contains(&value) {
        errors.add(path, range_msg);
        return Ok(None);
    }
    Ok(Some(value))
}

/// [`resolve_service_int`] for an optional field with a non-negative range
/// and a spec default: the resolved value, or `default` when the field is
/// absent or resolves to `null`.
#[allow(clippy::too_many_arguments)]
fn resolve_service_u64(
    fs: Option<&openjd_expr::FormatString>,
    symtab: &SymbolTable,
    budgets: super::EvalBudgets,
    path: &[PathElement],
    range: std::ops::RangeInclusive<i64>,
    range_msg: &str,
    default: u64,
    errors: &mut ValidationErrors,
) -> Result<u64, ModelError> {
    let Some(fs) = fs else {
        return Ok(default);
    };
    Ok(
        resolve_service_int(fs, symtab, budgets, path, range, range_msg, errors)?
            // The range starts at 0 or above, so the value is non-negative.
            .map_or(default, |v| v.unsigned_abs()),
    )
}

/// Build the check symbol table for a Service's carried-forward
/// (host-resolved) format strings: `base` (the Service's job-creation
/// table: concrete `Param.*` / `RawParam.*` / `Job.Name` / `Step.Name` /
/// step- and service-level `let` bindings) plus `Unresolved` placeholders
/// for `Session.*`, PATH `Param.*`, this Service's `Service.File.*`, its
/// own `Service.<name>.<port>.*` (with `bindAddress`) and the `port` /
/// `connectAddress` of every Service in `in_scope`, with the
/// `<ServiceScript>.let` bindings evaluated in — the scope the Service
/// Session binds at run time. `Task.*` is never in scope within a Service.
///
/// A `let` binding that fails to evaluate is reported into `errors` at
/// `<path> -> script -> let[j]` exactly as for a step script (see
/// [`build_task_check_symtab`]) and bound `Unresolved`.
#[allow(clippy::too_many_arguments)]
fn build_service_check_symtab<'a, 'r>(
    svc: &template::Service,
    base: &SymbolTable,
    has_expr: bool,
    ctx: &crate::types::ValidationContext,
    path: &[PathElement],
    in_scope: impl Iterator<Item = &'a template::Service>,
    requirements: impl IntoIterator<Item = &'r template::ServiceRequirement>,
    errors: &mut ValidationErrors,
) -> Result<SymbolTable, ModelError> {
    let mut symtab = base.clone();
    add_unresolved_session_symbols(&mut symtab)?;
    crate::job::service_symbols::add_unresolved_service_file_symbols(&mut symtab, svc)?;
    crate::job::service_symbols::add_unresolved_service_symbols(&mut symtab, in_scope, Some(svc))?;
    crate::job::service_symbols::add_unresolved_requirement_symbols(&mut symtab, requirements)?;
    if has_expr {
        if let Some(bindings) = &svc.script.let_bindings {
            crate::template::validate_v2023_09::format_strings::check_carried_forward_let_bindings(
                bindings,
                &path_field(&path_field(path, "script"), "let"),
                &mut symtab,
                ctx,
                openjd_expr::HostContext::Unresolved,
                errors,
            );
        }
    }
    Ok(symtab)
}

/// Add `Unresolved` placeholders for the symbols only a session can
/// bind: `Session.*`, plus `Param.*` for PATH-typed job parameters
/// (excluded from the job-creation symtab because they require
/// session-time path mapping — the Param type is derived from the
/// `RawParam.*` value that is present).
///
/// Seed failures (a key path colliding with an existing scalar) are
/// propagated rather than ignored — a silently failed seed would
/// degrade the downstream checks to no-ops. The seed keys are
/// uppercase-rooted and `let` bindings must start lowercase, so this
/// only fails if an internal invariant is broken.
fn add_unresolved_session_symbols(symtab: &mut SymbolTable) -> Result<(), ModelError> {
    let raw_param_names: Vec<String> = symtab
        .get_table("RawParam")
        .map(|t| t.keys().map(str::to_string).collect())
        .unwrap_or_default();
    for name in raw_param_names {
        let param_key = format!("Param.{name}");
        if !symtab.contains(&param_key) {
            let raw_key = format!("RawParam.{name}");
            let unresolved_type = match symtab.get_value(&raw_key) {
                Some(
                    openjd_expr::ExprValue::ListPath(..) | openjd_expr::ExprValue::ListString(..),
                ) => openjd_expr::ExprType::list(openjd_expr::ExprType::PATH),
                _ => openjd_expr::ExprType::PATH,
            };
            symtab.set(
                &param_key,
                openjd_expr::ExprValue::Unresolved(unresolved_type),
            )?;
        }
    }
    symtab.set(
        "Session.WorkingDirectory",
        openjd_expr::ExprValue::Unresolved(openjd_expr::ExprType::PATH),
    )?;
    symtab.set(
        "Session.HasPathMappingRules",
        openjd_expr::ExprValue::Unresolved(openjd_expr::ExprType::BOOL),
    )?;
    symtab.set(
        "Session.PathMappingRulesFile",
        openjd_expr::ExprValue::Unresolved(openjd_expr::ExprType::PATH),
    )?;
    Ok(())
}

/// Build the check symbol table for a step script's carried-forward
/// format strings (task scope): the step's symtab (concrete `Param.*` /
/// `RawParam.*` / `Job.Name` / `Step.Name` / step-level `let`
/// bindings) plus `Unresolved` placeholders for `Session.*`, PATH
/// `Param.*`, `Task.Param.*` / `Task.RawParam.*`, and `Task.File.*` —
/// the same scope the session runtime binds when it resolves these
/// format strings at run time.
///
/// Script-level `let` bindings are evaluated into the table with
/// unresolved host context through pass 8's own evaluation helper
/// (`check_carried_forward_let_bindings`): a binding that fails with the
/// real parameter values is reported into `errors` at
/// `<script_path> -> let[j]` with pass 8's message and caret, and bound
/// `Unresolved` so the script's remaining checks still run.
///
/// `in_scope_services` are the Services whose `Service.<name>.<port>.port`
/// / `.connectAddress` a Task of this Step may reference (RFC 0009): the
/// inline Services the Step lists in its `dependencies`; `requirements` are
/// the `externalServices` entries it lists, seeded the same way for their
/// declared ports.
///
/// `Err` only for a failed symbol seed — an internal invariant (see
/// [`add_unresolved_session_symbols`]).
#[allow(clippy::too_many_arguments)]
fn build_task_check_symtab<'a>(
    st: &template::StepTemplate,
    script: &template::StepScript,
    step_symtab: &SymbolTable,
    has_expr: bool,
    ctx: &crate::types::ValidationContext,
    script_path: &[PathElement],
    errors: &mut ValidationErrors,
    in_scope_services: impl Iterator<Item = &'a template::Service>,
    requirements: impl IntoIterator<Item = &'a template::ServiceRequirement>,
) -> Result<SymbolTable, ModelError> {
    let mut check_symtab = step_symtab.clone();
    add_unresolved_session_symbols(&mut check_symtab)?;
    crate::job::service_symbols::add_unresolved_requirement_symbols(
        &mut check_symtab,
        requirements,
    )?;
    crate::job::service_symbols::add_unresolved_service_symbols(
        &mut check_symtab,
        in_scope_services,
        None,
    )?;

    if let Some(ps) = &st.parameter_space {
        for tp in &ps.task_parameter_definitions {
            let tp_type = match tp {
                crate::template::TaskParameterDefinition::INT(_) => openjd_expr::ExprType::INT,
                crate::template::TaskParameterDefinition::CHUNK_INT(_) => {
                    openjd_expr::ExprType::RANGE_EXPR
                }
                crate::template::TaskParameterDefinition::FLOAT(_) => openjd_expr::ExprType::FLOAT,
                crate::template::TaskParameterDefinition::STRING(_) => {
                    openjd_expr::ExprType::STRING
                }
                crate::template::TaskParameterDefinition::PATH(_) => openjd_expr::ExprType::PATH,
            };
            check_symtab.set(
                &format!("Task.Param.{}", tp.name()),
                openjd_expr::ExprValue::Unresolved(tp_type.clone()),
            )?;
            let raw_type = match tp {
                crate::template::TaskParameterDefinition::PATH(_) => openjd_expr::ExprType::STRING,
                _ => tp_type,
            };
            check_symtab.set(
                &format!("Task.RawParam.{}", tp.name()),
                openjd_expr::ExprValue::Unresolved(raw_type),
            )?;
        }
    }

    if let Some(files) = &script.embedded_files {
        for f in files {
            check_symtab.set(
                &format!("Task.File.{}", f.name),
                openjd_expr::ExprValue::Unresolved(openjd_expr::ExprType::PATH),
            )?;
        }
    }

    if has_expr {
        if let Some(bindings) = &script.let_bindings {
            crate::template::validate_v2023_09::format_strings::check_carried_forward_let_bindings(
                bindings,
                &path_field(script_path, "let"),
                &mut check_symtab,
                ctx,
                openjd_expr::HostContext::Unresolved,
                errors,
            );
        }
    }

    Ok(check_symtab)
}

/// Build the check symbol table for an environment's carried-forward
/// format strings (session scope; job-level or step-level
/// environments): the base symtab (concrete `Param.*` / `RawParam.*` /
/// `Job.Name`, plus `Step.Name` and step-level `let` bindings when
/// `base` is a step's table) with `Unresolved` placeholders for
/// `Session.*`, PATH `Param.*`, this environment's `Env.File.*`, and its
/// `Env.Param.*` / `Env.RawParam.*` (ENV_PARAMS),
/// and the environment's script-level `let` bindings evaluated in —
/// mirroring what the session runtime binds when it resolves the
/// environment at run time.
///
/// A `let` binding that fails to evaluate is reported into `errors` at
/// `<env_path> -> script -> let[j]` exactly as for a step script (see
/// [`build_task_check_symtab`]): the bindings only evaluate when the
/// context profile enables EXPR, their expressions already type-checked
/// at pass 8 with everything unresolved, so a failure here comes from
/// the real parameter values and would deterministically recur in every
/// session entering the environment.
///
/// `in_scope_services` are the Services the environment may reference and
/// `requirements` the required Services it may: those it lists in its own
/// `dependencies` for a Job Environment or an attached Environment (which
/// has no requirements), those its Step lists for a Step Environment; their
/// `Service.<name>.<port>.port` / `.connectAddress` are seeded (Template
/// Schemas §9 scope rules 3–5, RFC 0009).
#[allow(clippy::too_many_arguments)]
pub(super) fn build_env_check_symtab<'a>(
    env: &template::Environment,
    base: &SymbolTable,
    has_expr: bool,
    ctx: &crate::types::ValidationContext,
    env_path: &[PathElement],
    errors: &mut ValidationErrors,
    in_scope_services: impl Iterator<Item = &'a template::Service>,
    requirements: impl IntoIterator<Item = &'a template::ServiceRequirement>,
) -> Result<SymbolTable, ModelError> {
    let mut symtab = base.clone();
    add_unresolved_session_symbols(&mut symtab)?;
    // `Env.Param.*` / `Env.RawParam.*` (ENV_PARAMS) are bound only when a
    // Session enters the Environment, so they stay unresolved here.
    crate::template::validate_v2023_09::format_strings::add_env_param_symbols(&mut symtab, env);
    crate::job::service_symbols::add_unresolved_service_symbols(
        &mut symtab,
        in_scope_services,
        None,
    )?;
    crate::job::service_symbols::add_unresolved_requirement_symbols(&mut symtab, requirements)?;
    if let Some(script) = &env.script {
        if let Some(files) = &script.embedded_files {
            for f in files {
                symtab.set(
                    &format!("Env.File.{}", f.name),
                    openjd_expr::ExprValue::Unresolved(openjd_expr::ExprType::PATH),
                )?;
            }
        }
        if has_expr {
            if let Some(bindings) = &script.let_bindings {
                crate::template::validate_v2023_09::format_strings::check_carried_forward_let_bindings(
                    bindings,
                    &path_field(&path_field(env_path, "script"), "let"),
                    &mut symtab,
                    ctx,
                    openjd_expr::HostContext::Unresolved,
                    errors,
                );
            }
        }
    }
    Ok(symtab)
}

fn convert_action(a: &template::Action) -> job::Action {
    job::Action {
        command: a.command.clone(),
        args: a.args.clone(),
        timeout: a.timeout.clone(),
        cancelation: a.cancelation.as_ref().map(|c| match c {
            template::CancelationMode::Terminate => job::CancelationMode::Terminate,
            template::CancelationMode::NotifyThenTerminate {
                notify_period_in_seconds,
            } => job::CancelationMode::NotifyThenTerminate {
                notify_period_in_seconds: notify_period_in_seconds.clone(),
            },
            template::CancelationMode::DeferredMode {
                mode,
                notify_period_in_seconds,
            } => job::CancelationMode::DeferredMode {
                mode: mode.clone(),
                notify_period_in_seconds: notify_period_in_seconds.clone(),
            },
        }),
    }
}

fn convert_step_script(s: &template::StepScript) -> job::StepScript {
    job::StepScript {
        let_bindings: s.let_bindings.clone(),
        actions: job::StepActions {
            on_run: convert_action(&s.actions.on_run),
        },
        embedded_files: s
            .embedded_files
            .as_ref()
            .map(|files| files.iter().map(convert_embedded_file).collect()),
    }
}

fn convert_embedded_file(f: &template::EmbeddedFile) -> job::EmbeddedFile {
    job::EmbeddedFile {
        name: f.name.clone(),
        file_type: f.file_type,
        filename: f.filename.clone(),
        data: f.data.clone(),
        runnable: f.runnable,
        end_of_line: f.end_of_line,
    }
}

/// Convert a template Environment (a Job Environment, a Step Environment, or
/// an Environment Template's `environment`) to a job Environment (SESSION
/// scope — keep FormatString).
#[must_use]
pub fn convert_environment(env: &template::Environment) -> job::Environment {
    convert_environment_with_symtab(env, None)
}

/// Convert a template Environment to a job Environment, optionally filtering
/// the symbol table to only symbols referenced by this environment's format strings.
#[must_use]
pub fn convert_environment_with_symtab(
    env: &template::Environment,
    symtab: Option<&SymbolTable>,
) -> job::Environment {
    let converted = job::Environment {
        name: env.name.clone(),
        description: env.description.as_ref().map(|d| d.0.clone()),
        // §4 item 3: carried as written; validation (pass 12) has rejected
        // every entry that is not `service: <name>` naming a Service the
        // Environment may depend on.
        dependencies: env
            .dependencies
            .as_ref()
            .map(|deps| deps.iter().map(job::StepDependency::from).collect()),
        script: env.script.as_ref().map(|s| job::EnvironmentScript {
            let_bindings: s.let_bindings.clone(),
            actions: job::EnvironmentActions {
                on_enter: s.actions.on_enter.as_ref().map(convert_action),
                on_wrap_env_enter: s.actions.on_wrap_env_enter.as_ref().map(convert_action),
                on_wrap_task_run: s.actions.on_wrap_task_run.as_ref().map(convert_action),
                on_wrap_env_exit: s.actions.on_wrap_env_exit.as_ref().map(convert_action),
                on_wrap_service_enter: s.actions.on_wrap_service_enter.as_ref().map(convert_action),
                on_wrap_service_run: s.actions.on_wrap_service_run.as_ref().map(convert_action),
                on_wrap_service_health_check: s
                    .actions
                    .on_wrap_service_health_check
                    .as_ref()
                    .map(convert_action),
                on_wrap_service_exit: s.actions.on_wrap_service_exit.as_ref().map(convert_action),
                on_exit: s.actions.on_exit.as_ref().map(convert_action),
            },
            embedded_files: s
                .embedded_files
                .as_ref()
                .map(|files| files.iter().map(convert_embedded_file).collect()),
        }),
        variables: env.variables.clone(),
        resolved_symtab: None,
    };
    match symtab {
        Some(st) => {
            let filtered = filter_symtab_for_environment(&converted, st);
            job::Environment {
                resolved_symtab: Some(openjd_expr::SerializedSymbolTable::from_symtab(&filtered)),
                ..converted
            }
        }
        None => converted,
    }
}

/// Evaluate let bindings and return a new symbol table with bound values.
///
/// `memory_limit` / `operation_limit` bound each binding's expression
/// evaluation (the Expression Language spec's "Memory-bounded evaluation"
/// budgets — `CallerLimits::max_eval_memory_bytes` /
/// `max_eval_operations`); `None` uses the spec-recommended defaults.
pub fn evaluate_let_bindings(
    bindings: &[String],
    symtab: &SymbolTable,
    library: Option<&openjd_expr::function_library::FunctionLibrary>,
    path_format: PathFormat,
    memory_limit: Option<usize>,
    operation_limit: Option<usize>,
) -> Result<SymbolTable, ModelError> {
    let mut result = symtab.clone();
    for binding in bindings {
        let eq_pos = binding.find('=').ok_or_else(|| {
            ModelError::Expression(ExpressionError::new(format!(
                "Missing '=' in let binding: {binding}"
            )))
        })?;
        let name = binding[..eq_pos].trim();
        let expr = binding[eq_pos + 1..].trim();
        let prefix = &binding
            [..eq_pos + 1 + binding[eq_pos + 1..].len() - binding[eq_pos + 1..].trim_start().len()];
        let parsed = openjd_expr::ParsedExpression::new(expr).map_err(|e| {
            ModelError::Expression(ExpressionError::new(format!(
                "Error evaluating let binding '{name}': {}",
                e.message_with_expr_prefix(prefix)
            )))
        })?;
        let mut builder = parsed.with_path_format(path_format);
        if let Some(lib) = library {
            builder = builder.with_library(lib);
        }
        if let Some(m) = memory_limit {
            builder = builder.with_memory_limit(m);
        }
        if let Some(o) = operation_limit {
            builder = builder.with_operation_limit(o);
        }
        let value = builder.evaluate(&[&result as &SymbolTable]).map_err(|e| {
            ModelError::Expression(ExpressionError::new(format!(
                "Error evaluating let binding '{name}': {}",
                e.message_with_expr_prefix(prefix)
            )))
        })?;
        result.set(name, value).map_err(|e| {
            ModelError::Expression(ExpressionError::new(format!(
                "Error setting let binding '{name}': {e}"
            )))
        })?;
    }
    Ok(result)
}

// ── Host-context symbol table filtering ─────────────────────────────
//
// `resolved_symtab` is transported to the worker host that runs the job.
// The host evaluates the format strings that remain unresolved after job
// creation, so we filter the full symbol table down to exactly the symbols
// those format strings reference. Most of these are host-context
// (SESSION/TASK scope); action `timeout`/`notifyPeriodInSeconds` are
// template scope (validation restricts them to job-creation-stage symbols)
// but also resolve on the worker, so their references are included too.
//
// Step and Environment have different sets of these format strings:
//   Step  — step-level let bindings, script (actions, embedded files,
//           script-level let bindings), and step-scoped environments
//           (variables, actions, embedded files).
//   Env   — variables, script (actions, embedded files, script-level
//           let bindings).
//
// Both apply the RawParam fallback uniformly: for PATH-typed parameters,
// `Param.X` is absent from the template-scope symtab, so when a format
// string references `Param.X` we include `RawParam.X` instead, allowing
// the session to construct `Param.X` with path mapping at runtime.

fn filter_symtab_for_step(
    full: &SymbolTable,
    script: Option<&job::StepScript>,
    step_environments: &Option<Vec<job::Environment>>,
    step_let_bindings: Option<&[String]>,
) -> SymbolTable {
    let mut filtered = SymbolTable::new();

    if let Some(bindings) = step_let_bindings {
        collect_let_binding_refs(bindings, full, &mut filtered);
    }

    if let Some(s) = script {
        s.actions
            .on_run
            .command
            .copy_used_symtab_values(full, &mut filtered);
        if let Some(args) = &s.actions.on_run.args {
            for a in args {
                a.copy_used_symtab_values(full, &mut filtered);
            }
        }
        if let Some(t) = &s.actions.on_run.timeout {
            t.copy_used_symtab_values(full, &mut filtered);
        }
        match &s.actions.on_run.cancelation {
            Some(job::CancelationMode::NotifyThenTerminate {
                notify_period_in_seconds: Some(n),
            }) => n.copy_used_symtab_values(full, &mut filtered),
            Some(job::CancelationMode::DeferredMode {
                mode,
                notify_period_in_seconds,
            }) => {
                mode.copy_used_symtab_values(full, &mut filtered);
                if let Some(n) = notify_period_in_seconds {
                    n.copy_used_symtab_values(full, &mut filtered);
                }
            }
            _ => {}
        }
        if let Some(files) = &s.embedded_files {
            for f in files {
                if let Some(d) = &f.data {
                    d.copy_used_symtab_values(full, &mut filtered);
                }
            }
        }
        if let Some(bindings) = &s.let_bindings {
            collect_let_binding_refs(bindings, full, &mut filtered);
        }
    }

    if let Some(envs) = step_environments {
        for env in envs {
            if let Some(vars) = &env.variables {
                for fs in vars.values() {
                    fs.copy_used_symtab_values(full, &mut filtered);
                }
            }
            if let Some(es) = &env.script {
                collect_env_action_refs(&es.actions, full, &mut filtered);
                if let Some(files) = &es.embedded_files {
                    for f in files {
                        if let Some(d) = &f.data {
                            d.copy_used_symtab_values(full, &mut filtered);
                        }
                    }
                }
            }
        }
    }

    // For PATH/LIST[PATH] params, Param.X is excluded from the template-scope symtab
    // (host-context only). When a format string references Param.X and it's missing from
    // full, include RawParam.X so the session can construct Param.X with path mapping.
    let all_symbols = collect_all_accessed_symbols(script, step_environments, step_let_bindings);
    include_raw_param_fallbacks(&all_symbols, full, &mut filtered);

    filtered
}

/// For PATH/LIST[PATH] params, `Param.X` is excluded from the template-scope symtab
/// (host-context only). When a format string references `Param.X` and it's missing from
/// `full`, include `RawParam.X` so the session can construct `Param.X` with path mapping.
fn include_raw_param_fallbacks(
    symbols: &std::collections::HashSet<String>,
    full: &SymbolTable,
    filtered: &mut SymbolTable,
) {
    for symbol in symbols {
        if let Some(rest) = symbol.strip_prefix("Param.") {
            // Extract just the parameter name (first component), ignoring
            // property/method access like Param.X.name or Param.X.upper().
            let param_name = rest.split('.').next().unwrap_or(rest);
            let param_key = format!("Param.{param_name}");
            if full.get_value(&param_key).is_none() {
                let raw_key = format!("RawParam.{param_name}");
                copy_symbol_value(&raw_key, full, filtered);
            }
        }
    }
}

/// Collect all symbol names accessed by format strings in a step's script,
/// step environments, and let bindings.
fn collect_all_accessed_symbols(
    script: Option<&job::StepScript>,
    step_environments: &Option<Vec<job::Environment>>,
    step_let_bindings: Option<&[String]>,
) -> std::collections::HashSet<String> {
    let mut symbols = std::collections::HashSet::new();

    fn collect_from_fs(
        fs: &openjd_expr::FormatString,
        out: &mut std::collections::HashSet<String>,
    ) {
        out.extend(fs.accessed_symbols());
    }

    fn collect_from_action(a: &job::Action, out: &mut std::collections::HashSet<String>) {
        collect_from_fs(&a.command, out);
        if let Some(args) = &a.args {
            for fs in args {
                collect_from_fs(fs, out);
            }
        }
        if let Some(t) = &a.timeout {
            collect_from_fs(t, out);
        }
        match &a.cancelation {
            Some(job::CancelationMode::NotifyThenTerminate {
                notify_period_in_seconds: Some(n),
            }) => collect_from_fs(n, out),
            Some(job::CancelationMode::DeferredMode {
                mode,
                notify_period_in_seconds,
            }) => {
                collect_from_fs(mode, out);
                if let Some(n) = notify_period_in_seconds {
                    collect_from_fs(n, out);
                }
            }
            _ => {}
        }
    }

    if let Some(bindings) = step_let_bindings {
        for binding in bindings {
            if let Some(eq_pos) = binding.find('=') {
                let expr = binding[eq_pos + 1..].trim();
                if let Ok(parsed) = openjd_expr::eval::ParsedExpression::new(expr) {
                    symbols.extend(parsed.accessed_symbols().iter().cloned());
                }
            }
        }
    }

    if let Some(s) = script {
        collect_from_action(&s.actions.on_run, &mut symbols);
        if let Some(files) = &s.embedded_files {
            for f in files {
                if let Some(d) = &f.data {
                    collect_from_fs(d, &mut symbols);
                }
            }
        }
        if let Some(bindings) = &s.let_bindings {
            for binding in bindings {
                if let Some(eq_pos) = binding.find('=') {
                    let expr = binding[eq_pos + 1..].trim();
                    if let Ok(parsed) = openjd_expr::eval::ParsedExpression::new(expr) {
                        symbols.extend(parsed.accessed_symbols().iter().cloned());
                    }
                }
            }
        }
    }

    if let Some(envs) = step_environments {
        for env in envs {
            if let Some(vars) = &env.variables {
                for fs in vars.values() {
                    collect_from_fs(fs, &mut symbols);
                }
            }
            if let Some(es) = &env.script {
                for action in es.actions.iter_actions() {
                    collect_from_action(action, &mut symbols);
                }
                if let Some(files) = &es.embedded_files {
                    for f in files {
                        if let Some(d) = &f.data {
                            collect_from_fs(d, &mut symbols);
                        }
                    }
                }
            }
        }
    }

    symbols
}

fn collect_let_binding_refs(bindings: &[String], full: &SymbolTable, filtered: &mut SymbolTable) {
    for binding in bindings {
        if let Some(eq_pos) = binding.find('=') {
            let expr = binding[eq_pos + 1..].trim();
            // Bindings have already been validated and evaluated by this point.
            // Parse here only to discover referenced symbols for the filtered symtab.
            // A parse failure is unreachable but harmless — the symbol just won't
            // appear in the filtered output.
            if let Ok(parsed) = openjd_expr::eval::ParsedExpression::new(expr) {
                for symbol in parsed.accessed_symbols() {
                    copy_symbol_value(symbol, full, filtered);
                }
            }
        }
    }
}

fn collect_env_action_refs(
    actions: &job::EnvironmentActions,
    full: &SymbolTable,
    filtered: &mut SymbolTable,
) {
    for action in actions.iter_actions() {
        action.command.copy_used_symtab_values(full, filtered);
        if let Some(args) = &action.args {
            for a in args {
                a.copy_used_symtab_values(full, filtered);
            }
        }
        if let Some(t) = &action.timeout {
            t.copy_used_symtab_values(full, filtered);
        }
        match &action.cancelation {
            Some(job::CancelationMode::NotifyThenTerminate {
                notify_period_in_seconds: Some(n),
            }) => n.copy_used_symtab_values(full, filtered),
            // A deferred (format-string) cancelation mode and its period are
            // resolved at run time; their referenced symbols must survive the
            // filter or resolution fails with "Undefined variable". Keep in
            // sync with collect_env_accessed_symbols below, which collects
            // the same fields for the RawParam.* fallback pass.
            Some(job::CancelationMode::DeferredMode {
                mode,
                notify_period_in_seconds,
            }) => {
                mode.copy_used_symtab_values(full, filtered);
                if let Some(n) = notify_period_in_seconds {
                    n.copy_used_symtab_values(full, filtered);
                }
            }
            _ => {}
        }
    }
}

fn filter_symtab_for_environment(env: &job::Environment, full: &SymbolTable) -> SymbolTable {
    let mut filtered = SymbolTable::new();
    if let Some(vars) = &env.variables {
        for fs in vars.values() {
            fs.copy_used_symtab_values(full, &mut filtered);
        }
    }
    if let Some(es) = &env.script {
        collect_env_action_refs(&es.actions, full, &mut filtered);
        if let Some(files) = &es.embedded_files {
            for f in files {
                if let Some(d) = &f.data {
                    d.copy_used_symtab_values(full, &mut filtered);
                }
            }
        }
        if let Some(bindings) = &es.let_bindings {
            collect_let_binding_refs(bindings, full, &mut filtered);
        }
    }
    let symbols = collect_env_accessed_symbols(env);
    include_raw_param_fallbacks(&symbols, full, &mut filtered);
    filtered
}

/// The Service counterpart of [`filter_symtab_for_environment`]: the
/// symbols the Service Session's host-resolved format strings reference —
/// `variables`, every action of the script (command, args, timeout,
/// cancelation), embedded-file `data`, and the `<ServiceScript>.let`
/// bindings — copied out of the Service's job-creation table (which holds
/// the resolved `<Service>.let` values), with the `RawParam.*` fallback for
/// PATH parameters.
fn filter_symtab_for_service(svc: &job::Service, full: &SymbolTable) -> SymbolTable {
    let mut filtered = SymbolTable::new();
    let mut symbols = std::collections::HashSet::new();
    if let Some(vars) = &svc.variables {
        for fs in vars.values() {
            fs.copy_used_symtab_values(full, &mut filtered);
            symbols.extend(fs.accessed_symbols());
        }
    }
    for action in svc.script.actions.iter_actions() {
        copy_action_refs(action, full, &mut filtered);
        collect_action_symbols(action, &mut symbols);
    }
    if let Some(files) = &svc.script.embedded_files {
        for f in files {
            if let Some(d) = &f.data {
                d.copy_used_symtab_values(full, &mut filtered);
                symbols.extend(d.accessed_symbols());
            }
        }
    }
    if let Some(bindings) = &svc.script.let_bindings {
        collect_let_binding_refs(bindings, full, &mut filtered);
        symbols.extend(let_binding_symbols(bindings));
    }
    include_raw_param_fallbacks(&symbols, full, &mut filtered);
    filtered
}

/// Copy the symbols one action's worker-resolved fields (command, args,
/// timeout, cancelation) reference from `full` into `filtered`.
fn copy_action_refs(action: &job::Action, full: &SymbolTable, filtered: &mut SymbolTable) {
    action.command.copy_used_symtab_values(full, filtered);
    if let Some(args) = &action.args {
        for a in args {
            a.copy_used_symtab_values(full, filtered);
        }
    }
    if let Some(t) = &action.timeout {
        t.copy_used_symtab_values(full, filtered);
    }
    match &action.cancelation {
        Some(job::CancelationMode::NotifyThenTerminate {
            notify_period_in_seconds: Some(n),
        }) => n.copy_used_symtab_values(full, filtered),
        Some(job::CancelationMode::DeferredMode {
            mode,
            notify_period_in_seconds,
        }) => {
            mode.copy_used_symtab_values(full, filtered);
            if let Some(n) = notify_period_in_seconds {
                n.copy_used_symtab_values(full, filtered);
            }
        }
        _ => {}
    }
}

/// The symbol names one action's worker-resolved fields access.
fn collect_action_symbols(action: &job::Action, out: &mut std::collections::HashSet<String>) {
    out.extend(action.command.accessed_symbols());
    if let Some(args) = &action.args {
        for fs in args {
            out.extend(fs.accessed_symbols());
        }
    }
    if let Some(t) = &action.timeout {
        out.extend(t.accessed_symbols());
    }
    match &action.cancelation {
        Some(job::CancelationMode::NotifyThenTerminate {
            notify_period_in_seconds: Some(n),
        }) => out.extend(n.accessed_symbols()),
        Some(job::CancelationMode::DeferredMode {
            mode,
            notify_period_in_seconds,
        }) => {
            out.extend(mode.accessed_symbols());
            if let Some(n) = notify_period_in_seconds {
                out.extend(n.accessed_symbols());
            }
        }
        _ => {}
    }
}

/// The symbol names a `let` list's expressions access.
fn let_binding_symbols(bindings: &[String]) -> std::collections::HashSet<String> {
    let mut symbols = std::collections::HashSet::new();
    for binding in bindings {
        if let Some(eq_pos) = binding.find('=') {
            let expr = binding[eq_pos + 1..].trim();
            if let Ok(parsed) = openjd_expr::eval::ParsedExpression::new(expr) {
                symbols.extend(parsed.accessed_symbols().iter().cloned());
            }
        }
    }
    symbols
}

/// Collect all symbol names accessed by an environment's worker-resolved
/// format strings (host-context fields plus template-scope timeouts).
fn collect_env_accessed_symbols(env: &job::Environment) -> std::collections::HashSet<String> {
    let mut symbols = std::collections::HashSet::new();
    if let Some(vars) = &env.variables {
        for fs in vars.values() {
            symbols.extend(fs.accessed_symbols());
        }
    }
    if let Some(es) = &env.script {
        for action in es.actions.iter_actions() {
            symbols.extend(action.command.accessed_symbols());
            if let Some(args) = &action.args {
                for fs in args {
                    symbols.extend(fs.accessed_symbols());
                }
            }
            if let Some(t) = &action.timeout {
                symbols.extend(t.accessed_symbols());
            }
            match &action.cancelation {
                Some(job::CancelationMode::NotifyThenTerminate {
                    notify_period_in_seconds: Some(n),
                }) => symbols.extend(n.accessed_symbols()),
                // Deferred (format-string) cancelation fields resolve at run
                // time; their symbols feed include_raw_param_fallbacks so a
                // PATH/LIST[PATH] Param.* referenced only here still gets its
                // RawParam.* fallback in the filtered symtab. Keep in sync
                // with collect_env_action_refs above.
                Some(job::CancelationMode::DeferredMode {
                    mode,
                    notify_period_in_seconds,
                }) => {
                    symbols.extend(mode.accessed_symbols());
                    if let Some(n) = notify_period_in_seconds {
                        symbols.extend(n.accessed_symbols());
                    }
                }
                _ => {}
            }
        }
        if let Some(files) = &es.embedded_files {
            for f in files {
                if let Some(d) = &f.data {
                    symbols.extend(d.accessed_symbols());
                }
            }
        }
        if let Some(bindings) = &es.let_bindings {
            for binding in bindings {
                if let Some(eq_pos) = binding.find('=') {
                    let expr = binding[eq_pos + 1..].trim();
                    if let Ok(parsed) = openjd_expr::eval::ParsedExpression::new(expr) {
                        symbols.extend(parsed.accessed_symbols().iter().cloned());
                    }
                }
            }
        }
    }
    symbols
}
