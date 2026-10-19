// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! The scheduler simulator of `openjd run` (`docs/control-flow-prototype-plan.md`
//! C1): the state of each Step and job output while a `CONTROL_FLOW` Job
//! runs.
//!
//! `create_job` returns each Step bound, skipped, or pending inputs
//! (RFC 0010 §Step input binding). The simulator keeps its own copy of the
//! Job and, whenever a Step's Tasks become known or it succeeds or is
//! skipped, advances every pending Step with `advance_step_binding` until
//! nothing changes, then evaluates every job output whose inputs have become
//! concrete. A group that fails stops the run, as a failed Task does. A Job
//! without `CONTROL_FLOW` has only bound Steps and no job outputs, so for it
//! the simulator only records what ran.

use std::collections::{BTreeMap, BTreeSet};

use openjd_model::job::{
    GroupReport, Job, JobOutputValue, ProducerState, Step, StepBinding, StepInputs,
};
use openjd_model::outputs::TaskOutputs;
use openjd_model::template::JobTemplate;
use openjd_model::types::{JobParameterValues, TaskParameterSet, ValidationContext};
use openjd_model::{ChunkTaskIndex, ModelError};
use openjd_sessions::action::ActionOutputs;

use super::RunError;

/// A Step's state in the simulator: the RFC's Step states (§Step states),
/// with `Bound` for `READY` and `Blocked` for a Step that can never run
/// because a dependency failed, is blocked, or has incomplete outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SimStepState {
    Pending,
    Bound,
    Skipped,
    Succeeded,
    Failed,
    Blocked,
}

impl SimStepState {
    /// The RFC's name for the state, for `--output json|yaml`.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "PENDING_INPUTS",
            Self::Bound => "READY",
            Self::Skipped => "SKIPPED",
            Self::Succeeded => "SUCCEEDED",
            Self::Failed => "FAILED",
            Self::Blocked => "BLOCKED",
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
    /// A chunked space, or Tasks given with `--tasks` or `--task-param`.
    Indexed(ChunkTaskIndex),
}

/// What the simulator knows of one Step.
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
    /// Why the Step failed or is blocked.
    pub(super) reason: Option<String>,
    /// Whether `--step-inputs` gave its outputs.
    supplied: bool,
}

impl StepRecord {
    /// Whether the Step succeeded with every one of its Tasks.
    fn complete(&self) -> bool {
        self.state == SimStepState::Succeeded && self.all_tasks_ran
    }
}

/// What starting a Step comes to (see [`Simulator::begin_step`]).
pub(super) enum Begin {
    /// Run its Tasks: the bound Step, with its `Steps.*` inputs attached.
    Run(Box<Step>),
    /// It is skipped, failed, or blocked, so nothing runs.
    NotRun,
    /// A group failed when it was advanced; the run stops.
    Failed,
}

/// The simulator's state for one run.
pub(super) struct Simulator {
    jt: JobTemplate,
    params: JobParameterValues,
    vctx: ValidationContext,
    /// The Job, with each Step's binding as advanced so far and each job
    /// output's value once evaluated.
    pub(super) job: Job,
    pub(super) records: Vec<StepRecord>,
    /// Every concrete `Steps.*` value so far, of the Steps something reads.
    inputs: StepInputs,
    read: BTreeSet<String>,
    /// Whether each Step is one this run runs.
    in_run: Vec<bool>,
    /// Every Step, in dependency order.
    order: Vec<usize>,
    /// Job outputs whose evaluation failed, so they are not retried.
    failed_outputs: BTreeSet<usize>,
    /// Whether the Job Template lists `CONTROL_FLOW`.
    control_flow: bool,
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

impl Simulator {
    /// The simulator for `job`, created from `jt` with `params` and `vctx`,
    /// for a run of the Steps `steps_to_run`.
    pub(super) fn new(
        jt: JobTemplate,
        params: JobParameterValues,
        vctx: ValidationContext,
        job: Job,
        steps_to_run: &[usize],
    ) -> Result<Self, RunError> {
        let order = openjd_model::StepDependencyGraph::new(&job)?.topo_sorted()?;
        let mut in_run = vec![false; job.steps.len()];
        for &idx in steps_to_run {
            in_run[idx] = true;
        }
        let records = job
            .steps
            .iter()
            .map(|step| StepRecord {
                state: match step.binding {
                    StepBinding::Bound => SimStepState::Bound,
                    StepBinding::PendingInputs(_) => SimStepState::Pending,
                    StepBinding::Skipped => SimStepState::Skipped,
                },
                task_outputs: BTreeMap::new(),
                placement: None,
                all_tasks_ran: false,
                reason: None,
                supplied: false,
            })
            .collect();
        let inputs = crate::common::creation_inputs(&job).map_err(internal)?;
        let read = crate::common::read_producers(&job);
        let control_flow = jt
            .profile()
            .has_extension(openjd_model::types::ModelExtension::ControlFlow);
        Ok(Self {
            control_flow,
            jt,
            params,
            vctx,
            job,
            records,
            inputs,
            read,
            in_run,
            order,
            failed_outputs: BTreeSet::new(),
        })
    }

