// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `CONTROL_FLOW` (RFC 0010) step input binding at job creation, the range
//! rules it brings, [`StepInputs::add_producer`], and [`attach_step_inputs`].
//! The group-by-group progress of a pending Step is in
//! `test_control_flow_progressive.rs`.

use openjd_expr::path_mapping::PathFormat;
use openjd_expr::{ExprType, ExprValue};
use openjd_model::job::{
    GroupState, PathInput, ProducerState, StepBinding, StepInputs, TaskParamRange, TaskParameter,
};
use openjd_model::{attach_step_inputs, CallerLimits};

use super::control_flow_binding_support::*;

const CF: &str = "[EXPR, CONTROL_FLOW, TASK_CHUNKING, FEATURE_BUNDLE_1]";
const EXPR_ONLY: &str = "[EXPR, TASK_CHUNKING, FEATURE_BUNDLE_1]";
const ECHO: &str = "    script: {actions: {onRun: {command: echo}}}\n";

/// A template with `extensions`, `params` (block YAML under
/// `parameterDefinitions`, or empty), and `steps` (block YAML).
fn tpl(extensions: &str, params: &str, steps: &str) -> String {
    let params = if params.is_empty() {
        String::new()
    } else {
        format!("parameterDefinitions:\n{params}")
    };
    format!("{}{params}steps:\n{steps}", header(extensions))
}

/// One Step `S` with one Task parameter `X` defined by `x` (flow YAML).
fn one_param(extensions: &str, params: &str, x: &str) -> String {
    tpl(
        extensions,
        params,
        &format!("  - name: S\n    parameterSpace:\n      taskParameterDefinitions: [{x}]\n{ECHO}"),
    )
}

/// The values of the first Task parameter of Step `S`, with chunking off.
fn values(job: &openjd_model::job::Job) -> Vec<String> {
    let space = step(job, "S").parameter_space.as_ref().unwrap();
    openjd_model::StepParameterSpaceIterator::new_with_chunk_override(space, Some(1))
        .unwrap()
        .map(|set| set.values().next().unwrap().value.to_display_string())
        .collect()
}

// ══════════════════════════════════════════════════════════════
// List-literal ranges: flattening and null items (Expression Language
// §1.3.2, plan A14), with EXPR alone and with CONTROL_FLOW
// ══════════════════════════════════════════════════════════════

#[test]
fn an_int_list_item_may_be_a_list_that_is_flattened() {
    for ext in [EXPR_ONLY, CF] {
        let f = Fixture::new(
            &one_param(
                ext,
                "  - {name: L, type: 'LIST[INT]', default: [3, 4]}\n",
                "{name: X, type: INT, range: [1, '{{Param.L}}', '{{null}}', 9]}",
            ),
            &[],
        );
        assert_eq!(values(&f.job()), ["1", "3", "4", "9"], "{ext}");
    }
}

#[test]
fn a_float_list_item_may_be_a_list_that_is_flattened() {
    for ext in [EXPR_ONLY, CF] {
        let f = Fixture::new(
            &one_param(
                ext,
                "  - {name: L, type: 'LIST[FLOAT]', default: [1.5, 2.5]}\n  - {name: F, type: FLOAT, default: '0.50'}\n",
                "{name: X, type: FLOAT, range: ['{{Param.L}}', '{{Param.F}}', '{{null}}']}",
            ),
            &[],
        );
        // A scalar item keeps its written spelling.
        assert_eq!(values(&f.job()), ["1.5", "2.5", "0.50"], "{ext}");
    }
}

#[test]
fn a_chunk_int_list_item_may_be_a_format_string() {
    let f = Fixture::new(
        &one_param(
            CF,
            "  - {name: Num, type: INT, default: 7}\n",
            "{name: X, type: 'CHUNK[INT]', range: [1, '{{Param.Num}}'], chunks: {defaultTaskCount: 1, rangeConstraint: NONCONTIGUOUS}}",
        ),
        &[],
    );
    assert_eq!(values(&f.job()), ["1", "7"]);
}

#[test]
fn an_int_list_item_format_string_without_expr() {
    // Template Schemas §3.4.1.1: a <TaskParameterStringValue> item must
    // resolve to the string form of an integer.
    let t = |n: &str| {
        one_param(
            "[]",
            &format!("  - {{name: Num, type: STRING, default: '{n}'}}\n"),
            "{name: X, type: INT, range: [1, '{{Param.Num}}']}",
        )
        .replace("extensions: []\n", "")
    };
    assert_eq!(values(&Fixture::new(&t("12"), &[]).job()), ["1", "12"]);
    let err = Fixture::new(&t("x"), &[])
        .create()
        .expect_err("x is no integer");
    assert_eq!(
        err.to_string(),
        "Expression error: Cannot parse 'x' as integer"
    );
}

