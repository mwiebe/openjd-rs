// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `CONTROL_FLOW` (RFC 0011) progressive step input binding: each group of a
//! pending Step is evaluated as soon as its inputs are concrete, `enableIf`
//! first, and the first failing group fails the Step. Each test asserts the
//! Step's binding after every `create_job` or `advance_step_binding` call.

use openjd_expr::ExprValue;
use openjd_model::job::{
    BindingReport, GroupReport, GroupState, HostRequirements, MaterializationGroup, PendingBinding,
    ProducerState, StepBinding, StepInputs,
};

use super::control_flow_binding_support::*;

/// Producers `P` (output `Count`) and `Q` (depends on `P`, output `Gpu`), and a
/// consumer `C` that depends on both: its range reads `P`, its host
/// requirements read `Q`.
fn p_q_c(c_extra: &str) -> String {
    p_q_c_script(c_extra, "{actions: {onRun: {command: echo}}}")
}

/// [`p_q_c`] with `C`'s `script` given as flow YAML.
fn p_q_c_script(c_extra: &str, c_script: &str) -> String {
    format!(
        "{}parameterDefinitions: [{{name: Ok, type: BOOL, default: true}}]
steps:
  - name: P
    outputVariables: [{{name: Count, type: INT}}]
    script: {{actions: {{onRun: {{command: echo}}}}}}
  - name: Q
    dependencies: [{{dependsOn: P}}]
    outputVariables: [{{name: Gpu, type: INT}}]
    script: {{actions: {{onRun: {{command: echo}}}}}}
  - name: C
    dependencies: [{{dependsOn: P}}, {{dependsOn: Q}}]
    parameterSpace:
      taskParameterDefinitions:
        - {{name: X, type: INT, range: '{{{{range(Steps.P.Output.Count)}}}}'}}
    hostRequirements:
      amounts: [{{name: amount.worker.gpu, min: '{{{{Steps.Q.Output.Gpu}}}}'}}]
{c_extra}    script: {c_script}
",
        header("[EXPR, CONTROL_FLOW, FEATURE_BUNDLE_1]")
    )
}

fn waiting<T>(on: &[&str]) -> GroupState<T> {
    GroupState::Waiting {
        on: on.iter().map(|s| s.to_string()).collect(),
    }
}

fn n_is(job: &openjd_model::job::Job, n: i64) -> StepInputs {
    let mut inputs = StepInputs::new();
    succeeded(
        &mut inputs,
        job,
        "P",
        &[outputs(&[("Count", ExprValue::Int(n))])],
    );
    inputs
}

fn gpu_is(job: &openjd_model::job::Job, gpu: i64) -> StepInputs {
    let mut inputs = StepInputs::new();
    succeeded(
        &mut inputs,
        job,
        "Q",
        &[outputs(&[("Gpu", ExprValue::Int(gpu))])],
    );
    inputs
}

fn both(job: &openjd_model::job::Job, n: i64, gpu: i64) -> StepInputs {
    let mut inputs = n_is(job, n);
    succeeded(
        &mut inputs,
        job,
        "Q",
        &[outputs(&[("Gpu", ExprValue::Int(gpu))])],
    );
    inputs
}

fn gpu_hr(min: f64) -> HostRequirements {
    serde_json::from_value(serde_json::json!({
        "amounts": [{"name": "amount.worker.gpu", "min": min, "max": null}],
        "attributes": null,
    }))
    .unwrap()
}

#[test]
fn tasks_become_known_before_host_requirements() {
    let f = Fixture::new(&p_q_c(""), &[]);
    let mut job = f.job();
    assert_eq!(step(&job, "P").binding, StepBinding::Bound);
    assert_eq!(step(&job, "Q").binding, StepBinding::Bound);
    assert_eq!(
        step(&job, "C").binding,
        StepBinding::PendingInputs(PendingBinding {
            enablement: GroupState::Known(()),
            tasks: waiting(&["Steps.P.Output.Count"]),
            host_requirements: waiting(&["Steps.Q.Output.Gpu"]),
            remaining: GroupState::Known(()),
            producers: vec!["P".into(), "Q".into()],
        })
    );
    assert_eq!(step(&job, "C").tasks_known(), None);

    // After P alone: Tasks known, host requirements still waiting for Q.
    let c = f.advance(&mut job, "C", |j| n_is(j, 3), 0).unwrap();
    let StepBinding::PendingInputs(p) = &c.binding else {
        panic!("C is still pending: {:?}", c.binding);
    };
    assert!(matches!(p.tasks, GroupState::Known(Some(_))));
    assert_eq!(p.host_requirements, waiting(&["Steps.Q.Output.Gpu"]));
    assert_eq!(c.tasks_known(), Some(3));
    assert_eq!(
        c.parameter_space, None,
        "a pending Step keeps no partial fields"
    );
    assert_eq!(
        c.binding_report(),
        Some(BindingReport {
            enablement: GroupReport::Known,
            tasks: GroupReport::Known,
            host_requirements: GroupReport::Waiting {
                on: vec!["Steps.Q.Output.Gpu".into()],
                for_enablement: false,
            },
            remaining: GroupReport::Known,
        })
    );
    // P's values are needed again on every later call.
    assert_eq!(c.input_producers(), ["P", "Q"]);

    // After Q too: bound, with both moved into the Step's fields.
    let c = f.advance(&mut job, "C", |j| both(j, 3, 1), 0).unwrap();
    assert_eq!(c.binding, StepBinding::Bound);
    assert_eq!(c.tasks_known(), Some(3));
    assert_eq!(c.host_requirements, Some(gpu_hr(1.0)));
    assert!(c.parameter_space.is_some());
    assert!(c.resolved_symtab.is_some());
    assert_eq!(c.binding_report(), None);
}

#[test]
fn host_requirements_become_known_before_tasks() {
    let f = Fixture::new(&p_q_c(""), &[]);
    let mut job = f.job();
    let c = f.advance(&mut job, "C", |j| gpu_is(j, 2), 0).unwrap();
    assert_eq!(
        c.binding,
        StepBinding::PendingInputs(PendingBinding {
            enablement: GroupState::Known(()),
            tasks: waiting(&["Steps.P.Output.Count"]),
            host_requirements: GroupState::Known(Some(gpu_hr(2.0))),
            remaining: GroupState::Known(()),
            producers: vec!["P".into(), "Q".into()],
        })
    );
    assert_eq!(
        c.host_requirements, None,
        "a pending Step keeps no partial fields"
    );
    let c = f.advance(&mut job, "C", |j| both(j, 2, 2), 0).unwrap();
    assert_eq!(c.binding, StepBinding::Bound);
    assert_eq!(c.tasks_known(), Some(2));
    assert_eq!(c.host_requirements, Some(gpu_hr(2.0)));
}

