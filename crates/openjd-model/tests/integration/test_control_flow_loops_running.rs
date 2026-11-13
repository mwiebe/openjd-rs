// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Running loops (`CONTROL_FLOW`, RFC 0013 §Running a loop): loop binding in
//! `create_job` and `advance_loop_binding`, iterations, and the iteration
//! boundary.
//!
//! The `conformance_*` tests mirror the `jobs/3.8.5--*` conformance tests of
//! openjd-specifications, with a [`run_loop`] driver in place of the CLI:
//! Task outputs come from a closure instead of a script.

use indexmap::IndexMap;
use openjd_expr::path_mapping::PathFormat;
use openjd_expr::{ExprType, ExprValue};
use openjd_model::job::PathInput;
use openjd_model::job::{
    BindingError, BoundaryOutcome, Job, LoopBinding, LoopCondition, LoopEnd, LoopExpression,
    LoopProducerState, LoopProgress, LoopResult, MaterializationGroup, ProducerState, StepBinding,
    StepInputs,
};
use openjd_model::outputs::TaskOutputs;
use openjd_model::step_dependency_graph::{StepDependencyGraph, StepNodeKind};
use openjd_model::{
    advance_body_step_binding, advance_loop_binding, advance_step_binding, attach_step_inputs,
    end_without_iterations, evaluate_iteration_boundary, evaluate_job_output, start_iteration,
    CallerLimits, StepParameterSpaceIterator,
};

use super::control_flow_binding_support::{expected, header, index, outputs, step, Fixture};

const SCRIPT: &str = "script:\n        actions:\n          onRun:\n            command: echo";

/// A template with `EXPR` and `CONTROL_FLOW` and these `steps` (and any
/// other top-level text before them in `extra`).
fn template(extra: &str, steps: &str) -> String {
    format!("{}{extra}steps:\n{steps}", header("[EXPR, CONTROL_FLOW]"))
}

/// A body Step `name` (indented for a loop body) with `rest` after its
/// script: `rest` is more `<StepTemplate>` lines at the same indentation.
fn body_step(name: &str, rest: &str) -> String {
    format!("    - name: {name}\n      {SCRIPT}\n{rest}")
}

fn ints(v: &[i64]) -> ExprValue {
    ExprValue::make_list(
        v.iter().map(|i| ExprValue::Int(*i)).collect(),
        ExprType::INT,
    )
    .unwrap()
}

// ── A loop driver ─────────────────────────────────────────────────

/// One Task of a body Step instance, given to the driver's closure.
struct TaskCall<'a> {
    /// `<Loop>[<N>].<Step>`.
    instance: String,
    /// The body Step's name.
    step: &'a str,
    iteration: u32,
    /// `Loop.*` for the iteration.
    values: &'a IndexMap<String, ExprValue>,
    /// The Task's parameter values, by name.
    params: IndexMap<String, ExprValue>,
    /// The instance's `resolved_symtab` after `attach_step_inputs`.
    symtab: openjd_expr::symbol_table::SymbolTable,
}

/// How a driven loop ended.
#[derive(Debug)]
enum Outcome {
    Succeeded(LoopResult),
    /// `start_iteration` or `advance_body_step_binding` failed.
    InstanceFailed(String),
    /// A body Task failed (the closure returned `Err`).
    TaskFailed(String),
    /// The iteration boundary or `end_without_iterations` failed.
    LoopFailed(String),
}

/// One iteration a driven loop started: its index, its `Loop.*` values, and
/// each instance's name with what happened to it (`ran N` Tasks, `skipped`).
type IterationRecord = (u32, IndexMap<String, ExprValue>, Vec<(String, String)>);

/// What a driven loop did.
#[derive(Debug)]
struct LoopRun {
    iterations: Vec<IterationRecord>,
    outcome: Outcome,
}

impl LoopRun {
    fn result(&self) -> &LoopResult {
        match &self.outcome {
            Outcome::Succeeded(r) => r,
            other => panic!("expected the loop to succeed, got {other:?}"),
        }
    }

    fn failure(&self) -> &str {
        match &self.outcome {
            Outcome::Succeeded(r) => panic!("expected the loop to fail, got {r:?}"),
            Outcome::InstanceFailed(m) | Outcome::TaskFailed(m) | Outcome::LoopFailed(m) => m,
        }
    }

    /// `<instance>: <what happened>` for every instance, in run order.
    fn trace(&self) -> Vec<String> {
        self.iterations
            .iter()
            .flat_map(|(_, _, instances)| instances.iter().map(|(n, s)| format!("{n}: {s}")))
            .collect()
    }
}

/// Run bound loop `loop_name` of `job` to its end as a scheduler would, with
/// `deps` holding its dependencies. Each Task of a body Step instance is
/// `task`, whose `Err` fails the Task (and so the loop).
fn run_loop(
    fx: &Fixture,
    job: &Job,
    loop_name: &str,
    deps: &StepInputs,
    mut task: impl FnMut(&TaskCall<'_>) -> Result<TaskOutputs, String>,
) -> LoopRun {
    let li = job.loops.iter().position(|l| l.name == loop_name).unwrap();
    let job_loop = &job.loops[li];
    let mut run = LoopRun {
        iterations: Vec::new(),
        outcome: Outcome::LoopFailed("not run".into()),
    };
    match &job_loop.binding {
        LoopBinding::Bound(b) if !b.has_iterations() => {
            run.outcome = match end_without_iterations(job, li) {
                Ok(r) => Outcome::Succeeded(r),
                Err(e) => Outcome::LoopFailed(e.to_string()),
            };
            return run;
        }
        LoopBinding::Bound(_) => {}
        other => panic!("loop {loop_name} is not bound: {other:?}"),
    }
    let mut progress = LoopProgress::new(job_loop).unwrap();
    loop {
        let mut it = match start_iteration(&fx.jt, &fx.params, &fx.ctx, job, li, &progress, deps, 0)
        {
            Ok(it) => it,
            Err(e) => {
                run.outcome = Outcome::InstanceFailed(e.to_string());
                return run;
            }
        };
        let n = it.steps.len();
        let mut done = vec![false; n];
        let mut trace = Vec::new();
        let mut inputs = deps.clone();
        while done.iter().any(|d| !d) {
            let mut moved = false;
            for j in 0..n {
                if done[j] {
                    continue;
                }
                let name = it.instance_name(j).unwrap();
                let st = it.steps[j].clone();
                match &st.binding {
                    StepBinding::PendingInputs(_) => {
                        match advance_body_step_binding(
                            &fx.jt, &fx.params, &fx.ctx, job, li, &progress, &it, j, &inputs, 0,
                        ) {
                            Ok(s) => {
                                if s != st {
                                    moved = true;
                                }
                                it.steps[j] = s;
                            }
                            Err(e) => {
                                run.iterations
                                    .push((it.index, progress.values.clone(), trace));
                                run.outcome = Outcome::InstanceFailed(e.to_string());
                                return run;
                            }
                        }
                    }
                    StepBinding::Skipped => {
                        inputs.add_producer(&st, ProducerState::Skipped).unwrap();
                        trace.push((name, "skipped".to_string()));
                        done[j] = true;
                        moved = true;
                    }
                    StepBinding::Bound => {
                        let ready = st.dependencies.iter().flatten().all(|d| {
                            it.steps
                                .iter()
                                .position(|s| s.name == d.depends_on)
                                .is_some_and(|k| done[k])
                        });
                        if !ready {
                            continue;
                        }
                        let attached = attach_step_inputs(&st, &inputs).unwrap();
                        let symtab = attached
                            .resolved_symtab
                            .as_ref()
                            .map(|s| s.to_symtab(PathFormat::Posix).unwrap())
                            .unwrap_or_default();
                        let sets: Vec<IndexMap<String, ExprValue>> = match &st.parameter_space {
                            None => vec![IndexMap::new()],
                            Some(ps) => {
                                StepParameterSpaceIterator::new_with_chunk_override(ps, Some(1))
                                    .unwrap()
                                    .map(|set| {
                                        set.iter()
                                            .map(|(k, v)| (k.clone(), v.value.clone()))
                                            .collect()
                                    })
                                    .collect()
                            }
                        };
                        let mut task_outputs = Vec::new();
                        for params in sets {
                            let call = TaskCall {
                                instance: name.clone(),
                                step: &st.name,
                                iteration: it.index,
                                values: &progress.values,
                                params,
                                symtab: symtab.clone(),
                            };
                            match task(&call) {
                                Ok(o) => task_outputs.push(o),
                                Err(m) => {
                                    trace.push((name.clone(), format!("failed: {m}")));
                                    run.iterations
                                        .push((it.index, progress.values.clone(), trace));
                                    run.outcome = Outcome::TaskFailed(name);
                                    return run;
                                }
                            }
                        }
                        trace.push((name, format!("ran {}", task_outputs.len())));
                        inputs
                            .add_producer(
                                &st,
                                ProducerState::Succeeded {
                                    task_outputs: &task_outputs,
                                },
                            )
                            .unwrap();
                        done[j] = true;
                        moved = true;
                    }
                }
            }
            assert!(moved, "the iteration is stuck: {:?}", it.steps);
        }
        run.iterations
            .push((it.index, progress.values.clone(), trace));
        match evaluate_iteration_boundary(&fx.jt, &fx.params, &fx.ctx, job, li, &progress, &inputs)
        {
            Ok(BoundaryOutcome::Continue(next)) => {
                assert_eq!(next.iteration, progress.iteration + 1);
                progress = next;
            }
            Ok(BoundaryOutcome::Succeeded(r)) => {
                run.outcome = Outcome::Succeeded(r);
                return run;
            }
            Err(e) => {
                run.outcome = Outcome::LoopFailed(e.to_string());
                return run;
            }
        }
    }
}

/// A Task that outputs nothing.
fn nothing(_: &TaskCall<'_>) -> Result<TaskOutputs, String> {
    Ok(TaskOutputs::new())
}

/// The value of `name` in a loop result.
fn out<'a>(r: &'a LoopResult, name: &str) -> &'a ExprValue {
    &r.outputs[name]
}

/// `inputs` with loop `name` of `job` added as succeeded with `r`.
fn with_loop(job: &Job, name: &str, r: &LoopResult) -> StepInputs {
    let mut inputs = StepInputs::new();
    let l = job.loops.iter().find(|l| l.name == name).unwrap();
    inputs
        .add_loop_producer(
            l,
            LoopProducerState::Succeeded {
                outputs: &r.outputs,
            },
        )
        .unwrap();
    inputs
}

/// The iteration indices that ran, one per iteration.
fn iterations(run: &LoopRun) -> Vec<u32> {
    run.iterations.iter().map(|(n, _, _)| *n).collect()
}

/// A loop `L` with `definition` (the lines under `loop:`, before `steps:`),
/// one body Step `W`, and `after` (more `<StepLoop>` lines, such as
/// `outputVariables`), followed by `rest` (more top-level entries).
fn single_loop(definition: &str, after: &str, rest: &str) -> String {
    format!(
        "- name: L\n  loop:\n{definition}    steps:\n{}{after}{rest}",
        body_step("W", "")
    )
}

// ── Conformance: jobs/3.8.5--* ────────────────────────────────────

#[test]
fn conformance_until_runs_until_true() {
    let fx = Fixture::new(
        &template(
            "",
            "- name: L
  loop:
    until: 'Steps.Count.Output.V >= 20'
    maxIterations: 10
    steps:
    - name: Count
      outputVariables:
      - {name: V, type: INT}
      script:
        actions:
          onRun:
            command: echo
  outputVariables:
  - {name: History, type: 'LIST[INT]', collect: APPEND, value: '{{Steps.Count.Output.V}}'}
  - {name: Final, type: INT, collect: LAST, value: '{{Steps.Count.Output.V}}'}
  - {name: Index, type: INT, collect: LAST, value: '{{Loop.Iteration}}'}
",
        ),
        &[],
    );
    let job = fx.job();
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), |t| {
        Ok(outputs(&[(
            "V",
            ExprValue::Int(i64::from(t.iteration) * 10),
        )]))
    });
    assert_eq!(iterations(&run), [0, 1, 2]);
    let r = run.result();
    assert_eq!(r.iterations, 3);
    assert_eq!(r.ended_by, LoopEnd::Until);
    assert_eq!(out(r, "History"), &ints(&[0, 10, 20]));
    assert_eq!(out(r, "Final"), &ExprValue::Int(20));
    assert_eq!(out(r, "Index"), &ExprValue::Int(2));
}

#[test]
fn conformance_while_checked_before_each_iteration() {
    let fx = Fixture::new(
        &template(
            "",
            &single_loop(
                "    variables:
    - {name: 'N', type: INT, initial: '{{0}}', next: '{{Loop.N + 1}}'}
    while: 'Loop.N < 3'
    maxIterations: 5
",
                "  outputVariables:
  - {name: Ns, type: 'LIST[INT]', collect: APPEND, value: '{{Loop.N}}'}
  - {name: Last, type: INT, collect: LAST, value: '{{Loop.Iteration}}'}
",
                "",
            ),
        ),
        &[],
    );
    let job = fx.job();
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), nothing);
    assert_eq!(iterations(&run), [0, 1, 2]);
    let values: Vec<&ExprValue> = run.iterations.iter().map(|(_, v, _)| &v["N"]).collect();
    assert_eq!(
        values,
        [&ExprValue::Int(0), &ExprValue::Int(1), &ExprValue::Int(2)]
    );
    let r = run.result();
    assert_eq!(r.ended_by, LoopEnd::While);
    assert_eq!(out(r, "Ns"), &ints(&[0, 1, 2]));
    assert_eq!(out(r, "Last"), &ExprValue::Int(2));
}

#[test]
fn conformance_while_zero_iterations() {
    let fx = Fixture::new(
        &template(
            "",
            &single_loop(
                "    variables:
    - {name: 'N', type: INT, initial: '{{0}}', next: '{{Loop.N - 1}}'}
    while: 'Loop.N > 0'
    maxIterations: 5
",
                "  outputVariables:
  - {name: Last, type: INT, collect: LAST, value: '{{Loop.N}}'}
  - {name: Hist, type: 'LIST[INT]', collect: APPEND, value: '{{Loop.N}}'}
",
                "- name: C
  dependencies:
  - dependsOn: L
  script:
    actions:
      onRun:
        command: echo
        args: ['{{Steps.L.Output.Last}}', '{{repr_json(Steps.L.Output.Hist)}}']
",
            ),
        ),
        &[],
    );
    let job = fx.job();
    let LoopBinding::Bound(bound) = &job.loops[0].binding else {
        panic!("{:?}", job.loops[0].binding)
    };
    assert!(!bound.has_iterations());
    assert_eq!(bound.start.first_while, Some(false));
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), nothing);
    assert!(run.iterations.is_empty());
    let r = run.result();
    assert_eq!(r.iterations, 0);
    assert_eq!(r.ended_by, LoopEnd::ZeroIterations);
    assert_eq!(out(r, "Last"), &ExprValue::Null);
    assert_eq!(out(r, "Hist"), &ints(&[]));
    // C reads `null` and `[]`.
    let inputs = with_loop(&job, "L", r);
    let c = attach_step_inputs(step(&job, "C"), &inputs).unwrap();
    let st = c
        .resolved_symtab
        .unwrap()
        .to_symtab(PathFormat::Posix)
        .unwrap();
    assert_eq!(st.get_value("Steps.L.Output.Last"), Some(&ExprValue::Null));
    assert_eq!(st.get_value("Steps.L.Output.Hist"), Some(&ints(&[])));
}

#[test]
fn conformance_while_zero_iterations_append_min_length_fails() {
    let fx = Fixture::new(
        &template(
            "",
            &single_loop(
                "    variables:
    - {name: 'N', type: INT, initial: '{{0}}', next: '{{Loop.N - 1}}'}
    while: 'Loop.N > 0'
    maxIterations: 5
",
                "  outputVariables:
  - {name: Hist, type: 'LIST[INT]', collect: APPEND, value: '{{Loop.N}}', minLength: 1}
",
                "",
            ),
        ),
        &[],
    );
    // minLength is never part of Job creation.
    let job = fx.job();
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), nothing);
    assert_eq!(
        run.failure(),
        format!(
            "Loop 'L' failed after zero iterations, in the minLength of loop output 'Hist':\n{}",
            expected(&[(
                "steps[0] -> outputVariables[0] -> minLength",
                "Loop output 'Hist': list length 0 is less than minimum 1"
            )])
        )
    );
}

