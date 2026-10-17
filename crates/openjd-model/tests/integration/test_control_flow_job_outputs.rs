// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `CONTROL_FLOW` (RFC 0010) job outputs: the `jobOutputDefinitions` schema
//! and its structural rules (pass 11), the job output scope and typing of
//! `value` (pass 8), evaluation at Job creation, and `job_output_inputs` /
//! `evaluate_job_output`.
//!
//! Failure tests assert the full error output (count, path, and message), as
//! `AGENTS.md` § Test Quality Standard requires.

use openjd_expr::ExprValue;
use openjd_model::job::{JobOutputValue, OutputBound, ProducerState, StepBinding, StepInputs};
use openjd_model::{
    decode_job_template, evaluate_job_output, job_output_inputs, CallerLimits, JobParameterType,
    ModelError,
};

use crate::control_flow_binding_support::{
    expected, header, outputs, step, succeeded, tasks_known, Fixture, SUPPORTED,
};

const STEP_S: &str = "steps:\n  - name: S\n    script: {actions: {onRun: {command: x}}}\n";

fn decode(template: &str) -> Result<openjd_model::template::JobTemplate, ModelError> {
    let value: serde_json::Value = serde_saphyr::from_str(template).unwrap();
    decode_job_template(value, Some(SUPPORTED), &CallerLimits::default())
}

/// A template with `extensions`, the `jobOutputDefinitions` lines `outputs`
/// (YAML at the top level), and `rest` (parameters and Steps).
fn template_with(extensions: &str, outputs: &str, rest: &str) -> String {
    format!(
        "{}jobOutputDefinitions:\n{outputs}{rest}",
        header(extensions)
    )
}

/// `template_with` with `[EXPR, CONTROL_FLOW]`.
fn template(outputs: &str, rest: &str) -> String {
    template_with("[EXPR, CONTROL_FLOW]", outputs, rest)
}

fn assert_errs(template: &str, errors: &[(&str, &str)]) {
    match decode(template) {
        Ok(_) => panic!("expected a validation error\n\n{template}"),
        Err(e) => assert_eq!(e.to_string(), expected(errors), "\n\n{template}"),
    }
}

fn assert_decode_err(template: &str, message: &str) {
    let err = decode(template).expect_err("expected a decode error");
    assert_eq!(
        err.to_string(),
        format!("Validation error: 'jobtemplate-2023-09' failed checks: {message}")
    );
}

fn assert_ok(template: &str) {
    if let Err(e) = decode(template) {
        panic!("expected the template to decode, got:\n{e}\n\n{template}");
    }
}

// ── Schema and structure (pass 11) ─────────────────────────────────

#[test]
fn job_output_definitions_require_the_extension() {
    assert_errs(
        &template_with(
            "[EXPR]",
            "  - {name: A, type: INT, value: '{{1}}'}\n",
            STEP_S,
        ),
        &[(
            "jobOutputDefinitions",
            "jobOutputDefinitions requires the CONTROL_FLOW extension.",
        )],
    );
}

#[test]
fn the_definition_is_parsed_into_its_parts() {
    let jt = decode(&template(
        "  - {name: Dir, type: PATH, value: '{{RawParam.Out}}', description: Results, unit: dirs, objectType: DIRECTORY, minLength: 2}\n",
        &format!("parameterDefinitions:\n  - {{name: Out, type: PATH, default: out}}\n{STEP_S}"),
    ))
    .unwrap();
    let def = &jt.job_output_definitions_list()[0];
    assert_eq!(def.name(), "Dir");
    assert_eq!(def.job_param_type(), JobParameterType::Path);
    assert_eq!(def.value().raw(), "{{RawParam.Out}}");
    assert_eq!(def.description(), Some("Results"));
    assert_eq!(def.unit(), Some("dirs"));
    assert_eq!(def.constraints().min_length, Some(2));
    let job_output = def.to_job_output();
    assert_eq!(job_output.declaration.name, "Dir");
    assert_eq!(job_output.declaration.unit.as_deref(), Some("dirs"));
    assert_eq!(job_output.description.as_deref(), Some("Results"));
    assert_eq!(job_output.value.raw(), "{{RawParam.Out}}");
    assert_eq!(job_output.evaluated, None);
}

#[test]
fn an_empty_list_is_rejected() {
    assert_errs(
        &format!(
            "{}jobOutputDefinitions: []\n{STEP_S}",
            header("[EXPR, CONTROL_FLOW]")
        ),
        &[(
            "jobOutputDefinitions",
            "if provided, must contain at least one element.",
        )],
    );
}

