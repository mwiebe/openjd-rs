// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Step input binding (`CONTROL_FLOW`, RFC 0010 §Step input binding): the
//! inputs a caller assembles for it ([`StepInputs`]), its error
//! ([`BindingError`]), and the read-only views of a [`Step`]'s binding
//! ([`Step::tasks_known`], [`Step::binding_report`], [`Step::input_producers`]).
//!
//! The functions that bind a Step are
//! [`advance_step_binding`](crate::create_job::advance_step_binding) and
//! [`attach_step_inputs`](crate::create_job::attach_step_inputs). See
//! `specs/model/job-creation.md` § Step input binding.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use openjd_expr::path_mapping::PathFormat;
use openjd_expr::{ExprType, ExprValue};

use super::{GroupState, Step, StepBinding, StepParameterSpace, TaskParameter};
use crate::error::ModelError;
use crate::outputs::TaskOutputs;
use crate::step_param_space::StepParameterSpaceIterator;
use crate::types::JobParameterType;

/// A Step's materialization groups (RFC 0010 §Materialization groups), in the
/// order they are evaluated when more than one can be. There is no Task
/// graph group: `TASK_DEPENDENCIES` is not implemented, and would add one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MaterializationGroup {
    /// The Step's `enableIf`.
    Enablement,
    /// The `range` of each Task parameter, then the other format strings of
    /// `parameterSpace` (the `chunks` fields).
    TaskEnumeration,
    /// The format strings of `hostRequirements`.
    HostSelection,
    /// Every other materialization expression: each Step Environment's
    /// `enableIf` and TEMPLATE-scope fields, and the script's TEMPLATE-scope
    /// fields (an action's `timeout` and `cancelation`).
    Remaining,
}

impl fmt::Display for MaterializationGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Enablement => "enablement",
            Self::TaskEnumeration => "Task enumeration",
            Self::HostSelection => "host selection",
            Self::Remaining => "remaining",
        })
    }
}

/// A materialization group that failed to evaluate, which fails its Step at
/// once (RFC 0010 §How a Step is bound, item 3).
#[derive(Debug)]
pub struct BindingError {
    /// The Step's name.
    pub step: String,
    /// The group that failed.
    pub group: MaterializationGroup,
    /// The group's errors: a `ModelValidation` error with a path per field
    /// (the message quotes the expression with a caret where the error comes
    /// from evaluating one), or a [`ModelError::Compatibility`] for a
    /// caller error, which does not fail the Step (see
    /// [`advance_step_binding`](crate::create_job::advance_step_binding)).
    pub errors: Vec<ModelError>,
}

impl BindingError {
    /// `true` when this is a caller error rather than the Step failing:
    /// `errors` is not empty and every error is a
    /// [`ModelError::Compatibility`] (see
    /// [`advance_step_binding`](crate::create_job::advance_step_binding)).
    /// Every `BindingError` this crate returns holds one error.
    pub fn is_caller_error(&self) -> bool {
        !self.errors.is_empty()
            && self
                .errors
                .iter()
                .all(|e| matches!(e, ModelError::Compatibility(_)))
    }
}

impl fmt::Display for BindingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_caller_error() {
            write!(
                f,
                "Binding Step '{}' was refused in its {} group:",
                self.step, self.group
            )?;
        } else {
            write!(
                f,
                "Step '{}' failed in its {} group:",
                self.step, self.group
            )?;
        }
        for e in &self.errors {
            write!(f, "\n{e}")?;
        }
        Ok(())
    }
}

impl std::error::Error for BindingError {}

/// What a producer Step contributes to a [`StepInputs`], given to
/// [`StepInputs::add_producer`].
#[derive(Debug, Clone, Copy)]
pub enum ProducerState<'a> {
    /// The producer's Tasks are known (it is bound, or pending with its Task
    /// enumeration group evaluated), but it has not finished. It contributes
    /// `TaskParam` and `TaskRawParam` only; its outputs stay unknown.
    TasksKnown,
    /// The producer succeeded. `task_outputs` holds each Task's output
    /// values, in the Step's parameter-space iteration order (with chunking
    /// disabled): one map for a Step without a `parameterSpace`.
    Succeeded {
        /// Each Task's outputs, in iteration order.
        task_outputs: &'a [TaskOutputs],
    },
    /// The producer is skipped: each of its `Steps.*` symbols is `null`.
    Skipped,
}