#[test]
fn a_known_group_is_kept_and_not_evaluated_again() {
    // With N=3 known, a later call whose inputs say N=5 keeps the Tasks
    // already known: a known group is never evaluated again.
    let f = Fixture::new(&p_q_c(""), &[]);
    let mut job = f.job();
    f.advance(&mut job, "C", |j| n_is(j, 3), 0).unwrap();
    let c = f.advance(&mut job, "C", |j| both(j, 5, 1), 0).unwrap();
    assert_eq!(c.tasks_known(), Some(3));
}

#[test]
fn a_call_with_no_new_inputs_returns_the_step_unchanged() {
    let f = Fixture::new(&p_q_c(""), &[]);
    let mut job = f.job();
    let before = step(&job, "C").clone();
    let c = f.advance(&mut job, "C", |_| StepInputs::new(), 0).unwrap();
    assert_eq!(c, before);
    let partial = f.advance(&mut job, "C", |j| n_is(j, 3), 0).unwrap();
    let again = f.advance(&mut job, "C", |j| n_is(j, 3), 0).unwrap();
    assert_eq!(again, partial);
}

#[test]
fn a_bound_or_skipped_step_is_returned_unchanged() {
    let f = Fixture::new(&p_q_c(""), &[]);
    let mut job = f.job();
    let p = step(&job, "P").clone();
    assert_eq!(
        f.advance(&mut job, "P", |_| StepInputs::new(), 0).unwrap(),
        p
    );
}

/// `C` with a dynamic `enableIf` that reads `Q`, and a range that would fail
/// unless `Ok` is false.
fn dynamic_enable_if() -> String {
    p_q_c("    enableIf: '{{Steps.Q.Output.Gpu > 0}}'\n").replace(
        "range: '{{range(Steps.P.Output.Count)}}'",
        "range: '{{range(Steps.P.Output.Count) if Param.Ok else fail(\"bad range\")}}'",
    )
}

#[test]
fn a_dynamic_enable_if_keeps_every_group_waiting() {
    let f = Fixture::new(&dynamic_enable_if(), &[("Ok", "false")]);
    let mut job = f.job();
    // Nothing else is evaluated before enableIf is decided, so the failing
    // range is not reported at job creation.
    let expect_all_waiting = |job: &openjd_model::job::Job, tasks_on: &[&str], host_on: &[&str]| {
        assert_eq!(
            step(job, "C").binding,
            StepBinding::PendingInputs(PendingBinding {
                enablement: waiting(&["Steps.Q.Output.Gpu"]),
                tasks: waiting(tasks_on),
                host_requirements: waiting(host_on),
                remaining: waiting(&[]),
                producers: vec!["P".into(), "Q".into()],
            })
        );
    };
    expect_all_waiting(&job, &["Steps.P.Output.Count"], &["Steps.Q.Output.Gpu"]);
    assert_eq!(
        step(&job, "C").binding_report().unwrap().tasks,
        GroupReport::Waiting {
            on: vec!["Steps.P.Output.Count".into()],
            for_enablement: true,
        }
    );
    // P's value makes the range's input concrete, but enableIf still waits:
    // each group's `on` is recomputed, and nothing is evaluated.
    f.advance(&mut job, "C", |j| n_is(j, 2), 0).unwrap();
    expect_all_waiting(&job, &[], &["Steps.Q.Output.Gpu"]);
    // enableIf true: the range fails, at once.
    let err = f
        .advance(&mut job, "C", |j| both(j, 2, 1), 0)
        .expect_err("the range fails");
    assert_eq!(err.group, MaterializationGroup::TaskEnumeration);
    assert_eq!(
        err.to_string(),
        format!(
            "Step 'C' failed in its Task enumeration group:\n{}",
            expected(&[(
                "steps[2] -> parameterSpace -> taskParameterDefinitions[0] -> range",
                "bad range\n  range(Steps.P.Output.Count) if Param.Ok else fail(\"bad range\")\n                                               ^~~~~~~~~~~~~~~~~",
            )])
        )
    );
}

#[test]
fn a_dynamic_enable_if_that_is_false_skips_the_step() {
    let f = Fixture::new(&dynamic_enable_if(), &[("Ok", "false")]);
    let mut job = f.job();
    let c = f
        .advance(&mut job, "C", |j| both(j, 2, 0), 0)
        .expect("the failing range is never evaluated");
    assert_eq!(c.binding, StepBinding::Skipped);
    assert_eq!(c.tasks_known(), Some(0));
    assert_eq!(c.parameter_space, None);
}

/// `R` (output `Go: BOOL`) runs when `P`'s `Count` is positive; `C`'s
/// `enableIf` is `R`'s `Go`, which is `null` when `R` is skipped.
fn null_enable_if() -> String {
    format!(
        "{}steps:
  - name: P
    outputVariables: [{{name: Count, type: INT}}]
    script: {{actions: {{onRun: {{command: echo}}}}}}
  - name: R
    dependencies: [{{dependsOn: P}}]
    enableIf: '{{{{Steps.P.Output.Count > 0}}}}'
    outputVariables: [{{name: Go, type: BOOL}}]
    script: {{actions: {{onRun: {{command: echo}}}}}}
  - name: C
    dependencies: [{{dependsOn: R}}]
    enableIf: '{{{{Steps.R.Output.Go}}}}'
    script: {{actions: {{onRun: {{command: echo}}}}}}
",
        header("[EXPR, CONTROL_FLOW]")
    )
}

#[test]
fn an_enable_if_that_resolves_to_null_fails_the_step() {
    let f = Fixture::new(&null_enable_if(), &[]);
    let mut job = f.job();
    let r = f.advance(&mut job, "R", |j| n_is(j, 0), 0).unwrap();
    assert_eq!(r.binding, StepBinding::Skipped);
    let err = f
        .advance(
            &mut job,
            "C",
            |j| {
                let mut inputs = StepInputs::new();
                inputs
                    .add_producer(step(j, "R"), ProducerState::Skipped)
                    .unwrap();
                inputs
            },
            0,
        )
        .expect_err("null is not a bool");
    assert_eq!(err.group, MaterializationGroup::Enablement);
    assert_eq!(
        err.to_string(),
        format!(
            "Step 'C' failed in its enablement group:\n{}",
            expected(&[(
                "steps[2] -> enableIf",
                "Cannot coerce nulltype to bool\n  Steps.R.Output.Go\n  ~~~~~~~~~~~~~~~^~",
            )])
        )
    );
}

