// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Job creation: parameter preprocessing and template instantiation.
//!
//! Mirrors Python `_create_job.py` and `_merge_job_parameter.py`.

mod binding;
mod instantiate;
mod job_outputs;
pub mod parameters;
mod ranges;

use indexmap::IndexMap;

use openjd_expr::path_mapping::PathFormat;

use crate::error::ModelError;
use crate::job;
use crate::template::validate_v2023_09::EffectiveLimits;
use crate::template::JobTemplate;
use crate::types::{JobParameterValues, ValidationContext};

pub(crate) use binding::host_scope_symbols;
pub use binding::{advance_step_binding, attach_step_inputs};
pub use job_outputs::{evaluate_job_output, job_output_inputs};
// Re-exports — preserve the existing public API
pub use instantiate::{
    convert_environment, convert_environment_with_symtab, evaluate_let_bindings,
};
pub use parameters::{
    build_symbol_table, merge_job_parameter_definitions, preprocess_job_parameters,
    MergedParameterDefinition, PathParameterOptions,
};

/// Caller evaluation budgets applied to every expression evaluation job
/// creation performs (format-string resolution, `let` bindings, task
/// ranges) — `CallerLimits::max_eval_memory_bytes` /
/// `max_eval_operations`, the Expression Language spec's
/// "Memory-bounded evaluation" lever. `None` uses the spec-recommended
/// defaults. Template validation and the session runtime apply the
/// same budgets, so job creation is never the stage where a lowered
/// budget silently stops applying.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EvalBudgets {
    pub(crate) memory: Option<usize>,
    pub(crate) operations: Option<usize>,
}

impl EvalBudgets {
    pub(crate) fn from_ctx(ctx: &ValidationContext) -> Self {
        Self {
            memory: ctx.caller_limits.max_eval_memory_bytes,
            operations: ctx.caller_limits.max_eval_operations,
        }
    }

    /// The standard job-creation resolution options: POSIX path format
    /// (job creation resolves no host paths) plus the caller budgets.
    /// Call sites chain their field-specific target type onto it.
    pub(crate) fn fs_options(&self) -> openjd_expr::FormatStringOptions<'static> {
        let mut opts = openjd_expr::FormatStringOptions::new().with_path_format(PathFormat::Posix);
        if let Some(m) = self.memory {
            opts = opts.with_memory_limit(m);
        }
        if let Some(o) = self.operations {
            opts = opts.with_operation_limit(o);
        }
        opts
    }
}

