// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! The scheduler simulator of `openjd run` (`docs/control-flow-prototype-plan.md`
//! C1 and L8): the state of each Step, loop, and job output while a
//! `CONTROL_FLOW` Job runs.
//!
//! `create_job` returns each Step and loop bound, skipped, or pending inputs
//! (RFC 0011 §Step input binding, RFC 0013 §Loop binding). The simulator keeps
//! its own copy of the Job and, whenever a Step's Tasks become known or a Step
//! or loop succeeds or is skipped, advances every pending Step with
//! `advance_step_binding` and every pending loop with `advance_loop_binding`
//! until nothing changes, then evaluates every job output whose inputs have
//! become concrete. A group that fails stops the run, as a failed Task does.
//!
//! A loop runs its iterations one after another (RFC 0013 §Iterations): each
//! iteration's body Step instances are created with `start_iteration`,
//! advanced with `advance_body_step_binding`, and run through the same
//! per-Step path as a top-level Step ([`Slot::Body`]); then
//! `evaluate_iteration_boundary` continues or ends the loop. A Job without
//! `CONTROL_FLOW` has only bound Steps and no job outputs, so for it the
//! simulator only records what ran.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use indexmap::IndexMap;
use openjd_expr::ExprValue;
use openjd_model::job::{
    BoundaryOutcome, GroupReport, GroupState, Job, JobOutputValue, Loop, LoopBinding, LoopEnd,
    LoopExpression, LoopIteration, LoopProducerState, LoopProgress, LoopResult, ProducerState,
    Step, StepBinding, StepInputs,
};
use openjd_model::outputs::TaskOutputs;
use openjd_model::step_dependency_graph::StepNodeKind;
use openjd_model::template::{JobTemplate, StepLoop};
use openjd_model::types::{
    JobParameterValues, OutputCollectMode, TaskParameterSet, ValidationContext,
};
use openjd_model::{ChunkTaskIndex, ModelError};
use openjd_sessions::action::ActionOutputs;

use super::RunError;

/// A top-level entry of the Job's `steps`: a Step (an index into
/// `job.steps`) or a loop (an index into `job.loops`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Node {
    Step(usize),
    Loop(usize),
}

/// A Step whose Tasks the run can run: a top-level Step (an index into
/// `job.steps`), or a body Step instance of the iteration in progress (its
/// index in the loop's body). Only one loop runs at a time, and only its last
/// iteration is in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Slot {
    Top(usize),
    Body(usize),
}

/// A Step's or loop's state in the simulator: RFC 0011 §Step states and
/// RFC 0013 §Loop states, with `Bound` for `READY` and `Blocked` for a Step
/// that can never run because a dependency failed, is blocked, or has
/// incomplete outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SimStepState {
    Pending,
    Bound,
    /// A loop whose iterations are running.
    Running,
    Skipped,
    Succeeded,
    Failed,
    Blocked,
}

impl SimStepState {
    /// RFC 0011's name for the state, for `--output json|yaml`.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "PENDING_INPUTS",
            Self::Bound => "READY",
            Self::Running => "RUNNING",
            Self::Skipped => "SKIPPED",
            Self::Succeeded => "SUCCEEDED",
            Self::Failed => "FAILED",
            Self::Blocked => "BLOCKED",
        }
    }

    fn of_step(step: &Step) -> Self {
        match step.binding {
            StepBinding::Bound => Self::Bound,
            StepBinding::PendingInputs(_) => Self::Pending,
            StepBinding::Skipped => Self::Skipped,
        }
    }
}

/// How the Tasks of a running Step's Actions are placed in its per-Task
/// iteration order (only for a Step that declares output variables).
#[derive(Debug)]
enum Placement {
    /// An unchunked parameter space iterated in order: the next Action is the
    /// next Task. No index is built, so a large space costs nothing until its
    /// Tasks run.
    Sequential(usize),
    /// A chunked space run in iteration order of its chunks.
    Indexed(Box<ChunkTaskIndex>),
    /// Tasks given with `--tasks` or `--task-param`, in any order: each
    /// Action's Task parameter values, `CHUNK[INT]` value, and outputs, in
    /// the order they ran. They are placed at their per-Task indices only
    /// when they are as many as the Step's Tasks (see
    /// [`Simulator::finish_step`]), so a run of a few Tasks of a large space
    /// never walks it.
    Explicit(Vec<(TaskParameterSet, Option<i64>, TaskOutputs)>),
}

/// What the simulator knows of one Step or body Step instance.
#[derive(Debug)]
pub(super) struct StepRecord {
    pub(super) state: SimStepState,
    /// The outputs of each Task that succeeded, by per-Task index, for a
    /// Step that declares output variables. Kept sparse, so a run that stops
    /// early in a large space holds only what ran.
    pub(super) task_outputs: BTreeMap<usize, TaskOutputs>,
    /// Places each Action's Tasks; `None` while the Step has not started, or
    /// when it declares no output variables.
    placement: Option<Placement>,
    /// Whether every one of its Tasks ran and succeeded.
    all_tasks_ran: bool,
    /// The outputs of Tasks given with `--tasks` or `--task-param` that do
    /// not cover the Step, in the order they ran (not placed).
    pub(super) unplaced: Vec<TaskOutputs>,
    /// Why the Step failed or is blocked.
    pub(super) reason: Option<String>,
    /// Whether `--step-inputs` gave its outputs.
    supplied: bool,
}

impl StepRecord {
    fn new(step: &Step) -> Self {
        Self {
            state: SimStepState::of_step(step),
            task_outputs: BTreeMap::new(),
            placement: None,
            all_tasks_ran: false,
            unplaced: Vec::new(),
            reason: None,
            supplied: false,
        }
    }

    /// Whether the Step succeeded with every one of its Tasks.
    fn complete(&self) -> bool {
        self.state == SimStepState::Succeeded && self.all_tasks_ran
    }

    /// Whether a dependent's `dependsOn` on it is satisfied with its outputs
    /// complete: it succeeded with all of its Tasks, or it is skipped.
    fn done(&self) -> bool {
        self.complete() || (self.state == SimStepState::Skipped && self.reason.is_none())
    }
}

/// One iteration a loop started (RFC 0013 §Iterations).
#[derive(Debug)]
pub(super) struct IterationRecord {
    /// Its body Step instances, in body order, each as advanced so far.
    pub(super) iteration: LoopIteration,
    /// `Loop.<V>` for the iteration.
    pub(super) values: IndexMap<String, ExprValue>,
    /// One record per instance, in body order.
    pub(super) records: Vec<StepRecord>,
    /// Each loop output's value for this iteration, once its boundary
    /// evaluated them: the `LAST` value, or the element `APPEND` added.
    pub(super) outputs: Option<IndexMap<String, ExprValue>>,
}

/// What the simulator knows of one loop.
#[derive(Debug)]
pub(super) struct LoopRecord {
    pub(super) state: SimStepState,
    /// Why the loop failed or is blocked.
    pub(super) reason: Option<String>,
    /// The progress of a running loop.
    progress: Option<LoopProgress>,
    /// The iterations it started, in order.
    pub(super) iterations: Vec<IterationRecord>,
    /// The loop's result once it succeeded.
    pub(super) result: Option<LoopResult>,
    /// Whether `--step-inputs` gave its outputs.
    supplied: bool,
}

/// What starting a Step comes to (see [`Simulator::begin_step`]).
pub(super) enum Begin {
    /// Run its Tasks: the bound Step, with its `Steps.*` inputs attached.
    Run(Box<Step>),
    /// It is skipped, failed, or blocked, so nothing runs.
    NotRun,
    /// The run must stop: Tasks were given with `--tasks` or `--task-param`
    /// for a Step skipped at run time.
    Failed,
}

/// What starting a loop comes to (see [`Simulator::begin_loop`]).
pub(super) enum LoopBegin {
    /// Run its iterations.
    Run,
    /// It is skipped, failed, blocked, or ended without iterations, so
    /// nothing runs.
    NotRun,
    /// It failed as it started (a zero-iteration loop's `minLength`), which
    /// stops the run.
    Failed,
}

/// What the end of an iteration comes to (see [`Simulator::end_iteration`]).
pub(super) enum IterationEnd {
    /// The boundary continues the loop with the next iteration.
    Continue,
    /// The loop succeeded.
    Succeeded,
    /// The boundary failed the loop, which stops the run.
    Failed,
    /// A body Step instance did not complete, so the boundary is not
    /// evaluated and the loop is blocked.
    Blocked,
}

/// The simulator's state for one run.
pub(super) struct Simulator {
    jt: JobTemplate,
    params: JobParameterValues,
    vctx: ValidationContext,
    /// The Job, with each Step's and loop's binding as advanced so far and
    /// each job output's value once evaluated.
    pub(super) job: Job,
    /// One per top-level Step, as `job.steps`.
    pub(super) records: Vec<StepRecord>,
    /// One per loop, as `job.loops`.
    pub(super) loop_records: Vec<LoopRecord>,
    /// The loop whose last iteration is in progress.
    running: Option<usize>,
    /// The `Steps.*` values the instances of the iteration in progress and
    /// its boundary read: `inputs` as the iteration started, and each
    /// instance of the iteration as it is skipped, its Tasks become known,
    /// or it succeeds with all of its Tasks.
    iteration_inputs: StepInputs,
    /// Every concrete `Steps.*` value so far, of the Steps and loops
    /// something reads, except the `TaskParam` lists of the Steps in
    /// `tasks_known_pending`.
    inputs: StepInputs,
    /// Read Steps whose Tasks are known but whose `TaskParam` lists have not
    /// been built into `inputs` yet: they are built only when something
    /// that reads the Step is advanced, attached, or evaluated, so a large
    /// parameter space is not enumerated for nothing.
    tasks_known_pending: BTreeSet<String>,
    read: BTreeSet<String>,
    /// Bumped on every change of a Step's or loop's state that can make
    /// `Steps.*` symbols concrete.
    generation: u64,
    /// The generation of each Step's and loop's last such change, by name.
    changed: HashMap<String, u64>,
    /// The generation at which each pending Step or loop was last advanced,
    /// by name: it is advanced again only once one of its producers has
    /// changed since, as nothing else can make more of its groups evaluable.
    advanced: HashMap<String, u64>,
    /// The entries this run runs.
    in_run: HashSet<Node>,
    /// Every entry, in dependency order.
    order: Vec<Node>,
    /// Every entry, in template order.
    template_order: Vec<Node>,
    /// Job outputs whose evaluation failed, so they are not retried.
    failed_outputs: BTreeSet<usize>,
    /// Whether the Job Template lists `CONTROL_FLOW`.
    control_flow: bool,
    /// Each top-level entry, by name.
    index_of: HashMap<String, Node>,
}

/// Print one line of the run's log.
fn say(line: &str) {
    println!("{}\t{line}", crate::format_log_timestamp());
}

fn internal(message: impl std::fmt::Display) -> RunError {
    format!("internal error in the openjd run scheduler simulator: {message}").into()
}

fn tasks_noun(n: u64) -> &'static str {
    if n == 1 {
        "task"
    } else {
        "tasks"
    }
}

fn iterations_noun(n: u32) -> &'static str {
    if n == 1 {
        "iteration"
    } else {
        "iterations"
    }
}

fn enablement_known(step: &Step) -> bool {
    match step.binding_report() {
        Some(report) => report.enablement == GroupReport::Known,
        None => step.binding.is_bound(),
    }
}

fn host_known(step: &Step) -> bool {
    match step.binding_report() {
        Some(report) => report.host_requirements == GroupReport::Known,
        None => step.binding.is_bound(),
    }
}

/// Whether the Step has host requirements, once they are known.
fn has_host_requirements(step: &Step) -> bool {
    match &step.binding {
        StepBinding::Bound => step.host_requirements.is_some(),
        StepBinding::PendingInputs(pending) => matches!(
            &pending.host_requirements,
            openjd_model::job::GroupState::Known(Some(_))
        ),
        _ => false,
    }
}

/// What a Step's advance made known (see [`report_step_advance`]).
struct StepAdvance {
    /// It is skipped.
    skipped: bool,
    /// Its Tasks became known.
    tasks_known: bool,
}

/// Print each fact `after` gains over `before`, for the Step named `name`
/// in the log (`<Loop>[<N>].<Step>` for a body Step instance).
fn report_step_advance(name: &str, before: &Step, after: &Step) -> StepAdvance {
    if matches!(after.binding, StepBinding::Skipped) {
        say(&format!("Step '{name}' skipped (enableIf is false)"));
        return StepAdvance {
            skipped: true,
            tasks_known: false,
        };
    }
    if !enablement_known(before) && enablement_known(after) {
        say(&format!("Step '{name}': enabled (enableIf is true)"));
    }
    let mut tasks_known = false;
    if let (None, Some(n)) = (before.tasks_known(), after.tasks_known()) {
        say(&format!(
            "Step '{name}': Tasks known ({n} {})",
            tasks_noun(n)
        ));
        tasks_known = true;
    }
    if !host_known(before) && host_known(after) && has_host_requirements(after) {
        say(&format!("Step '{name}': host requirements known"));
    }
    if after.binding.is_bound() {
        say(&format!("Step '{name}' is ready"));
    }
    StepAdvance {
        skipped: false,
        tasks_known,
    }
}

