// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Running a loop (`CONTROL_FLOW`, RFC 0013 §Running a loop).
//!
//! A loop is bound like a Step, by [`bind_loop`], with three groups in
//! order: enablement (`enableIf`), bound (`maxIterations`), and start (each
//! `initial`, then the first `while`). `create_job` calls it for every loop
//! with the values concrete then, and [`advance_loop_binding`] with more.
//!
//! A bound loop runs one iteration after another. [`start_iteration`] creates
//! an instance of every body Step for one iteration, bound through the Step
//! binding core with the iteration's `Loop.*` values as one more layer of its
//! symbol table, and [`advance_body_step_binding`] advances an instance as
//! `advance_step_binding` advances a Step. When every instance has succeeded
//! or been skipped, [`evaluate_iteration_boundary`] evaluates the loop
//! outputs, `until`, `next`, `while`, and the bound, in that order, and
//! decides whether the loop continues. [`end_without_iterations`] ends a loop
//! whose first `while` is `false`.
//!
//! The model never sees a Task attempt, so every error these functions return
//! for the loop is final (RFC 0013 §Failure). A caller that sees a body Task
//! fail decides whether that failure is recoverable.

use std::collections::BTreeSet;

use indexmap::IndexMap;
use openjd_expr::path_mapping::PathFormat;
use openjd_expr::symbol_table::{SymbolTable, SymbolTableError};
use openjd_expr::{ExprType, ExprValue, FormatString};

use super::binding::{
    self, evaluate_enable_if, job_symbol_table, located, missing, steps_symbols, BindContext,
    Built, GroupFailure, StepSite,
};
use super::instantiate;
use super::EvalBudgets;
use crate::error::{path_field, path_index, ModelError, PathElement, ValidationErrors};
use crate::job::{
    self, binding::steps_key, BindingError, BoundLoop, BoundaryOutcome, GroupState, LoopBinding,
    LoopCondition, LoopEnd, LoopError, LoopErrorField, LoopExpression, LoopIteration, LoopProgress,
    LoopResult, LoopStart, MaterializationGroup, OutputVariable, PendingLoopBinding, StepBinding,
    StepInputs,
};
use crate::template::validate_v2023_09::steps_scope::StepLocation;
use crate::template::validate_v2023_09::EffectiveLimits;
use crate::template::{
    BoolOrFormatString, IntOrFormatString, JobTemplate, StepEntry, StepLoop, StepTemplate,
};
use crate::types::{JobParameterType, JobParameterValues, OutputCollectMode, ValidationContext};

/// The largest `maxIterations` (RFC 0013 §Limits item 1).
const MAX_ITERATIONS: i64 = 1000;

/// How a loop variable names itself in its messages.
const VARIABLE_LABEL: &str = "Loop variable";
/// How a loop output names itself in its messages.
const OUTPUT_LABEL: &str = "Loop output";

// ── Paths ─────────────────────────────────────────────────────────

fn entry_path(entry: usize) -> Vec<PathElement> {
    vec![
        PathElement::Field("steps".to_string()),
        PathElement::Index(entry),
    ]
}

fn loop_field_path(entry: usize, field: &str) -> Vec<PathElement> {
    path_field(&path_field(&entry_path(entry), "loop"), field)
}

fn variable_path(entry: usize, k: usize, field: &str) -> Vec<PathElement> {
    path_field(
        &path_index(
            &path_field(&path_field(&entry_path(entry), "loop"), "variables"),
            k,
        ),
        field,
    )
}

fn output_path(entry: usize, k: usize) -> Vec<PathElement> {
    path_index(&path_field(&entry_path(entry), "outputVariables"), k)
}

fn body_path(entry: usize, j: usize) -> Vec<PathElement> {
    path_index(&loop_field_path(entry, "steps"), j)
}

/// `message` as a one-error `ModelValidation` at `path`.
fn error_at(path: &[PathElement], message: String) -> ModelError {
    let mut errors = ValidationErrors::default();
    errors.add(path, message);
    match errors.into_result("JobTemplate") {
        Err(e) => e,
        Ok(()) => unreachable!("one error was added"),
    }
}

fn is_path_type(ty: JobParameterType) -> bool {
    matches!(ty, JobParameterType::Path | JobParameterType::ListPath)
}

// ── The `Loop.*` layer ────────────────────────────────────────────

/// The `Loop.*` and `RawLoop.*` values of one iteration of a loop, as one more
/// layer of a body Step instance's symbol table (RFC 0013 §The `Loop.*`
/// scope).
pub(super) struct LoopLayer<'a> {
    pub(super) step_loop: &'a StepLoop,
    pub(super) iteration: u32,
    pub(super) values: &'a IndexMap<String, ExprValue>,
}

impl LoopLayer<'_> {
    /// Set `Loop.Iteration`, and `Loop.<V>` and `RawLoop.<V>` for each
    /// variable, in `symtab`. A variable whose type is or contains `path`
    /// gets only `RawLoop.<V>`: the model resolves nothing on the worker
    /// host, where `Loop.<V>` is derived from it.
    pub(super) fn set_symbols(&self, symtab: &mut SymbolTable) -> Result<(), SymbolTableError> {
        set_loop_symbols(symtab, self.step_loop, self.iteration, self.values)
    }

    /// Store each path-typed loop variable that bound Step `step`'s
    /// worker-resolved fields read, as `Loop.<V>` or `RawLoop.<V>`, in its
    /// `resolved_symtab` as `RawLoop.<V>`, a `string` (or list of strings) in
    /// the submission path context, and record it in
    /// [`job::Step::path_inputs`]. The Session sets `RawLoop.<V>` from it and
    /// maps `Loop.<V>` forward (RFC 0013 §`<LoopVariableDefinition>`).
    pub(super) fn store_path_values(
        &self,
        step: &mut job::Step,
        st: &StepTemplate,
    ) -> Result<(), ModelError> {
        let accessed = instantiate::collect_all_accessed_symbols(
            Some(&step.script),
            &step.step_environments,
            st.let_bindings.as_deref(),
        );
        let reads = |name: &str| {
            accessed.iter().any(|s| {
                [format!("Loop.{name}"), format!("RawLoop.{name}")]
                    .iter()
                    .any(|k| s == k || s.starts_with(&format!("{k}.")))
            })
        };
        let mut stored = Vec::new();
        for var in self.step_loop.variables() {
            if !is_path_type(var.job_param_type()) || !reads(var.name()) {
                continue;
            }
            let Some(value) = self.values.get(var.name()) else {
                continue;
            };
            let Some(strings) = binding::path_as_strings(value) else {
                continue;
            };
            stored.push((
                format!("RawLoop.{}", var.name()),
                strings,
                value.expr_type(),
            ));
        }
        if stored.is_empty() {
            return Ok(());
        }
        let mut symtab = match &step.resolved_symtab {
            Some(s) => s
                .to_symtab(PathFormat::Posix)
                .map_err(ModelError::DecodeValidation)?,
            None => SymbolTable::new(),
        };
        for (symbol, strings, ty) in stored {
            symtab.set(&symbol, strings)?;
            step.path_inputs.retain(|p| p.symbol != symbol);
            step.path_inputs.push(job::PathInput { symbol, ty });
        }
        step.resolved_symtab = Some(openjd_expr::SerializedSymbolTable::from_symtab(&symtab));
        Ok(())
    }
}