#[test]
fn conformance_skipped_loop_outputs_null() {
    let fx = Fixture::new(
        &template(
            "jobOutputDefinitions:\n- {name: J, type: INT, value: '{{Steps.L.Output.Last}}'}\n",
            "- name: L
  enableIf: false
  loop:
    until: 'true'
    maxIterations: 5
    steps:
    - name: B
      script:
        actions:
          onRun:
            command: echo
  outputVariables:
  - {name: Last, type: INT, collect: LAST, value: '{{1}}'}
  - {name: Hist, type: 'LIST[INT]', collect: APPEND, value: '{{1}}'}
- name: C
  dependencies:
  - dependsOn: L
  script:
    actions:
      onRun:
        command: echo
        args: ['{{Steps.L.Output.Last}}', '{{Steps.L.Output.Hist}}']
",
        ),
        &[],
    );
    let job = fx.job();
    assert!(matches!(job.loops[0].binding, LoopBinding::Skipped));
    assert_eq!(job.loops[0].tasks_known(), Some(0));
    // The job output reads the skipped loop's null at creation.
    let jo = job.outputs.as_ref().unwrap();
    assert_eq!(
        jo[0].evaluated,
        Some(openjd_model::job::JobOutputValue::Null)
    );
    let mut inputs = StepInputs::new();
    inputs
        .add_loop_producer(&job.loops[0], LoopProducerState::Skipped)
        .unwrap();
    let c = attach_step_inputs(step(&job, "C"), &inputs).unwrap();
    let st = c
        .resolved_symtab
        .unwrap()
        .to_symtab(PathFormat::Posix)
        .unwrap();
    assert_eq!(st.get_value("Steps.L.Output.Last"), Some(&ExprValue::Null));
    assert_eq!(st.get_value("Steps.L.Output.Hist"), Some(&ExprValue::Null));
}

#[test]
fn conformance_multi_step_body_check_drives_next() {
    let fx = Fixture::new(
        &template(
            "",
            &format!(
                "- name: L
  loop:
    variables:
    - {{name: Value, type: INT, initial: '{{{{1}}}}', next: '{{{{Steps.Check.Output.Score}}}}'}}
    until: 'Steps.Check.Output.Score > 10'
    maxIterations: 10
    steps:
{}{}  outputVariables:
  - {{name: Values, type: 'LIST[INT]', collect: APPEND, value: '{{{{Loop.Value}}}}'}}
  - {{name: FinalScore, type: INT, collect: LAST, value: '{{{{Steps.Check.Output.Score}}}}'}}
",
                body_step("Render", ""),
                body_step(
                    "Check",
                    "      dependencies:\n      - dependsOn: Render\n      outputVariables:\n      - {name: Score, type: INT}\n"
                )
            ),
        ),
        &[],
    );
    let job = fx.job();
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), |t| match t.step {
        "Check" => {
            let ExprValue::Int(v) = t.values["Value"] else {
                panic!()
            };
            Ok(outputs(&[("Score", ExprValue::Int(v * 2))]))
        }
        _ => Ok(TaskOutputs::new()),
    });
    assert_eq!(
        run.trace(),
        [
            "L[0].Render: ran 1",
            "L[0].Check: ran 1",
            "L[1].Render: ran 1",
            "L[1].Check: ran 1",
            "L[2].Render: ran 1",
            "L[2].Check: ran 1",
            "L[3].Render: ran 1",
            "L[3].Check: ran 1",
        ]
    );
    let r = run.result();
    assert_eq!(out(r, "Values"), &ints(&[1, 2, 4, 8]));
    assert_eq!(out(r, "FinalScore"), &ExprValue::Int(16));
}

#[test]
fn conformance_next_reads_previous_values() {
    let fx = Fixture::new(
        &template(
            "",
            &single_loop(
                "    variables:
    - {name: A, type: INT, initial: '{{1}}', next: '{{Loop.B}}'}
    - {name: B, type: INT, initial: '{{2}}', next: '{{Loop.A}}'}
    until: 'Loop.Iteration >= 2'
    maxIterations: 5
",
                "  outputVariables:
  - {name: Pairs, type: 'LIST[INT]', collect: APPEND, value: '{{Loop.A * 10 + Loop.B}}'}
",
                "",
            ),
        ),
        &[],
    );
    let job = fx.job();
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), nothing);
    assert_eq!(out(run.result(), "Pairs"), &ints(&[12, 21, 12]));
}

#[test]
fn conformance_until_true_skips_next() {
    let fx = Fixture::new(
        &template(
            "",
            &format!(
                "- name: L
  loop:
    variables:
    - {{name: 'N', type: INT, initial: '{{{{0}}}}', next: \"{{{{fail('next evaluated') if Steps.W.Output.Done else Loop.N + 1}}}}\"}}
    until: 'Steps.W.Output.Done'
    maxIterations: 5
    steps:
{}  outputVariables:
  - {{name: 'N', type: INT, collect: LAST, value: '{{{{Loop.N}}}}'}}
",
                body_step("W", "      outputVariables:\n      - {name: Done, type: BOOL}\n")
            ),
        ),
        &[],
    );
    let job = fx.job();
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), |t| {
        Ok(outputs(&[("Done", ExprValue::Bool(t.iteration == 1))]))
    });
    assert_eq!(iterations(&run), [0, 1]);
    assert_eq!(out(run.result(), "N"), &ExprValue::Int(1));
}

#[test]
fn conformance_body_step_skipped_in_iteration() {
    let fx = Fixture::new(
        &template(
            "",
            &format!(
                "- name: L
  loop:
    until: 'Loop.Iteration >= 2'
    maxIterations: 10
    steps:
{}  outputVariables:
  - {{name: Xs, type: 'LIST[INT]', collect: APPEND, value: '{{{{Steps.S.Output.X or -1}}}}'}}
",
                body_step(
                    "S",
                    "      enableIf: 'Loop.Iteration == 1'\n      outputVariables:\n      - {name: X, type: INT}\n"
                )
            ),
        ),
        &[],
    );
    let job = fx.job();
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), |_| {
        Ok(outputs(&[("X", ExprValue::Int(7))]))
    });
    assert_eq!(
        run.trace(),
        ["L[0].S: skipped", "L[1].S: ran 1", "L[2].S: skipped"]
    );
    assert_eq!(out(run.result(), "Xs"), &ints(&[-1, 7, -1]));
}

#[test]
fn conformance_body_fan_out_from_loop_variable() {
    let fx = Fixture::new(
        &template(
            "",
            &format!(
                "- name: L
  loop:
    variables:
    - {{name: Frames, type: 'LIST[INT]', initial: '{{{{[1, 2, 3]}}}}', next: '{{{{Steps.K.Output.Rest}}}}', minLength: 1}}
    until: 'len(Steps.K.Output.Rest) == 0'
    maxIterations: 10
    steps:
{}{}  outputVariables:
  - {{name: Counts, type: 'LIST[INT]', collect: APPEND, value: '{{{{len(Loop.Frames)}}}}'}}
",
                body_step(
                    "R",
                    "      parameterSpace:\n        taskParameterDefinitions:\n        - {name: F, type: INT, range: '{{Loop.Frames}}'}\n"
                ),
                body_step(
                    "K",
                    "      dependencies:\n      - dependsOn: R\n      outputVariables:\n      - {name: Rest, type: 'LIST[INT]'}\n"
                )
            ),
        ),
        &[],
    );
    let job = fx.job();
    let mut frames = Vec::new();
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), |t| match t.step {
        "R" => {
            frames.push(format!(
                "F{}_{}",
                t.iteration,
                t.params["F"].to_display_string()
            ));
            Ok(TaskOutputs::new())
        }
        _ => {
            let rest = t.values["Frames"].list_elements().unwrap()[1..].to_vec();
            Ok(outputs(&[(
                "Rest",
                ExprValue::make_list(rest, ExprType::INT).unwrap(),
            )]))
        }
    });
    assert_eq!(frames, ["F0_1", "F0_2", "F0_3", "F1_2", "F1_3", "F2_3"]);
    assert_eq!(out(run.result(), "Counts"), &ints(&[3, 2, 1]));
}

#[test]
fn conformance_body_reads_loop_dependency() {
    let fx = Fixture::new(
        &template(
            "",
            &format!(
                "- name: Setup
  outputVariables:
  - {{name: Base, type: INT}}
  {SCRIPT}
- name: L
  dependencies:
  - dependsOn: Setup
  loop:
    until: 'Loop.Iteration >= 1'
    maxIterations: 5
    steps:
    - name: W
      script:
        actions:
          onRun:
            command: echo
            args: ['{{{{Steps.Setup.Output.Base + Loop.Iteration}}}}']
"
            ),
        ),
        &[],
    );
    let job = fx.job();
    // The loop's own groups read nothing of Setup, so it is bound already.
    assert!(matches!(job.loops[0].binding, LoopBinding::Bound(_)));
    let mut deps = StepInputs::new();
    super::control_flow_binding_support::succeeded(
        &mut deps,
        &job,
        "Setup",
        &[outputs(&[("Base", ExprValue::Int(5))])],
    );
    let mut seen = Vec::new();
    let run = run_loop(&fx, &job, "L", &deps, |t| {
        seen.push((
            t.instance.clone(),
            t.symtab.get_value("Steps.Setup.Output.Base").cloned(),
            t.symtab.get_value("Loop.Iteration").cloned(),
        ));
        Ok(TaskOutputs::new())
    });
    run.result();
    assert_eq!(
        seen,
        [
            (
                "L[0].W".to_string(),
                Some(ExprValue::Int(5)),
                Some(ExprValue::Int(0))
            ),
            (
                "L[1].W".to_string(),
                Some(ExprValue::Int(5)),
                Some(ExprValue::Int(1))
            ),
        ]
    );
}

#[test]
fn conformance_consumer_fans_out_over_append_output() {
    let fx = Fixture::new(
        &template(
            "",
            &format!(
                "- name: L
  loop:
    until: 'Loop.Iteration >= 2'
    maxIterations: 10
    steps:
{}  outputVariables:
  - {{name: H, type: 'LIST[INT]', collect: APPEND, value: '{{{{Steps.Count.Output.V}}}}'}}
- name: C
  dependencies:
  - dependsOn: L
  parameterSpace:
    taskParameterDefinitions:
    - {{name: X, type: INT, range: '{{{{Steps.L.Output.H}}}}'}}
  {SCRIPT}
",
                body_step(
                    "Count",
                    "      outputVariables:\n      - {name: V, type: INT}\n"
                )
            ),
        ),
        &[],
    );
    let mut job = fx.job();
    assert!(matches!(
        step(&job, "C").binding,
        StepBinding::PendingInputs(_)
    ));
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), |t| {
        Ok(outputs(&[(
            "V",
            ExprValue::Int(i64::from(t.iteration) * 10),
        )]))
    });
    let r = run.result().clone();
    let c = fx
        .advance(&mut job, "C", |job| with_loop(job, "L", &r), 0)
        .unwrap();
    assert_eq!(c.tasks_known(), Some(3));
    let space = c.parameter_space.as_ref().unwrap();
    let xs: Vec<String> = StepParameterSpaceIterator::new(space)
        .unwrap()
        .map(|set| set["X"].value.to_display_string())
        .collect();
    assert_eq!(xs, ["0", "10", "20"]);
}

#[test]
fn conformance_error_in_next_fails_loop() {
    let fx = Fixture::new(
        &template(
            "",
            &format!(
                "- name: L
  loop:
    variables:
    - {{name: 'N', type: INT, initial: '{{{{0}}}}', next: \"{{{{fail('stop') if Steps.W.Output.V == 1 else Loop.N + 1}}}}\"}}
    until: 'Loop.Iteration >= 4'
    maxIterations: 10
    steps:
{}",
                body_step("W", "      outputVariables:\n      - {name: V, type: INT}\n")
            ),
        ),
        &[],
    );
    let job = fx.job();
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), |t| {
        Ok(outputs(&[("V", ExprValue::Int(i64::from(t.iteration)))]))
    });
    assert_eq!(iterations(&run), [0, 1]);
    assert_eq!(
        run.failure(),
        format!(
            "Loop 'L' failed at the boundary of iteration 1, in the next of loop variable 'N':\n{}",
            expected(&[(
                "steps[0] -> loop -> variables[0] -> next",
                "Loop variable 'N': stop\n  fail('stop') if Steps.W.Output.V == 1 else Loop.N + 1\n  ^~~~~~~~~~~~"
            )])
        )
    );
}

#[test]
fn conformance_error_in_output_value_fails_loop() {
    let fx = Fixture::new(
        &template(
            "",
            &single_loop(
                "    until: 'Loop.Iteration >= 4'\n    maxIterations: 10\n",
                "  outputVariables:
  - {name: X, type: 'LIST[INT]', collect: APPEND, value: \"{{fail('bad value') if Loop.Iteration == 1 else Loop.Iteration}}\"}
",
                "",
            ),
        ),
        &[],
    );
    let job = fx.job();
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), nothing);
    assert_eq!(iterations(&run), [0, 1]);
    assert_eq!(
        run.failure(),
        format!(
            "Loop 'L' failed at the boundary of iteration 1, in the value of loop output 'X':\n{}",
            expected(&[(
                "steps[0] -> outputVariables[0] -> value",
                "Loop output 'X': bad value\n  fail('bad value') if Loop.Iteration == 1 else Loop.Iteration\n  ^~~~~~~~~~~~~~~~~"
            )])
        )
    );
}

#[test]
fn conformance_loop_variable_constraint_fails() {
    let fx = Fixture::new(
        &template(
            "",
            &single_loop(
                "    variables:
    - {name: 'N', type: INT, initial: '{{0}}', next: '{{Loop.N + 1}}', maxValue: 2}
    until: 'Loop.Iteration >= 4'
    maxIterations: 10
",
                "",
                "",
            ),
        ),
        &[],
    );
    let job = fx.job();
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), nothing);
    assert_eq!(iterations(&run), [0, 1, 2]);
    assert_eq!(
        run.failure(),
        format!(
            "Loop 'L' failed at the boundary of iteration 2, in the next of loop variable 'N':\n{}",
            expected(&[(
                "steps[0] -> loop -> variables[0] -> next",
                "Loop variable 'N': value 3 exceeds maximum 2\n  {{Loop.N + 1}}"
            )])
        )
    );
}

#[test]
fn conformance_next_constraint_at_final_while_boundary_fails() {
    let fx = Fixture::new(
        &template(
            "",
            &single_loop(
                "    variables:
    - {name: 'N', type: INT, initial: '{{0}}', next: '{{Loop.N + 1}}', maxValue: 2}
    while: 'Loop.Iteration < 3'
    maxIterations: 10
",
                "",
                "",
            ),
        ),
        &[],
    );
    let job = fx.job();
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), nothing);
    assert_eq!(iterations(&run), [0, 1, 2]);
    assert_eq!(
        run.failure(),
        format!(
            "Loop 'L' failed at the boundary of iteration 2, in the next of loop variable 'N':\n{}",
            expected(&[(
                "steps[0] -> loop -> variables[0] -> next",
                "Loop variable 'N': value 3 exceeds maximum 2\n  {{Loop.N + 1}}"
            )])
        )
    );
}

#[test]
fn conformance_max_iterations_exceeded_fails() {
    let fx = Fixture::new(
        &template(
            "",
            &single_loop(
                "    until: 'Loop.Iteration >= 4'\n    maxIterations: 3\n",
                "",
                "",
            ),
        ),
        &[],
    );
    let job = fx.job();
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), nothing);
    assert_eq!(iterations(&run), [0, 1, 2]);
    assert_eq!(
        run.failure(),
        format!(
            "Loop 'L' failed at the boundary of iteration 2, in maxIterations:\n{}",
            expected(&[(
                "steps[0] -> loop -> maxIterations",
                "Loop 'L' has run maxIterations (3) iterations without ending, so it fails."
            )])
        )
    );
}

#[test]
fn conformance_max_iterations_from_param() {
    let tpl = template(
        "parameterDefinitions:\n- {name: Max, type: INT}\n",
        &single_loop(
            "    until: 'Loop.Iteration >= 4'\n    maxIterations: '{{Param.Max}}'\n",
            "",
            "",
        ),
    );
    let fx = Fixture::new(&tpl, &[("Max", "2")]);
    let job = fx.job();
    let LoopBinding::Bound(b) = &job.loops[0].binding else {
        panic!()
    };
    assert_eq!(b.max_iterations, 2);
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), nothing);
    assert_eq!(iterations(&run), [0, 1]);
    assert_eq!(
        run.failure(),
        format!(
            "Loop 'L' failed at the boundary of iteration 1, in maxIterations:\n{}",
            expected(&[(
                "steps[0] -> loop -> maxIterations",
                "Loop 'L' has run maxIterations (2) iterations without ending, so it fails.\n  {{Param.Max}}"
            )])
        )
    );
    // A computed bound outside 1 to 1,000 fails Job creation.
    for (max, value) in [("0", 0), ("1001", 1001)] {
        let err = Fixture::new(&tpl, &[("Max", max)]).create().unwrap_err();
        assert_eq!(
            err.to_string(),
            expected(&[(
                "steps[0] -> loop -> maxIterations",
                &format!(
                    "maxIterations must be from 1 to 1000; it is {value}.\n  {{{{Param.Max}}}}"
                )
            )])
        );
    }
}

