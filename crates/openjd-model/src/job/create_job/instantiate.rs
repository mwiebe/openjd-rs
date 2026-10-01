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
use crate::template::validate_v2023_09::helpers::{check_capability_name, CapabilityKind};
use crate::template::validate_v2023_09::EffectiveLimits;
use openjd_expr::ExpressionError;

use super::ranges;

/// The job-wide inputs every instantiation step shares: the context's
/// extensions and limits, the caller budgets, and the template's Job
/// Services (whose `Service.*` symbols are in scope for every Step).
#[derive(Clone, Copy)]
pub(super) struct InstantiateCtx<'a> {
    pub(super) has_expr: bool,
    pub(super) limits: &'a EffectiveLimits,
    pub(super) ctx: &'a crate::types::ValidationContext,
    pub(super) budgets: super::EvalBudgets,
    /// The template's `jobServices`, in start order (empty when none).
    pub(super) job_services: &'a [template::Service],
}

/// Evaluate a `let` list in template scope (no PATH `Param.*`, no host
/// context) into `symtab`: `<StepTemplate>.let` and, under RFC 0009,
/// `<Service>.let`. `what` names the owner in error messages.
fn evaluate_template_let_bindings(
    bindings: &[String],
    symtab: &mut SymbolTable,
    icx: InstantiateCtx<'_>,
    what: &str,
) -> Result<(), ModelError> {
    let template_profile = icx
        .ctx
        .profile
        .to_expr_profile(openjd_expr::HostContext::None);
    let template_lib = openjd_expr::FunctionLibrary::for_profile(&template_profile);
    for binding in bindings {
        if let Some(eq_pos) = binding.find('=') {
            let name = binding[..eq_pos].trim();
            let expr = binding[eq_pos + 1..].trim();
            if !name.is_empty() && !expr.is_empty() {
                let parsed =
                    openjd_expr::eval::ParsedExpression::with_profile(expr, &template_profile)
                        .map_err(|e| {
                            ModelError::Expression(ExpressionError::new(format!(
                                "{what} '{name}': {e}"
                            )))
                        })?;
                let val = budgeted(
                    parsed
                        .with_path_format(PathFormat::Posix)
                        .with_library(&template_lib),
                    icx.budgets,
                )
                .evaluate(&[symtab as &SymbolTable])
                .map_err(|e| {
                    ModelError::Expression(ExpressionError::new(format!("{what} '{name}': {e}")))
                })?;
                symtab.set(name, val)?;
            }
        }
    }
    Ok(())
}