static NULL: ExprValue = ExprValue::Null;

/// The `Steps.*` values of one producer.
#[derive(Debug, Clone, Default)]
struct ProducerInputs {
    /// Every symbol of the producer is `null`.
    skipped: bool,
    /// Concrete values keyed by `<Member>.<Name>`.
    values: BTreeMap<String, ExprValue>,
    /// Path-typed `Output.<Name>` and `TaskParam.<Name>` keys: concrete, but
    /// derived on the worker host from the `Raw` twin, which `values` holds.
    derived: BTreeSet<String>,
}

/// The concrete `Steps.*` values a Step's binding reads, assembled from its
/// producers with [`add_producer`](Self::add_producer).
///
/// Symbols are named `Steps.<Step>.<Member>.<Name>`, where `<Member>` is
/// `Output`, `RawOutput`, `TaskParam`, or `TaskRawParam`. Values are in the
/// submission path context: a `PATH` or `LIST[PATH]` value is held only in
/// its `Raw` form, and the `Output` or `TaskParam` form is derived from it on
/// the worker host (RFC 0010 §Path values).
#[derive(Debug, Clone, Default)]
pub struct StepInputs {
    producers: BTreeMap<String, ProducerInputs>,
}

impl StepInputs {
    /// No inputs.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add (or replace) producer `producer`'s values, typed as the RFC's §The
    /// `Steps.*` scope table types them.
    ///
    /// * [`ProducerState::Succeeded`]: `Output.<V>` and `RawOutput.<V>` for
    ///   each declared output: the value itself for a producer without a
    ///   `parameterSpace`, otherwise a list with one element per Task in
    ///   iteration order (`[]` for zero Tasks). Each value is converted to
    ///   the declared type. Also everything [`ProducerState::TasksKnown`]
    ///   adds.
    /// * [`ProducerState::TasksKnown`]: `TaskParam.<N>` and
    ///   `TaskRawParam.<N>` for each Task parameter, a list with one element
    ///   per Task, from the parameter space with chunking disabled. A
    ///   `CHUNK[INT]` parameter contributes one `int` per Task.
    /// * [`ProducerState::Skipped`]: every symbol of the producer is `null`.
    ///   [`ProducerState::TasksKnown`] on a skipped producer is the same,
    ///   since its `TaskParam` is concrete as `null`.
    ///
    /// For a type that is or contains `path`, only the `Raw` form gets a
    /// value, with a URI in the Posix format the model evaluates in (its
    /// text unchanged), since the evaluator refuses `PathFormat::Uri`; the
    /// other form is concrete but derived on the worker host.
    ///
    /// Errors, each a [`ModelError::Compatibility`] (a caller error), when
    /// the state does not match the producer's binding (`Succeeded` needs a
    /// bound Step, `TasksKnown` known Tasks or a skipped Step, `Skipped` a
    /// skipped Step), when `task_outputs` does not have one map per Task, or
    /// when a Task lacks a declared output or holds a value that does not
    /// convert to its type.
    ///
    /// A producer need not be a dependency of anything: job outputs read any
    /// Step.
    pub fn add_producer(
        &mut self,
        producer: &Step,
        state: ProducerState<'_>,
    ) -> Result<(), ModelError> {
        let p = &producer.name;
        let mut entry = ProducerInputs::default();
        match state {
            ProducerState::Skipped => {
                if !matches!(producer.binding, StepBinding::Skipped) {
                    return Err(misuse(format!(
                        "Step '{p}' is not skipped, so it cannot be added as a skipped producer."
                    )));
                }
                entry.skipped = true;
            }
            // A skipped producer's TaskParam is concrete too, as `null`
            // (RFC 0010 §The `Steps.*` scope, Concreteness), and
            // `tasks_known` reports its zero Tasks.
            ProducerState::TasksKnown if matches!(producer.binding, StepBinding::Skipped) => {
                entry.skipped = true;
            }
            ProducerState::TasksKnown => {
                let Some(space) = known_parameter_space(producer) else {
                    return Err(misuse(format!(
                        "the Tasks of Step '{p}' are not known, so it cannot be added as a producer whose Tasks are known."
                    )));
                };
                add_task_params(&mut entry, space)?;
            }
            ProducerState::Succeeded { task_outputs } => {
                if !producer.binding.is_bound() {
                    return Err(misuse(format!(
                        "Step '{p}' is not bound, so it cannot have succeeded."
                    )));
                }
                let space = producer.parameter_space.as_ref();
                let task_count = task_count(space)?;
                if task_outputs.len() as u64 != task_count {
                    return Err(misuse(format!(
                        "Step '{p}' has {task_count} Tasks, but outputs were given for {}.",
                        task_outputs.len()
                    )));
                }
                for ov in producer.output_variables.iter().flatten() {
                    let ty = ov.param_type.expr_type();
                    let mut values = Vec::with_capacity(task_outputs.len());
                    for (i, outputs) in task_outputs.iter().enumerate() {
                        let Some(v) = outputs.get(&ov.name) else {
                            return Err(misuse(format!(
                                "Task {i} of Step '{p}' has no value for output '{}'.",
                                ov.name
                            )));
                        };
                        values.push(conform(v.clone(), &ty).map_err(|e| {
                            misuse(format!(
                                "Task {i} of Step '{p}': output '{}' does not convert to {}: {e}",
                                ov.name,
                                ov.param_type.as_spec_str()
                            ))
                        })?);
                    }
                    let value = if space.is_some() {
                        ExprValue::make_list(values, ty).map_err(ModelError::Expression)?
                    } else {
                        values.pop().expect("one Task")
                    };
                    let is_path = matches!(
                        ov.param_type,
                        JobParameterType::Path | JobParameterType::ListPath
                    );
                    insert(&mut entry, "Output", "RawOutput", &ov.name, value, is_path);
                }
                add_task_params(&mut entry, space)?;
            }
        }
        self.producers.insert(p.clone(), entry);
        Ok(())
    }