#[test]
fn conformance_max_iterations_reached_by_condition_succeeds() {
    let fx = Fixture::new(
        &template(
            "",
            &single_loop(
                "    until: 'Loop.Iteration >= 2'\n    maxIterations: 3\n",
                "",
                "",
            ),
        ),
        &[],
    );
    let job = fx.job();
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), nothing);
    assert_eq!(iterations(&run), [0, 1, 2]);
    assert_eq!(run.result().iterations, 3);
}

#[test]
fn conformance_while_ends_at_max_iterations_succeeds() {
    let fx = Fixture::new(
        &template(
            "",
            &single_loop(
                "    while: 'Loop.Iteration < 3'\n    maxIterations: 3\n",
                "",
                "",
            ),
        ),
        &[],
    );
    let job = fx.job();
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), nothing);
    assert_eq!(iterations(&run), [0, 1, 2]);
    assert_eq!(run.result().ended_by, LoopEnd::While);
}

#[test]
fn conformance_body_step_failure_stops_loop() {
    let fx = Fixture::new(
        &template(
            "",
            &format!(
                "- name: L
  loop:
    until: 'Loop.Iteration >= 4'
    maxIterations: 10
    steps:
{}{}",
                body_step("Work", ""),
                body_step("Check", "      dependencies:\n      - dependsOn: Work\n")
            ),
        ),
        &[],
    );
    let job = fx.job();
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), |t| {
        if t.step == "Work" && t.iteration == 1 {
            Err("exit 1".into())
        } else {
            Ok(TaskOutputs::new())
        }
    });
    assert_eq!(
        run.trace(),
        [
            "L[0].Work: ran 1",
            "L[0].Check: ran 1",
            "L[1].Work: failed: exit 1"
        ]
    );
    assert_eq!(run.failure(), "L[1].Work");
}

#[test]
fn conformance_job_output_from_loop_output() {
    let tpl = |fail_on: i64| {
        template(
            &format!(
                "jobOutputDefinitions:
- name: Final
  type: INT
  value: \"{{{{fail('final is ' + string(Steps.L.Output.Final)) if Steps.L.Output.Final == {fail_on} else Steps.L.Output.Final}}}}\"
"
            ),
            &single_loop(
                "    until: 'Loop.Iteration >= 2'\n    maxIterations: 5\n",
                "  outputVariables:\n  - {name: Final, type: INT, collect: LAST, value: '{{Loop.Iteration}}'}\n",
                "",
            ),
        )
    };
    let failure = expected(&[(
        "jobOutputDefinitions[0] -> value",
        "Job output 'Final': final is 2\n  fail('final is ' + string(Steps.L.Output.Final)) if Steps.L.Output.Final == 2 else Steps.L.Output.Final\n  ^~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~",
    )]);
    for (fail_on, result) in [(7, Ok(Some(ExprValue::Int(2)))), (2, Err(failure.as_str()))] {
        let fx = Fixture::new(&tpl(fail_on), &[]);
        let job = fx.job();
        assert_eq!(
            openjd_model::job_output_inputs(&job, 0),
            ["Steps.L.Output.Final"]
        );
        assert_eq!(job.outputs.as_ref().unwrap()[0].evaluated, None);
        let run = run_loop(&fx, &job, "L", &StepInputs::new(), nothing);
        let inputs = with_loop(&job, "L", run.result());
        let got = evaluate_job_output(&fx.jt, &fx.params, &fx.ctx, &job, 0, &inputs);
        match result {
            Ok(v) => assert_eq!(got.unwrap(), v),
            Err(m) => assert_eq!(got.unwrap_err().to_string(), m),
        }
    }
}

// ── Loop binding ──────────────────────────────────────────────────

#[test]
fn create_job_records_loops_beside_steps_in_template_order() {
    let fx = Fixture::new(
        &template(
            "",
            &format!(
                "- name: A
  {SCRIPT}
- name: L
  description: the loop
  dependencies:
  - dependsOn: A
  loop:
    variables:
    - {{name: 'N', type: INT, initial: '{{{{2}}}}', next: '{{{{Loop.N + 1}}}}'}}
    until: 'Loop.Iteration >= 1'
    maxIterations: 4
    steps:
{}{}  outputVariables:
  - {{name: O, type: INT, collect: LAST, value: '{{{{Loop.N}}}}', unit: s}}
- name: C
  dependencies:
  - dependsOn: L
  {SCRIPT}
",
                body_step("W1", ""),
                body_step("W2", "")
            ),
        ),
        &[],
    );
    let job = fx.job();
    let names: Vec<&str> = job.steps.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["A", "C"]);
    assert_eq!(job.loops.len(), 1);
    let l = &job.loops[0];
    assert_eq!(l.name, "L");
    assert_eq!(l.description.as_deref(), Some("the loop"));
    assert_eq!(l.entry_index, 1);
    assert_eq!(l.condition, LoopCondition::Until);
    assert_eq!(l.body, ["W1", "W2"]);
    assert_eq!(l.variables[0].name, "N");
    assert_eq!(l.output_variables[0].unit.as_deref(), Some("s"));
    let LoopBinding::Bound(b) = &l.binding else {
        panic!("{:?}", l.binding)
    };
    assert_eq!(b.max_iterations, 4);
    assert_eq!(b.start.initial["N"], ExprValue::Int(2));
    assert_eq!(b.start.first_while, None);
    assert!(b.has_iterations());
    assert_eq!(l.tasks_known(), None);
    assert_eq!(l.binding_report(), None);
    // The graph has the loop as a node, in template order.
    let g = StepDependencyGraph::new(&job).unwrap();
    assert_eq!(g.topo_sorted_names().unwrap(), ["A", "L", "C"]);
    let node = g.step_node("L").unwrap();
    assert_eq!((node.kind, node.step_index), (StepNodeKind::Loop, 0));
    let node = g.step_node("C").unwrap();
    assert_eq!((node.kind, node.step_index), (StepNodeKind::Step, 1));
    // Serialized with its loops; a Job without loops has no `loops` key.
    let json = serde_json::to_value(&job).unwrap();
    assert_eq!(json["loops"][0]["name"], "L");
    assert_eq!(json["loops"][0]["binding"]["state"], "BOUND");
    assert_eq!(json["loops"][0]["binding"]["maxIterations"], 4);
    assert_eq!(
        json["loops"][0]["binding"]["start"],
        serde_json::json!({"initial": {"N": {"Int": 2}}})
    );
    assert_eq!(json["loops"][0]["entryIndex"], 1);
    assert_eq!(json["loops"][0]["condition"], "UNTIL");
    let plain = Fixture::new(&template("", &format!("- name: A\n  {SCRIPT}\n")), &[]).job();
    assert!(serde_json::to_value(&plain).unwrap().get("loops").is_none());
}