    /// Print what `create_job` decided: the job outputs it evaluated, the
    /// Steps it skipped, and what is known of each pending Step.
    pub(super) fn report_initial(&self) {
        for output in self.job.outputs.iter().flatten() {
            if let Some(value) = &output.evaluated {
                say(&job_output_line(output, value));
            }
        }
        let in_run = self.job.steps.iter().zip(&self.in_run);
        for step in in_run.filter(|(_, run)| **run).map(|(step, _)| step) {
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
    }

    /// Give the outputs of the Steps the target Step reads, from
    /// `--step-inputs` (plan C1, Options): `values` maps
    /// `Steps.P.Output.V` to a value, which for a Step with a
    /// `parameterSpace` is a list with one value per Task. Each value is
    /// checked against its declared type and constraints as a reported
    /// value is (the `openjd_output_var` collector). Then check that every
    /// `Steps.*` value the target reads is concrete, so that a missing one
    /// is reported before the Session starts.
    pub(super) fn give_step_inputs(
        &mut self,
        target: usize,
        values: Option<&serde_json::Map<String, serde_json::Value>>,
    ) -> Result<(), RunError> {
        let target_name = self.job.steps[target].name.clone();
        let producers = self.job.steps[target].input_producers();
        let mut by_producer: std::collections::BTreeMap<String, Vec<(String, &serde_json::Value)>> =
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
            if !producers.iter().any(|q| q == p) {
                return Err(format!(
                    "--step-inputs: Step '{target_name}' does not read the Steps.* values of a Step '{p}'."
                )
                .into());
            }
            let pi = self
                .step_index(p)
                .ok_or_else(|| internal(format!("no Step '{p}'")))?;
            let producer = &self.job.steps[pi];
            if matches!(producer.binding, StepBinding::Skipped) {
                return Err(format!(
                    "--step-inputs: Step '{p}' is skipped by its enableIf, so its Steps.* values are null; remove '{key}'."
                )
                .into());
            }
            if *member == "TaskParam" {
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
                .any(|v| v.name == *name)
            {
                return Err(format!(
                    "--step-inputs: Step '{p}' declares no output variable '{name}'."
                )
                .into());
            }
            by_producer
                .entry(p.to_string())
                .or_default()
                .push((name.to_string(), value));
        }
        for (p, given) in by_producer {
            let pi = self.step_index(&p).expect("checked above");
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
            self.records[pi].supplied = true;
        }

        // Every Steps.* value the target reads must now be concrete. Both
        // calls are pure, so the run repeats them when it starts the Step.
        let missing_hint = format!(
            "Give them with --step-inputs, or run Step '{target_name}' with --run-dependencies."
        );
        let step = &self.job.steps[target];
        let step = match step.binding {
            StepBinding::PendingInputs(_) => match openjd_model::advance_step_binding(
                &self.jt,
                &self.params,
                &self.vctx,
                &self.job,
                target,
                &self.inputs,
                self.committed_task_count(target),
            ) {
                Ok(step) => step,
                Err(e) if e.is_caller_error() => {
                    return Err(format!(
                        "Step '{target_name}' cannot run alone: {e}\n{missing_hint}"
                    )
                    .into())
                }
                // A group that fails fails the Step when the run starts it.
                Err(_) => return Ok(()),
            },
            _ => step.clone(),
        };
        if let Some(report) = step.binding_report() {
            let mut waiting: Vec<String> = Vec::new();
            for group in [
                &report.enablement,
                &report.tasks,
                &report.host_requirements,
                &report.remaining,
            ] {
                if let GroupReport::Waiting { on, .. } = group {
                    waiting.extend(on.iter().cloned());
                }
            }
            waiting.sort();
            waiting.dedup();
            return Err(format!(
                "Step '{target_name}' reads Steps.* values of Steps this run does not run: {}.\n{missing_hint}",
                waiting.join(", ")
            )
            .into());
        }
        if step.binding.is_bound() && !step.input_producers().is_empty() {
            if let Err(e) = openjd_model::attach_step_inputs(&step, &self.inputs) {
                return Err(
                    format!("Step '{target_name}' cannot run alone: {e}\n{missing_hint}").into(),
                );
            }
        }
        Ok(())
    }

    /// Whether the Job Template lists `CONTROL_FLOW`.
    pub(super) fn control_flow(&self) -> bool {
        self.control_flow
    }

    fn step_index(&self, name: &str) -> Option<usize> {
        self.job.steps.iter().position(|s| s.name == name)
    }

    /// The Task count of every other Step whose Tasks are known, for the
    /// Task-count limit.
    fn committed_task_count(&self, idx: usize) -> u64 {
        self.job
            .steps
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != idx)
            .filter_map(|(_, s)| s.tasks_known())
            .sum()
    }