#[test]
fn an_int_list_item_without_braces_must_be_an_integer() {
    let value: serde_json::Value = serde_saphyr::from_str(&one_param(
        CF,
        "",
        "{name: X, type: INT, range: [1, 'two']}",
    ))
    .unwrap();
    let err = openjd_model::decode_job_template(value, Some(SUPPORTED), &CallerLimits::default())
        .expect_err("'two' is not an <intstring>");
    assert_eq!(
        err.to_string(),
        "Validation error: 'jobtemplate-2023-09' failed checks: Cannot parse 'two' as integer"
    );
}

#[test]
fn an_int_list_item_format_string_is_checked_at_validation() {
    let value: serde_json::Value = serde_saphyr::from_str(&one_param(
        CF,
        "",
        "{name: X, type: INT, range: [1, '{{Param.Nope}}']}",
    ))
    .unwrap();
    let err = openjd_model::decode_job_template(value, Some(SUPPORTED), &CallerLimits::default())
        .expect_err("undefined symbol");
    assert_eq!(
        err.to_string(),
        expected(&[(
            "steps[0] -> parameterSpace -> taskParameterDefinitions[0] -> range[1]",
            "Failed to parse interpolation expression at [0, 14]. Undefined variable: 'Param.Nope'.\n  Param.Nope\n  ~~~~~~^~~~",
        )])
    );
}

#[test]
fn a_list_literal_that_flattens_to_nothing() {
    // An error with EXPR alone; with CONTROL_FLOW the result may be empty
    // (RFC 0010 §Empty parameter spaces), so the Step has zero Tasks.
    let params = "  - {name: L, type: 'LIST[INT]', default: []}\n  - {name: S, type: 'LIST[STRING]', default: []}\n  - {name: F, type: 'LIST[FLOAT]', default: []}\n";
    for x in [
        "{name: X, type: INT, range: ['{{Param.L}}']}",
        "{name: X, type: FLOAT, range: ['{{Param.F}}']}",
        "{name: X, type: STRING, range: ['{{Param.S}}']}",
        "{name: X, type: PATH, range: ['{{Param.S}}']}",
    ] {
        let err = Fixture::new(&one_param(EXPR_ONLY, params, x), &[])
            .create()
            .expect_err(x);
        assert_eq!(
            err.to_string(),
            "Validation error: Task parameter 'X' range has no elements after resolution",
            "{x}"
        );
        let job = Fixture::new(&one_param(CF, params, x), &[]).job();
        assert_eq!(step(&job, "S").tasks_known(), Some(0), "{x}");
        assert_eq!(step(&job, "S").binding, StepBinding::Bound);
    }
}

#[test]
fn an_empty_format_string_range_is_zero_tasks() {
    for x in [
        "{name: X, type: INT, range: '{{[]}}'}",
        "{name: X, type: FLOAT, range: '{{[]}}'}",
        "{name: X, type: STRING, range: '{{[]}}'}",
    ] {
        let job = Fixture::new(&one_param(CF, "", x), &[]).job();
        assert_eq!(step(&job, "S").tasks_known(), Some(0), "{x}");
    }
}

#[test]
fn empty_associated_ranges() {
    let steps = |b: &str| {
        format!(
            "  - name: S
    parameterSpace:
      taskParameterDefinitions:
        - {{name: A, type: INT, range: '{{{{[]}}}}'}}
        - {{name: B, type: STRING, range: {b}}}
      combination: '(A, B)'
{ECHO}"
        )
    };
    let job = Fixture::new(&tpl(CF, "", &steps("'{{[]}}'")), &[]).job();
    assert_eq!(step(&job, "S").tasks_known(), Some(0));
    let err = Fixture::new(&tpl(CF, "", &steps("[a, b]")), &[])
        .create()
        .expect_err("0 and 2 values");
    assert_eq!(
        err.to_string(),
        "Validation error: Associative combination: all members must have the same number of values, got 0 and 2"
    );
}