/// A loop `L` after a Step `P` with output `V`, whose `enableIf`, `initial`,
/// and first `while` are given.
fn after_producer(enable_if: &str, initial: &str, while_: &str) -> String {
    template(
        "",
        &format!(
            "- name: P
  outputVariables:
  - {{name: V, type: INT}}
  {SCRIPT}
- name: L
  dependencies:
  - dependsOn: P
{enable_if}  loop:
    variables:
    - {{name: 'N', type: INT, initial: \"{initial}\", next: '{{{{Loop.N + 1}}}}', maxValue: 5}}
    while: \"{while_}\"
    maxIterations: 3
    steps:
{}",
            body_step("W", "")
        ),
    )
}

fn p_succeeded(job: &Job, v: i64) -> StepInputs {
    let mut inputs = StepInputs::new();
    super::control_flow_binding_support::succeeded(
        &mut inputs,
        job,
        "P",
        &[outputs(&[("V", ExprValue::Int(v))])],
    );
    inputs
}

#[test]
fn loop_binding_waits_for_its_inputs_and_advances() {
    // enableIf reads P: nothing else is evaluated until it is decided.
    let fx = Fixture::new(
        &after_producer(
            "  enableIf: 'Steps.P.Output.V > 0'\n",
            "{{Steps.P.Output.V}}",
            "Loop.N < 2",
        ),
        &[],
    );
    let job = fx.job();
    let l = &job.loops[0];
    assert_eq!(l.input_producers(), ["P"]);
    let report = l.binding_report().unwrap();
    assert_eq!(
        format!("{report:?}"),
        "LoopBindingReport { enablement: Waiting { on: [\"Steps.P.Output.V\"], for_enablement: false }, \
         bound: Waiting { on: [], for_enablement: true }, \
         start: Waiting { on: [\"Steps.P.Output.V\"], for_enablement: true } }"
    );
    // With no new inputs it is unchanged.
    let same =
        advance_loop_binding(&fx.jt, &fx.params, &fx.ctx, &job, 0, &StepInputs::new()).unwrap();
    assert_eq!(&same, l);
    let bound =
        advance_loop_binding(&fx.jt, &fx.params, &fx.ctx, &job, 0, &p_succeeded(&job, 1)).unwrap();
    let LoopBinding::Bound(b) = &bound.binding else {
        panic!("{:?}", bound.binding)
    };
    assert_eq!(b.start.initial["N"], ExprValue::Int(1));
    assert_eq!(b.start.first_while, Some(true));
    // Decided false: skipped.
    let skipped =
        advance_loop_binding(&fx.jt, &fx.params, &fx.ctx, &job, 0, &p_succeeded(&job, 0)).unwrap();
    assert!(matches!(skipped.binding, LoopBinding::Skipped));
    assert_eq!(skipped.tasks_known(), Some(0));
    // Advancing a bound loop returns it unchanged.
    let mut job2 = job.clone();
    job2.loops[0] = bound.clone();
    assert_eq!(
        advance_loop_binding(&fx.jt, &fx.params, &fx.ctx, &job2, 0, &StepInputs::new()).unwrap(),
        bound
    );
}

#[test]
fn loop_bound_and_start_groups_progress_separately() {
    // Only the start group reads P: maxIterations is known at creation.
    let fx = Fixture::new(
        &after_producer("", "{{Steps.P.Output.V}}", "Loop.N < 2"),
        &[],
    );
    let job = fx.job();
    let LoopBinding::PendingInputs(p) = &job.loops[0].binding else {
        panic!()
    };
    assert!(p.enablement.is_known());
    assert_eq!(format!("{:?}", p.bound), "Known(3)");
    assert_eq!(
        format!("{:?}", p.start),
        "Waiting { on: [\"Steps.P.Output.V\"] }"
    );
    // The first while false: zero iterations, not a failure.
    let bound =
        advance_loop_binding(&fx.jt, &fx.params, &fx.ctx, &job, 0, &p_succeeded(&job, 4)).unwrap();
    let LoopBinding::Bound(b) = &bound.binding else {
        panic!()
    };
    assert!(!b.has_iterations());
}

fn binding_error(e: BindingError) -> (String, MaterializationGroup, String) {
    let text = e.errors[0].to_string();
    (e.step, e.group, text)
}

#[test]
fn loop_start_group_errors_fail_the_loop_finally() {
    let fx = Fixture::new(
        &after_producer(
            "",
            "{{fail('no N') if Steps.P.Output.V < 0 else Steps.P.Output.V}}",
            "Loop.N < 2",
        ),
        &[],
    );
    let job = fx.job();
    let err = |v: i64| {
        binding_error(
            advance_loop_binding(&fx.jt, &fx.params, &fx.ctx, &job, 0, &p_succeeded(&job, v))
                .unwrap_err(),
        )
    };
    assert_eq!(
        err(-1),
        (
            "L".to_string(),
            MaterializationGroup::LoopStart,
            expected(&[(
                "steps[1] -> loop -> variables[0] -> initial",
                "Loop variable 'N': no N\n  fail('no N') if Steps.P.Output.V < 0 else Steps.P.Output.V\n  ^~~~~~~~~~~~"
            )])
        )
    );
    // A violated constraint, with the expression.
    assert_eq!(
        err(9).2,
        expected(&[(
            "steps[1] -> loop -> variables[0] -> initial",
            "Loop variable 'N': value 9 exceeds maximum 5\n  {{fail('no N') if Steps.P.Output.V < 0 else Steps.P.Output.V}}"
        )])
    );
    assert_eq!(
        format!(
            "{}",
            advance_loop_binding(&fx.jt, &fx.params, &fx.ctx, &job, 0, &p_succeeded(&job, -1))
                .unwrap_err()
        )
        .lines()
        .next()
        .unwrap(),
        "Step 'L' failed in its start group:"
    );
}

/// A loop `L` with a job parameter `X` (an `INT`, 0 by default), whose
/// `enableIf`, `initial`, and first `while` are given.
fn with_param(enable_if: &str, initial: &str, while_: &str) -> String {
    template(
        "parameterDefinitions:\n- {name: X, type: INT, default: 0}\n",
        &format!(
            "- name: L
{enable_if}  loop:
    variables:
    - {{name: 'N', type: INT, initial: \"{initial}\", next: '{{{{Loop.N + 1}}}}'}}
    while: \"{while_}\"
    maxIterations: 3
    steps:
{}",
            body_step("W", "")
        ),
    )
}

#[test]
fn loop_group_errors_at_creation_fail_create_job() {
    let cases = [
        (
            with_param("", "{{null if Param.X == 0 else 1}}", "Loop.N < 2"),
            "steps[0] -> loop -> variables[0] -> initial",
            "Loop variable 'N': Cannot coerce nulltype to int\n  null if Param.X == 0 else 1\n  ^~~~",
        ),
        (
            with_param("", "{{'x' if Param.X == 0 else 1}}", "Loop.N < 2"),
            "steps[0] -> loop -> variables[0] -> initial",
            "Loop variable 'N': Cannot convert 'x' to int: invalid digit found in string\n  'x' if Param.X == 0 else 1\n  ^~~",
        ),
        (
            with_param("", "{{1}}", "Loop.N / Param.X > 1"),
            "steps[0] -> loop -> while",
            "could not be evaluated: Division by zero\n  Loop.N / Param.X > 1\n  ~~~~~~~^~~~~~~~~",
        ),
        (
            with_param(
                "  enableIf: \"fail('nope') if Param.X == 0 else true\"\n",
                "{{1}}",
                "true",
            ),
            "steps[0] -> enableIf",
            "could not be evaluated: nope\n  fail('nope') if Param.X == 0 else true\n  ^~~~~~~~~~~~",
        ),
    ];
    for (tpl, path, message) in cases {
        let err = Fixture::new(&tpl, &[]).create().unwrap_err();
        assert_eq!(err.to_string(), expected(&[(path, message)]), "{tpl}");
    }
}

// ── Iterations ────────────────────────────────────────────────────

#[test]
fn instance_group_errors_name_the_instance_and_fail_the_loop() {
    let fx = Fixture::new(
        &template(
            "",
            &format!(
                "- name: L
  loop:
    variables:
    - {{name: Frames, type: 'LIST[INT]', initial: '{{{{[1]}}}}', next: '{{{{[]}}}}'}}
    until: 'false'
    maxIterations: 3
    steps:
{}",
                body_step(
                    "R",
                    "      parameterSpace:\n        taskParameterDefinitions:\n        - {name: F, type: INT, range: \"{{fail('no frames') if len(Loop.Frames) == 0 else Loop.Frames}}\"}\n"
                )
            ),
        ),
        &[],
    );
    let job = fx.job();
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), nothing);
    assert_eq!(iterations(&run), [0]);
    assert_eq!(
        run.failure(),
        format!(
            "Step 'L[1].R' failed in its Task enumeration group:\n{}",
            expected(&[(
                "steps[0] -> loop -> steps[0] -> parameterSpace -> taskParameterDefinitions[0] -> range",
                "no frames\n  fail('no frames') if len(Loop.Frames) == 0 else Loop.Frames\n  ^~~~~~~~~~~~~~~~~"
            )])
        )
    );
}

#[test]
fn path_loop_variables_are_stored_raw_and_recorded_as_path_inputs() {
    let fx = Fixture::new(
        &template(
            "parameterDefinitions:\n- {name: Out, type: PATH}\n",
            "- name: L
  loop:
    variables:
    - {name: Dir, type: PATH, initial: '{{RawParam.Out}}', next: \"{{RawLoop.Dir / 'x'}}\"}
    - {name: Files, type: 'LIST[PATH]', initial: '{{[RawParam.Out]}}', next: '{{RawLoop.Files}}'}
    until: 'Loop.Iteration >= 1'
    maxIterations: 3
    steps:
    - name: W
      script:
        actions:
          onRun:
            command: echo
            args: ['{{Loop.Dir}}', '{{Loop.Files[0].name}}', '{{Loop.Iteration}}']
",
        ),
        &[("Out", "/src/out")],
    );
    let job = fx.job();
    let mut seen = Vec::new();
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), |t| {
        seen.push(t.symtab.clone());
        Ok(TaskOutputs::new())
    });
    run.result();
    let progress = LoopProgress::new(&job.loops[0]).unwrap();
    let it = start_iteration(
        &fx.jt,
        &fx.params,
        &fx.ctx,
        &job,
        0,
        &progress,
        &StepInputs::new(),
        0,
    )
    .unwrap();
    let w = &it.steps[0];
    assert_eq!(
        w.path_inputs,
        [
            PathInput {
                symbol: "RawLoop.Dir".into(),
                ty: ExprType::PATH
            },
            PathInput {
                symbol: "RawLoop.Files".into(),
                ty: ExprType::list(ExprType::PATH)
            },
        ]
    );
    // attach_step_inputs keeps them.
    let attached = attach_step_inputs(w, &StepInputs::new()).unwrap();
    assert_eq!(attached.path_inputs, w.path_inputs);
    assert_eq!(
        seen[0].get_value("RawLoop.Dir"),
        Some(&ExprValue::String("/src/out".into()))
    );
    assert_eq!(
        seen[1].get_value("RawLoop.Dir"),
        Some(&ExprValue::String("/src/out/x".into()))
    );
    assert!(seen[0].get_value("Loop.Dir").is_none());
    assert_eq!(
        seen[1].get_value("Loop.Iteration"),
        Some(&ExprValue::Int(1))
    );
}

#[test]
fn max_task_count_counts_instances_as_their_tasks_become_known() {
    let fx = Fixture::with_limits(
        &template(
            "",
            &format!(
                "- name: L
  loop:
    until: 'false'
    maxIterations: 3
    steps:
{}",
                body_step(
                    "R",
                    "      parameterSpace:\n        taskParameterDefinitions:\n        - {name: F, type: INT, range: '1-3'}\n"
                )
            ),
        ),
        &[],
        CallerLimits {
            max_task_count: Some(5),
            ..Default::default()
        },
    );
    let job = fx.job();
    let progress = LoopProgress::new(&job.loops[0]).unwrap();
    let start = |committed| {
        start_iteration(
            &fx.jt,
            &fx.params,
            &fx.ctx,
            &job,
            0,
            &progress,
            &StepInputs::new(),
            committed,
        )
    };
    assert_eq!(start(2).unwrap().steps[0].tasks_known(), Some(3));
    assert_eq!(
        binding_error(start(3).unwrap_err()),
        (
            "L[0].R".to_string(),
            MaterializationGroup::TaskEnumeration,
            expected(&[(
                "steps[0] -> loop -> steps[0]",
                "Total task count (6) exceeds caller limit of 5: Step 'L[0].R' has 3 Tasks, and the Steps whose Tasks are known before it have 3."
            )])
        )
    );
}

// ── The iteration boundary ────────────────────────────────────────

#[test]
fn boundary_checks_output_constraints() {
    let tpl = |outputs: &str| {
        template(
            "",
            &single_loop(
                "    until: 'Loop.Iteration >= 3'\n    maxIterations: 5\n",
                &format!("  outputVariables:\n{outputs}"),
                "",
            ),
        )
    };
    let cases = [
        (
            "  - {name: X, type: INT, collect: LAST, value: '{{Loop.Iteration}}', maxValue: 0}\n",
            1,
            "Loop 'L' failed at the boundary of iteration 1, in the value of loop output 'X':",
            "steps[0] -> outputVariables[0] -> value",
            "Loop output 'X': value 1 exceeds maximum 0\n  {{Loop.Iteration}}",
        ),
        (
            "  - {name: X, type: 'LIST[INT]', collect: APPEND, value: '{{Loop.Iteration}}', maxLength: 2}\n",
            2,
            "Loop 'L' failed at the boundary of iteration 2, in the value of loop output 'X':",
            "steps[0] -> outputVariables[0] -> value",
            "Loop output 'X': list length 3 exceeds maximum 2\n  {{Loop.Iteration}}",
        ),
        (
            "  - {name: X, type: 'LIST[INT]', collect: APPEND, value: '{{Loop.Iteration}}', item: {maxValue: 1}}\n",
            2,
            "Loop 'L' failed at the boundary of iteration 2, in the value of loop output 'X':",
            "steps[0] -> outputVariables[0] -> value",
            "Loop output 'X': element 2 exceeds maximum 1\n  {{Loop.Iteration}}",
        ),
        (
            "  - {name: X, type: 'LIST[INT]', collect: APPEND, value: \"{{'x' if Loop.Iteration == 0 else 1}}\"}\n",
            0,
            "Loop 'L' failed at the boundary of iteration 0, in the value of loop output 'X':",
            "steps[0] -> outputVariables[0] -> value",
            "Loop output 'X': Cannot convert 'x' to int: invalid digit found in string\n  'x' if Loop.Iteration == 0 else 1\n  ^~~",
        ),
        (
            "  - {name: X, type: 'LIST[INT]', collect: APPEND, value: '{{Loop.Iteration}}', minLength: 5}\n",
            3,
            "Loop 'L' failed at the boundary of iteration 3, in the minLength of loop output 'X':",
            "steps[0] -> outputVariables[0] -> minLength",
            "Loop output 'X': list length 4 is less than minimum 5",
        ),
    ];
    for (outputs, last, head, path, message) in cases {
        let fx = Fixture::new(&tpl(outputs), &[]);
        let job = fx.job();
        let run = run_loop(&fx, &job, "L", &StepInputs::new(), nothing);
        assert_eq!(*iterations(&run).last().unwrap(), last, "{outputs}");
        assert_eq!(
            run.failure(),
            format!("{head}\n{}", expected(&[(path, message)])),
            "{outputs}"
        );
    }
}

#[test]
fn boundary_condition_errors_name_the_field() {
    let tpl = |definition: &str| template("", &single_loop(definition, "", ""));
    let cases = [
        (
            "    until: 'null if Loop.Iteration == 0 else true'\n    maxIterations: 5\n",
            "Loop 'L' failed at the boundary of iteration 0, in until:",
            "steps[0] -> loop -> until",
            "resolved to null, which does not fit type bool (Template Schemas §7.6).\n  null if \
             Loop.Iteration == 0 else true",
        ),
        (
            "    variables:\n    - {name: 'N', type: INT, initial: '{{0}}', next: '{{Loop.N + 1}}'}\n    while: \"Loop.N < 2 if Loop.Iteration < 1 else fail('stop at ' + string(Loop.N))\"\n    maxIterations: 5\n",
            "Loop 'L' failed at the boundary of iteration 0, in while:",
            // `while` reads the new values with `Loop.Iteration` 1.
            "steps[0] -> loop -> while",
            "could not be evaluated: stop at 1\n  Loop.N < 2 if Loop.Iteration < 1 else fail('stop at ' + string(Loop.N))\n                                        ^~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~",
        ),
        (
            "    variables:\n    - {name: 'N', type: INT, initial: '{{0}}', next: '{{null if Loop.Iteration == 0 else 1}}'}\n    until: 'false'\n    maxIterations: 5\n",
            "Loop 'L' failed at the boundary of iteration 0, in the next of loop variable 'N':",
            "steps[0] -> loop -> variables[0] -> next",
            "Loop variable 'N': Cannot coerce nulltype to int\n  null if Loop.Iteration == 0 else 1\n  ^~~~",
        ),
    ];
    for (definition, head, path, message) in cases {
        let fx = Fixture::new(&tpl(definition), &[]);
        let job = fx.job();
        let run = run_loop(&fx, &job, "L", &StepInputs::new(), nothing);
        assert_eq!(
            run.failure(),
            format!("{head}\n{}", expected(&[(path, message)])),
            "{definition}"
        );
    }
}

#[test]
fn loop_value_size_limit_is_checked_at_three_points() {
    let tpl = template(
        "parameterDefinitions:\n- {name: S, type: STRING, default: ''}\n",
        &single_loop(
            "    variables:
    - {name: V, type: STRING, initial: '{{Param.S}}', next: \"{{Loop.V + 'abcdefghij'}}\"}
    until: 'Loop.Iteration >= 3'
    maxIterations: 5
",
            "  outputVariables:\n  - {name: H, type: 'LIST[STRING]', collect: APPEND, value: '{{Loop.V}}'}\n",
            "",
        ),
    );
    let limits = |n| CallerLimits {
        max_loop_value_bytes: Some(n),
        ..Default::default()
    };
    // After `initial`: "1234567890" is 12 bytes as JSON, and H, with no
    // elements, counts nothing: over 11, so Job creation fails.
    let err = Fixture::with_limits(&tpl, &[("S", "1234567890")], limits(11))
        .create()
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        expected(&[(
            "steps[0]",
            "Loop 'L': its loop variable and loop output values total 12 bytes, over the limit of 11 bytes."
        )])
    );
    // After the outputs: V "" (2) and H's one element "" (2) are 4, over 3.
    let fx = Fixture::with_limits(&tpl, &[], limits(3));
    let job = fx.job();
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), nothing);
    assert_eq!(
        run.failure(),
        format!(
            "Loop 'L' failed at the boundary of iteration 0, in the size of its values:\n{}",
            expected(&[(
                "steps[0]",
                "Loop 'L': its loop variable and loop output values total 4 bytes, over the limit of 3 bytes."
            )])
        )
    );
    // After `next`: "abcdefghij" (12) and H's "" (2) are 14, over 13; after
    // the outputs it was 4.
    let fx = Fixture::with_limits(&tpl, &[], limits(13));
    let job = fx.job();
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), nothing);
    assert_eq!(
        run.failure(),
        format!(
            "Loop 'L' failed at the boundary of iteration 0, in the size of its values:\n{}",
            expected(&[(
                "steps[0]",
                "Loop 'L': its loop variable and loop output values total 14 bytes, over the limit of 13 bytes."
            )])
        )
    );
}

// ── Inputs, consumption, and caller errors ────────────────────────