    /// The concrete value of `symbol` (`Steps.<Step>.<Member>.<Name>`):
    /// `null` for any symbol of a skipped producer. `None` when it is not
    /// concrete, and for a path-typed `Output` or `TaskParam`, which is
    /// derived from its `Raw` twin on the worker host (see
    /// [`is_concrete`](Self::is_concrete)).
    pub fn get(&self, symbol: &str) -> Option<&ExprValue> {
        let (p, key) = split_symbol(symbol)?;
        let entry = self.producers.get(p)?;
        if entry.skipped {
            return Some(&NULL);
        }
        entry.values.get(key)
    }

    /// Whether `symbol` is concrete: it has a value, or it is a path-typed
    /// `Output` or `TaskParam` whose `Raw` twin has one.
    pub fn is_concrete(&self, symbol: &str) -> bool {
        let Some((p, key)) = split_symbol(symbol) else {
            return false;
        };
        self.producers.get(p).is_some_and(|entry| {
            entry.skipped || entry.values.contains_key(key) || entry.derived.contains(key)
        })
    }

    /// The names of the producers added, sorted.
    pub fn producers(&self) -> impl Iterator<Item = &str> {
        self.producers.keys().map(String::as_str)
    }
}

fn misuse(message: String) -> ModelError {
    ModelError::Compatibility(message)
}

/// `("P", "Member.Name")` for `Steps.P.Member.Name`.
fn split_symbol(symbol: &str) -> Option<(&str, &str)> {
    let rest = symbol.strip_prefix("Steps.")?;
    let (p, key) = rest.split_once('.')?;
    // `Member.Name`, with nothing after the name.
    let (_, name) = key.split_once('.')?;
    (!name.contains('.')).then_some((p, key))
}