    /// Advance pending Step `idx` with the inputs known now, print each fact
    /// it gains, and record the result (see [`Advanced`]). A group that
    /// fails is printed and fails the Step.
    fn advance(&mut self, idx: usize) -> Result<Advanced, RunError> {
        let before = &self.job.steps[idx];
        let had_enabled = enablement_known(before);
        let had_tasks = before.tasks_known().is_some();
        let had_host = host_known(before);
        let committed = self.committed_task_count(idx);
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
        let name = step.name.clone();
        let mut opened = false;
        match step.binding {
            StepBinding::Skipped => {
                say(&format!("Step '{name}' skipped (enableIf is false)"));
                self.records[idx].state = SimStepState::Skipped;
                if self.read.contains(&name) {
                    self.inputs
                        .add_producer(&step, ProducerState::Skipped)
                        .map_err(internal)?;
                }
                opened = true;
            }
            _ => {
                if !had_enabled && enablement_known(&step) {
                    say(&format!("Step '{name}': enabled (enableIf is true)"));
                }
                if let (false, Some(n)) = (had_tasks, step.tasks_known()) {
                    say(&format!(
                        "Step '{name}': Tasks known ({n} {})",
                        tasks_noun(n)
                    ));
                    if self.read.contains(&name) {
                        self.inputs
                            .add_producer(&step, ProducerState::TasksKnown)
                            .map_err(internal)?;
                    }
                    opened = true;
                }
                if !had_host && host_known(&step) && has_host_requirements(&step) {
                    say(&format!("Step '{name}': host requirements known"));
                }
                if step.binding.is_bound() {
                    say(&format!("Step '{name}' is ready"));
                    self.records[idx].state = SimStepState::Bound;
                }
            }
        }
        self.job.steps[idx] = step;
        Ok(if opened {
            Advanced::Opened
        } else {
            Advanced::Quiet
        })
    }

