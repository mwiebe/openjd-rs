// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Job output evaluation (`CONTROL_FLOW`, RFC 0010 §Job outputs).
//!
//! A job output's `value` is evaluated once every `Steps.*` symbol it reads
//! is concrete: at Job creation for one that reads none, or only the `null`s
//! of a Step skipped then, or only `TaskParam` and `TaskRawParam` of Steps
//! whose Tasks are known then; later, by the caller, through
//! [`evaluate_job_output`]. Either way the value is converted to the declared
//! type and checked against the declared constraint properties, unless it is
//! `null`, and a failure fails the Job.

use std::collections::BTreeSet;

use openjd_expr::path_mapping::PathFormat;
use openjd_expr::symbol_table::SymbolTable;
use openjd_expr::{ExprType, ExprValue};

use super::EvalBudgets;
use crate::error::{path_field, path_index, ModelError, PathElement, ValidationErrors};
use crate::job::{self, binding::steps_key, JobOutput, JobOutputValue, StepBinding, StepInputs};
use crate::template::JobTemplate;
use crate::types::{JobParameterValues, ModelExtension, ValidationContext};

/// How a job output names itself in its messages.
const LABEL: &str = "Job output";

/// The path of output `index`'s `value`.
fn value_path(index: usize) -> Vec<PathElement> {
    path_field(
        &path_index(&path_field(&[], "jobOutputDefinitions"), index),
        "value",
    )
}

/// The `Steps.*` symbols `output`'s `value` reads, as
/// `Steps.<Step>.<Member>.<Name>`, sorted.
fn steps_inputs(output: &JobOutput) -> BTreeSet<String> {
    output
        .value
        .accessed_symbols()
        .iter()
        .filter_map(|s| steps_key(s))
        .collect()
}

/// The `Steps.*` symbols that job output `output_index` of `job` reads, as
/// `Steps.<Step>.<Member>.<Name>` (a property read from one, such as
/// `.name`, is not part of it), sorted and without repeats.
///
/// These are what the output waits on: it can be evaluated with
/// [`evaluate_job_output`] once each is concrete in the caller's
/// [`StepInputs`](job::StepInputs) (see
/// [`StepInputs::is_concrete`](job::StepInputs::is_concrete)). An
/// `Output` or `RawOutput` symbol is concrete once its Step has succeeded or
/// been skipped, and a `TaskParam` or `TaskRawParam` symbol once its Step's
/// Tasks are known or it has been skipped. Empty for an output that reads no
/// `Steps.*` symbol, which `create_job` has already evaluated, and for an
/// `output_index` past the Job's outputs.
pub fn job_output_inputs(job: &job::Job, output_index: usize) -> Vec<String> {
    job.outputs
        .as_deref()
        .and_then(|outputs| outputs.get(output_index))
        .map(|output| steps_inputs(output).into_iter().collect())
        .unwrap_or_default()
}

/// Evaluate job output `output_index` of `job` with the `Steps.*` values in
/// `inputs` (RFC 0010 §Job outputs).
///
/// The `value` is resolved against the job symbol table (`Param.*`,
/// `RawParam.*`, `Job.Name`) and the `Steps.*` symbols from `inputs`, with
/// the declared type's expression type, made optional, as its target type.
/// A `null` result is `Ok(None)`: the output has no value, which is not an
/// error, and its constraint properties are not checked. Any other result is
/// converted to the declared type with EXPR's implicit coercion and checked
/// against the declared constraint properties, and returned as `Ok(Some)`.
/// The caller stores the result in
/// [`JobOutput::evaluated`](job::JobOutput::evaluated), for example with
/// `JobOutputValue::from`.
///
/// `job_template`, `job_parameter_values`, and `ctx` are the ones `job` was
/// created from. `inputs` must hold every producer whose symbols the output
/// reads (see [`job_output_inputs`]), each added with
/// [`StepInputs::add_producer`](job::StepInputs::add_producer). A producer
/// need not be a dependency of anything.
///
/// An `Err` that is a [`ModelError::ModelValidation`] at
/// `jobOutputDefinitions[i] -> value` fails the Job: the evaluation failed
/// (including a `fail(...)` call), the value does not convert to the declared
/// type, or it violates a constraint property. The message names the output
/// and, for an evaluation error, quotes the expression with a caret. An
/// `Err` that is a [`ModelError::Compatibility`] is a caller error, which
/// says nothing about the Job: an `output_index` past the Job's outputs, a
/// template whose output at that index has another name, or a symbol the
/// output reads that is not concrete in `inputs` (the message names it).
pub fn evaluate_job_output(
    job_template: &JobTemplate,
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
    job: &job::Job,
    output_index: usize,
    inputs: &StepInputs,
) -> Result<Option<ExprValue>, ModelError> {
    let outputs = job.outputs.as_deref().unwrap_or(&[]);
    let Some(output) = outputs.get(output_index) else {
        let n = outputs.len();
        let noun = if n == 1 { "job output" } else { "job outputs" };
        return Err(ModelError::Compatibility(format!(
            "the Job has {n} {noun}, so it has no job output {output_index}."
        )));
    };
    let name = &output.declaration.name;
    let template_name = job_template
        .job_output_definitions_list()
        .get(output_index)
        .map(|d| d.name());
    if template_name != Some(name.as_str()) {
        let theirs = match template_name {
            Some(t) => format!("the template's is '{t}'"),
            None => "the template has none".to_string(),
        };
        return Err(ModelError::Compatibility(format!(
            "job output {output_index} of the Job is '{name}', but {theirs}; pass the template the Job was created from."
        )));
    }
    let mut symtab = super::build_symbol_table(job_parameter_values)?;
    if ctx.profile.has_extension(ModelExtension::Expr) {
        symtab.set("Job.Name", ExprValue::String(job.name.clone()))?;
    }
    for sym in steps_inputs(output) {
        let Some(value) = inputs.get(&sym) else {
            return Err(ModelError::Compatibility(format!(
                "job output '{name}' reads '{sym}', which is not concrete in the given inputs."
            )));
        };
        symtab.set(&sym, value.clone())?;
    }
    evaluate(output, output_index, &symtab, EvalBudgets::from_ctx(ctx))
}