#[test]
fn fifty_outputs_are_accepted_and_fifty_one_rejected() {
    let lines = |n: usize| {
        (1..=n)
            .map(|i| format!("  - {{name: O{i}, type: INT, value: '{{{{{i}}}}}'}}\n"))
            .collect::<String>()
    };
    assert_ok(&template(&lines(50), STEP_S));
    assert_errs(
        &template(&lines(51), STEP_S),
        &[(
            "jobOutputDefinitions",
            "must not contain more than 50 elements.",
        )],
    );
}

#[test]
fn names_are_unique_and_may_repeat_a_job_parameter_name() {
    assert_errs(
        &template(
            "  - {name: X, type: INT, value: '{{1}}'}\n  - {name: X, type: INT, value: '{{2}}'}\n",
            STEP_S,
        ),
        &[("jobOutputDefinitions[1]", "duplicate job output name: 'X'")],
    );
    assert_ok(&template(
        "  - {name: Count, type: INT, value: '{{Param.Count * 2}}'}\n",
        &format!("parameterDefinitions:\n  - {{name: Count, type: INT, default: 1}}\n{STEP_S}"),
    ));
}

#[test]
fn a_name_must_be_an_identifier() {
    assert_decode_err(
        &template(
            "  - {name: Frame Count, type: INT, value: '{{1}}'}\n",
            STEP_S,
        ),
        "Validation error: Identifier 'Frame Count' does not match pattern [A-Za-z_][A-Za-z0-9_]*",
    );
}

#[test]
fn collect_default_user_interface_and_data_flow_are_not_allowed() {
    assert_errs(
        &template(
            "  - {name: X, type: INT, value: '{{1}}', collect: ONCE, default: 1, userInterface: {control: SPIN_BOX}}\n  - {name: P, type: PATH, value: a, dataFlow: OUT}\n",
            STEP_S,
        ),
        &[
            (
                "jobOutputDefinitions[0] -> collect",
                "is not allowed in a job output definition.",
            ),
            (
                "jobOutputDefinitions[0] -> default",
                "is not allowed in a job output definition.",
            ),
            (
                "jobOutputDefinitions[0] -> userInterface",
                "is not allowed in a job output definition.",
            ),
            (
                "jobOutputDefinitions[1] -> dataFlow",
                "is not allowed in a job output definition.",
            ),
        ],
    );
}

#[test]
fn an_explicit_null_collect_counts_as_omitted() {
    assert_ok(&template(
        "  - {name: X, type: INT, value: '{{1}}', collect: null}\n",
        STEP_S,
    ));
}

#[test]
fn a_constraint_of_another_type_is_an_unknown_field_listing_the_job_output_fields() {
    assert_decode_err(
        &template("  - {name: S, type: STRING, value: a, minValue: 1}\n", STEP_S),
        "unknown field `minValue`, expected one of `name`, `description`, `allowedValues`, `minLength`, `maxLength`, `value`, `unit`",
    );
}

#[test]
fn constraint_properties_must_be_valid_for_the_type() {
    assert_errs(
        &template(
            "  - {name: Num, type: INT, value: '{{1}}', minValue: 5, maxValue: 1}\n",
            STEP_S,
        ),
        &[(
            "jobOutputDefinitions[0]",
            "Job output 'Num': minValue (5) > maxValue (1).",
        )],
    );
}

#[test]
fn a_value_is_required() {
    assert_decode_err(
        &template("  - {name: X, type: INT}\n", STEP_S),
        "missing 'value' field in job output definition",
    );
    assert_decode_err(
        &template("  - {name: X, type: INT, value: null}\n", STEP_S),
        "missing 'value' field in job output definition",
    );
}

#[test]
fn the_type_must_be_an_output_variable_type() {
    assert_decode_err(
        &template("  - {name: X, type: DICT, value: a}\n", STEP_S),
        "unknown job output type: 'DICT'",
    );
}

#[test]
fn unit_rules_are_those_of_an_output_variable() {
    assert_errs(
        &template(
            &format!(
                "  - {{name: A, type: INT, value: '{{{{1}}}}', unit: ''}}\n  - {{name: B, type: INT, value: '{{{{1}}}}', unit: {}}}\n  - {{name: C, type: INT, value: '{{{{1}}}}', unit: \"a\\tb\"}}\n",
                "u".repeat(33)
            ),
            STEP_S,
        ),
        &[
            ("jobOutputDefinitions[0] -> unit", "must not be empty."),
            ("jobOutputDefinitions[1] -> unit", "exceeds 32 characters."),
            (
                "jobOutputDefinitions[2] -> unit",
                "contains control characters.",
            ),
        ],
    );
    assert_ok(&template(
        &format!(
            "  - {{name: A, type: INT, value: '{{{{1}}}}', unit: {}}}\n",
            "u".repeat(32)
        ),
        STEP_S,
    ));
}