/// Create an instantiated Job from a validated JobTemplate and preprocessed parameter values.
///
/// Environment template parameters should already be merged into `job_parameter_values`
/// via [`preprocess_job_parameters`] before calling this function.
///
/// The `ctx` parameter carries the specification revision, enabled
/// extensions, and caller limits that apply to this job instance. Its
/// revision must match the template's, and it must enable every
/// extension the template declares (enabling more is allowed — extra
/// extensions only add symbols and functions the template never
/// references); `create_job` returns a `Compatibility` error otherwise.
/// A context enabling *fewer* extensions than the template declares
/// would make every downstream evaluation ambiguous — an error could be
/// a template defect or a context artifact. An application that does
/// not support an extension already rejects such templates at decode
/// via `supported_extensions`; the convenient "do what the template
/// says" context is [`JobTemplate::default_validation_context`], with
/// [`with_caller_limits`](crate::types::ValidationContext::with_caller_limits)
/// layered on for caller policy (e.g. "enforce stricter caller limits
/// on this queue").
///
/// When `ctx.caller_limits.max_task_count` is set, the total task count
/// across all steps is checked after parameter spaces are resolved. When
/// `max_step_task_count` is set, each Step's task count is checked.
///
/// The resolved-value re-checks on carried-forward fields (and the
/// `let` bindings feeding them) are reported together: every such
/// failure across job environments, steps, and step environments
/// becomes one `ModelError::ModelValidation`, in template-validation
/// order. Failures that make a step uninstantiable (resolving its host
/// requirements or parameter space) abort immediately and are reported
/// alone.
///
/// With `CONTROL_FLOW` (RFC 0010 §When groups are evaluated), each Step's
/// `outputVariables` are copied onto [`job::Step::output_variables`], and
/// each Step is bound as far as the values concrete at job creation allow:
/// its `enableIf` is decided if its inputs are concrete (a `false` Step is
/// [`StepBinding::Skipped`](job::StepBinding::Skipped)), and then every
/// materialization group whose inputs are concrete is evaluated. A Step all
/// of whose groups evaluate is [`StepBinding::Bound`](job::StepBinding::Bound);
/// any other is [`StepBinding::PendingInputs`](job::StepBinding::PendingInputs),
/// for [`advance_step_binding`] to finish. Steps are bound in dependency
/// order, because a Step skipped here makes its `null` symbols concrete and
/// a Step whose Tasks are known makes its `TaskParam` symbols concrete. A
/// step `let` error and the remaining group's check errors are collected and
/// reported with the others, in template order; a Step's or Job
/// Environment's `enableIf` error, a range or host requirement error, a
/// repeated `CHUNK[INT]` value in a Step with outputs, or a Task-count limit
/// aborts at once. `max_task_count` counts the
/// Steps whose Tasks are known at creation. Each Job Environment's
/// `enableIf` is decided here and sets its
/// [`enabled`](job::Environment::enabled); a disabled one is not checked.
///
/// Each `jobOutputDefinitions` entry becomes a [`job::JobOutput`] in
/// [`job::Job::outputs`]. One whose `value` reads no `Steps.*` symbol that is
/// not concrete at creation (it reads only job parameters, the `null`s of a
/// Step skipped here, or `TaskParam` of a Step whose Tasks are known here,
/// though it may still be pending) is evaluated after the Steps, and its
/// [`evaluated`](job::JobOutput::evaluated) set; the rest are left for
/// [`evaluate_job_output`]. A job output that fails to evaluate, convert, or
/// meet its constraints fails `create_job`, every such failure reported
/// together.
pub fn create_job(
    job_template: &JobTemplate,
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
) -> Result<job::Job, ModelError> {
    // The context must be compatible with what the template declares:
    // same revision, and at least the template's extensions enabled.
    // Anything less makes the evaluation passes below ambiguous (an
    // error could be a context artifact instead of a template defect),
    // which is why evaluation errors can be reported strictly.
    let template_profile = job_template.profile();
    // Unreachable until a second SpecificationRevision variant exists
    // (V2023_09 is the only one today, so both sides are always equal);
    // written now so revision coverage doesn't silently go missing when
    // one is added. No test can pin this error message until then.
    if ctx.profile.revision() != template_profile.revision() {
        return Err(ModelError::Compatibility(format!(
            "create_job requires a context matching the template's specification revision: \
             the template is {}, but the context is {}.",
            template_profile.revision(),
            ctx.profile.revision(),
        )));
    }
    // Iterate the template's own extension set rather than
    // `ModelExtension::ALL`: exhaustive by construction, so a future
    // variant omitted from `ALL` cannot escape the check. Sorted so the
    // message is deterministic (the set is a HashSet).
    let mut missing: Vec<&str> = template_profile
        .extensions()
        .iter()
        .filter(|e| !ctx.profile.has_extension(**e))
        .map(|e| e.as_str())
        .collect();
    missing.sort_unstable();
    if !missing.is_empty() {
        return Err(ModelError::Compatibility(format!(
            "create_job requires a context enabling every extension the template declares: \
             missing {}. An application that does not support an extension should reject \
             the template at decode via its supported-extensions list.",
            missing.join(", "),
        )));
    }

    // Validate parameter values against template constraints.
    let merged = parameters::merge_job_parameter_definitions(job_template, &[])?;
    for param in &merged {
        if let Some(jpv) = job_parameter_values.get(&param.name) {
            param.check_constraints(&jpv.value)?;
        }
    }

    let mut symtab = build_symbol_table(job_parameter_values)?;

    let has_expr = ctx
        .profile
        .has_extension(crate::types::ModelExtension::Expr);
    let limits = EffectiveLimits::from_context(ctx);
    let budgets = EvalBudgets::from_ctx(ctx);

    // Required string field (§1.1.1): a single whole-field expression
    // resolves with target type `string` (Expression Language §1.3.2), so
    // `null` and list values are errors rather than display renderings.
    let job_name = job_template
        .name
        .resolve_with(
            &symtab,
            &budgets
                .fs_options()
                .with_target_type(&openjd_expr::ExprType::STRING),
        )
        .map(|v| match v {
            openjd_expr::ExprValue::String(s) => s,
            other => other.to_display_string(),
        })
        .map_err(|e| ModelError::FormatStringError {
            message: format!("Failed to resolve job name: {e}"),
            input: Some(job_template.name.raw().to_string()),
            start: None,
            end: None,
        })?;

    if job_name.chars().count() > limits.max_job_name_len {
        return Err(ModelError::DecodeValidation(format!(
            "Job name exceeds maximum length of {} characters (got {})",
            limits.max_job_name_len,
            job_name.chars().count()
        )));
    }
    // §1.1.1 minimum length 1: the raw-text pass rejects an empty literal,
    // but an interpolated name is only known here.
    if job_name.is_empty() {
        return Err(ModelError::DecodeValidation(
            "Job name must not resolve to an empty string".to_string(),
        ));
    }
    // §1.1.1 forbids control (Cc) characters in the resolved name. The
    // raw-text pass rejects control characters in the literal text, and
    // the format-string pass checks the resolved value when the name is
    // fully static — but a control character introduced by an
    // interpolated value is only known here.
    if job_name.chars().any(char::is_control) {
        return Err(ModelError::DecodeValidation(
            "Job name must not contain control characters".to_string(),
        ));
    }

    if has_expr {
        symtab.set("Job.Name", openjd_expr::ExprValue::String(job_name.clone()))?;
    }

    let parameters: IndexMap<String, job::JobParameter> = job_parameter_values
        .iter()
        .map(|(name, pv)| {
            (
                name.clone(),
                job::JobParameter {
                    name: name.clone(),
                    param_type: pv.param_type,
                    value: pv.value.clone(),
                },
            )
        })
        .collect();

    // Re-run the carried-forward resolved-value checks (and evaluate the
    // `let` bindings feeding them) against check symbol tables where job
    // parameters are bound to real values. A violation template
    // validation could only lower-bound is decidable here — fail at
    // submission, not on the worker.
    //
    // Every check failure across the whole template accumulates into
    // this one collection and is reported together as a single
    // `ModelValidation` error, in pass 8's order: `jobEnvironments`
    // first, then each step in turn (step `let`, script, step
    // environments). Resolution failures that leave a step
    // uninstantiable (host requirements, parameter space) are not
    // checks: they abort immediately, reported alone.
    let mut check_errors = crate::error::ValidationErrors::default();
    // Keyed on the template's own extensions, not the context's: a context
    // may enable more extensions than the template lists, and a template
    // that does not list CONTROL_FLOW keeps today's behavior (RFC 0010
    // Technical Requirement 1), including its order of evaluation and its
    // range rules.
    let control_flow = template_profile.has_extension(crate::types::ModelExtension::ControlFlow);
    // CONTROL_FLOW: a Job Environment's `enableIf` is decided here (it may
    // not read `Steps.*`); a disabled one is not entered, so its fields are
    // not checked.
    let mut job_env_enabled = Vec::new();
    if let Some(envs) = &job_template.job_environments {
        for (i, env) in envs.iter().enumerate() {
            let env_path = [
                crate::error::PathElement::Field("jobEnvironments".to_string()),
                crate::error::PathElement::Index(i),
            ];
            let enabled = binding::evaluate_enable_if(
                env.enable_if.as_ref(),
                &symtab,
                budgets,
                &crate::error::path_field(&env_path, "enableIf"),
            )?;
            job_env_enabled.push(enabled);
            if !enabled {
                continue;
            }
            let env_symtab = instantiate::build_env_check_symtab(
                env,
                &symtab,
                None,
                has_expr,
                ctx,
                &env_path,
                &mut check_errors,
            )?;
            crate::template::validate_v2023_09::format_strings::check_carried_forward_environment(
                env,
                &env_symtab,
                &symtab,
                ctx,
                limits.max_env_var_value_len,
                &env_path,
                &mut check_errors,
            );
        }
    }

    let steps: Vec<job::Step> = if control_flow {
        instantiate_steps_with_binding(
            job_template,
            &symtab,
            has_expr,
            &limits,
            ctx,
            budgets,
            &mut check_errors,
        )?
    } else {
        let mut steps = Vec::with_capacity(job_template.steps.len());
        for (step_index, st) in job_template.steps.iter().enumerate() {
            steps.push(instantiate::instantiate_step(
                st,
                &symtab,
                has_expr,
                &limits,
                ctx,
                step_index,
                budgets,
                &mut check_errors,
            )?);
        }
        check_errors.into_result("JobTemplate")?;
        // `instantiate_step` only declines to build a step after recording
        // the failure in `check_errors`, so every step is present here.
        let steps: Vec<job::Step> = steps.into_iter().collect::<Option<_>>().ok_or_else(|| {
            ModelError::DecodeValidation(
                "internal error: a step was skipped without a recorded error".to_string(),
            )
        })?;
        // Caller-imposed task count limits, per Step and across all steps.
        let limits_set = ctx.caller_limits.max_step_task_count.is_some()
            || ctx.caller_limits.max_task_count.is_some();
        if limits_set {
            let mut total: u64 = 0;
            for step in &steps {
                let step_tasks = job::binding::task_count(step.parameter_space.as_ref())?;
                binding::check_step_task_count(ctx, &step.name, step_tasks)?;
                total = total.saturating_add(step_tasks);
            }
            if let Some(max_task_count) = ctx.caller_limits.max_task_count {
                if total > max_task_count {
                    return Err(ModelError::ModelValidation(
                        crate::error::ValidationErrors::single(format!(
                            "Total task count ({total}) exceeds caller limit of {max_task_count}."
                        )),
                    ));
                }
            }
        }
        steps
    };

    // CONTROL_FLOW (RFC 0010 §Job outputs item 1): every job output whose
    // inputs are concrete now is evaluated, and a failure fails Job
    // creation, as an invalid job parameter value does.
    let outputs = job_outputs::evaluate_at_creation(job_template, &symtab, &steps, budgets)?;

    let job_environments = job_template.job_environments.as_ref().map(|envs| {
        envs.iter()
            .enumerate()
            .map(|(i, e)| job::Environment {
                enabled: job_env_enabled.get(i).copied().unwrap_or(true),
                ..instantiate::convert_environment_with_symtab(e, Some(&symtab))
            })
            .collect()
    });

    // job_template.extensions is `Option<Vec<ExtensionName>>` where every
    // entry has already passed decode-time recognition: any string that
    // isn't a valid ModelExtension would have been rejected in parse.rs.
    // Map into the typed Job.extensions form; in the unexpected case that
    // an entry doesn't parse (e.g. directly-constructed JobTemplate
    // bypassing decode), skip it — the decode path is the single source
    // of truth for extension recognition.
    let extensions = job_template.extensions.as_ref().map(|exts| {
        exts.iter()
            .filter_map(|e| std::str::FromStr::from_str(e.as_str()).ok())
            .collect()
    });

    // Caller-imposed step script size limit (JSON-encoded bytes)
    if let Some(max) = ctx.caller_limits.max_step_script_size {
        for step in &steps {
            let size = serde_json::to_string(&step.script)
                .map(|s| s.len())
                .unwrap_or(0);
            if size > max {
                return Err(ModelError::ModelValidation(
                    crate::error::ValidationErrors::single(format!(
                        "Step '{}' script size ({size} bytes) exceeds caller limit of {max} bytes.",
                        step.name
                    )),
                ));
            }
        }
    }

    // Caller-imposed environment size limit (JSON-encoded bytes)
    if let Some(max) = ctx.caller_limits.max_environment_size {
        let all_envs = steps
            .iter()
            .flat_map(|s| s.step_environments.iter().flatten())
            .chain(job_environments.iter().flatten());
        for env in all_envs {
            let size = serde_json::to_string(env).map(|s| s.len()).unwrap_or(0);
            if size > max {
                return Err(ModelError::ModelValidation(
                    crate::error::ValidationErrors::single(format!(
                        "Environment '{}' size ({size} bytes) exceeds caller limit of {max} bytes.",
                        env.name
                    )),
                ));
            }
        }
    }

    Ok(job::Job {
        name: job_name,
        description: job_template.description.as_ref().map(|d| d.0.clone()),
        extensions,
        parameters,
        steps,
        job_environments,
        outputs,
    })
}