    /// Advance every pending Step this run runs, in dependency order, until
    /// a pass changes nothing (a Step whose Tasks become known can make its
    /// consumers' `TaskParam` inputs concrete), then evaluate every job
    /// output whose inputs are concrete. Returns `true` when a Step's group
    /// or a job output failed, which stops the run.
    pub(super) fn settle(&mut self) -> Result<bool, RunError> {
        loop {
            let mut opened = false;
            for idx in self.order.clone() {
                if !self.in_run[idx] || self.records[idx].state != SimStepState::Pending {
                    continue;
                }
                match self.advance(idx)? {
                    Advanced::Failed => return Ok(true),
                    Advanced::Opened => opened = true,
                    Advanced::Quiet => {}
                }
            }
            if !opened {
                break;
            }
        }
        self.evaluate_job_outputs()
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
            if !symbols.iter().all(|s| self.inputs.is_concrete(s)) {
                continue;
            }
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

    /// Why Step `idx` can never run, when a dependency this run runs has
    /// failed or is blocked.
    fn blocked_reason(&self, idx: usize) -> Option<String> {
        for dep in self.job.steps[idx].dependencies.iter().flatten() {
            let j = self.step_index(&dep.depends_on)?;
            if !self.in_run[j] {
                continue;
            }
            match self.records[j].state {
                SimStepState::Failed => {
                    return Some(format!("its dependency '{}' failed", dep.depends_on))
                }
                SimStepState::Blocked => {
                    return Some(format!("its dependency '{}' is blocked", dep.depends_on))
                }
                _ => {}
            }
        }
        None
    }

    /// The Step this run runs whose outputs `idx` reads but that did not run
    /// all of its Tasks (`--maximum-tasks`, `--tasks`, `--task-param`).
    fn incomplete_producer(&self, idx: usize) -> Option<String> {
        self.job.steps[idx].input_producers().into_iter().find(|p| {
            self.step_index(p).is_some_and(|j| {
                self.in_run[j]
                    && self.records[j].state == SimStepState::Succeeded
                    && !self.records[j].complete()
            })
        })
    }

    /// At the end of a run that stopped early, mark every Step of the run
    /// that can never run because a dependency failed or is blocked, so its
    /// state and the job outputs waiting on it say so (RFC 0010 §Job outputs
    /// item 4). A Step the run did not reach for another reason (an
    /// interruption, a failed job output) keeps its state.
    pub(super) fn block_unreachable(&mut self) {
        for idx in self.order.clone() {
            let state = self.records[idx].state;
            if !self.in_run[idx]
                || matches!(
                    state,
                    SimStepState::Succeeded | SimStepState::Failed | SimStepState::Blocked
                )
            {
                continue;
            }
            if let Some(reason) = self.blocked_reason(idx) {
                self.block(idx, reason);
            }
        }
    }

    fn block(&mut self, idx: usize, reason: String) {
        say(&format!(
            "Step '{}' is blocked: {reason}",
            self.job.steps[idx].name
        ));
        let record = &mut self.records[idx];
        record.state = SimStepState::Blocked;
        record.reason = Some(reason);
    }

    /// Start Step `idx` of the run (plan C1 items 1 to 4): block it behind a
    /// failed or blocked dependency, advance it if it is still pending (its
    /// dependencies have finished, so it binds, is skipped, or fails), and
    /// for a bound Step attach its `Steps.*` inputs.
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
        if let Some(reason) = self.blocked_reason(idx) {
            self.block(idx, reason);
            return Ok(Begin::NotRun);
        }
        if self.records[idx].state == SimStepState::Pending {
            if let Advanced::Failed = self.advance(idx)? {
                return Ok(Begin::Failed);
            }
        }
        let name = self.job.steps[idx].name.clone();
        let incomplete = |p: String| {
            format!("inputs from Step '{p}' are incomplete because not all of its Tasks ran")
        };
        match self.records[idx].state {
            SimStepState::Skipped => {
                say(&format!("Step '{name}' is skipped; it runs no Tasks"));
                Ok(Begin::NotRun)
            }
            SimStepState::Pending => {
                if let Some(p) = self.incomplete_producer(idx) {
                    self.block(idx, incomplete(p));
                    return Ok(Begin::NotRun);
                }
                Err(internal(format!(
                    "Step '{name}' is still pending inputs after its dependencies finished: {}",
                    crate::common::binding_lines(&self.job.steps[idx]).join("; ")
                )))
            }
            SimStepState::Bound => {
                let step = &self.job.steps[idx];
                let step = if step.input_producers().is_empty() {
                    step.clone()
                } else {
                    match openjd_model::attach_step_inputs(step, &self.inputs) {
                        Ok(step) => step,
                        Err(e) => {
                            if let Some(p) = self.incomplete_producer(idx) {
                                self.block(idx, incomplete(p));
                                return Ok(Begin::NotRun);
                            }
                            return Err(internal(e));
                        }
                    }
                };
                let chunked = step.parameter_space.as_ref().is_some_and(|space| {
                    space
                        .task_parameter_definitions
                        .values()
                        .any(|p| matches!(p, openjd_model::job::TaskParameter::ChunkInt { .. }))
                });
                let placement = match &step.parameter_space {
                    _ if step.output_variables.is_none() => None,
                    Some(space) if chunked || explicit_tasks => {
                        Some(Placement::Indexed(ChunkTaskIndex::new(space)?))
                    }
                    _ => Some(Placement::Sequential(0)),
                };
                let record = &mut self.records[idx];
                record.task_outputs.clear();
                record.all_tasks_ran = false;
                record.placement = placement;
                Ok(Begin::Run(Box::new(step)))
            }
            other => Err(internal(format!(
                "Step '{name}' cannot start in state {}",
                other.as_str()
            ))),
        }
    }