fn set_loop_symbols(
    symtab: &mut SymbolTable,
    l: &StepLoop,
    iteration: u32,
    values: &IndexMap<String, ExprValue>,
) -> Result<(), SymbolTableError> {
    symtab.set("Loop.Iteration", ExprValue::Int(i64::from(iteration)))?;
    for var in l.variables() {
        let Some(value) = values.get(var.name()) else {
            continue;
        };
        if !is_path_type(var.job_param_type()) {
            symtab.set(&format!("Loop.{}", var.name()), value.clone())?;
        }
        symtab.set(&format!("RawLoop.{}", var.name()), value.clone())?;
    }
    Ok(())
}

// ── Evaluating a declared value ───────────────────────────────────

/// Evaluate `fs` with target type `ty` (not optional, so EXPR reports a
/// `null` result as an error), and convert the result to `ty`. Errors are at `path`, prefixed
/// `subject: ` (such as `Loop variable 'N'`); one that has no caret ends
/// with a line holding the expression.
fn evaluate_typed(
    fs: &FormatString,
    ty: &ExprType,
    type_name: &str,
    subject: &str,
    symtab: &SymbolTable,
    budgets: EvalBudgets,
    path: &[PathElement],
) -> Result<ExprValue, ModelError> {
    let fail = |message: String| error_at(path, format!("{subject}: {message}"));
    let with_expression =
        |message: String| format!("{message}\n  {}", crate::outputs::shorten(fs.raw()));
    let value = fs
        .resolve_with(symtab, &budgets.fs_options().with_target_type(ty))
        .map_err(|e| fail(super::job_outputs::evaluation_message(&e)))?;
    if value.expr_type() == *ty {
        return Ok(in_eval_format(value));
    }
    let converted = value.clone().coerce(ty, PathFormat::Posix).map_err(|e| {
        fail(with_expression(format!(
            "value {} does not convert to {type_name}: {}",
            crate::outputs::quote(&value.to_display_string()),
            crate::outputs::shorten(&e)
        )))
    })?;
    Ok(in_eval_format(converted))
}

/// A path value, or a list of them, with every path in the Posix format the
/// model evaluates in (see `StepInputs`): a value read from `RawOutput` may be
/// a URI.
fn in_eval_format(value: ExprValue) -> ExprValue {
    match &value {
        ExprValue::Path {
            value: text,
            format: PathFormat::Uri,
            ..
        } => ExprValue::new_path(text.clone(), PathFormat::Posix),
        ExprValue::ListPath(items, PathFormat::Uri, _) => ExprValue::make_list(
            items
                .iter()
                .map(|t| ExprValue::new_path(t.clone(), PathFormat::Posix))
                .collect(),
            ExprType::PATH,
        )
        .unwrap_or(value),
        _ => value,
    }
}

/// `message` with a line holding `fs`, for a check that has no caret.
fn with_expression(message: String, fs: &FormatString) -> String {
    format!("{message}\n  {}", crate::outputs::shorten(fs.raw()))
}

/// The element type of a list output type.
fn element_type(ty: JobParameterType) -> ExprType {
    ty.expr_type()
        .list_element_type()
        .cloned()
        .unwrap_or(ExprType::NULLTYPE)
}

// ── The size limit ────────────────────────────────────────────────

/// The JSON serialization of `value`, as a value of a loop variable or loop
/// output is measured (RFC 0013 §Limits item 4): numbers, strings (a path's
/// text, a range expression's spelling), booleans, and arrays.
fn to_json(value: &ExprValue) -> serde_json::Value {
    use serde_json::Value as J;
    match value {
        ExprValue::Null => J::Null,
        ExprValue::Bool(b) => J::Bool(*b),
        ExprValue::Int(i) => J::from(*i),
        ExprValue::Float(f) => {
            let text = f.to_display_string();
            serde_json::from_str::<serde_json::Number>(&text)
                .map(J::Number)
                .unwrap_or(J::String(text))
        }
        ExprValue::String(s) => J::String(s.clone()),
        ExprValue::Path { value, .. } => J::String(value.clone()),
        v if v.is_list() => J::Array(
            v.list_elements()
                .unwrap_or_default()
                .iter()
                .map(to_json)
                .collect(),
        ),
        other => J::String(other.to_display_string()),
    }
}

/// The bytes of `value` as JSON.
fn json_len(value: &ExprValue) -> usize {
    to_json(value).to_string().len()
}

/// The bytes of a loop's current values (RFC 0013 §Limits item 4, plan A21):
/// each variable's value as JSON; each `LAST` output's value as JSON, or
/// nothing while it is `null` (no value yet); and each `APPEND` output's
/// elements, each as JSON, so an output with no elements yet counts nothing.
fn values_size(
    job_loop: &job::Loop,
    variables: &IndexMap<String, ExprValue>,
    outputs: &IndexMap<String, ExprValue>,
) -> usize {
    let vars: usize = variables.values().map(json_len).sum();
    let outs: usize = job_loop
        .output_variables
        .iter()
        .filter_map(|o| outputs.get(&o.name).map(|v| (o, v)))
        .map(|(o, v)| match (o.collect, v) {
            (_, ExprValue::Null) => 0,
            (OutputCollectMode::Append, v) => v
                .list_elements()
                .map_or_else(|| json_len(v), |e| e.iter().map(json_len).sum()),
            (_, v) => json_len(v),
        })
        .sum();
    vars + outs
}

/// Check RFC 0013 §Limits item 4, `CallerLimits::max_loop_value_bytes`, on
/// `variables` and `outputs`. The error is at the loop's path.
fn check_size(
    ctx: &ValidationContext,
    job_loop: &job::Loop,
    variables: &IndexMap<String, ExprValue>,
    outputs: &IndexMap<String, ExprValue>,
) -> Result<(), ModelError> {
    let Some(max) = ctx.caller_limits.max_loop_value_bytes else {
        return Ok(());
    };
    let total = values_size(job_loop, variables, outputs);
    if total > max {
        return Err(error_at(
            &entry_path(job_loop.entry_index),
            format!(
                "Loop '{}': its loop variable and loop output values total {total} bytes, over the limit of {max} bytes.",
                job_loop.name
            ),
        ));
    }
    Ok(())
}

// ── Loop binding ──────────────────────────────────────────────────

/// The `Steps.*` inputs of a loop's groups. The bound group reads none
/// (template validation guarantees it).
struct LoopGroups {
    enablement: BTreeSet<String>,
    start: BTreeSet<String>,
}