#[test]
fn the_1024_cap_is_lifted_for_computed_ranges() {
    let x = "{name: X, type: INT, range: '{{range(2000)}}'}";
    let err = Fixture::new(&one_param(EXPR_ONLY, "", x), &[])
        .create()
        .expect_err("over 1024 without CONTROL_FLOW");
    assert_eq!(
        err.to_string(),
        "Validation error: Task parameter 'X' range exceeds 1024 elements (2000 elements)"
    );
    let job = Fixture::new(&one_param(CF, "", x), &[]).job();
    assert_eq!(step(&job, "S").tasks_known(), Some(2000));
    // A list literal stays capped as written.
    let items: Vec<String> = (0..1025).map(|i| i.to_string()).collect();
    let literal = format!("{{name: X, type: INT, range: [{}]}}", items.join(", "));
    let value: serde_json::Value = serde_saphyr::from_str(&one_param(CF, "", &literal)).unwrap();
    let err = openjd_model::decode_job_template(value, Some(SUPPORTED), &CallerLimits::default())
        .expect_err("1025 items as written");
    assert_eq!(
        err.to_string(),
        expected(&[(
            "steps[0] -> parameterSpace -> taskParameterDefinitions[0]",
            "INT parameter 'X' range exceeds 1024 elements.",
        )])
    );
}

#[test]
fn a_context_enabling_control_flow_does_not_change_a_template_without_it() {
    // The relaxations are keyed on the template's extensions (RFC 0010
    // Technical Requirement 1), not on a wider context.
    let x = "{name: X, type: INT, range: '{{range(2000)}}'}";
    let jt = decode(&one_param(EXPR_ONLY, "", x));
    let ctx = openjd_model::ValidationContext::with_extensions(
        openjd_model::SpecificationRevision::V2023_09,
        openjd_model::ModelExtension::ALL.iter().copied().collect(),
    );
    let err = openjd_model::create_job(&jt, &params(&jt, &[]), &ctx).expect_err("still capped");
    assert_eq!(
        err.to_string(),
        "Validation error: Task parameter 'X' range exceeds 1024 elements (2000 elements)"
    );
}

// ══════════════════════════════════════════════════════════════
// Distinct CHUNK[INT] values in a Step with outputs
// ══════════════════════════════════════════════════════════════

fn chunked(outputs: bool, range: &str) -> String {
    let ov = if outputs {
        "    outputVariables: [{name: T, type: FLOAT}]\n"
    } else {
        ""
    };
    format!(
        "{}parameterDefinitions: [{{name: One, type: INT, default: 1}}]
steps:
  - name: P
    outputVariables: [{{name: L, type: 'LIST[INT]'}}]
{ECHO}  - name: S
    dependencies: [{{dependsOn: P}}]
{ov}    parameterSpace:
      taskParameterDefinitions:
        - {{name: F, type: 'CHUNK[INT]', range: {range}, chunks: {{defaultTaskCount: 2, rangeConstraint: NONCONTIGUOUS}}}}
{ECHO}",
        header(CF)
    )
}

#[test]
fn a_computed_chunk_range_repeating_a_value_fails_job_creation() {
    let msg = "Validation error: Task parameter 'F' range must have distinct values in a Step with outputVariables, so that each value names one Task; value 1 appears more than once";
    for range in ["'{{[1, 1, 2]}}'", "[1, '{{Param.One}}', 2]"] {
        let err = Fixture::new(&chunked(true, range), &[])
            .create()
            .expect_err(range);
        assert_eq!(err.to_string(), msg, "{range}");
        // Without outputs, a repeated value is allowed.
        let job = Fixture::new(&chunked(false, range), &[]).job();
        assert_eq!(step(&job, "S").tasks_known(), Some(3), "{range}");
    }
}

#[test]
fn a_chunk_range_repeating_values_at_binding_fails_the_step() {
    let f = Fixture::new(&chunked(true, "'{{Steps.P.Output.L}}'"), &[]);
    let mut job = f.job();
    let err = f
        .advance(
            &mut job,
            "S",
            |j| {
                let mut inputs = StepInputs::new();
                let l =
                    ExprValue::make_list([3, 2, 3, 2].map(ExprValue::Int).to_vec(), ExprType::INT)
                        .unwrap();
                succeeded(&mut inputs, j, "P", &[outputs(&[("L", l)])]);
                inputs
            },
            0,
        )
        .expect_err("3 and 2 repeat");
    assert_eq!(
        err.to_string(),
        "Step 'S' failed in its Task enumeration group:\n\
         Validation error: Task parameter 'F' range must have distinct values in a Step with outputVariables, so that each value names one Task; values 2, 3 appear more than once"
    );
}

