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
    until: '{{Steps.Count.Output.V >= 20}}'
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
    while: '{{Loop.N < 3}}'
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
    while: '{{Loop.N > 0}}'
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
    while: '{{Loop.N > 0}}'
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
    until: '{{true}}'
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
    until: '{{{{Steps.Check.Output.Score > 10}}}}'
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
    until: '{{Loop.Iteration >= 2}}'
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
    until: '{{{{Steps.W.Output.Done}}}}'
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
    until: '{{{{Loop.Iteration >= 2}}}}'
    maxIterations: 10
    steps:
{}  outputVariables:
  - {{name: Xs, type: 'LIST[INT]', collect: APPEND, value: '{{{{Steps.S.Output.X or -1}}}}'}}
",
                body_step(
                    "S",
                    "      enableIf: '{{Loop.Iteration == 1}}'\n      outputVariables:\n      - {name: X, type: INT}\n"
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
    until: '{{{{len(Steps.K.Output.Rest) == 0}}}}'
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
    until: '{{{{Loop.Iteration >= 1}}}}'
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
    until: '{{{{Loop.Iteration >= 2}}}}'
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
    until: '{{{{Loop.Iteration >= 4}}}}'
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
                "    until: '{{Loop.Iteration >= 4}}'\n    maxIterations: 10\n",
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
    until: '{{Loop.Iteration >= 4}}'
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
    while: '{{Loop.Iteration < 3}}'
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
                "    until: '{{Loop.Iteration >= 4}}'\n    maxIterations: 3\n",
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
            "    until: '{{Loop.Iteration >= 4}}'\n    maxIterations: '{{Param.Max}}'\n",
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
                "    until: '{{Loop.Iteration >= 2}}'\n    maxIterations: 3\n",
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
                "    while: '{{Loop.Iteration < 3}}'\n    maxIterations: 3\n",
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
    until: '{{{{Loop.Iteration >= 4}}}}'
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
                "    until: '{{Loop.Iteration >= 2}}'\n    maxIterations: 5\n",
                "  outputVariables:\n  - {name: Final, type: INT, collect: LAST, value: '{{Loop.Iteration}}'}\n",
                "",
            ),
        )
    };
    for (fail_on, result) in [(7, Ok(Some(ExprValue::Int(2)))), (2, Err("final is 2"))] {
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
            Err(m) => assert!(got.unwrap_err().to_string().contains(m)),
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
    until: '{{{{Loop.Iteration >= 1}}}}'
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
            "  enableIf: '{{Steps.P.Output.V > 0}}'\n",
            "{{Steps.P.Output.V}}",
            "{{Loop.N < 2}}",
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
        &after_producer("", "{{Steps.P.Output.V}}", "{{Loop.N < 2}}"),
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
            "{{Loop.N < 2}}",
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
            with_param("", "{{null if Param.X == 0 else 1}}", "{{Loop.N < 2}}"),
            "steps[0] -> loop -> variables[0] -> initial",
            "Loop variable 'N': Cannot coerce nulltype to int\n  null if Param.X == 0 else 1\n  ^~~~",
        ),
        (
            with_param("", "{{'x' if Param.X == 0 else 1}}", "{{Loop.N < 2}}"),
            "steps[0] -> loop -> variables[0] -> initial",
            "Loop variable 'N': Cannot convert 'x' to int: invalid digit found in string\n  'x' if Param.X == 0 else 1\n  ^~~",
        ),
        (
            with_param("", "{{1}}", "{{Loop.N / Param.X > 1}}"),
            "steps[0] -> loop -> while",
            "Division by zero\n  Loop.N / Param.X > 1\n  ~~~~~~~^~~~~~~~~",
        ),
        (
            with_param(
                "  enableIf: \"{{fail('nope') if Param.X == 0 else true}}\"\n",
                "{{1}}",
                "{{true}}",
            ),
            "steps[0] -> enableIf",
            "nope\n  fail('nope') if Param.X == 0 else true\n  ^~~~~~~~~~~~",
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
    until: '{{{{false}}}}'
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
    until: '{{Loop.Iteration >= 1}}'
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
    until: '{{{{false}}}}'
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
                "Total task count (6) exceeds caller limit of 5: Step 'R' has 3 Tasks, and the Steps whose Tasks are known before it have 3."
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
                "    until: '{{Loop.Iteration >= 3}}'\n    maxIterations: 5\n",
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
            "    until: '{{null if Loop.Iteration == 0 else true}}'\n    maxIterations: 5\n",
            "Loop 'L' failed at the boundary of iteration 0, in until:",
            "steps[0] -> loop -> until",
            "Cannot coerce nulltype to bool\n  null if Loop.Iteration == 0 else true\n  ^~~~",
        ),
        (
            "    variables:\n    - {name: 'N', type: INT, initial: '{{0}}', next: '{{Loop.N + 1}}'}\n    while: \"{{Loop.N < 2 if Loop.Iteration < 1 else fail('stop at ' + string(Loop.N))}}\"\n    maxIterations: 5\n",
            "Loop 'L' failed at the boundary of iteration 0, in while:",
            // `while` reads the new values with `Loop.Iteration` 1.
            "steps[0] -> loop -> while",
            "stop at 1\n  Loop.N < 2 if Loop.Iteration < 1 else fail('stop at ' + string(Loop.N))\n                                        ^~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~",
        ),
        (
            "    variables:\n    - {name: 'N', type: INT, initial: '{{0}}', next: '{{null if Loop.Iteration == 0 else 1}}'}\n    until: '{{false}}'\n    maxIterations: 5\n",
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
    until: '{{Loop.Iteration >= 3}}'
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
    // After `initial`: "1234567890" is 12 bytes as JSON and the empty list
    // of H 2, over 11, so Job creation fails.
    let err = Fixture::with_limits(&tpl, &[("S", "1234567890")], limits(11))
        .create()
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        expected(&[(
            "steps[0]",
            "Loop 'L': its loop variable and loop output values total 14 bytes, over the limit of 11 bytes."
        )])
    );
    // After the outputs: "" (2) + [""] (4) is 6, over 5.
    let fx = Fixture::with_limits(&tpl, &[], limits(5));
    let job = fx.job();
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), nothing);
    assert_eq!(
        run.failure(),
        format!(
            "Loop 'L' failed at the boundary of iteration 0, in the size of its values:\n{}",
            expected(&[(
                "steps[0]",
                "Loop 'L': its loop variable and loop output values total 6 bytes, over the limit of 5 bytes."
            )])
        )
    );
    // After `next`: "abcdefghij" (12) + [""] (4) is 16, over 15; after the
    // outputs it was 6.
    let fx = Fixture::with_limits(&tpl, &[], limits(15));
    let job = fx.job();
    let run = run_loop(&fx, &job, "L", &StepInputs::new(), nothing);
    assert_eq!(
        run.failure(),
        format!(
            "Loop 'L' failed at the boundary of iteration 0, in the size of its values:\n{}",
            expected(&[(
                "steps[0]",
                "Loop 'L': its loop variable and loop output values total 16 bytes, over the limit of 15 bytes."
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
                "    while: '{{Loop.Iteration < 1}}'\n    maxIterations: 3\n",
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
  enableIf: '{{{{Steps.P.Output.V > 0}}}}'
  loop:
    variables:
    - {{name: 'N', type: INT, initial: '{{{{Steps.P.Output.V}}}}', next: '{{{{Steps.W.Output.X + Steps.P.Output.V}}}}'}}
    until: '{{{{Steps.W.Output.X > 1 or Steps.W.Output.X < 0}}}}'
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
                "    variables:\n    - {name: 'N', type: INT, initial: '{{0}}', next: '{{Loop.N}}'}\n    until: '{{false}}'\n    maxIterations: 2\n",
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
        "Compatibility error: the progress of loop 'L' has no value for variable 'N'."
    );
    let e = end_without_iterations(&job, 0).unwrap_err();
    assert!(e.is_caller_error());
    assert_eq!(
        e.to_string(),
        "Evaluating loop 'L' after zero iterations was refused, in its arguments:\n\
         Compatibility error: loop 'L' is not a bound loop whose first while is false."
    );
    // A boundary that reads a body Step not in the inputs.
    let fx2 = Fixture::new(
        &template(
            "",
            &format!(
                "- name: L
  loop:
    until: '{{{{Steps.W.Output.X > 0}}}}'
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
                    "    until: '{{true}}'\n    maxIterations: 2\n",
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