#[test]
fn list_list_int_is_allowed_from_a_multi_task_step() {
    assert_ok(&template(
        "  - {name: Grid, type: 'LIST[LIST[INT]]', value: '{{Steps.P.Output.L}}'}\n",
        "steps:
  - name: P
    parameterSpace:
      taskParameterDefinitions: [{name: F, type: INT, range: [1, 2]}]
    outputVariables: [{name: L, type: 'LIST[INT]'}]
    script: {actions: {onRun: {command: x}}}
",
    ));
}

// ── The value's scope and type (pass 8) ────────────────────────────

/// Steps for the scope tests: `P` (no parameterSpace) with INT, PATH, and
/// LIST[PATH] outputs; `Q` (parameterSpace) with PATH and INT Task
/// parameters and an INT output; `R` (enableIf) with an INT output; and
/// `Find Frames`, whose name is not an `<Identifier>`. None depends on
/// another.
const SCOPE_STEPS: &str = "parameterDefinitions:
  - {name: Out, type: PATH, default: out}
  - {name: Outs, type: 'LIST[PATH]', default: [a]}
  - {name: Title, type: STRING, default: s}
steps:
  - name: P
    outputVariables:
      - {name: Count, type: INT}
      - {name: F, type: PATH}
      - {name: Fs, type: 'LIST[PATH]'}
    script: {actions: {onRun: {command: x}}}
  - name: Q
    parameterSpace:
      taskParameterDefinitions:
        - {name: D, type: PATH, range: [/a, /b]}
        - {name: I, type: INT, range: [1, 2]}
    outputVariables: [{name: M, type: INT}]
    script: {actions: {onRun: {command: x}}}
  - name: R
    enableIf: true
    outputVariables: [{name: B, type: INT}]
    script: {actions: {onRun: {command: x}}}
  - name: Find Frames
    outputVariables: [{name: Count, type: INT}]
    script: {actions: {onRun: {command: x}}}
  - name: L
    let: [x = 1]
    script: {actions: {onRun: {command: '{{x}}'}}}
";

/// One job output of `ty` with `value`, over [`SCOPE_STEPS`].
fn scoped(ty: &str, value: &str) -> String {
    template(
        &format!("  - {{name: O, type: '{ty}', value: \"{value}\"}}\n"),
        SCOPE_STEPS,
    )
}

const VALUE: &str = "jobOutputDefinitions[0] -> value";

#[test]
fn any_steps_symbols_are_readable_without_a_dependency() {
    for (ty, value) in [
        ("INT", "{{Steps.P.Output.Count + Steps.Q.Output.M[0]}}"),
        ("PATH", "{{Steps.P.RawOutput.F}}"),
        ("LIST[PATH]", "{{Steps.P.RawOutput.Fs}}"),
        ("LIST[PATH]", "{{Steps.Q.TaskRawParam.D}}"),
        ("LIST[INT]", "{{Steps.Q.TaskParam.I}}"),
        ("LIST[INT]", "{{Steps.Q.Output.M}}"),
        // A conditional producer's T? meets the optional target type.
        ("INT", "{{Steps.R.Output.B}}"),
        ("PATH", "{{RawParam.Out}}"),
        ("LIST[STRING]", "{{RawParam.Outs}}"),
        ("STRING", "{{Job.Name}}"),
        ("STRING", "{{Param.Title}}"),
        // Text around an expression resolves to a string.
        ("STRING", "n={{Steps.P.Output.Count}}"),
    ] {
        assert_ok(&scoped(ty, value));
    }
}

#[test]
fn path_typed_steps_values_are_readable_only_through_their_raw_forms() {
    for (sym, raw) in [
        ("Steps.P.Output.F", "Steps.P.RawOutput.F"),
        ("Steps.P.Output.Fs", "Steps.P.RawOutput.Fs"),
        ("Steps.Q.TaskParam.D", "Steps.Q.TaskRawParam.D"),
    ] {
        assert_errs(
            &scoped("STRING", &format!("{{{{{sym}}}}}")),
            &[(
                VALUE,
                &format!(
                    "'{sym}' has a path type, so it is available only in fields resolved on the worker host. Use {raw} here."
                ),
            )],
        );
    }
}

#[test]
fn path_typed_job_parameters_are_readable_only_through_raw_param() {
    assert_errs(
        &scoped("PATH", "{{Param.Out}}"),
        &[(
            VALUE,
            "Failed to parse interpolation expression at [0, 13]. Undefined variable: 'Param.Out'. Did you mean: RawParam.Out\n  Param.Out\n  ~~~~~~^~~",
        )],
    );
    assert_errs(
        &scoped("LIST[PATH]", "{{Param.Outs}}"),
        &[(
            VALUE,
            "Failed to parse interpolation expression at [0, 14]. Undefined variable: 'Param.Outs'. Did you mean: RawParam.Outs\n  Param.Outs\n  ~~~~~~^~~~",
        )],
    );
}