/// A loop's `maxIterations`, once it is known.
fn max_iterations(job_loop: &Loop) -> Option<u32> {
    match &job_loop.binding {
        LoopBinding::Bound(bound) => Some(bound.max_iterations),
        LoopBinding::PendingInputs(pending) => match pending.bound {
            GroupState::Known(n) => Some(n),
            GroupState::Waiting { .. } => None,
        },
        LoopBinding::Skipped => None,
    }
}

fn loop_enablement_known(job_loop: &Loop) -> bool {
    match &job_loop.binding {
        LoopBinding::PendingInputs(pending) => pending.enablement.is_known(),
        LoopBinding::Bound(_) => true,
        LoopBinding::Skipped => false,
    }
}

/// `V = <value>, ...` for each value, or `None` when there are none.
fn value_list(
    values: &IndexMap<String, ExprValue>,
    units: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    (!values.is_empty()).then(|| {
        values
            .iter()
            .map(|(name, value)| {
                format!(
                    "{name} = {}",
                    crate::common::with_unit(value, units(name).as_deref())
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    })
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

/// Each loop output's value for one iteration, from the outputs so far
/// after its boundary: the `LAST` value, or the last element of an `APPEND`
/// list.
fn iteration_outputs(
    job_loop: &Loop,
    so_far: &IndexMap<String, ExprValue>,
) -> IndexMap<String, ExprValue> {
    job_loop
        .output_variables
        .iter()
        .filter_map(|o| {
            let value = so_far.get(&o.name)?;
            let value = match o.collect {
                OutputCollectMode::Append => value
                    .list_elements()
                    .and_then(|l| l.last().cloned())
                    .unwrap_or(ExprValue::Null),
                _ => value.clone(),
            };
            Some((o.name.clone(), value))
        })
        .collect()
}

/// What ended a loop, for its log line.
fn ended_by(end: LoopEnd) -> &'static str {
    match end {
        LoopEnd::Until => "until is true",
        LoopEnd::While => "while is false",
        LoopEnd::ZeroIterations => "the first while is false",
    }
}

/// Place each Task's `outputs` of an Action of `step`, whose Task parameter
/// values were `action`, in `record`, and print each with its unit.
fn record_outputs(
    step: &Step,
    record: &mut StepRecord,
    action: Option<&TaskParameterSet>,
    outputs: ActionOutputs,
) {
    for (value, task_outputs) in outputs {
        for (name, output) in &task_outputs {
            let unit = step
                .output_variables
                .iter()
                .flatten()
                .find(|v| &v.name == name)
                .and_then(|v| v.unit.as_deref());
            let label = match value {
                Some(v) => format!("{name}[{v}]"),
                None => name.clone(),
            };
            say(&format!(
                "Output {label} = {}",
                crate::common::with_unit(output, unit)
            ));
        }
        let slot = match (record.placement.as_mut(), action) {
            (Some(Placement::Sequential(next)), _) => {
                *next += 1;
                Some(*next - 1)
            }
            (Some(Placement::Indexed(index)), Some(action)) => index.take(action, value),
            (Some(Placement::Explicit(ran)), Some(action)) => {
                ran.push((action.clone(), value, task_outputs));
                continue;
            }
            _ => None,
        };
        if let Some(slot) = slot {
            record.task_outputs.insert(slot, task_outputs);
        }
    }
}

/// Record that `step` finished in `record`: failed, or succeeded (see
/// [`Simulator::finish_step`]). Returns each Task's outputs, in per-Task
/// order, when it succeeded with all of its Tasks.
fn finish_record(
    step: &Step,
    record: &mut StepRecord,
    failed: bool,
    ran_all: bool,
) -> Result<Option<Vec<TaskOutputs>>, RunError> {
    let task_count = step.tasks_known().unwrap_or(0) as usize;
    let declares_outputs = step.output_variables.is_some();
    if failed {
        record.state = SimStepState::Failed;
        record
            .reason
            .get_or_insert_with(|| "a Task failed".to_string());
        // Keep the outputs of the given Tasks that succeeded, in run order:
        // a failed Step cannot have run all of its Tasks.
        if let Some(Placement::Explicit(ran)) = &mut record.placement {
            record.unplaced = std::mem::take(ran)
                .into_iter()
                .map(|(_, _, outputs)| outputs)
                .collect();
        }
        return Ok(None);
    }
    record.state = SimStepState::Succeeded;
    // Tasks given explicitly are placed only when they are as many as the
    // Step's Tasks, which bounds the walk by the Tasks that ran; a Task given
    // twice takes no second index, so the Step stays incomplete.
    if let Some(Placement::Explicit(ran)) = &mut record.placement {
        let ran = std::mem::take(ran);
        if ran.len() == task_count {
            if let Some(space) = step.parameter_space.as_ref() {
                let mut index = ChunkTaskIndex::new(space)?;
                for (action, value, outputs) in ran {
                    if let Some(slot) = index.take(&action, value) {
                        record.task_outputs.insert(slot, outputs);
                    }
                }
            }
        } else {
            record.unplaced = ran.into_iter().map(|(_, _, outputs)| outputs).collect();
        }
    }
    // With output variables, count the Tasks whose outputs arrived, so a Task
    // given twice with `--tasks` does not stand in for another.
    record.all_tasks_ran = if declares_outputs {
        record.task_outputs.len() == task_count
    } else {
        ran_all
    };
    if !record.complete() {
        return Ok(None);
    }
    Ok(Some(if declares_outputs {
        record.task_outputs.values().cloned().collect()
    } else {
        vec![TaskOutputs::new(); task_count]
    }))
}

/// The `steps` entry of `--output json|yaml` for one Step or body Step
/// instance, named `name`.
fn step_json(
    name: &str,
    step: &Step,
    record: &StepRecord,
) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert("name".into(), name.into());
    m.insert("state".into(), record.state.as_str().into());
    m.insert(
        "task_count".into(),
        step.tasks_known()
            .map_or(serde_json::Value::Null, Into::into),
    );
    if record.state == SimStepState::Succeeded && !record.complete() {
        m.insert("incomplete".into(), true.into());
    }
    if let Some(reason) = &record.reason {
        m.insert("reason".into(), reason.clone().into());
    }
    if record.supplied {
        m.insert("inputs_supplied".into(), true.into());
    }
    if step.output_variables.is_some() && record.placement.is_some() {
        // One entry per Task up to the last whose outputs arrived (`null`
        // for a Task without them), so a run that stopped early lists only
        // what ran; a complete Step has one per Task.
        let len = if record.complete() {
            record.task_outputs.len()
        } else {
            record
                .task_outputs
                .keys()
                .next_back()
                .map_or(0, |last| last + 1)
        };
        let to_json = |t: &TaskOutputs| {
            serde_json::Value::Object(
                t.iter()
                    .map(|(k, v)| (k.clone(), crate::common::expr_value_to_json(v)))
                    .collect(),
            )
        };
        let outputs: Vec<serde_json::Value> = if record.unplaced.is_empty() {
            (0..len)
                .map(|i| {
                    record
                        .task_outputs
                        .get(&i)
                        .map_or(serde_json::Value::Null, to_json)
                })
                .collect()
        } else {
            // Tasks given with --tasks or --task-param that do not cover the
            // Step: in the order they ran.
            m.insert("outputs_in_run_order".into(), true.into());
            record.unplaced.iter().map(to_json).collect()
        };
        m.insert("outputs".into(), outputs.into());
    }
    m
}

fn values_json(values: &IndexMap<String, ExprValue>) -> serde_json::Value {
    serde_json::Value::Object(
        values
            .iter()
            .map(|(k, v)| (k.clone(), crate::common::expr_value_to_json(v)))
            .collect(),
    )
}

impl Simulator {
    /// The simulator for `job`, created from `jt` with `params` and `vctx`,
    /// for a run of the entries `to_run`.
    pub(super) fn new(
        jt: JobTemplate,
        params: JobParameterValues,
        vctx: ValidationContext,
        job: Job,
        to_run: &[Node],
    ) -> Result<Self, RunError> {
        let graph = openjd_model::StepDependencyGraph::new(&job)?;
        let node = |i: usize| {
            let n = graph.node(i).expect("a node of the graph");
            match n.kind {
                StepNodeKind::Step => Node::Step(n.step_index),
                StepNodeKind::Loop => Node::Loop(n.step_index),
            }
        };
        let order: Vec<Node> = graph.topo_sorted()?.into_iter().map(node).collect();
        let template_order: Vec<Node> = (0..graph.node_count()).map(node).collect();
        let records = job.steps.iter().map(StepRecord::new).collect();
        let loop_records = job
            .loops
            .iter()
            .map(|l| LoopRecord {
                state: match l.binding {
                    LoopBinding::Bound(_) => SimStepState::Bound,
                    LoopBinding::PendingInputs(_) => SimStepState::Pending,
                    LoopBinding::Skipped => SimStepState::Skipped,
                },
                reason: None,
                progress: None,
                iterations: Vec::new(),
                result: None,
                supplied: false,
            })
            .collect();
        let read = crate::common::read_producers(&job);
        let mut inputs = StepInputs::new();
        let mut tasks_known_pending = BTreeSet::new();
        for step in job.steps.iter().filter(|s| read.contains(&s.name)) {
            if matches!(step.binding, StepBinding::Skipped) {
                inputs
                    .add_producer(step, ProducerState::Skipped)
                    .map_err(internal)?;
            } else if step.tasks_known().is_some() {
                tasks_known_pending.insert(step.name.clone());
            }
        }
        for l in job.loops.iter().filter(|l| read.contains(&l.name)) {
            if matches!(l.binding, LoopBinding::Skipped) {
                inputs
                    .add_loop_producer(l, LoopProducerState::Skipped)
                    .map_err(internal)?;
            }
        }
        let mut index_of: HashMap<String, Node> = job
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.clone(), Node::Step(i)))
            .collect();
        index_of.extend(
            job.loops
                .iter()
                .enumerate()
                .map(|(i, l)| (l.name.clone(), Node::Loop(i))),
        );
        let control_flow = jt
            .profile()
            .has_extension(openjd_model::types::ModelExtension::ControlFlow);
        Ok(Self {
            control_flow,
            index_of,
            jt,
            params,
            vctx,
            job,
            records,
            loop_records,
            running: None,
            iteration_inputs: StepInputs::new(),
            inputs,
            tasks_known_pending,
            read,
            generation: 0,
            changed: HashMap::new(),
            advanced: HashMap::new(),
            in_run: to_run.iter().copied().collect(),
            order,
            template_order,
            failed_outputs: BTreeSet::new(),
        })
    }

    /// Print what `create_job` decided: the job outputs it evaluated, the
    /// Steps and loops it skipped, and what is known of each pending Step
    /// and loop.
    pub(super) fn report_initial(&self) {
        for output in self.job.outputs.iter().flatten() {
            if let Some(value) = &output.evaluated {
                say(&job_output_line(output, value));
            }
        }
        for node in self
            .template_order
            .iter()
            .filter(|n| self.in_run.contains(n))
        {
            match *node {
                Node::Step(i) => {
                    let step = &self.job.steps[i];
                    match step.binding {
                        StepBinding::Skipped => {
                            say(&format!("Step '{}' skipped (enableIf is false)", step.name))
                        }
                        StepBinding::PendingInputs(_) => {
                            say(&format!("Step '{}' is pending inputs:", step.name));
                            for line in crate::common::binding_lines(step) {
                                say(&format!("  {line}"));
                            }
                        }
                        StepBinding::Bound => {}
                    }
                }
                Node::Loop(li) => {
                    let l = &self.job.loops[li];
                    match l.binding {
                        LoopBinding::Skipped => {
                            say(&format!("Loop '{}' skipped (enableIf is false)", l.name))
                        }
                        LoopBinding::PendingInputs(_) => {
                            say(&format!("Loop '{}' is pending inputs:", l.name));
                            for line in crate::common::loop_binding_lines(l) {
                                say(&format!("  {line}"));
                            }
                        }
                        LoopBinding::Bound(_) => {}
                    }
                }
            }
        }
    }

    /// Give the outputs of the Steps and loops the target reads, from
    /// `--step-inputs` (plan C1, Options, and L8): `values` maps
    /// `Steps.P.Output.V` to a value, which for a Step with a
    /// `parameterSpace` is a list with one value per Task, and for a loop
    /// one value of the output's declared type. Each value is checked
    /// against its declared type and constraints as a reported value is
    /// (the `openjd_output_var` collector). Then check that every `Steps.*`
    /// value the target reads is concrete, so that a missing one is reported
    /// before the Session starts.
    ///
    /// For a target loop, the producers it may read are its dependencies
    /// (RFC 0013 §`Steps.*` in and around a loop), and what it reads is
    /// found from its pending groups, the body Step instances of iteration
    /// 0, and its boundary expressions.
    pub(super) fn give_step_inputs(
        &mut self,
        target: Node,
        values: Option<&serde_json::Map<String, serde_json::Value>>,
    ) -> Result<(), RunError> {
        let target_name = self.node_name(target).to_string();
        let target_kind = match target {
            Node::Step(_) => "Step",
            Node::Loop(_) => "Loop",
        };
        // The producers whose outputs the target may read: a Step's groups'
        // inputs while pending, and its script and Step Environments; a
        // loop's dependencies.
        let producers: BTreeSet<String> = match target {
            Node::Step(i) => self.job.steps[i]
                .input_symbols()
                .iter()
                .filter(|s| s.contains(".Output.") || s.contains(".RawOutput."))
                .filter_map(|s| crate::common::step_of_symbol(s).map(str::to_string))
                .collect(),
            Node::Loop(li) => self.job.loops[li]
                .dependencies
                .iter()
                .flatten()
                .map(|d| d.depends_on.clone())
                .collect(),
        };
        let mut by_producer: BTreeMap<String, Vec<(String, &serde_json::Value)>> =
            Default::default();
        for (key, value) in values.into_iter().flatten() {
            let parts: Vec<&str> = key.split('.').collect();
            let [root, p, member, name] = parts.as_slice() else {
                return Err(format!(
                    "--step-inputs: '{key}' is not a Steps.<Step>.Output.<Name> symbol."
                )
                .into());
            };
            if *root != "Steps" || !matches!(*member, "Output" | "TaskParam") {
                return Err(format!(
                    "--step-inputs: '{key}' is not a Steps.<Step>.Output.<Name> symbol."
                )
                .into());
            }
            let Some(node) = self.node_of(p) else {
                if let Some(l) = self.loop_of_body_step(p) {
                    return Err(format!(
                        "--step-inputs: Step '{p}' is a body Step of loop '{l}', and its outputs are not available outside the loop."
                    )
                    .into());
                }
                return Err(format!("--step-inputs: the Job has no Step '{p}'.").into());
            };
            self.check_supplied_key(
                node,
                key,
                p,
                member,
                name,
                &producers,
                &target_name,
                target_kind,
            )?;
            by_producer
                .entry(p.to_string())
                .or_default()
                .push((name.to_string(), value));
        }
        for (p, given) in by_producer {
            match self.node_of(&p).expect("checked above") {
                Node::Step(pi) => {
                    let task_outputs = supplied_task_outputs(&self.job.steps[pi], &given)
                        .map_err(|e| format!("--step-inputs: Step '{p}': {e}"))?;
                    self.inputs
                        .add_producer(
                            &self.job.steps[pi],
                            ProducerState::Succeeded {
                                task_outputs: &task_outputs,
                            },
                        )
                        .map_err(|e| format!("--step-inputs: {e}"))?;
                    // Succeeded carries the TaskParam lists too; building
                    // them again as TasksKnown would replace the supplied
                    // outputs.
                    self.tasks_known_pending.remove(&p);
                    self.records[pi].supplied = true;
                }
                Node::Loop(li) => {
                    let outputs = supplied_loop_outputs(&self.job.loops[li], &given)
                        .map_err(|e| format!("--step-inputs: Loop '{p}': {e}"))?;
                    self.inputs
                        .add_loop_producer(
                            &self.job.loops[li],
                            LoopProducerState::Succeeded { outputs: &outputs },
                        )
                        .map_err(|e| format!("--step-inputs: {e}"))?;
                    self.loop_records[li].supplied = true;
                }
            }
            self.mark_changed(&p);
        }
        let missing = match target {
            Node::Step(i) => self.missing_for_step(i)?,
            Node::Loop(li) => self.missing_for_loop(li)?,
        };
        if missing.is_empty() {
            return Ok(());
        }
        // --step-inputs can supply the outputs only of a producer bound at
        // Job creation.
        let suppliable = missing.iter().any(|s| {
            let member = s.split('.').nth(2);
            matches!(member, Some("Output" | "RawOutput"))
                && crate::common::step_of_symbol(s)
                    .and_then(|p| self.node_of(p))
                    .is_some_and(|n| match n {
                        Node::Step(j) => self.job.steps[j].binding.is_bound(),
                        Node::Loop(j) => matches!(self.job.loops[j].binding, LoopBinding::Bound(_)),
                    })
        });
        let hint = if suppliable {
            format!(
                "Give them with --step-inputs, or run {target_kind} '{target_name}' with --run-dependencies."
            )
        } else {
            format!("Run {target_kind} '{target_name}' with --run-dependencies.")
        };
        Err(format!(
            "{target_kind} '{target_name}' reads Steps.* values of Steps this run does not run: {}.\n{hint}",
            missing.join(", ")
        )
        .into())
    }

    /// Check one `--step-inputs` key, `Steps.<p>.<member>.<name>`, for
    /// producer `node`.
    #[allow(clippy::too_many_arguments)]
    fn check_supplied_key(
        &self,
        node: Node,
        key: &str,
        p: &str,
        member: &str,
        name: &str,
        producers: &BTreeSet<String>,
        target_name: &str,
        target_kind: &str,
    ) -> Result<(), RunError> {
        match node {
            Node::Step(pi) => {
                let producer = &self.job.steps[pi];
                if matches!(producer.binding, StepBinding::Skipped) {
                    return Err(format!(
                        "--step-inputs: Step '{p}' is skipped by its enableIf, so its Steps.* values are null; remove '{key}'."
                    )
                    .into());
                }
                if member == "TaskParam" {
                    return Err(if producer.tasks_known().is_some() {
                        format!(
                            "--step-inputs: the Tasks of Step '{p}' are known at Job creation, so '{key}' comes from its parameter space; remove it."
                        )
                    } else {
                        format!(
                            "--step-inputs: the Tasks of Step '{p}' are determined at run time, which --step-inputs cannot simulate; use --run-dependencies."
                        )
                    }
                    .into());
                }
                if !producers.contains(p) {
                    return Err(format!(
                        "--step-inputs: {target_kind} '{target_name}' does not read the outputs of Step '{p}'."
                    )
                    .into());
                }
                if !producer.binding.is_bound() {
                    return Err(format!(
                        "--step-inputs: Step '{p}' is pending inputs itself, which --step-inputs cannot simulate; use --run-dependencies."
                    )
                    .into());
                }
                if !producer
                    .output_variables
                    .iter()
                    .flatten()
                    .any(|v| v.name == name)
                {
                    return Err(format!(
                        "--step-inputs: Step '{p}' declares no output variable '{name}'."
                    )
                    .into());
                }
            }
            Node::Loop(li) => {
                let producer = &self.job.loops[li];
                if matches!(producer.binding, LoopBinding::Skipped) {
                    return Err(format!(
                        "--step-inputs: Loop '{p}' is skipped by its enableIf, so its Steps.* values are null; remove '{key}'."
                    )
                    .into());
                }
                if member == "TaskParam" {
                    return Err(format!(
                        "--step-inputs: Loop '{p}' has no Task parameters; remove '{key}'."
                    )
                    .into());
                }
                if !producers.contains(p) {
                    return Err(format!(
                        "--step-inputs: {target_kind} '{target_name}' does not read the outputs of Loop '{p}'."
                    )
                    .into());
                }
                if !matches!(producer.binding, LoopBinding::Bound(_)) {
                    return Err(format!(
                        "--step-inputs: Loop '{p}' is pending inputs itself, which --step-inputs cannot simulate; use --run-dependencies."
                    )
                    .into());
                }
                if producer.output_variable(name).is_none() {
                    return Err(format!(
                        "--step-inputs: Loop '{p}' declares no output variable '{name}'."
                    )
                    .into());
                }
            }
        }
        Ok(())
    }

    /// The `Steps.*` values target Step `i` reads that are not concrete: its
    /// groups' inputs while it is pending, and what its script and Step
    /// Environments read.
    fn missing_for_step(&mut self, i: usize) -> Result<Vec<String>, RunError> {
        self.ensure_for_step(i)?;
        // Both calls are pure; the run's first settle advances the Step for
        // real.
        let step = &self.job.steps[i];
        let step = match step.binding {
            StepBinding::PendingInputs(_) => match openjd_model::advance_step_binding(
                &self.jt,
                &self.params,
                &self.vctx,
                &self.job,
                i,
                &self.inputs,
                self.committed_task_count(Some(Slot::Top(i))),
            ) {
                Ok(step) => step,
                Err(e) if e.is_caller_error() => return Err(internal(e)),
                // A group that fails fails the Step in the run's first settle.
                Err(_) => return Ok(Vec::new()),
            },
            _ => step.clone(),
        };
        Ok(step
            .input_symbols()
            .into_iter()
            .filter(|s| !self.is_concrete(s))
            .collect())
    }

    /// The `Steps.*` values of the dependencies of target loop `li` that the
    /// loop and its body read and that are not concrete: its groups' inputs
    /// while it is pending; otherwise those of the body Step instances of
    /// iteration 0 (once the loop is bound with iterations), and of its
    /// boundary expressions. Each call is pure.
    fn missing_for_loop(&mut self, li: usize) -> Result<Vec<String>, RunError> {
        let producers: Vec<String> = self.job.loops[li]
            .dependencies
            .iter()
            .flatten()
            .map(|d| d.depends_on.clone())
            .collect();
        self.ensure_task_params(producers.iter().map(String::as_str))?;
        let job_loop = match openjd_model::advance_loop_binding(
            &self.jt,
            &self.params,
            &self.vctx,
            &self.job,
            li,
            &self.inputs,
        ) {
            Ok(l) => l,
            Err(e) if e.is_caller_error() => return Err(internal(e)),
            // A group that fails fails the loop in the run's first settle.
            Err(_) => return Ok(Vec::new()),
        };
        let mut symbols: BTreeSet<String> = BTreeSet::new();
        match &job_loop.binding {
            LoopBinding::PendingInputs(pending) => {
                for group in [
                    waiting_on(&pending.enablement),
                    waiting_on(&pending.bound),
                    waiting_on(&pending.start),
                ] {
                    symbols.extend(group.into_iter().flatten().cloned());
                }
            }
            LoopBinding::Skipped => {}
            LoopBinding::Bound(bound) => {
                let body: BTreeSet<&str> = job_loop.body.iter().map(String::as_str).collect();
                let outside = |s: &String| {
                    crate::common::step_of_symbol(s).is_some_and(|p| !body.contains(p))
                };
                let mut expressions = vec![LoopExpression::Until, LoopExpression::While];
                expressions.extend(
                    job_loop
                        .variables
                        .iter()
                        .map(|v| LoopExpression::Next(v.name.clone())),
                );
                expressions.extend(
                    job_loop
                        .output_variables
                        .iter()
                        .map(|o| LoopExpression::OutputValue(o.name.clone())),
                );
                for e in &expressions {
                    let read = job_loop.expression_inputs(&self.jt, e).map_err(internal)?;
                    symbols.extend(read.into_iter().filter(outside));
                }
                if bound.has_iterations() {
                    let mut probe = self.job.clone();
                    probe.loops[li] = job_loop.clone();
                    let progress = LoopProgress::new(&job_loop).expect("a bound loop");
                    match openjd_model::start_iteration(
                        &self.jt,
                        &self.params,
                        &self.vctx,
                        &probe,
                        li,
                        &progress,
                        &self.inputs,
                        0,
                    ) {
                        Ok(iteration) => {
                            for step in &iteration.steps {
                                symbols.extend(step.input_symbols().into_iter().filter(outside));
                            }
                        }
                        Err(e) if e.is_caller_error() => return Err(internal(e)),
                        // An instance group that fails fails the loop when
                        // the run starts iteration 0.
                        Err(_) => {}
                    }
                }
            }
        }
        Ok(symbols
            .into_iter()
            .filter(|s| !self.is_concrete(s))
            .collect())
    }

    /// Whether the Job Template lists `CONTROL_FLOW`.
    pub(super) fn control_flow(&self) -> bool {
        self.control_flow
    }

    fn step_index(&self, name: &str) -> Option<usize> {
        match self.index_of.get(name) {
            Some(Node::Step(i)) => Some(*i),
            _ => None,
        }
    }

    /// The top-level Step or loop named `name`.
    pub(super) fn node_of(&self, name: &str) -> Option<Node> {
        self.index_of.get(name).copied()
    }

    /// The name of top-level entry `node`.
    pub(super) fn node_name(&self, node: Node) -> &str {
        match node {
            Node::Step(i) => &self.job.steps[i].name,
            Node::Loop(li) => &self.job.loops[li].name,
        }
    }

    /// The loop whose body has a Step named `name`.
    fn loop_of_body_step(&self, name: &str) -> Option<&str> {
        self.job
            .loops
            .iter()
            .find(|l| l.body.iter().any(|b| b == name))
            .map(|l| l.name.as_str())
    }

    fn state_of(&self, node: Node) -> SimStepState {
        match node {
            Node::Step(i) => self.records[i].state,
            Node::Loop(li) => self.loop_records[li].state,
        }
    }

    fn reason_of(&self, node: Node) -> Option<&str> {
        match node {
            Node::Step(i) => self.records[i].reason.as_deref(),
            Node::Loop(li) => self.loop_records[li].reason.as_deref(),
        }
    }

    /// Record that the Step or loop named `name` changed state in a way that
    /// can make its `Steps.*` symbols concrete.
    fn mark_changed(&mut self, name: &str) {
        self.generation += 1;
        self.changed.insert(name.to_string(), self.generation);
    }

    /// Build into `inputs` the `TaskParam` lists of each of `producers`
    /// whose Tasks are known and that have not been built yet.
    fn ensure_task_params<'a>(
        &mut self,
        producers: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), RunError> {
        for p in producers {
            if self.tasks_known_pending.remove(p) {
                let pi = self
                    .step_index(p)
                    .ok_or_else(|| internal(format!("no Step '{p}'")))?;
                self.inputs
                    .add_producer(&self.job.steps[pi], ProducerState::TasksKnown)
                    .map_err(internal)?;
            }
        }
        Ok(())
    }

    /// Whether `symbol` (`Steps.<Step>.<Member>.<Name>`) is concrete,
    /// counting the `TaskParam` and `TaskRawParam` symbols of Steps in
    /// `tasks_known_pending`.
    fn is_concrete(&self, symbol: &str) -> bool {
        if self.inputs.is_concrete(symbol) {
            return true;
        }
        let mut parts = symbol.splitn(4, '.');
        let (_, Some(p), Some(member)) = (parts.next(), parts.next(), parts.next()) else {
            return false;
        };
        matches!(member, "TaskParam" | "TaskRawParam") && self.tasks_known_pending.contains(p)
    }

    /// Build the `TaskParam` lists Step `idx` may read.
    fn ensure_for_step(&mut self, idx: usize) -> Result<(), RunError> {
        let producers = self.job.steps[idx].input_producers();
        self.ensure_task_params(producers.iter().map(String::as_str))
    }

    /// The Task count of every Step and body Step instance whose Tasks are
    /// known, except `exclude`, for the Task-count limit (plan A19).
    fn committed_task_count(&self, exclude: Option<Slot>) -> u64 {
        let top: u64 = self
            .job
            .steps
            .iter()
            .enumerate()
            .filter(|(j, _)| exclude != Some(Slot::Top(*j)))
            .filter_map(|(_, s)| s.tasks_known())
            .sum();
        let running_last = self
            .running
            .map(|li| (li, self.loop_records[li].iterations.len().saturating_sub(1)));
        let mut body = 0u64;
        for (li, record) in self.loop_records.iter().enumerate() {
            for (n, it) in record.iterations.iter().enumerate() {
                for (j, step) in it.iteration.steps.iter().enumerate() {
                    if running_last == Some((li, n)) && exclude == Some(Slot::Body(j)) {
                        continue;
                    }
                    body = body.saturating_add(step.tasks_known().unwrap_or(0));
                }
            }
        }
        top.saturating_add(body)
    }

    /// Advance pending Step `idx` with the inputs known now, print each fact
    /// it gains, and record the result (see [`Advanced`]). A group that
    /// fails is printed and fails the Step.
    fn advance(&mut self, idx: usize) -> Result<Advanced, RunError> {
        self.ensure_for_step(idx)?;
        let name = self.job.steps[idx].name.clone();
        self.advanced.insert(name.clone(), self.generation);
        let committed = self.committed_task_count(Some(Slot::Top(idx)));
        let step = match openjd_model::advance_step_binding(
            &self.jt,
            &self.params,
            &self.vctx,
            &self.job,
            idx,
            &self.inputs,
            committed,
        ) {
            Ok(step) => step,
            Err(e) if e.is_caller_error() => return Err(internal(e)),
            Err(e) => {
                eprintln!("ERROR: {e}");
                let record = &mut self.records[idx];
                record.state = SimStepState::Failed;
                record.reason = Some(e.to_string());
                return Ok(Advanced::Failed);
            }
        };
        let fact = report_step_advance(&name, &self.job.steps[idx], &step);
        let mut opened = false;
        if fact.skipped {
            self.records[idx].state = SimStepState::Skipped;
            if self.read.contains(&name) {
                self.inputs
                    .add_producer(&step, ProducerState::Skipped)
                    .map_err(internal)?;
            }
            self.mark_changed(&name);
            opened = true;
        } else {
            if fact.tasks_known {
                if self.read.contains(&name) {
                    self.tasks_known_pending.insert(name.clone());
                }
                self.mark_changed(&name);
                opened = true;
            }
            if step.binding.is_bound() {
                self.records[idx].state = SimStepState::Bound;
            }
        }
        self.job.steps[idx] = step;
        Ok(if opened {
            Advanced::Opened
        } else {
            Advanced::Quiet
        })
    }

    /// Advance pending loop `li` with the inputs known now (RFC 0013 §Loop
    /// binding), print each fact it gains, and record the result. A group
    /// that fails is printed and fails the loop.
    fn advance_loop(&mut self, li: usize) -> Result<Advanced, RunError> {
        let producers = self.job.loops[li].input_producers();
        self.ensure_task_params(producers.iter().map(String::as_str))?;
        let name = self.job.loops[li].name.clone();
        self.advanced.insert(name.clone(), self.generation);
        let job_loop = match openjd_model::advance_loop_binding(
            &self.jt,
            &self.params,
            &self.vctx,
            &self.job,
            li,
            &self.inputs,
        ) {
            Ok(l) => l,
            Err(e) if e.is_caller_error() => return Err(internal(e)),
            Err(e) => {
                eprintln!("ERROR: {e}");
                let record = &mut self.loop_records[li];
                record.state = SimStepState::Failed;
                record.reason = Some(e.to_string());
                return Ok(Advanced::Failed);
            }
        };
        let before = &self.job.loops[li];
        let mut opened = false;
        if matches!(job_loop.binding, LoopBinding::Skipped) {
            say(&format!("Loop '{name}' skipped (enableIf is false)"));
            self.loop_records[li].state = SimStepState::Skipped;
            if self.read.contains(&name) {
                self.inputs
                    .add_loop_producer(&job_loop, LoopProducerState::Skipped)
                    .map_err(internal)?;
            }
            self.mark_changed(&name);
            opened = true;
        } else {
            if !loop_enablement_known(before) && loop_enablement_known(&job_loop) {
                say(&format!("Loop '{name}': enabled (enableIf is true)"));
            }
            if let (None, Some(n)) = (max_iterations(before), max_iterations(&job_loop)) {
                say(&format!("Loop '{name}': maxIterations {n}"));
            }
            if let LoopBinding::Bound(bound) = &job_loop.binding {
                if !bound.has_iterations() {
                    say(&format!(
                        "Loop '{name}': the first while is false, so it has 0 iterations"
                    ));
                }
                say(&format!("Loop '{name}' is ready"));
                self.loop_records[li].state = SimStepState::Bound;
            }
        }
        self.job.loops[li] = job_loop;
        Ok(if opened {
            Advanced::Opened
        } else {
            Advanced::Quiet
        })
    }

    /// Advance every pending Step and loop this run runs, in dependency
    /// order, until a pass changes nothing (a Step whose Tasks become known
    /// can make its consumers' `TaskParam` inputs concrete), then evaluate
    /// every job output whose inputs are concrete. Returns `true` when a
    /// group or a job output failed, which stops the run.
    pub(super) fn settle(&mut self) -> Result<bool, RunError> {
        // A Step whose group fails fails at once, but the other pending
        // Steps are still advanced and job outputs evaluated, as a scheduler
        // evaluates each as soon as its inputs are concrete; only Tasks stop.
        let mut failed = false;
        loop {
            let mut opened = false;
            for node in self.order.clone() {
                if !self.in_run.contains(&node)
                    || self.state_of(node) != SimStepState::Pending
                    || !self.is_fresh(node)
                {
                    continue;
                }
                let advanced = match node {
                    Node::Step(i) => self.advance(i)?,
                    Node::Loop(li) => self.advance_loop(li)?,
                };
                match advanced {
                    Advanced::Failed => failed = true,
                    Advanced::Opened => opened = true,
                    Advanced::Quiet => {}
                }
            }
            if !opened {
                break;
            }
        }
        let output_failed = self.evaluate_job_outputs()?;
        Ok(failed || output_failed)
    }

    /// Whether one of pending `node`'s producers changed state since it was
    /// last advanced, so advancing it could make more of it known.
    fn is_fresh(&self, node: Node) -> bool {
        let (name, producers) = match node {
            Node::Step(i) => (&self.job.steps[i].name, self.job.steps[i].input_producers()),
            Node::Loop(li) => (
                &self.job.loops[li].name,
                self.job.loops[li].input_producers(),
            ),
        };
        let since = self.advanced.get(name).copied().unwrap_or(0);
        producers
            .iter()
            .any(|p| self.changed.get(p).is_some_and(|&g| g > since))
    }

    /// Evaluate each job output not yet evaluated whose inputs are all
    /// concrete, and print it. Returns `true` when one fails; the others
    /// whose inputs are concrete are evaluated too.
    fn evaluate_job_outputs(&mut self) -> Result<bool, RunError> {
        let count = self.job.outputs.as_ref().map_or(0, Vec::len);
        let mut failed = false;
        for i in 0..count {
            let output = &self.job.outputs.as_ref().expect("counted")[i];
            if output.evaluated.is_some() || self.failed_outputs.contains(&i) {
                continue;
            }
            let symbols = openjd_model::job_output_inputs(&self.job, i);
            if !symbols.iter().all(|s| self.is_concrete(s)) {
                continue;
            }
            let producers: BTreeSet<String> = symbols
                .iter()
                .filter_map(|s| crate::common::step_of_symbol(s).map(str::to_string))
                .collect();
            self.ensure_task_params(producers.iter().map(String::as_str))?;
            let output = &self.job.outputs.as_ref().expect("counted")[i];
            match openjd_model::evaluate_job_output(
                &self.jt,
                &self.params,
                &self.vctx,
                &self.job,
                i,
                &self.inputs,
            ) {
                Ok(value) => {
                    let value = JobOutputValue::from(value);
                    say(&job_output_line(output, &value));
                    self.job.outputs.as_mut().expect("counted")[i].evaluated = Some(value);
                }
                Err(ModelError::Compatibility(message)) => return Err(internal(message)),
                Err(e) => {
                    eprintln!("ERROR: {e}");
                    self.failed_outputs.insert(i);
                    failed = true;
                }
            }
        }
        Ok(failed)
    }

    /// Why `node` can never run, when a dependency this run runs has failed
    /// or is blocked.
    fn blocked_reason(&self, node: Node) -> Option<String> {
        let deps = match node {
            Node::Step(i) => self.job.steps[i].dependencies.as_ref(),
            Node::Loop(li) => self.job.loops[li].dependencies.as_ref(),
        };
        for dep in deps.into_iter().flatten() {
            let j = self.node_of(&dep.depends_on)?;
            if !self.in_run.contains(&j) {
                continue;
            }
            match self.state_of(j) {
                SimStepState::Failed => {
                    return Some(format!("its dependency '{}' failed", dep.depends_on))
                }
                SimStepState::Blocked => {
                    return Some(format!("its dependency '{}' is blocked", dep.depends_on))
                }
                // A skipped Step behind a failure never satisfies dependsOn
                // (RFC 0011 §Conditional execution, Failure is not skipping).
                SimStepState::Skipped if self.reason_of(j).is_some() => {
                    return Some(format!(
                        "its dependency '{}' is skipped but never satisfies dependsOn",
                        dep.depends_on
                    ))
                }
                _ => {}
            }
        }
        None
    }

    /// The Step this run runs whose outputs `node` reads but that did not
    /// run all of its Tasks (`--maximum-tasks`, `--tasks`, `--task-param`).
    fn incomplete_producer(&self, node: Node) -> Option<String> {
        let producers = match node {
            Node::Step(i) => self.job.steps[i].input_producers(),
            Node::Loop(li) => self.job.loops[li].input_producers(),
        };
        producers.into_iter().find(|p| self.is_incomplete_top(p))
    }

    /// Whether top-level Step `p`, which this run runs, succeeded without
    /// running all of its Tasks.
    fn is_incomplete_top(&self, p: &str) -> bool {
        self.step_index(p).is_some_and(|j| {
            self.in_run.contains(&Node::Step(j))
                && self.records[j].state == SimStepState::Succeeded
                && !self.records[j].complete()
        })
    }

    /// At the end of a run that stopped early, mark every Step and loop of
    /// the run that can never run because a dependency failed or is blocked,
    /// so its state and the job outputs waiting on it say so (RFC 0011 §Job
    /// outputs item 4). An entry the run did not reach for another reason
    /// (an interruption, a failed job output) keeps its state.
    pub(super) fn block_unreachable(&mut self, interrupted: bool) {
        // After an interruption the run did not reach Steps because it
        // stopped, not because they can never run.
        if interrupted {
            return;
        }
        for node in self.order.clone() {
            let done = match self.state_of(node) {
                SimStepState::Succeeded
                | SimStepState::Failed
                | SimStepState::Blocked
                | SimStepState::Running => true,
                SimStepState::Skipped => self.reason_of(node).is_some(),
                _ => false,
            };
            if !self.in_run.contains(&node) || done {
                continue;
            }
            if let Some(reason) = self.blocked_reason(node) {
                self.block(node, reason);
            }
        }
    }

    /// Record that `node` can never run, for `reason`. A skipped Step stays
    /// `SKIPPED`, which is terminal (RFC 0011 §Step states), but its
    /// dependents are blocked behind it.
    fn block(&mut self, node: Node, reason: String) {
        let kind = match node {
            Node::Step(_) => "Step",
            Node::Loop(_) => "Loop",
        };
        let name = self.node_name(node).to_string();
        let (state, stored) = match node {
            Node::Step(i) => {
                let r = &mut self.records[i];
                (&mut r.state, &mut r.reason)
            }
            Node::Loop(li) => {
                let r = &mut self.loop_records[li];
                (&mut r.state, &mut r.reason)
            }
        };
        if *state == SimStepState::Skipped {
            say(&format!(
                "{kind} '{name}' is skipped and never satisfies dependsOn: {reason}"
            ));
        } else {
            say(&format!("{kind} '{name}' is blocked: {reason}"));
            *state = SimStepState::Blocked;
        }
        *stored = Some(reason);
    }

    /// Start Step `idx` of the run (plan C1 items 1 to 4): block it behind a
    /// failed or blocked dependency, block a Step still pending because a
    /// producer did not run all of its Tasks (settle is the one place a
    /// pending Step advances, after each producer change), and for a bound
    /// Step attach its `Steps.*` inputs.
    ///
    /// `explicit_tasks` is whether the run gives this Step's Tasks with
    /// `--tasks` or `--task-param`, so its Actions do not follow iteration
    /// order.
    pub(super) fn begin_step(
        &mut self,
        idx: usize,
        explicit_tasks: bool,
    ) -> Result<Begin, RunError> {
        if self.records[idx].state == SimStepState::Failed {
            return Ok(Begin::NotRun);
        }
        if let Some(reason) = self.blocked_reason(Node::Step(idx)) {
            self.block(Node::Step(idx), reason);
            return Ok(Begin::NotRun);
        }
        // A pending Step is not advanced here: settle advanced it after the
        // last change of any of its producers, so nothing new can bind it,
        // and it stays pending (below).
        let name = self.job.steps[idx].name.clone();
        match self.records[idx].state {
            SimStepState::Skipped if explicit_tasks => {
                // As for a Step skipped at Job creation (preflight).
                eprintln!(
                    "ERROR: Step '{name}' is skipped by its enableIf; the Tasks given with \
                     --tasks or --task-param cannot be run."
                );
                Ok(Begin::Failed)
            }
            SimStepState::Skipped => {
                say(&format!("Step '{name}' is skipped; it runs no Tasks"));
                Ok(Begin::NotRun)
            }
            SimStepState::Pending => {
                if let Some(p) = self.incomplete_producer(Node::Step(idx)) {
                    self.block(Node::Step(idx), incomplete(&p));
                    return Ok(Begin::NotRun);
                }
                Err(internal(format!(
                    "Step '{name}' is still pending inputs after its dependencies finished: {}",
                    crate::common::binding_lines(&self.job.steps[idx]).join("; ")
                )))
            }
            SimStepState::Bound => {
                // attach_step_inputs needs the TaskParam lists only of the
                // producers whose TaskParam the Step reads.
                let task_param_producers: BTreeSet<String> = self.job.steps[idx]
                    .input_symbols()
                    .iter()
                    .filter(|s| s.contains(".TaskParam.") || s.contains(".TaskRawParam."))
                    .filter_map(|s| crate::common::step_of_symbol(s).map(str::to_string))
                    .collect();
                self.ensure_task_params(task_param_producers.iter().map(String::as_str))?;
                let prepared = prepare_bound(&self.job.steps[idx], &self.inputs, explicit_tasks);
                match prepared {
                    Ok((step, placement)) => {
                        let record = &mut self.records[idx];
                        record.task_outputs.clear();
                        record.unplaced.clear();
                        record.all_tasks_ran = false;
                        record.placement = placement;
                        Ok(Begin::Run(Box::new(step)))
                    }
                    Err(e) => {
                        if let Some(p) = self.incomplete_producer(Node::Step(idx)) {
                            self.block(Node::Step(idx), incomplete(&p));
                            return Ok(Begin::NotRun);
                        }
                        Err(e)
                    }
                }
            }
            other => Err(internal(format!(
                "Step '{name}' cannot start in state {}",
                other.as_str()
            ))),
        }
    }

    /// The Step and record of `slot`.
    fn slot_mut(&mut self, slot: Slot) -> (&Step, &mut StepRecord) {
        match slot {
            Slot::Top(i) => (&self.job.steps[i], &mut self.records[i]),
            Slot::Body(j) => {
                let li = self.running.expect("a loop is running");
                let it = self.loop_records[li]
                    .iterations
                    .last_mut()
                    .expect("an iteration is in progress");
                (&it.iteration.steps[j], &mut it.records[j])
            }
        }
    }

    /// Record a successful Action of `slot` whose Task parameter values were
    /// `action` (`None` for a Step without a `parameterSpace`): each Task's
    /// `outputs`, placed at its per-Task index. Prints each output value with
    /// its unit (plan C1 item 4).
    pub(super) fn record_action(
        &mut self,
        slot: Slot,
        action: Option<&TaskParameterSet>,
        outputs: Option<ActionOutputs>,
    ) {
        let Some(outputs) = outputs else {
            return;
        };
        let (step, record) = self.slot_mut(slot);
        record_outputs(step, record, action, outputs);
    }

    /// Set why `slot` failed, before [`finish_step`](Self::finish_step).
    pub(super) fn set_reason(&mut self, slot: Slot, reason: &str) {
        self.slot_mut(slot).1.reason = Some(reason.to_string());
    }

    /// Record that `slot` finished: failed, or succeeded. `ran_all` is
    /// whether every one of its Tasks ran (not so after `--maximum-tasks`,
    /// `--tasks`, or `--task-param`). A top-level Step that succeeded with
    /// all of them gives its outputs to its consumers and to the job
    /// outputs; a body Step instance, to the other instances of its
    /// iteration and to the boundary. A failed body Step instance fails its
    /// loop.
    pub(super) fn finish_step(
        &mut self,
        slot: Slot,
        failed: bool,
        ran_all: bool,
    ) -> Result<(), RunError> {
        let (step, record) = self.slot_mut(slot);
        let Some(task_outputs) = finish_record(step, record, failed, ran_all)? else {
            if failed {
                if let Slot::Body(j) = slot {
                    let li = self.running.expect("a loop is running");
                    let name = self.instance_name(j);
                    self.fail_loop(li, format!("its body Step instance '{name}' failed"));
                }
            }
            return Ok(());
        };
        match slot {
            Slot::Top(idx) => {
                let step = &self.job.steps[idx];
                if self.read.contains(&step.name) {
                    // Succeeded carries the TaskParam lists too; building
                    // them again as TasksKnown would replace the outputs.
                    self.tasks_known_pending.remove(&step.name);
                    self.inputs
                        .add_producer(
                            step,
                            ProducerState::Succeeded {
                                task_outputs: &task_outputs,
                            },
                        )
                        .map_err(internal)?;
                    let name = step.name.clone();
                    self.mark_changed(&name);
                }
            }
            Slot::Body(j) => {
                let li = self.running.expect("a loop is running");
                let it = self.loop_records[li]
                    .iterations
                    .last()
                    .expect("an iteration");
                self.iteration_inputs
                    .add_producer(
                        &it.iteration.steps[j],
                        ProducerState::Succeeded {
                            task_outputs: &task_outputs,
                        },
                    )
                    .map_err(internal)?;
            }
        }
        Ok(())
    }

    /// Record that loop `li` failed, for `reason`, and print its first line
    /// (an error's located message is printed on stderr).
    fn fail_loop(&mut self, li: usize, reason: String) {
        let name = &self.job.loops[li].name;
        let first = reason
            .lines()
            .next()
            .unwrap_or_default()
            .trim_end_matches(':');
        if first.starts_with(&format!("Loop '{name}' failed")) {
            say(first);
        } else {
            say(&format!("Loop '{name}' failed: {first}"));
        }
        let record = &mut self.loop_records[li];
        record.state = SimStepState::Failed;
        record.reason = Some(reason);
        self.running = None;
    }

    // ── Loops ─────────────────────────────────────────────────────

    /// Start loop `li` of the run (plan L8): block it behind a failed or
    /// blocked dependency, or a producer that did not run all of its Tasks;
    /// end a bound loop whose first `while` is false with
    /// `end_without_iterations`; and set any other bound loop running.
    pub(super) fn begin_loop(&mut self, li: usize) -> Result<LoopBegin, RunError> {
        let node = Node::Loop(li);
        if self.loop_records[li].state == SimStepState::Failed {
            return Ok(LoopBegin::NotRun);
        }
        if let Some(reason) = self.blocked_reason(node) {
            self.block(node, reason);
            return Ok(LoopBegin::NotRun);
        }
        let name = self.job.loops[li].name.clone();
        match self.loop_records[li].state {
            SimStepState::Skipped => {
                say(&format!("Loop '{name}' is skipped; it runs no iterations"));
                Ok(LoopBegin::NotRun)
            }
            SimStepState::Pending => {
                if let Some(p) = self.incomplete_producer(node) {
                    self.block(node, incomplete(&p));
                    return Ok(LoopBegin::NotRun);
                }
                Err(internal(format!(
                    "Loop '{name}' is still pending inputs after its dependencies finished: {}",
                    crate::common::loop_binding_lines(&self.job.loops[li]).join("; ")
                )))
            }
            SimStepState::Bound => {
                let LoopBinding::Bound(bound) = &self.job.loops[li].binding else {
                    return Err(internal(format!("Loop '{name}' is ready but not bound")));
                };
                if !bound.has_iterations() {
                    return match openjd_model::end_without_iterations(&self.job, li) {
                        Ok(result) => {
                            self.succeed_loop(li, result)?;
                            Ok(LoopBegin::NotRun)
                        }
                        Err(e) if e.is_caller_error() => Err(internal(e)),
                        Err(e) => {
                            eprintln!("ERROR: {e}");
                            self.fail_loop(li, e.to_string());
                            Ok(LoopBegin::Failed)
                        }
                    };
                }
                // The body and the boundary read the loop's dependencies'
                // TaskParam lists from `inputs`.
                let deps: Vec<String> = self.job.loops[li]
                    .dependencies
                    .iter()
                    .flatten()
                    .map(|d| d.depends_on.clone())
                    .collect();
                self.ensure_task_params(deps.iter().map(String::as_str))?;
                let record = &mut self.loop_records[li];
                record.progress = LoopProgress::new(&self.job.loops[li]);
                record.state = SimStepState::Running;
                self.running = Some(li);
                Ok(LoopBegin::Run)
            }
            other => Err(internal(format!(
                "Loop '{name}' cannot start in state {}",
                other.as_str()
            ))),
        }
    }

    /// Record that loop `li` succeeded with `result`, print it, and give its
    /// outputs to its consumers and to the job outputs.
    fn succeed_loop(&mut self, li: usize, result: LoopResult) -> Result<(), RunError> {
        let name = self.job.loops[li].name.clone();
        say(&format!(
            "Loop '{name}' succeeded after {} {} ({})",
            result.iterations,
            iterations_noun(result.iterations),
            ended_by(result.ended_by)
        ));
        if let Some(outputs) = value_list(&result.outputs, |n| self.loop_unit(li, n)) {
            say(&format!("Loop '{name}' outputs: {outputs}"));
        }
        if self.read.contains(&name) {
            self.inputs
                .add_loop_producer(
                    &self.job.loops[li],
                    LoopProducerState::Succeeded {
                        outputs: &result.outputs,
                    },
                )
                .map_err(internal)?;
            self.mark_changed(&name);
        }
        let record = &mut self.loop_records[li];
        record.state = SimStepState::Succeeded;
        record.result = Some(result);
        record.progress = None;
        self.running = None;
        Ok(())
    }

    fn loop_unit(&self, li: usize, output: &str) -> Option<String> {
        self.job.loops[li]
            .output_variable(output)
            .and_then(|o| o.unit.clone())
    }

    /// The template of loop `li`.
    fn step_loop(&self, li: usize) -> Result<&StepLoop, RunError> {
        let job_loop = &self.job.loops[li];
        self.jt
            .steps
            .get(job_loop.entry_index)
            .and_then(|e| e.as_loop())
            .ok_or_else(|| internal(format!("no loop '{}' in the template", job_loop.name)))
    }

    /// The body Steps of running loop `li`, in dependency order.
    pub(super) fn body_order(&self, li: usize) -> Result<Vec<usize>, RunError> {
        Ok(body_order(self.step_loop(li)?))
    }

    /// `<Loop>[<N>].<Step>` for body Step instance `j` of the iteration in
    /// progress.
    pub(super) fn instance_name(&self, j: usize) -> String {
        self.running
            .and_then(|li| self.loop_records[li].iterations.last())
            .and_then(|it| it.iteration.instance_name(j))
            .unwrap_or_default()
    }

    /// Start the next iteration of running loop `li` (RFC 0013 §Iterations):
    /// print its index and loop variable values, create its body Step
    /// instances with `start_iteration`, and record what each is. Returns
    /// `false` when an instance's group fails, which fails the loop.
    pub(super) fn start_next_iteration(&mut self, li: usize) -> Result<bool, RunError> {
        let progress = self.loop_records[li]
            .progress
            .clone()
            .ok_or_else(|| internal("a running loop has no progress"))?;
        let name = self.job.loops[li].name.clone();
        let n = progress.iteration;
        match value_list(&progress.values, |_| None) {
            Some(values) => say(&format!("Loop '{name}' iteration {n}: {values}")),
            None => say(&format!("Loop '{name}' iteration {n}")),
        }
        let committed = self.committed_task_count(None);
        let iteration = match openjd_model::start_iteration(
            &self.jt,
            &self.params,
            &self.vctx,
            &self.job,
            li,
            &progress,
            &self.inputs,
            committed,
        ) {
            Ok(iteration) => iteration,
            Err(e) if e.is_caller_error() => return Err(internal(e)),
            Err(e) => {
                eprintln!("ERROR: {e}");
                self.fail_loop(li, e.to_string());
                return Ok(false);
            }
        };
        self.iteration_inputs = self.inputs.clone();
        for (j, step) in iteration.steps.iter().enumerate() {
            let instance = iteration.instance_name(j).unwrap_or_default();
            match step.binding {
                StepBinding::Skipped => {
                    say(&format!("Step '{instance}' skipped (enableIf is false)"));
                    self.iteration_inputs
                        .add_producer(step, ProducerState::Skipped)
                        .map_err(internal)?;
                }
                StepBinding::PendingInputs(_) => {
                    say(&format!("Step '{instance}' is pending inputs:"));
                    for line in crate::common::binding_lines(step) {
                        say(&format!("  {line}"));
                    }
                    if step.tasks_known().is_some() {
                        self.iteration_inputs
                            .add_producer(step, ProducerState::TasksKnown)
                            .map_err(internal)?;
                    }
                }
                StepBinding::Bound => {
                    self.iteration_inputs
                        .add_producer(step, ProducerState::TasksKnown)
                        .map_err(internal)?;
                }
            }
        }
        let records = iteration.steps.iter().map(StepRecord::new).collect();
        self.loop_records[li].iterations.push(IterationRecord {
            values: progress.values.clone(),
            iteration,
            records,
            outputs: None,
        });
        Ok(true)
    }

    /// Advance every pending body Step instance of the iteration in
    /// progress, in body order, until a pass changes nothing (C1 item 6
    /// within the iteration). Returns `true` when an instance's group
    /// failed, which fails the loop.
    pub(super) fn settle_iteration(&mut self) -> Result<bool, RunError> {
        let li = self.running.ok_or_else(|| internal("no loop is running"))?;
        let order = self.body_order(li)?;
        loop {
            let mut opened = false;
            for &j in &order {
                let it = self.loop_records[li]
                    .iterations
                    .last()
                    .expect("an iteration");
                if it.records[j].state != SimStepState::Pending {
                    continue;
                }
                match self.advance_instance(li, j)? {
                    Advanced::Failed => return Ok(true),
                    Advanced::Opened => opened = true,
                    Advanced::Quiet => {}
                }
            }
            if !opened {
                return Ok(false);
            }
        }
    }

    /// Advance pending body Step instance `j` of the iteration in progress
    /// of loop `li`, print each fact it gains, and record the result.
    fn advance_instance(&mut self, li: usize, j: usize) -> Result<Advanced, RunError> {
        let committed = self.committed_task_count(Some(Slot::Body(j)));
        let record = &self.loop_records[li];
        let it = record.iterations.last().expect("an iteration");
        let progress = record.progress.as_ref().expect("a running loop");
        let name = it.iteration.instance_name(j).unwrap_or_default();
        let step = match openjd_model::advance_body_step_binding(
            &self.jt,
            &self.params,
            &self.vctx,
            &self.job,
            li,
            progress,
            &it.iteration,
            j,
            &self.iteration_inputs,
            committed,
        ) {
            Ok(step) => step,
            Err(e) if e.is_caller_error() => return Err(internal(e)),
            Err(e) => {
                eprintln!("ERROR: {e}");
                let it = self.loop_records[li]
                    .iterations
                    .last_mut()
                    .expect("an iteration");
                it.records[j].state = SimStepState::Failed;
                it.records[j].reason = Some(e.to_string());
                self.fail_loop(li, format!("its body Step instance '{name}' failed"));
                return Ok(Advanced::Failed);
            }
        };
        let fact = report_step_advance(&name, &it.iteration.steps[j], &step);
        let mut opened = false;
        if fact.skipped {
            self.iteration_inputs
                .add_producer(&step, ProducerState::Skipped)
                .map_err(internal)?;
            opened = true;
        } else if fact.tasks_known {
            self.iteration_inputs
                .add_producer(&step, ProducerState::TasksKnown)
                .map_err(internal)?;
            opened = true;
        }
        let it = self.loop_records[li]
            .iterations
            .last_mut()
            .expect("an iteration");
        it.records[j].state = SimStepState::of_step(&step);
        it.iteration.steps[j] = step;
        Ok(if opened {
            Advanced::Opened
        } else {
            Advanced::Quiet
        })
    }

    /// Start body Step instance `j` of the iteration in progress, as
    /// [`begin_step`](Self::begin_step) starts a top-level Step: block it
    /// behind a failed or blocked dependency in the iteration, or a producer
    /// that did not run all of its Tasks, and for a bound instance attach
    /// its `Steps.*` inputs.
    pub(super) fn begin_instance(&mut self, j: usize) -> Result<Begin, RunError> {
        let li = self.running.ok_or_else(|| internal("no loop is running"))?;
        let name = self.instance_name(j);
        let it = self.loop_records[li]
            .iterations
            .last()
            .expect("an iteration");
        if it.records[j].state == SimStepState::Failed {
            return Ok(Begin::NotRun);
        }
        if let Some(reason) = self.instance_blocked_reason(j) {
            self.block_instance(j, reason);
            return Ok(Begin::NotRun);
        }
        let it = self.loop_records[li]
            .iterations
            .last()
            .expect("an iteration");
        match it.records[j].state {
            SimStepState::Skipped => {
                say(&format!("Step '{name}' is skipped; it runs no Tasks"));
                Ok(Begin::NotRun)
            }
            SimStepState::Pending => {
                if let Some(p) = self.instance_incomplete_producer(j) {
                    self.block_instance(j, incomplete(&p));
                    return Ok(Begin::NotRun);
                }
                Err(internal(format!(
                    "Step '{name}' is still pending inputs after its dependencies finished: {}",
                    crate::common::binding_lines(&it.iteration.steps[j]).join("; ")
                )))
            }
            SimStepState::Bound => {
                match prepare_bound(&it.iteration.steps[j], &self.iteration_inputs, false) {
                    Ok((step, placement)) => {
                        let it = self.loop_records[li]
                            .iterations
                            .last_mut()
                            .expect("an iteration");
                        let record = &mut it.records[j];
                        record.task_outputs.clear();
                        record.all_tasks_ran = false;
                        record.placement = placement;
                        Ok(Begin::Run(Box::new(step)))
                    }
                    Err(e) => {
                        if let Some(p) = self.instance_incomplete_producer(j) {
                            self.block_instance(j, incomplete(&p));
                            return Ok(Begin::NotRun);
                        }
                        Err(e)
                    }
                }
            }
            other => Err(internal(format!(
                "Step '{name}' cannot start in state {}",
                other.as_str()
            ))),
        }
    }

    /// Why body Step instance `j` of the iteration in progress can never
    /// run, when a body Step it depends on failed or is blocked in the
    /// iteration.
    fn instance_blocked_reason(&self, j: usize) -> Option<String> {
        let li = self.running?;
        let it = self.loop_records[li].iterations.last()?;
        for dep in it.iteration.steps[j].dependencies.iter().flatten() {
            let Some(k) = it
                .iteration
                .steps
                .iter()
                .position(|s| s.name == dep.depends_on)
            else {
                continue;
            };
            let dep_name = it.iteration.instance_name(k).unwrap_or_default();
            match it.records[k].state {
                SimStepState::Failed => return Some(format!("its dependency '{dep_name}' failed")),
                SimStepState::Blocked => {
                    return Some(format!("its dependency '{dep_name}' is blocked"))
                }
                _ => {}
            }
        }
        None
    }

    /// The producer, named as in the log, whose outputs body Step instance
    /// `j` reads but that did not run all of its Tasks: another instance of
    /// the iteration, or a dependency of the loop.
    fn instance_incomplete_producer(&self, j: usize) -> Option<String> {
        let li = self.running?;
        let it = self.loop_records[li].iterations.last()?;
        it.iteration.steps[j]
            .input_producers()
            .into_iter()
            .find_map(
                |p| match it.iteration.steps.iter().position(|s| s.name == p) {
                    Some(k) => (it.records[k].state == SimStepState::Succeeded
                        && !it.records[k].complete())
                    .then(|| it.iteration.instance_name(k).unwrap_or_default()),
                    None => self.is_incomplete_top(&p).then_some(p),
                },
            )
    }

    /// Record that body Step instance `j` of the iteration in progress can
    /// never run, for `reason`.
    fn block_instance(&mut self, j: usize, reason: String) {
        let name = self.instance_name(j);
        say(&format!("Step '{name}' is blocked: {reason}"));
        let li = self.running.expect("a loop is running");
        let it = self.loop_records[li]
            .iterations
            .last_mut()
            .expect("an iteration");
        it.records[j].state = SimStepState::Blocked;
        it.records[j].reason = Some(reason);
    }

    /// End the iteration in progress of loop `li` (RFC 0013 §The iteration
    /// boundary): when every instance of it succeeded with all of its Tasks
    /// or was skipped, evaluate the boundary with
    /// `evaluate_iteration_boundary`, print the iteration's loop output
    /// values, and continue, succeed, or fail the loop. Otherwise the
    /// iteration never completes (`--maximum-tasks`), and the loop is
    /// blocked.
    pub(super) fn end_iteration(&mut self, li: usize) -> Result<IterationEnd, RunError> {
        let name = self.job.loops[li].name.clone();
        let it = self.loop_records[li]
            .iterations
            .last()
            .expect("an iteration");
        let n = it.iteration.index;
        let unfinished = (0..it.records.len()).find(|&j| !it.records[j].done());
        if let Some(j) = unfinished {
            let instance = it.iteration.instance_name(j).unwrap_or_default();
            let why = match it.records[j].state {
                SimStepState::Succeeded => "did not run all of its Tasks",
                SimStepState::Blocked => "is blocked",
                SimStepState::Failed => "failed",
                _ => "did not run",
            };
            let reason = format!(
                "its body Step instance '{instance}' {why}, so iteration {n} never completes"
            );
            self.block(Node::Loop(li), reason);
            self.running = None;
            return Ok(IterationEnd::Blocked);
        }
        let progress = self.loop_records[li]
            .progress
            .clone()
            .ok_or_else(|| internal("a running loop has no progress"))?;
        let outcome = openjd_model::evaluate_iteration_boundary(
            &self.jt,
            &self.params,
            &self.vctx,
            &self.job,
            li,
            &progress,
            &self.iteration_inputs,
        );
        let so_far = match &outcome {
            Ok(BoundaryOutcome::Continue(next)) => Some(&next.outputs),
            Ok(BoundaryOutcome::Succeeded(result)) => Some(&result.outputs),
            Err(_) => None,
        };
        if let Some(so_far) = so_far {
            let values = iteration_outputs(&self.job.loops[li], so_far);
            if let Some(line) = value_list(&values, |o| self.loop_unit(li, o)) {
                say(&format!("Loop '{name}' iteration {n} outputs: {line}"));
            }
            let it = self.loop_records[li]
                .iterations
                .last_mut()
                .expect("an iteration");
            it.outputs = Some(values);
        }
        match outcome {
            Ok(BoundaryOutcome::Continue(next)) => {
                self.loop_records[li].progress = Some(next);
                Ok(IterationEnd::Continue)
            }
            Ok(BoundaryOutcome::Succeeded(result)) => {
                self.succeed_loop(li, result)?;
                Ok(IterationEnd::Succeeded)
            }
            Err(e) if e.is_caller_error() => Err(internal(e)),
            Err(e) => {
                eprintln!("ERROR: {e}");
                self.fail_loop(li, e.to_string());
                Ok(IterationEnd::Failed)
            }
        }
    }

    /// Record that running loop `li` stopped before its iteration in
    /// progress completed, because the run stopped (a failed Environment, or
    /// an interruption).
    pub(super) fn stop_loop(&mut self, li: usize, reason: &str) {
        if self.loop_records[li].state == SimStepState::Running {
            self.fail_loop(li, reason.to_string());
        }
        self.running = None;
    }

    /// The job outputs not evaluated, each with the Steps and loops it waits
    /// on and why each has not given its values; an output whose evaluation
    /// failed has none.
    pub(super) fn not_evaluated(&self) -> Vec<(String, Vec<String>)> {
        let mut result = Vec::new();
        for (i, output) in self.job.outputs.iter().flatten().enumerate() {
            if output.evaluated.is_some() {
                continue;
            }
            if self.failed_outputs.contains(&i) {
                result.push((output.declaration.name.clone(), Vec::new()));
                continue;
            }
            let mut steps: Vec<String> = openjd_model::job_output_inputs(&self.job, i)
                .iter()
                .filter(|s| !self.is_concrete(s))
                .filter_map(|s| crate::common::step_of_symbol(s).map(str::to_string))
                .collect();
            steps.dedup();
            let waits = steps
                .into_iter()
                .map(|p| match self.node_of(&p) {
                    None => format!("Step '{p}', which is unknown"),
                    Some(node) => {
                        let kind = match node {
                            Node::Step(_) => "Step",
                            Node::Loop(_) => "Loop",
                        };
                        let why = if !self.in_run.contains(&node) {
                            "this run does not run"
                        } else {
                            match self.state_of(node) {
                                SimStepState::Failed => "failed",
                                SimStepState::Blocked => "is blocked",
                                SimStepState::Succeeded => "did not run all of its Tasks",
                                _ => "the run stopped before",
                            }
                        };
                        format!("{kind} '{p}', which {why}")
                    }
                })
                .collect();
            result.push((output.declaration.name.clone(), waits));
        }
        result
    }

    /// Print the job outputs not evaluated at the end of the run.
    pub(super) fn report_not_evaluated(&self) {
        for (name, waits) in self.not_evaluated() {
            if waits.is_empty() {
                say(&format!(
                    "Job output {name} has no value: its evaluation failed"
                ));
            } else {
                say(&format!(
                    "Job output {name} was not evaluated: it waits on {}",
                    waits.join("; ")
                ));
            }
        }
    }

    /// The `steps` entries of `--output json|yaml`, in template order: one
    /// per Step, and for a loop its own entry followed by one per body Step
    /// instance, in run order (plan L8, Reporting).
    pub(super) fn steps_json(&self) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        for node in &self.template_order {
            match *node {
                Node::Step(i) => {
                    let step = &self.job.steps[i];
                    out.push(step_json(&step.name, step, &self.records[i]).into());
                }
                Node::Loop(li) => {
                    let l = &self.job.loops[li];
                    let record = &self.loop_records[li];
                    let mut m = serde_json::Map::new();
                    m.insert("name".into(), l.name.clone().into());
                    m.insert("kind".into(), "loop".into());
                    m.insert("state".into(), record.state.as_str().into());
                    if let Some(n) = max_iterations(l) {
                        m.insert("max_iterations".into(), n.into());
                    }
                    let iterations = record
                        .result
                        .as_ref()
                        .map_or(record.iterations.len() as u32, |r| r.iterations);
                    m.insert("iterations".into(), iterations.into());
                    if let Some(result) = &record.result {
                        m.insert("outputs".into(), values_json(&result.outputs));
                    } else if record.state == SimStepState::Skipped {
                        let nulls: IndexMap<String, ExprValue> = l
                            .output_variables
                            .iter()
                            .map(|o| (o.name.clone(), ExprValue::Null))
                            .collect();
                        m.insert("outputs".into(), values_json(&nulls));
                    }
                    if let Some(reason) = &record.reason {
                        m.insert("reason".into(), reason.clone().into());
                    }
                    if record.supplied {
                        m.insert("inputs_supplied".into(), true.into());
                    }
                    // Each iteration's loop variable values, and its loop
                    // output values once its boundary evaluated them.
                    let history: Vec<serde_json::Value> = record
                        .iterations
                        .iter()
                        .map(|it| {
                            let mut h = serde_json::Map::new();
                            h.insert("iteration".into(), it.iteration.index.into());
                            h.insert("variables".into(), values_json(&it.values));
                            if let Some(outputs) = &it.outputs {
                                h.insert("outputs".into(), values_json(outputs));
                            }
                            h.into()
                        })
                        .collect();
                    if !history.is_empty() {
                        m.insert("iteration_values".into(), history.into());
                    }
                    out.push(m.into());
                    for it in &record.iterations {
                        for (j, step) in it.iteration.steps.iter().enumerate() {
                            let name = it.iteration.instance_name(j).unwrap_or_default();
                            let mut m = step_json(&name, step, &it.records[j]);
                            m.insert("loop".into(), l.name.clone().into());
                            m.insert("iteration".into(), it.iteration.index.into());
                            out.push(m.into());
                        }
                    }
                }
            }
        }
        out
    }

    /// The `job_outputs` map of `--output json|yaml`: each evaluated job
    /// output's value (`null` for no value).
    pub(super) fn job_outputs_json(&self) -> serde_json::Map<String, serde_json::Value> {
        self.job
            .outputs
            .iter()
            .flatten()
            .filter_map(|o| {
                let value = match o.evaluated.as_ref()? {
                    JobOutputValue::Value(v) => crate::common::expr_value_to_json(v),
                    JobOutputValue::Null => serde_json::Value::Null,
                };
                Some((o.declaration.name.clone(), value))
            })
            .collect()
    }

    /// The job outputs, as `name = value` lines, for the run's summary.
    pub(super) fn job_output_lines(&self) -> Vec<String> {
        self.job
            .outputs
            .iter()
            .flatten()
            .filter_map(|o| o.evaluated.as_ref().map(|v| job_output_line(o, v)))
            .collect()
    }

    /// Whether the supplied `--step-inputs` gave Step `idx`'s values.
    #[cfg(test)]
    fn supplied(&self, idx: usize) -> bool {
        self.records[idx].supplied
    }
}