#[test]
fn a_failing_range_fails_the_step_while_the_remaining_group_waits() {
    let f = Fixture::new(
        &p_q_c(
            "    stepEnvironments: [{name: E, script: {actions: {onEnter: {command: echo, timeout: '{{Steps.Q.Output.Gpu + 1}}'}}}}]\n",
        )
        .replace(
            "range: '{{range(Steps.P.Output.Count)}}'",
            "range: '{{range(Steps.P.Output.Count) if Steps.P.Output.Count > 0 else fail(\"no frames\")}}'",
        ),
        &[],
    );
    let mut job = f.job();
    let err = f
        .advance(&mut job, "C", |j| n_is(j, 0), 0)
        .expect_err("the range fails as soon as P's value is known");
    assert_eq!(err.group, MaterializationGroup::TaskEnumeration);
    assert_eq!(
        err.to_string(),
        format!(
            "Step 'C' failed in its Task enumeration group:\n{}",
            expected(&[(
                "steps[2] -> parameterSpace -> taskParameterDefinitions[0] -> range",
                "no frames\n  range(Steps.P.Output.Count) if Steps.P.Output.Count > 0 else fail(\"no frames\")\n                                                               ^~~~~~~~~~~~~~~~~",
            )])
        )
    );
}

#[test]
fn a_failing_range_that_reads_no_steps_aborts_create_job() {
    // C waits for P's output in its host requirements, but its range reads
    // only a job parameter: it is evaluated at job creation and fails it.
    let template = p_q_c("").replace(
        "range: '{{range(Steps.P.Output.Count)}}'",
        "range: '{{fail(\"no\") if Param.Ok else [1]}}'",
    );
    let err = Fixture::new(&template, &[])
        .create()
        .expect_err("the range fails");
    assert_eq!(
        err.to_string(),
        expected(&[(
            "steps[2] -> parameterSpace -> taskParameterDefinitions[0] -> range",
            "no\n  fail(\"no\") if Param.Ok else [1]\n  ^~~~~~~~~~",
        )])
    );
}

#[test]
fn the_tasks_of_a_pending_producer_make_a_consumers_tasks_known() {
    // M's Tasks are known at creation though it is pending (its host
    // requirements wait for P); D's range reads M's TaskParam, so D's Tasks
    // become known from M's while M is still pending.
    let template = format!(
        "{}steps:
  - name: P
    outputVariables: [{{name: G, type: INT}}]
    script: {{actions: {{onRun: {{command: echo}}}}}}
  - name: M
    dependencies: [{{dependsOn: P}}]
    parameterSpace:
      taskParameterDefinitions: [{{name: F, type: INT, range: '1-4'}}]
    hostRequirements:
      amounts: [{{name: amount.worker.gpu, min: '{{{{Steps.P.Output.G}}}}'}}]
    script: {{actions: {{onRun: {{command: echo}}}}}}
  - name: D
    dependencies: [{{dependsOn: M}}]
    parameterSpace:
      taskParameterDefinitions: [{{name: G, type: INT, range: '{{{{Steps.M.TaskParam.F[1:]}}}}'}}]
    script: {{actions: {{onRun: {{command: echo}}}}}}
",
        header("[EXPR, CONTROL_FLOW, FEATURE_BUNDLE_1]")
    );
    let f = Fixture::new(&template, &[]);
    let job = f.job();
    assert_eq!(step(&job, "M").tasks_known(), Some(4));
    assert!(matches!(
        step(&job, "M").binding,
        StepBinding::PendingInputs(_)
    ));
    // D is bound at creation from M's TaskParam alone.
    assert_eq!(step(&job, "D").binding, StepBinding::Bound);
    assert_eq!(step(&job, "D").tasks_known(), Some(3));
}

#[test]
fn a_task_count_limit_fails_the_step_when_its_tasks_become_known() {
    let f = Fixture::with_limits(
        &p_q_c(""),
        &[],
        openjd_model::CallerLimits {
            max_task_count: Some(10),
            ..Default::default()
        },
    );
    let mut job = f.job();
    // P and Q have one Task each; the caller counts them as committed.
    let err = f
        .advance(&mut job, "C", |j| n_is(j, 9), 2)
        .expect_err("2 + 9 > 10");
    assert_eq!(err.group, MaterializationGroup::TaskEnumeration);
    assert_eq!(
        err.to_string(),
        format!(
            "Step 'C' failed in its Task enumeration group:\n{}",
            expected(&[(
                "steps[2]",
                "Total task count (11) exceeds caller limit of 10: Step 'C' has 9 Tasks, and the Steps whose Tasks are known before it have 2.",
            )])
        )
    );
    // 2 + 8 is within the limit.
    let mut job = f.job();
    let c = f.advance(&mut job, "C", |j| n_is(j, 8), 2).unwrap();
    assert_eq!(c.tasks_known(), Some(8));
}

#[test]
fn a_per_step_task_count_limit_fails_the_step() {
    let f = Fixture::with_limits(
        &p_q_c(""),
        &[],
        openjd_model::CallerLimits {
            max_step_task_count: Some(4),
            ..Default::default()
        },
    );
    let mut job = f.job();
    let err = f
        .advance(&mut job, "C", |j| n_is(j, 5), 0)
        .expect_err("5 > 4");
    assert_eq!(
        err.to_string(),
        format!(
            "Step 'C' failed in its Task enumeration group:\n{}",
            expected(&[(
                "steps[2]",
                "Step 'C' has 5 Tasks, which exceeds the caller limit of 4 Tasks per Step.",
            )])
        )
    );
}

#[test]
fn a_let_used_only_by_the_remaining_group_is_not_evaluated_with_the_tasks() {
    // `bad` is read only by the script, so it belongs to the remaining
    // group, which waits for Q (its timeout). The Task enumeration group
    // reads P; evaluating it must not evaluate `bad`.
    let template = p_q_c_script(
        "    let: ['bad = fail(\"boom\") if Param.Ok else 1']\n",
        "{actions: {onRun: {command: echo, args: ['{{bad}}'], timeout: '{{Steps.Q.Output.Gpu + 1}}'}}}",
    )
    .replace(
        "    hostRequirements:\n      amounts: [{name: amount.worker.gpu, min: '{{Steps.Q.Output.Gpu}}'}]\n",
        "",
    );
    let f = Fixture::new(&template, &[]);
    let mut job = f.job();
    let c = f.advance(&mut job, "C", |j| n_is(j, 2), 0).unwrap();
    assert_eq!(c.tasks_known(), Some(2));
    let err = f
        .advance(&mut job, "C", |j| both(j, 2, 1), 0)
        .expect_err("the remaining group evaluates `bad`");
    assert_eq!(err.group, MaterializationGroup::Remaining);
    assert_eq!(
        err.to_string(),
        format!(
            "Step 'C' failed in its remaining group:\n{}",
            expected(&[(
                "steps[2] -> let[0]",
                "Invalid expression in let binding 'bad': boom\n  bad = fail(\"boom\") if Param.Ok else 1\n        ^~~~~~~~~~~~",
            )])
        )
    );
}