#[test]
fn step_name_task_session_and_let_names_are_not_available() {
    for (value, message) in [
        (
            "{{Step.Name}}",
            "Failed to parse interpolation expression at [0, 13]. Undefined variable: 'Step.Name'. Did you mean: Job.Name\n  Step.Name\n  ~~~~~^~~~",
        ),
        (
            "{{Task.Param.I}}",
            "Failed to parse interpolation expression at [0, 16]. Undefined variable: 'Task.Param.I'.\n  Task.Param.I\n  ~~~~~~~~~~~^",
        ),
        (
            "{{Session.WorkingDirectory}}",
            "Failed to parse interpolation expression at [0, 28]. Undefined variable: 'Session.WorkingDirectory'.\n  Session.WorkingDirectory\n  ~~~~~~~~^~~~~~~~~~~~~~~~",
        ),
        (
            "{{x}}",
            "Failed to parse interpolation expression at [0, 5]. Undefined variable: 'x'.\n  x\n  ^",
        ),
    ] {
        assert_errs(&scoped("STRING", value), &[(VALUE, message)]);
    }
}

#[test]
fn an_unavailable_steps_reference_gets_its_own_message() {
    for (value, message) in [
        (
            "{{Steps.Nope.Output.Count}}",
            "'Steps.Nope.Output.Count': there is no Step named 'Nope'.",
        ),
        (
            "{{Steps.P.Output.Nope}}",
            "'Steps.P.Output.Nope': Step 'P' has no output variable 'Nope'.",
        ),
        (
            "{{Steps.P.TaskParam.I}}",
            "'Steps.P.TaskParam.I': Step 'P' has no parameterSpace, so it has no Task parameters.",
        ),
        (
            "{{Steps.Q.TaskParam.Nope}}",
            "'Steps.Q.TaskParam.Nope': Step 'Q' has no Task parameter 'Nope'.",
        ),
        (
            "{{Steps.P.Other.N}}",
            "'Steps.P.Other.N': 'Other' is not a member of Steps.P; expected Output, RawOutput, TaskParam, TaskRawParam.",
        ),
        (
            "{{Steps.P.Output}}",
            "'Steps.P.Output' is not a complete Steps.* reference; expected Steps.<StepName>.Output.<Name>, .RawOutput.<Name>, .TaskParam.<Name>, or .TaskRawParam.<Name>.",
        ),
        // Checked statically, so a branch never taken counts.
        (
            "{{1 if true else Steps.Nope.Output.Count}}",
            "'Steps.Nope.Output.Count': there is no Step named 'Nope'.",
        ),
    ] {
        assert_errs(&scoped("INT", value), &[(VALUE, message)]);
    }
}

#[test]
fn a_step_whose_name_is_not_an_identifier_cannot_be_read() {
    // `Steps.Find Frames...` is not an expression; read through a dotted
    // name, the Step `Find` does not exist, so this is the closest an
    // expression can come.
    let t = template(
        "  - {name: O, type: INT, value: '{{Steps.Find_Frames.Output.Count}}'}\n",
        SCOPE_STEPS,
    );
    assert_errs(
        &t,
        &[(
            VALUE,
            "'Steps.Find_Frames.Output.Count': there is no Step named 'Find_Frames'.",
        )],
    );
}

#[test]
fn the_value_is_typed_against_the_declared_type_made_optional() {
    assert_errs(
        &scoped("INT", "{{[1, 2]}}"),
        &[(
            VALUE,
            "Failed to parse interpolation expression at [0, 10]. Cannot coerce list[int] to int?\n  [1, 2]\n  ^~~~~~",
        )],
    );
    // A multi-Task producer's output is a list.
    assert_errs(
        &scoped("INT", "{{Steps.Q.Output.M}}"),
        &[(
            VALUE,
            "Failed to parse interpolation expression at [0, 20]. Cannot coerce list[int] to int?\n  Steps.Q.Output.M\n  ~~~~~~~~~~~~~~~^",
        )],
    );
    // A static null is the output having no value.
    assert_ok(&scoped("INT", "{{null}}"));
}

#[test]
fn a_static_string_value_must_convert_to_the_declared_type() {
    assert_ok(&scoped("INT", "12"));
    assert_ok(&scoped("BOOL", "yes"));
    assert_ok(&scoped("STRING", "anything"));
    assert_errs(
        &scoped("INT", "abc"),
        &[(
            VALUE,
            "must convert to int: Cannot convert 'abc' to int: invalid digit found in string.",
        )],
    );
    assert_errs(
        &scoped("INT", "a{{1}}"),
        &[(
            VALUE,
            "must convert to int: Cannot convert 'a1' to int: invalid digit found in string.",
        )],
    );
}

