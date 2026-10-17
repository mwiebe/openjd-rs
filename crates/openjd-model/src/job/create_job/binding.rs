// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Progressive step input binding (`CONTROL_FLOW`, RFC 0010 §Step input
//! binding).
//!
//! A Step's materialization expressions fall into four groups (enablement,
//! Task enumeration, host selection, remaining). [`bind_step`] is the core
//! that both `create_job` and [`advance_step_binding`] call: it decides the
//! Step's `enableIf` first, then evaluates every other group whose inputs are
//! concrete, in that order, and stops at the first group that fails. A
//! group's inputs come from static analysis ([`StepGroups`]): the `Steps.*`
//! symbols its fields reference, directly or through the step `let` bindings
//! it uses, whether or not a branch reading them is ever taken.

use std::collections::{BTreeSet, HashMap};

use openjd_expr::path_mapping::PathFormat;
use openjd_expr::symbol_table::SymbolTable;
use openjd_expr::{ExprType, ExprValue};

use super::instantiate::{self, step_path};
use super::{ranges, EvalBudgets};
use crate::error::{path_field, path_index, ModelError, PathElement, ValidationErrors};
use crate::job::{
    self, binding::steps_key, BindingError, GroupState, MaterializationGroup, PendingBinding,
    StepBinding, StepInputs,
};
use crate::template::validate_v2023_09::steps_scope::{add_steps_scope, StepsFieldScope};
use crate::template::validate_v2023_09::EffectiveLimits;
use crate::template::{self, BoolOrFormatString, JobTemplate, StepTemplate};
use crate::types::{JobParameterValues, ValidationContext};

/// Everything a binding needs from the Job besides the Step and its inputs.
pub(super) struct BindContext<'a> {
    pub(super) jt: &'a JobTemplate,
    pub(super) ctx: &'a ValidationContext,
    /// The job symbol table: `Param.*`, `RawParam.*`, and with `EXPR`
    /// `Job.Name`.
    pub(super) symtab: &'a SymbolTable,
    pub(super) has_expr: bool,
    pub(super) limits: &'a EffectiveLimits,
    pub(super) budgets: EvalBudgets,
}

/// How a group failure reaches the caller of `create_job`.
#[derive(Debug)]
pub(super) enum Failure {
    /// A step `let` error, or the remaining group's check errors: collected
    /// with the other Steps' errors and reported together.
    Collected(ValidationErrors),
    /// An `enableIf`, range, or host requirement error, a repeated
    /// `CHUNK[INT]` value, or a Task-count limit: reported alone.
    Abort(ModelError),
}

/// A failed group.
#[derive(Debug)]
pub(super) struct GroupFailure {
    pub(super) group: MaterializationGroup,
    pub(super) failure: Failure,
}

impl GroupFailure {
    fn abort(group: MaterializationGroup, e: ModelError) -> Self {
        Self {
            group,
            failure: Failure::Abort(e),
        }
    }

    fn collected(group: MaterializationGroup, errors: ValidationErrors) -> Self {
        Self {
            group,
            failure: Failure::Collected(errors),
        }
    }

    fn into_binding_error(self, step: &str) -> BindingError {
        let error = match self.failure {
            Failure::Abort(e) => e,
            Failure::Collected(errors) => match errors.into_result("JobTemplate") {
                Err(e) => e,
                Ok(()) => ModelError::DecodeValidation(
                    "internal error: a binding group failed without an error".to_string(),
                ),
            },
        };
        BindingError {
            step: step.to_string(),
            group: self.group,
            errors: vec![error],
        }
    }
}

// ── Group inputs ──────────────────────────────────────────────────

/// The static inputs of a Step's groups: the `Steps.*` symbols each group
/// references (as `Steps.<Step>.<Member>.<Name>`), directly or through the
/// step `let` bindings it uses, and those bindings' indices.
#[derive(Debug, Default)]
pub(super) struct StepGroups {
    enablement: BTreeSet<String>,
    tasks: BTreeSet<String>,
    tasks_lets: BTreeSet<usize>,
    host: BTreeSet<String>,
    host_lets: BTreeSet<usize>,
    /// Uses every step `let` binding.
    remaining: BTreeSet<String>,
}