fn waiting_on<T>(g: &GroupState<T>) -> Option<&Vec<String>> {
    match g {
        GroupState::Waiting { on } => Some(on),
        GroupState::Known(_) => None,
    }
}

/// `inputs from Step 'P' are incomplete because not all of its Tasks ran`.
fn incomplete(p: &str) -> String {
    format!("inputs from Step '{p}' are incomplete because not all of its Tasks ran")
}

/// Bound `step` with its `Steps.*` inputs from `inputs` attached, and how
/// to place its Actions' outputs.
fn prepare_bound(
    step: &Step,
    inputs: &StepInputs,
    explicit_tasks: bool,
) -> Result<(Step, Option<Placement>), RunError> {
    let step = if step.input_producers().is_empty() {
        step.clone()
    } else {
        openjd_model::attach_step_inputs(step, inputs).map_err(internal)?
    };
    let chunked = step.parameter_space.as_ref().is_some_and(|space| {
        space
            .task_parameter_definitions
            .values()
            .any(|p| matches!(p, openjd_model::job::TaskParameter::ChunkInt { .. }))
    });
    let placement = match &step.parameter_space {
        _ if step.output_variables.is_none() => None,
        Some(_) if explicit_tasks => Some(Placement::Explicit(Vec::new())),
        Some(space) if chunked => Some(Placement::Indexed(Box::new(ChunkTaskIndex::new(space)?))),
        _ => Some(Placement::Sequential(0)),
    };
    Ok((step, placement))
}

