// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Job-side loops (`CONTROL_FLOW`, RFC 0013 §Running a loop): a loop of a
//! [`Job`](super::Job) ([`Loop`]), its binding ([`LoopBinding`]), the state a
//! caller keeps while it runs ([`LoopProgress`]), one iteration's body Step
//! instances ([`LoopIteration`]), what an iteration boundary decides
//! ([`BoundaryOutcome`]), and its errors ([`LoopError`]).
//!
//! The functions that bind and run a loop are in
//! [`create_job`](crate::create_job): `advance_loop_binding`,
//! `start_iteration`, `advance_body_step_binding`,
//! `evaluate_iteration_boundary`, and `end_without_iterations`. See
//! `specs/model/job-creation.md` § Loops.

use std::fmt;
use std::hash::{Hash, Hasher};

use indexmap::IndexMap;
use openjd_expr::ExprValue;
use serde::Serialize;

use super::{hash_map_entries, GroupState, OutputVariable, Step, StepDependency};
use crate::error::ModelError;
use crate::types::OutputCollectMode;

/// A loop of a Job (RFC 0013 `<StepLoop>`), in the form `create_job` stores in
/// [`Job::loops`](super::Job::loops).
///
/// A loop is a node of the step graph, as a Step is, and has no Tasks of its
/// own. Its body Step templates and its format strings stay in the
/// `JobTemplate`, which the loop functions take, as
/// [`advance_step_binding`](crate::create_job::advance_step_binding) does.
#[derive(Debug, Clone, PartialEq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Loop {
    /// The loop's name, unique among all Steps of the template.
    pub name: String,
    /// For display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The index of the loop's entry in the template's `steps` list, so that a
    /// Job alone can list its Steps and loops in template order.
    pub entry_index: usize,
    /// The top-level Steps and loops the loop depends on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<Vec<StepDependency>>,
    /// `while` or `until`.
    pub condition: LoopCondition,
    /// The loop variables' declarations, in declaration order (name, type,
    /// and constraints; `collect` is the default and unused).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<OutputVariable>,
    /// The loop outputs' declarations, in declaration order (name, type,
    /// `collect` (`LAST` or `APPEND`), `unit`, and constraints).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_variables: Vec<OutputVariable>,
    /// The names of the body Steps, in body order.
    pub body: Vec<String>,
    /// The loop's binding state.
    pub binding: LoopBinding,
}

/// Which of `while` and `until` a loop has (exactly one, RFC 0013
/// §`<LoopDefinition>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LoopCondition {
    /// `while`: checked before each iteration, including the first.
    While,
    /// `until`: checked after each iteration.
    Until,
}

/// A loop's binding (RFC 0013 §Loop binding), as [`StepBinding`](super::StepBinding)
/// is a Step's.
#[derive(Debug, Clone, PartialEq, Hash, Serialize)]
#[serde(tag = "state", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LoopBinding {
    /// Every group has evaluated: the loop is `READY`.
    Bound(BoundLoop),
    /// Not bound yet (`PENDING_INPUTS`). What is known so far, per group.
    PendingInputs(PendingLoopBinding),
    /// The loop's `enableIf` evaluated to `false` (`SKIPPED`): no iteration
    /// runs, and its outputs are `null`.
    Skipped,
}

/// What a bound loop's groups evaluated to.
#[derive(Debug, Clone, PartialEq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundLoop {
    /// `maxIterations`, from 1 to 1,000.
    pub max_iterations: u32,
    /// The start group's values.
    pub start: LoopStart,
}

impl BoundLoop {
    /// `false` when the loop has zero iterations: its first `while` is
    /// `false`. An `until` loop always runs at least once.
    pub fn has_iterations(&self) -> bool {
        self.start.first_while != Some(false)
    }
}

/// The start group's values (RFC 0013 §Loop binding item 3).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopStart {
    /// Each variable's `initial` value, converted to its type, in declaration
    /// order. A `PATH` value is in the submission path context.
    pub initial: IndexMap<String, ExprValue>,
    /// The first `while`, for a `while` loop; `None` for an `until` loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_while: Option<bool>,
}