#[test]
fn add_loop_producer_types_outputs_and_checks_state() {
    let fx = Fixture::new(
        &template(
            "",
            &single_loop(
                "    while: 'Loop.Iteration < 1'\n    maxIterations: 3\n",
                "  outputVariables:
  - {name: P, type: PATH, collect: LAST, value: \"{{'/a/b'}}\"}
  - {name: 'N', type: INT, collect: LAST, value: '{{1}}'}
  - {name: H, type: 'LIST[INT]', collect: APPEND, value: '{{1}}'}
",
                "",
            ),
        ),
        &[],
    );
    let job = fx.job();
    let l = &job.loops[0];
    let mut values = IndexMap::new();
    values.insert(
        "P".to_string(),
        ExprValue::new_path("/a/b", PathFormat::Posix),
    );
    values.insert("N".to_string(), ExprValue::Null);
    values.insert("H".to_string(), ints(&[1]));
    let mut inputs = StepInputs::new();
    inputs
        .add_loop_producer(l, LoopProducerState::Succeeded { outputs: &values })
        .unwrap();
    // A path-typed output is held only in its Raw form, and derived on the
    // worker host.
    assert_eq!(inputs.get("Steps.L.Output.P"), None);
    assert!(inputs.is_concrete("Steps.L.Output.P"));
    assert_eq!(
        inputs.get("Steps.L.RawOutput.P"),
        Some(&ExprValue::new_path("/a/b", PathFormat::Posix))
    );
    assert_eq!(inputs.get("Steps.L.Output.N"), Some(&ExprValue::Null));
    assert_eq!(inputs.get("Steps.L.Output.H"), Some(&ints(&[1])));
    let err = |state: LoopProducerState<'_>,
               values_override: Option<IndexMap<String, ExprValue>>| {
        let mut inputs = StepInputs::new();
        let v = values_override.unwrap_or_else(|| values.clone());
        let state = match state {
            LoopProducerState::Succeeded { .. } => LoopProducerState::Succeeded { outputs: &v },
            s => s,
        };
        inputs.add_loop_producer(l, state).unwrap_err().to_string()
    };
    assert_eq!(
        err(LoopProducerState::Skipped, None),
        "Compatibility error: loop 'L' is not skipped, so it cannot be added as a skipped producer."
    );
    let mut missing = values.clone();
    missing.shift_remove("H");
    assert_eq!(
        err(
            LoopProducerState::Succeeded { outputs: &values },
            Some(missing)
        ),
        "Compatibility error: loop 'L' has no value for output 'H'."
    );
    let mut bad = values.clone();
    bad.insert("N".into(), ExprValue::String("x".into()));
    assert_eq!(
        err(LoopProducerState::Succeeded { outputs: &values }, Some(bad)),
        "Compatibility error: loop 'L': output 'N' does not convert to INT: Cannot convert 'x' to int: invalid digit found in string"
    );
    let mut null_append = values.clone();
    null_append.insert("H".into(), ExprValue::Null);
    assert_eq!(
        err(LoopProducerState::Succeeded { outputs: &values }, Some(null_append)),
        "Compatibility error: loop 'L': APPEND output 'H' is null; a loop that succeeded has a list."
    );
}

#[test]
fn a_given_loop_leaves_outputs_it_lacks_unknown() {
    let fx = Fixture::new(
        &template(
            "",
            &single_loop(
                "    while: 'Loop.Iteration < 1'\n    maxIterations: 3\n",
                "  outputVariables:
  - {name: 'N', type: INT, collect: LAST, value: '{{1}}'}
  - {name: H, type: 'LIST[INT]', collect: APPEND, value: '{{1}}'}
",
                "",
            ),
        ),
        &[],
    );
    let job = fx.job();
    let l = &job.loops[0];
    let mut values = IndexMap::new();
    values.insert("N".to_string(), ExprValue::Int(4));
    let mut inputs = StepInputs::new();
    inputs
        .add_loop_producer(l, LoopProducerState::Given { outputs: &values })
        .unwrap();
    assert_eq!(inputs.get("Steps.L.Output.N"), Some(&ExprValue::Int(4)));
    assert!(!inputs.is_concrete("Steps.L.Output.H"));
    assert!(!inputs.is_concrete("Steps.L.RawOutput.H"));
    // The values given are checked as under Succeeded.
    values.insert("H".to_string(), ExprValue::Null);
    assert_eq!(
        inputs
            .add_loop_producer(l, LoopProducerState::Given { outputs: &values })
            .unwrap_err()
            .to_string(),
        "Compatibility error: loop 'L': APPEND output 'H' is null; an APPEND output is a list."
    );
}

#[test]
fn expression_inputs_list_the_steps_symbols_each_expression_reads() {
    let fx = Fixture::new(
        &template(
            "",
            &format!(
                "- name: P
  outputVariables:
  - {{name: V, type: INT}}
  {SCRIPT}
- name: L
  dependencies:
  - dependsOn: P
  enableIf: 'Steps.P.Output.V > 0'
  loop:
    variables:
    - {{name: 'N', type: INT, initial: '{{{{Steps.P.Output.V}}}}', next: '{{{{Steps.W.Output.X + Steps.P.Output.V}}}}'}}
    until: 'Steps.W.Output.X > 1 or Steps.W.Output.X < 0'
    maxIterations: 3
    steps:
{}  outputVariables:
  - {{name: O, type: INT, collect: LAST, value: '{{{{Loop.N}}}}'}}
",
                body_step("W", "      outputVariables:\n      - {name: X, type: INT}\n")
            ),
        ),
        &[],
    );
    let job = fx.job();
    let l = &job.loops[0];
    let inputs = |e: LoopExpression| l.expression_inputs(&fx.jt, &e).unwrap();
    assert_eq!(inputs(LoopExpression::EnableIf), ["Steps.P.Output.V"]);
    assert_eq!(
        inputs(LoopExpression::Initial("N".into())),
        ["Steps.P.Output.V"]
    );
    assert_eq!(
        inputs(LoopExpression::Next("N".into())),
        ["Steps.P.Output.V", "Steps.W.Output.X"]
    );
    assert_eq!(inputs(LoopExpression::Until), ["Steps.W.Output.X"]);
    assert!(inputs(LoopExpression::While).is_empty());
    assert!(inputs(LoopExpression::OutputValue("O".into())).is_empty());
    assert_eq!(
        l.expression_inputs(&fx.jt, &LoopExpression::Next("Q".into()))
            .unwrap_err()
            .to_string(),
        "Compatibility error: loop 'L' has no variable 'Q'."
    );
}

#[test]
fn loop_functions_refuse_caller_errors() {
    let fx = Fixture::new(
        &template(
            "",
            &single_loop(
                "    variables:\n    - {name: 'N', type: INT, initial: '{{0}}', next: '{{Loop.N}}'}\n    until: 'false'\n    maxIterations: 2\n",
                "",
                "",
            ),
        ),
        &[],
    );
    let job = fx.job();
    let progress = LoopProgress::new(&job.loops[0]).unwrap();
    let start = |job: &Job, index: usize, progress: &LoopProgress| {
        let e = start_iteration(
            &fx.jt,
            &fx.params,
            &fx.ctx,
            job,
            index,
            progress,
            &StepInputs::new(),
            0,
        )
        .unwrap_err();
        assert!(e.is_caller_error());
        e.errors[0].to_string()
    };
    assert_eq!(
        start(&job, 1, &progress),
        "Compatibility error: the Job has 1 loop, so it has no loop 1."
    );
    let past = LoopProgress {
        iteration: 2,
        ..progress.clone()
    };
    assert_eq!(
        start(&job, 0, &past),
        "Compatibility error: loop 'L' has maxIterations 2, so it has no iteration 2."
    );
    let no_values = LoopProgress {
        values: IndexMap::new(),
        ..progress.clone()
    };
    assert_eq!(
        start(&job, 0, &no_values),
        "Compatibility error: the progress of loop 'L' has no value for variable 'N'; pass the progress the loop functions returned."
    );
    let e = end_without_iterations(&job, 0).unwrap_err();
    assert!(e.is_caller_error());
    assert_eq!(
        e.to_string(),
        "Ending loop 'L' without iterations was refused, in its arguments:\n\
         Compatibility error: loop 'L' is not a bound loop whose first while is false."
    );
    // A boundary that reads a body Step not in the inputs.
    let fx2 = Fixture::new(
        &template(
            "",
            &format!(
                "- name: L
  loop:
    until: 'Steps.W.Output.X > 0'
    maxIterations: 2
    steps:
{}",
                body_step(
                    "W",
                    "      outputVariables:\n      - {name: X, type: INT}\n"
                )
            ),
        ),
        &[],
    );
    let job2 = fx2.job();
    let p2 = LoopProgress::new(&job2.loops[0]).unwrap();
    let e = evaluate_iteration_boundary(
        &fx2.jt,
        &fx2.params,
        &fx2.ctx,
        &job2,
        0,
        &p2,
        &StepInputs::new(),
    )
    .unwrap_err();
    assert!(e.is_caller_error());
    assert_eq!(
        e.errors[0].to_string(),
        "Compatibility error: loop 'L' reads 'Steps.W.Output.X' at its iteration boundary, which is not concrete in the given inputs."
    );
    // advance_body_step_binding with the wrong iteration.
    let it = start_iteration(
        &fx.jt,
        &fx.params,
        &fx.ctx,
        &job,
        0,
        &progress,
        &StepInputs::new(),
        0,
    )
    .unwrap();
    let next = LoopProgress {
        iteration: 1,
        ..progress.clone()
    };
    let e = advance_body_step_binding(
        &fx.jt,
        &fx.params,
        &fx.ctx,
        &job,
        0,
        &next,
        &it,
        0,
        &StepInputs::new(),
        0,
    )
    .unwrap_err();
    assert!(e.is_caller_error());
    assert_eq!(
        e.errors[0].to_string(),
        "Compatibility error: the iteration is L[0], but the progress is of loop 'L' at iteration 1."
    );
    // advance_step_binding maps a Job Step index past a loop.
    let fx3 = Fixture::new(
        &template(
            "",
            &format!(
                "{}- name: C
  dependencies:
  - dependsOn: L
  parameterSpace:
    taskParameterDefinitions:
    - {{name: X, type: INT, range: '{{{{Steps.L.Output.O}}}}'}}
  {SCRIPT}
",
                single_loop(
                    "    until: 'true'\n    maxIterations: 2\n",
                    "  outputVariables:\n  - {name: O, type: 'LIST[INT]', collect: APPEND, value: '{{5}}'}\n",
                    ""
                )
            ),
        ),
        &[],
    );
    let job3 = fx3.job();
    assert_eq!(index(&job3, "C"), 0);
    let run = run_loop(&fx3, &job3, "L", &StepInputs::new(), nothing);
    let c = advance_step_binding(
        &fx3.jt,
        &fx3.params,
        &fx3.ctx,
        &job3,
        0,
        &with_loop(&job3, "L", run.result()),
        0,
    )
    .unwrap();
    assert_eq!(c.tasks_known(), Some(1));
}

// ── Iteration start, without the driver ───────────────────────────
//
// `run_loop` advances every pending instance with full inputs, so it would
// hide an instance that `start_iteration` leaves pending; these inspect
// `start_iteration`'s own result.

/// The body Step `W` with a script, at body indentation.
const W: &str = "    - name: W
      script:
        actions:
          onRun:
            command: echo
";

/// A loop `L` that runs once, with this body, and nothing else.
fn body_loop(body: &str) -> String {
    template(
        "",
        &format!("- name: L\n  loop:\n    until: 'true'\n    maxIterations: 2\n    steps:\n{body}"),
    )
}

/// `start_iteration` of iteration 0 of loop 0, with no inputs.
fn start0(fx: &Fixture) -> Result<openjd_model::job::LoopIteration, BindingError> {
    let job = fx.job();
    let progress = LoopProgress::new(&job.loops[0]).unwrap();
    start_iteration(
        &fx.jt,
        &fx.params,
        &fx.ctx,
        &job,
        0,
        &progress,
        &StepInputs::new(),
        0,
    )
}

const SKIPPED_A: &str = "    - name: A
      enableIf: 'Loop.Iteration < 0'
      outputVariables:
      - {name: X, type: INT}
      script:
        actions:
          onRun:
            command: echo
";

const B_READS_A: &str = "    - name: B
      dependencies:
      - dependsOn: A
      enableIf: 'Steps.A.Output.X == null'
      script:
        actions:
          onRun:
            command: echo
";

#[test]
fn start_iteration_makes_a_skipped_instance_concrete_for_its_dependents() {
    let it = start0(&Fixture::new(
        &body_loop(&format!("{SKIPPED_A}{B_READS_A}")),
        &[],
    ))
    .unwrap();
    assert!(matches!(it.steps[0].binding, StepBinding::Skipped));
    assert!(matches!(it.steps[1].binding, StepBinding::Bound));
}

#[test]
fn start_iteration_binds_in_body_dependency_order_and_returns_body_order() {
    // B comes first in the body but depends on A.
    let it = start0(&Fixture::new(
        &body_loop(&format!("{B_READS_A}{SKIPPED_A}")),
        &[],
    ))
    .unwrap();
    let names: Vec<&str> = it.steps.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["B", "A"]);
    assert!(matches!(it.steps[0].binding, StepBinding::Bound));
    assert_eq!(it.instance_name(0).as_deref(), Some("L[0].B"));
    assert_eq!(it.instance_name(2), None);
}

#[test]
fn start_iteration_makes_known_tasks_concrete_down_a_chain() {
    let body = |name: &str, range: &str, dep: Option<&str>| {
        let deps = dep.map_or(String::new(), |d| {
            format!("      dependencies:\n      - dependsOn: {d}\n")
        });
        format!(
            "    - name: {name}\n{deps}      parameterSpace:\n        taskParameterDefinitions:\n        - {{name: F, type: INT, range: '{range}'}}\n      script:\n        actions:\n          onRun:\n            command: echo\n"
        )
    };
    let it = start0(&Fixture::new(
        &body_loop(&format!(
            "{}{}{}",
            body("A", "1-3", None),
            body("B", "{{Steps.A.TaskParam.F}}", Some("A")),
            body("C", "{{Steps.B.TaskParam.F[1:]}}", Some("B")),
        )),
        &[],
    ))
    .unwrap();
    let tasks: Vec<Option<u64>> = it.steps.iter().map(|s| s.tasks_known()).collect();
    assert_eq!(tasks, [Some(3), Some(3), Some(2)]);
}

#[test]
fn start_iteration_counts_each_instance_toward_max_task_count() {
    let r = |n: &str| {
        format!("    - name: {n}\n      parameterSpace:\n        taskParameterDefinitions:\n        - {{name: F, type: INT, range: '1-3'}}\n      script:\n        actions:\n          onRun:\n            command: echo\n")
    };
    let fx = Fixture::with_limits(
        &body_loop(&format!("{}{}", r("R1"), r("R2"))),
        &[],
        CallerLimits {
            max_task_count: Some(5),
            ..Default::default()
        },
    );
    assert_eq!(
        binding_error(start0(&fx).unwrap_err()),
        (
            "L[0].R2".to_string(),
            MaterializationGroup::TaskEnumeration,
            expected(&[(
                "steps[0] -> loop -> steps[1]",
                "Total task count (6) exceeds caller limit of 5: Step 'L[0].R2' has 3 Tasks, and the Steps whose Tasks are known before it have 3."
            )])
        )
    );
}

#[test]
fn start_iteration_total_is_exact_beyond_u64() {
    // R1 and R2 have (2^63 - 1) * 2 Tasks each, so their total, 2^65 - 4,
    // is past u64::MAX; a saturating u64 total would pass a u64::MAX limit.
    let r = |n: &str| {
        format!("    - name: {n}\n      parameterSpace:\n        taskParameterDefinitions:\n        - {{name: A, type: INT, range: '1-9223372036854775807'}}\n        - {{name: B, type: INT, range: [1, 2]}}\n      script:\n        actions:\n          onRun:\n            command: echo\n")
    };
    let fx = Fixture::with_limits(
        &body_loop(&format!("{}{}", r("R1"), r("R2"))),
        &[],
        CallerLimits {
            max_task_count: Some(u64::MAX),
            ..Default::default()
        },
    );
    assert_eq!(
        binding_error(start0(&fx).unwrap_err()),
        (
            "L[0].R2".to_string(),
            MaterializationGroup::TaskEnumeration,
            expected(&[(
                "steps[0] -> loop -> steps[1]",
                "Total task count (36893488147419103228) exceeds caller limit of 18446744073709551615: Step 'L[0].R2' has 18446744073709551614 Tasks, and the Steps whose Tasks are known before it have 18446744073709551614."
            )])
        )
    );
}

#[test]
fn body_step_host_requirement_errors_name_the_body_path() {
    let fx = Fixture::new(
        &template(
            "parameterDefinitions:\n- {name: A, type: STRING, default: 'bad name'}\n",
            &format!(
                "- name: L\n  loop:\n    until: 'true'\n    maxIterations: 2\n    steps:\n{W}{}",
                "    - name: C
      hostRequirements:
        attributes:
        - {name: 'attr.{{Param.A}}', anyOf: [x]}
      script:
        actions:
          onRun:
            command: echo
"
            ),
        ),
        &[],
    );
    assert_eq!(
        binding_error(start0(&fx).unwrap_err()),
        (
            "L[0].C".to_string(),
            MaterializationGroup::HostSelection,
            expected(&[(
                "steps[0] -> loop -> steps[1] -> hostRequirements -> attributes[0]",
                "name 'attr.bad name' does not match capability name pattern."
            )])
        )
    );
}

#[test]
fn top_level_step_after_a_loop_reports_its_template_path() {
    let fx = Fixture::new(
        &template(
            "parameterDefinitions:\n- {name: A, type: STRING, default: 'bad name'}\n",
            &format!(
                "- name: L\n  loop:\n    until: 'true'\n    maxIterations: 2\n    steps:\n{W}- name: C
  hostRequirements:
    attributes:
    - {{name: 'attr.{{{{Param.A}}}}', anyOf: [x]}}
  {SCRIPT}
"
            ),
        ),
        &[],
    );
    assert_eq!(
        fx.create().unwrap_err().to_string(),
        expected(&[(
            "steps[1] -> hostRequirements -> attributes[0]",
            "name 'attr.bad name' does not match capability name pattern."
        )])
    );
}

#[test]
fn raw_loop_of_a_non_path_variable_is_its_value() {
    let fx = Fixture::new(
        &template(
            "",
            "- name: L
  loop:
    variables:
    - {name: 'N', type: INT, initial: '{{7}}', next: '{{Loop.N}}'}
    until: 'true'
    maxIterations: 2
    steps:
    - name: W
      parameterSpace:
        taskParameterDefinitions:
        - {name: F, type: INT, range: '{{[RawLoop.N, Loop.N]}}'}
      script:
        actions:
          onRun:
            command: echo
",
        ),
        &[],
    );
    let it = start0(&fx).unwrap();
    let space = it.steps[0].parameter_space.as_ref().unwrap();
    let fs: Vec<String> = StepParameterSpaceIterator::new(space)
        .unwrap()
        .map(|set| set["F"].value.to_display_string())
        .collect();
    assert_eq!(fs, ["7", "7"]);
}

/// A loop with path variables `Dir` and `Other`, whose body Step `W` has
/// these `args`; the first iteration's `W`.
fn path_instance(args: &str) -> openjd_model::job::Step {
    let fx = Fixture::new(
        &template(
            "",
            &format!(
                "- name: L
  loop:
    variables:
    - {{name: Dir, type: PATH, initial: /src/out, next: '{{{{RawLoop.Dir}}}}'}}
    - {{name: Other, type: PATH, initial: /src/other, next: '{{{{RawLoop.Other}}}}'}}
    until: 'true'
    maxIterations: 2
    steps:
    - name: W
      script:
        actions:
          onRun:
            command: echo
            args: [{args}]
"
            ),
        ),
        &[],
    );
    start0(&fx).unwrap().steps.remove(0)
}