/// The result of advancing a pending Step or loop.
enum Advanced {
    /// A group failed: the Step or loop is failed.
    Failed,
    /// Its Tasks became known or it was skipped, which can make other
    /// Steps' inputs concrete.
    Opened,
    /// Anything else, including no change.
    Quiet,
}

/// `Job output <name> = <value> <unit>`, or `= null (no value)`.
fn job_output_line(output: &openjd_model::job::JobOutput, value: &JobOutputValue) -> String {
    let name = &output.declaration.name;
    match value {
        JobOutputValue::Value(v) => format!(
            "Job output {name} = {}",
            crate::common::with_unit(v, output.declaration.unit.as_deref())
        ),
        JobOutputValue::Null => format!("Job output {name} = null (no value)"),
    }
}

/// Loop `producer`'s outputs from `--step-inputs` values: each given in its
/// declared type, a single value under `LAST` (or `null`) and the whole list
/// under `APPEND`, checked as reported values are, each element of an
/// `APPEND` output separately. Every output of the loop must be given.
fn supplied_loop_outputs(
    producer: &Loop,
    given: &[(String, &serde_json::Value)],
) -> Result<IndexMap<String, ExprValue>, String> {
    let declared = &producer.output_variables;
    let missing: Vec<&str> = declared
        .iter()
        .filter(|o| !given.iter().any(|(n, _)| *n == o.name))
        .map(|o| o.name.as_str())
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "every output of the loop must be given; missing {}",
            missing
                .iter()
                .map(|n| format!("Steps.{}.Output.{n}", producer.name))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    // A LAST output given as null (after zero iterations of a while loop)
    // is not reported to the collector, which requires a LAST value.
    let nulls: Vec<&str> = given
        .iter()
        .filter(|(name, value)| {
            value.is_null()
                && producer
                    .output_variable(name)
                    .is_some_and(|o| o.collect != OutputCollectMode::Append)
        })
        .map(|(name, _)| name.as_str())
        .collect();
    let reported: Vec<openjd_model::job::OutputVariable> = declared
        .iter()
        .filter(|o| !nulls.contains(&o.name.as_str()))
        .cloned()
        .collect();
    let mut collector = openjd_model::outputs::OutputCollector::new(
        &reported,
        openjd_model::outputs::MIN_OUTPUT_BYTES_LIMIT,
        &[],
        openjd_expr::PathFormat::host(),
        None,
    )
    .map_err(|e| e.message().to_string())?;
    for (name, value) in given {
        if nulls.contains(&name.as_str()) {
            continue;
        }
        let where_ = format!("Steps.{}.Output.{name}", producer.name);
        let decl = producer
            .output_variable(name)
            .ok_or_else(|| format!("{where_}: the loop declares no such output"))?;
        match (decl.collect, value) {
            (OutputCollectMode::Append, serde_json::Value::Array(list)) => {
                for element in list {
                    collector
                        .accept(name, None, element.to_string().as_bytes(), false)
                        .map_err(|e| format!("{where_}: {}", e.message()))?;
                }
            }
            (OutputCollectMode::Append, _) => {
                return Err(format!(
                    "{where_}: an APPEND output must be given as a list"
                ))
            }
            (_, value) => {
                collector
                    .accept(name, None, value.to_string().as_bytes(), false)
                    .map_err(|e| format!("{where_}: {}", e.message()))?;
            }
        }
    }
    let mut finished = collector.finish().map_err(|e| e.message().to_string())?;
    let mut values = finished.swap_remove(&None).unwrap_or_default();
    let outputs = declared
        .iter()
        .map(|o| {
            let value = values.swap_remove(&o.name).unwrap_or(ExprValue::Null);
            (o.name.clone(), value)
        })
        .collect();
    Ok(outputs)
}