impl LoopGroups {
    fn of(l: &StepLoop) -> Self {
        let enablement = match &l.enable_if {
            Some(BoolOrFormatString::FormatString(fs)) => steps_symbols(fs.accessed_symbols()),
            _ => BTreeSet::new(),
        };
        let mut start = BTreeSet::new();
        for var in l.variables() {
            start.extend(steps_symbols(var.initial().accessed_symbols()));
        }
        if let Some(BoolOrFormatString::FormatString(fs)) = &l.loop_definition.while_condition {
            start.extend(steps_symbols(fs.accessed_symbols()));
        }
        Self { enablement, start }
    }

    fn all(&self) -> impl Iterator<Item = &String> {
        self.enablement.iter().chain(&self.start)
    }

    fn producers(&self) -> Vec<String> {
        let set: BTreeSet<String> = self
            .all()
            .filter_map(|s| s.split('.').nth(1).map(str::to_string))
            .collect();
        set.into_iter().collect()
    }
}

/// The job form of loop `l`, entry `entry` of the template's `steps`, before
/// anything of it is evaluated.
fn loop_shell(l: &StepLoop, entry: usize) -> job::Loop {
    job::Loop {
        name: l.name.clone(),
        description: l.description.as_ref().map(|d| d.0.clone()),
        entry_index: entry,
        dependencies: l.dependencies.as_ref().map(|deps| {
            deps.iter()
                .map(|d| job::StepDependency {
                    depends_on: d.depends_on.clone(),
                })
                .collect()
        }),
        condition: if l.loop_definition.until_condition.is_some() {
            LoopCondition::Until
        } else {
            LoopCondition::While
        },
        variables: l
            .variables()
            .iter()
            .map(|v| v.to_output_variable())
            .collect(),
        output_variables: l
            .output_variables_list()
            .iter()
            .map(|o| o.to_output_variable())
            .collect(),
        body: l.body().iter().map(|s| s.name.clone()).collect(),
        binding: LoopBinding::Skipped,
    }
}

/// The `Steps.*` values concrete at job creation for loop `l`.
pub(super) fn loop_creation_inputs(
    jt: &JobTemplate,
    l: &StepLoop,
    built: &[Option<Built>],
) -> Result<StepInputs, ModelError> {
    let groups = LoopGroups::of(l);
    binding::creation_inputs(
        jt,
        l.dependencies.as_deref().unwrap_or(&[]),
        groups.all(),
        built,
    )
}

/// Bind loop `l`, entry `entry` of the template's `steps`, as far as `inputs`
/// allow (RFC 0013 §Loop binding): `PendingInputs`, `Bound`, or `Skipped`; or
/// the first group that failed. `prior` is the loop from an earlier call,
/// whose known groups are kept; `None` at job creation.
pub(super) fn bind_loop(
    cx: &BindContext<'_>,
    entry: usize,
    l: &StepLoop,
    prior: Option<&job::Loop>,
    inputs: &StepInputs,
) -> Result<job::Loop, GroupFailure> {
    use MaterializationGroup::{Enablement, LoopBound, LoopStart as Start};
    let groups = LoopGroups::of(l);
    let mut job_loop = loop_shell(l, entry);
    let prior_pending = match prior.map(|p| &p.binding) {
        Some(LoopBinding::PendingInputs(p)) => Some(p),
        _ => None,
    };
    let mut symtab = cx.symtab.clone();
    let internal = |e: SymbolTableError| GroupFailure::abort(Enablement, ModelError::from(e));
    if cx.has_expr {
        // `Step.Name` in a loop's `enableIf` is the loop's name.
        symtab
            .set("Step.Name", ExprValue::String(l.name.clone()))
            .map_err(internal)?;
    }
    for sym in groups.all() {
        if let Some(v) = inputs.get(sym) {
            symtab.set(sym, v.clone()).map_err(internal)?;
        }
    }

    // ── Enablement ──
    if !prior_pending.is_some_and(|p| p.enablement.is_known()) {
        let waiting = missing(&groups.enablement, inputs);
        if !waiting.is_empty() {
            job_loop.binding = LoopBinding::PendingInputs(PendingLoopBinding {
                enablement: GroupState::Waiting { on: waiting },
                bound: GroupState::Waiting { on: Vec::new() },
                start: GroupState::Waiting {
                    on: missing(&groups.start, inputs),
                },
                producers: groups.producers(),
            });
            return Ok(job_loop);
        }
        let enabled = evaluate_enable_if(
            l.enable_if.as_ref(),
            &symtab,
            cx.budgets,
            &path_field(&entry_path(entry), "enableIf"),
        )
        .map_err(|e| GroupFailure::abort(Enablement, e))?;
        if !enabled {
            job_loop.binding = LoopBinding::Skipped;
            return Ok(job_loop);
        }
    }

    // ── Bound ──
    let max_iterations = match prior_pending.map(|p| &p.bound) {
        Some(GroupState::Known(n)) => *n,
        _ => evaluate_max_iterations(cx, l, entry, &symtab)
            .map_err(|e| GroupFailure::abort(LoopBound, e))?,
    };

    // ── Start ──
    let waiting = missing(&groups.start, inputs);
    if !waiting.is_empty() {
        job_loop.binding = LoopBinding::PendingInputs(PendingLoopBinding {
            enablement: GroupState::Known(()),
            bound: GroupState::Known(max_iterations),
            start: GroupState::Waiting { on: waiting },
            producers: groups.producers(),
        });
        return Ok(job_loop);
    }
    let start = evaluate_start(cx, l, entry, &job_loop, &symtab)
        .map_err(|e| GroupFailure::abort(Start, e))?;
    job_loop.binding = LoopBinding::Bound(BoundLoop {
        max_iterations,
        start,
    });
    Ok(job_loop)
}

/// The bound group: `maxIterations`, an integer from 1 to 1,000.
fn evaluate_max_iterations(
    cx: &BindContext<'_>,
    l: &StepLoop,
    entry: usize,
    symtab: &SymbolTable,
) -> Result<u32, ModelError> {
    let path = loop_field_path(entry, "maxIterations");
    let n = match &l.loop_definition.max_iterations {
        IntOrFormatString::Int(n) => *n,
        IntOrFormatString::FormatString(fs) => {
            let value = fs
                .resolve_with(
                    symtab,
                    &cx.budgets.fs_options().with_target_type(&ExprType::INT),
                )
                .map_err(|e| located(&path, &ModelError::Expression(e)))?;
            match value.clone().coerce(&ExprType::INT, PathFormat::Posix) {
                Ok(ExprValue::Int(n)) => n,
                _ => {
                    return Err(error_at(
                        &path,
                        with_expression(
                            format!(
                                "maxIterations must be an integer; it is {}.",
                                crate::outputs::quote(&value.to_display_string())
                            ),
                            fs,
                        ),
                    ))
                }
            }
        }
    };
    if !(1..=MAX_ITERATIONS).contains(&n) {
        let mut message = format!("maxIterations must be from 1 to 1000; it is {n}.");
        if let IntOrFormatString::FormatString(fs) = &l.loop_definition.max_iterations {
            message = with_expression(message, fs);
        }
        return Err(error_at(&path, message));
    }
    // 1 to 1,000 fits.
    Ok(n as u32)
}