#[test]
fn only_the_path_variables_an_instance_reads_are_stored() {
    for args in ["'{{Loop.Dir}}'", "'{{Loop.Dir.name}}'", "'{{RawLoop.Dir}}'"] {
        let w = path_instance(args);
        let symbols: Vec<&str> = w.path_inputs.iter().map(|p| p.symbol.as_str()).collect();
        assert_eq!(symbols, ["RawLoop.Dir"], "{args}");
        // Stored as a string in the submission form, as the Session expects.
        let json = serde_json::to_value(w.resolved_symtab.as_ref().unwrap()).unwrap();
        let entry = json
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == "RawLoop.Dir")
            .unwrap();
        assert_eq!(
            entry,
            &serde_json::json!({"name": "RawLoop.Dir", "type": "string", "value": "/src/out"}),
            "{args}"
        );
    }
    // A literal PATH `initial` is converted to a path.
    let fx = Fixture::new(
        &template(
            "",
            &format!(
                "- name: L
  loop:
    variables:
    - {{name: F, type: FLOAT, initial: '{{{{1}}}}', next: '{{{{Loop.F}}}}'}}
    - {{name: P, type: PATH, initial: /a, next: '{{{{RawLoop.P}}}}'}}
    - {{name: LP, type: 'LIST[PATH]', initial: \"{{{{['/a']}}}}\", next: '{{{{RawLoop.LP}}}}'}}
    until: 'true'
    maxIterations: 2
    steps:
{W}"
            ),
        ),
        &[],
    );
    let job = fx.job();
    let LoopBinding::Bound(b) = &job.loops[0].binding else {
        panic!()
    };
    let types: Vec<String> = b
        .start
        .initial
        .values()
        .map(|v| v.expr_type().to_string())
        .collect();
    assert_eq!(types, ["float", "path", "list[path]"]);
}

#[test]
fn attach_step_inputs_keeps_raw_loop_records_beside_steps_inputs() {
    let fx = Fixture::new(
        &template(
            "",
            &format!(
                "- name: P
  outputVariables:
  - {{name: V, type: INT}}
  {SCRIPT}
- name: L
  dependencies:
  - dependsOn: P
  loop:
    variables:
    - {{name: Dir, type: PATH, initial: /src/out, next: '{{{{RawLoop.Dir}}}}'}}
    until: 'true'
    maxIterations: 2
    steps:
    - name: W
      script:
        actions:
          onRun:
            command: echo
            args: ['{{{{Loop.Dir}}}}', '{{{{Steps.P.Output.V}}}}']
"
            ),
        ),
        &[],
    );
    let job = fx.job();
    let deps = p_succeeded(&job, 3);
    let progress = LoopProgress::new(&job.loops[0]).unwrap();
    let it = start_iteration(&fx.jt, &fx.params, &fx.ctx, &job, 0, &progress, &deps, 0).unwrap();
    let attached = attach_step_inputs(&it.steps[0], &deps).unwrap();
    assert_eq!(
        attached.path_inputs,
        [PathInput {
            symbol: "RawLoop.Dir".into(),
            ty: ExprType::PATH
        }]
    );
    let st = attached
        .resolved_symtab
        .unwrap()
        .to_symtab(PathFormat::Posix)
        .unwrap();
    assert_eq!(st.get_value("Steps.P.Output.V"), Some(&ExprValue::Int(3)));
}

// ── Loop binding edges ────────────────────────────────────────────

#[test]
fn a_false_enable_if_evaluates_nothing_else() {
    // maxIterations 0 would fail the bound group.
    let fx = Fixture::new(
        &template(
            "parameterDefinitions:\n- {name: M, type: INT, default: 0}\n",
            &format!("- name: L\n  enableIf: 'Param.M > 0'\n  loop:\n    until: 'true'\n    maxIterations: '{{{{Param.M}}}}'\n    steps:\n{W}"),
        ),
        &[],
    );
    assert!(matches!(fx.job().loops[0].binding, LoopBinding::Skipped));
}

#[test]
fn a_waiting_start_group_names_its_producers() {
    let fx = Fixture::new(
        &after_producer("", "{{Steps.P.Output.V}}", "Loop.N < 2"),
        &[],
    );
    assert_eq!(fx.job().loops[0].input_producers(), ["P"]);
}

#[test]
fn step_name_in_a_loop_enable_if_is_the_loop_name() {
    let fx = Fixture::new(
        &template(
            "",
            &format!("- name: L\n  enableIf: \"Step.Name == 'L'\"\n  loop:\n    until: 'true'\n    maxIterations: 2\n    steps:\n{W}"),
        ),
        &[],
    );
    assert!(matches!(fx.job().loops[0].binding, LoopBinding::Bound(_)));
}

#[test]
fn max_iterations_accepts_1_and_1000() {
    for m in [1, 1000] {
        let fx = Fixture::new(
            &template(
                "parameterDefinitions:\n- {name: M, type: INT}\n",
                &format!("- name: L\n  loop:\n    until: 'true'\n    maxIterations: '{{{{Param.M}}}}'\n    steps:\n{W}"),
            ),
            &[("M", &m.to_string())],
        );
        let LoopBinding::Bound(b) = &fx.job().loops[0].binding else {
            panic!()
        };
        assert_eq!(b.max_iterations, m);
    }
}

// ── The size measurement ──────────────────────────────────────────

/// The size error of a loop with this one variable and these outputs, under
/// `limit`, at Job creation; `None` when it creates.
fn size_err(variable: &str, outputs: &str, limit: usize) -> Option<String> {
    let fx = Fixture::with_limits(
        &template(
            "",
            &format!("- name: L\n  loop:\n    variables:\n    - {variable}\n    until: 'true'\n    maxIterations: 2\n    steps:\n{W}{outputs}"),
        ),
        &[],
        CallerLimits {
            max_loop_value_bytes: Some(limit),
            ..Default::default()
        },
    );
    fx.create().err().map(|e| e.to_string())
}

fn size_message(total: usize, limit: usize) -> String {
    expected(&[(
        "steps[0]",
        &format!("Loop 'L': its loop variable and loop output values total {total} bytes, over the limit of {limit} bytes."),
    )])
}

#[test]
fn loop_value_size_is_json_bytes() {
    // "abc" is 5 bytes: at the limit is accepted.
    let v = "{name: V, type: STRING, initial: abc, next: '{{Loop.V}}'}";
    assert_eq!(size_err(v, "", 5), None);
    assert_eq!(size_err(v, "", 4), Some(size_message(5, 4)));
    // A LAST output with no value yet, and an APPEND output with no
    // elements, count nothing.
    let o = "  outputVariables:\n  - {name: O, type: INT, collect: LAST, value: '{{1}}'}\n  - {name: H, type: 'LIST[INT]', collect: APPEND, value: '{{1}}'}\n";
    assert_eq!(size_err(v, o, 5), None);
    // A path counts its text; a float its spelling; an int its digits.
    let p = "{name: V, type: PATH, initial: /abcdefghij, next: '{{RawLoop.V}}'}";
    assert_eq!(size_err(p, "", 12), Some(size_message(13, 12)));
    let f = "{name: V, type: FLOAT, initial: '{{1.5}}', next: '{{Loop.V}}'}";
    assert_eq!(size_err(f, "", 3), None);
    assert_eq!(size_err(f, "", 2), Some(size_message(3, 2)));
    let i = "{name: V, type: INT, initial: '{{12345}}', next: '{{Loop.V}}'}";
    assert_eq!(size_err(i, "", 4), Some(size_message(5, 4)));
    let l = "{name: V, type: 'LIST[INT]', initial: '{{[1, 22]}}', next: '{{Loop.V}}'}";
    assert_eq!(size_err(l, "", 5), Some(size_message(6, 5)));
}

// ── Loops downstream ──────────────────────────────────────────────

#[test]
fn a_loop_skipped_at_creation_is_null_for_steps_that_read_it() {
    let fx = Fixture::new(
        &template(
            "",
            &format!(
                "- name: L
  enableIf: false
  loop:
    until: 'true'
    maxIterations: 2
    steps:
{W}  outputVariables:
  - {{name: O, type: INT, collect: LAST, value: '{{{{1}}}}'}}
- name: C
  dependencies:
  - dependsOn: L
  enableIf: 'Steps.L.Output.O == null'
  {SCRIPT}
"
            ),
        ),
        &[],
    );
    assert!(matches!(step(&fx.job(), "C").binding, StepBinding::Bound));
}

#[test]
fn two_loops_keep_template_order_in_the_job_and_the_graph() {
    let lp = |n: &str| {
        format!("- name: {n}\n  loop:\n    until: 'true'\n    maxIterations: 2\n    steps:\n    - name: W{n}\n      {SCRIPT}\n")
    };
    let job = Fixture::new(
        &template(
            "",
            &format!("{}- name: S\n  {SCRIPT}\n{}", lp("L1"), lp("L2")),
        ),
        &[],
    )
    .job();
    let names: Vec<&str> = job.loops.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["L1", "L2"]);
    let entries: Vec<usize> = job.loops.iter().map(|l| l.entry_index).collect();
    assert_eq!(entries, [0, 2]);
    let g = StepDependencyGraph::new(&job).unwrap();
    assert_eq!(g.topo_sorted_names().unwrap(), ["L1", "S", "L2"]);
    let n = g.step_node("S").unwrap();
    assert_eq!((n.kind, n.step_index), (StepNodeKind::Step, 0));
    let n = g.step_node("L2").unwrap();
    assert_eq!((n.kind, n.step_index), (StepNodeKind::Loop, 1));
    // A Job whose loops do not fit the template order is refused.
    let mut bad = job.clone();
    bad.loops[1].entry_index = 0;
    assert_eq!(
        StepDependencyGraph::new(&bad).unwrap_err().to_string(),
        "Validation error: Loop 'L2' has entry index 0, which does not fit the Job's 3 Steps and loops"
    );
    let mut bad = job.clone();
    bad.loops[1].dependencies = Some(vec![openjd_model::job::StepDependency {
        depends_on: "Nope".into(),
    }]);
    assert_eq!(
        StepDependencyGraph::new(&bad).unwrap_err().to_string(),
        "Validation error: Loop 'L2' depends on unknown step 'Nope'"
    );
}

#[test]
fn add_loop_producer_holds_list_path_outputs_raw_and_needs_a_bound_loop() {
    let fx = Fixture::new(
        &template(
            "",
            &format!(
                "- name: L\n  loop:\n    until: 'true'\n    maxIterations: 2\n    steps:\n{W}  outputVariables:\n  - {{name: P, type: 'LIST[PATH]', collect: APPEND, value: \"{{{{'/a'}}}}\"}}\n"
            ),
        ),
        &[],
    );
    let job = fx.job();
    let mut values = IndexMap::new();
    values.insert(
        "P".to_string(),
        ExprValue::make_list(
            vec![ExprValue::new_path("/a", PathFormat::Posix)],
            ExprType::PATH,
        )
        .unwrap(),
    );
    let mut inputs = StepInputs::new();
    inputs
        .add_loop_producer(
            &job.loops[0],
            LoopProducerState::Succeeded { outputs: &values },
        )
        .unwrap();
    assert_eq!(inputs.get("Steps.L.Output.P"), None);
    assert!(inputs.is_concrete("Steps.L.Output.P"));
    assert_eq!(
        inputs
            .get("Steps.L.RawOutput.P")
            .map(ExprValue::to_display_string),
        Some(r#"["/a"]"#.to_string())
    );
    let mut skipped = job.clone();
    skipped.loops[0].binding = LoopBinding::Skipped;
    assert_eq!(
        StepInputs::new()
            .add_loop_producer(
                &skipped.loops[0],
                LoopProducerState::Succeeded { outputs: &values }
            )
            .unwrap_err()
            .to_string(),
        "Compatibility error: loop 'L' is not bound, so it cannot have succeeded."
    );
    assert_eq!(
        StepInputs::new()
            .add_loop_producer(
                &skipped.loops[0],
                LoopProducerState::Given { outputs: &values }
            )
            .unwrap_err()
            .to_string(),
        "Compatibility error: loop 'L' is not bound, so it cannot be given outputs."
    );
}

#[test]
fn loop_progress_new_and_output_lookup() {
    let fx = Fixture::new(
        &template(
            "",
            &format!(
                "- name: L\n  loop:\n    until: 'true'\n    maxIterations: 2\n    steps:\n{W}  outputVariables:\n  - {{name: A, type: 'LIST[INT]', collect: APPEND, value: '{{{{1}}}}'}}\n  - {{name: B, type: INT, collect: LAST, value: '{{{{1}}}}'}}\n"
            ),
        ),
        &[],
    );
    let job = fx.job();
    let p = LoopProgress::new(&job.loops[0]).unwrap();
    assert_eq!(p.iteration, 0);
    assert_eq!(p.outputs["A"], ints(&[]));
    assert_eq!(p.outputs["A"].expr_type(), ExprType::list(ExprType::INT));
    assert_eq!(p.outputs["B"], ExprValue::Null);
    assert_eq!(job.loops[0].output_variable("B").unwrap().name, "B");
    assert!(job.loops[0].output_variable("Z").is_none());
    let mut skipped = job.clone();
    skipped.loops[0].binding = LoopBinding::Skipped;
    assert_eq!(LoopProgress::new(&skipped.loops[0]), None);
}

// ── More caller errors ────────────────────────────────────────────

/// The caller error text of a `BindingError`.
fn caller_error(e: BindingError) -> String {
    assert!(e.is_caller_error(), "{e}");
    e.errors[0].to_string()
}

#[test]
fn loop_functions_refuse_a_mismatched_template_or_zero_iteration_loop() {
    let tpl = |name: &str| {
        template(
            "",
            &format!("- name: {name}\n  loop:\n    while: 'false'\n    maxIterations: 2\n    steps:\n{W}"),
        )
    };
    let fx = Fixture::new(&tpl("L"), &[]);
    let job = fx.job();
    let progress = LoopProgress::new(&job.loops[0]).unwrap();
    let start = |jt: &openjd_model::template::JobTemplate| {
        caller_error(
            start_iteration(
                jt,
                &fx.params,
                &fx.ctx,
                &job,
                0,
                &progress,
                &StepInputs::new(),
                0,
            )
            .unwrap_err(),
        )
    };
    assert_eq!(
        start(&fx.jt),
        "Compatibility error: loop 'L' has zero iterations: its first while is false; end it with end_without_iterations."
    );
    let other = Fixture::new(&tpl("Z"), &[]);
    assert_eq!(
        start(&other.jt),
        "Compatibility error: loop 0 of the Job is 'L', at entry 0 of the template's steps, but the template has no such loop there; pass the template the Job was created from."
    );
    assert_eq!(
        job.loops[0]
            .expression_inputs(&other.jt, &LoopExpression::While)
            .unwrap_err()
            .to_string(),
        "Compatibility error: the template has no loop 'L' at entry 0 of its steps; pass the template the Job was created from."
    );
    let e = end_without_iterations(&job, 2).unwrap_err();
    assert_eq!(
        e.errors[0].to_string(),
        "Compatibility error: the Job has 1 loop, so it has no loop 2."
    );
}

#[test]
fn advance_body_step_binding_checks_the_instance_and_leaves_a_bound_one() {
    let fx = Fixture::with_limits(
        &body_loop(
            "    - name: R
      parameterSpace:
        taskParameterDefinitions:
        - {name: F, type: INT, range: '1-3'}
      script:
        actions:
          onRun:
            command: echo
",
        ),
        &[],
        CallerLimits {
            max_task_count: Some(5),
            ..Default::default()
        },
    );
    let job = fx.job();
    let progress = LoopProgress::new(&job.loops[0]).unwrap();
    let it = start_iteration(
        &fx.jt,
        &fx.params,
        &fx.ctx,
        &job,
        0,
        &progress,
        &StepInputs::new(),
        0,
    )
    .unwrap();
    let advance = |it: &openjd_model::job::LoopIteration, committed| {
        advance_body_step_binding(
            &fx.jt,
            &fx.params,
            &fx.ctx,
            &job,
            0,
            &progress,
            it,
            0,
            &StepInputs::new(),
            committed,
        )
    };
    // A bound instance is returned as it is, whatever the committed count.
    assert_eq!(advance(&it, 100).unwrap(), it.steps[0]);
    let mut renamed = it.clone();
    renamed.steps[0].name = "X".into();
    assert_eq!(
        caller_error(advance(&renamed, 0).unwrap_err()),
        "Compatibility error: instance 0 of the iteration is 'X', but the template's body Step is 'R'."
    );
}

#[test]
fn loop_functions_refuse_a_progress_of_the_wrong_shape() {
    let fx = Fixture::new(
        &template(
            "",
            &single_loop(
                "    variables:\n    - {name: 'N', type: INT, initial: '{{0}}', next: '{{Loop.N + 1}}'}\n    until: 'Loop.Iteration >= 2'\n    maxIterations: 5\n",
                "  outputVariables:\n  - {name: Ns, type: 'LIST[INT]', collect: APPEND, value: '{{Loop.N}}'}\n  - {name: Last, type: INT, collect: LAST, value: '{{Loop.N}}'}\n",
                "",
            ),
        ),
        &[],
    );
    let job = fx.job();
    let good = LoopProgress::new(&job.loops[0]).unwrap();
    let refused = |edit: &dyn Fn(&mut LoopProgress)| {
        let mut p = good.clone();
        edit(&mut p);
        let start = caller_error(
            start_iteration(
                &fx.jt,
                &fx.params,
                &fx.ctx,
                &job,
                0,
                &p,
                &StepInputs::new(),
                0,
            )
            .unwrap_err(),
        );
        let e = evaluate_iteration_boundary(
            &fx.jt,
            &fx.params,
            &fx.ctx,
            &job,
            0,
            &p,
            &StepInputs::new(),
        )
        .unwrap_err();
        assert!(e.is_caller_error(), "{e}");
        assert_eq!(e.errors[0].to_string(), start);
        start
    };
    let msg = |m: &str| {
        format!("Compatibility error: the progress of loop 'L' {m}; pass the progress the loop functions returned.")
    };
    assert_eq!(
        refused(&|p| {
            p.values.insert("N".into(), ExprValue::String("x".into()));
        }),
        msg("has a value of type string for variable 'N', which is INT")
    );
    assert_eq!(
        refused(&|p| {
            p.values.insert("Q".into(), ExprValue::Int(1));
        }),
        msg("has a value for 'Q', which is not a variable of the loop")
    );
    assert_eq!(
        refused(&|p| {
            p.outputs.shift_remove("Ns");
        }),
        msg("has no value for output 'Ns'")
    );
    assert_eq!(
        refused(&|p| {
            p.outputs.insert("Ns".into(), ExprValue::Int(3));
        }),
        msg("has a value of type int for APPEND output 'Ns', which is LIST[INT]")
    );
    assert_eq!(
        refused(&|p| {
            p.outputs.insert("Ns".into(), ExprValue::Null);
        }),
        msg("has a value of type nulltype for APPEND output 'Ns', which is LIST[INT]")
    );
    assert_eq!(
        refused(&|p| {
            p.outputs.insert("Z".into(), ExprValue::Null);
        }),
        msg("has a value for 'Z', which is not an output of the loop")
    );
    // A LAST output may be null or of its type.
    let mut p = good.clone();
    p.outputs.insert("Last".into(), ExprValue::Int(4));
    start_iteration(
        &fx.jt,
        &fx.params,
        &fx.ctx,
        &job,
        0,
        &p,
        &StepInputs::new(),
        0,
    )
    .unwrap();
}

// ── Round-trips and the remaining messages ────────────────────────

#[test]
fn a_continue_progress_of_every_type_is_accepted_again() {
    let vars = [
        ("A", "INT", "'{{1}}'"),
        ("B", "FLOAT", "'{{1}}'"),
        ("C", "STRING", "x"),
        ("D", "PATH", "/a"),
        ("E", "BOOL", "'{{true}}'"),
        ("F", "RANGE_EXPR", "1-3"),
        ("G", "'LIST[INT]'", "'{{[]}}'"),
        ("H", "'LIST[FLOAT]'", "'{{[1, 2]}}'"),
        ("I", "'LIST[STRING]'", "'{{[]}}'"),
        ("J", "'LIST[PATH]'", "'{{[]}}'"),
        ("K", "'LIST[BOOL]'", "'{{[true]}}'"),
        ("M", "'LIST[LIST[INT]]'", "'{{[]}}'"),
    ];
    let mut definition = String::from("    variables:\n");
    for (name, ty, initial) in vars {
        let next = if ty.contains("PATH") {
            format!("RawLoop.{name}")
        } else {
            format!("Loop.{name}")
        };
        definition.push_str(&format!(
            "    - {{name: {name}, type: {ty}, initial: {initial}, next: '{{{{{next}}}}}'}}\n"
        ));
    }
    definition.push_str("    until: 'Loop.Iteration >= 2'\n    maxIterations: 5\n");
    let fx = Fixture::new(
        &template(
            "",
            &single_loop(
                &definition,
                "  outputVariables:\n  - {name: P, type: 'LIST[PATH]', collect: APPEND, value: '{{RawLoop.D}}'}\n  - {name: Q, type: 'LIST[LIST[INT]]', collect: APPEND, value: '{{RawLoop.G}}'}\n  - {name: R, type: PATH, collect: LAST, value: '{{RawLoop.D}}'}\n",
                "",
            ),
        ),
        &[],
    );
    let job = fx.job();
    // run_loop feeds each Continue progress back into start_iteration and
    // the next boundary.
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), nothing);
    assert_eq!(iterations(&run), [0, 1, 2]);
    let r = run.result();
    assert_eq!(out(r, "P").to_display_string(), r#"["/a", "/a", "/a"]"#);
    assert_eq!(out(r, "Q").to_display_string(), "[[], [], []]");
}