#[test]
fn a_step_with_a_let_read_only_by_its_script_has_its_tasks_known_at_creation() {
    // The A11 case: only the remaining group waits for P.
    let template = format!(
        "{}steps:
  - name: P
    outputVariables: [{{name: X, type: STRING}}]
    script: {{actions: {{onRun: {{command: echo}}}}}}
  - name: C
    dependencies: [{{dependsOn: P}}]
    let: ['x = Steps.P.Output.X']
    parameterSpace:
      taskParameterDefinitions: [{{name: F, type: INT, range: '1-240'}}]
    script: {{actions: {{onRun: {{command: echo, args: ['{{{{x}}}}']}}}}}}
",
        header("[EXPR, CONTROL_FLOW, FEATURE_BUNDLE_1]")
    );
    let f = Fixture::new(&template, &[]);
    let mut job = f.job();
    let c = step(&job, "C");
    assert_eq!(c.tasks_known(), Some(240));
    assert_eq!(
        c.binding_report().unwrap().remaining,
        GroupReport::Waiting {
            on: vec!["Steps.P.Output.X".into()],
            for_enablement: false,
        }
    );
    let mut inputs = StepInputs::new();
    succeeded(
        &mut inputs,
        &job,
        "P",
        &[outputs(&[("X", ExprValue::String("v".into()))])],
    );
    let c = f.advance(&mut job, "C", |_| inputs, 0).unwrap();
    assert_eq!(c.binding, StepBinding::Bound);
    // The `let` value the script reads is in its resolved table.
    let symtab = c
        .resolved_symtab
        .as_ref()
        .unwrap()
        .to_symtab(openjd_expr::path_mapping::PathFormat::Posix)
        .unwrap();
    assert_eq!(symtab.get_value("x"), Some(&ExprValue::String("v".into())));
    assert_eq!(symtab.get_value("Steps.P.Output.X"), None);
}

#[test]
fn an_enable_if_that_reads_a_producer_skipped_at_creation_is_decided_then() {
    // P is skipped at creation; C comes first in the template, yet its
    // enableIf, which reads only P, is decided at creation.
    let template = format!(
        "{}parameterDefinitions: [{{name: Run, type: BOOL, default: false}}]
steps:
  - name: C
    dependencies: [{{dependsOn: P}}]
    enableIf: '{{{{Steps.P.Output.Count == null}}}}'
    script: {{actions: {{onRun: {{command: echo}}}}}}
  - name: P
    enableIf: '{{{{Param.Run}}}}'
    outputVariables: [{{name: Count, type: INT}}, {{name: Go, type: BOOL}}]
    script: {{actions: {{onRun: {{command: echo}}}}}}
  - name: D
    dependencies: [{{dependsOn: P}}]
    enableIf: '{{{{Steps.P.Output.Count != null}}}}'
    script: {{actions: {{onRun: {{command: echo}}}}}}
",
        header("[EXPR, CONTROL_FLOW]")
    );
    let job = Fixture::new(&template, &[]).job();
    assert_eq!(step(&job, "P").binding, StepBinding::Skipped);
    assert_eq!(step(&job, "C").binding, StepBinding::Bound);
    assert_eq!(step(&job, "D").binding, StepBinding::Skipped);
    // An enableIf that is null at creation fails job creation.
    let null = template.replace(
        "    enableIf: '{{Steps.P.Output.Count != null}}'\n",
        "    enableIf: '{{Steps.P.Output.Go}}'\n",
    );
    let err = Fixture::new(&null, &[])
        .create()
        .expect_err("D's enableIf is null");
    assert_eq!(
        err.to_string(),
        expected(&[(
            "steps[2] -> enableIf",
            "Cannot coerce nulltype to bool\n  Steps.P.Output.Go\n  ~~~~~~~~~~~~~~~^~",
        )])
    );
}

#[test]
fn a_skipped_producer_gives_its_consumers_null_and_an_empty_task_list() {
    let template = format!(
        "{}steps:
  - name: P
    enableIf: false
    parameterSpace:
      taskParameterDefinitions: [{{name: F, type: INT, range: '1-3'}}]
    outputVariables: [{{name: Count, type: INT}}]
    script: {{actions: {{onRun: {{command: echo}}}}}}
  - name: C
    dependencies: [{{dependsOn: P}}]
    parameterSpace:
      taskParameterDefinitions:
        - {{name: X, type: INT, range: '{{{{Steps.P.TaskParam.F or [7, 8]}}}}'}}
    hostRequirements:
      amounts: [{{name: amount.worker.gpu, min: '{{{{len(Steps.P.Output.Count or [])}}}}'}}]
    script: {{actions: {{onRun: {{command: echo}}}}}}
",
        header("[EXPR, CONTROL_FLOW, FEATURE_BUNDLE_1]")
    );
    let job = Fixture::new(&template, &[]).job();
    assert_eq!(step(&job, "P").binding, StepBinding::Skipped);
    assert_eq!(step(&job, "C").binding, StepBinding::Bound);
    assert_eq!(step(&job, "C").tasks_known(), Some(2));
    let mut inputs = StepInputs::new();
    inputs
        .add_producer(step(&job, "P"), ProducerState::Skipped)
        .unwrap();
    assert_eq!(inputs.get("Steps.P.Output.Count"), Some(&ExprValue::Null));
    assert_eq!(inputs.get("Steps.P.TaskParam.F"), Some(&ExprValue::Null));
    assert!(inputs.is_concrete("Steps.P.RawOutput.N"));
}

// ══════════════════════════════════════════════════════════════
// Known groups are kept across calls
// ══════════════════════════════════════════════════════════════

/// `P` (outputs `Go: BOOL`, `Gpu: INT`) and `Q` (output `Count: INT`), and
/// a consumer `C` of both whose range reads `Q`, with `c_extra` fields.
fn go_gpu_count(c_extra: &str) -> String {
    format!(
        "{}steps:
  - name: P
    outputVariables: [{{name: Go, type: BOOL}}, {{name: Gpu, type: INT}}]
    script: {{actions: {{onRun: {{command: echo}}}}}}
  - name: Q
    outputVariables: [{{name: Count, type: INT}}]
    script: {{actions: {{onRun: {{command: echo}}}}}}
  - name: C
    dependencies: [{{dependsOn: P}}, {{dependsOn: Q}}]
    parameterSpace:
      taskParameterDefinitions: [{{name: X, type: INT, range: '{{{{range(Steps.Q.Output.Count)}}}}'}}]
{c_extra}    script: {{actions: {{onRun: {{command: echo}}}}}}
",
        header("[EXPR, CONTROL_FLOW, FEATURE_BUNDLE_1]")
    )
}