/// The start group: each `initial` in declaration order, converted and
/// checked against its variable's constraints, the size limit, then the first
/// `while` with `Loop.Iteration` 0 and the initial values.
fn evaluate_start(
    cx: &BindContext<'_>,
    l: &StepLoop,
    entry: usize,
    job_loop: &job::Loop,
    symtab: &SymbolTable,
) -> Result<LoopStart, ModelError> {
    let mut initial = IndexMap::new();
    for (k, (var, decl)) in l.variables().iter().zip(&job_loop.variables).enumerate() {
        let path = variable_path(entry, k, "initial");
        let subject = format!("{VARIABLE_LABEL} '{}'", var.name());
        let value = evaluate_typed(
            var.initial(),
            &decl.param_type.expr_type(),
            decl.param_type.as_spec_str(),
            &subject,
            symtab,
            cx.budgets,
            &path,
        )?;
        OutputVariable::relabel(decl.check_constraints(&value), VARIABLE_LABEL)
            .map_err(|m| error_at(&path, with_expression(m, var.initial())))?;
        initial.insert(var.name().to_string(), value);
    }
    check_size(
        cx.ctx,
        job_loop,
        &initial,
        &job::step_loop_empty_outputs(job_loop),
    )?;
    let first_while = match &l.loop_definition.while_condition {
        None => None,
        Some(condition) => {
            let mut symtab = symtab.clone();
            set_loop_symbols(&mut symtab, l, 0, &initial)?;
            Some(evaluate_enable_if(
                Some(condition),
                &symtab,
                cx.budgets,
                &loop_field_path(entry, "while"),
            )?)
        }
    };
    Ok(LoopStart {
        initial,
        first_while,
    })
}

/// The loop at `loop_index` of `job`, with its template, or the caller error.
fn job_loop_and_template<'a>(
    job_template: &'a JobTemplate,
    job: &'a job::Job,
    loop_index: usize,
) -> Result<(&'a job::Loop, &'a StepLoop), ModelError> {
    let Some(job_loop) = job.loops.get(loop_index) else {
        let n = job.loops.len();
        return Err(ModelError::Compatibility(format!(
            "the Job has {n} loop{}, so it has no loop {loop_index}.",
            if n == 1 { "" } else { "s" }
        )));
    };
    match job_template.steps.get(job_loop.entry_index) {
        Some(StepEntry::Loop(l)) if l.name == job_loop.name => Ok((job_loop, l)),
        _ => Err(ModelError::Compatibility(format!(
            "loop {loop_index} of the Job is '{}', at entry {} of the template's steps, but the template has no such loop there; pass the template the Job was created from.",
            job_loop.name, job_loop.entry_index
        ))),
    }
}

/// A [`BindContext`] over the job symbol table `symtab`.
macro_rules! bind_context {
    ($jt:expr, $ctx:expr, $symtab:expr, $has_expr:expr, $limits:expr) => {
        BindContext {
            jt: $jt,
            ctx: $ctx,
            symtab: $symtab,
            has_expr: $has_expr,
            limits: $limits,
            budgets: EvalBudgets::from_ctx($ctx),
        }
    };
}

fn binding_misuse(name: &str, e: ModelError) -> BindingError {
    BindingError {
        step: name.to_string(),
        group: MaterializationGroup::Enablement,
        errors: vec![e],
    }
}

/// Advance the binding of pending loop `loop_index` of `job` with the
/// `Steps.*` values in `inputs` (RFC 0013 §Loop binding).
///
/// Evaluates every group of the loop that has not evaluated and whose inputs
/// are now concrete, in RFC 0013's order: enablement (`enableIf`) first, and
/// nothing else until it is `true`; then the bound (`maxIterations`, which
/// must be from 1 to 1,000); then the start group (each variable's `initial`
/// in declaration order, converted and checked against its type and
/// constraints, then `CallerLimits::max_loop_value_bytes` on the initial
/// values, then the first `while` with `Loop.Iteration` 0). It stops at the
/// first group that fails and returns it: the loop is `FAILED`, finally.
///
/// Returns the loop with more known, `Bound` once every group has evaluated,
/// or `Skipped` when `enableIf` is `false`. A bound loop whose first `while`
/// is `false` has zero iterations ([`BoundLoop::has_iterations`]); call
/// [`end_without_iterations`] once its dependencies are satisfied. Called on a
/// bound or skipped loop, or with no new inputs, it returns the loop
/// unchanged. The caller swaps the result into its Job.
///
/// `inputs` holds the loop's dependencies whose state is known (see
/// [`job::Loop::input_producers`]). `job_template`, `job_parameter_values`,
/// and `ctx` are the ones `job` was created from.
///
/// Caller errors are a `BindingError` whose error is a
/// [`ModelError::Compatibility`]: a `loop_index` past the Job's loops (with
/// `step` `#<index>`), or a template that does not have the loop at its
/// entry. Every other `BindingError` fails the loop.
pub fn advance_loop_binding(
    job_template: &JobTemplate,
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
    job: &job::Job,
    loop_index: usize,
    inputs: &StepInputs,
) -> Result<job::Loop, BindingError> {
    let name = job
        .loops
        .get(loop_index)
        .map_or(format!("#{loop_index}"), |l| l.name.clone());
    let (job_loop, l) = job_loop_and_template(job_template, job, loop_index)
        .map_err(|e| binding_misuse(&name, e))?;
    if !matches!(job_loop.binding, LoopBinding::PendingInputs(_)) {
        return Ok(job_loop.clone());
    }
    let (symtab, has_expr) = job_symbol_table(job_parameter_values, ctx, &job.name)
        .map_err(|e| binding_misuse(&name, e))?;
    let limits = EffectiveLimits::from_context(ctx);
    let cx = bind_context!(job_template, ctx, &symtab, has_expr, &limits);
    bind_loop(&cx, job_loop.entry_index, l, Some(job_loop), inputs)
        .map_err(|f| f.into_binding_error(&name))
}

// ── Iterations ────────────────────────────────────────────────────

/// The bound loop at `loop_index`, its template, and its bound, checked
/// against `progress`; or the caller error.
fn running_loop<'a>(
    job_template: &'a JobTemplate,
    job: &'a job::Job,
    loop_index: usize,
    progress: &LoopProgress,
) -> Result<(&'a job::Loop, &'a StepLoop, &'a BoundLoop), ModelError> {
    let (job_loop, l) = job_loop_and_template(job_template, job, loop_index)?;
    let LoopBinding::Bound(bound) = &job_loop.binding else {
        return Err(ModelError::Compatibility(format!(
            "loop '{}' is not bound, so it has no iterations.",
            job_loop.name
        )));
    };
    if !bound.has_iterations() {
        return Err(ModelError::Compatibility(format!(
            "loop '{}' has zero iterations: its first while is false; end it with end_without_iterations.",
            job_loop.name
        )));
    }
    if progress.iteration >= bound.max_iterations {
        return Err(ModelError::Compatibility(format!(
            "loop '{}' has maxIterations {}, so it has no iteration {}.",
            job_loop.name, bound.max_iterations, progress.iteration
        )));
    }
    check_progress(job_loop, progress)?;
    Ok((job_loop, l, bound))
}