impl StepGroups {
    pub(super) fn of(st: &StepTemplate) -> Self {
        let lets = LetRefs::of(st);
        let mut groups = Self::default();

        if let Some(BoolOrFormatString::FormatString(fs)) = &st.enable_if {
            // enableIf may not read `let` (validation), so no expansion.
            groups.enablement = steps_symbols(fs.accessed_symbols());
        }

        let mut tasks_syms = Vec::new();
        for tp in st
            .parameter_space
            .iter()
            .flat_map(|ps| &ps.task_parameter_definitions)
        {
            for fs in instantiate::task_parameter_format_strings(tp) {
                tasks_syms.extend(fs.accessed_symbols());
            }
        }
        (groups.tasks, groups.tasks_lets) = lets.expand(tasks_syms);

        let mut host_syms = Vec::new();
        for fs in host_requirement_format_strings(st) {
            host_syms.extend(fs.accessed_symbols());
        }
        (groups.host, groups.host_lets) = lets.expand(host_syms);

        let mut remaining_syms: Vec<String> = Vec::new();
        for env in st.step_environments.iter().flatten() {
            if let Some(BoolOrFormatString::FormatString(fs)) = &env.enable_if {
                remaining_syms.extend(fs.accessed_symbols());
            }
            if let Some(script) = &env.script {
                for action in script.actions.iter_actions() {
                    template_scope_action_symbols(action, &mut remaining_syms);
                }
            }
        }
        if let Some(script) = st.resolve_syntax_sugar() {
            template_scope_action_symbols(&script.actions.on_run, &mut remaining_syms);
        }
        for refs in &lets.refs {
            remaining_syms.extend(refs.iter().cloned());
        }
        (groups.remaining, _) = lets.expand(remaining_syms);
        groups
    }

    /// Every `Steps.*` symbol a group of the Step reads.
    fn all(&self) -> impl Iterator<Item = &String> {
        self.enablement
            .iter()
            .chain(&self.tasks)
            .chain(&self.host)
            .chain(&self.remaining)
    }
}

/// The symbols of each step `let` binding, by index, and the indices of
/// each name.
struct LetRefs {
    refs: Vec<Vec<String>>,
    by_name: HashMap<String, Vec<usize>>,
}

impl LetRefs {
    fn of(st: &StepTemplate) -> Self {
        let mut refs = Vec::new();
        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        for (j, binding) in st.let_bindings.iter().flatten().enumerate() {
            let (name, symbols) = match binding.split_once('=') {
                Some((name, expr)) => (
                    name.trim().to_string(),
                    openjd_expr::eval::ParsedExpression::new(expr.trim())
                        .map(|p| p.accessed_symbols().iter().cloned().collect())
                        .unwrap_or_default(),
                ),
                None => (String::new(), Vec::new()),
            };
            by_name.entry(name).or_default().push(j);
            refs.push(symbols);
        }
        Self { refs, by_name }
    }

    /// The `Steps.*` symbols of `symbols` and of the `let` bindings they
    /// read, transitively, with those bindings' indices.
    fn expand(&self, symbols: Vec<String>) -> (BTreeSet<String>, BTreeSet<usize>) {
        let mut steps = BTreeSet::new();
        let mut lets = BTreeSet::new();
        let mut work = symbols;
        while let Some(sym) = work.pop() {
            if let Some(key) = steps_key(&sym) {
                steps.insert(key);
                continue;
            }
            let root = sym.split('.').next().unwrap_or(&sym);
            for &j in self.by_name.get(root).into_iter().flatten() {
                if lets.insert(j) {
                    work.extend(self.refs[j].iter().cloned());
                }
            }
        }
        (steps, lets)
    }
}

fn steps_symbols(symbols: impl IntoIterator<Item = String>) -> BTreeSet<String> {
    symbols.into_iter().filter_map(|s| steps_key(&s)).collect()
}

/// The format strings of a Step's `hostRequirements`.
fn host_requirement_format_strings(st: &StepTemplate) -> Vec<&openjd_expr::FormatString> {
    let mut fields = Vec::new();
    if let Some(hr) = &st.host_requirements {
        for amt in hr.amounts.iter().flatten() {
            fields.push(&amt.name);
            fields.extend(amt.min.iter());
            fields.extend(amt.max.iter());
        }
        for attr in hr.attributes.iter().flatten() {
            fields.push(&attr.name);
            fields.extend(attr.any_of.iter().flatten());
            fields.extend(attr.all_of.iter().flatten());
        }
    }
    fields
}