#[test]
fn a_progress_list_of_the_wrong_element_type_is_refused() {
    let fx = Fixture::new(
        &template(
            "",
            &single_loop(
                "    variables:\n    - {name: V, type: 'LIST[INT]', initial: '{{[1]}}', next: '{{Loop.V}}'}\n    until: 'true'\n    maxIterations: 2\n",
                "",
                "",
            ),
        ),
        &[],
    );
    let job = fx.job();
    let refused = |value: ExprValue| {
        let mut p = LoopProgress::new(&job.loops[0]).unwrap();
        p.values.insert("V".into(), value);
        caller_error(
            start_iteration(
                &fx.jt,
                &fx.params,
                &fx.ctx,
                &job,
                0,
                &p,
                &StepInputs::new(),
                0,
            )
            .unwrap_err(),
        )
    };
    let strings = |v: &[&str]| {
        ExprValue::make_list(
            v.iter().map(|s| ExprValue::String(s.to_string())).collect(),
            ExprType::STRING,
        )
        .unwrap()
    };
    let msg = |ty: &str| {
        format!("Compatibility error: the progress of loop 'L' has a value of type {ty} for variable 'V', which is LIST[INT]; pass the progress the loop functions returned.")
    };
    assert_eq!(refused(strings(&["x"])), msg("list[string]"));
    assert_eq!(refused(strings(&[])), msg("list[string]"));
    assert_eq!(refused(ExprValue::Int(1)), msg("int"));
}

#[test]
fn loop_value_size_sums_variables_in_utf8_bytes_and_counts_bools() {
    let two = "{name: V, type: STRING, initial: abc, next: '{{Loop.V}}'}\n    - {name: U, type: STRING, initial: de, next: '{{Loop.U}}'}";
    assert_eq!(size_err(two, "", 9), None);
    assert_eq!(size_err(two, "", 8), Some(size_message(9, 8)));
    let e = "{name: V, type: STRING, initial: é, next: '{{Loop.V}}'}";
    assert_eq!(size_err(e, "", 3), Some(size_message(4, 3)));
    let b = "{name: V, type: BOOL, initial: '{{true}}', next: '{{Loop.V}}'}";
    assert_eq!(size_err(b, "", 4), None);
    assert_eq!(size_err(b, "", 3), Some(size_message(4, 3)));
    // The error is at the loop's own path.
    let fx = Fixture::with_limits(
        &template(
            "",
            &format!("- name: S\n  {SCRIPT}\n- name: L\n  loop:\n    variables:\n    - {{name: V, type: STRING, initial: abc, next: '{{{{Loop.V}}}}'}}\n    until: 'true'\n    maxIterations: 2\n    steps:\n{W}"),
        ),
        &[],
        CallerLimits {
            max_loop_value_bytes: Some(4),
            ..Default::default()
        },
    );
    assert_eq!(
        fx.create().unwrap_err().to_string(),
        expected(&[(
            "steps[1]",
            "Loop 'L': its loop variable and loop output values total 5 bytes, over the limit of 4 bytes."
        )])
    );
}

#[test]
fn conversion_failures_name_the_value_and_the_expression() {
    let tpl = |definition: &str| {
        template(
            "parameterDefinitions:\n- {name: S, type: STRING, default: abc}\n",
            &single_loop(definition, "", ""),
        )
    };
    let err = |definition: &str| {
        Fixture::new(&tpl(definition), &[])
            .create()
            .unwrap_err()
            .to_string()
    };
    assert_eq!(
        err("    variables:\n    - {name: V, type: INT, initial: 'n{{Param.S}}', next: '{{Loop.V}}'}\n    until: 'true'\n    maxIterations: 2\n"),
        expected(&[(
            "steps[0] -> loop -> variables[0] -> initial",
            "Loop variable 'V': value 'nabc' does not convert to INT: Cannot convert 'nabc' to int: invalid digit found in string\n  n{{Param.S}}"
        )])
    );
    assert_eq!(
        err("    until: 'true'\n    maxIterations: 'n{{Param.S}}'\n"),
        expected(&[(
            "steps[0] -> loop -> maxIterations",
            "maxIterations must be an integer; it is 'nabc'.\n  n{{Param.S}}"
        )])
    );
}

#[test]
fn a_reported_value_in_a_loop_error_is_escaped() {
    let fx = Fixture::new(
        &template(
            "",
            &format!(
                "- name: L
  loop:
    until: 'true'
    maxIterations: 2
    steps:
{}  outputVariables:
  - {{name: O, type: STRING, collect: LAST, value: \"{{{{fail(Steps.W.Output.S) if Loop.Iteration == 0 else 'x'}}}}\"}}
",
                body_step("W", "      outputVariables:\n      - {name: S, type: STRING}\n")
            ),
        ),
        &[],
    );
    let job = fx.job();
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), |_| {
        Ok(outputs(&[("S", ExprValue::String("a\nb\u{1b}c".into()))]))
    });
    assert_eq!(
        run.failure(),
        format!(
            "Loop 'L' failed at the boundary of iteration 0, in the value of loop output 'O':\n{}",
            expected(&[(
                "steps[0] -> outputVariables[0] -> value",
                "Loop output 'O': a\\nb\\u{1b}c\n  fail(Steps.W.Output.S) if Loop.Iteration == 0 else 'x'\n  ^~~~~~~~~~~~~~~~~~~~~~"
            )])
        )
    );
}

#[test]
fn advance_loop_binding_keeps_the_known_bound_and_names_failing_groups() {
    let fx = Fixture::new(
        &after_producer("", "{{Steps.P.Output.V}}", "Loop.N < 2"),
        &[],
    );
    let job = fx.job();
    let bound =
        advance_loop_binding(&fx.jt, &fx.params, &fx.ctx, &job, 0, &p_succeeded(&job, 1)).unwrap();
    let LoopBinding::Bound(b) = &bound.binding else {
        panic!("{:?}", bound.binding)
    };
    assert_eq!(b.max_iterations, 3);
    // A bound group failure later: maxIterations from a parameter read only
    // once enableIf is decided.
    let fx = Fixture::new(
        &template(
            "parameterDefinitions:\n- {name: M, type: INT, default: 0}\n",
            &format!(
                "- name: P\n  outputVariables:\n  - {{name: V, type: INT}}\n  {SCRIPT}\n- name: L\n  dependencies:\n  - dependsOn: P\n  enableIf: 'Steps.P.Output.V > 0'\n  loop:\n    until: 'true'\n    maxIterations: '{{{{Param.M}}}}'\n    steps:\n{W}"
            ),
        ),
        &[],
    );
    let job = fx.job();
    let e = advance_loop_binding(&fx.jt, &fx.params, &fx.ctx, &job, 0, &p_succeeded(&job, 1))
        .unwrap_err();
    assert!(!e.is_caller_error());
    assert_eq!(
        e.to_string(),
        format!(
            "Step 'L' failed in its bound group:\n{}",
            expected(&[(
                "steps[1] -> loop -> maxIterations",
                "maxIterations must be from 1 to 1000; it is 0.\n  {{Param.M}}"
            )])
        )
    );
    // start_iteration on a loop that is not bound yet.
    let pending = Fixture::new(
        &after_producer("", "{{Steps.P.Output.V}}", "Loop.N < 2"),
        &[],
    );
    let pjob = pending.job();
    let mut fake = pjob.clone();
    fake.loops[0] = bound.clone();
    let progress = LoopProgress::new(&fake.loops[0]).unwrap();
    assert_eq!(
        caller_error(
            start_iteration(
                &pending.jt,
                &pending.params,
                &pending.ctx,
                &pjob,
                0,
                &progress,
                &StepInputs::new(),
                0
            )
            .unwrap_err()
        ),
        "Compatibility error: loop 'L' is not bound, so it has no iterations."
    );
}