// ══════════════════════════════════════════════════════════════
// Job creation order and error reporting
// ══════════════════════════════════════════════════════════════

#[test]
fn a_consumer_of_task_params_before_its_producer_binds_at_creation() {
    let f = Fixture::new(
        &tpl(
            CF,
            "",
            &format!(
                "  - name: C
    dependencies: [{{dependsOn: P}}]
    parameterSpace:
      taskParameterDefinitions: [{{name: X, type: INT, range: '{{{{Steps.P.TaskParam.F}}}}'}}]
{ECHO}  - name: P
    parameterSpace:
      taskParameterDefinitions: [{{name: F, type: INT, range: '4-6'}}]
{ECHO}"
            ),
        ),
        &[],
    );
    let job = f.job();
    assert_eq!(job.steps[0].name, "C", "Steps stay in template order");
    assert_eq!(step(&job, "C").binding, StepBinding::Bound);
    let space = step(&job, "C").parameter_space.as_ref().unwrap();
    assert_eq!(
        space.task_parameter_definitions["X"],
        TaskParameter::Int {
            range: TaskParamRange::List(vec![4, 5, 6]),
            chunks: None
        }
    );
}

/// `A`'s `let` (read by its range) fails, and both Steps have a Step
/// Environment variable over the limit.
fn failed_let_template(extensions: &str) -> String {
    tpl(
        extensions,
        "  - {name: X, type: STRING}\n",
        &format!(
            "  - name: A
    let: ['q = 1 / int(Param.X)']
    parameterSpace:
      taskParameterDefinitions: [{{name: F, type: INT, range: '1-{{{{ q }}}}'}}]
    stepEnvironments: [{{name: E, variables: {{V: '{{{{ Param.X * 3000 }}}}'}}}}]
{ECHO}  - name: B
    stepEnvironments: [{{name: E, variables: {{V: '{{{{ Param.X * 3000 }}}}'}}}}]
{ECHO}"
        ),
    )
}

#[test]
fn a_failed_let_stops_its_step_but_other_steps_are_still_checked() {
    let over = "resolves to at least 3000 characters, exceeding the maximum of 2048.";
    // Without CONTROL_FLOW, A's own Environment is still checked.
    let err = Fixture::new(&failed_let_template(EXPR_ONLY), &[("X", "0")])
        .create()
        .expect_err("expected failures");
    let div = "Invalid expression in let binding 'q': Division by zero\n  q = 1 / int(Param.X)\n      ~~^~~~~~~~~~~~~~";
    assert_eq!(
        err.to_string(),
        expected(&[
            ("steps[0] -> let[0]", div),
            ("steps[0] -> stepEnvironments[0] -> variables -> V", over),
            ("steps[1] -> stepEnvironments[0] -> variables -> V", over),
        ])
    );
    // With CONTROL_FLOW, A stops at its Task enumeration group, whose `let`
    // failed, so its Environment (the remaining group) is not evaluated.
    let err = Fixture::new(&failed_let_template(CF), &[("X", "0")])
        .create()
        .expect_err("expected failures");
    assert_eq!(
        err.to_string(),
        expected(&[
            ("steps[0] -> let[0]", div),
            ("steps[1] -> stepEnvironments[0] -> variables -> V", over),
        ])
    );
}

#[test]
fn a_pending_steps_remaining_group_errors_are_reported_with_other_steps() {
    // C's range waits for P, but its script's timeout reads only a job
    // parameter, so its remaining group is evaluated at creation and fails.
    let f = Fixture::new(
        &tpl(
            CF,
            "  - {name: Z, type: INT, default: 0}\n",
            &format!(
                "  - name: P
    outputVariables: [{{name: Count, type: INT}}]
{ECHO}  - name: C
    dependencies: [{{dependsOn: P}}]
    parameterSpace:
      taskParameterDefinitions: [{{name: X, type: INT, range: '{{{{range(Steps.P.Output.Count)}}}}'}}]
    script: {{actions: {{onRun: {{command: echo, timeout: '{{{{Param.Z}}}}'}}}}}}
  - name: D
    script: {{actions: {{onRun: {{command: echo, timeout: '{{{{Param.Z}}}}'}}}}}}
"
            ),
        ),
        &[],
    );
    let err = f.create().expect_err("both timeouts are 0");
    assert_eq!(
        err.to_string(),
        expected(&[
            (
                "steps[1] -> script -> actions -> onRun -> timeout",
                "timeout must be > 0."
            ),
            (
                "steps[2] -> script -> actions -> onRun -> timeout",
                "timeout must be > 0."
            ),
        ])
    );
}