/// Manual because `IndexMap` has no `Hash`; entries hash sorted by key.
impl Hash for LoopStart {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_map_entries(self.initial.iter(), state);
        self.first_while.hash(state);
    }
}

/// What is known of a [`LoopBinding::PendingInputs`] loop, per group (RFC
/// 0013 §Loop binding).
#[derive(Debug, Clone, PartialEq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingLoopBinding {
    /// `Known(())` once `enableIf` is `true` (or absent).
    pub enablement: GroupState<()>,
    /// `maxIterations` once evaluated. It reads no `Steps.*` symbol, so it
    /// waits only for `enableIf`.
    pub bound: GroupState<u32>,
    /// The `initial` values and the first `while`, once evaluated.
    pub start: GroupState<LoopStart>,
    /// The producers whose `Steps.*` symbols any of the loop's groups read,
    /// sorted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub producers: Vec<String>,
}

/// What is known of a pending loop, per group, as
/// [`BindingReport`](super::BindingReport) is for a Step.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LoopBindingReport {
    /// The loop's `enableIf`.
    pub enablement: super::GroupReport,
    /// `maxIterations`.
    pub bound: super::GroupReport,
    /// The `initial` values and the first `while`.
    pub start: super::GroupReport,
}

impl Loop {
    /// `Some(0)` for a skipped loop, and `None` otherwise: a loop's Task count
    /// is not known until it ends (RFC 0013 §Iterations).
    pub fn tasks_known(&self) -> Option<u64> {
        matches!(self.binding, LoopBinding::Skipped).then_some(0)
    }

    /// What is known of a [`LoopBinding::PendingInputs`] loop, per group;
    /// `None` for a bound or skipped loop.
    pub fn binding_report(&self) -> Option<LoopBindingReport> {
        let LoopBinding::PendingInputs(p) = &self.binding else {
            return None;
        };
        fn report<T>(g: &GroupState<T>, for_enablement: bool) -> super::GroupReport {
            match g {
                GroupState::Known(_) => super::GroupReport::Known,
                GroupState::Waiting { on } => super::GroupReport::Waiting {
                    on: on.clone(),
                    for_enablement,
                },
            }
        }
        let waits = !p.enablement.is_known();
        Some(LoopBindingReport {
            enablement: report(&p.enablement, false),
            bound: report(&p.bound, waits),
            start: report(&p.start, waits),
        })
    }

    /// The producers whose `Steps.*` symbols the loop's own groups read,
    /// sorted: those a caller passes to every
    /// [`advance_loop_binding`](crate::create_job::advance_loop_binding) call
    /// once their state is known. Empty for a bound or skipped loop.
    pub fn input_producers(&self) -> Vec<String> {
        match &self.binding {
            LoopBinding::PendingInputs(p) => p.producers.clone(),
            _ => Vec::new(),
        }
    }

    /// The declaration of loop output `name`.
    pub fn output_variable(&self, name: &str) -> Option<&OutputVariable> {
        self.output_variables.iter().find(|o| o.name == name)
    }
}

/// A running loop's state between iterations, kept by the caller (RFC 0013
/// §Iterations), as a scheduler keeps a Step's state.
#[derive(Debug, Clone, PartialEq)]
pub struct LoopProgress {
    /// The iteration about to start, or in progress, from 0.
    pub iteration: u32,
    /// `Loop.<V>` for `iteration`, in declaration order, each converted to
    /// its variable's type. A `PATH` value is in the submission path context.
    pub values: IndexMap<String, ExprValue>,
    /// Each loop output's value so far, in declaration order: for `LAST` the
    /// value of the last iteration whose boundary has been evaluated (`null`
    /// before the first), and for `APPEND` the list of every such
    /// iteration's element (`[]` before the first). Not committed until the
    /// loop succeeds (RFC 0013 §Loop outputs).
    pub outputs: IndexMap<String, ExprValue>,
}