/// `P`'s outputs, and `Q`'s when `count` is given.
fn p_and_q(job: &openjd_model::job::Job, go: bool, gpu: i64, count: Option<i64>) -> StepInputs {
    let mut inputs = StepInputs::new();
    succeeded(
        &mut inputs,
        job,
        "P",
        &[outputs(&[
            ("Go", ExprValue::Bool(go)),
            ("Gpu", ExprValue::Int(gpu)),
        ])],
    );
    if let Some(n) = count {
        succeeded(
            &mut inputs,
            job,
            "Q",
            &[outputs(&[("Count", ExprValue::Int(n))])],
        );
    }
    inputs
}

#[test]
fn a_known_enablement_is_kept() {
    // enableIf is decided true with Go=true; a later call whose inputs say
    // Go=false does not decide it again.
    let f = Fixture::new(
        &go_gpu_count("    enableIf: '{{Steps.P.Output.Go}}'\n"),
        &[],
    );
    let mut job = f.job();
    let c = f
        .advance(&mut job, "C", |j| p_and_q(j, true, 0, None), 0)
        .unwrap();
    assert!(matches!(c.binding, StepBinding::PendingInputs(_)));
    let c = f
        .advance(&mut job, "C", |j| p_and_q(j, false, 0, Some(2)), 0)
        .unwrap();
    assert_eq!(c.binding, StepBinding::Bound);
    assert_eq!(c.tasks_known(), Some(2));
}

#[test]
fn known_host_requirements_are_kept() {
    let f = Fixture::new(
        &go_gpu_count("    hostRequirements:\n      amounts: [{name: amount.worker.gpu, min: '{{Steps.P.Output.Gpu}}'}]\n"),
        &[],
    );
    let mut job = f.job();
    f.advance(&mut job, "C", |j| p_and_q(j, true, 2, None), 0)
        .unwrap();
    let c = f
        .advance(&mut job, "C", |j| p_and_q(j, true, 5, Some(1)), 0)
        .unwrap();
    assert_eq!(c.binding, StepBinding::Bound);
    assert_eq!(c.host_requirements, Some(gpu_hr(2.0)));
}

#[test]
fn a_known_remaining_group_keeps_its_environments_enabled() {
    let f = Fixture::new(
        &go_gpu_count("    stepEnvironments: [{name: E, enableIf: '{{Steps.P.Output.Gpu > 0}}', variables: {V: v}}]\n"),
        &[],
    );
    let mut job = f.job();
    let c = f
        .advance(&mut job, "C", |j| p_and_q(j, true, 0, None), 0)
        .unwrap();
    let StepBinding::PendingInputs(p) = &c.binding else {
        panic!("C is pending");
    };
    assert_eq!(p.remaining, GroupState::Known(()));
    assert!(!c.step_environments.as_ref().unwrap()[0].enabled);
    let c = f
        .advance(&mut job, "C", |j| p_and_q(j, true, 1, Some(1)), 0)
        .unwrap();
    assert_eq!(c.binding, StepBinding::Bound);
    assert!(!c.step_environments.as_ref().unwrap()[0].enabled);
}

#[test]
fn a_bound_step_has_every_let_in_its_resolved_table() {
    // The remaining group is known at creation; the Step binds later, when
    // its range's input arrives, and its script still gets `y` and
    // `Step.Name`.
    let template = go_gpu_count("    let: ['y = len(Job.Name) * 4']\n").replace(
        "    let: ['y = len(Job.Name) * 4']\n    script: {actions: {onRun: {command: echo}}}\n",
        "    let: ['y = len(Job.Name) * 4']\n    script: {actions: {onRun: {command: echo, args: ['{{y}}', '{{Step.Name}}']}}}\n",
    );
    let f = Fixture::new(&template, &[]);
    let mut job = f.job();
    let StepBinding::PendingInputs(p) = &step(&job, "C").binding else {
        panic!("C is pending");
    };
    assert_eq!(p.remaining, GroupState::Known(()));
    let c = f
        .advance(&mut job, "C", |j| p_and_q(j, true, 0, Some(2)), 0)
        .unwrap();
    assert_eq!(c.binding, StepBinding::Bound);
    let symtab = c
        .resolved_symtab
        .as_ref()
        .unwrap()
        .to_symtab(openjd_expr::path_mapping::PathFormat::Posix)
        .unwrap();
    assert_eq!(symtab.get_value("y"), Some(&ExprValue::Int(4)));
    assert_eq!(
        symtab.get_value("Step.Name"),
        Some(&ExprValue::String("C".into()))
    );
}

#[test]
fn a_step_bound_by_advancing_is_then_returned_unchanged() {
    let f = Fixture::new(&go_gpu_count(""), &[]);
    let mut job = f.job();
    let c = f
        .advance(&mut job, "C", |j| p_and_q(j, true, 0, Some(2)), 0)
        .unwrap();
    assert_eq!(c.binding, StepBinding::Bound);
    let again = f.advance(&mut job, "C", |_| StepInputs::new(), 0).unwrap();
    assert_eq!(again, c);
}

#[test]
fn a_step_skipped_by_advancing_is_then_returned_unchanged() {
    let f = Fixture::new(
        &go_gpu_count("    enableIf: '{{Steps.P.Output.Go}}'\n"),
        &[],
    );
    let mut job = f.job();
    let c = f
        .advance(&mut job, "C", |j| p_and_q(j, false, 0, None), 0)
        .unwrap();
    assert_eq!(c.binding, StepBinding::Skipped);
    assert_eq!(c.input_producers(), Vec::<String>::new());
    let again = f.advance(&mut job, "C", |_| StepInputs::new(), 0).unwrap();
    assert_eq!(again, c);
}

// ══════════════════════════════════════════════════════════════
// Caller errors
// ══════════════════════════════════════════════════════════════

#[test]
fn inputs_must_be_cumulative() {
    // The remaining group used `p`, which reads P; a later call that gives
    // only Q's values is refused as a caller error, not a failed Step.
    let f = Fixture::new(&go_gpu_count("    let: ['p = Steps.P.Output.Gpu']\n"), &[]);
    let mut job = f.job();
    let c = f
        .advance(&mut job, "C", |j| p_and_q(j, true, 1, None), 0)
        .unwrap();
    let StepBinding::PendingInputs(p) = &c.binding else {
        panic!("C is pending");
    };
    assert_eq!(p.remaining, GroupState::Known(()));
    let err = f
        .advance(
            &mut job,
            "C",
            |j| {
                let mut inputs = StepInputs::new();
                succeeded(
                    &mut inputs,
                    j,
                    "Q",
                    &[outputs(&[("Count", ExprValue::Int(1))])],
                );
                inputs
            },
            0,
        )
        .expect_err("P's values are missing");
    assert_eq!(err.group, MaterializationGroup::Remaining);
    assert!(matches!(
        err.errors[..],
        [openjd_model::ModelError::Compatibility(_)]
    ));
    assert_eq!(
        err.to_string(),
        "Binding Step 'C' was refused in its remaining group:\n\
         Compatibility error: the remaining group of Step 'C' was evaluated with 'Steps.P.Output.Gpu', which the given inputs do not hold; on each call, pass every producer in the Step's input_producers() whose Tasks are known or that has succeeded or been skipped."
    );
}