fn insert(
    entry: &mut ProducerInputs,
    member: &str,
    raw_member: &str,
    name: &str,
    value: ExprValue,
    is_path: bool,
) {
    let value = if is_path {
        in_eval_format(value)
    } else {
        value
    };
    if is_path {
        entry.derived.insert(format!("{member}.{name}"));
    } else {
        entry
            .values
            .insert(format!("{member}.{name}"), value.clone());
    }
    entry.values.insert(format!("{raw_member}.{name}"), value);
}

/// `value`, a `path` value or a (nested) list of them, with every URI path
/// in the Posix format the model evaluates in (see `create_job`), its text
/// unchanged. A reported URI output carries `PathFormat::Uri`, and the EXPR
/// evaluator refuses a `path` value in another format than its own, so a
/// group or job output reading it would fail with "Path format mismatch".
/// EXPR recognizes a URI by its text, so its format changes nothing else.
/// A path in a Windows format (a reverse-mapping rule's source format) is
/// left as it is: relabelling it Posix would make path operations such as
/// `.name` give wrong results, so it is refused instead, as a Windows
/// `RawParam` is. Anything else, such as a skipped producer's `null`, is
/// returned as it is.
fn in_eval_format(value: ExprValue) -> ExprValue {
    match &value {
        ExprValue::Path {
            value: text,
            format: PathFormat::Uri,
            ..
        } => ExprValue::new_path(text.clone(), PathFormat::Posix),
        v if v.is_list() => {
            let ty = v.expr_type();
            let Some(elements) = v.list_elements() else {
                return value;
            };
            let element_ty = ty.list_element_type().cloned().unwrap_or(ExprType::PATH);
            let elements: Vec<ExprValue> = elements.into_iter().map(in_eval_format).collect();
            ExprValue::make_list(elements, element_ty).unwrap_or(value)
        }
        _ => value,
    }
}

/// Convert `value` to `ty` with EXPR's implicit coercion, unless it already
/// has that type.
fn conform(value: ExprValue, ty: &ExprType) -> Result<ExprValue, String> {
    if value.expr_type() == *ty {
        return Ok(value);
    }
    value.coerce(ty, PathFormat::Posix)
}

/// The materialized parameter space of a Step whose Tasks are known:
/// `Some(None)` for a Step without a `parameterSpace`, `None` when its Tasks
/// are not known.
fn known_parameter_space(step: &Step) -> Option<Option<&StepParameterSpace>> {
    match &step.binding {
        StepBinding::Bound => Some(step.parameter_space.as_ref()),
        StepBinding::PendingInputs(pending) => match &pending.tasks {
            GroupState::Known(space) => Some(space.as_ref()),
            GroupState::Waiting { .. } => None,
        },
        StepBinding::Skipped => None,
    }
}

/// The number of Tasks of a parameter space (one for none), with chunking
/// disabled.
pub(crate) fn task_count(space: Option<&StepParameterSpace>) -> Result<u64, ModelError> {
    match space {
        None => Ok(1),
        Some(space) => {
            Ok(StepParameterSpaceIterator::new_with_chunk_override(space, Some(1))?.len() as u64)
        }
    }
}