/// Check that `progress` has the shape a loop function gave it: exactly the
/// loop's variables, each of its declared type, and exactly its outputs, a
/// `LAST` output `null` or of its type and an `APPEND` output a list of its
/// type. A value of the right type is not re-checked against its
/// constraints.
fn check_progress(job_loop: &job::Loop, progress: &LoopProgress) -> Result<(), ModelError> {
    let refuse = |what: String| {
        ModelError::Compatibility(format!(
            "the progress of loop '{}' {what}; pass the progress the loop functions returned.",
            job_loop.name
        ))
    };
    let has_type = |value: &ExprValue, decl: &OutputVariable| {
        value.expr_type() == decl.param_type.expr_type()
            || (value.is_list()
                && value.list_elements().is_some_and(|e| e.is_empty())
                && decl.param_type.expr_type().list_element_type().is_some())
    };
    for decl in &job_loop.variables {
        match progress.values.get(&decl.name) {
            None => return Err(refuse(format!("has no value for variable '{}'", decl.name))),
            Some(v) if !has_type(v, decl) => {
                return Err(refuse(format!(
                    "has a value of type {} for variable '{}', which is {}",
                    v.expr_type(),
                    decl.name,
                    decl.param_type.as_spec_str()
                )))
            }
            Some(_) => {}
        }
    }
    if let Some(name) = progress
        .values
        .keys()
        .find(|k| !job_loop.variables.iter().any(|v| &v.name == *k))
    {
        return Err(refuse(format!(
            "has a value for '{name}', which is not a variable of the loop"
        )));
    }
    for decl in &job_loop.output_variables {
        let append = decl.collect == OutputCollectMode::Append;
        match progress.outputs.get(&decl.name) {
            None => return Err(refuse(format!("has no value for output '{}'", decl.name))),
            Some(ExprValue::Null) if !append => {}
            Some(v) if has_type(v, decl) => {}
            Some(v) => {
                return Err(refuse(format!(
                    "has a value of type {} for {} output '{}', which is {}",
                    v.expr_type(),
                    decl.collect.as_spec_str(),
                    decl.name,
                    decl.param_type.as_spec_str()
                )))
            }
        }
    }
    if let Some(name) = progress
        .outputs
        .keys()
        .find(|k| job_loop.output_variable(k).is_none())
    {
        return Err(refuse(format!(
            "has a value for '{name}', which is not an output of the loop"
        )));
    }
    Ok(())
}

/// The body Steps' indices in dependency order, ties broken by body order.
/// Validation rejects a cycle; body order is the fallback.
fn body_order(l: &StepLoop) -> Vec<usize> {
    let body = l.body();
    let index_of = |name: &str| body.iter().position(|s| s.name == name);
    let mut in_degree = vec![0usize; body.len()];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); body.len()];
    for (j, st) in body.iter().enumerate() {
        for dep in st.dependencies.iter().flatten() {
            if let Some(d) = index_of(&dep.depends_on) {
                dependents[d].push(j);
                in_degree[j] += 1;
            }
        }
    }
    let mut ready: BTreeSet<usize> = (0..body.len()).filter(|&j| in_degree[j] == 0).collect();
    let mut order = Vec::with_capacity(body.len());
    while let Some(j) = ready.pop_first() {
        order.push(j);
        for &k in &dependents[j] {
            in_degree[k] -= 1;
            if in_degree[k] == 0 {
                ready.insert(k);
            }
        }
    }
    if order.len() == body.len() {
        order
    } else {
        (0..body.len()).collect()
    }
}

fn body_site<'a>(
    l: &'a StepLoop,
    entry: usize,
    j: usize,
    iteration: u32,
    values: &'a IndexMap<String, ExprValue>,
) -> StepSite<'a> {
    StepSite {
        st: &l.body()[j],
        loc: StepLocation {
            entry,
            body: Some(j),
        },
        path: body_path(entry, j),
        loop_layer: Some(LoopLayer {
            step_loop: l,
            iteration,
            values,
        }),
    }
}

/// Start iteration `progress.iteration` of bound loop `loop_index` of `job`
/// (RFC 0013 §Iterations): create an instance of every body Step, with every
/// `Loop.*` and `RawLoop.*` symbol concrete for the iteration, and evaluate
/// each instance's groups whose inputs are concrete now, as `create_job` does
/// for a top-level Step, including the carried-forward checks of its script
/// and Step Environments.
///
/// Instances are bound in body dependency order, so an instance skipped by
/// its `enableIf`, or whose Tasks are known, makes its symbols concrete for
/// the instances that read them. An instance all of whose groups evaluate is
/// `Bound`, one whose `enableIf` is `false` is `Skipped`, and any other is
/// `PendingInputs`, for [`advance_body_step_binding`]. A bound instance whose
/// script reads a path-typed loop variable has it in its `resolved_symtab` as
/// `RawLoop.<V>`, recorded in [`job::Step::path_inputs`]. A bound instance
/// that reads `Steps.*` in its script or Step Environments needs
/// [`attach_step_inputs`](super::attach_step_inputs) before its Tasks run.
///
/// `inputs` holds the loop's dependencies (each that has succeeded or been
/// skipped). `committed_task_count` is the number of Tasks of every Step and
/// earlier instance whose Tasks are known, for `CallerLimits::max_task_count`;
/// each instance's Tasks are added to it as they become known.
///
/// An error from an instance's group fails the instance, and so the loop,
/// finally (RFC 0013 §Failure). Its `BindingError::step` is the instance's
/// name, `<Loop>[<N>].<Step>`. Caller errors are a `BindingError` whose error
/// is a [`ModelError::Compatibility`]: a loop that is not bound or has zero
/// iterations, an iteration at or past `maxIterations`, a `progress` whose
/// variables or outputs are not the loop's or not of their declared types,
/// and the errors of [`advance_loop_binding`].
#[allow(clippy::too_many_arguments)]
pub fn start_iteration(
    job_template: &JobTemplate,
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
    job: &job::Job,
    loop_index: usize,
    progress: &LoopProgress,
    inputs: &StepInputs,
    committed_task_count: u64,
) -> Result<LoopIteration, BindingError> {
    let name = job
        .loops
        .get(loop_index)
        .map_or(format!("#{loop_index}"), |l| l.name.clone());
    let (job_loop, l, _) = running_loop(job_template, job, loop_index, progress)
        .map_err(|e| binding_misuse(&name, e))?;
    let (symtab, has_expr) = job_symbol_table(job_parameter_values, ctx, &job.name)
        .map_err(|e| binding_misuse(&name, e))?;
    let limits = EffectiveLimits::from_context(ctx);
    let cx = bind_context!(job_template, ctx, &symtab, has_expr, &limits);
    let entry = job_loop.entry_index;
    let n = progress.iteration;
    let mut built: Vec<Option<job::Step>> = vec![None; l.body().len()];
    let mut committed = committed_task_count;
    for j in body_order(l) {
        let site = body_site(l, entry, j, n, &progress.values);
        let instance = job::step_loop_instance_name(&l.name, n, &site.st.name);
        let step_inputs = iteration_inputs(site.st, l, inputs, &built)
            .map_err(|e| binding_misuse(&instance, e))?;
        let step = binding::bind_step(&cx, &site, None, &step_inputs, committed)
            .map_err(|f| f.into_binding_error(&instance))?;
        if let Some(tasks) = step.tasks_known() {
            committed = committed.saturating_add(tasks);
        }
        built[j] = Some(step);
    }
    let steps = built.into_iter().flatten().collect();
    Ok(LoopIteration {
        loop_name: l.name.clone(),
        index: n,
        steps,
    })
}