/// Each Task's outputs of `producer` from `--step-inputs` values, checked
/// as reported values are: a value per Task for a Step with a
/// `parameterSpace` (a JSON list with one element per Task), each element
/// of an `APPEND` output reported separately.
fn supplied_task_outputs(
    producer: &Step,
    given: &[(String, &serde_json::Value)],
) -> Result<Vec<TaskOutputs>, String> {
    let declared = producer.output_variables.as_deref().unwrap_or(&[]);
    let task_count = producer.tasks_known().ok_or("its Tasks are not known")? as usize;
    let multi = producer.parameter_space.is_some();
    let mut per_task: Vec<Vec<(&str, &serde_json::Value)>> = vec![Vec::new(); task_count];
    for (name, value) in given {
        if multi {
            let list = value
                .as_array()
                .filter(|l| l.len() == task_count)
                .ok_or_else(|| {
                    format!(
                    "Steps.{}.Output.{name} must be a list with one value per Task ({task_count})",
                    producer.name
                )
                })?;
            for (task, v) in per_task.iter_mut().zip(list) {
                task.push((name, v));
            }
        } else if let Some(task) = per_task.first_mut() {
            task.push((name, value));
        }
    }
    let mut result = Vec::with_capacity(task_count);
    for (t, values) in per_task.into_iter().enumerate() {
        let mut collector = openjd_model::outputs::OutputCollector::new(
            declared,
            openjd_model::outputs::MIN_OUTPUT_BYTES_LIMIT,
            &[],
            openjd_expr::PathFormat::host(),
            None,
        )
        .map_err(|e| e.message().to_string())?;
        let where_ = |name: &str| {
            if multi {
                format!("Steps.{}.Output.{name}[{t}]", producer.name)
            } else {
                format!("Steps.{}.Output.{name}", producer.name)
            }
        };
        for (name, value) in values {
            let append = declared
                .iter()
                .find(|v| v.name == name)
                .is_some_and(|v| v.collect == openjd_model::types::OutputCollectMode::Append);
            let elements: Vec<&serde_json::Value> = match (append, value.as_array()) {
                (true, Some(list)) => list.iter().collect(),
                (true, None) => {
                    return Err(format!(
                        "{}: an APPEND output must be given as a list",
                        where_(name)
                    ))
                }
                (false, _) => vec![value],
            };
            for element in elements {
                collector
                    .accept(name, None, element.to_string().as_bytes(), false)
                    .map_err(|e| format!("{}: {}", where_(name), e.message()))?;
            }
        }
        let mut finished = collector.finish().map_err(|e| e.message().to_string())?;
        result.push(finished.swap_remove(&None).unwrap_or_default());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn simulator(template: &str, steps_to_run: Option<&[usize]>) -> Simulator {
        let value = openjd_model::template::parse::document_string_to_object(
            template,
            openjd_model::template::parse::DocumentType::Yaml,
            &Default::default(),
        )
        .unwrap();
        let jt = openjd_model::template::parse::decode_job_template(
            value,
            Some(&["EXPR", "CONTROL_FLOW"]),
            &Default::default(),
        )
        .unwrap();
        let params = openjd_model::preprocess_job_parameters(
            &jt,
            &Default::default(),
            &[],
            &openjd_model::PathParameterOptions {
                job_template_dir: "/",
                current_working_dir: "/",
                path_format: openjd_expr::PathFormat::Posix,
                allow_template_dir_walk_up: false,
                allow_uri_path_values: true,
            },
        )
        .unwrap();
        let vctx = jt.default_validation_context();
        let job = openjd_model::create_job(&jt, &params, &vctx).unwrap();
        let to_run: Vec<Node> = match steps_to_run {
            Some(steps) => steps.iter().map(|&i| Node::Step(i)).collect(),
            None => (0..job.steps.len())
                .map(Node::Step)
                .chain((0..job.loops.len()).map(Node::Loop))
                .collect(),
        };
        Simulator::new(jt, params, vctx, job, &to_run).unwrap()
    }

    const FAN_OUT: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: J
jobOutputDefinitions:
- name: Frames
  type: LIST[INT]
  value: "{{Steps.C.TaskParam.F}}"
steps:
- name: P
  outputVariables:
  - {name: N, type: INT}
  script: {actions: {onRun: {command: x}}}
- name: C
  dependencies: [{dependsOn: P}]
  parameterSpace:
    taskParameterDefinitions:
    - {name: F, type: INT, range: "{{range(Steps.P.Output.N)}}"}
  script: {actions: {onRun: {command: x}}}
"#;

    fn outputs(pairs: &[(&str, openjd_expr::ExprValue)]) -> ActionOutputs {
        let mut task = TaskOutputs::new();
        for (k, v) in pairs {
            task.insert(k.to_string(), v.clone());
        }
        let mut result = ActionOutputs::new();
        result.insert(None, task);
        result
    }

    #[test]
    fn producer_success_binds_consumer_and_evaluates_job_output() {
        let mut sim = simulator(FAN_OUT, None);
        assert_eq!(sim.records[1].state, SimStepState::Pending);
        assert!(!sim.settle().unwrap());
        let Begin::Run(_) = sim.begin_step(0, false).unwrap() else {
            panic!("P runs")
        };
        sim.record_action(
            Slot::Top(0),
            None,
            Some(outputs(&[("N", openjd_expr::ExprValue::Int(3))])),
        );
        sim.finish_step(Slot::Top(0), false, true).unwrap();
        assert!(!sim.settle().unwrap());
        assert_eq!(sim.records[1].state, SimStepState::Bound);
        assert_eq!(sim.job.steps[1].tasks_known(), Some(3));
        // The job output reads only C's TaskParam, concrete once its Tasks
        // are known, before C runs.
        assert_eq!(
            sim.job_outputs_json()["Frames"],
            serde_json::json!([0, 1, 2])
        );
        assert!(sim.not_evaluated().is_empty());
    }

    /// A read Step whose Tasks are known has its TaskParam lists built only
    /// when something that reads it needs them.
    #[test]
    fn task_param_lists_are_built_only_when_needed() {
        let template = r#"
specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: J
steps:
- name: P
  parameterSpace:
    taskParameterDefinitions:
    - {name: I, type: INT, range: [1, 2]}
  outputVariables:
  - {name: V, type: INT}
  script: {actions: {onRun: {command: x}}}
- name: C
  dependencies: [{dependsOn: P}]
  script: {actions: {onRun: {command: "{{Steps.P.TaskParam.I}} {{Steps.P.Output.V}}"}}}
"#;
        let mut sim = simulator(template, None);
        assert!(sim.is_concrete("Steps.P.TaskParam.I"));
        assert!(!sim.is_concrete("Steps.P.Output.V"));
        assert_eq!(sim.inputs.get("Steps.P.TaskParam.I"), None);
        sim.ensure_for_step(1).unwrap();
        assert_eq!(
            sim.inputs
                .get("Steps.P.TaskParam.I")
                .map(|v| v.to_display_string()),
            Some("[1, 2]".to_string())
        );
    }

    #[test]
    fn failed_producer_blocks_its_consumer() {
        let mut sim = simulator(FAN_OUT, None);
        let Begin::Run(_) = sim.begin_step(0, false).unwrap() else {
            panic!("P runs")
        };
        sim.finish_step(Slot::Top(0), true, false).unwrap();
        let Begin::NotRun = sim.begin_step(1, false).unwrap() else {
            panic!("C is blocked")
        };
        assert_eq!(sim.records[1].state, SimStepState::Blocked);
        assert_eq!(
            sim.records[1].reason.as_deref(),
            Some("its dependency 'P' failed")
        );
        assert_eq!(
            sim.not_evaluated(),
            [(
                "Frames".to_string(),
                vec!["Step 'C', which is blocked".to_string()]
            )]
        );
    }

    #[test]
    fn incomplete_producer_blocks_the_consumer_that_reads_it() {
        let template = r#"
specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: J
steps:
- name: P
  parameterSpace:
    taskParameterDefinitions:
    - {name: I, type: INT, range: [1, 2]}
  outputVariables:
  - {name: N, type: INT}
  script: {actions: {onRun: {command: x}}}
- name: C
  dependencies: [{dependsOn: P}]
  script: {actions: {onRun: {command: "{{Steps.P.Output.N}}"}}}
"#;
        let mut sim = simulator(template, None);
        let Begin::Run(step) = sim.begin_step(0, false).unwrap() else {
            panic!("P runs")
        };
        let first =
            openjd_model::StepParameterSpaceIterator::new(step.parameter_space.as_ref().unwrap())
                .unwrap()
                .next()
                .unwrap();
        sim.record_action(
            Slot::Top(0),
            Some(&first),
            Some(outputs(&[("N", openjd_expr::ExprValue::Int(1))])),
        );
        sim.finish_step(Slot::Top(0), false, true).unwrap();
        assert!(!sim.records[0].complete());
        let Begin::NotRun = sim.begin_step(1, false).unwrap() else {
            panic!("C is blocked")
        };
        assert_eq!(
            sim.records[1].reason.as_deref(),
            Some("inputs from Step 'P' are incomplete because not all of its Tasks ran")
        );
    }

    #[test]
    fn step_inputs_bind_a_step_run_alone() {
        let mut sim = simulator(FAN_OUT, Some(&[1]));
        let err = sim
            .give_step_inputs(Node::Step(1), None)
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "Step 'C' reads Steps.* values of Steps this run does not run: Steps.P.Output.N.\nGive them with --step-inputs, or run Step 'C' with --run-dependencies."
        );
        let values: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"Steps.P.Output.N": "2"}"#).unwrap();
        sim.give_step_inputs(Node::Step(1), Some(&values)).unwrap();
        assert!(sim.supplied(0));
        assert!(!sim.settle().unwrap());
        assert_eq!(sim.job.steps[1].tasks_known(), Some(2));
    }

    #[test]
    fn step_inputs_are_checked_against_the_declared_type() {
        let mut sim = simulator(FAN_OUT, Some(&[1]));
        let values: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"Steps.P.Output.N": 1.5}"#).unwrap();
        let err = sim
            .give_step_inputs(Node::Step(1), Some(&values))
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "--step-inputs: Step 'P': Steps.P.Output.N: Output variable 'N': value (float) \
             '1.5' does not convert to int: Cannot coerce float to int: 1.5 is not a whole number"
        );
        for (json, want) in [
            (
                r#"{"Steps.Q.Output.N": 1}"#,
                "--step-inputs: the Job has no Step 'Q'.",
            ),
            (
                r#"{"Steps.P.Output.M": 1}"#,
                "--step-inputs: Step 'P' declares no output variable 'M'.",
            ),
            (
                r#"{"Steps.P.TaskParam.I": [1]}"#,
                "--step-inputs: the Tasks of Step 'P' are known at Job creation, so 'Steps.P.TaskParam.I' comes from its parameter space; remove it.",
            ),
            (
                r#"{"Steps.P.Out.N": 1}"#,
                "--step-inputs: 'Steps.P.Out.N' is not a Steps.<Step>.Output.<Name> symbol.",
            ),
        ] {
            let values: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(json).unwrap();
            let mut sim = simulator(FAN_OUT, Some(&[1]));
            assert_eq!(sim.give_step_inputs(Node::Step(1), Some(&values)).unwrap_err().to_string(), want);
        }
    }

    #[test]
    fn early_group_failure_fails_the_step() {
        let template = FAN_OUT.replace(
            "range: \"{{range(Steps.P.Output.N)}}\"",
            "range: \"{{range(Steps.P.Output.N) if Steps.P.Output.N > 0 else fail('none')}}\"",
        );
        let mut sim = simulator(&template, None);
        let Begin::Run(_) = sim.begin_step(0, false).unwrap() else {
            panic!("P runs")
        };
        sim.record_action(
            Slot::Top(0),
            None,
            Some(outputs(&[("N", openjd_expr::ExprValue::Int(0))])),
        );
        sim.finish_step(Slot::Top(0), false, true).unwrap();
        assert!(sim.settle().unwrap());
        assert_eq!(sim.records[1].state, SimStepState::Failed);
        let reason = sim.records[1].reason.clone().unwrap();
        assert_eq!(
            reason,
            "Step 'C' failed in its Task enumeration group:\n\
             Model validation error: 1 validation error for JobTemplate\n\
             steps[1] -> parameterSpace -> taskParameterDefinitions[0] -> range:\n\
             \tnone\n  \
             range(Steps.P.Output.N) if Steps.P.Output.N > 0 else fail('none')\n  \
             \x20                                                    ^~~~~~~~~~~~"
        );
        let Begin::NotRun = sim.begin_step(1, false).unwrap() else {
            panic!("a failed Step does not run")
        };
    }

    const LOOP: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: J