/// `S0` depends on `S1`. `S0` has a bad host requirement, `S1` a bad range.
fn bad_host_then_bad_range(extensions: &str) -> String {
    tpl(
        extensions,
        "  - {name: Z, type: INT, default: 0}\n",
        &format!(
            "  - name: S0
    dependencies: [{{dependsOn: S1}}]
    hostRequirements:
      amounts: [{{name: amount.worker.vcpu, min: '{{{{1 // Param.Z}}}}'}}]
{ECHO}  - name: S1
    parameterSpace:
      taskParameterDefinitions: [{{name: X, type: INT, range: '{{{{[1 // Param.Z]}}}}'}}]
{ECHO}"
        ),
    )
}

#[test]
fn which_resolution_error_aborts_create_job() {
    // Without CONTROL_FLOW, Steps are instantiated in template order, so
    // S0's host requirement is reported.
    let err = Fixture::new(&bad_host_then_bad_range(EXPR_ONLY), &[])
        .create()
        .expect_err("S0 fails first");
    assert_eq!(
        err.to_string(),
        "Format string error: hostRequirements amount min: Division by zero\n  1 // Param.Z\n  ~~^~~~~~~~~~"
    );
    // With CONTROL_FLOW, Steps are bound in dependency order, so S1's range
    // is reported.
    let err = Fixture::new(&bad_host_then_bad_range(CF), &[])
        .create()
        .expect_err("S1 fails first");
    assert_eq!(
        err.to_string(),
        "Expression error: Division by zero\n  [1 // Param.Z]\n   ~~^~~~~~~~~~"
    );
}

#[test]
fn max_task_count_counts_the_steps_whose_tasks_are_known_at_creation() {
    // P has 3 Tasks; C's are not known at creation, so they do not count.
    let template = tpl(
        CF,
        "",
        &format!(
            "  - name: P
    parameterSpace:
      taskParameterDefinitions: [{{name: F, type: INT, range: '1-3'}}]
    outputVariables: [{{name: Count, type: 'LIST[INT]'}}]
{ECHO}  - name: C
    dependencies: [{{dependsOn: P}}]
    parameterSpace:
      taskParameterDefinitions: [{{name: X, type: INT, range: '{{{{Steps.P.Output.Count}}}}'}}]
{ECHO}  - name: D
    parameterSpace:
      taskParameterDefinitions: [{{name: G, type: INT, range: '1-2'}}]
{ECHO}"
        ),
    );
    let limits = |n| CallerLimits {
        max_task_count: Some(n),
        ..Default::default()
    };
    let job = Fixture::with_limits(&template, &[], limits(5)).job();
    assert_eq!(step(&job, "C").tasks_known(), None);
    let err = Fixture::with_limits(&template, &[], limits(4))
        .create()
        .expect_err("3 + 2 > 4");
    assert_eq!(
        err.to_string(),
        "Model validation error: 1 validation error for Template\n\
         Template: Total task count (5) exceeds caller limit of 4: Step 'D' has 2 Tasks, and the Steps whose Tasks are known before it have 3."
    );
}

#[test]
fn max_step_task_count_without_control_flow() {
    let template = one_param(EXPR_ONLY, "", "{name: X, type: INT, range: '1-5'}");
    let err = Fixture::with_limits(
        &template,
        &[],
        CallerLimits {
            max_step_task_count: Some(4),
            ..Default::default()
        },
    )
    .create()
    .expect_err("5 > 4");
    assert_eq!(
        err.to_string(),
        "Model validation error: 1 validation error for Template\n\
         Template: Step 'S' has 5 Tasks, which exceeds the caller limit of 4 Tasks per Step."
    );
}

#[test]
fn a_pending_job_round_trips_through_serde() {
    let template = tpl(
        CF,
        "",
        &format!(
            "  - name: P
    outputVariables: [{{name: G, type: INT}}]
{ECHO}  - name: C
    dependencies: [{{dependsOn: P}}]
    parameterSpace:
      taskParameterDefinitions: [{{name: X, type: FLOAT, range: [1.50, 2]}}]
    hostRequirements:
      amounts: [{{name: amount.worker.gpu, min: '{{{{Steps.P.Output.G}}}}'}}]
{ECHO}"
        ),
    );
    let job = Fixture::new(&template, &[]).job();
    let StepBinding::PendingInputs(pending) = &step(&job, "C").binding else {
        panic!("C is pending");
    };
    assert!(matches!(pending.tasks, GroupState::Known(Some(_))));
    let json = serde_json::to_value(&job.steps[1]).unwrap();
    let back: openjd_model::job::Step = serde_json::from_value(json).unwrap();
    assert_eq!(back, job.steps[1]);
}