#[test]
fn a_step_index_past_the_job_is_a_caller_error() {
    let f = Fixture::new(&go_gpu_count(""), &[]);
    let job = f.job();
    let err = openjd_model::advance_step_binding(
        &f.jt,
        &f.params,
        &f.ctx,
        &job,
        9,
        &StepInputs::new(),
        0,
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Binding Step '#9' was refused in its enablement group:\n\
         Compatibility error: the Job has 3 Steps, so it has no Step 9."
    );
}

#[test]
fn a_template_that_is_not_the_jobs_is_a_caller_error() {
    let f = Fixture::new(&go_gpu_count(""), &[]);
    let job = f.job();
    let other = Fixture::new(&go_gpu_count("").replace("- name: C\n", "- name: D\n"), &[]);
    let err = openjd_model::advance_step_binding(
        &other.jt,
        &other.params,
        &other.ctx,
        &job,
        2,
        &StepInputs::new(),
        0,
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Binding Step 'C' was refused in its enablement group:\n\
         Compatibility error: Step 2 of the Job is 'C', but the template's is not; pass the template the Job was created from."
    );
}

// ══════════════════════════════════════════════════════════════
// Reports
// ══════════════════════════════════════════════════════════════

#[test]
fn every_group_reports_that_it_waits_for_enablement() {
    let f = Fixture::new(
        &go_gpu_count("    enableIf: '{{Steps.P.Output.Go}}'\n"),
        &[],
    );
    let job = f.job();
    let c = step(&job, "C");
    assert_eq!(
        c.binding_report(),
        Some(BindingReport {
            enablement: GroupReport::Waiting {
                on: vec!["Steps.P.Output.Go".into()],
                for_enablement: false,
            },
            tasks: GroupReport::Waiting {
                on: vec!["Steps.Q.Output.Count".into()],
                for_enablement: true,
            },
            host_requirements: GroupReport::Waiting {
                on: vec![],
                for_enablement: true,
            },
            remaining: GroupReport::Waiting {
                on: vec![],
                for_enablement: true,
            },
        })
    );
    assert_eq!(c.input_producers(), ["P", "Q"]);
    // A Step that reads P only in its enableIf still needs P.
    let f = Fixture::new(
        &format!(
            "{}steps:
  - name: P
    outputVariables: [{{name: Go, type: BOOL}}]
    script: {{actions: {{onRun: {{command: echo}}}}}}
  - name: C
    dependencies: [{{dependsOn: P}}]
    enableIf: '{{{{Steps.P.Output.Go}}}}'
    script: {{actions: {{onRun: {{command: echo}}}}}}
",
            header("[EXPR, CONTROL_FLOW]")
        ),
        &[],
    );
    assert_eq!(step(&f.job(), "C").input_producers(), ["P"]);
}

// ══════════════════════════════════════════════════════════════
// The remaining group
// ══════════════════════════════════════════════════════════════

#[test]
fn the_remaining_group_checks_a_concrete_steps_value() {
    // A timeout and a format-string cancelation mode are TEMPLATE-scope
    // fields, so each puts the remaining group behind P and is checked with
    // P's value.
    let template = format!(
        "{}steps:
  - name: P
    outputVariables: [{{name: Secs, type: INT}}, {{name: Mode, type: STRING}}]
    script: {{actions: {{onRun: {{command: echo}}}}}}
  - name: T
    dependencies: [{{dependsOn: P}}]
    script: {{actions: {{onRun: {{command: echo, timeout: '{{{{Steps.P.Output.Secs}}}}'}}}}}}
  - name: M
    dependencies: [{{dependsOn: P}}]
    script: {{actions: {{onRun: {{command: echo, cancelation: {{mode: '{{{{Steps.P.Output.Mode}}}}'}}}}}}}}
",
        header("[EXPR, CONTROL_FLOW, FEATURE_BUNDLE_1]")
    );
    let f = Fixture::new(&template, &[]);
    let mut job = f.job();
    for name in ["T", "M"] {
        let StepBinding::PendingInputs(p) = &step(&job, name).binding else {
            panic!("{name} is pending");
        };
        assert!(!p.remaining.is_known(), "{name}");
    }
    let p = |j: &openjd_model::job::Job| {
        let mut inputs = StepInputs::new();
        succeeded(
            &mut inputs,
            j,
            "P",
            &[outputs(&[
                ("Secs", ExprValue::Int(0)),
                ("Mode", ExprValue::String("BOGUS".into())),
            ])],
        );
        inputs
    };
    let err = f.advance(&mut job, "T", p, 0).expect_err("timeout 0");
    assert_eq!(
        err.to_string(),
        format!(
            "Step 'T' failed in its remaining group:\n{}",
            expected(&[(
                "steps[1] -> script -> actions -> onRun -> timeout",
                "timeout must be > 0."
            )])
        )
    );
    let err = f.advance(&mut job, "M", p, 0).expect_err("mode BOGUS");
    assert_eq!(
        err.to_string(),
        format!(
            "Step 'M' failed in its remaining group:\n{}",
            expected(&[(
                "steps[2] -> script -> actions -> onRun -> cancelation",
                "mode must resolve to TERMINATE or NOTIFY_THEN_TERMINATE, got 'BOGUS'.",
            )])
        )
    );
}