/// Evaluate `output` against `symtab`, which holds every symbol it reads.
fn evaluate(
    output: &JobOutput,
    index: usize,
    symtab: &SymbolTable,
    budgets: EvalBudgets,
) -> Result<Option<ExprValue>, ModelError> {
    let name = &output.declaration.name;
    let ty = output.declaration.param_type.expr_type();
    let target = ExprType::union(vec![ty.clone(), ExprType::NULLTYPE]);
    let fail = |message: String| located(index, format!("{LABEL} '{name}': {message}"));
    let value = output
        .value
        .resolve_with(symtab, &budgets.fs_options().with_target_type(&target))
        .map_err(|e| fail(e.to_string()))?;
    if value == ExprValue::Null {
        return Ok(None);
    }
    let value = if value.expr_type() == ty {
        value
    } else {
        value.clone().coerce(&ty, PathFormat::Posix).map_err(|e| {
            fail(format!(
                "value {} does not convert to {}: {e}",
                crate::outputs::quote(&value.to_display_string()),
                output.declaration.param_type.as_spec_str()
            ))
        })?
    };
    output
        .declaration
        .check_constraints_as(&value, LABEL)
        .map_err(|m| located(index, m))?;
    Ok(Some(value))
}

/// `message` as a one-error `ModelValidation` at output `index`'s `value`.
fn located(index: usize, message: String) -> ModelError {
    let mut errors = ValidationErrors::default();
    errors.add(&value_path(index), message);
    match errors.into_result("JobTemplate") {
        Err(e) => e,
        Ok(()) => unreachable!("one error was added"),
    }
}

/// `create_job`'s job outputs: each output of `job_template`, evaluated if
/// every `Steps.*` symbol it reads is concrete at Job creation, given the
/// Steps `create_job` built (`steps`, in template order) and the job symbol
/// table `symtab`. A symbol is concrete then when its Step is skipped (all
/// its symbols are `null`), or when it is a `TaskParam` or `TaskRawParam`
/// of a Step whose Tasks are known. `None` when the template declares no
/// job outputs.
///
/// Every output that fails is reported, together, in template order.
pub(super) fn evaluate_at_creation(
    job_template: &JobTemplate,
    symtab: &SymbolTable,
    steps: &[job::Step],
    budgets: EvalBudgets,
) -> Result<Option<Vec<JobOutput>>, ModelError> {
    let Some(defs) = &job_template.job_output_definitions else {
        return Ok(None);
    };
    let mut outputs: Vec<JobOutput> = defs.iter().map(|d| d.to_job_output()).collect();
    let inputs = creation_inputs(&outputs, steps)?;
    let mut errors = ValidationErrors::default();
    for (index, output) in outputs.iter_mut().enumerate() {
        let symbols = steps_inputs(output);
        if !symbols.iter().all(|s| inputs.is_concrete(s)) {
            continue;
        }
        let mut output_symtab = symtab.clone();
        for sym in &symbols {
            // A path-typed `Output` or `TaskParam` is rejected in a job
            // output's value at validation, so every symbol has a value.
            let Some(value) = inputs.get(sym) else {
                continue;
            };
            output_symtab.set(sym, value.clone())?;
        }
        match evaluate(output, index, &output_symtab, budgets) {
            Ok(value) => output.evaluated = Some(JobOutputValue::from(value)),
            Err(ModelError::ModelValidation(e)) => errors.errors.extend(e.errors),
            Err(e) => return Err(e),
        }
    }
    errors.into_result("JobTemplate")?;
    Ok(Some(outputs))
}

/// The `Steps.*` values concrete at Job creation for `outputs`, built once
/// for all of them: every Step they read that `create_job` skipped, and,
/// for a Step whose Tasks are known, its `TaskParam` lists when some output
/// reads its `TaskParam` or `TaskRawParam` (building them walks the Step's
/// whole parameter space, so once per Step, not once per output).
fn creation_inputs(outputs: &[JobOutput], steps: &[job::Step]) -> Result<StepInputs, ModelError> {
    let mut read: BTreeSet<String> = BTreeSet::new();
    let mut reads_task_params: BTreeSet<String> = BTreeSet::new();
    for output in outputs {
        for sym in steps_inputs(output) {
            let mut parts = sym.split('.').skip(1);
            let (Some(p), Some(member)) = (parts.next(), parts.next()) else {
                continue;
            };
            if matches!(member, "TaskParam" | "TaskRawParam") {
                reads_task_params.insert(p.to_string());
            }
            read.insert(p.to_string());
        }
    }
    let mut inputs = StepInputs::new();
    for p in &read {
        // Validation rejects a reference to a Step that does not exist.
        let Some(step) = steps.iter().find(|s| &s.name == p) else {
            continue;
        };
        if matches!(step.binding, StepBinding::Skipped) {
            inputs.add_producer(step, job::ProducerState::Skipped)?;
        } else if reads_task_params.contains(p) && step.tasks_known().is_some() {
            inputs.add_producer(step, job::ProducerState::TasksKnown)?;
        }
    }
    Ok(inputs)
}
