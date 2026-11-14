// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Job creation: parameter preprocessing and template instantiation.
//!
//! Mirrors Python `_create_job.py` and `_merge_job_parameter.py`.

mod binding;
mod env_params;
mod external;
mod host_requirements;
mod instantiate;
mod job_outputs;
mod loops;
pub mod parameters;
mod ranges;
mod staged;

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
pub use loops::{
    advance_body_step_binding, advance_loop_binding, end_without_iterations,
    evaluate_iteration_boundary, start_iteration,
};
// Re-exports — preserve the existing public API
pub use external::{
    check_environment_templates, AttachedEnvironmentTemplate, CreatedJob, DiscardedHostRequirement,
    EnvironmentScope, EnvironmentStatus, HostRequirementKind, OptionalEntryNotFound,
    StepDisabledEnvironments, StepEnvironmentStatus, StepEnvironmentStatuses, SubmissionWarning,
    SupersedingEnvironment,
};
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

/// Create an instantiated Job from a validated JobTemplate and preprocessed
/// parameter values, with no Environment Templates attached.
///
/// This is [`create_job_with_environment_templates`] with no attachments,
/// returning only the Job; see it for every rule. In particular, with
/// `ENV_PARAMS` an enabled `environment:` entry that names no
/// `jobEnvironments` entry is an error unless it has `required: false`,
/// and the error is reported for `JobTemplate`, at the Job Template's
/// paths, with every other error.
pub fn create_job(
    job_template: &JobTemplate,
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
) -> Result<job::Job, ModelError> {
    create_job_with_environment_templates(job_template, &[], job_parameter_values, ctx)
        .map(|created| created.job)
}