/// Add `TaskParam.<N>` / `TaskRawParam.<N>` for each Task parameter of
/// `space` (none for a Step without one).
fn add_task_params(
    entry: &mut ProducerInputs,
    space: Option<&StepParameterSpace>,
) -> Result<(), ModelError> {
    let Some(space) = space else {
        return Ok(());
    };
    let names: Vec<&String> = space.task_parameter_definitions.keys().collect();
    let mut columns: Vec<Vec<ExprValue>> = vec![Vec::new(); names.len()];
    for set in StepParameterSpaceIterator::new_with_chunk_override(space, Some(1))? {
        for (column, name) in columns.iter_mut().zip(&names) {
            let value = set
                .get(name.as_str())
                .map(|v| v.value.clone())
                .unwrap_or(ExprValue::Null);
            column.push(task_param_value(value));
        }
    }
    for ((name, column), param) in names
        .iter()
        .zip(columns)
        .zip(space.task_parameter_definitions.values())
    {
        let (ty, is_path) = match param {
            TaskParameter::Int { .. } | TaskParameter::ChunkInt { .. } => (ExprType::INT, false),
            TaskParameter::Float { .. } => (ExprType::FLOAT, false),
            TaskParameter::String { .. } => (ExprType::STRING, false),
            TaskParameter::Path { .. } => (ExprType::PATH, true),
        };
        let column = if is_path {
            column
                .into_iter()
                .map(|v| ExprValue::new_path(v.to_display_string(), PathFormat::Posix))
                .collect()
        } else {
            column
        };
        let list = make_list(column, ty)?;
        insert(entry, "TaskParam", "TaskRawParam", name, list, is_path);
    }
    Ok(())
}

fn make_list(values: Vec<ExprValue>, ty: ExprType) -> Result<ExprValue, ModelError> {
    ExprValue::make_list(values, ty).map_err(ModelError::Expression)
}

/// One Task's value of a Task parameter as a `Steps.*` list element: the
/// one-value range of a `CHUNK[INT]` parameter becomes its `int`.
fn task_param_value(value: ExprValue) -> ExprValue {
    match value {
        ExprValue::RangeExpr(r) => match r.iter().collect::<Vec<i64>>().as_slice() {
            [single] => ExprValue::Int(*single),
            _ => ExprValue::RangeExpr(r),
        },
        other => other,
    }
}

/// What is known of one materialization group of a pending Step, in a
/// [`BindingReport`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GroupReport {
    /// The group has evaluated.
    Known,
    /// The group has not evaluated.
    Waiting {
        /// Its own `Steps.*` inputs that are not concrete yet, sorted.
        on: Vec<String>,
        /// `true` when it also waits for the Step's undecided `enableIf`
        /// (always `false` for the enablement group itself).
        for_enablement: bool,
    },
}

/// What is known of a pending Step (RFC 0010 §Implementation strategy): for
/// its enablement, its Tasks, its host requirements, and its remaining
/// group, whether each is known and what it waits for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BindingReport {
    /// The Step's `enableIf`.
    pub enablement: GroupReport,
    /// Its Task enumeration group: once known, its Tasks are enumerable
    /// ([`Step::tasks_known`]).
    pub tasks: GroupReport,
    /// Its host selection group.
    pub host_requirements: GroupReport,
    /// Its remaining group, which says why a Step whose Tasks and host
    /// requirements are known is still pending.
    pub remaining: GroupReport,
}

impl Step {
    /// The Step's Task count once its Tasks are known: bound, or pending
    /// with its Task enumeration group evaluated (one Task without a
    /// `parameterSpace`), and `Some(0)` when it is skipped. `None` while
    /// its Tasks are not known.
    pub fn tasks_known(&self) -> Option<u64> {
        if matches!(self.binding, StepBinding::Skipped) {
            return Some(0);
        }
        task_count(known_parameter_space(self)?).ok()
    }

    /// What is known of a [`StepBinding::PendingInputs`] Step, per group;
    /// `None` for a bound or skipped Step.
    pub fn binding_report(&self) -> Option<BindingReport> {
        let StepBinding::PendingInputs(pending) = &self.binding else {
            return None;
        };
        let enablement_waits = !pending.enablement.is_known();
        let report = |state_on: Option<&Vec<String>>, for_enablement: bool| match state_on {
            None => GroupReport::Known,
            Some(on) => GroupReport::Waiting {
                on: on.clone(),
                for_enablement,
            },
        };
        fn waiting<T>(g: &GroupState<T>) -> Option<&Vec<String>> {
            match g {
                GroupState::Waiting { on } => Some(on),
                GroupState::Known(_) => None,
            }
        }
        Some(BindingReport {
            enablement: report(waiting(&pending.enablement), false),
            tasks: report(waiting(&pending.tasks), enablement_waits),
            host_requirements: report(waiting(&pending.host_requirements), enablement_waits),
            remaining: report(waiting(&pending.remaining), enablement_waits),
        })
    }