/// `inputs` (the loop's dependencies) and, for the body Steps `st` depends on
/// that are already instantiated, a skipped instance's `null`s and the
/// `TaskParam`s of one whose Tasks are known.
fn iteration_inputs(
    st: &StepTemplate,
    l: &StepLoop,
    inputs: &StepInputs,
    built: &[Option<job::Step>],
) -> Result<StepInputs, ModelError> {
    let mut out = inputs.clone();
    for dep in st.dependencies.iter().flatten() {
        let Some(producer) = l
            .body()
            .iter()
            .position(|s| s.name == dep.depends_on)
            .and_then(|k| built[k].as_ref())
        else {
            continue;
        };
        if matches!(producer.binding, StepBinding::Skipped) {
            out.add_producer(producer, job::ProducerState::Skipped)?;
        } else if producer.tasks_known().is_some() {
            out.add_producer(producer, job::ProducerState::TasksKnown)?;
        }
    }
    Ok(out)
}

/// Advance the binding of pending body Step instance `body_index` of
/// `iteration` (RFC 0013 §Iterations item 2), as
/// [`advance_step_binding`](super::advance_step_binding) advances a Step: the
/// same groups, in the same order, with the iteration's `Loop.*` values.
/// Returns the instance with more known, `Bound`, or `Skipped`; unchanged when
/// it is not pending. The caller swaps the result into `iteration.steps`.
///
/// `inputs` holds, on every call, the loop's dependencies and each instance
/// of the same iteration in the instance's
/// [`input_producers()`](job::Step::input_producers) whose state is known.
/// `progress` is the loop's progress for the iteration, as given to
/// [`start_iteration`]. `committed_task_count` is as for `start_iteration`.
///
/// An error fails the instance, and so the loop, finally; its
/// `BindingError::step` is `<Loop>[<N>].<Step>`. Caller errors are as for
/// `start_iteration`, and a `body_index` past the body or an `iteration` that
/// is not `progress.iteration` of this loop.
#[allow(clippy::too_many_arguments)]
pub fn advance_body_step_binding(
    job_template: &JobTemplate,
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
    job: &job::Job,
    loop_index: usize,
    progress: &LoopProgress,
    iteration: &LoopIteration,
    body_index: usize,
    inputs: &StepInputs,
    committed_task_count: u64,
) -> Result<job::Step, BindingError> {
    let name = job
        .loops
        .get(loop_index)
        .map_or(format!("#{loop_index}"), |l| l.name.clone());
    let (job_loop, l, _) = running_loop(job_template, job, loop_index, progress)
        .map_err(|e| binding_misuse(&name, e))?;
    if iteration.loop_name != job_loop.name || iteration.index != progress.iteration {
        return Err(binding_misuse(
            &name,
            ModelError::Compatibility(format!(
                "the iteration is {}[{}], but the progress is of loop '{}' at iteration {}.",
                iteration.loop_name, iteration.index, job_loop.name, progress.iteration
            )),
        ));
    }
    let (Some(step), Some(st)) = (iteration.steps.get(body_index), l.body().get(body_index)) else {
        return Err(binding_misuse(
            &name,
            ModelError::Compatibility(format!(
                "loop '{}' has {} body Steps, so it has no body Step {body_index}.",
                job_loop.name,
                l.body().len()
            )),
        ));
    };
    let instance = job::step_loop_instance_name(&l.name, iteration.index, &st.name);
    if step.name != st.name {
        return Err(binding_misuse(
            &instance,
            ModelError::Compatibility(format!(
                "instance {body_index} of the iteration is '{}', but the template's body Step is '{}'.",
                step.name, st.name
            )),
        ));
    }
    if !matches!(step.binding, StepBinding::PendingInputs(_)) {
        return Ok(step.clone());
    }
    let (symtab, has_expr) = job_symbol_table(job_parameter_values, ctx, &job.name)
        .map_err(|e| binding_misuse(&instance, e))?;
    let limits = EffectiveLimits::from_context(ctx);
    let cx = bind_context!(job_template, ctx, &symtab, has_expr, &limits);
    let site = body_site(
        l,
        job_loop.entry_index,
        body_index,
        iteration.index,
        &progress.values,
    );
    binding::bind_step(&cx, &site, Some(step), inputs, committed_task_count)
        .map_err(|f| f.into_binding_error(&instance))
}

// ── The iteration boundary ────────────────────────────────────────

/// The format strings a loop's boundary evaluates: each output's `value`,
/// `until`, each `next`, and `while`.
fn boundary_format_strings(l: &StepLoop) -> Vec<&FormatString> {
    let mut out: Vec<&FormatString> = l
        .output_variables_list()
        .iter()
        .map(|o| o.value())
        .collect();
    if let Some(BoolOrFormatString::FormatString(fs)) = &l.loop_definition.until_condition {
        out.push(fs);
    }
    out.extend(l.variables().iter().map(|v| v.next()));
    if let Some(BoolOrFormatString::FormatString(fs)) = &l.loop_definition.while_condition {
        out.push(fs);
    }
    out
}

/// Evaluate `condition` (`until` or `while`) as a `bool` at `path`.
fn evaluate_condition(
    condition: &BoolOrFormatString,
    symtab: &SymbolTable,
    budgets: EvalBudgets,
    path: &[PathElement],
) -> Result<bool, ModelError> {
    evaluate_enable_if(Some(condition), symtab, budgets, path)
}