// ── Evaluation at Job creation ─────────────────────────────────────

/// The `evaluated` of each of `job`'s outputs, in order.
fn evaluated(job: &openjd_model::job::Job) -> Vec<Option<JobOutputValue>> {
    job.outputs
        .iter()
        .flatten()
        .map(|o| o.evaluated.clone())
        .collect()
}

fn value(v: ExprValue) -> Option<JobOutputValue> {
    Some(JobOutputValue::Value(v))
}

/// A Job with outputs over: `P`, bound, with an INT output `Count` and Task
/// parameter `X` in `1-3`; `Skip`, statically skipped, with an INT output;
/// `W`, pending on `P` in its host requirements only, with Task parameter
/// `Seq` in `[10, 20]`; and job parameters `Num` (INT, default 4), `Out`
/// (PATH, given as `/out`), and `Word` (STRING).
const CREATION_STEPS: &str = "parameterDefinitions:
  - {name: Num, type: INT, default: 4}
  - {name: Out, type: PATH}
  - {name: Word, type: STRING, default: abc}
steps:
  - name: P
    parameterSpace:
      taskParameterDefinitions: [{name: X, type: INT, range: '1-3'}]
    outputVariables: [{name: Count, type: INT}]
    script: {actions: {onRun: {command: x}}}
  - name: Skip
    enableIf: false
    outputVariables: [{name: Count, type: INT}]
    script: {actions: {onRun: {command: x}}}
  - name: W
    dependencies: [{dependsOn: P}]
    hostRequirements:
      amounts: [{name: amount.worker.vcpu, min: '{{Steps.P.Output.Count[0]}}'}]
    parameterSpace:
      taskParameterDefinitions: [{name: Seq, type: INT, range: [10, 20]}]
    script: {actions: {onRun: {command: x}}}
";

fn creation(outputs: &str) -> Fixture {
    // FEATURE_BUNDLE_1 for W's format-string host requirement.
    Fixture::new(
        &template_with(
            "[EXPR, FEATURE_BUNDLE_1, CONTROL_FLOW]",
            outputs,
            CREATION_STEPS,
        ),
        &[("Out", "/out")],
    )
}

#[test]
fn outputs_whose_inputs_are_concrete_are_evaluated_at_creation() {
    let f = creation(
        "  - {name: Doubled, type: INT, value: '{{Param.Num * 2}}', unit: frames}
  - {name: Dir, type: PATH, value: '{{RawParam.Out}}'}
  - {name: Name, type: STRING, value: 'job {{Job.Name}}'}
  - {name: Skipped, type: INT, value: '{{Steps.Skip.Output.Count}}', minValue: 100}
  - {name: Frames, type: 'LIST[INT]', value: '{{Steps.P.TaskParam.X}}'}
  - {name: PendingTasks, type: INT, value: '{{len(Steps.W.TaskRawParam.Seq)}}'}
  - {name: Later, type: INT, value: '{{Steps.P.Output.Count[0]}}'}
  - {name: Both, type: INT, value: '{{Steps.P.Output.Count[0] + len(Steps.P.TaskParam.X)}}'}
  - {name: Literal, type: INT, value: '12'}
  - {name: Joined, type: INT, value: '1{{Param.Num}}'}
",
    );
    let job = f.job();
    // W's Tasks are known though W is still pending (its host requirements
    // wait for P's output).
    assert!(matches!(
        step(&job, "W").binding,
        StepBinding::PendingInputs(_)
    ));
    assert_eq!(step(&job, "W").tasks_known(), Some(2));
    assert_eq!(
        evaluated(&job),
        vec![
            value(ExprValue::Int(8)),
            value(ExprValue::new_path(
                "/out".to_string(),
                openjd_expr::PathFormat::Posix
            )),
            value(ExprValue::String("job J".to_string())),
            // A skipped Step's symbols are null, and a null value is not
            // checked against minValue.
            Some(JobOutputValue::Null),
            value(ExprValue::ListInt(vec![1, 2, 3])),
            value(ExprValue::Int(2)),
            // Output of a Step that has not run, alone or with a TaskParam
            // that is concrete: pending.
            None,
            None,
            value(ExprValue::Int(12)),
            // Text around an expression resolves to a string, then converts.
            value(ExprValue::Int(14)),
        ]
    );
    let doubled = &job.outputs.as_ref().unwrap()[0];
    assert_eq!(doubled.declaration.unit.as_deref(), Some("frames"));
}