/// Instantiate a StepTemplate into a Step.
pub(super) fn instantiate_step(
    st: &template::StepTemplate,
    symtab: &SymbolTable,
    icx: InstantiateCtx<'_>,
    step_index: usize,
) -> Result<job::Step, ModelError> {
    let InstantiateCtx {
        has_expr,
        limits,
        ctx,
        budgets,
        job_services,
    } = icx;
    let step_path = [
        PathElement::Field("steps".to_string()),
        PathElement::Index(step_index),
    ];
    let mut step_symtab = symtab.clone();

    let step_name = st.name.clone();

    if has_expr {
        step_symtab.set(
            "Step.Name",
            openjd_expr::ExprValue::String(step_name.clone()),
        )?;
    }

    // Evaluate step-level let bindings (TEMPLATE scope — no PATH Param.*, no host context)
    if has_expr {
        if let Some(bindings) = &st.let_bindings {
            evaluate_template_let_bindings(bindings, &mut step_symtab, icx, "let binding")?;
        }
    }

    let script_template = st.resolve_syntax_sugar()?.or_else(|| st.script.clone());
    let script = script_template.as_ref().map(convert_step_script);

    // The Services whose `Service.*` endpoints this Step's Task Sessions
    // may reference (RFC 0009): every Job Service and the Step's own.
    let step_services_t: &[template::Service] = st.step_services.as_deref().unwrap_or(&[]);
    let in_scope_services = || job_services.iter().chain(step_services_t.iter());

    // Check symbol table for the step script's carried-forward
    // (session/task-scope) format strings: `step_symtab`'s concrete
    // values plus `Unresolved` placeholders for everything only a
    // session can bind. Building it also evaluates script-level `let`
    // bindings (with unresolved host context), which doubles as the
    // type check this stage has always performed on them.
    let check_symtab = script_template
        .as_ref()
        .map(|s| {
            build_task_check_symtab(
                st,
                s,
                &step_symtab,
                has_expr,
                ctx,
                budgets,
                in_scope_services(),
            )
        })
        .transpose()?;

    let host_requirements = st
        .host_requirements
        .as_ref()
        .map(|hr| resolve_host_requirements(hr, &step_symtab, ctx, &step_path, budgets))
        .transpose()?;

    let parameter_space = st
        .parameter_space
        .as_ref()
        .map(|ps| ranges::resolve_parameter_space(ps, &step_symtab, limits, budgets))
        .transpose()?;

    // Validate the resolved parameter space (e.g. association length mismatches)
    if let Some(ref ps) = parameter_space {
        let _ = crate::job::step_param_space::StepParameterSpaceIterator::new(ps)?;
    }

    // Re-run the carried-forward format-string resolved-value checks —
    // the `onRun` action's `command`/`args` and each embedded file's
    // `data` — against the check symbol table, where job parameters are
    // bound to real values. A violation template validation could only
    // lower-bound is decidable here: fail at submission, not on every
    // worker.
    if let (Some(s), Some(cst)) = (&script_template, &check_symtab) {
        let mut check_errors = ValidationErrors::default();
        let script_path = path_field(&step_path, "script");
        crate::template::validate_v2023_09::format_strings::check_carried_forward_step_script(
            s,
            cst,
            ctx,
            &script_path,
            &mut check_errors,
        );
        check_errors.into_result("JobTemplate")?;
    }

    // The same checks for this step's environments (session scope):
    // `variables` values, every action's `command`/`args`, embedded-file
    // `data`. An Environment whose `runScope` excludes SERVICE also sees
    // the in-scope `Service.*` endpoints (RFC 0009).
    if let Some(envs) = &st.step_environments {
        let mut check_errors = ValidationErrors::default();
        let envs_path = path_field(&step_path, "stepEnvironments");
        for (j, env) in envs.iter().enumerate() {
            let env_symtab = build_env_check_symtab(
                env,
                &step_symtab,
                has_expr,
                ctx,
                budgets,
                in_scope_services(),
            )?;
            crate::template::validate_v2023_09::format_strings::check_carried_forward_environment(
                env,
                &env_symtab,
                ctx,
                limits.max_env_var_value_len,
                &path_index(&envs_path, j),
                &mut check_errors,
            );
        }
        check_errors.into_result("JobTemplate")?;
    }

    let step_environments = st
        .step_environments
        .as_ref()
        .map(|envs| envs.iter().map(convert_environment).collect());

    // RFC 0009 `stepServices`: instantiated in the Step's scope (Step.Name
    // and the step-level `let` bindings are in scope), each seeing every
    // Job Service and the Step Services before it.
    let step_services = st
        .step_services
        .as_ref()
        .map(|services| {
            let list_path = path_field(&step_path, "stepServices");
            services
                .iter()
                .enumerate()
                .map(|(k, svc)| {
                    instantiate_service(
                        svc,
                        &step_symtab,
                        icx,
                        &path_index(&list_path, k),
                        job_services.iter().chain(services[..k].iter()),
                    )
                })
                .collect::<Result<Vec<_>, ModelError>>()
        })
        .transpose()?;

    let dependencies = st.dependencies.as_ref().map(|deps| {
        deps.iter()
            .map(|d| job::StepDependency {
                depends_on: d.depends_on.clone(),
            })
            .collect()
    });

    let script = script.ok_or_else(|| {
        ModelError::DecodeValidation("Step must have a script or SimpleAction".to_string())
    })?;
    let filtered_symtab = filter_symtab_for_step(
        &step_symtab,
        Some(&script),
        &step_environments,
        st.let_bindings.as_deref(),
    );

    Ok(job::Step {
        name: step_name,
        description: st.description.as_ref().map(|d| d.0.clone()),
        script,
        step_environments,
        parameter_space,
        host_requirements,
        dependencies,
        step_services,
        resolved_symtab: Some(openjd_expr::SerializedSymbolTable::from_symtab(
            &filtered_symtab,
        )),
    })
}

// ── RFC 0009 Services ─────────────────────────────────────────────────