/// `create_job`'s Steps under `CONTROL_FLOW` (RFC 0010 §When groups are
/// evaluated): every Step is bound as far as the values concrete at job
/// creation allow, in dependency order so that a producer skipped or with
/// known Tasks makes its symbols concrete for the Steps that read them.
///
/// A group failure that is a step `let` error or a remaining-group check
/// error is collected into `check_errors` with the other Steps' errors, in
/// template order; any other group failure aborts at once. The Task-count
/// limits are checked as each Step's Tasks become known.
#[allow(clippy::too_many_arguments)]
fn instantiate_steps_with_binding(
    job_template: &JobTemplate,
    symtab: &openjd_expr::symbol_table::SymbolTable,
    has_expr: bool,
    limits: &EffectiveLimits,
    ctx: &ValidationContext,
    budgets: EvalBudgets,
    check_errors: &mut crate::error::ValidationErrors,
) -> Result<Vec<job::Step>, ModelError> {
    let n = job_template.steps.len();
    // Validation rejects a cycle; template order is a safe fallback.
    let order = crate::template::validate_v2023_09::step_dependency_order(&job_template.steps)
        .unwrap_or_else(|| (0..n).collect());
    let cx = binding::BindContext {
        jt: job_template,
        ctx,
        symtab,
        has_expr,
        limits,
        budgets,
    };
    let mut built: Vec<Option<job::Step>> = vec![None; n];
    let mut step_errors: Vec<crate::error::ValidationErrors> =
        (0..n).map(|_| Default::default()).collect();
    let mut committed: u64 = 0;
    for i in order {
        let inputs = binding::creation_inputs(job_template, i, &built)?;
        match binding::bind_step(&cx, i, None, &inputs, committed) {
            Ok(step) => {
                if let Some(tasks) = step.tasks_known() {
                    committed = committed.saturating_add(tasks);
                }
                built[i] = Some(step);
            }
            Err(binding::GroupFailure {
                failure: binding::Failure::Collected(errors),
                ..
            }) => step_errors[i] = errors,
            Err(binding::GroupFailure {
                failure: binding::Failure::Abort(e),
                ..
            }) => return Err(e),
        }
    }
    for errors in step_errors {
        check_errors.errors.extend(errors.errors);
    }
    std::mem::take(check_errors).into_result("JobTemplate")?;
    built.into_iter().collect::<Option<_>>().ok_or_else(|| {
        ModelError::DecodeValidation(
            "internal error: a step was skipped without a recorded error".to_string(),
        )
    })
}