// ══════════════════════════════════════════════════════════════
// StepInputs::add_producer
// ══════════════════════════════════════════════════════════════

/// Producer `P` with `space` (block YAML under `parameterSpace`, or empty)
/// and `outputs` (flow YAML), and consumer `C` reading nothing.
fn producer(space: &str, outputs: &str) -> openjd_model::job::Job {
    let space = if space.is_empty() {
        String::new()
    } else {
        format!("    parameterSpace:\n{space}")
    };
    Fixture::new(
        &tpl(
            CF,
            "",
            &format!(
                "  - name: P\n{space}    outputVariables: {outputs}\n{ECHO}  - name: C\n    dependencies: [{{dependsOn: P}}]\n{ECHO}"
            ),
        ),
        &[],
    )
    .job()
}

#[test]
fn a_single_task_producers_output_is_a_scalar() {
    let job = producer("", "[{name: Num, type: INT}, {name: S, type: STRING}]");
    let mut inputs = StepInputs::new();
    succeeded(
        &mut inputs,
        &job,
        "P",
        &[outputs(&[
            ("Num", ExprValue::Int(3)),
            ("S", ExprValue::String("s".into())),
        ])],
    );
    assert_eq!(inputs.get("Steps.P.Output.Num"), Some(&ExprValue::Int(3)));
    assert_eq!(
        inputs.get("Steps.P.RawOutput.Num"),
        Some(&ExprValue::Int(3))
    );
    assert_eq!(
        inputs.get("Steps.P.Output.S"),
        Some(&ExprValue::String("s".into()))
    );
    assert_eq!(inputs.get("Steps.P.TaskParam.F"), None);
    assert_eq!(inputs.producers().collect::<Vec<_>>(), ["P"]);
}

#[test]
fn task_params_and_outputs_align_in_iteration_order() {
    // `combination: A * B`: B varies fastest.
    let job = producer(
        "      taskParameterDefinitions:
        - {name: A, type: INT, range: [1, 2]}
        - {name: B, type: STRING, range: [xa, yb]}
      combination: 'A * B'
",
        "[{name: T, type: FLOAT}]",
    );
    let task_outputs: Vec<_> = (0..4)
        .map(|i| outputs(&[("T", ExprValue::Int(i))]))
        .collect();
    let mut inputs = StepInputs::new();
    succeeded(&mut inputs, &job, "P", &task_outputs);
    let show = |s: &str| inputs.get(s).unwrap().to_display_string();
    assert_eq!(show("Steps.P.TaskParam.A"), "[1, 1, 2, 2]");
    assert_eq!(show("Steps.P.TaskRawParam.A"), "[1, 1, 2, 2]");
    assert_eq!(
        show("Steps.P.TaskParam.B"),
        "[\"xa\", \"yb\", \"xa\", \"yb\"]"
    );
    assert_eq!(show("Steps.P.Output.T"), "[0.0, 1.0, 2.0, 3.0]");
    assert_eq!(
        inputs.get("Steps.P.Output.T").unwrap().expr_type(),
        ExprType::list(ExprType::FLOAT)
    );
}

#[test]
fn a_chunk_int_task_param_is_one_int_per_task() {
    let job = producer(
        "      taskParameterDefinitions:
        - {name: F, type: 'CHUNK[INT]', range: [5, 1, 3, 2], chunks: {defaultTaskCount: 2, rangeConstraint: NONCONTIGUOUS}}
",
        "[{name: T, type: INT}]",
    );
    let mut inputs = StepInputs::new();
    tasks_known(&mut inputs, &job, "P");
    assert_eq!(
        inputs.get("Steps.P.TaskParam.F"),
        Some(
            &ExprValue::make_list([5, 1, 3, 2].map(ExprValue::Int).to_vec(), ExprType::INT)
                .unwrap()
        )
    );
    assert_eq!(
        inputs.get("Steps.P.Output.T"),
        None,
        "outputs are not known yet"
    );
    assert!(!inputs.is_concrete("Steps.P.Output.T"));
}