#[test]
fn every_known_groups_inputs_must_be_given_again() {
    let refused = |group: &str, sym: &str| {
        format!(
            "Binding Step 'C' was refused in its {group} group:\n\
             Compatibility error: the {group} group of Step 'C' was evaluated with '{sym}', which the given inputs do not hold; on each call, pass every producer in the Step's input_producers() whose Tasks are known or that has succeeded or been skipped."
        )
    };
    // Task enumeration known from P, then only Q.
    let f = Fixture::new(&p_q_c(""), &[]);
    let mut job = f.job();
    f.advance(&mut job, "C", |j| n_is(j, 2), 0).unwrap();
    let err = f.advance(&mut job, "C", |j| gpu_is(j, 1), 0).unwrap_err();
    assert_eq!(
        err.to_string(),
        refused("Task enumeration", "Steps.P.Output.Count")
    );
    // Host selection known from Q, then only P.
    let mut job = f.job();
    f.advance(&mut job, "C", |j| gpu_is(j, 1), 0).unwrap();
    let err = f.advance(&mut job, "C", |j| n_is(j, 2), 0).unwrap_err();
    assert_eq!(
        err.to_string(),
        refused("host selection", "Steps.Q.Output.Gpu")
    );
    // Enablement known from P, then only Q.
    let f = Fixture::new(
        &go_gpu_count("    enableIf: '{{Steps.P.Output.Go}}'\n"),
        &[],
    );
    let mut job = f.job();
    f.advance(&mut job, "C", |j| p_and_q(j, true, 0, None), 0)
        .unwrap();
    let only_q = |j: &openjd_model::job::Job| {
        let mut inputs = StepInputs::new();
        succeeded(
            &mut inputs,
            j,
            "Q",
            &[outputs(&[("Count", ExprValue::Int(1))])],
        );
        inputs
    };
    let err = f.advance(&mut job, "C", only_q, 0).unwrap_err();
    assert_eq!(err.to_string(), refused("enablement", "Steps.P.Output.Go"));
    // With two inputs missing, the first in sorted order is named.
    let f = Fixture::new(
        &go_gpu_count("    let: ['a = Steps.P.Output.Gpu', 'b = Steps.P.Output.Go']\n"),
        &[],
    );
    let mut job = f.job();
    f.advance(&mut job, "C", |j| p_and_q(j, true, 0, None), 0)
        .unwrap();
    let err = f.advance(&mut job, "C", only_q, 0).unwrap_err();
    assert_eq!(err.to_string(), refused("remaining", "Steps.P.Output.Go"));
}

#[test]
fn a_skipped_step_needs_no_producer() {
    let f = Fixture::new(
        &go_gpu_count("    enableIf: false\n").replace(
            "    enableIf: false\n    script: {actions: {onRun: {command: echo}}}\n",
            "    enableIf: false\n    script: {actions: {onRun: {command: echo, args: ['{{Steps.P.Output.Gpu}}']}}}\n",
        ),
        &[],
    );
    let job = f.job();
    assert_eq!(step(&job, "C").binding, StepBinding::Skipped);
    assert_eq!(step(&job, "C").input_producers(), Vec::<String>::new());
}

#[test]
fn a_deferred_cancelation_periods_inputs_are_remaining_inputs() {
    let f = Fixture::new(
        &go_gpu_count("").replace(
            "  - name: C\n",
            "  - name: D\n    dependencies: [{dependsOn: P}]\n    script: {actions: {onRun: {command: echo, cancelation: {mode: '{{\"NOTIFY_THEN_TERMINATE\"}}', notifyPeriodInSeconds: '{{Steps.P.Output.Gpu}}'}}}}\n  - name: C\n",
        ),
        &[],
    );
    let job = f.job();
    let StepBinding::PendingInputs(p) = &step(&job, "D").binding else {
        panic!("D is pending");
    };
    assert_eq!(p.remaining, waiting(&["Steps.P.Output.Gpu"]));
}

#[test]
fn a_host_requirement_may_read_steps_through_a_let() {
    let f = Fixture::new(
        &go_gpu_count(
            "    let: ['g = Steps.P.Output.Gpu']\n    hostRequirements:\n      amounts: [{name: amount.worker.gpu, min: '{{g}}'}]\n",
        ),
        &[],
    );
    let mut job = f.job();
    let StepBinding::PendingInputs(p) = &step(&job, "C").binding else {
        panic!("C is pending");
    };
    assert_eq!(p.host_requirements, waiting(&["Steps.P.Output.Gpu"]));
    let c = f
        .advance(&mut job, "C", |j| p_and_q(j, true, 3, None), 0)
        .unwrap();
    let StepBinding::PendingInputs(p) = &c.binding else {
        panic!("C is pending");
    };
    assert_eq!(p.host_requirements, GroupState::Known(Some(gpu_hr(3.0))));
}

#[test]
fn a_host_requirement_check_error_keeps_its_own_path() {
    let f = Fixture::new(
        &go_gpu_count(
            "    hostRequirements:\n      amounts: [{name: amount.worker.gpu, min: '{{Steps.P.Output.Gpu}}', max: 1}]\n",
        ),
        &[],
    );
    let mut job = f.job();
    let err = f
        .advance(&mut job, "C", |j| p_and_q(j, true, 5, None), 0)
        .unwrap_err();
    assert!(!err.is_caller_error());
    assert_eq!(
        err.to_string(),
        format!(
            "Step 'C' failed in its host selection group:\n{}",
            expected(&[(
                "steps[2] -> hostRequirements -> amounts[0]",
                "min (5) > max (1)."
            )])
        )
    );
}

#[test]
fn a_caller_that_passes_every_input_producer_is_never_refused() {
    // The scheduler loop the docs describe: on each call, pass every
    // producer in input_producers() whose state is known. M's Tasks are
    // known at creation; C's range reads them, and its host requirements
    // read Q.
    let template = format!(
        "{}steps:
  - name: M
    parameterSpace:
      taskParameterDefinitions: [{{name: F, type: INT, range: '1-3'}}]
    script: {{actions: {{onRun: {{command: echo}}}}}}
  - name: Q
    outputVariables: [{{name: Gpu, type: INT}}]
    script: {{actions: {{onRun: {{command: echo}}}}}}
  - name: C
    dependencies: [{{dependsOn: M}}, {{dependsOn: Q}}]
    parameterSpace:
      taskParameterDefinitions: [{{name: X, type: INT, range: '{{{{Steps.M.TaskParam.F}}}}'}}]
    hostRequirements:
      amounts: [{{name: amount.worker.gpu, min: '{{{{Steps.Q.Output.Gpu}}}}'}}]
    script: {{actions: {{onRun: {{command: echo}}}}}}
",
        header("[EXPR, CONTROL_FLOW, FEATURE_BUNDLE_1]")
    );
    let f = Fixture::new(&template, &[]);
    let mut job = f.job();
    let c = step(&job, "C");
    assert_eq!(c.tasks_known(), Some(3));
    assert_eq!(c.input_producers(), ["M", "Q"]);
    let c = f
        .advance(
            &mut job,
            "C",
            |j| {
                let mut inputs = StepInputs::new();
                tasks_known(&mut inputs, j, "M");
                succeeded(
                    &mut inputs,
                    j,
                    "Q",
                    &[outputs(&[("Gpu", ExprValue::Int(1))])],
                );
                inputs
            },
            0,
        )
        .unwrap();
    assert_eq!(c.binding, StepBinding::Bound);
    // Q alone is refused: M's TaskParam is an input of the known Task
    // enumeration group.
    let mut job = f.job();
    let err = f.advance(&mut job, "C", gpu_is_q, 0).unwrap_err();
    assert!(err.is_caller_error());
    assert_eq!(
        err.to_string(),
        "Binding Step 'C' was refused in its Task enumeration group:\n\
         Compatibility error: the Task enumeration group of Step 'C' was evaluated with 'Steps.M.TaskParam.F', which the given inputs do not hold; on each call, pass every producer in the Step's input_producers() whose Tasks are known or that has succeeded or been skipped."
    );
}