/// Evaluate the iteration boundary of iteration `progress.iteration` of bound
/// loop `loop_index` of `job` (RFC 0013 §The iteration boundary). Call it
/// when every body Step instance of the iteration has succeeded or been
/// skipped.
///
/// With `Loop.*` holding the iteration's values, it evaluates, in order:
///
/// 1. each loop output's `value`, converted to its type (under `APPEND` its
///    element type) and checked: under `LAST` every constraint property,
///    under `APPEND` the `item` constraints on the new element and
///    `maxLength` on the grown list; then `CallerLimits::max_loop_value_bytes`;
/// 2. `until`, if the loop has it: when `true` the loop ends here, no `next`
///    is evaluated, and it succeeds once each `APPEND` output's `minLength`
///    holds;
/// 3. every variable's `next`, each reading the iteration's values, converted
///    and checked against its constraints; then the size limit on the new
///    values and the outputs so far;
/// 4. `while`, if the loop has it, with the new values and `Loop.Iteration`
///    one more: when `false` the loop ends as in step 2;
/// 5. the bound: if the next iteration would be the `maxIterations`-th plus
///    one, the loop fails, with a message that names `maxIterations` and its
///    value.
///
/// Returns [`BoundaryOutcome::Continue`] with the next iteration's progress,
/// or [`BoundaryOutcome::Succeeded`] with the committed outputs. The first
/// error stops the evaluation and fails the loop, finally: a `fail(...)`
/// call, a `null` result, a value that does not convert, a violated
/// constraint, `minLength`, the size limit, or the bound. Its message names
/// the field (by its path), the expression, and the problem, and the
/// [`LoopError`] names the iteration.
///
/// `inputs` holds the loop's dependencies and every body Step instance of
/// the iteration, each added with
/// [`StepInputs::add_producer`](job::StepInputs::add_producer) as succeeded
/// or skipped. Caller errors are a `LoopError` whose error is a
/// [`ModelError::Compatibility`]: those of [`start_iteration`], and a symbol
/// a boundary expression reads that is not concrete in `inputs`.
pub fn evaluate_iteration_boundary(
    job_template: &JobTemplate,
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
    job: &job::Job,
    loop_index: usize,
    progress: &LoopProgress,
    inputs: &StepInputs,
) -> Result<BoundaryOutcome, LoopError> {
    let n = progress.iteration;
    let name = job
        .loops
        .get(loop_index)
        .map_or(format!("#{loop_index}"), |l| l.name.clone());
    let fail = |field: LoopErrorField, e: ModelError| LoopError {
        loop_name: name.clone(),
        iteration: Some(n),
        field,
        errors: vec![e],
    };
    let (job_loop, l, bound) = running_loop(job_template, job, loop_index, progress)
        .map_err(|e| fail(LoopErrorField::Inputs, e))?;
    let entry = job_loop.entry_index;
    let budgets = EvalBudgets::from_ctx(ctx);

    // The job table and every `Steps.*` symbol the boundary reads.
    let (mut base, _) = job_symbol_table(job_parameter_values, ctx, &job.name)
        .map_err(|e| fail(LoopErrorField::Inputs, e))?;
    let mut read = BTreeSet::new();
    for fs in boundary_format_strings(l) {
        read.extend(fs.accessed_symbols().iter().filter_map(|s| steps_key(s)));
    }
    for sym in &read {
        match inputs.get(sym) {
            Some(v) => base
                .set(sym, v.clone())
                .map_err(|e| fail(LoopErrorField::Inputs, e.into()))?,
            None if inputs.is_concrete(sym) => {}
            None => {
                return Err(fail(
                    LoopErrorField::Inputs,
                    ModelError::Compatibility(format!(
                        "loop '{}' reads '{sym}' at its iteration boundary, which is not concrete in the given inputs.",
                        l.name
                    )),
                ))
            }
        }
    }
    let mut symtab = base.clone();
    set_loop_symbols(&mut symtab, l, n, &progress.values)
        .map_err(|e| fail(LoopErrorField::Inputs, e.into()))?;

    // 1. Loop outputs.
    let mut outputs = progress.outputs.clone();
    for (k, (ov, decl)) in l
        .output_variables_list()
        .iter()
        .zip(&job_loop.output_variables)
        .enumerate()
    {
        let field = LoopErrorField::OutputValue(ov.name().to_string());
        let path = path_field(&output_path(entry, k), "value");
        let subject = format!("{OUTPUT_LABEL} '{}'", ov.name());
        let append = decl.collect == OutputCollectMode::Append;
        let (ty, type_name) = if append {
            let elem = element_type(decl.param_type);
            let name = elem.to_string();
            (elem, name)
        } else {
            (
                decl.param_type.expr_type(),
                decl.param_type.as_spec_str().to_string(),
            )
        };
        let value = evaluate_typed(
            ov.value(),
            &ty,
            &type_name,
            &subject,
            &symtab,
            budgets,
            &path,
        )
        .map_err(|e| fail(field.clone(), e))?;
        let check = |r: Result<(), String>| {
            OutputVariable::relabel(r, OUTPUT_LABEL).map_err(|m| {
                fail(
                    field.clone(),
                    error_at(&path, with_expression(m, ov.value())),
                )
            })
        };
        let new = if append {
            check(decl.check_element(&value))?;
            let mut elements = outputs
                .get(ov.name())
                .and_then(ExprValue::list_elements)
                .unwrap_or_default();
            elements.push(value);
            check(decl.check_length(elements.len(), false))?;
            ExprValue::make_list(elements, ty)
                .map_err(|e| fail(field.clone(), ModelError::Expression(e)))?
        } else {
            check(decl.check_constraints(&value))?;
            value
        };
        outputs.insert(ov.name().to_string(), new);
    }
    check_size(ctx, job_loop, &progress.values, &outputs)
        .map_err(|e| fail(LoopErrorField::ValueSize, e))?;

    let succeed = |outputs: IndexMap<String, ExprValue>, ended_by: LoopEnd| {
        check_min_lengths(job_loop, entry, &outputs).map_err(|(f, e)| fail(f, e))?;
        Ok(BoundaryOutcome::Succeeded(LoopResult {
            iterations: n + 1,
            ended_by,
            outputs,
        }))
    };

    // 2. until.
    if let Some(until) = &l.loop_definition.until_condition {
        let done = evaluate_condition(until, &symtab, budgets, &loop_field_path(entry, "until"))
            .map_err(|e| fail(LoopErrorField::Until, e))?;
        if done {
            return succeed(outputs, LoopEnd::Until);
        }
    }

    // 3. next, each reading iteration N's values.
    let mut values = IndexMap::new();
    for (k, (var, decl)) in l.variables().iter().zip(&job_loop.variables).enumerate() {
        let field = LoopErrorField::Next(var.name().to_string());
        let path = variable_path(entry, k, "next");
        let subject = format!("{VARIABLE_LABEL} '{}'", var.name());
        let value = evaluate_typed(
            var.next(),
            &decl.param_type.expr_type(),
            decl.param_type.as_spec_str(),
            &subject,
            &symtab,
            budgets,
            &path,
        )
        .map_err(|e| fail(field.clone(), e))?;
        OutputVariable::relabel(decl.check_constraints(&value), VARIABLE_LABEL).map_err(|m| {
            fail(
                field.clone(),
                error_at(&path, with_expression(m, var.next())),
            )
        })?;
        values.insert(var.name().to_string(), value);
    }
    check_size(ctx, job_loop, &values, &outputs).map_err(|e| fail(LoopErrorField::ValueSize, e))?;

    // 4. while, with the new values and Loop.Iteration N + 1.
    if let Some(condition) = &l.loop_definition.while_condition {
        let mut next_symtab = base;
        set_loop_symbols(&mut next_symtab, l, n + 1, &values)
            .map_err(|e| fail(LoopErrorField::While, e.into()))?;
        let again = evaluate_condition(
            condition,
            &next_symtab,
            budgets,
            &loop_field_path(entry, "while"),
        )
        .map_err(|e| fail(LoopErrorField::While, e))?;
        if !again {
            return succeed(outputs, LoopEnd::While);
        }
    }

    // 5. The bound.
    if n + 1 >= bound.max_iterations {
        let mut message = format!(
            "Loop '{}' has run maxIterations ({}) iterations without ending, so it fails.",
            l.name, bound.max_iterations
        );
        if let IntOrFormatString::FormatString(fs) = &l.loop_definition.max_iterations {
            message = with_expression(message, fs);
        }
        return Err(fail(
            LoopErrorField::MaxIterations,
            error_at(&loop_field_path(entry, "maxIterations"), message),
        ));
    }
    Ok(BoundaryOutcome::Continue(LoopProgress {
        iteration: n + 1,
        values,
        outputs,
    }))
}