impl LoopProgress {
    /// The progress of a loop about to start iteration 0: its `initial`
    /// values and no outputs yet. `None` unless the loop is bound.
    pub fn new(job_loop: &Loop) -> Option<Self> {
        let LoopBinding::Bound(bound) = &job_loop.binding else {
            return None;
        };
        Some(Self {
            iteration: 0,
            values: bound.start.initial.clone(),
            outputs: empty_outputs(job_loop),
        })
    }
}

/// Each loop output's value before any iteration: `null` for `LAST`, `[]` of
/// the declared type for `APPEND`.
pub(crate) fn empty_outputs(job_loop: &Loop) -> IndexMap<String, ExprValue> {
    job_loop
        .output_variables
        .iter()
        .map(|o| {
            let value = match o.collect {
                OutputCollectMode::Append => {
                    let ty = o.param_type.expr_type();
                    let elem = ty
                        .list_element_type()
                        .cloned()
                        .unwrap_or(openjd_expr::ExprType::NULLTYPE);
                    ExprValue::make_list(Vec::new(), elem).unwrap_or(ExprValue::Null)
                }
                _ => ExprValue::Null,
            };
            (o.name.clone(), value)
        })
        .collect()
}

/// The body Step instances of one iteration, in body order (RFC 0013
/// §Iterations item 2).
///
/// An instance is a [`Step`] named by its body Step's name, which is unique
/// in the template, so [`StepInputs::add_producer`](super::StepInputs::add_producer)
/// and [`attach_step_inputs`](crate::create_job::attach_step_inputs) work on
/// it unchanged. A scheduler identifies it by the loop, the iteration, and
/// that name: [`instance_name`](Self::instance_name) gives
/// `<Loop>[<N>].<Step>`.
#[derive(Debug, Clone, PartialEq)]
pub struct LoopIteration {
    /// The loop's name.
    pub loop_name: String,
    /// The iteration's index, from 0.
    pub index: u32,
    /// One instance per body Step, in body order. The caller swaps in the
    /// result of each
    /// [`advance_body_step_binding`](crate::create_job::advance_body_step_binding) call.
    pub steps: Vec<Step>,
}

impl LoopIteration {
    /// `<Loop>[<N>].<Step>` for body Step `body_index`; `None` past the body.
    pub fn instance_name(&self, body_index: usize) -> Option<String> {
        self.steps
            .get(body_index)
            .map(|s| instance_name(&self.loop_name, self.index, &s.name))
    }
}

/// `<Loop>[<N>].<Step>`.
pub(crate) fn instance_name(loop_name: &str, iteration: u32, step: &str) -> String {
    format!("{loop_name}[{iteration}].{step}")
}

/// What an iteration boundary decides (RFC 0013 §The iteration boundary).
#[derive(Debug, Clone, PartialEq)]
pub enum BoundaryOutcome {
    /// Start the next iteration, with this progress: the next iteration's
    /// index and variable values, and the outputs so far.
    Continue(LoopProgress),
    /// The loop succeeded.
    Succeeded(LoopResult),
}

/// A loop that succeeded.
#[derive(Debug, Clone, PartialEq)]
pub struct LoopResult {
    /// The number of iterations it ran.
    pub iterations: u32,
    /// What ended it.
    pub ended_by: LoopEnd,
    /// Each loop output's committed value, in declaration order: for `LAST`
    /// the last iteration's (`null` after zero iterations), for `APPEND` the
    /// list of every iteration's (`[]` after zero iterations). Pass it to
    /// [`StepInputs::add_loop_producer`](super::StepInputs::add_loop_producer).
    pub outputs: IndexMap<String, ExprValue>,
}

/// What ended a loop that succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoopEnd {
    /// `until` was `true`.
    Until,
    /// `while` was `false` at a boundary.
    While,
    /// The first `while` was `false`, so no iteration ran.
    ZeroIterations,
}