    /// The `Steps.<Step>.<Member>.<Name>` symbols the Step reads, sorted:
    /// in its script and Step Environments (which
    /// [`attach_step_inputs`](crate::create_job::attach_step_inputs) needs
    /// concrete) and, while it is pending, those its groups wait on. Empty
    /// for a skipped Step.
    pub fn input_symbols(&self) -> Vec<String> {
        if matches!(self.binding, StepBinding::Skipped) {
            return Vec::new();
        }
        let mut symbols: BTreeSet<String> = crate::job::create_job::host_scope_symbols(self)
            .iter()
            .filter_map(|s| steps_key(s))
            .collect();
        if let StepBinding::PendingInputs(pending) = &self.binding {
            let groups: [Option<&Vec<String>>; 4] = [
                waiting_on(&pending.enablement),
                waiting_on(&pending.tasks),
                waiting_on(&pending.host_requirements),
                waiting_on(&pending.remaining),
            ];
            symbols.extend(groups.into_iter().flatten().flatten().cloned());
        }
        symbols.into_iter().collect()
    }

    /// The producers whose `Steps.*` symbols the Step reads, sorted: in its
    /// script and Step Environments (for
    /// [`attach_step_inputs`](crate::create_job::attach_step_inputs)) and,
    /// while it is pending, in any of its groups, known or not. A caller
    /// passes each of them whose state is known (Tasks known, succeeded, or
    /// skipped) to every
    /// [`advance_step_binding`](crate::create_job::advance_step_binding) call
    /// and to `attach_step_inputs`. Empty for a skipped Step, which
    /// dispatches nothing.
    pub fn input_producers(&self) -> Vec<String> {
        if matches!(self.binding, StepBinding::Skipped) {
            // A skipped Step dispatches nothing, so it needs nothing.
            return Vec::new();
        }
        let mut producers = BTreeSet::new();
        let symbols = crate::job::create_job::host_scope_symbols(self);
        for s in &symbols {
            if let Some((p, _)) = steps_key(s).as_deref().and_then(split_owned) {
                producers.insert(p);
            }
        }
        if let StepBinding::PendingInputs(pending) = &self.binding {
            producers.extend(pending.producers.iter().cloned());
            // The groups' waiting sets name producers too; they are in
            // `producers` for a Step this crate bound, and are added for one
            // deserialized without it.
            let groups: [Option<&Vec<String>>; 4] = [
                waiting_on(&pending.enablement),
                waiting_on(&pending.tasks),
                waiting_on(&pending.host_requirements),
                waiting_on(&pending.remaining),
            ];
            for s in groups.into_iter().flatten().flatten() {
                if let Some((p, _)) = split_owned(s) {
                    producers.insert(p);
                }
            }
        }
        producers.into_iter().collect()
    }
}

fn waiting_on<T>(g: &GroupState<T>) -> Option<&Vec<String>> {
    match g {
        GroupState::Waiting { on } => Some(on),
        GroupState::Known(_) => None,
    }
}

fn split_owned(symbol: &str) -> Option<(String, String)> {
    split_symbol(symbol).map(|(p, k)| (p.to_string(), k.to_string()))
}

/// The `Steps.<Step>.<Member>.<Name>` symbol a symbol read starts with:
/// `Steps.P.Output.X` for `Steps.P.Output.X.name`. `None` outside the
/// `Steps` root, or for an incomplete reference (validation rejects those).
pub(crate) fn steps_key(symbol: &str) -> Option<String> {
    if !symbol.starts_with("Steps.") {
        return None;
    }
    let parts: Vec<&str> = symbol.splitn(5, '.').collect();
    (parts.len() >= 4).then(|| parts[..4].join("."))
}