    /// Record a successful Action of Step `idx` whose Task parameter values
    /// were `action` (`None` for a Step without a `parameterSpace`): each
    /// Task's `outputs`, placed at its per-Task index. Prints each output
    /// value with its unit (plan C1 item 4).
    pub(super) fn record_action(
        &mut self,
        idx: usize,
        action: Option<&TaskParameterSet>,
        outputs: Option<ActionOutputs>,
    ) {
        let step = &self.job.steps[idx];
        let record = &mut self.records[idx];
        let Some(outputs) = outputs else {
            return;
        };
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
                _ => None,
            };
            if let Some(slot) = slot {
                record.task_outputs.insert(slot, task_outputs);
            }
        }
    }

    /// Record that Step `idx` finished: failed, or succeeded. `ran_all` is
    /// whether every one of its Tasks ran (not so after `--maximum-tasks`,
    /// `--tasks`, or `--task-param`). A Step that succeeded with all of them
    /// gives its outputs to its consumers and to the job outputs.
    pub(super) fn finish_step(
        &mut self,
        idx: usize,
        failed: bool,
        ran_all: bool,
    ) -> Result<(), RunError> {
        let task_count = self.job.steps[idx].tasks_known().unwrap_or(0) as usize;
        let declares_outputs = self.job.steps[idx].output_variables.is_some();
        let record = &mut self.records[idx];
        if failed {
            record.state = SimStepState::Failed;
            record
                .reason
                .get_or_insert_with(|| "a Task failed".to_string());
            return Ok(());
        }
        record.state = SimStepState::Succeeded;
        // With output variables, count the Tasks whose outputs arrived, so a
        // Task given twice with `--tasks` does not stand in for another.
        record.all_tasks_ran = if declares_outputs {
            record.task_outputs.len() == task_count
        } else {
            ran_all
        };
        let step = &self.job.steps[idx];
        if self.read.contains(&step.name) && record.complete() {
            let task_outputs: Vec<TaskOutputs> = if declares_outputs {
                record.task_outputs.values().cloned().collect()
            } else {
                vec![TaskOutputs::new(); task_count]
            };
            self.inputs
                .add_producer(
                    step,
                    ProducerState::Succeeded {
                        task_outputs: &task_outputs,
                    },
                )
                .map_err(internal)?;
        }
        Ok(())
    }

    /// The job outputs not evaluated, each with the Steps it waits on and
    /// why each has not given its values; an output whose evaluation failed
    /// has none.
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
                .filter(|s| !self.inputs.is_concrete(s))
                .filter_map(|s| crate::common::step_of_symbol(s).map(str::to_string))
                .collect();
            steps.dedup();
            let waits = steps
                .into_iter()
                .map(|p| {
                    let why = self.step_index(&p).map_or("is unknown", |j| {
                        if !self.in_run[j] {
                            "this run does not run"
                        } else {
                            match self.records[j].state {
                                SimStepState::Failed => "failed",
                                SimStepState::Blocked => "is blocked",
                                SimStepState::Succeeded => "did not run all of its Tasks",
                                _ => "the run stopped before",
                            }
                        }
                    });
                    format!("Step '{p}', which {why}")
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

    /// The `steps` entries of `--output json|yaml`.
    pub(super) fn steps_json(&self) -> Vec<serde_json::Value> {
        self.job
            .steps
            .iter()
            .zip(&self.records)
            .map(|(step, record)| {
                let mut m = serde_json::Map::new();
                m.insert("name".into(), step.name.clone().into());
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
                if step.output_variables.is_some() && record.placement.is_some() {
                    // One entry per Task up to the last whose outputs
                    // arrived (`null` for a Task without them), so a run
                    // that stopped early lists only what ran; a complete
                    // Step has one per Task.
                    let len = if record.complete() {
                        record.task_outputs.len()
                    } else {
                        record
                            .task_outputs
                            .keys()
                            .next_back()
                            .map_or(0, |last| last + 1)
                    };
                    m.insert(
                        "outputs".into(),
                        (0..len)
                            .map(|i| match record.task_outputs.get(&i) {
                                Some(t) => serde_json::Value::Object(
                                    t.iter()
                                        .map(|(k, v)| {
                                            (k.clone(), crate::common::expr_value_to_json(v))
                                        })
                                        .collect(),
                                ),
                                None => serde_json::Value::Null,
                            })
                            .collect(),
                    );
                }
                serde_json::Value::Object(m)
            })
            .collect()
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

/// The result of advancing a pending Step.
enum Advanced {
    /// A group failed: the Step is failed.
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
                        "{}: an APPEND output is given as a list",
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
        let all: Vec<usize> = (0..job.steps.len()).collect();
        Simulator::new(jt, params, vctx, job, steps_to_run.unwrap_or(&all)).unwrap()
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
            0,
            None,
            Some(outputs(&[("N", openjd_expr::ExprValue::Int(3))])),
        );
        sim.finish_step(0, false, true).unwrap();
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

    #[test]
    fn failed_producer_blocks_its_consumer() {
        let mut sim = simulator(FAN_OUT, None);
        let Begin::Run(_) = sim.begin_step(0, false).unwrap() else {
            panic!("P runs")
        };
        sim.finish_step(0, true, false).unwrap();
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
            0,
            Some(&first),
            Some(outputs(&[("N", openjd_expr::ExprValue::Int(1))])),
        );
        sim.finish_step(0, false, true).unwrap();
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
        let err = sim.give_step_inputs(1, None).unwrap_err().to_string();
        assert_eq!(
            err,
            "Step 'C' reads Steps.* values of Steps this run does not run: Steps.P.Output.N.\nGive them with --step-inputs, or run Step 'C' with --run-dependencies."
        );
        let values: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"Steps.P.Output.N": "2"}"#).unwrap();
        sim.give_step_inputs(1, Some(&values)).unwrap();
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
            .give_step_inputs(1, Some(&values))
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
                "--step-inputs: Step 'C' does not read the Steps.* values of a Step 'Q'.",
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
            assert_eq!(sim.give_step_inputs(1, Some(&values)).unwrap_err().to_string(), want);
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
            0,
            None,
            Some(outputs(&[("N", openjd_expr::ExprValue::Int(0))])),
        );
        sim.finish_step(0, false, true).unwrap();
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
}