/// The field of a loop that a [`LoopError`] comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LoopErrorField {
    /// Loop output `name`'s `value`.
    OutputValue(String),
    /// `until`.
    Until,
    /// Loop variable `name`'s `next`.
    Next(String),
    /// `while`.
    While,
    /// `maxIterations`: the loop reached it while continuing.
    MaxIterations,
    /// `APPEND` loop output `name`'s `minLength`, when the loop ends.
    MinLength(String),
    /// The total size of the loop variable and loop output values (RFC 0013
    /// §Limits item 4).
    ValueSize,
    /// The caller's arguments (a caller error, which does not fail the loop).
    Inputs,
}

impl fmt::Display for LoopErrorField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutputValue(n) => write!(f, "the value of loop output '{n}'"),
            Self::Until => f.write_str("until"),
            Self::Next(n) => write!(f, "the next of loop variable '{n}'"),
            Self::While => f.write_str("while"),
            Self::MaxIterations => f.write_str("maxIterations"),
            Self::MinLength(n) => write!(f, "the minLength of loop output '{n}'"),
            Self::ValueSize => f.write_str("the size of its values"),
            Self::Inputs => f.write_str("its arguments"),
        }
    }
}

/// A loop that failed at an iteration boundary or when it ended (RFC 0013
/// §Failure), which is final; or, when
/// [`is_caller_error`](Self::is_caller_error) is `true`, a call the loop
/// functions refused, which says nothing about the loop.
#[derive(Debug)]
pub struct LoopError {
    /// The loop's name.
    pub loop_name: String,
    /// The iteration whose boundary failed; `None` for a zero-iteration loop.
    pub iteration: Option<u32>,
    /// The field that failed.
    pub field: LoopErrorField,
    /// The errors: a `ModelValidation` error at the field's path, whose
    /// message names the expression (with a caret for an evaluation error),
    /// or a [`ModelError::Compatibility`] for a caller error, which does not
    /// fail the loop.
    pub errors: Vec<ModelError>,
}

impl LoopError {
    /// `true` when this is a caller error rather than the loop failing:
    /// `errors` is not empty and every error is a
    /// [`ModelError::Compatibility`].
    pub fn is_caller_error(&self) -> bool {
        !self.errors.is_empty()
            && self
                .errors
                .iter()
                .all(|e| matches!(e, ModelError::Compatibility(_)))
    }
}

impl fmt::Display for LoopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let at = match self.iteration {
            Some(n) => format!("at the boundary of iteration {n}"),
            None => "after zero iterations".to_string(),
        };
        if self.is_caller_error() {
            write!(
                f,
                "Evaluating loop '{}' {at} was refused, in {}:",
                self.loop_name, self.field
            )?;
        } else {
            write!(
                f,
                "Loop '{}' failed {at}, in {}:",
                self.loop_name, self.field
            )?;
        }
        for e in &self.errors {
            write!(f, "\n{e}")?;
        }
        Ok(())
    }
}

impl std::error::Error for LoopError {}

/// One of a loop's own expressions that reads `Steps.*`, for
/// [`Loop::expression_inputs`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LoopExpression {
    /// `enableIf`.
    EnableIf,
    /// Loop variable `name`'s `initial`.
    Initial(String),
    /// `while`.
    While,
    /// `until`.
    Until,
    /// Loop variable `name`'s `next`.
    Next(String),
    /// Loop output `name`'s `value`.
    OutputValue(String),
}

/// What a loop contributes to a [`StepInputs`](super::StepInputs), given to
/// [`StepInputs::add_loop_producer`](super::StepInputs::add_loop_producer).
#[derive(Debug, Clone, Copy)]
pub enum LoopProducerState<'a> {
    /// The loop succeeded, with these outputs ([`LoopResult::outputs`]).
    Succeeded {
        /// Each loop output's committed value, by name.
        outputs: &'a IndexMap<String, ExprValue>,
    },
    /// The loop is skipped: each of its `Steps.*` symbols is `null`.
    Skipped,
}