#[test]
fn a_template_without_outputs_has_none() {
    let f = Fixture::new(&format!("{}{STEP_S}", header("[EXPR, CONTROL_FLOW]")), &[]);
    assert_eq!(f.job().outputs, None);
}

#[test]
fn every_output_that_fails_at_creation_fails_create_job_together() {
    let f = creation(
        "  - {name: Fine, type: INT, value: '{{Param.Num}}'}
  - {name: Failing, type: INT, value: \"{{Param.Num if Param.Num > 5 else fail('too few')}}\"}
  - {name: Checked, type: INT, value: '{{Param.Num}}', maxValue: 3}
  - {name: Converted, type: INT, value: '{{Param.Word}}'}
  - {name: Listed, type: 'LIST[INT]', value: '{{Steps.P.TaskParam.X}}', maxLength: 2}
  - {name: Joined, type: INT, value: 'x{{Param.Num}}'}
",
    );
    let err = f.create().expect_err("the job outputs fail");
    assert_eq!(
        err.to_string(),
        expected(&[
            (
                "jobOutputDefinitions[1] -> value",
                "Job output 'Failing': too few\n  Param.Num if Param.Num > 5 else fail('too few')\n                                  ^~~~~~~~~~~~~~~",
            ),
            (
                "jobOutputDefinitions[2] -> value",
                "Job output 'Checked': value 4 exceeds maximum 3",
            ),
            (
                "jobOutputDefinitions[3] -> value",
                "Job output 'Converted': Cannot coerce string to int?\n  Param.Word\n  ~~~~~~^~~~",
            ),
            (
                "jobOutputDefinitions[4] -> value",
                "Job output 'Listed': list length 3 exceeds maximum 2",
            ),
            (
                "jobOutputDefinitions[5] -> value",
                "Job output 'Joined': value 'x4' does not convert to INT: Cannot convert 'x4' to int: invalid digit found in string",
            ),
        ])
    );
}

#[test]
fn a_failing_output_that_waits_for_a_step_does_not_fail_create_job() {
    let f = creation(
        "  - {name: Later, type: INT, value: \"{{fail('never at creation') if Steps.P.Output.Count[0] > 0 else 1}}\"}\n",
    );
    assert_eq!(evaluated(&f.job()), vec![None]);
}

// ── job_output_inputs ──────────────────────────────────────────────

#[test]
fn job_output_inputs_names_each_steps_symbol_once_without_properties() {
    let f = Fixture::new(
        &template(
            "  - {name: A, type: STRING, value: '{{Steps.P.RawOutput.F.name}} {{Steps.P.RawOutput.F.stem}} {{Steps.Q.Output.Count}}'}
  - {name: B, type: INT, value: '{{Param.Num}}'}
",
            "parameterDefinitions: [{name: Num, type: INT, default: 1}]
steps:
  - name: P
    outputVariables: [{name: F, type: PATH}]
    script: {actions: {onRun: {command: x}}}
  - name: Q
    outputVariables: [{name: Count, type: INT}]
    script: {actions: {onRun: {command: x}}}
",
        ),
        &[],
    );
    let job = f.job();
    assert_eq!(
        job_output_inputs(&job, 0),
        vec!["Steps.P.RawOutput.F", "Steps.Q.Output.Count"]
    );
    assert_eq!(job_output_inputs(&job, 1), Vec::<String>::new());
    assert_eq!(job_output_inputs(&job, 2), Vec::<String>::new());
}

// ── evaluate_job_output ────────────────────────────────────────────

/// `P` (3 Tasks, INT output `Count`, LIST[INT] output `L`, PATH output
/// `F`); `G`, whose `enableIf` reads `P`'s first count; and `Z`, whose range
/// reads `P`'s first count and may be empty, with INT output `V`.
const LATER_STEPS: &str = "steps:
  - name: P
    parameterSpace:
      taskParameterDefinitions: [{name: X, type: INT, range: '1-3'}]
    outputVariables:
      - {name: Count, type: INT}
      - {name: L, type: 'LIST[INT]'}
      - {name: F, type: PATH}
    script: {actions: {onRun: {command: x}}}
  - name: G
    dependencies: [{dependsOn: P}]
    enableIf: '{{Steps.P.Output.Count[0] > 0}}'
    outputVariables: [{name: V, type: INT}]
    script: {actions: {onRun: {command: x}}}
  - name: Z
    dependencies: [{dependsOn: P}]
    parameterSpace:
      taskParameterDefinitions: [{name: I, type: INT, range: '{{range(Steps.P.Output.Count[0])}}'}]
    outputVariables: [{name: V, type: INT}]
    script: {actions: {onRun: {command: x}}}
";

fn p_outputs(counts: [i64; 3]) -> Vec<openjd_model::outputs::TaskOutputs> {
    counts
        .iter()
        .map(|c| {
            outputs(&[
                ("Count", ExprValue::Int(*c)),
                ("L", ExprValue::ListInt(vec![*c, *c + 1])),
                (
                    "F",
                    ExprValue::new_path(format!("/r/{c}.exr"), openjd_expr::PathFormat::Posix),
                ),
            ])
        })
        .collect()
}

fn later(outputs: &str) -> Fixture {
    Fixture::new(&template(outputs, LATER_STEPS), &[])
}

fn eval(
    f: &Fixture,
    job: &openjd_model::job::Job,
    index: usize,
    inputs: &StepInputs,
) -> Result<Option<ExprValue>, ModelError> {
    evaluate_job_output(&f.jt, &f.params, &f.ctx, job, index, inputs)
}

#[test]
fn an_output_reading_a_step_is_evaluated_once_the_step_succeeds() {
    let f = later(
        "  - {name: Best, type: INT, value: '{{min(Steps.P.Output.Count)}}', minValue: 0}
  - {name: Grid, type: 'LIST[LIST[INT]]', value: '{{Steps.P.Output.L}}'}
  - {name: Files, type: 'LIST[PATH]', value: '{{Steps.P.RawOutput.F}}'}
  - {name: Frames, type: STRING, value: 'frames {{Steps.P.TaskParam.X}}'}
",
    );
    let job = f.job();
    assert_eq!(
        evaluated(&job),
        vec![
            None,
            None,
            None,
            value(ExprValue::String("frames [1, 2, 3]".into()))
        ]
    );
    let mut inputs = StepInputs::new();
    succeeded(&mut inputs, &job, "P", &p_outputs([5, 2, 9]));
    assert_eq!(eval(&f, &job, 0, &inputs).unwrap(), Some(ExprValue::Int(2)));
    let grid = eval(&f, &job, 1, &inputs).unwrap().unwrap();
    assert_eq!(grid.to_display_string(), "[[5, 6], [2, 3], [9, 10]]");
    let files = eval(&f, &job, 2, &inputs).unwrap().unwrap();
    assert_eq!(files.expr_type().to_string(), "list[path]");
    assert_eq!(
        files.to_display_string(),
        r#"["/r/5.exr", "/r/2.exr", "/r/9.exr"]"#
    );
    // An output create_job already evaluated evaluates the same again.
    assert_eq!(
        eval(&f, &job, 3, &inputs).unwrap(),
        Some(ExprValue::String("frames [1, 2, 3]".into()))
    );
}

#[test]
fn a_later_failure_conversion_or_constraint_violation_fails_the_job() {
    let f = later(
        "  - {name: Best, type: INT, value: '{{min(Steps.P.Output.Count)}}', minValue: 3}
  - {name: Text, type: INT, value: \"{{string(Steps.P.Output.Count[0]) + 'x'}}\"}
  - {name: Failing, type: INT, value: \"{{fail('bad ' + string(Steps.P.Output.Count[1])) if Steps.P.Output.Count[1] < 10 else 0}}\"}
",
    );
    let job = f.job();
    let mut inputs = StepInputs::new();
    succeeded(&mut inputs, &job, "P", &p_outputs([5, 2, 9]));
    assert_eq!(
        eval(&f, &job, 0, &inputs).unwrap_err().to_string(),
        expected(&[(
            "jobOutputDefinitions[0] -> value",
            "Job output 'Best': value 2 is less than minimum 3"
        )])
    );
    assert_eq!(
        eval(&f, &job, 1, &inputs).unwrap_err().to_string(),
        expected(&[(
            "jobOutputDefinitions[1] -> value",
            "Job output 'Text': Cannot coerce string to int?\n  string(Steps.P.Output.Count[0]) + 'x'\n  ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^~~~~"
        )])
    );
    assert_eq!(
        eval(&f, &job, 2, &inputs).unwrap_err().to_string(),
        expected(&[(
            "jobOutputDefinitions[2] -> value",
            "Job output 'Failing': bad 2\n  fail('bad ' + string(Steps.P.Output.Count[1])) if Steps.P.Output.Count[1] < 10 else 0\n  ^~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~"
        )])
    );
}

#[test]
fn a_dynamically_skipped_step_gives_null_and_its_constraints_are_not_checked() {
    let f = later("  - {name: V, type: INT, value: '{{Steps.G.Output.V}}', minValue: 100}\n");
    let mut job = f.job();
    let g = f
        .advance(
            &mut job,
            "G",
            |job| {
                let mut inputs = StepInputs::new();
                succeeded(&mut inputs, job, "P", &p_outputs([0, 1, 2]));
                inputs
            },
            3,
        )
        .unwrap();
    assert!(matches!(g.binding, StepBinding::Skipped));
    let mut inputs = StepInputs::new();
    inputs
        .add_producer(step(&job, "G"), ProducerState::Skipped)
        .unwrap();
    assert_eq!(eval(&f, &job, 0, &inputs).unwrap(), None);
    assert_eq!(JobOutputValue::from(None), JobOutputValue::Null);
}

#[test]
fn a_zero_task_step_gives_empty_lists() {
    let f = later(
        "  - {name: Best, type: INT, value: '{{min(Steps.Z.Output.V)}}'}
  - {name: Guarded, type: INT, value: '{{min(Steps.Z.Output.V) if len(Steps.Z.Output.V) > 0 else null}}'}
  - {name: Count, type: INT, value: '{{len(Steps.Z.TaskParam.I)}}'}
",
    );
    let mut job = f.job();
    let z = f
        .advance(
            &mut job,
            "Z",
            |job| {
                let mut inputs = StepInputs::new();
                succeeded(&mut inputs, job, "P", &p_outputs([0, 1, 2]));
                inputs
            },
            3,
        )
        .unwrap();
    assert_eq!(z.tasks_known(), Some(0));
    let mut inputs = StepInputs::new();
    tasks_known(&mut inputs, &job, "Z");
    assert_eq!(eval(&f, &job, 2, &inputs).unwrap(), Some(ExprValue::Int(0)));
    let mut inputs = StepInputs::new();
    succeeded(&mut inputs, &job, "Z", &[]);
    assert_eq!(
        eval(&f, &job, 0, &inputs).unwrap_err().to_string(),
        expected(&[(
            "jobOutputDefinitions[0] -> value",
            "Job output 'Best': min() requires a non-empty list\n  min(Steps.Z.Output.V)\n  ^~~~~~~~~~~~~~~~~~~~~"
        )])
    );
    assert_eq!(eval(&f, &job, 1, &inputs).unwrap(), None);
}

#[test]
fn caller_errors_are_compatibility_errors() {
    let f = later("  - {name: Best, type: INT, value: '{{min(Steps.P.Output.Count)}}'}\n");
    let job = f.job();
    let mut inputs = StepInputs::new();
    tasks_known(&mut inputs, &job, "P");
    let err = eval(&f, &job, 0, &inputs).unwrap_err();
    assert!(matches!(err, ModelError::Compatibility(_)));
    assert_eq!(
        err.to_string(),
        "Compatibility error: job output 'Best' reads 'Steps.P.Output.Count', which is not concrete in the given inputs."
    );
    assert_eq!(
        eval(&f, &job, 1, &inputs).unwrap_err().to_string(),
        "Compatibility error: the Job has 1 job outputs, so it has no job output 1."
    );
    let other = later("  - {name: Other, type: INT, value: '{{1}}'}\n");
    assert_eq!(
        evaluate_job_output(&other.jt, &f.params, &f.ctx, &job, 0, &inputs)
            .unwrap_err()
            .to_string(),
        "Compatibility error: job output 0 of the Job is 'Best', but the template's is not; pass the template the Job was created from."
    );
}

// ── The job-side form ──────────────────────────────────────────────

#[test]
fn job_outputs_serialize_with_their_declaration_and_value() {
    let f = creation(
        "  - {name: Doubled, type: INT, value: '{{Param.Num * 2}}', unit: frames, description: Twice, maxValue: 10}
  - {name: Later, type: INT, value: '{{Steps.P.Output.Count[0]}}'}
  - {name: Skipped, type: INT, value: '{{Steps.Skip.Output.Count}}'}
",
    );
    let job = f.job();
    let json = serde_json::to_value(&job).unwrap();
    assert_eq!(
        json["outputs"],
        serde_json::json!([
            {
                "name": "Doubled",
                "type": "INT",
                "unit": "frames",
                "constraints": {"maxValue": {"int": 10}},
                "description": "Twice",
                "value": "{{Param.Num * 2}}",
                // ExprValue's serde form, as for a job parameter's value.
                "evaluated": {"value": {"Int": 8}},
            },
            {
                "name": "Later",
                "type": "INT",
                "value": "{{Steps.P.Output.Count[0]}}",
            },
            {
                "name": "Skipped",
                "type": "INT",
                "value": "{{Steps.Skip.Output.Count}}",
                "evaluated": "null",
            },
        ])
    );
    assert_eq!(
        job.outputs.as_ref().unwrap()[0]
            .declaration
            .constraints
            .max_value,
        Some(OutputBound::Int(10))
    );
    // A Job without outputs serializes without the key.
    let none = Fixture::new(&format!("{}{STEP_S}", header("[EXPR, CONTROL_FLOW]")), &[]).job();
    assert!(serde_json::to_value(&none)
        .unwrap()
        .get("outputs")
        .is_none());
}