/// The `minLength` of each `APPEND` loop output, checked when the loop ends
/// (RFC 0013 §`<LoopOutputVariableDefinition>` item 6).
fn check_min_lengths(
    job_loop: &job::Loop,
    entry: usize,
    outputs: &IndexMap<String, ExprValue>,
) -> Result<(), (LoopErrorField, ModelError)> {
    for (k, decl) in job_loop.output_variables.iter().enumerate() {
        if decl.collect != OutputCollectMode::Append {
            continue;
        }
        let len = outputs
            .get(&decl.name)
            .and_then(ExprValue::list_elements)
            .map_or(0, |e| e.len());
        OutputVariable::relabel(decl.check_length(len, true), OUTPUT_LABEL).map_err(|m| {
            (
                LoopErrorField::MinLength(decl.name.clone()),
                error_at(&path_field(&output_path(entry, k), "minLength"), m),
            )
        })?;
    }
    Ok(())
}

/// End bound loop `loop_index` of `job`, whose first `while` is `false`, once
/// its dependencies are satisfied (RFC 0013 §Loop binding): check each
/// `APPEND` output's `minLength` against `[]`, and succeed with zero
/// iterations, each `LAST` output `null` and each `APPEND` output `[]`.
///
/// A violated `minLength` fails the loop, finally. This check is not a
/// materialization group, and is never part of `create_job`. Caller errors,
/// a [`LoopError`] whose error is a [`ModelError::Compatibility`]: a
/// `loop_index` past the Job's loops, or a loop that is not bound with a
/// `false` first `while`.
pub fn end_without_iterations(job: &job::Job, loop_index: usize) -> Result<LoopResult, LoopError> {
    let name = job
        .loops
        .get(loop_index)
        .map_or(format!("#{loop_index}"), |l| l.name.clone());
    let misuse = |message: String| LoopError {
        loop_name: name.clone(),
        iteration: None,
        field: LoopErrorField::Inputs,
        errors: vec![ModelError::Compatibility(message)],
    };
    let Some(job_loop) = job.loops.get(loop_index) else {
        let n = job.loops.len();
        return Err(misuse(format!(
            "the Job has {n} loop{}, so it has no loop {loop_index}.",
            if n == 1 { "" } else { "s" }
        )));
    };
    match &job_loop.binding {
        LoopBinding::Bound(b) if !b.has_iterations() => {}
        _ => {
            return Err(misuse(format!(
                "loop '{}' is not a bound loop whose first while is false.",
                job_loop.name
            )))
        }
    }
    let outputs = job::step_loop_empty_outputs(job_loop);
    check_min_lengths(job_loop, job_loop.entry_index, &outputs).map_err(|(field, e)| {
        LoopError {
            loop_name: name.clone(),
            iteration: None,
            field,
            errors: vec![e],
        }
    })?;
    Ok(LoopResult {
        iterations: 0,
        ended_by: LoopEnd::ZeroIterations,
        outputs,
    })
}

// ── Consumption ───────────────────────────────────────────────────

impl job::Loop {
    /// The `Steps.*` symbols (`Steps.<Step>.<Member>.<Name>`, sorted) that
    /// one of the loop's own expressions references, found by symbol
    /// collection, whether or not a branch reading them is taken.
    ///
    /// RFC 0013 §Retries and re-runs counts `while`, `until`, each `next`,
    /// each `initial`, and each loop output `value` as consumers of every
    /// `Steps.*` output they reference, once evaluated; a scheduler that
    /// allows re-runs marks these consumed when it evaluates the field, so it
    /// can apply RFC 0012 §Retries and re-runs rule 3. `enableIf` is a
    /// materialization expression, consumed as a Step's is.
    ///
    /// `job_template` is the one the Job was created from. Errors, each a
    /// [`ModelError::Compatibility`], when it does not have the loop at its
    /// entry, or the loop has no such variable or output.
    pub fn expression_inputs(
        &self,
        job_template: &JobTemplate,
        expression: &LoopExpression,
    ) -> Result<Vec<String>, ModelError> {
        let l = match job_template.steps.get(self.entry_index) {
            Some(StepEntry::Loop(l)) if l.name == self.name => l,
            _ => {
                return Err(ModelError::Compatibility(format!(
                    "the template has no loop '{}' at entry {} of its steps; pass the template the Job was created from.",
                    self.name, self.entry_index
                )))
            }
        };
        let unknown = |what: &str, n: &str| {
            ModelError::Compatibility(format!("loop '{}' has no {what} '{n}'.", self.name))
        };
        let condition = |c: &Option<BoolOrFormatString>| match c {
            Some(BoolOrFormatString::FormatString(fs)) => fs.accessed_symbols(),
            _ => Default::default(),
        };
        let symbols: std::collections::HashSet<String> = match expression {
            LoopExpression::EnableIf => condition(&l.enable_if),
            LoopExpression::While => condition(&l.loop_definition.while_condition),
            LoopExpression::Until => condition(&l.loop_definition.until_condition),
            LoopExpression::Initial(n) | LoopExpression::Next(n) => {
                let var = l
                    .variables()
                    .iter()
                    .find(|v| v.name() == n)
                    .ok_or_else(|| unknown("variable", n))?;
                let fs = if matches!(expression, LoopExpression::Initial(_)) {
                    var.initial()
                } else {
                    var.next()
                };
                fs.accessed_symbols()
            }
            LoopExpression::OutputValue(n) => l
                .output_variables_list()
                .iter()
                .find(|o| o.name() == n)
                .ok_or_else(|| unknown("output", n))?
                .value()
                .accessed_symbols(),
        };
        let set: BTreeSet<String> = symbols.iter().filter_map(|s| steps_key(s)).collect();
        Ok(set.into_iter().collect())
    }
}