#[test]
fn a_zero_task_producers_lists_are_empty_and_typed() {
    let job = producer(
        "      taskParameterDefinitions: [{name: F, type: INT, range: '{{[]}}'}]\n",
        "[{name: L, type: 'LIST[INT]'}]",
    );
    let mut inputs = StepInputs::new();
    succeeded(&mut inputs, &job, "P", &[]);
    let l = inputs.get("Steps.P.Output.L").unwrap();
    assert_eq!(l.expr_type(), ExprType::list(ExprType::list(ExprType::INT)));
    assert_eq!(l.list_len(), Some(0));
    assert_eq!(
        inputs.get("Steps.P.TaskParam.F").unwrap().expr_type(),
        ExprType::list(ExprType::INT)
    );
}

#[test]
fn a_path_value_is_held_only_in_its_raw_form() {
    let job = producer(
        "      taskParameterDefinitions: [{name: D, type: PATH, range: ['/a', '/b']}]\n",
        "[{name: F, type: PATH}]",
    );
    let mut inputs = StepInputs::new();
    let f = |p: &str| outputs(&[("F", ExprValue::new_path(p, PathFormat::Posix))]);
    succeeded(&mut inputs, &job, "P", &[f("/x"), f("/y")]);
    for (raw, mapped) in [
        ("Steps.P.RawOutput.F", "Steps.P.Output.F"),
        ("Steps.P.TaskRawParam.D", "Steps.P.TaskParam.D"),
    ] {
        assert_eq!(
            inputs.get(raw).unwrap().expr_type(),
            ExprType::list(ExprType::PATH),
            "{raw}"
        );
        assert_eq!(
            inputs.get(mapped),
            None,
            "{mapped} is derived on the worker"
        );
        assert!(inputs.is_concrete(mapped));
    }
}

#[test]
fn add_producer_refuses_a_state_that_does_not_match() {
    let job = producer(
        "      taskParameterDefinitions: [{name: F, type: INT, range: '1-2'}]\n",
        "[{name: Num, type: INT}]",
    );
    let mut inputs = StepInputs::new();
    let err = |r: Result<(), openjd_model::ModelError>| r.unwrap_err().to_string();
    let p = step(&job, "P");
    assert_eq!(
        err(inputs.add_producer(p, ProducerState::Skipped)),
        "Validation error: Step 'P' is not skipped, so it cannot be added as a skipped producer."
    );
    assert_eq!(
        err(inputs.add_producer(p, ProducerState::Succeeded { task_outputs: &[] })),
        "Validation error: Step 'P' has 2 Tasks, but outputs were given for 0."
    );
    let one = outputs(&[("Num", ExprValue::Int(1))]);
    assert_eq!(
        err(inputs.add_producer(
            p,
            ProducerState::Succeeded {
                task_outputs: &[one.clone(), outputs(&[])]
            }
        )),
        "Validation error: Task 1 of Step 'P' has no value for output 'Num'."
    );
    assert_eq!(
        err(inputs.add_producer(
            p,
            ProducerState::Succeeded {
                task_outputs: &[one, outputs(&[("Num", ExprValue::String("x".into()))])]
            }
        )),
        "Validation error: Task 1 of Step 'P': output 'Num' does not convert to INT: Cannot convert 'x' to int: invalid digit found in string"
    );
    let mut pending = p.clone();
    pending.binding = StepBinding::PendingInputs(openjd_model::job::PendingBinding {
        enablement: GroupState::Known(()),
        tasks: GroupState::Waiting { on: vec![] },
        host_requirements: GroupState::Known(None),
        remaining: GroupState::Known(()),
    });
    assert_eq!(
        err(inputs.add_producer(&pending, ProducerState::TasksKnown)),
        "Validation error: the Tasks of Step 'P' are not known, so it cannot be added as a producer whose Tasks are known."
    );
    assert_eq!(
        err(inputs.add_producer(&pending, ProducerState::Succeeded { task_outputs: &[] })),
        "Validation error: Step 'P' is not bound, so it cannot have succeeded."
    );
    assert_eq!(
        inputs.producers().count(),
        0,
        "a refused producer is not added"
    );
}

// ══════════════════════════════════════════════════════════════
// attach_step_inputs
// ══════════════════════════════════════════════════════════════