/// Instantiate one `<Service>` (Template Schemas §9) at `path`
/// (`jobServices[k]` or `steps[i] -> stepServices[k]`).
///
/// `base` is the scope's job-creation symbol table — the job's for a Job
/// Service; the Step's (with `Step.Name` and the step-level `let`
/// bindings) for a Step Service. `in_scope` is every Service whose
/// `Service.<name>.<port>.port` / `.connectAddress` the Service's
/// host-resolved fields may reference: the Services earlier in its own list
/// and, for a Step Service, every Job Service. The Service itself is seeded
/// separately, with `bindAddress`.
///
/// Job-creation-stage fields resolve here: `<Service>.let` (template scope,
/// like `<StepTemplate>.let`), the numeric `@fmtstring` fields with their
/// §9 ranges and defaults, and `hostRequirements`. `variables` and `script`
/// are `@fmtstring[host]`: they are carried forward unresolved and re-checked
/// against a check table where `Param.*` have real values, like an
/// Environment's.
pub(super) fn instantiate_service<'a>(
    svc: &template::Service,
    base: &SymbolTable,
    icx: InstantiateCtx<'_>,
    path: &[PathElement],
    in_scope: impl Iterator<Item = &'a template::Service>,
) -> Result<job::Service, ModelError> {
    let InstantiateCtx {
        has_expr,
        limits,
        ctx,
        budgets,
        ..
    } = icx;
    let mut service_symtab = base.clone();
    if has_expr {
        if let Some(bindings) = &svc.let_bindings {
            evaluate_template_let_bindings(
                bindings,
                &mut service_symtab,
                icx,
                "service let binding",
            )?;
        }
    }

    let host_requirements = svc
        .host_requirements
        .as_ref()
        .map(|hr| resolve_host_requirements(hr, &service_symtab, ctx, path, budgets))
        .transpose()?;

    // §9.2 / §9.3 / §9.4 numeric `@fmtstring` fields: target type `int?`,
    // a `null` resolution meaning "not provided" (the default applies), and
    // a non-null value satisfying the field's range.
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
                    )
                })
                .transpose()?
                .flatten()
                // Range-checked above, so the narrowing cannot fail.
                .map(|v| u16::try_from(v).unwrap_or(u16::MAX));
            Ok(job::ServicePort {
                name: p.name.clone(),
                port,
            })
        })
        .collect::<Result<Vec<_>, ModelError>>()?;

    let rc_path = path_field(path, "readinessCheck");
    let positive = 1..=i64::MAX;
    let readiness_check = match svc.readiness_check() {
        template::ServiceReadinessCheck::TcpConnect {
            ports: probed,
            timeout_seconds,
        } => job::ServiceReadinessCheck::TcpConnect {
            ports: probed.unwrap_or_else(|| svc.port_names().map(str::to_string).collect()),
            timeout_seconds: resolve_service_u64(
                timeout_seconds.as_ref(),
                &service_symtab,
                budgets,
                &path_field(&rc_path, "timeoutSeconds"),
                positive.clone(),
                "must be > 0.",
                template::ServiceReadinessCheck::DEFAULT_TIMEOUT_SECONDS,
            )?,
        },
        template::ServiceReadinessCheck::Command {
            interval_seconds,
            timeout_seconds,
        } => job::ServiceReadinessCheck::Command {
            interval_seconds: resolve_service_u64(
                interval_seconds.as_ref(),
                &service_symtab,
                budgets,
                &path_field(&rc_path, "intervalSeconds"),
                positive.clone(),
                "must be > 0.",
                template::ServiceReadinessCheck::DEFAULT_COMMAND_INTERVAL_SECONDS,
            )?,
            timeout_seconds: resolve_service_u64(
                timeout_seconds.as_ref(),
                &service_symtab,
                budgets,
                &path_field(&rc_path, "timeoutSeconds"),
                positive.clone(),
                "must be > 0.",
                template::ServiceReadinessCheck::DEFAULT_TIMEOUT_SECONDS,
            )?,
        },
        template::ServiceReadinessCheck::Stdout { timeout_seconds } => {
            job::ServiceReadinessCheck::Stdout {
                timeout_seconds: resolve_service_u64(
                    timeout_seconds.as_ref(),
                    &service_symtab,
                    budgets,
                    &path_field(&rc_path, "timeoutSeconds"),
                    positive,
                    "must be > 0.",
                    template::ServiceReadinessCheck::DEFAULT_TIMEOUT_SECONDS,
                )?,
            }
        }
    };

    let policy = svc.restart_policy();
    let restart_policy = job::ServiceRestartPolicy {
        max_attempts: resolve_service_u64(
            policy.max_attempts.as_ref(),
            &service_symtab,
            budgets,
            &path_field(&path_field(path, "restartPolicy"), "maxAttempts"),
            0..=i64::MAX,
            "must be >= 0.",
            // DEFAULT_MAX_ATTEMPTS is 0, which fits every unsigned width.
            template::ServiceRestartPolicy::DEFAULT_MAX_ATTEMPTS.unsigned_abs(),
        )?,
        completed_tasks: policy.completed_tasks(),
    };

    // Re-check the carried-forward (host-resolved) fields with the job
    // parameters bound: `variables`, every action, embedded-file `data`,
    // after evaluating `<ServiceScript>.let` into the check table.
    let in_scope: Vec<&template::Service> = in_scope.collect();
    let check_symtab = build_service_check_symtab(
        svc,
        &service_symtab,
        has_expr,
        ctx,
        budgets,
        in_scope.iter().copied(),
    )?;
    let mut check_errors = ValidationErrors::default();
    crate::template::validate_v2023_09::format_strings::check_carried_forward_service(
        svc,
        &check_symtab,
        ctx,
        limits.max_env_var_value_len,
        path,
        &mut check_errors,
    );
    check_errors.into_result("JobTemplate")?;

    // The same re-checks for the Service's own `serviceEnvironments` (§9
    // item 5), each against a check table with the declaring Service's own
    // `Service.*` scope — `bindAddress` included — and its own `Env.File.*`.
    if let Some(envs) = &svc.service_environments {
        let mut check_errors = ValidationErrors::default();
        let envs_path = path_field(path, "serviceEnvironments");
        for (j, env) in envs.iter().enumerate() {
            let env_symtab = build_service_env_check_symtab(
                env,
                svc,
                &service_symtab,
                has_expr,
                ctx,
                budgets,
                in_scope.iter().copied(),
            )?;
            crate::template::validate_v2023_09::format_strings::check_carried_forward_environment(
                env,
                &env_symtab,
                ctx,
                limits.max_env_var_value_len,
                &path_index(&envs_path, j),
                &mut check_errors,
            );
        }
        check_errors.into_result("JobTemplate")?;
    }

    // Converted like `stepEnvironments`, but — as a Job Environment is —
    // with a `resolved_symtab` of their own, filtered from the Service's
    // job-creation table, so a Service Session can enter them from the
    // `job::Service` alone.
    let service_environments = svc.service_environments.as_ref().map(|envs| {
        envs.iter()
            .map(|e| convert_environment_with_symtab(e, Some(&service_symtab)))
            .collect()
    });

    let converted = job::Service {
        name: svc.name.clone(),
        description: svc.description.as_ref().map(|d| d.0.clone()),
        host_requirements,
        service_environments,
        ports,
        readiness_check,
        restart_policy,
        variables: svc.variables.clone(),
        script: job::ServiceScript {
            let_bindings: svc.script.let_bindings.clone(),
            actions: job::ServiceActions {
                on_enter: svc.script.actions.on_enter.as_ref().map(convert_action),
                on_run: convert_action(&svc.script.actions.on_run),
                on_readiness_check: svc
                    .script
                    .actions
                    .on_readiness_check
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
    Ok(job::Service {
        resolved_symtab: Some(openjd_expr::SerializedSymbolTable::from_symtab(&filtered)),
        ..converted
    })
}

/// Resolve one of a Service's numeric `@fmtstring` fields (`port`,
/// `timeoutSeconds`, `intervalSeconds`, `maxAttempts`) with target type
/// `int?` (RFC 0009 §9.2): `Ok(None)` when a whole-field expression
/// resolves to `null` — the field is treated as not provided — otherwise
/// the integer, which must lie in `range` (else `range_msg`, as a
/// validation error at `path`, matching the raw-text check for the literal
/// form of the same field). A multi-segment string concatenates and parses
/// with surrounding whitespace tolerated, as `<Action>.timeout` does.
fn resolve_service_int(
    fs: &openjd_expr::FormatString,
    symtab: &SymbolTable,
    budgets: super::EvalBudgets,
    path: &[PathElement],
    range: std::ops::RangeInclusive<i64>,
    range_msg: &str,
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
        })?;
    let mut errors = ValidationErrors::default();
    let value = match resolved {
        openjd_expr::ExprValue::Null => return Ok(None),
        openjd_expr::ExprValue::Int(v) => v,
        other => match other.to_display_string().trim().parse::<i64>() {
            Ok(v) => v,
            Err(_) => {
                errors.add(path, "must be an integer.");
                return errors.into_result("JobTemplate").map(|()| None);
            }
        },
    };
    if !range.contains(&value) {
        errors.add(path, range_msg);
    }
    errors.into_result("JobTemplate")?;
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
) -> Result<u64, ModelError> {
    let Some(fs) = fs else {
        return Ok(default);
    };
    Ok(
        resolve_service_int(fs, symtab, budgets, path, range, range_msg)?
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
fn build_service_check_symtab<'a>(
    svc: &template::Service,
    base: &SymbolTable,
    has_expr: bool,
    ctx: &crate::types::ValidationContext,
    budgets: super::EvalBudgets,
    in_scope: impl Iterator<Item = &'a template::Service>,
) -> Result<SymbolTable, ModelError> {
    let mut symtab = base.clone();
    add_unresolved_session_symbols(&mut symtab)?;
    crate::job::service_symbols::add_unresolved_service_file_symbols(&mut symtab, svc)?;
    crate::job::service_symbols::add_unresolved_service_symbols(&mut symtab, in_scope, Some(svc))?;
    if has_expr {
        if let Some(bindings) = &svc.script.let_bindings {
            let host_profile = ctx
                .profile
                .to_expr_profile(openjd_expr::HostContext::Unresolved);
            evaluate_check_let_bindings(bindings, &mut symtab, &host_profile, budgets)?;
        }
    }
    Ok(symtab)
}

/// Build the check symbol table for one of a Service's `serviceEnvironments`
/// (RFC 0009, Template Schemas §9 item 5): `base` — the Service's
/// job-creation table (concrete `Param.*` / `RawParam.*` / `Job.Name`, for a
/// Step Service `Step.Name` and the step-level `let` bindings, and the
/// `<Service>.let` bindings, which §9 item 3 makes available in
/// `serviceEnvironments` as a Step's are in `stepEnvironments`) plus `Unresolved`
/// placeholders for `Session.*`, PATH `Param.*`, the declaring Service's own
/// `Service.<name>.<port>.*` (with `bindAddress`), the `port` /
/// `connectAddress` of every Service in `in_scope`, and the Environment's
/// own `Env.File.*`, with its `<EnvironmentScript>.let` bindings evaluated
/// in. A Service Environment is entered only in the declaring Service's
/// Session, so unlike [`build_env_check_symtab`] the `Service.*` scope is
/// unconditional and includes the declaring Service itself. `Task.*` and
/// `Service.File.*` are never in scope.
fn build_service_env_check_symtab<'a>(
    env: &template::Environment,
    svc: &template::Service,
    base: &SymbolTable,
    has_expr: bool,
    ctx: &crate::types::ValidationContext,
    budgets: super::EvalBudgets,
    in_scope: impl Iterator<Item = &'a template::Service>,
) -> Result<SymbolTable, ModelError> {
    let mut symtab = base.clone();
    add_unresolved_session_symbols(&mut symtab)?;
    crate::job::service_symbols::add_unresolved_service_symbols(&mut symtab, in_scope, Some(svc))?;
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
                let host_profile = ctx
                    .profile
                    .to_expr_profile(openjd_expr::HostContext::Unresolved);
                evaluate_check_let_bindings(bindings, &mut symtab, &host_profile, budgets)?;
            }
        }
    }
    Ok(symtab)
}