/// The symbols of an action's TEMPLATE-scope fields (Template Schemas §5:
/// plain `@fmtstring`, not `@fmtstring[host]`): `timeout`, and the
/// cancelation's `notifyPeriodInSeconds` and format-string `mode`.
fn template_scope_action_symbols(action: &template::Action, out: &mut Vec<String>) {
    if let Some(t) = &action.timeout {
        out.extend(t.accessed_symbols());
    }
    match &action.cancelation {
        Some(template::CancelationMode::NotifyThenTerminate {
            notify_period_in_seconds: Some(n),
        }) => out.extend(n.accessed_symbols()),
        Some(template::CancelationMode::DeferredMode {
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

/// The symbols of `inputs` that are not concrete, in sorted order.
fn missing(symbols: &BTreeSet<String>, inputs: &StepInputs) -> Vec<String> {
    symbols
        .iter()
        .filter(|s| !inputs.is_concrete(s))
        .cloned()
        .collect()
}

// ── The core ──────────────────────────────────────────────────────

/// Bind Step `step_index` as far as `inputs` allow (RFC 0010 §How a Step is
/// bound): the Step as it now is, `PendingInputs` (with more known than
/// `prior`, or the same), `Bound`, or `Skipped`; or the first group that
/// failed.
///
/// `prior` is the Step from an earlier call, whose known groups are kept
/// rather than evaluated again; `None` at job creation. `committed_task_count`
/// is the number of Tasks of the other Steps whose Tasks are known, for the
/// Job-total Task-count limit.
pub(super) fn bind_step(
    cx: &BindContext<'_>,
    step_index: usize,
    prior: Option<&job::Step>,
    inputs: &StepInputs,
    committed_task_count: u64,
) -> Result<job::Step, GroupFailure> {
    use MaterializationGroup::*;
    let st = &cx.jt.steps[step_index];
    let groups = StepGroups::of(st);
    let path = step_path(step_index);
    let prior_pending = match prior.map(|s| &s.binding) {
        Some(StepBinding::PendingInputs(p)) => Some(p),
        _ => None,
    };

    // `inputs` must hold every value given in earlier calls: a known group
    // is not evaluated again, but the `let` bindings are, and the Step's
    // table is rebuilt from `inputs` alone.
    if let Some(p) = prior_pending {
        let known: [(MaterializationGroup, bool, &BTreeSet<String>); 4] = [
            (Enablement, p.enablement.is_known(), &groups.enablement),
            (TaskEnumeration, p.tasks.is_known(), &groups.tasks),
            (HostSelection, p.host_requirements.is_known(), &groups.host),
            (Remaining, p.remaining.is_known(), &groups.remaining),
        ];
        for (group, is_known, symbols) in known {
            if let Some(sym) = is_known
                .then(|| missing(symbols, inputs))
                .and_then(|m| m.into_iter().next())
            {
                return Err(GroupFailure::abort(
                    group,
                    ModelError::Compatibility(format!(
                        "the {group} group of Step '{}' was evaluated with '{sym}', which the given inputs do not hold; pass every producer's values given before, as well as new ones.",
                        st.name
                    )),
                ));
            }
        }
    }

    let mut step = instantiate::step_shell(st).map_err(|e| GroupFailure::abort(Enablement, e))?;
    if let (Some(prior), Some(p)) = (prior, prior_pending) {
        if p.remaining.is_known() {
            // The Step Environments' `enabled` were decided with the
            // remaining group.
            step.step_environments = prior.step_environments.clone();
        }
    }

    // The Step's table: the job table, `Step.Name`, and the concrete
    // `Steps.*` values its groups read.
    let mut step_symtab = cx.symtab.clone();
    let internal = |e: openjd_expr::symbol_table::SymbolTableError| {
        GroupFailure::abort(Enablement, ModelError::from(e))
    };
    if cx.has_expr {
        step_symtab
            .set("Step.Name", ExprValue::String(st.name.clone()))
            .map_err(internal)?;
    }
    for sym in groups.all() {
        if let Some(v) = inputs.get(sym) {
            step_symtab.set(sym, v.clone()).map_err(internal)?;
        }
    }

    // ── Enablement ──
    let enablement_known = prior_pending.is_some_and(|p| p.enablement.is_known());
    if !enablement_known {
        let waiting = missing(&groups.enablement, inputs);
        if !waiting.is_empty() {
            step.binding = StepBinding::PendingInputs(PendingBinding {
                enablement: GroupState::Waiting { on: waiting },
                tasks: GroupState::Waiting {
                    on: missing(&groups.tasks, inputs),
                },
                host_requirements: GroupState::Waiting {
                    on: missing(&groups.host, inputs),
                },
                remaining: GroupState::Waiting {
                    on: missing(&groups.remaining, inputs),
                },
            });
            return Ok(step);
        }
        let enabled = evaluate_enable_if(
            st.enable_if.as_ref(),
            &step_symtab,
            cx.budgets,
            &path_field(&path, "enableIf"),
        )
        .map_err(|e| GroupFailure::abort(Enablement, e))?;
        if !enabled {
            step.binding = StepBinding::Skipped;
            return Ok(step);
        }
    }

    let mut lets = LetEvaluator::new(cx, st, &path);

    // ── Task enumeration ──
    let tasks = match prior_pending.map(|p| &p.tasks) {
        Some(GroupState::Known(space)) => GroupState::Known(space.clone()),
        _ => {
            let waiting = missing(&groups.tasks, inputs);
            if waiting.is_empty() {
                lets.evaluate(&groups.tasks_lets, &mut step_symtab)
                    .map_err(|e| GroupFailure::collected(TaskEnumeration, e))?;
                let space = enumerate_tasks(cx, st, step_index, &step_symtab, committed_task_count)
                    .map_err(|e| GroupFailure::abort(TaskEnumeration, e))?;
                GroupState::Known(space)
            } else {
                GroupState::Waiting { on: waiting }
            }
        }
    };

    // ── Host selection ──
    let host_requirements = match prior_pending.map(|p| &p.host_requirements) {
        Some(GroupState::Known(hr)) => GroupState::Known(hr.clone()),
        _ => {
            let waiting = missing(&groups.host, inputs);
            if waiting.is_empty() {
                lets.evaluate(&groups.host_lets, &mut step_symtab)
                    .map_err(|e| GroupFailure::collected(HostSelection, e))?;
                let hr = st
                    .host_requirements
                    .as_ref()
                    .map(|hr| {
                        instantiate::resolve_host_requirements(
                            hr,
                            &step_symtab,
                            cx.ctx,
                            step_index,
                            cx.budgets,
                        )
                    })
                    .transpose()
                    .map_err(|e| {
                        // A resolution error names the field in its text;
                        // give it the Step's path too.
                        let e = match e {
                            e @ ModelError::ModelValidation(_) => e,
                            e => located(&path_field(&path, "hostRequirements"), &e),
                        };
                        GroupFailure::abort(HostSelection, e)
                    })?;
                GroupState::Known(hr)
            } else {
                GroupState::Waiting { on: waiting }
            }
        }
    };

    // ── Remaining ──
    let remaining = match prior_pending.map(|p| &p.remaining) {
        Some(GroupState::Known(())) => GroupState::Known(()),
        _ => {
            let waiting = missing(&groups.remaining, inputs);
            if waiting.is_empty() {
                lets.evaluate_all(&mut step_symtab)
                    .map_err(|e| GroupFailure::collected(Remaining, e))?;
                let errors = evaluate_remaining(cx, st, step_index, &step_symtab, &mut step)
                    .map_err(|e| GroupFailure::abort(Remaining, e))?;
                if !errors.is_empty() {
                    return Err(GroupFailure::collected(Remaining, errors));
                }
                GroupState::Known(())
            } else {
                GroupState::Waiting { on: waiting }
            }
        }
    };

    match (tasks, host_requirements, remaining) {
        (GroupState::Known(space), GroupState::Known(hr), GroupState::Known(())) => {
            // Every binding has evaluated successfully in this call or an
            // earlier one; one not evaluated in this call gives the same
            // value again.
            lets.evaluate_all(&mut step_symtab)
                .map_err(|e| GroupFailure::collected(Remaining, e))?;
            step.parameter_space = space;
            step.host_requirements = hr;
            step.resolved_symtab = Some(instantiate::resolved_step_symtab(&step, st, &step_symtab));
            step.binding = StepBinding::Bound;
            Ok(step)
        }
        (tasks, host_requirements, remaining) => {
            step.binding = StepBinding::PendingInputs(PendingBinding {
                enablement: GroupState::Known(()),
                tasks,
                host_requirements,
                remaining,
            });
            Ok(step)
        }
    }
}

/// Evaluate an `enableIf`, with target type `bool` (a `null` result is an
/// error). `None` (no `enableIf`) is `true`.
pub(super) fn evaluate_enable_if(
    enable_if: Option<&BoolOrFormatString>,
    symtab: &SymbolTable,
    budgets: EvalBudgets,
    path: &[PathElement],
) -> Result<bool, ModelError> {
    let fs = match enable_if {
        None => return Ok(true),
        Some(BoolOrFormatString::Bool(b)) => return Ok(*b),
        Some(BoolOrFormatString::FormatString(fs)) => fs,
    };
    let fail = |message: String| {
        let mut errors = ValidationErrors::default();
        errors.add(path, message);
        match errors.into_result("JobTemplate") {
            Err(e) => e,
            Ok(()) => unreachable!("one error was added"),
        }
    };
    let value = fs
        .resolve_with(
            symtab,
            &budgets.fs_options().with_target_type(&ExprType::BOOL),
        )
        .map_err(|e| fail(e.to_string()))?;
    match value {
        ExprValue::Bool(b) => Ok(b),
        other => match other.coerce(&ExprType::BOOL, PathFormat::Posix) {
            Ok(ExprValue::Bool(b)) => Ok(b),
            _ => Err(fail(format!("'{}' does not convert to a bool.", fs.raw()))),
        },
    }
}

/// The Task enumeration group: materialize the parameter space, check its
/// combination, its distinct `CHUNK[INT]` values in a Step with outputs, and
/// the Task-count limits. `None` for a Step without a `parameterSpace`.
///
/// Every error is located (RFC 0010 §How a Step is bound item 3: the message
/// names the field and the expression): a range or `chunks` error at
/// `steps[i] -> parameterSpace -> taskParameterDefinitions[k] -> range` (or
/// `-> chunks`), a combination error at `steps[i] -> parameterSpace`, and a
/// Task-count limit at `steps[i]`.
fn enumerate_tasks(
    cx: &BindContext<'_>,
    st: &StepTemplate,
    step_index: usize,
    step_symtab: &SymbolTable,
    committed_task_count: u64,
) -> Result<Option<job::StepParameterSpace>, ModelError> {
    let step_path = step_path(step_index);
    let ps_path = path_field(&step_path, "parameterSpace");
    let tpd_path = path_field(&ps_path, "taskParameterDefinitions");
    let space = st
        .parameter_space
        .as_ref()
        .map(|ps| {
            ranges::resolve_parameter_space_by_field(
                ps,
                step_symtab,
                cx.limits,
                cx.budgets,
                ranges::RangeRules {
                    expr: cx.has_expr,
                    control_flow: true,
                },
            )
        })
        .transpose()
        .map_err(|(k, e)| located(&path_field(&path_index(&tpd_path, k), e.field), &e.error))?;
    if let Some(space) = &space {
        // Association length mismatches and the like.
        crate::step_param_space::StepParameterSpaceIterator::new(space)
            .map_err(|e| located(&ps_path, &e))?;
        if st.output_variables.is_some() {
            check_chunk_values_distinct(space)
                .map_err(|(k, e)| located(&path_field(&path_index(&tpd_path, k), "range"), &e))?;
        }
    }
    let task_count = job::binding::task_count(space.as_ref())?;
    check_task_count(cx.ctx, &st.name, task_count, committed_task_count)
        .map_err(|e| located(&step_path, &e))?;
    Ok(space)
}

/// `error` as a one-error `ModelValidation` at `path`, its message without
/// the `ModelError` variant's prefix.
fn located(path: &[PathElement], error: &ModelError) -> ModelError {
    let message = match error {
        ModelError::DecodeValidation(m) => m.clone(),
        ModelError::Expression(e) => e.to_string(),
        e @ ModelError::FormatStringError { .. } => {
            let text = e.to_string();
            text.strip_prefix("Format string error: ")
                .map(str::to_string)
                .unwrap_or(text)
        }
        ModelError::ModelValidation(errors) => errors
            .errors
            .iter()
            .map(|e| e.message.clone())
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    };
    let mut errors = ValidationErrors::default();
    errors.add(path, message);
    match errors.into_result("JobTemplate") {
        Err(e) => e,
        Ok(()) => unreachable!("one error was added"),
    }
}

/// In a Step that declares `outputVariables`, a `CHUNK[INT]` range must
/// have distinct values, so that a task selector names one Task (RFC 0010
/// §Chunked Steps). A range expression's values are distinct by
/// construction. The error names the parameter's index.
fn check_chunk_values_distinct(space: &job::StepParameterSpace) -> Result<(), (usize, ModelError)> {
    for (k, param) in space.task_parameter_definitions.values().enumerate() {
        let job::TaskParameter::ChunkInt {
            range: job::TaskParamRange::List(values),
            ..
        } = param
        else {
            continue;
        };
        let mut seen = std::collections::HashSet::new();
        let mut repeated = BTreeSet::new();
        for v in values {
            if !seen.insert(*v) {
                repeated.insert(*v);
            }
        }
        if !repeated.is_empty() {
            let values: Vec<String> = repeated.iter().map(i64::to_string).collect();
            let (noun, verb) = if values.len() == 1 {
                ("value", "appears")
            } else {
                ("values", "appear")
            };
            return Err((
                k,
                ModelError::DecodeValidation(format!(
                    "must have distinct values in a Step with outputVariables, so that each value names one Task; {noun} {} {verb} more than once.",
                    values.join(", ")
                )),
            ));
        }
    }
    Ok(())
}

/// The caller's Task-count limits, checked when a Step's Tasks become known
/// (RFC 0010 §Limits item 4).
pub(super) fn check_task_count(
    ctx: &ValidationContext,
    step: &str,
    task_count: u64,
    committed_task_count: u64,
) -> Result<(), ModelError> {
    check_step_task_count(ctx, step, task_count)?;
    if let Some(max) = ctx.caller_limits.max_task_count {
        let total = committed_task_count.saturating_add(task_count);
        if total > max {
            return Err(ModelError::ModelValidation(ValidationErrors::single(format!(
                "Total task count ({total}) exceeds caller limit of {max}: Step '{step}' has {task_count} Tasks, and the Steps whose Tasks are known before it have {committed_task_count}."
            ))));
        }
    }
    Ok(())
}

/// `CallerLimits::max_step_task_count`.
pub(super) fn check_step_task_count(
    ctx: &ValidationContext,
    step: &str,
    task_count: u64,
) -> Result<(), ModelError> {
    if let Some(max) = ctx.caller_limits.max_step_task_count {
        if task_count > max {
            return Err(ModelError::ModelValidation(ValidationErrors::single(format!(
                "Step '{step}' has {task_count} Tasks, which exceeds the caller limit of {max} Tasks per Step."
            ))));
        }
    }
    Ok(())
}

/// The remaining group: each Step Environment's `enableIf` (setting its
/// `enabled` on `step`), then the carried-forward checks of the script and
/// of each enabled Step Environment. Returns the errors, which `create_job`
/// collects; `Err` only for an internal symbol-table failure.
fn evaluate_remaining(
    cx: &BindContext<'_>,
    st: &StepTemplate,
    step_index: usize,
    step_symtab: &SymbolTable,
    step: &mut job::Step,
) -> Result<ValidationErrors, ModelError> {
    let path = step_path(step_index);
    let steps_scope = Some((cx.jt, step_index));
    let mut template_check_symtab = step_symtab.clone();
    add_steps_scope(
        &mut template_check_symtab,
        cx.jt,
        step_index,
        StepsFieldScope::Template,
    )?;
    let mut errors = ValidationErrors::default();
    instantiate::check_step_script(
        st,
        &path,
        step_symtab,
        &template_check_symtab,
        steps_scope,
        cx.has_expr,
        cx.ctx,
        &mut errors,
    )?;
    for (j, env) in st.step_environments.iter().flatten().enumerate() {
        let env_path = path_index(&path_field(&path, "stepEnvironments"), j);
        let enabled = match evaluate_enable_if(
            env.enable_if.as_ref(),
            step_symtab,
            cx.budgets,
            &path_field(&env_path, "enableIf"),
        ) {
            Ok(enabled) => enabled,
            Err(ModelError::ModelValidation(e)) => {
                errors.errors.extend(e.errors);
                continue;
            }
            Err(e) => return Err(e),
        };
        if let Some(job_env) = step
            .step_environments
            .as_mut()
            .and_then(|envs| envs.get_mut(j))
        {
            job_env.enabled = enabled;
        }
        if !enabled {
            // A disabled Environment is never entered, so its fields never
            // apply.
            continue;
        }
        instantiate::check_step_environment(
            env,
            &path,
            j,
            step_symtab,
            &template_check_symtab,
            steps_scope,
            cx.has_expr,
            cx.limits,
            cx.ctx,
            &mut errors,
        )?;
    }
    Ok(errors)
}

/// Evaluates step `let` bindings on demand, each at most once per call.
struct LetEvaluator<'a> {
    cx: &'a BindContext<'a>,
    st: &'a StepTemplate,
    let_path: Vec<PathElement>,
    evaluated: BTreeSet<usize>,
}

impl<'a> LetEvaluator<'a> {
    fn new(cx: &'a BindContext<'a>, st: &'a StepTemplate, step_path: &[PathElement]) -> Self {
        Self {
            cx,
            st,
            let_path: path_field(step_path, "let"),
            evaluated: BTreeSet::new(),
        }
    }

    /// Evaluate the bindings `indices` not evaluated yet into `symtab`, in
    /// declaration order. Their errors, at `steps[i] -> let[j]`, if any.
    fn evaluate(
        &mut self,
        indices: &BTreeSet<usize>,
        symtab: &mut SymbolTable,
    ) -> Result<(), ValidationErrors> {
        let Some(bindings) = &self.st.let_bindings else {
            return Ok(());
        };
        if !self.cx.has_expr {
            return Ok(());
        }
        let todo: BTreeSet<usize> = indices.difference(&self.evaluated).copied().collect();
        if todo.is_empty() {
            return Ok(());
        }
        let mut errors = ValidationErrors::default();
        crate::template::validate_v2023_09::format_strings::check_carried_forward_let_bindings_where(
            bindings,
            &self.let_path,
            symtab,
            self.cx.ctx,
            openjd_expr::HostContext::None,
            &mut errors,
            |j| todo.contains(&j),
        );
        self.evaluated.extend(todo);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    fn evaluate_all(&mut self, symtab: &mut SymbolTable) -> Result<(), ValidationErrors> {
        let all: BTreeSet<usize> = (0..self.st.let_bindings.as_ref().map_or(0, Vec::len)).collect();
        self.evaluate(&all, symtab)
    }
}

// ── create_job's inputs ───────────────────────────────────────────

/// The `Steps.*` values that are concrete at job creation for Step
/// `step_index`, from the Steps already instantiated (`built`, by template
/// index): the `null`s of a skipped producer, and the `TaskParam`s of a
/// producer whose Tasks are known.
pub(super) fn creation_inputs(
    jt: &JobTemplate,
    step_index: usize,
    built: &[Option<job::Step>],
) -> Result<StepInputs, ModelError> {
    // Only the producers the Step's groups read: building a producer's
    // TaskParam lists walks its whole parameter space.
    let read: BTreeSet<String> = StepGroups::of(&jt.steps[step_index])
        .all()
        .filter_map(|s| s.split('.').nth(1).map(str::to_string))
        .collect();
    let mut inputs = StepInputs::new();
    for dep in jt.steps[step_index].dependencies.iter().flatten() {
        if !read.contains(&dep.depends_on) {
            continue;
        }
        let Some(producer) = jt
            .steps
            .iter()
            .position(|s| s.name == dep.depends_on)
            .and_then(|i| built[i].as_ref())
        else {
            continue;
        };
        match &producer.binding {
            StepBinding::Skipped => inputs.add_producer(producer, job::ProducerState::Skipped)?,
            _ if producer.tasks_known().is_some() => {
                inputs.add_producer(producer, job::ProducerState::TasksKnown)?
            }
            _ => {}
        }
    }
    Ok(inputs)
}

// ── Public API ────────────────────────────────────────────────────

/// Advance the binding of pending Step `step_index` of `job` with the
/// `Steps.*` values in `inputs` (RFC 0010 §How a Step is bound).
///
/// Evaluates every group of the Step that has not evaluated and whose
/// inputs are now concrete, in the RFC's order: enablement first, and
/// nothing else until it is `true`; then Task enumeration (materialize the
/// parameter space, check distinct `CHUNK[INT]` values in a Step with
/// outputs and the Task-count limits); then host selection; then the
/// remaining group (each Step Environment's `enableIf`, which sets its
/// [`enabled`](job::Environment::enabled), and the carried-forward checks of
/// the script and the enabled Environments). It stops at the first group
/// that fails and returns it: the Step is `FAILED` at once, whatever its
/// other groups wait for.
///
/// Returns the Step with more known, `Bound` once every group has evaluated
/// (its parameter space and host requirements moved into its own fields and
/// its `resolved_symtab` built), or `Skipped` when `enableIf` is `false`.
/// Called with no new inputs, it returns the Step unchanged; called on a
/// bound or skipped Step, it returns it unchanged. The caller swaps the
/// result into its Job, and dispatches Tasks only of a bound Step, so
/// dispatch stays atomic.
///
/// `job_template`, `job_parameter_values`, and `ctx` are the ones `job` was
/// created from. `committed_task_count` is the number of Tasks of the other
/// Steps whose Tasks are known, for `CallerLimits::max_task_count`.
///
/// `inputs` is cumulative: it must hold every producer's values given in
/// earlier calls for this Step, as well as new ones. Step `let` bindings are
/// evaluated on demand, only those the group being evaluated uses, and are
/// evaluated again in later calls rather than cached, which gives the same
/// values as long as the inputs do; a known group's input missing from
/// `inputs` is refused rather than reported as the Step failing. A bound
/// Step that reads `Steps.*` in its script or Step Environments needs
/// [`attach_step_inputs`] before its Tasks run.
///
/// Caller errors, which say nothing about the Step, are a `BindingError`
/// whose error is a [`ModelError::Compatibility`]: a `step_index` past the
/// Job's Steps (with `step` `#<index>`), a template whose Step at that index
/// has another name, and the missing input above. Every other
/// `BindingError` fails the Step.
pub fn advance_step_binding(
    job_template: &JobTemplate,
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
    job: &job::Job,
    step_index: usize,
    inputs: &StepInputs,
    committed_task_count: u64,
) -> Result<job::Step, BindingError> {
    let misuse = |step: &str, message: String| BindingError {
        step: step.to_string(),
        group: MaterializationGroup::Enablement,
        errors: vec![ModelError::Compatibility(message)],
    };
    let Some(step) = job.steps.get(step_index) else {
        return Err(misuse(
            &format!("#{step_index}"),
            format!(
                "the Job has {} Steps, so it has no Step {step_index}.",
                job.steps.len()
            ),
        ));
    };
    if !matches!(step.binding, StepBinding::PendingInputs(_)) {
        return Ok(step.clone());
    }
    if job_template.steps.get(step_index).map(|s| &s.name) != Some(&step.name) {
        return Err(misuse(
            &step.name,
            format!(
                "Step {step_index} of the Job is '{}', but the template's is not; pass the template the Job was created from.",
                step.name
            ),
        ));
    }
    let to_err = |e: ModelError| BindingError {
        step: step.name.clone(),
        group: MaterializationGroup::Enablement,
        errors: vec![e],
    };
    let has_expr = ctx
        .profile
        .has_extension(crate::types::ModelExtension::Expr);
    let mut symtab = super::build_symbol_table(job_parameter_values).map_err(to_err)?;
    if has_expr {
        symtab
            .set("Job.Name", ExprValue::String(job.name.clone()))
            .map_err(|e| to_err(e.into()))?;
    }
    let limits = EffectiveLimits::from_context(ctx);
    let cx = BindContext {
        jt: job_template,
        ctx,
        symtab: &symtab,
        has_expr,
        limits: &limits,
        budgets: EvalBudgets::from_ctx(ctx),
    };
    bind_step(&cx, step_index, Some(step), inputs, committed_task_count)
        .map_err(|f| f.into_binding_error(&step.name))
}

/// Merge the concrete `Steps.*` values that bound Step `step`'s script and
/// Step Environments read into its `resolved_symtab`, so its Tasks can run
/// (RFC 0010 §Concreteness: they are concrete once its dependencies have
/// finished).
///
/// A path-typed `Steps.P.Output.X` or `Steps.P.TaskParam.N` is not in
/// `inputs` (only its `Raw` twin is), so the twin is stored instead, as a
/// `PATH` `Param.*` falls back to its `RawParam.*`. Every path-typed `Raw`
/// value stored, read directly or through this fallback, is stored as a
/// `string` (or a list of strings) and recorded in
/// [`job::Step::path_inputs`], so that the session builds the `path` values
/// and maps `Output` and `TaskParam` forward on the worker host. A skipped
/// producer's `null`s are stored as they are. Every `Steps.*` value and
/// `path_inputs` record comes from `inputs`: attaching again replaces those
/// of an earlier call.
///
/// Errors when the Step is not bound, or when a symbol it reads is not
/// concrete in `inputs` (the error names it).
pub fn attach_step_inputs(step: &job::Step, inputs: &StepInputs) -> Result<job::Step, ModelError> {
    if !step.binding.is_bound() {
        return Err(ModelError::DecodeValidation(format!(
            "Step '{}' is not bound, so its Steps.* inputs cannot be attached.",
            step.name
        )));
    }
    let symbols: BTreeSet<String> = host_scope_symbols(step)
        .iter()
        .filter_map(|s| steps_key(s))
        .collect();
    if symbols.is_empty() && step.path_inputs.is_empty() {
        return Ok(step.clone());
    }
    // Start from the table without any `Steps.*` value: every one, and every
    // `path_inputs` record, comes from this call's `inputs`, so attaching
    // again with other inputs leaves nothing of the earlier ones behind.
    let mut symtab = SymbolTable::new();
    if let Some(s) = &step.resolved_symtab {
        let full = s
            .to_symtab(PathFormat::Posix)
            .map_err(ModelError::DecodeValidation)?;
        for path in full.all_paths("") {
            if path.starts_with("Steps.") {
                continue;
            }
            if let Some(v) = full.get_value(&path) {
                symtab.set(&path, v.clone())?;
            }
        }
    }
    let mut path_inputs: std::collections::BTreeMap<String, job::PathInput> =
        std::collections::BTreeMap::new();
    for sym in &symbols {
        let (stored, value) = match inputs.get(sym) {
            Some(v) => (sym.clone(), v.clone()),
            None if inputs.is_concrete(sym) => {
                let twin = raw_twin(sym);
                let Some(v) = inputs.get(&twin) else {
                    return Err(not_concrete(step, sym));
                };
                (twin, v.clone())
            }
            None => return Err(not_concrete(step, sym)),
        };
        let value = match path_as_strings(&value) {
            Some(strings) => {
                path_inputs.insert(
                    stored.clone(),
                    job::PathInput {
                        symbol: stored.clone(),
                        ty: value.expr_type(),
                    },
                );
                strings
            }
            None => value,
        };
        symtab.set(&stored, value)?;
    }
    let mut attached = step.clone();
    attached.resolved_symtab = Some(openjd_expr::SerializedSymbolTable::from_symtab(&symtab));
    attached.path_inputs = path_inputs.into_values().collect();
    Ok(attached)
}

fn not_concrete(step: &job::Step, sym: &str) -> ModelError {
    ModelError::DecodeValidation(format!(
        "Step '{}' reads '{sym}', which is not concrete in the given inputs.",
        step.name
    ))
}

/// `Steps.P.RawOutput.X` for `Steps.P.Output.X`, `Steps.P.TaskRawParam.N`
/// for `Steps.P.TaskParam.N`.
fn raw_twin(sym: &str) -> String {
    let parts: Vec<&str> = sym.splitn(4, '.').collect();
    let member = match parts.get(2) {
        Some(&"Output") => "RawOutput",
        Some(&"TaskParam") => "TaskRawParam",
        Some(other) => other,
        None => "",
    };
    format!(
        "Steps.{}.{member}.{}",
        parts.get(1).unwrap_or(&""),
        parts.get(3).unwrap_or(&"")
    )
}

/// A value that is or contains `path`, as the same shape of strings; `None`
/// for any other value.
fn path_as_strings(value: &ExprValue) -> Option<ExprValue> {
    match value {
        ExprValue::Path { value, .. } => Some(ExprValue::String(value.clone())),
        ExprValue::ListPath(v, _, _) => ExprValue::make_list(
            v.iter().cloned().map(ExprValue::String).collect(),
            ExprType::STRING,
        )
        .ok(),
        ExprValue::ListList(elems, elem_ty, _) if *elem_ty == ExprType::list(ExprType::PATH) => {
            let strings: Option<Vec<ExprValue>> = elems.iter().map(path_as_strings).collect();
            ExprValue::make_list(strings?, ExprType::list(ExprType::STRING)).ok()
        }
        _ => None,
    }
}

/// The symbols a Step's worker-resolved fields read: its script (actions,
/// embedded files, script `let`) and Step Environments.
pub(crate) fn host_scope_symbols(step: &job::Step) -> std::collections::HashSet<String> {
    instantiate::collect_all_accessed_symbols(Some(&step.script), &step.step_environments, None)
}