#[test]
fn advance_body_step_binding_failures_name_the_instance() {
    let fx = Fixture::new(
        &body_loop(&format!(
            "{}    - name: B
      dependencies:
      - dependsOn: A
      parameterSpace:
        taskParameterDefinitions:
        - {{name: F, type: INT, range: \"{{{{fail('no B') if Steps.A.Output.X == 0 else [1]}}}}\"}}
      script:
        actions:
          onRun:
            command: echo
",
            body_step(
                "A",
                "      outputVariables:\n      - {name: X, type: INT}\n"
            )
        )),
        &[],
    );
    let job = fx.job();
    let progress = LoopProgress::new(&job.loops[0]).unwrap();
    let it = start_iteration(
        &fx.jt,
        &fx.params,
        &fx.ctx,
        &job,
        0,
        &progress,
        &StepInputs::new(),
        0,
    )
    .unwrap();
    assert!(matches!(it.steps[1].binding, StepBinding::PendingInputs(_)));
    let mut inputs = StepInputs::new();
    inputs
        .add_producer(
            &it.steps[0],
            ProducerState::Succeeded {
                task_outputs: &[outputs(&[("X", ExprValue::Int(0))])],
            },
        )
        .unwrap();
    let advance = |it: &openjd_model::job::LoopIteration| {
        advance_body_step_binding(
            &fx.jt, &fx.params, &fx.ctx, &job, 0, &progress, it, 1, &inputs, 0,
        )
    };
    let e = advance(&it).unwrap_err();
    assert_eq!(e.step, "L[0].B");
    assert_eq!(
        e.to_string(),
        format!(
            "Step 'L[0].B' failed in its Task enumeration group:\n{}",
            expected(&[(
                "steps[0] -> loop -> steps[1] -> parameterSpace -> taskParameterDefinitions[0] -> range",
                "no B\n  fail('no B') if Steps.A.Output.X == 0 else [1]\n  ^~~~~~~~~~~~"
            )])
        )
    );
    let mut other = it.clone();
    other.loop_name = "M".into();
    assert_eq!(
        caller_error(advance(&other).unwrap_err()),
        "Compatibility error: the iteration is M[0], but the progress is of loop 'L' at iteration 0."
    );
}

#[test]
fn loop_error_is_caller_error_needs_an_error() {
    let e = openjd_model::job::LoopError {
        loop_name: "L".into(),
        iteration: Some(0),
        field: openjd_model::job::LoopErrorField::Until,
        errors: Vec::new(),
    };
    assert!(!e.is_caller_error());
    assert_eq!(
        e.to_string(),
        "Loop 'L' failed at the boundary of iteration 0, in until:"
    );
}

// ── Carried-forward checks of body Steps ──────────────────────────

/// A loop whose body Step `W` fails its carried-forward checks when the job
/// parameter `Q` is 1: in its script's `args`, or in a Step Environment's
/// `onEnter` command.
fn bad_body(field: &str) -> Fixture {
    let (args, env) = match field {
        "script" => (
            "\n            args: [\"{{fail('bad arg') if Param.Q == 1 else 'a'}}\"]",
            "",
        ),
        _ => (
            "",
            "\n      stepEnvironments:
      - name: E
        script:
          actions:
            onEnter:
              command: \"{{fail('bad env') if Param.Q == 1 else 'a'}}\"",
        ),
    };
    Fixture::new(
        &template(
            "parameterDefinitions:\n- {name: Q, type: INT, default: 1}\n",
            &format!(
                "- name: L\n  loop:\n    until: 'true'\n    maxIterations: 2\n    steps:\n    - name: W{env}\n      script:\n        actions:\n          onRun:\n            command: echo{args}\n"
            ),
        ),
        &[],
    )
}

#[test]
fn body_step_carried_forward_checks_run_when_the_iteration_starts() {
    for (field, path, message) in [
        (
            "script",
            "steps[0] -> loop -> steps[0] -> script -> actions -> onRun -> args[0]",
            // The carried-forward checks report as template validation does.
            "Failed to parse interpolation expression at [0, 44]. bad arg\n  fail('bad arg') if Param.Q == 1 else 'a'\n  ^~~~~~~~~~~~~~~",
        ),
        (
            "environment",
            "steps[0] -> loop -> steps[0] -> stepEnvironments[0] -> script -> actions -> onEnter -> command",
            "Failed to parse interpolation expression at [0, 44]. bad env\n  fail('bad env') if Param.Q == 1 else 'a'\n  ^~~~~~~~~~~~~~~",
        ),
    ] {
        let fx = bad_body(field);
        // Nothing of a body Step is evaluated at Job creation.
        let job = fx.job();
        let progress = LoopProgress::new(&job.loops[0]).unwrap();
        let e = start_iteration(
            &fx.jt,
            &fx.params,
            &fx.ctx,
            &job,
            0,
            &progress,
            &StepInputs::new(),
            0,
        )
        .unwrap_err();
        assert!(!e.is_caller_error());
        assert_eq!(
            e.to_string(),
            format!(
                "Step 'L[0].W' failed in its remaining group:\n{}",
                expected(&[(path, message)])
            ),
            "{field}"
        );
    }
}

#[test]
fn a_missing_known_input_names_the_body_instance() {
    let fx = Fixture::new(
        &template(
            "",
            &format!(
                "- name: P\n  outputVariables:\n  - {{name: G, type: BOOL}}\n  {SCRIPT}\n- name: L\n  dependencies:\n  - dependsOn: P\n  loop:\n    until: 'true'\n    maxIterations: 2\n    steps:\n{}{}",
                body_step("A", "      outputVariables:\n      - {name: X, type: INT}\n"),
                "    - name: B
      dependencies:
      - dependsOn: A
      enableIf: 'Steps.P.Output.G'
      parameterSpace:
        taskParameterDefinitions:
        - {name: F, type: INT, range: '{{[Steps.A.Output.X]}}'}
      script:
        actions:
          onRun:
            command: echo
"
            ),
        ),
        &[],
    );
    let job = fx.job();
    let mut deps = StepInputs::new();
    super::control_flow_binding_support::succeeded(
        &mut deps,
        &job,
        "P",
        &[outputs(&[("G", ExprValue::Bool(true))])],
    );
    let progress = LoopProgress::new(&job.loops[0]).unwrap();
    let it = start_iteration(&fx.jt, &fx.params, &fx.ctx, &job, 0, &progress, &deps, 0).unwrap();
    // B's enablement was decided with P; advancing without P is refused.
    let e = advance_body_step_binding(
        &fx.jt,
        &fx.params,
        &fx.ctx,
        &job,
        0,
        &progress,
        &it,
        1,
        &StepInputs::new(),
        0,
    )
    .unwrap_err();
    assert_eq!(
        caller_error(e),
        "Compatibility error: the enablement group of Step 'L[0].B' was evaluated with 'Steps.P.Output.G', which the given inputs do not hold; on each call, pass every producer in the Step's input_producers() whose Tasks are known or that has succeeded or been skipped."
    );
}

// ── Body Step Environments, host requirements, and Step.Name ──────

/// A loop whose body Step `W` has two Step Environments: `E0`, enabled only
/// in iteration 1, whose `onEnter` fails its check in iteration 0, and `E1`
/// with `enableIf` `e1`.
fn body_environments(e1: &str) -> Fixture {
    Fixture::new(
        &body_loop(&format!(
            "    - name: W
      stepEnvironments:
      - name: E0
        enableIf: '{{{{Loop.Iteration == 1}}}}'
        script:
          actions:
            onEnter:
              command: \"{{{{fail('disabled env checked') if Loop.Iteration == 0 else 'a'}}}}\"
      - name: E1
        enableIf: \"{e1}\"
        script:
          actions:
            onEnter:
              command: echo
      script:
        actions:
          onRun:
            command: echo
"
        )),
        &[],
    )
}

#[test]
fn body_step_environment_enable_if_reads_loop_and_skips_the_disabled_one() {
    let it = start0(&body_environments("{{Loop.Iteration == 0}}")).unwrap();
    let enabled: Vec<bool> = it.steps[0]
        .step_environments
        .iter()
        .flatten()
        .map(|e| e.enabled)
        .collect();
    // E0 is disabled, so its failing check is not run.
    assert_eq!(enabled, [false, true]);
}

#[test]
fn body_step_environment_enable_if_errors_name_the_body_path() {
    let e = start0(&body_environments(
        "{{fail('bad enable') if Loop.Iteration == 0 else true}}",
    ))
    .unwrap_err();
    assert_eq!(
        e.to_string(),
        format!(
            "Step 'L[0].W' failed in its remaining group:\n{}",
            expected(&[(
                "steps[0] -> loop -> steps[0] -> stepEnvironments[1] -> enableIf",
                "bad enable\n  fail('bad enable') if Loop.Iteration == 0 else true\n  ^~~~~~~~~~~~~~~~~~"
            )])
        )
    );
}

#[test]
fn body_step_environment_reads_the_loops_dependencies() {
    let fx = Fixture::new(
        &template(
            "",
            &format!(
                "- name: Setup
  outputVariables:
  - {{name: Base, type: INT}}
  {SCRIPT}
- name: L
  dependencies:
  - dependsOn: Setup
  loop:
    until: 'true'
    maxIterations: 2
    steps:
    - name: W
      stepEnvironments:
      - name: E
        script:
          actions:
            onEnter:
              command: echo
              args: ['{{{{Steps.Setup.Output.Base + Loop.Iteration}}}}']
      script:
        actions:
          onRun:
            command: echo
"
            ),
        ),
        &[],
    );
    let job = fx.job();
    let mut deps = StepInputs::new();
    super::control_flow_binding_support::succeeded(
        &mut deps,
        &job,
        "Setup",
        &[outputs(&[("Base", ExprValue::Int(5))])],
    );
    let progress = LoopProgress::new(&job.loops[0]).unwrap();
    let it = start_iteration(&fx.jt, &fx.params, &fx.ctx, &job, 0, &progress, &deps, 0).unwrap();
    assert!(matches!(it.steps[0].binding, StepBinding::Bound));
}

/// `start_iteration`'s error for a body Step `W` with these
/// `hostRequirements` lines, each failing only in iteration 0.
fn body_host_requirements_error(hr: &str) -> String {
    let fx = Fixture::new(
        &format!(
            "{}parameterDefinitions:
- {{name: Arches, type: 'LIST[STRING]', default: [x86_64, arm64]}}
steps:
- name: L
  loop:
    until: 'true'
    maxIterations: 2
    steps:
    - name: W
      hostRequirements:
{hr}
      script:
        actions:
          onRun:
            command: echo
",
            header("[EXPR, CONTROL_FLOW, FEATURE_BUNDLE_1]")
        ),
        &[],
    );
    let e = start0(&fx).unwrap_err();
    assert_eq!(e.group, MaterializationGroup::HostSelection);
    e.errors[0].to_string()
}

#[test]
fn body_step_host_requirement_resolution_errors_name_the_body_path() {
    let at = "steps[0] -> loop -> steps[0] -> hostRequirements";
    for (hr, path, message) in [
        (
            "        attributes:\n        - {name: attr.custom.tag, anyOf: ['{{ null if Loop.Iteration == 0 else 1 }}']}",
            at.to_string(),
            format!("{at} -> attributes[0] -> anyOf: has no elements after resolution"),
        ),
        (
            "        attributes:\n        - {name: attr.worker.cpu.arch, allOf: ['{{ RawParam.Arches if Loop.Iteration == 0 else [] }}']}",
            at.to_string(),
            format!("{at} -> attributes[0] -> allOf: single-valued attribute cannot have more than 1 element after resolution"),
        ),
        (
            "        attributes:\n        - {name: 'attr.custom.{{\"x\" if Loop.Iteration == 0 else \"y\"}}', anyOf: [a]}\n        - {name: attr.custom.x, anyOf: [b]}",
            format!("{at} -> attributes[1]"),
            "duplicate attribute name 'attr.custom.x'.".to_string(),
        ),
        (
            "        amounts:\n        - {name: amount.worker.gpu, min: '{{5 if Loop.Iteration == 0 else 0}}', max: 1}",
            format!("{at} -> amounts[0]"),
            "min (5) > max (1).".to_string(),
        ),
        (
            "        attributes:\n        - {name: attr.worker.os.family, anyOf: ['{{\"bogus\" if Loop.Iteration == 0 else \"linux\"}}']}",
            format!("{at} -> attributes[0] -> anyOf[0]"),
            "value 'bogus' is not valid for attr.worker.os.family.".to_string(),
        ),
    ] {
        assert_eq!(
            body_host_requirements_error(hr),
            expected(&[(path.as_str(), message.as_str())]),
            "{hr}"
        );
    }
}

#[test]
fn a_loop_binds_at_creation_from_producers_known_then() {
    let fx = Fixture::new(
        &template(
            "",
            &format!(
                "- name: M
  parameterSpace:
    taskParameterDefinitions:
    - {{name: F, type: INT, range: '1-3'}}
  {SCRIPT}
- name: S
  enableIf: false
  outputVariables:
  - {{name: G, type: BOOL}}
  {SCRIPT}
- name: L
  dependencies:
  - dependsOn: M
  - dependsOn: S
  enableIf: 'Steps.S.Output.G == null'
  loop:
    variables:
    - {{name: Count, type: INT, initial: '{{{{len(Steps.M.TaskParam.F)}}}}', next: '{{{{Loop.Count}}}}'}}
    until: 'true'
    maxIterations: 2
    steps:
{W}"
            ),
        ),
        &[],
    );
    let job = fx.job();
    let LoopBinding::Bound(b) = &job.loops[0].binding else {
        panic!("{:?}", job.loops[0].binding)
    };
    assert_eq!(b.start.initial["Count"], ExprValue::Int(3));
}

#[test]
fn step_name_in_a_body_instance_is_the_body_step_name() {
    let it = start0(&Fixture::new(
        &body_loop(
            "    - name: W
      enableIf: \"Step.Name == 'W'\"
      script:
        actions:
          onRun:
            command: echo
            args: ['{{Step.Name}}']
",
        ),
        &[],
    ))
    .unwrap();
    assert!(matches!(it.steps[0].binding, StepBinding::Bound));
    let st = it.steps[0]
        .resolved_symtab
        .as_ref()
        .unwrap()
        .to_symtab(PathFormat::Posix)
        .unwrap();
    assert_eq!(
        st.get_value("Step.Name"),
        Some(&ExprValue::String("W".into()))
    );
}

#[test]
fn a_body_step_environment_let_carries_its_loop_values() {
    let fx = Fixture::new(
        &body_loop(
            "    - name: W
      stepEnvironments:
      - name: E
        script:
          let: ['d = Loop.D', 'r = RawLoop.D', 'k = Loop.K + Loop.Iteration']
          actions:
            onEnter:
              command: echo
              args: ['{{d}}', '{{r}}', '{{k}}']
      script:
        actions:
          onRun:
            command: echo
",
        )
        .replace(
            "  loop:\n",
            "  loop:\n    variables:\n    - {name: D, type: PATH, initial: /src/base, next: '{{RawLoop.D}}'}\n    - {name: K, type: INT, initial: '{{7}}', next: '{{Loop.K}}'}\n",
        ),
        &[],
    );
    let w = start0(&fx).unwrap().steps.remove(0);
    let st = w
        .resolved_symtab
        .as_ref()
        .unwrap()
        .to_symtab(PathFormat::Posix)
        .unwrap();
    assert_eq!(
        st.get_value("RawLoop.D"),
        Some(&ExprValue::String("/src/base".into()))
    );
    assert_eq!(st.get_value("Loop.K"), Some(&ExprValue::Int(7)));
    assert_eq!(st.get_value("Loop.Iteration"), Some(&ExprValue::Int(0)));
    assert_eq!(
        w.path_inputs,
        [PathInput {
            symbol: "RawLoop.D".into(),
            ty: ExprType::PATH
        }]
    );
}

#[test]
fn a_step_environment_let_carries_its_steps_inputs() {
    let fx = Fixture::new(
        &template(
            "",
            &format!(
                "- name: P
  outputVariables:
  - {{name: V, type: INT}}
  {SCRIPT}
- name: C
  dependencies:
  - dependsOn: P
  stepEnvironments:
  - name: E
    script:
      let: ['v = Steps.P.Output.V']
      actions:
        onEnter:
          command: echo
          args: ['{{{{v}}}}']
  {SCRIPT}
"
            ),
        ),
        &[],
    );
    let job = fx.job();
    let c = step(&job, "C");
    assert_eq!(c.input_producers(), ["P"]);
    let mut inputs = StepInputs::new();
    super::control_flow_binding_support::succeeded(
        &mut inputs,
        &job,
        "P",
        &[outputs(&[("V", ExprValue::Int(4))])],
    );
    let attached = attach_step_inputs(c, &inputs).unwrap();
    let st = attached
        .resolved_symtab
        .unwrap()
        .to_symtab(PathFormat::Posix)
        .unwrap();
    assert_eq!(st.get_value("Steps.P.Output.V"), Some(&ExprValue::Int(4)));
}

#[test]
fn loop_errors_name_one_byte_and_a_refused_boundary_call() {
    let b = "{name: V, type: INT, initial: '{{1}}', next: '{{Loop.V}}'}";
    assert_eq!(
        size_err(b, "", 0),
        Some(expected(&[(
            "steps[0]",
            "Loop 'L': its loop variable and loop output values total 1 byte, over the limit of 0 bytes."
        )]))
    );
    let fx = Fixture::new(&body_loop(W), &[]);
    let job = fx.job();
    let mut p = LoopProgress::new(&job.loops[0]).unwrap();
    p.iteration = 2;
    let e =
        evaluate_iteration_boundary(&fx.jt, &fx.params, &fx.ctx, &job, 0, &p, &StepInputs::new())
            .unwrap_err();
    assert_eq!(
        e.to_string(),
        "Evaluating the boundary of iteration 2 of loop 'L' was refused, in its arguments:\nCompatibility error: loop 'L' has maxIterations 2, so it has no iteration 2."
    );
}

// ── Script and Step Environment lets reading path values ──────────

#[test]
fn a_body_script_let_reading_a_path_variable_records_it() {
    let fx = Fixture::new(
        &body_loop(
            "    - name: W
      script:
        let: ['d = Loop.D']
        actions:
          onRun:
            command: echo
            args: ['{{d}}']
",
        )
        .replace(
            "  loop:\n",
            "  loop:\n    variables:\n    - {name: D, type: PATH, initial: /src/base, next: '{{RawLoop.D}}'}\n",
        ),
        &[],
    );
    let w = start0(&fx).unwrap().steps.remove(0);
    assert_eq!(
        w.path_inputs,
        [PathInput {
            symbol: "RawLoop.D".into(),
            ty: ExprType::PATH
        }]
    );
}

/// The `RawParam.X` value in the `resolved_symtab` of top-level Step `S`,
/// whose `field` (`script` or `env`) has `let: ['p = Param.X']` for a PATH
/// parameter `X`.
fn path_param_fallback(field: &str) -> Option<ExprValue> {
    let body = match field {
        "script" => "  script:\n    let: ['p = Param.X']\n    actions:\n      onRun:\n        command: echo\n        args: ['{{p}}']\n".to_string(),
        _ => format!("  stepEnvironments:\n  - name: E\n    script:\n      let: ['p = Param.X']\n      actions:\n        onEnter:\n          command: echo\n          args: ['{{{{p}}}}']\n  {SCRIPT}\n"),
    };
    let fx = Fixture::new(
        &template(
            "parameterDefinitions:\n- {name: X, type: PATH}\n",
            &format!("- name: S\n{body}"),
        ),
        &[("X", "/src/x")],
    );
    let job = fx.job();
    step(&job, "S")
        .resolved_symtab
        .as_ref()
        .unwrap()
        .to_symtab(PathFormat::Posix)
        .unwrap()
        .get_value("RawParam.X")
        .cloned()
}

#[test]
fn a_let_reading_a_path_param_carries_its_raw_param() {
    for field in ["script", "env"] {
        assert!(path_param_fallback(field).is_some(), "{field}");
    }
}

#[test]
fn a_first_while_reading_steps_waits_in_the_start_group() {
    let fx = Fixture::new(&after_producer("", "{{1}}", "Steps.P.Output.V > 0"), &[]);
    let job = fx.job();
    let LoopBinding::PendingInputs(p) = &job.loops[0].binding else {
        panic!("{:?}", job.loops[0].binding)
    };
    assert_eq!(
        format!("{:?}", p.start),
        "Waiting { on: [\"Steps.P.Output.V\"] }"
    );
    let bound =
        advance_loop_binding(&fx.jt, &fx.params, &fx.ctx, &job, 0, &p_succeeded(&job, 0)).unwrap();
    let LoopBinding::Bound(b) = &bound.binding else {
        panic!()
    };
    assert_eq!(b.start.first_while, Some(false));
}

#[test]
fn a_while_reading_steps_is_evaluated_at_each_boundary() {
    // `while` reads a dependency's output before the first iteration and at
    // every boundary, so the boundary's symbol table holds it too.
    let fx = Fixture::new(
        &after_producer("", "{{1}}", "Loop.N <= Steps.P.Output.V"),
        &[],
    );
    let job = fx.job();
    let deps = p_succeeded(&job, 2);
    let job = Job {
        loops: vec![advance_loop_binding(&fx.jt, &fx.params, &fx.ctx, &job, 0, &deps).unwrap()],
        ..job
    };
    let run = run_loop(&fx, &job, "L", &deps, nothing);
    assert_eq!(iterations(&run), [0, 1]);
    assert_eq!(run.result().ended_by, LoopEnd::While);
}

#[test]
fn the_initial_size_check_runs_after_every_initial() {
    // B's initial fails; the size limit, which A alone exceeds, is checked
    // only after both.
    let fx = Fixture::with_limits(
        &template(
            "parameterDefinitions:\n- {name: S, type: STRING, default: abc}\n",
            &single_loop(
                "    variables:\n    - {name: A, type: INT, initial: '{{10}}', next: '{{Loop.A}}'}\n    - {name: B, type: INT, initial: '{{int(Param.S)}}', next: '{{Loop.B}}'}\n    until: 'true'\n    maxIterations: 2\n",
                "",
                "",
            ),
        ),
        &[],
        CallerLimits {
            max_loop_value_bytes: Some(1),
            ..Default::default()
        },
    );
    assert_eq!(
        fx.create().unwrap_err().to_string(),
        expected(&[(
            "steps[0] -> loop -> variables[1] -> initial",
            "Loop variable 'B': Cannot convert 'abc' to int\n  int(Param.S)\n  ^~~~~~~~~~~~"
        )])
    );
}

#[test]
fn without_control_flow_a_step_environment_let_carries_its_param() {
    let fx = Fixture::new(
        &format!(
            "{}parameterDefinitions:\n- {{name: Q, type: INT, default: 3}}\nsteps:\n- name: S\n  stepEnvironments:\n  - name: E\n    script:\n      let: ['q = Param.Q']\n      actions:\n        onEnter:\n          command: echo\n          args: ['{{{{q}}}}']\n  {SCRIPT}\n",
            header("[EXPR]")
        ),
        &[],
    );
    let job = fx.job();
    let st = step(&job, "S")
        .resolved_symtab
        .as_ref()
        .unwrap()
        .to_symtab(PathFormat::Posix)
        .unwrap();
    assert_eq!(st.get_value("Param.Q"), Some(&ExprValue::Int(3)));
}