fn gpu_is_q(job: &openjd_model::job::Job) -> StepInputs {
    let mut inputs = StepInputs::new();
    succeeded(
        &mut inputs,
        job,
        "Q",
        &[outputs(&[("Gpu", ExprValue::Int(1))])],
    );
    inputs
}

/// A reported URI `PATH` output carries `PathFormat::Uri`. A group and a job
/// output read it in the model's evaluation format, keeping its text, so
/// neither fails with "Path format mismatch". A Windows-format stored path
/// (a reverse rule's source format) is not relabelled, since that would make
/// path operations wrong, and reading it in the model is refused.
#[test]
fn uri_path_outputs_are_readable_by_groups_and_job_outputs() {
    let f = Fixture::new(
        &format!(
            "{}jobOutputDefinitions:
- {{name: J, type: PATH, value: '{{{{Steps.P.RawOutput.U}}}}'}}
- {{name: W, type: STRING, value: '{{{{Steps.P.RawOutput.W[0].name}}}}'}}
- {{name: Wd, type: STRING, value: '{{{{Steps.P.RawOutput.Wd.name}}}}'}}
steps:
- name: P
  outputVariables: [{{name: U, type: PATH}}, {{name: W, type: 'LIST[PATH]'}}, {{name: Wd, type: PATH}}]
  script: {{actions: {{onRun: {{command: echo}}}}}}
- name: C
  dependencies: [{{dependsOn: P}}]
  parameterSpace:
    taskParameterDefinitions:
    - {{name: X, type: STRING, range: '{{{{[string(Steps.P.RawOutput.U), Steps.P.RawOutput.U.name]}}}}'}}
  script: {{actions: {{onRun: {{command: echo}}}}}}
",
            header("[EXPR, CONTROL_FLOW]")
        ),
        &[],
    );
    let mut job = f.job();
    let uri = ExprValue::new_path("s3://b/k.exr", openjd_expr::PathFormat::Uri);
    let windows = ExprValue::make_list(
        vec![ExprValue::new_path(
            r"C:\out\a.exr",
            openjd_expr::PathFormat::Windows,
        )],
        openjd_expr::ExprType::PATH,
    )
    .unwrap();
    let windows_scalar = ExprValue::new_path(r"C:\\out\\b.exr", openjd_expr::PathFormat::Windows);
    let produced = [outputs(&[
        ("U", uri),
        ("W", windows),
        ("Wd", windows_scalar),
    ])];
    let c = f
        .advance(
            &mut job,
            "C",
            |job| {
                let mut inputs = StepInputs::new();
                succeeded(&mut inputs, job, "P", &produced);
                inputs
            },
            0,
        )
        .unwrap();
    assert!(c.binding.is_bound());
    let openjd_model::job::TaskParameter::String { range } = &c
        .parameter_space
        .as_ref()
        .unwrap()
        .task_parameter_definitions["X"]
    else {
        panic!("X is a STRING parameter")
    };
    assert_eq!(range, &["s3://b/k.exr".to_string(), "k.exr".to_string()]);

    let mut inputs = StepInputs::new();
    succeeded(&mut inputs, &job, "P", &produced);
    let evaluate =
        |i| openjd_model::evaluate_job_output(&f.jt, &f.params, &f.ctx, &job, i, &inputs);
    assert_eq!(
        evaluate(0).unwrap().unwrap().to_display_string(),
        "s3://b/k.exr"
    );
    assert_eq!(
        evaluate(1).unwrap_err().to_string(),
        "Model validation error: 1 validation error for JobTemplate\n\
         jobOutputDefinitions[1] -> value:\n\
         \tJob output 'W': Path format mismatch for 'Steps.P.RawOutput.W': value has Windows \
         but evaluator uses Posix\n  \
         Steps.P.RawOutput.W[0].name\n  \
         ~~~~~~~~~~~~~~~~~~^"
    );
    // An attribute of the symbol read directly is refused too, as an
    // undefined variable.
    assert_eq!(
        evaluate(2).unwrap_err().to_string(),
        "Model validation error: 1 validation error for JobTemplate\n\
         jobOutputDefinitions[2] -> value:\n\
         \tJob output 'Wd': Undefined variable: 'Steps.P.RawOutput.Wd.name'.\n  \
         Steps.P.RawOutput.Wd.name\n  \
         ~~~~~~~~~~~~~~~~~~~~~^~~~"
    );
}

/// `Step::input_symbols` lists what a bound Step's script and Step
/// Environments read and, while it is pending, what its groups wait on;
/// nothing for a skipped Step.
#[test]
fn input_symbols_cover_scripts_and_waiting_groups() {
    let f = Fixture::new(
        &format!(
            "{}steps:
- name: P
  outputVariables: [{{name: Num, type: INT}}, {{name: Mx, type: INT}}]
  script: {{actions: {{onRun: {{command: echo}}}}}}
- name: C
  dependencies: [{{dependsOn: P}}]
  parameterSpace:
    taskParameterDefinitions:
    - {{name: F, type: INT, range: '{{{{range(Steps.P.Output.Num)}}}}'}}
  stepEnvironments:
  - {{name: E, variables: {{V: '{{{{Steps.P.Output.Mx}}}}'}}}}
  script: {{actions: {{onRun: {{command: 'echo {{{{Steps.P.RawOutput.Mx}}}}'}}}}}}
- name: S
  enableIf: '{{{{1 > 2}}}}'
  dependencies: [{{dependsOn: P}}]
  script: {{actions: {{onRun: {{command: 'echo {{{{Steps.P.Output.Mx}}}}'}}}}}}
",
            header("[EXPR, CONTROL_FLOW]")
        ),
        &[],
    );
    let mut job = f.job();
    assert_eq!(
        step(&job, "C").input_symbols(),
        [
            "Steps.P.Output.Mx",
            "Steps.P.Output.Num",
            "Steps.P.RawOutput.Mx"
        ]
    );
    assert!(step(&job, "S").input_symbols().is_empty());
    f.advance(
        &mut job,
        "C",
        |job| {
            let mut inputs = StepInputs::new();
            succeeded(
                &mut inputs,
                job,
                "P",
                &[outputs(&[
                    ("Num", ExprValue::Int(2)),
                    ("Mx", ExprValue::Int(3)),
                ])],
            );
            inputs
        },
        0,
    )
    .unwrap();
    // Bound: only what the script and Step Environments read.
    assert_eq!(
        step(&job, "C").input_symbols(),
        ["Steps.P.Output.Mx", "Steps.P.RawOutput.Mx"]
    );
}