steps:
- name: L
  loop:
    variables:
    - {name: K, type: INT, initial: "{{2}}", next: "{{Loop.K - 1}}"}
    while: "{{Loop.K > 0}}"
    maxIterations: 5
    steps:
    - name: C
      dependencies: [{dependsOn: B}]
      script: {actions: {onRun: {command: x}}}
    - name: A
      script: {actions: {onRun: {command: x}}}
    - name: B
      dependencies: [{dependsOn: A}]
      script: {actions: {onRun: {command: x}}}
  outputVariables:
  - {name: Last, type: INT, collect: LAST, value: "{{Loop.K}}", minValue: 1}
  - {name: All, type: 'LIST[INT]', collect: APPEND, value: "{{Loop.K}}", maxLength: 2}
- name: D
  dependencies: [{dependsOn: L}]
  script: {actions: {onRun: {command: "{{Steps.L.Output.Last}}"}}}
"#;

    #[test]
    fn body_steps_run_in_dependency_order() {
        let sim = simulator(LOOP, None);
        // C depends on B, which depends on A.
        assert_eq!(sim.body_order(0).unwrap(), [1, 2, 0]);
        assert_eq!(sim.template_order, [Node::Loop(0), Node::Step(0)]);
        assert_eq!(sim.order, [Node::Loop(0), Node::Step(0)]);
    }

    #[test]
    fn instances_are_named_by_loop_iteration_and_body_step() {
        let mut sim = simulator(LOOP, None);
        let LoopBegin::Run = sim.begin_loop(0).unwrap() else {
            panic!("L runs")
        };
        assert_eq!(sim.loop_records[0].state, SimStepState::Running);
        assert!(sim.start_next_iteration(0).unwrap());
        assert_eq!(
            (0..3).map(|j| sim.instance_name(j)).collect::<Vec<_>>(),
            ["L[0].C", "L[0].A", "L[0].B"]
        );
    }

    #[test]
    fn supplied_loop_outputs_are_checked_against_their_declarations() {
        let sim = simulator(LOOP, None);
        let l = &sim.job.loops[0];
        let given = |json: &str| -> Result<IndexMap<String, ExprValue>, String> {
            let map: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(json).unwrap();
            let pairs: Vec<(String, &serde_json::Value)> =
                map.iter().map(|(k, v)| (k.clone(), v)).collect();
            supplied_loop_outputs(l, &pairs)
        };
        // A LAST output may be null, after zero iterations of a while loop.
        let outputs = given(r#"{"Last": null, "All": []}"#).unwrap();
        assert_eq!(outputs["Last"], ExprValue::Null);
        assert_eq!(outputs["All"].to_display_string(), "[]");
        let outputs = given(r#"{"Last": 3, "All": [1, 2]}"#).unwrap();
        assert_eq!(outputs["Last"], ExprValue::Int(3));
        assert_eq!(outputs["All"].to_display_string(), "[1, 2]");
        for (json, want) in [
            (
                r#"{"Last": 3}"#,
                "every output of the loop must be given; missing Steps.L.Output.All",
            ),
            (
                r#"{"Last": 0, "All": []}"#,
                "Steps.L.Output.Last: Output variable 'Last': value 0 is less than minimum 1",
            ),
            (
                r#"{"Last": 1, "All": [1, 2, 3]}"#,
                "Steps.L.Output.All: Output variable 'All': list length 3 exceeds maximum 2",
            ),
            (
                r#"{"Last": 1, "All": null}"#,
                "Steps.L.Output.All: an APPEND output must be given as a list",
            ),
        ] {
            assert_eq!(given(json).unwrap_err(), want, "{json}");
        }
    }

    #[test]
    fn iteration_outputs_are_the_last_value_and_the_appended_element() {
        let sim = simulator(LOOP, None);
        let mut so_far = IndexMap::new();
        so_far.insert("Last".to_string(), ExprValue::Int(1));
        so_far.insert(
            "All".to_string(),
            ExprValue::make_list(
                vec![ExprValue::Int(2), ExprValue::Int(1)],
                openjd_expr::ExprType::INT,
            )
            .unwrap(),
        );
        let values = iteration_outputs(&sim.job.loops[0], &so_far);
        assert_eq!(values["Last"], ExprValue::Int(1));
        assert_eq!(values["All"], ExprValue::Int(1));
    }

    #[test]
    fn a_dependency_of_a_loop_counts_as_read() {
        let sim = simulator(
            &LOOP
                .replace(
                    "- name: L\n  loop:",
                    "- name: L\n  dependencies: [{dependsOn: P}]\n  loop:",
                )
                .replace(
                    "steps:\n- name: L",
                    "steps:\n- name: P\n  script: {actions: {onRun: {command: x}}}\n- name: L",
                ),
            None,
        );
        assert!(sim.read.contains("P"));
        assert!(sim.read.contains("L"));
    }
}