/// Apply the caller evaluation budgets to an expression evaluation
/// builder (for the `let`-binding sites, which parse expressions
/// directly rather than resolving a `FormatString`).
fn budgeted(
    mut builder: openjd_expr::EvalBuilder<'_>,
    budgets: super::EvalBudgets,
) -> openjd_expr::EvalBuilder<'_> {
    if let Some(m) = budgets.memory {
        builder = builder.with_memory_limit(m);
    }
    if let Some(o) = budgets.operations {
        builder = builder.with_operation_limit(o);
    }
    builder
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

/// Evaluate a script's `let` bindings into a check symbol table, for
/// both check-symtab builders (step scripts and environments).
///
/// Parses under the caller's host profile — the same profile pass 8
/// parsed the bindings with — so syntax the profile does not enable is
/// refused here exactly as it was at template validation. (Parsing
/// with the latest profile instead would accept at job creation what
/// pass 8 refused, or vice versa after a crate upgrade.) Evaluates
/// under `PathFormat::Posix` like every other job-creation evaluation,
/// with the caller's budgets. A binding that fails to evaluate fails
/// job creation: its expression type-checked at pass 8 with everything
/// unresolved, so the failure comes from the real parameter values and
/// would deterministically recur in every session.
fn evaluate_check_let_bindings(
    bindings: &[String],
    check_symtab: &mut SymbolTable,
    host_profile: &openjd_expr::ExprProfile,
    budgets: super::EvalBudgets,
) -> Result<(), ModelError> {
    let host_lib = openjd_expr::FunctionLibrary::for_profile(host_profile);
    for binding in bindings {
        let Some(eq_pos) = binding.find('=') else {
            continue;
        };
        let name = binding[..eq_pos].trim();
        let expr = binding[eq_pos + 1..].trim();
        if name.is_empty() || expr.is_empty() {
            continue;
        }
        let parsed = openjd_expr::eval::ParsedExpression::with_profile(expr, host_profile)
            .map_err(|e| {
                ModelError::Expression(ExpressionError::new(format!(
                    "script let binding '{name}': {e}"
                )))
            })?;
        let val = budgeted(
            parsed
                .with_path_format(PathFormat::Posix)
                .with_library(&host_lib),
            budgets,
        )
        .evaluate(&[check_symtab as &SymbolTable])
        .map_err(|e| {
            ModelError::Expression(ExpressionError::new(format!(
                "script let binding '{name}': {e}"
            )))
        })?;
        check_symtab.set(name, val)?;
    }
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
/// unresolved host context, which is also the type check job creation
/// has always performed on them: a binding that fails with the real
/// parameter values fails here, deterministically, rather than in every
/// session.
///
/// `in_scope_services` are the Services whose `Service.<name>.<port>.port`
/// / `.connectAddress` a Task of this Step may reference (RFC 0009): every
/// Job Service and the Step's own `stepServices`.
fn build_task_check_symtab<'a>(
    st: &template::StepTemplate,
    script: &template::StepScript,
    step_symtab: &SymbolTable,
    has_expr: bool,
    ctx: &crate::types::ValidationContext,
    budgets: super::EvalBudgets,
    in_scope_services: impl Iterator<Item = &'a template::Service>,
) -> Result<SymbolTable, ModelError> {
    let mut check_symtab = step_symtab.clone();
    add_unresolved_session_symbols(&mut check_symtab)?;
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
            let host_profile = ctx
                .profile
                .to_expr_profile(openjd_expr::HostContext::Unresolved);
            evaluate_check_let_bindings(bindings, &mut check_symtab, &host_profile, budgets)?;
        }
    }

    Ok(check_symtab)
}