/// Create an instantiated Job from a validated JobTemplate, the
/// Environment Templates the scheduler attaches to its submission, in the
/// scheduler's order, and preprocessed parameter values (Template Schemas
/// §7.4 "Job creation").
///
/// Environment template parameters should already be merged into `job_parameter_values`
/// via [`preprocess_job_parameters`] before calling this function, with
/// the same Environment Templates.
///
/// Each value is re-checked against the Job Template's parameter
/// definitions ([`MergedParameterDefinition::check_constraints`]). A
/// RANGE_EXPR value is a `range_expr`, with no spelling as written, so for
/// a RANGE_EXPR Job Parameter in a Job Template that does not list
/// `ENV_PARAMS` the re-check enforces no `minLength` or `maxLength`: a
/// `range_expr` that did not come from [`preprocess_job_parameters`] is
/// accepted whatever its length (§2.10).
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
/// on this queue"). The caller limits also apply to every attached
/// document, each evaluated under its own profile.
///
/// When `ctx.caller_limits.max_task_count` is set, the total task count
/// across all steps is checked after parameter spaces are resolved. When
/// `max_step_task_count` is set, each Step's task count is checked.
///
/// Job creation takes the steps of Template Schemas §7.4.1 over the Job
/// Template and the attached Environment Templates together, in one pass,
/// whether or not the Job Template lists `ENV_PARAMS` (without it, steps
/// 1–5 file no error for a Job Template that template validation accepts,
/// and the attachments' errors are ordered the same way):
///
/// 1. `Job.Name` is resolved and, with `EXPR`, `Job.EnvNames` is bound to
///    the names of the attached Environments, in attachment order,
///    followed by the Job Template's `jobEnvironments`, whatever their
///    `activation` (§7.3.1; plan P2), before any TEMPLATE-scope expression,
///    including each Step's `let` bindings;
/// 2. each `environment:` entry's `enableIf` is evaluated; a disabled entry
///    supplies nothing, and what it means depends on the `activation` of
///    the Environment it names (§3.2 item 3.2);
/// 3. the entries that name no Job Environment, enabled or disabled, are
///    resolved against `attached` (§3.2.1; a disabled one that matches no
///    attachment has no effect);
/// 4. each Step's set of Environments is fixed: its Sessions would enter
///    every Environment with `activation: ALWAYS`, every `BY_DEFAULT` one
///    that no disabled entry of the Step declines, and every `ON_REQUEST`
///    one an enabled entry requests, and a disabled entry for an `ALWAYS`
///    one is an error naming the Step and the Environment;
/// 5. the Step gets its effective values for each Environment in its set
///    (an entry's undeclared key is an error here, constraint 5, except that
///    an attachment that declares no parameters ignores a `required: false`
///    entry's `parameters`, with a warning, §3.2.1 item 1), and constraint 7
///    is checked for every Environment in the set, whatever its own
///    `enableIf` or `exclusiveGroup`: the Step must supply each parameter
///    without a `default` (§3.2 constraint 7, §7.4.1 step 5);
/// 6. the Environments are taken in the Job's Environment order: one whose
///    `exclusiveGroup` (§4 item 7) an earlier one that remains in the Step's
///    set already has is superseded, which disables it for the Step, and its
///    own `enableIf` is not evaluated; for any other, its own `enableIf`
///    (§4 item 6) is evaluated with the Step's concrete values (§3.2.2 item
///    2), unless it reads a value that is missing, fails to resolve, or
///    does not convert, when it is not evaluated and does not disable the
///    Environment (that value is an error already). One that is `false` is
///    left out of the Step's set and values, or, for an `ALWAYS` one, is an
///    error naming the Step (§3.2.2 item 6), while one it does not disable
///    takes its group. Each Step Environment's own `enableIf` is decided
///    too, and a disabled one is left out of
///    [`job::Step::step_environments`];
/// 7. for the Environments that remain in each Step's set, each value is
///    checked against its definition, and so is the `WRAP_ACTIONS`
///    single-layer rule;
/// 8. each Environment's `hostRequirements` (`ENV_PARAMS`, §4 item 5) are
///    resolved for each Step whose Sessions enter it, with that Step's
///    effective values, and combined with the Step's own into the Step's
///    combined host requirements (§3.3.3), which replace
///    [`job::Step::host_requirements`]. A requirement that resolves to
///    nothing, of the Step or of an Environment, is discarded before
///    combining (§3.3) and listed in
///    [`CreatedJob::discarded_host_requirements`].
///
/// The combined Job lists the attached Environments before the Job
/// Template's own, in the Job's Environment order (a Session enters them
/// first; Template Schemas §1.1 item 7), and records on each Step
/// the Job Environments its Sessions enter ([`job::Step::environments`]);
/// [`CreatedJob::disabled_environments`] lists, per Step, the ones an
/// `enableIf` disabled or a group superseded, and
/// [`CreatedJob::environment_statuses`] every Environment each Step
/// considered with whether its Sessions enter it or which decision left it
/// out (declined at step 4, superseded or disabled at step 6), and its
/// `required: false` entries that named nothing.
/// `job.extensions` is left as the Job Template declared it: an extension
/// applies to the document that lists it (§1.2 item 3). Every document
/// that holds an Environment parameter value lists `ENV_PARAMS` and so
/// `EXPR` (§1.1 item 3), so a single `{{ }}` expression value is typed
/// whatever `ctx` enables.
///
/// Every check failure is reported in one `ModelError::ModelValidation`. Resolving a step's own host
/// requirements or parameter space aborts job creation immediately instead.
/// A step-level `let` binding that fails leaves its step uninstantiable:
/// the other steps get only the checks made before the attachments are
/// applied (§7.4.1 step 2, steps 4 and 5 for their Job Environment
/// entries, step 6 for their Step Environments), the attachments are not
/// applied, and
/// the errors found so far are reported. The caller limits abort job
/// creation, but only when no other error was found, after the submission
/// stage. These aborts keep their own forms, which the shape rule below
/// does not cover. The errors are in this order:
///
/// - first the checks job creation makes in the template-validation style:
///   the resolved-value re-checks on the Job Environments' carried-forward
///   fields, then the Job Environments' literal `default`s, then each
///   Step's carried-forward fields (and the `let` bindings feeding them),
///   then the attachments' literal `default`s;
/// - then by §7.4.1 step, then by Step, then by the Environment's place in
///   the Job's Environment order (an entry's error by its place in the
///   Step's `dependencies`). An error that names several Steps (a failed
///   `enableIf`, an Environment's resolved `hostRequirements`) is filed under
///   the first; one about no Step (an unreachable attachment) after the
///   Steps' errors of its step.
///
/// A Step with an error from steps 1–4 has no fixed set of Environments, so
/// steps 5–8 are not taken for it, and none of its errors from them are
/// reported; the other Steps are checked in full. So a Step's value and
/// constraint 7 errors appear only once its step 1–4 errors are fixed.
///
/// The error is shaped by the inputs. Without attached Environment Templates it is for `JobTemplate`,
/// at paths relative to the Job Template, as job creation's other errors
/// are. With them it is for
/// `Submission`, and every path starts with its document (`JobTemplate`,
/// `EnvironmentTemplate[i]`, or the attachment's label). The warnings
/// ([`CreatedJob::warnings`]) have the same paths.
///
/// For a Job Template without `ENV_PARAMS` that template validation
/// accepts, with no attachments, the result is what it was before the
/// one-pass order. With attachments, its own
/// errors and theirs are now reported together, for `Submission`, where
/// before its own errors were reported alone, for `JobTemplate`, and the
/// attachments were applied only when it had none; the attachments' errors
/// are in §7.4.1 order rather than the order they were found; and a caller
/// limit is reported only when the attachments add no error.
///
/// With `CONTROL_FLOW` (RFC 0011 §When groups are evaluated), each Step's
/// `outputVariables` are copied onto [`job::Step::output_variables`], and
/// each Step is bound as far as the values concrete at job creation allow:
/// its `enableIf` is decided if its inputs are concrete (a `false` Step is
/// [`StepBinding::Skipped`](job::StepBinding::Skipped)), then its
/// Environment selection (RFC 0011 `<Environment>` item 3: the work of
/// §7.4.1 above for that Step) is made, and then every other
/// materialization group whose inputs are concrete is evaluated. A Step all
/// of whose groups evaluate is [`StepBinding::Bound`](job::StepBinding::Bound);
/// any other is [`StepBinding::PendingInputs`](job::StepBinding::PendingInputs),
/// for [`advance_step_binding`] to finish. A skipped Step has no Environment
/// selection, and one whose `enableIf` is decided later makes it then, so
/// the §7.4.1 errors of steps 2 and 4 to 8 for such a Step are not reported
/// here. Steps are bound in dependency order, because a Step skipped here
/// makes its `null` symbols concrete and a Step whose Tasks are known makes
/// its `TaskParam` symbols concrete. `max_task_count` counts the Steps whose
/// Tasks are known at creation.
///
/// Each `jobOutputDefinitions` entry becomes a [`job::JobOutput`] in
/// [`job::Job::outputs`]. One whose `value` reads no `Steps.*` symbol that is
/// not concrete at creation (it reads only job parameters, the `null`s of a
/// Step skipped here, or `TaskParam` of a Step whose Tasks are known here,
/// though it may still be pending) is evaluated after the Steps, and its
/// [`evaluated`](job::JobOutput::evaluated) set; the rest are left for
/// [`evaluate_job_output`]. A job output that fails to evaluate, convert, or
/// meet its constraints fails job creation, every such failure reported
/// together.
///
/// Each loop (`CONTROL_FLOW`, RFC 0013) becomes a [`job::Loop`] in
/// [`job::Job::loops`], bound as a Step is (RFC 0013 §Loop binding): its
/// `enableIf` is decided if its inputs are concrete, then its `maxIterations`
/// is evaluated, then its start group (each `initial`, the size limit, and
/// the first `while`) if its inputs are concrete. A failure in any of them
/// aborts job creation. Nothing of a loop's body Steps is evaluated, and the
/// `minLength` of its `APPEND` outputs is never checked here. Steps and loops
/// are bound in dependency order, so a loop skipped here makes its `null`s
/// concrete for the Steps and job outputs that read it.
pub fn create_job_with_environment_templates(
    job_template: &JobTemplate,
    attached: &[AttachedEnvironmentTemplate<'_>],
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
) -> Result<CreatedJob, ModelError> {
    // §7.4.1: the attached Environments are known first, so `Job.EnvNames`
    // is concrete before any TEMPLATE-scope expression.
    let env_names: Vec<String> = attached
        .iter()
        .map(|a| a.template.environment().name.clone())
        .chain(
            job_template
                .job_environments
                .iter()
                .flatten()
                .map(|e| e.name.clone()),
        )
        .collect();
    let body = create_job_from_template(
        job_template,
        job_parameter_values,
        ctx,
        &env_names,
        attached.len(),
    )?;
    if job_template
        .profile()
        .has_extension(crate::types::ModelExtension::ControlFlow)
    {
        return control_flow_job(
            job_template,
            body,
            &env_names,
            attached,
            job_parameter_values,
            ctx,
        );
    }
    external::apply_environment_templates(
        job_template,
        body,
        &env_names,
        attached,
        job_parameter_values,
        ctx,
    )
}

/// The rest of [`create_job_with_environment_templates`] for a Job Template
/// that lists `CONTROL_FLOW` (RFC 0011 `<Environment>` items 3 to 6): make
/// every Step's Environment selection with the attachments, then bind the
/// Steps and loops, applying each Step's selection once its `enableIf` is
/// `true`. A Step whose `enableIf` is decided `true` here has its
/// selection's errors reported with job creation's others; a Step that is
/// skipped here never makes it; and any other Step, and every loop body
/// Step, keeps its selection, errors included, for step input binding.
fn control_flow_job(
    job_template: &JobTemplate,
    body: Body<'_>,
    env_names: &[String],
    attached: &[AttachedEnvironmentTemplate<'_>],
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
) -> Result<CreatedJob, ModelError> {
    let Body {
        mut job,
        selections,
        job_envs,
        mut check_errors,
        staged,
        limits: _,
        symtab,
    } = body;
    let has_expr = ctx
        .profile
        .has_extension(crate::types::ModelExtension::Expr);
    let limits = EffectiveLimits::from_context(ctx);
    let budgets = EvalBudgets::from_ctx(ctx);
    let mut subjects = std::mem::take(&mut job.steps);
    let paths: Vec<Vec<crate::error::PathElement>> =
        job_template.step_templates().map(|st| st.path()).collect();
    let mut selected = external::select_environments(
        job,
        &mut subjects,
        &paths,
        selections,
        &job_envs,
        staged,
        job_template,
        env_names,
        attached,
        job_parameter_values,
        ctx,
    )?;
    // The entered Environments' requirements, resolved for each Step and
    // combined with each other (RFC 0011 `<Environment>` item 5); the
    // Steps' own are combined with them in host selection.
    let mut kept = Vec::new();
    let mut discarded: Vec<Vec<DiscardedHostRequirement>> = selected
        .selections
        .iter()
        .map(|s| s.discarded_host_requirements.clone())
        .collect();
    {
        let sources = selected.sources(job_template, attached, ctx);
        let mut host_staged = staged::StagedErrors::default();
        host_requirements::combine_step_host_requirements(
            &mut subjects,
            &paths,
            &selected.job.name,
            &sources,
            &selected.entered_per_step,
            &selected.job_path,
            &selected.default_symtab,
            &selected.env_names,
            budgets,
            &mut discarded,
            &mut host_staged,
            Some(&mut kept),
        );
        drop(sources);
        selected.staged.absorb(host_staged, &[]);
    }
    let wrap_layers = selected.wrap_layers(&subjects);
    let model = if attached.is_empty() {
        "JobTemplate"
    } else {
        "Submission"
    };
    let mut environment_selections: Vec<job::EnvironmentSelection> = subjects
        .iter()
        .zip(kept.into_iter().chain(std::iter::repeat_with(Vec::new)))
        .zip(wrap_layers)
        .map(|((step, host), wrap)| job::EnvironmentSelection {
            environments: step.environments.clone(),
            environment_parameters: step.environment_parameters.clone(),
            host_requirements: host,
            wrap_layers: wrap,
            errors: Vec::new(),
            model: model.to_string(),
        })
        .collect();

    let mut built = instantiate_steps_with_binding(
        job_template,
        &symtab,
        has_expr,
        &limits,
        ctx,
        budgets,
        &environment_selections,
        &mut check_errors,
    )?;
    // For each subject, the entry of its Step when it is a top-level Step.
    let top_level: Vec<Option<usize>> = job_template
        .step_templates()
        .map(|st| st.body_index.is_none().then_some(st.entry_index))
        .collect();
    // Which subjects' selections were applied here: the top-level Steps
    // whose `enableIf` was decided `true`, including one whose group failed
    // later (its error is collected). Every other subject's errors are its
    // own, for step input binding, and a skipped Step's are dropped.
    let mut applied_at_creation = vec![false; top_level.len()];
    for (i, top) in top_level.iter().enumerate() {
        let applied = top.is_some_and(|e| match &built[e] {
            Some(binding::Built::Step(step)) => match &step.binding {
                job::StepBinding::Bound => true,
                job::StepBinding::PendingInputs(p) => p.enablement.is_known(),
                job::StepBinding::Skipped => false,
            },
            _ => true,
        });
        if applied {
            applied_at_creation[i] = true;
            continue;
        }
        let errors: Vec<job::SelectionError> = selected
            .staged
            .take_step(i)
            .into_iter()
            .chain(selected.body_staged.take_step(i))
            .map(|e| job::SelectionError {
                path: e.path,
                message: e.message,
            })
            .collect();
        environment_selections[i].errors = errors;
        if let Some(Some(binding::Built::Step(step))) = top.map(|e| &mut built[e]) {
            if let job::StepBinding::PendingInputs(p) = &mut step.binding {
                // A selection with nothing to apply was not stored, but one
                // with errors is.
                if !environment_selections[i].is_empty() {
                    p.environment_selection = Some(environment_selections[i].clone());
                }
            }
        }
    }
    let external::Selected {
        mut job,
        environments,
        environment_documents,
        staged,
        body_staged,
        warnings,
        disabled_environments,
        environment_statuses,
        attached_count,
        ..
    } = selected;
    finish_errors(check_errors, body_staged, staged, attached_count > 0)?;
    let mut steps = Vec::new();
    let mut loops = Vec::new();
    for entry in built {
        match entry {
            Some(binding::Built::Step(step)) => steps.push(step),
            Some(binding::Built::Loop(job_loop)) => loops.push(job_loop),
            None => {
                return Err(ModelError::DecodeValidation(
                    "internal error: a step was skipped without a recorded error".to_string(),
                ))
            }
        }
    }
    // Each loop's body Steps' selections, for their instances.
    for job_loop in &mut loops {
        let entry = job_loop.entry_index;
        let selections: Vec<job::EnvironmentSelection> = job_template
            .step_templates()
            .zip(&environment_selections)
            .filter(|(st, _)| st.entry_index == entry && st.body_index.is_some())
            .map(|(_, sel)| sel.clone())
            .collect();
        // None when no body Step has one to apply.
        if selections.iter().any(|s| !s.is_empty()) {
            job_loop.body_environment_selections = selections;
        }
    }

    // CONTROL_FLOW (RFC 0011 §Job outputs item 1): every job output whose
    // inputs are concrete now is evaluated, and a failure fails Job
    // creation, as an invalid job parameter value does.
    let outputs =
        job_outputs::evaluate_at_creation(job_template, &symtab, &steps, &loops, budgets)?;

    check_caller_limits(&steps, Some(&environments), false, ctx)?;
    // The per-Step lists of `CreatedJob` are for the top-level Steps, and
    // are empty for a Step whose Environment selection is not made here: a
    // skipped Step never makes one, and a pending one makes it at step
    // input binding.
    fn pick<T: Default>(v: Vec<T>, top_level: &[Option<usize>], applied: &[bool]) -> Vec<T> {
        v.into_iter()
            .zip(top_level)
            .zip(applied)
            .filter(|((_, top), _)| top.is_some())
            .map(|((x, _), applied)| if *applied { x } else { T::default() })
            .collect()
    }
    job.steps = steps;
    job.loops = loops;
    job.outputs = outputs;
    job.job_environments = (!environments.is_empty()).then_some(environments);
    Ok(CreatedJob {
        job,
        environment_documents,
        warnings,
        disabled_environments: pick(disabled_environments, &top_level, &applied_at_creation),
        environment_statuses: pick(environment_statuses, &top_level, &applied_at_creation),
        discarded_host_requirements: pick(discarded, &top_level, &applied_at_creation),
    })
}

/// What job creation from the Job Template alone gives the submission
/// stage.
pub(super) struct Body<'a> {
    /// The Job, without the attached Environments and with no Step's
    /// `environments` set.
    pub(super) job: job::Job,
    /// For each Step, what its entries decided.
    pub(super) selections: Vec<env_params::StepSelection<'a>>,
    /// The Job Environments, set up for the submission stage to decide.
    pub(super) job_envs: env_params::JobEnvironments<'a>,
    /// The template-validation-style errors, at paths relative to the Job
    /// Template ([`staged::Stage::Validation`]).
    pub(super) check_errors: crate::error::ValidationErrors,
    /// The errors of §7.4.1's steps found so far, at paths relative to the
    /// Job Template.
    pub(super) staged: staged::StagedErrors,
    /// The caller limits' result, which the submission stage reports only
    /// when it and job creation proper found no error.
    pub(super) limits: Result<(), ModelError>,
    /// The job symbol table: `Param.*`, `RawParam.*`, and with `EXPR`
    /// `Job.Name` and `Job.EnvNames`.
    pub(super) symtab: openjd_expr::symbol_table::SymbolTable,
}

/// Report every error of one job creation in §7.4.1 order: the
/// template-validation-style `check_errors`, then `body`'s and
/// `submission`'s by step, Step and position (see [`staged`]), leaving out
/// steps 5–8 of every Step with an error from steps 1–4.
///
/// The error is shaped by the inputs. Without attached Environment
/// Templates it is for `JobTemplate`, at paths relative to the Job
/// Template. With them it is for `Submission`, and every path starts with
/// its document: `check_errors` and `body` get `JobTemplate`, and
/// `submission`'s already have it.
pub(super) fn finish_errors(
    check_errors: crate::error::ValidationErrors,
    body: staged::StagedErrors,
    submission: staged::StagedErrors,
    attached: bool,
) -> Result<(), ModelError> {
    let prefix: Vec<crate::error::PathElement> = if attached {
        vec![crate::error::PathElement::Field("JobTemplate".to_string())]
    } else {
        Vec::new()
    };
    let mut all = staged::StagedErrors::default();
    all.at(staged::Stage::Validation, 0, 0).errors.extend(
        check_errors
            .errors
            .into_iter()
            .map(|e| staged::prefixed(&prefix, e)),
    );
    all.absorb(body, &prefix);
    all.absorb(submission, &[]);
    all.finish(if attached {
        "Submission"
    } else {
        "JobTemplate"
    })
}

/// Job creation from the Job Template alone (§7.4.1 step 2, steps
/// 4 and 5 for the Job Environments, and step 6 for the Step Environments,
/// of
/// [`create_job_with_environment_templates`]): the Job, without the
/// attached Environments and with no Step's `environments` set, for each
/// Step what the submission stage needs, and the errors found so far,
/// which the submission stage reports with its own. `env_names` is the
/// value of `Job.EnvNames`; `attached_count` is the number of attached
/// Environments, which come first in the Job's Environment order.
fn create_job_from_template<'a>(
    job_template: &'a JobTemplate,
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
    env_names: &[String],
    attached_count: usize,
) -> Result<Body<'a>, ModelError> {
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

    // Validate parameter values against template constraints. A RANGE_EXPR
    // value is a `range_expr`, whose spelling as submitted is not known
    // here, so only its canonical form is checked, and only in a template
    // that lists ENV_PARAMS (§2.10); `preprocess_job_parameters` checks
    // the value as written.
    let merged = parameters::merge_job_parameter_definitions(job_template, &[])?;
    for param in &merged {
        if let Some(jpv) = job_parameter_values.get(&param.name) {
            param.check_constraints(&jpv.value)?;
        }
    }

    let mut symtab = build_symbol_table(job_parameter_values)?;
    // ENV_PARAMS `default`s resolve in the Job Parameters and `Job.Name`,
    // without `Job.EnvNames` (§4.5 item 1, §7.3.2), so keep the table before
    // the `Job.*` symbols are added, and add `Job.Name` once it is resolved.
    let mut default_symtab = symtab.clone();

    let has_expr = ctx
        .profile
        .has_extension(crate::types::ModelExtension::Expr);
    // Whether the template itself enables EXPR, which decides how a numeric
    // range list element resolves (`TaskParameterType::range_list_item_target`).
    // Taken from the template, as template validation takes it, not from
    // `ctx`, which may enable extensions the template does not declare: a
    // template's element must resolve the way it was validated.
    let template_expr = template_profile.has_extension(crate::types::ModelExtension::Expr);
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
            source: Some(e),
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

    // Only a document that lists ENV_PARAMS, and so EXPR, has `default`s.
    default_symtab.set("Job.Name", openjd_expr::ExprValue::String(job_name.clone()))?;
    if has_expr {
        symtab.set("Job.Name", openjd_expr::ExprValue::String(job_name.clone()))?;
        // §7.3.1, §7.4.1: concrete before any TEMPLATE-scope expression,
        // including a Step's `let` bindings. Template validation rejects a
        // reference without ENV_PARAMS, and `resolved_symtab`s keep only
        // the symbols a field references, so binding it with EXPR alone
        // adds nothing to a Job that does not use it.
        symtab.set("Job.EnvNames", external::env_names_value(env_names))?;
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
    // this one collection, in pass 8's order: `jobEnvironments` first,
    // then each step in turn (step `let`, script, step environments). It
    // is reported first, with the §7.4.1 steps' errors (`finish_errors`).
    // Resolution failures that leave a step uninstantiable (host
    // requirements, parameter space) are not checks: they abort
    // immediately, reported alone.
    let mut check_errors = crate::error::ValidationErrors::default();
    // Keyed on the template's own extensions, not the context's: a context
    // may enable more extensions than the template lists, and a template
    // that does not list CONTROL_FLOW keeps today's behavior (RFC 0011
    // Technical Requirement 1), including its order of evaluation and its
    // range rules.
    let control_flow = template_profile.has_extension(crate::types::ModelExtension::ControlFlow);
    if let Some(envs) = &job_template.job_environments {
        for (i, env) in envs.iter().enumerate() {
            let env_path = [
                crate::error::PathElement::Field("jobEnvironments".to_string()),
                crate::error::PathElement::Index(i),
            ];
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

    // ENV_PARAMS (§3.2.2): each Step's effective values for the Job
    // Environments that declare parameters. Literal `default`s are checked
    // here, before any Step, since one that fails is an error even when no
    // Step takes it (§4.5 item 2).
    let env_policy = env_params::ResolvePolicy::new(ctx);
    let job_envs = env_params::JobEnvironments::new(
        job_template,
        default_symtab,
        symtab.clone(),
        attached_count,
        env_policy,
        &mut check_errors,
    );
    let mut staged = staged::StagedErrors::default();

    let lists_env_params = job_template
        .profile()
        .has_extension(crate::types::ModelExtension::EnvParams);
    let (steps, selections) = if control_flow {
        // CONTROL_FLOW: every Step, top-level or in a loop's body, makes
        // an Environment selection, worked out here with what job creation
        // knows (RFC 0011 `<Environment>` items 2 and 3); the Steps are
        // bound later, by `control_flow_job`, which applies each one's
        // selection once its `enableIf` is `true`.
        let mut shells = Vec::new();
        let mut selections = Vec::new();
        for (i, st) in job_template.step_templates().enumerate() {
            shells.push(instantiate::step_shell(st.step)?);
            let entry_symtab = binding::creation_scope_symtab(st.step, &symtab, has_expr, ctx)?;
            selections.push(
                env_params::resolve_step_entries(
                    st.step,
                    i,
                    &st.path(),
                    &entry_symtab,
                    &job_envs,
                    env_policy,
                    false,
                    &mut staged,
                )
                .selection,
            );
        }
        (shells, selections)
    } else {
        let mut steps = Vec::with_capacity(job_template.steps.len());
        // Without CONTROL_FLOW a template has no loops (validation), so these
        // are all the entries.
        for (step_index, st) in job_template.top_level_steps().enumerate() {
            steps.push(instantiate::instantiate_step(
                st,
                &symtab,
                has_expr,
                template_expr,
                &limits,
                ctx,
                step_index,
                budgets,
                &job_envs,
                lists_env_params,
                &mut check_errors,
                &mut staged,
            )?);
        }
        // `instantiate_step` only declines to build a step after recording
        // the failure in `check_errors`. Such a Step cannot be instantiated, so
        // the submission stage does not run, and the errors so far are
        // reported, in the shape the inputs give (see `finish_errors`).
        let Some(built) = steps.into_iter().collect::<Option<Vec<_>>>() else {
            finish_errors(
                check_errors,
                staged,
                staged::StagedErrors::default(),
                attached_count > 0,
            )?;
            return Err(ModelError::DecodeValidation(
                "internal error: a step was skipped without a recorded error".to_string(),
            ));
        };
        built.into_iter().unzip()
    };
    let job_environments: Option<Vec<job::Environment>> =
        job_template.job_environments.as_ref().map(|envs| {
            envs.iter()
                .map(|e| instantiate::convert_environment_with_symtab(e, Some(&symtab)))
                .collect()
        });
    // The caller limits abort job creation alone, and only when no other
    // error was found. The submission stage finds more errors, so it checks
    // the limits after them. With CONTROL_FLOW they are checked once the
    // Steps are bound.
    let limits = if control_flow {
        Ok(())
    } else {
        check_caller_limits(&steps, job_environments.as_deref(), true, ctx)
    };

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

    Ok(Body {
        job: job::Job {
            name: job_name,
            description: job_template.description.as_ref().map(|d| d.0.clone()),
            extensions,
            parameters,
            steps,
            loops: Vec::new(),
            job_environments,
            outputs: None,
        },
        selections,
        job_envs,
        check_errors,
        staged,
        limits,
        symtab,
    })
}

/// The caller limits (`ctx.caller_limits`): each Step's task count and the
/// total across all steps (when `check_task_count`; with `CONTROL_FLOW`
/// they are checked as each Step's Tasks become known), each step script's
/// size, and each Environment's size
/// (JSON-encoded bytes), every Step's Step Environments before the Job
/// Environments. The first one exceeded is the error; a value equal to its
/// limit is within it.
fn check_caller_limits(
    steps: &[job::Step],
    job_environments: Option<&[job::Environment]>,
    check_task_count: bool,
    ctx: &ValidationContext,
) -> Result<(), ModelError> {
    let limits_set = ctx.caller_limits.max_task_count.is_some()
        || ctx.caller_limits.max_step_task_count.is_some();
    if check_task_count && limits_set {
        // u128: each step's count is a u64, so the sum cannot overflow, and
        // the total is compared and reported exactly.
        let mut total: u128 = 0;
        for step in steps {
            let step_tasks = step
                .parameter_space
                .as_ref()
                .map(|ps| {
                    crate::job::step_param_space::StepParameterSpaceIterator::new_with_chunk_override(ps, Some(1))
                        // `len` is an exact u64 on every target.
                        .map(|iter| iter.len())
                })
                .transpose()?
                .unwrap_or(1); // Steps without a parameter space have 1 task
            binding::check_step_task_count(ctx, &step.name, step_tasks)?;
            total += u128::from(step_tasks);
        }
        if let Some(max_task_count) = ctx
            .caller_limits
            .max_task_count
            .filter(|max| total > u128::from(*max))
        {
            return Err(ModelError::ModelValidation(
                crate::error::ValidationErrors::single(format!(
                    "Total task count ({total}) exceeds caller limit of {max_task_count}."
                )),
            ));
        }
    }
    if let Some(max) = ctx.caller_limits.max_step_script_size {
        for step in steps {
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
    if let Some(max) = ctx.caller_limits.max_environment_size {
        let all_envs = steps
            .iter()
            .flat_map(|s| s.step_environments.iter().flatten())
            .chain(job_environments.into_iter().flatten());
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
    Ok(())
}

/// `create_job`'s Steps and loops under `CONTROL_FLOW` (RFC 0011 §When groups
/// are evaluated, RFC 0013 §Loop binding): every Step and loop is bound as far
/// as the values concrete at job creation allow, in dependency order so that
/// a producer skipped or with known Tasks makes its symbols concrete for the
/// entries that read them. Returns each entry of the template's `steps`, in
/// template order: `None` for a Step whose group failed with an error
/// collected into `check_errors`.
///
/// `selections` holds each Step's Environment selection, index for index
/// with [`JobTemplate::step_templates`]; a top-level Step applies its own
/// once its `enableIf` is `true` (RFC 0011 `<Environment>` item 3). Their
/// errors are not in them here: job creation reports those of the Steps
/// that apply them.
///
/// A Step group failure that is a step `let` error or a remaining-group check
/// error is collected into `check_errors` with the other Steps' errors, in
/// template order; any other Step group failure, and every loop group
/// failure, aborts at once. The Task-count limits are checked as each Step's
/// Tasks become known.
#[allow(clippy::too_many_arguments)]
fn instantiate_steps_with_binding(
    job_template: &JobTemplate,
    symtab: &openjd_expr::symbol_table::SymbolTable,
    has_expr: bool,
    limits: &EffectiveLimits,
    ctx: &ValidationContext,
    budgets: EvalBudgets,
    selections: &[job::EnvironmentSelection],
    check_errors: &mut crate::error::ValidationErrors,
) -> Result<Vec<Option<binding::Built>>, ModelError> {
    use crate::template::StepEntry;
    let n = job_template.steps.len();
    // Validation rejects a cycle; template order is a safe fallback.
    let order = crate::template::validate_v2023_09::step_dependency_order(&job_template.steps)
        .unwrap_or_else(|| (0..n).collect());
    // The index in `selections` of each top-level Step's entry.
    let mut flat_of_entry = vec![None; n];
    for (flat, st) in job_template.step_templates().enumerate() {
        if st.body_index.is_none() {
            flat_of_entry[st.entry_index] = Some(flat);
        }
    }
    let cx = binding::BindContext {
        jt: job_template,
        ctx,
        symtab,
        has_expr,
        limits,
        budgets,
    };
    let mut built: Vec<Option<binding::Built>> = vec![None; n];
    let mut step_errors: Vec<crate::error::ValidationErrors> =
        (0..n).map(|_| Default::default()).collect();
    // u128, so the Job total is compared and reported exactly.
    let mut committed: u128 = 0;
    for i in order {
        match &job_template.steps[i] {
            StepEntry::Step(st) => {
                let mut site = binding::StepSite::top(job_template, i)?;
                site.selection = flat_of_entry[i].and_then(|f| selections.get(f));
                let inputs = binding::step_creation_inputs(job_template, st, &built)?;
                match binding::bind_step(&cx, &site, None, &inputs, committed) {
                    Ok(step) => {
                        if let Some(tasks) = step.tasks_known() {
                            committed += u128::from(tasks);
                        }
                        built[i] = Some(binding::Built::Step(step));
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
            StepEntry::Loop(l) => {
                let inputs = loops::loop_creation_inputs(job_template, l, &built)?;
                match loops::bind_loop(&cx, i, l, None, &inputs) {
                    Ok(job_loop) => built[i] = Some(binding::Built::Loop(job_loop)),
                    Err(binding::GroupFailure {
                        failure: binding::Failure::Abort(e),
                        ..
                    }) => return Err(e),
                    Err(binding::GroupFailure {
                        failure: binding::Failure::Collected(errors),
                        ..
                    }) => step_errors[i] = errors,
                }
            }
        }
    }
    for errors in step_errors {
        check_errors.errors.extend(errors.errors);
    }
    Ok(built)
}