fn attach_template() -> String {
    tpl(
        CF,
        "",
        &format!(
            "  - name: P
    outputVariables: [{{name: Num, type: INT}}, {{name: F, type: PATH}}]
{ECHO}  - name: M
    parameterSpace:
      taskParameterDefinitions: [{{name: I, type: INT, range: [1, 2]}}]
    outputVariables: [{{name: Ds, type: 'LIST[PATH]'}}]
{ECHO}  - name: Z
    enableIf: false
    outputVariables: [{{name: V, type: INT}}]
{ECHO}  - name: C
    dependencies: [{{dependsOn: P}}, {{dependsOn: M}}, {{dependsOn: Z}}]
    stepEnvironments: [{{name: E, variables: {{D: '{{{{Steps.M.Output.Ds[0][0]}}}}'}}}}]
    script:
      actions:
        onRun:
          command: echo
          args: ['{{{{Steps.P.Output.Num}}}}', '{{{{Steps.P.Output.F}}}}', '{{{{Steps.P.RawOutput.F.name}}}}', '{{{{Steps.Z.Output.V}}}}']
"
        ),
    )
}

fn attach_inputs(job: &openjd_model::job::Job) -> StepInputs {
    let mut inputs = StepInputs::new();
    succeeded(
        &mut inputs,
        job,
        "P",
        &[outputs(&[
            ("Num", ExprValue::Int(4)),
            ("F", ExprValue::new_path("/s/f", PathFormat::Posix)),
        ])],
    );
    let ds = |p: &str| {
        outputs(&[(
            "Ds",
            ExprValue::make_list(
                vec![ExprValue::new_path(p, PathFormat::Posix)],
                ExprType::PATH,
            )
            .unwrap(),
        )])
    };
    succeeded(&mut inputs, job, "M", &[ds("/a"), ds("/b")]);
    inputs
        .add_producer(step(job, "Z"), ProducerState::Skipped)
        .unwrap();
    inputs
}

#[test]
fn attach_step_inputs_stores_what_the_script_and_environments_read() {
    let job = Fixture::new(&attach_template(), &[]).job();
    let c = step(&job, "C");
    assert_eq!(
        c.binding,
        StepBinding::Bound,
        "C reads Steps.* only in host fields"
    );
    assert_eq!(c.input_producers(), ["M", "P", "Z"]);
    let attached = attach_step_inputs(c, &attach_inputs(&job)).unwrap();
    let symtab = attached
        .resolved_symtab
        .as_ref()
        .unwrap()
        .to_symtab(PathFormat::Posix)
        .unwrap();
    assert_eq!(
        symtab.get_value("Steps.P.Output.Num"),
        Some(&ExprValue::Int(4))
    );
    // A path-typed Output is stored as its Raw twin, as a string.
    assert_eq!(symtab.get_value("Steps.P.Output.F"), None);
    assert_eq!(
        symtab.get_value("Steps.P.RawOutput.F"),
        Some(&ExprValue::String("/s/f".into()))
    );
    assert_eq!(
        symtab
            .get_value("Steps.M.RawOutput.Ds")
            .unwrap()
            .to_display_string(),
        "[[\"/a\"], [\"/b\"]]"
    );
    assert_eq!(symtab.get_value("Steps.Z.Output.V"), Some(&ExprValue::Null));
    assert_eq!(
        attached.path_inputs,
        vec![
            PathInput {
                symbol: "Steps.M.RawOutput.Ds".into(),
                ty: ExprType::list(ExprType::list(ExprType::PATH)),
            },
            PathInput {
                symbol: "Steps.P.RawOutput.F".into(),
                ty: ExprType::PATH,
            },
        ]
    );
    // Attaching again gives the same Step.
    assert_eq!(
        attach_step_inputs(&attached, &attach_inputs(&job)).unwrap(),
        attached
    );
}

#[test]
fn attach_step_inputs_names_a_symbol_that_is_not_concrete() {
    let job = Fixture::new(&attach_template(), &[]).job();
    let mut inputs = StepInputs::new();
    tasks_known(&mut inputs, &job, "M");
    let err = attach_step_inputs(step(&job, "C"), &inputs).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Validation error: Step 'C' reads 'Steps.M.Output.Ds', which is not concrete in the given inputs."
    );
}

#[test]
fn attach_step_inputs_needs_a_bound_step() {
    let job = Fixture::new(&attach_template(), &[]).job();
    let err = attach_step_inputs(step(&job, "Z"), &StepInputs::new()).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Validation error: Step 'Z' is not bound, so its Steps.* inputs cannot be attached."
    );
}