/// Build the check symbol table for an environment's carried-forward
/// format strings (session scope; job-level or step-level
/// environments): the base symtab (concrete `Param.*` / `RawParam.*` /
/// `Job.Name`, plus `Step.Name` when `base` is a step's table) with
/// `Unresolved` placeholders for `Session.*`, PATH `Param.*`, and this
/// environment's `Env.File.*`, and the environment's script-level
/// `let` bindings evaluated in — mirroring what the session runtime
/// binds when it resolves the environment at run time.
///
/// A `let` binding that fails to evaluate fails job creation (as the
/// step-script `let` check always has): the bindings only evaluate
/// when the context profile enables EXPR, their expressions already
/// type-checked at pass 8 with everything unresolved, so a failure
/// here comes from the real parameter values and would
/// deterministically recur in every session entering the environment.
///
/// `in_scope_services` are the Services in scope where the environment is
/// defined (every Job Service, plus the Step's for a step environment);
/// their `Service.<name>.<port>.port` / `.connectAddress` are seeded only
/// when the environment's `runScope` excludes `SERVICE` (Template Schemas
/// §4 item 3.2, RFC 0009).
pub(super) fn build_env_check_symtab<'a>(
    env: &template::Environment,
    base: &SymbolTable,
    has_expr: bool,
    ctx: &crate::types::ValidationContext,
    budgets: super::EvalBudgets,
    in_scope_services: impl Iterator<Item = &'a template::Service>,
) -> Result<SymbolTable, ModelError> {
    let mut symtab = base.clone();
    add_unresolved_session_symbols(&mut symtab)?;
    if !env.runs_in(template::RunScope::Service) {
        crate::job::service_symbols::add_unresolved_service_symbols(
            &mut symtab,
            in_scope_services,
            None,
        )?;
    }
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
                let host_profile = ctx
                    .profile
                    .to_expr_profile(openjd_expr::HostContext::Unresolved);
                evaluate_check_let_bindings(bindings, &mut symtab, &host_profile, budgets)?;
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

/// Convert a template Environment to a job Environment (SESSION scope — keep FormatString).
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
        // Validation (pass 11) has rejected unrecognized names, so every
        // entry parses; one that does not (a directly-constructed template
        // bypassing decode) is dropped rather than failing conversion.
        run_scope: env.run_scope.as_ref().map(|names| {
            names
                .iter()
                .filter_map(|n| n.parse::<template::RunScope>().ok())
                .collect()
        }),
        script: env.script.as_ref().map(|s| job::EnvironmentScript {
            let_bindings: s.let_bindings.clone(),
            actions: job::EnvironmentActions {
                on_enter: s.actions.on_enter.as_ref().map(convert_action),
                on_wrap_env_enter: s.actions.on_wrap_env_enter.as_ref().map(convert_action),
                on_wrap_task_run: s.actions.on_wrap_task_run.as_ref().map(convert_action),
                on_wrap_env_exit: s.actions.on_wrap_env_exit.as_ref().map(convert_action),
                on_wrap_service_enter: s.actions.on_wrap_service_enter.as_ref().map(convert_action),
                on_wrap_service_run: s.actions.on_wrap_service_run.as_ref().map(convert_action),
                on_wrap_service_readiness_check: s
                    .actions
                    .on_wrap_service_readiness_check
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

/// Resolve a `hostRequirements` object (a Step's, or a Service's under RFC
/// 0009) against `symtab`. `owner_path` is the path of the object that
/// declares it (`steps[i]`, `jobServices[k]`, `steps[i] -> stepServices[k]`)
/// and prefixes every error path.
fn resolve_host_requirements(
    hr: &template::HostRequirements,
    symtab: &SymbolTable,
    ctx: &crate::types::ValidationContext,
    owner_path: &[PathElement],
    budgets: super::EvalBudgets,
) -> Result<job::HostRequirements, ModelError> {
    let hr_path = path_field(owner_path, "hostRequirements");
    let amounts = hr
        .amounts
        .as_ref()
        .map(|amts| {
            let standard = crate::capabilities::standard_amount_capability_names(
                ctx.profile.revision(),
                ctx.profile.extensions(),
            )?;
            let names = amts
                .iter()
                .enumerate()
                .map(|(amount_index, a)| {
                    resolve_capability_name(
                        &a.name,
                        CapabilityKind::Amount,
                        standard,
                        symtab,
                        budgets,
                        &hr_path,
                        amount_index,
                    )
                })
                .collect::<Result<Vec<_>, ModelError>>()?;
            check_resolved_names_unique(&names, CapabilityKind::Amount, &hr_path)?;
            amts.iter()
                .zip(names)
                .enumerate()
                .map(|(amount_index, (a, name))| {
                    let min = a
                        .min
                        .as_ref()
                        .map(|fs| {
                            ranges::resolve_to_f64(
                                fs,
                                symtab,
                                "hostRequirements amount min",
                                budgets,
                            )
                        })
                        .transpose()?
                        .flatten();
                    let max = a
                        .max
                        .as_ref()
                        .map(|fs| {
                            ranges::resolve_to_f64(
                                fs,
                                symtab,
                                "hostRequirements amount max",
                                budgets,
                            )
                        })
                        .transpose()?
                        .flatten();
                    check_resolved_amount_bounds(min, max, &hr_path, amount_index)?;
                    Ok(job::AmountRequirement { name, min, max })
                })
                .collect::<Result<Vec<_>, ModelError>>()
        })
        .transpose()?;

    let attributes = hr
        .attributes
        .as_ref()
        .map(|attrs| {
            let standard = crate::capabilities::standard_attribute_capabilities(
                ctx.profile.revision(),
                ctx.profile.extensions(),
            )?;
            let standard_names: Vec<&str> = standard.iter().map(|(name, _)| *name).collect();
            let names = attrs
                .iter()
                .enumerate()
                .map(|(attr_index, a)| {
                    resolve_capability_name(
                        &a.name,
                        CapabilityKind::Attribute,
                        &standard_names,
                        symtab,
                        budgets,
                        &hr_path,
                        attr_index,
                    )
                })
                .collect::<Result<Vec<_>, ModelError>>()?;
            check_resolved_names_unique(&names, CapabilityKind::Attribute, &hr_path)?;
            attrs
                .iter()
                .zip(names)
                .enumerate()
                .map(|(attr_index, (a, name))| {
                    let any_of = a
                        .any_of
                        .as_ref()
                        .map(|vals| ranges::resolve_string_list(vals, symtab, budgets))
                        .transpose()?;
                    let all_of = a
                        .all_of
                        .as_ref()
                        .map(|vals| ranges::resolve_string_list(vals, symtab, budgets))
                        .transpose()?;
                    let attr_lower = name.to_lowercase();
                    let is_single_valued = attr_lower == "attr.worker.os.family"
                        || attr_lower == "attr.worker.cpu.arch";
                    let attr_path = path_index(&path_field(&hr_path, "attributes"), attr_index);
                    for (field, values) in [("anyOf", &any_of), ("allOf", &all_of)] {
                        if let Some(values) = values {
                            let field_path = path_to_string(&path_field(&attr_path, field));
                            // Decode checks these counts on the template's
                            // element list, but a whole-field expression
                            // flattens a list inline (Expression Language
                            // §1.3.2), so the resolved count is unrelated
                            // to the count decode saw. Re-apply the rules
                            // on the resolved list, like the emptiness
                            // check below and the range-element cap in
                            // resolve_string_range.
                            if values.is_empty() {
                                return Err(ModelError::DecodeValidation(format!(
                                    "{field_path}: has no elements after resolution"
                                )));
                            }
                            if values.len() > 50 {
                                return Err(ModelError::DecodeValidation(format!(
                                    "{field_path}: exceeds 50 elements after resolution"
                                )));
                            }
                            if is_single_valued && field == "allOf" && values.len() > 1 {
                                return Err(ModelError::DecodeValidation(format!(
                                    "{field_path}: single-valued attribute cannot have more than 1 element after resolution"
                                )));
                            }
                            check_resolved_attribute_values(
                                &name, field, values, standard, &attr_path,
                            )?;
                        }
                    }
                    Ok(job::AttributeRequirement {
                        name,
                        any_of,
                        all_of,
                    })
                })
                .collect::<Result<Vec<_>, ModelError>>()
        })
        .transpose()?;

    Ok(job::HostRequirements {
        amounts,
        attributes,
    })
}

/// Resolve a `hostRequirements` capability name and check the §3.3.1.1 /
/// §3.3.2.1 constraints on the resolved value.
///
/// `name` is `@fmtstring`, and template validation can only check a name
/// whose value it knows: a literal, or one that is fully static. Job
/// creation resolves every name, so the checks run here on the resolved
/// value, through the same [`check_capability_name`] template validation
/// uses, so a violation reads the same way whichever stage caught it.
fn resolve_capability_name(
    name: &openjd_expr::FormatString,
    kind: CapabilityKind,
    standard: &[&str],
    symtab: &SymbolTable,
    budgets: super::EvalBudgets,
    hr_path: &[PathElement],
    index: usize,
) -> Result<String, ModelError> {
    // Required string field: a single whole-field expression resolves with
    // target type `string` (Expression Language §1.3.2).
    let resolved = name
        .resolve_with(
            symtab,
            &budgets
                .fs_options()
                .with_target_type(&openjd_expr::ExprType::STRING),
        )
        .map(|v| match v {
            openjd_expr::ExprValue::String(s) => s,
            other => other.to_display_string(),
        })
        .map_err(|e| ModelError::FormatStringError {
            message: format!("hostRequirements {} name: {e}", kind.noun()),
            input: Some(name.raw().to_string()),
            start: None,
            end: None,
        })?;
    let path = path_index(&path_field(hr_path, kind.field()), index);
    let mut errors = ValidationErrors::default();
    check_capability_name(&resolved, kind, standard, &path, &mut errors);
    errors.into_result("JobTemplate")?;
    Ok(resolved)
}

/// Check the §3.3 constraint that no two amounts, and no two attributes,
/// have the same name after the name format strings have been resolved.
///
/// Decode compares literal names only, since a name containing expressions
/// is unknown there. Two different templates names can resolve to the same
/// capability, so the resolved names are compared here, case-insensitively
/// as §3.3.1.1 / §3.3.2.1 require.
fn check_resolved_names_unique(
    names: &[String],
    kind: CapabilityKind,
    hr_path: &[PathElement],
) -> Result<(), ModelError> {
    let mut seen = std::collections::HashSet::new();
    let mut errors = ValidationErrors::default();
    let list_path = path_field(hr_path, kind.field());
    for (index, name) in names.iter().enumerate() {
        if !seen.insert(name.to_lowercase()) {
            errors.add(
                &path_index(&list_path, index),
                format!("duplicate {} name '{name}'.", kind.noun()),
            );
        }
    }
    errors.into_result("JobTemplate")
}

/// Re-check the `amounts[].min` / `amounts[].max` bounds (§3.3.1) on resolved
/// values.
///
/// Under FEATURE_BUNDLE_1 both fields may be format strings, so the bounds
/// cannot be applied at decode — `validate_v2023_09::structure`'s
/// `parse_literal_amount` returns `None` for a non-literal for exactly that
/// reason, which skips all three checks. Job creation resolves the values, so
/// the deferred checks resume here.
///
/// `min` is `<nonnegativefloat>` and `max` is `<positivefloat>`, so `0` is
/// legal for one and not the other. Paths and wording match the decode-time
/// checks, so a violation reads the same way whichever path caught it.
fn check_resolved_amount_bounds(
    min: Option<f64>,
    max: Option<f64>,
    hr_path: &[PathElement],
    amount_index: usize,
) -> Result<(), ModelError> {
    let amount_path = path_index(&path_field(hr_path, "amounts"), amount_index);
    let mut errors = ValidationErrors::default();
    // Decode enforces "at least one of min or max" on field *presence*,
    // but a whole-field expression resolving to null under the `float?`
    // target means "bound unset" — so a present field can still resolve
    // away. Re-apply the rule on the resolved values; without it the job
    // would carry a boundless amount requirement that matches every
    // worker. The "after resolution" suffix distinguishes this from the
    // decode-time message: the author *did* provide the field.
    if min.is_none() && max.is_none() {
        errors.add(
            &amount_path,
            "must have at least one of min or max after resolution.",
        );
    }
    if let Some(min) = min {
        if min < 0.0 {
            errors.add(&path_field(&amount_path, "min"), "must be non-negative.");
        }
    }
    if let Some(max) = max {
        if max <= 0.0 {
            errors.add(&path_field(&amount_path, "max"), "must be positive.");
        }
    }
    if let (Some(min), Some(max)) = (min, max) {
        if min > max {
            errors.add(&amount_path, format!("min ({min}) > max ({max})."));
        }
    }
    errors.into_result("JobTemplate")
}

/// Re-check `<AttributeCapabilityValue>` constraints (§3.3.2.2) on resolved
/// `attributes[].anyOf` / `attributes[].allOf` values.
///
/// Both fields are `@fmtstring` in base 2023-09, so a value written as a
/// format string is unknown at decode and its constraints cannot be applied
/// there — `validate_v2023_09::structure` gates the decode-time check on
/// `FormatString::is_literal` for exactly that reason. Job creation resolves
/// the value, so the deferred check resumes here.
///
/// Errors are reported at the path and with the wording the decode-time check
/// uses for the same violation, so a violation reads the same way whichever
/// path caught it. Two differences are deliberate: the path always carries the
/// failing element's index, which decode omits for standard capabilities, and
/// the length message names the resolved value.
///
/// Only the first failing `anyOf`/`allOf` group on the first failing attribute
/// is reported, because the caller collects through `?`. Decode accumulates
/// every violation instead. Closing that gap means threading one
/// `ValidationErrors` through the whole of `resolve_host_requirements`.
fn check_resolved_attribute_values(
    capability_name: &str,
    field: &str,
    values: &[String],
    standard: &[(&str, &[&str])],
    attr_path: &[PathElement],
) -> Result<(), ModelError> {
    let mut errors = ValidationErrors::default();
    let field_path = path_field(attr_path, field);
    for (value_index, value) in values.iter().enumerate() {
        if let Err(message) = crate::capabilities::validate_attribute_capability_value(
            capability_name,
            value,
            standard,
        ) {
            errors.add(&path_index(&field_path, value_index), message);
        }
    }
    errors.into_result("JobTemplate")
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
pub(crate) fn collect_env_accessed_symbols(
    env: &job::Environment,
) -> std::collections::HashSet<String> {
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
