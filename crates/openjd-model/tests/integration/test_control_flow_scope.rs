// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `CONTROL_FLOW` (RFC 0010) `Steps.*` scope and `enableIf` typing in
//! template validation (pass 8), and the `create_job` re-checks that see the
//! same `Steps.*` symbols.
//!
//! Failure tests assert the full error output (count, path, and message), as
//! `AGENTS.md` § Test Quality Standard requires.

use openjd_model::{
    create_job, decode_job_template, preprocess_job_parameters, CallerLimits, PathParameterOptions,
};

const SUPPORTED: &[&str] = &["EXPR", "CONTROL_FLOW", "TASK_CHUNKING", "FEATURE_BUNDLE_1"];

fn yaml(s: &str) -> serde_json::Value {
    serde_saphyr::from_str(s).unwrap()
}

fn decode(template: &str) -> Result<openjd_model::template::JobTemplate, openjd_model::ModelError> {
    decode_job_template(yaml(template), Some(SUPPORTED), &CallerLimits::default())
}

fn decode_ok(template: &str) -> openjd_model::template::JobTemplate {
    match decode(template) {
        Ok(jt) => jt,
        Err(e) => panic!("expected the template to decode, got:\n{e}\n\n{template}"),
    }
}

fn expected(errors: &[(&str, &str)]) -> String {
    let n = errors.len();
    let word = if n == 1 { "error" } else { "errors" };
    let mut out = format!("Model validation error: {n} validation {word} for JobTemplate");
    for (path, msg) in errors {
        out.push_str(&format!("\n{path}:\n\t{msg}"));
    }
    out
}

/// Assert the complete validation error output.
fn assert_errs(template: &str, errors: &[(&str, &str)]) {
    let err = decode(template).expect_err("expected a validation error");
    assert_eq!(err.to_string(), expected(errors));
}

const EXTENSIONS: &str = "[EXPR, CONTROL_FLOW, TASK_CHUNKING, FEATURE_BUNDLE_1]";

/// A template with three producers and a consumer `C` (`steps[3]`) that
/// depends directly on all of them:
///
/// - `P`: no parameterSpace, outputs `Count: INT`, `F: PATH`,
///   `Fs: LIST[PATH]`;
/// - `Q`: depends on `P`; Task parameters `Frame: INT` and `D: PATH`;
///   output `T: FLOAT`;
/// - `R`: `enableIf`, output `Go: BOOL`.
///
/// `c_fields` are `C`'s other fields, as block YAML indented four spaces;
/// they must include its `script` or a SimpleAction.
fn template(c_fields: &str) -> String {
    template_with(EXTENSIONS, "", c_fields)
}

fn template_with(extensions: &str, job_fields: &str, c_fields: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09
extensions: {extensions}
name: J
parameterDefinitions:
  - {{name: Run, type: BOOL, default: true}}
  - {{name: Scene, type: PATH, default: scene}}
{job_fields}steps:
  - name: P
    outputVariables:
      - {{name: Count, type: INT}}
      - {{name: F, type: PATH}}
      - {{name: Fs, type: 'LIST[PATH]'}}
    script: {{actions: {{onRun: {{command: echo}}}}}}
  - name: Q
    dependencies: [{{dependsOn: P}}]
    parameterSpace:
      taskParameterDefinitions:
        - {{name: Frame, type: INT, range: '1-3'}}
        - {{name: D, type: PATH, range: ['/a']}}
    outputVariables:
      - {{name: T, type: FLOAT}}
    script: {{actions: {{onRun: {{command: echo}}}}}}
  - name: R
    enableIf: '{{{{Param.Run}}}}'
    outputVariables:
      - {{name: Go, type: BOOL}}
    script: {{actions: {{onRun: {{command: echo}}}}}}
  - name: C
    dependencies: [{{dependsOn: P}}, {{dependsOn: Q}}, {{dependsOn: R}}]
{c_fields}"
    )
}

/// `C` with one `onRun` argument (single-quoted flow YAML), after
/// `other_fields`.
fn c_arg(other_fields: &str, arg: &str) -> String {
    template(&format!(
        "{other_fields}    script: {{actions: {{onRun: {{command: echo, args: ['{arg}']}}}}}}\n"
    ))
}

const ECHO: &str = "    script: {actions: {onRun: {command: echo}}}\n";
const ARG0: &str = "steps[3] -> script -> actions -> onRun -> args[0]";

// ══════════════════════════════════════════════════════════════
// Where Steps.* is available
// ══════════════════════════════════════════════════════════════

#[test]
fn every_allowed_field_may_read_steps() {
    decode_ok(&template(
        "    enableIf: '{{Steps.R.Output.Go != false and Steps.P.Output.Count > 0}}'
    let:
      - n2 = Steps.P.Output.Count * 2
      - frames = Steps.Q.TaskParam.Frame
    hostRequirements:
      amounts:
        - {name: amount.worker.vcpu, min: '{{Steps.P.Output.Count}}'}
      attributes:
        - {name: attr.worker.os.family, anyOf: ['{{Steps.Q.TaskRawParam.D[0].name}}']}
    stepEnvironments:
      - name: CEnv
        enableIf: '{{Steps.P.Output.Count > 0}}'
        variables:
          CF_N: '{{Steps.P.Output.Count}}'
          CF_F: '{{Steps.P.Output.F}}'
        script:
          actions:
            onEnter: {command: echo, args: ['{{Steps.Q.TaskParam.D}}'], timeout: '{{Steps.P.Output.Count}}'}
    parameterSpace:
      taskParameterDefinitions:
        - {name: X, type: INT, range: '1-{{Steps.P.Output.Count}}'}
        - {name: W, type: STRING, range: ['{{Steps.P.RawOutput.F}}']}
    script:
      let:
        - fs = Steps.P.Output.Fs
      actions:
        onRun:
          command: echo
          args: ['{{Steps.P.Output.F}}', '{{fs}}', '{{n2}}', '{{Steps.Q.Output.T}}']
          timeout: '{{Steps.P.Output.Count + 60}}'
",
    ));
}

#[test]
fn simple_action_field_may_read_steps() {
    decode_ok(&template(
        "    bash: {script: 'echo {{Steps.P.Output.F}} {{Steps.Q.Output.T}}'}\n",
    ));
}

#[test]
fn steps_in_a_simple_action_is_checked_at_the_authored_field() {
    assert_errs(
        &template("    bash: {script: 'echo {{Steps.Nope.Output.A}}'}\n"),
        &[(
            "steps[3] -> bash -> script",
            "'Steps.Nope.Output.A': there is no Step named 'Nope'.",
        )],
    );
}

#[test]
fn job_name_may_not_read_steps() {
    let t = template(ECHO).replacen("name: J\n", "name: '{{Steps.P.Output.Count}}'\n", 1);
    assert_errs(
        &t,
        &[(
            "name",
            "'Steps.P.Output.Count': Steps.* symbols are not available in this field. Only a Step's enableIf, let, hostRequirements, parameterSpace, stepEnvironments, and script may read them.",
        )],
    );
}

#[test]
fn job_environments_may_not_read_steps() {
    let job_fields = "jobEnvironments:
  - name: JE
    enableIf: '{{Steps.P.Output.Count > 0}}'
    variables: {V: '{{Steps.P.Output.Count}}'}
    script:
      let: ['x = Steps.P.Output.Count']
      actions:
        onEnter: {command: echo, timeout: '{{Steps.P.Output.Count}}'}
";
    let not_here = "'Steps.P.Output.Count': Steps.* symbols are not available in this field. Only a Step's enableIf, let, hostRequirements, parameterSpace, stepEnvironments, and script may read them.";
    let let_msg = format!("Invalid expression in let binding 'x': {not_here}");
    assert_errs(
        &template_with(EXTENSIONS, job_fields, ECHO),
        &[
            ("jobEnvironments[0] -> enableIf", not_here),
            ("jobEnvironments[0] -> script -> let[0]", &let_msg),
            ("jobEnvironments[0] -> variables -> V", not_here),
            (
                "jobEnvironments[0] -> script -> actions -> onEnter -> timeout",
                not_here,
            ),
        ],
    );
}

#[test]
fn without_control_flow_steps_is_an_undefined_variable() {
    let t = "specificationVersion: jobtemplate-2023-09
extensions: [EXPR]
name: J
steps:
  - name: P
    script: {actions: {onRun: {command: echo}}}
  - name: C
    dependencies: [{dependsOn: P}]
    script: {actions: {onRun: {command: echo, args: ['{{Steps.P.Output.Count}}']}}}
";
    assert_eq!(
        decode(t).unwrap_err().to_string(),
        expected(&[(
            "steps[1] -> script -> actions -> onRun -> args[0]",
            "Failed to parse interpolation expression at [0, 24]. Undefined variable: 'Steps.P.Output.Count'.\n  Steps.P.Output.Count\n  ~~~~~~~~~~~~~~~^~~~~",
        )])
    );
}

// ══════════════════════════════════════════════════════════════
// One message per kind of unavailable reference
// ══════════════════════════════════════════════════════════════

#[test]
fn no_such_step() {
    assert_errs(
        &c_arg("", "{{Steps.Nope.Output.Count}}"),
        &[(
            ARG0,
            "'Steps.Nope.Output.Count': there is no Step named 'Nope'.",
        )],
    );
}

#[test]
fn step_name_that_is_not_an_identifier() {
    let t = "specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: J
steps:
  - name: Café
    outputVariables: [{name: Count, type: INT}]
    script: {actions: {onRun: {command: echo}}}
  - name: C
    dependencies: [{dependsOn: Café}]
    script: {actions: {onRun: {command: echo, args: ['{{Steps.Café.Output.Count}}']}}}
";
    assert_errs(
        t,
        &[(
            "steps[1] -> script -> actions -> onRun -> args[0]",
            "'Steps.Café.Output.Count': Step 'Café' cannot be referenced through Steps.*, because its name is not an <Identifier>.",
        )],
    );
}

#[test]
fn a_step_whose_name_is_not_an_identifier_is_still_a_valid_dependency() {
    decode_ok(
        "specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: J
steps:
  - name: Find Frames
    outputVariables: [{name: Count, type: INT}]
    script: {actions: {onRun: {command: echo}}}
  - name: C
    dependencies: [{dependsOn: Find Frames}]
    script: {actions: {onRun: {command: echo}}}
",
    );
}

#[test]
fn self_reference() {
    let t = template(ECHO).replacen(
        "      - {name: Fs, type: 'LIST[PATH]'}\n",
        "      - {name: Fs, type: 'LIST[PATH]'}\n    let: ['x = Steps.P.Output.Count']\n",
        1,
    );
    assert_errs(
        &t,
        &[(
            "steps[0] -> let[0]",
            "Invalid expression in let binding 'x': 'Steps.P.Output.Count': a Step cannot read its own Steps.* symbols.",
        )],
    );
}

#[test]
fn not_a_dependency() {
    let t = template_with(
        EXTENSIONS,
        "",
        "    script: {actions: {onRun: {command: echo}}}
  - name: D
    script: {actions: {onRun: {command: echo, args: ['{{Steps.P.Output.Count}}']}}}
",
    );
    assert_errs(
        &t,
        &[(
            "steps[4] -> script -> actions -> onRun -> args[0]",
            "'Steps.P.Output.Count': Step 'P' is not a direct dependency of Step 'D'. Add 'dependsOn: P' to its dependencies to read Steps.P.*.",
        )],
    );
}

#[test]
fn transitive_dependency_is_not_enough() {
    // D depends on C, which depends on P.
    let t = template_with(
        EXTENSIONS,
        "",
        "    script: {actions: {onRun: {command: echo}}}
  - name: D
    dependencies: [{dependsOn: C}]
    script: {actions: {onRun: {command: echo, args: ['{{Steps.P.Output.Count}}']}}}
",
    );
    assert_errs(
        &t,
        &[(
            "steps[4] -> script -> actions -> onRun -> args[0]",
            "'Steps.P.Output.Count': Step 'P' is not a direct dependency of Step 'D'. Add 'dependsOn: P' to its dependencies to read Steps.P.*.",
        )],
    );
}

#[test]
fn unknown_member() {
    assert_errs(
        &c_arg("", "{{Steps.P.Outputs.Count}}"),
        &[(
            ARG0,
            "'Steps.P.Outputs.Count': 'Outputs' is not a member of Steps.P; expected Output, RawOutput, TaskParam, TaskRawParam.",
        )],
    );
}

#[test]
fn incomplete_reference() {
    assert_errs(
        &c_arg("", "{{len(Steps.P.Output)}}"),
        &[(
            ARG0,
            "'Steps.P.Output' is not a complete Steps.* reference; expected Steps.<StepName>.Output.<Name>, .RawOutput.<Name>, .TaskParam.<Name>, or .TaskRawParam.<Name>.",
        )],
    );
}

#[test]
fn no_such_output() {
    assert_errs(
        &c_arg("", "{{Steps.Q.RawOutput.Nope}}"),
        &[(
            ARG0,
            "'Steps.Q.RawOutput.Nope': Step 'Q' has no output variable 'Nope'.",
        )],
    );
}

#[test]
fn producer_without_output_variables_has_no_outputs() {
    let t = template_with(
        EXTENSIONS,
        "",
        "    script: {actions: {onRun: {command: echo}}}
  - name: D
    dependencies: [{dependsOn: C}]
    script: {actions: {onRun: {command: echo, args: ['{{Steps.C.Output.X}}']}}}
",
    );
    assert_errs(
        &t,
        &[(
            "steps[4] -> script -> actions -> onRun -> args[0]",
            "'Steps.C.Output.X': Step 'C' has no output variable 'X'.",
        )],
    );
}

#[test]
fn task_param_of_a_step_without_parameter_space() {
    assert_errs(
        &c_arg("", "{{Steps.P.TaskRawParam.Frame}}"),
        &[(
            ARG0,
            "'Steps.P.TaskRawParam.Frame': Step 'P' has no parameterSpace, so it has no Task parameters.",
        )],
    );
}

#[test]
fn no_such_task_param() {
    assert_errs(
        &c_arg("", "{{Steps.Q.TaskParam.Nope}}"),
        &[(
            ARG0,
            "'Steps.Q.TaskParam.Nope': Step 'Q' has no Task parameter 'Nope'.",
        )],
    );
}

#[test]
fn task_param_of_a_step_without_output_variables_is_available() {
    let t = template_with(
        EXTENSIONS,
        "",
        "    parameterSpace:
      taskParameterDefinitions: [{name: K, type: INT, range: [1, 2]}]
    script: {actions: {onRun: {command: echo}}}
  - name: D
    dependencies: [{dependsOn: C}]
    let: ['total = sum(Steps.C.TaskParam.K)']
    script: {actions: {onRun: {command: echo, args: ['{{total}}']}}}
",
    );
    decode_ok(&t);
}

#[test]
fn several_references_in_one_field_are_reported_in_symbol_order() {
    assert_errs(
        &c_arg("", "{{Steps.Q.Output.Zed + Steps.Nope.Output.A}}"),
        &[
            (
                ARG0,
                "'Steps.Nope.Output.A': there is no Step named 'Nope'.",
            ),
            (
                ARG0,
                "'Steps.Q.Output.Zed': Step 'Q' has no output variable 'Zed'.",
            ),
        ],
    );
}

#[test]
fn a_reference_in_a_branch_that_is_never_taken_is_checked() {
    assert_errs(
        &c_arg("", "{{1 if true else Steps.Nope.Output.A}}"),
        &[(
            ARG0,
            "'Steps.Nope.Output.A': there is no Step named 'Nope'.",
        )],
    );
}

#[test]
fn a_reference_through_a_let_is_reported_at_the_let_only() {
    assert_errs(
        &c_arg("    let: ['x = Steps.Nope.Output.A']\n", "{{x + 1}}"),
        &[(
            "steps[3] -> let[0]",
            "Invalid expression in let binding 'x': 'Steps.Nope.Output.A': there is no Step named 'Nope'.",
        )],
    );
}

#[test]
fn expression_ranges_that_pass_8_does_not_evaluate_still_check_steps() {
    assert_errs(
        &template(
            "    parameterSpace:
      taskParameterDefinitions:
        - {name: S, type: STRING, range: '{{Steps.Nope.Output.A}}'}
        - {name: D, type: PATH, range: '{{Steps.P.Output.Fs}}'}
        - {name: X, type: FLOAT, range: '{{Steps.Q.Output.Nope}}'}
        - {name: W, type: FLOAT, range: [1.5, '{{Steps.Q.TaskParam.Nope}}']}
    script: {actions: {onRun: {command: echo}}}
",
        ),
        &[
            (
                "steps[3] -> parameterSpace -> taskParameterDefinitions[0] -> range",
                "'Steps.Nope.Output.A': there is no Step named 'Nope'.",
            ),
            (
                "steps[3] -> parameterSpace -> taskParameterDefinitions[1] -> range",
                "'Steps.P.Output.Fs' has a path type, so it is available only in fields resolved on the worker host. Use Steps.P.RawOutput.Fs here.",
            ),
            (
                "steps[3] -> parameterSpace -> taskParameterDefinitions[2] -> range",
                "'Steps.Q.Output.Nope': Step 'Q' has no output variable 'Nope'.",
            ),
            (
                "steps[3] -> parameterSpace -> taskParameterDefinitions[3] -> range[1]",
                "'Steps.Q.TaskParam.Nope': Step 'Q' has no Task parameter 'Nope'.",
            ),
        ],
    );
}

// ══════════════════════════════════════════════════════════════
// PATH values: only on the worker host, except through Raw
// ══════════════════════════════════════════════════════════════

#[test]
fn path_values_in_template_scope_fields_need_the_raw_form() {
    let use_raw_output = |v: &str| {
        format!(
            "'Steps.P.Output.{v}' has a path type, so it is available only in fields resolved on the worker host. Use Steps.P.RawOutput.{v} here."
        )
    };
    let use_raw_param = "'Steps.Q.TaskParam.D' has a path type, so it is available only in fields resolved on the worker host. Use Steps.Q.TaskRawParam.D here.";
    let let_msg = format!(
        "Invalid expression in let binding 'f': {}",
        use_raw_output("F")
    );
    assert_errs(
        &template(
            "    enableIf: '{{len(Steps.P.Output.Fs) > 0}}'
    let: ['f = Steps.P.Output.F']
    stepEnvironments:
      - name: E
        enableIf: '{{Steps.P.Output.F != null}}'
        script:
          actions:
            onEnter: {command: echo, timeout: '{{len(Steps.P.Output.Fs)}}'}
    parameterSpace:
      taskParameterDefinitions:
        - {name: S, type: STRING, range: ['{{Steps.P.Output.F}}']}
    script: {actions: {onRun: {command: echo, timeout: '{{len(Steps.Q.TaskParam.D)}}'}}}
",
        ),
        &[
            ("steps[3] -> enableIf", &use_raw_output("Fs")),
            (
                "steps[3] -> parameterSpace -> taskParameterDefinitions[0] -> range[0]",
                &use_raw_output("F"),
            ),
            ("steps[3] -> let[0]", &let_msg),
            (
                "steps[3] -> script -> actions -> onRun -> timeout",
                use_raw_param,
            ),
            (
                "steps[3] -> stepEnvironments[0] -> enableIf",
                &use_raw_output("F"),
            ),
            (
                "steps[3] -> stepEnvironments[0] -> script -> actions -> onEnter -> timeout",
                &use_raw_output("Fs"),
            ),
        ],
    );
}

#[test]
fn path_values_in_host_requirements_need_the_raw_form() {
    assert_errs(
        &template(
            "    hostRequirements:
      attributes:
        - {name: attr.worker.os.family, anyOf: ['{{Steps.Q.TaskParam.D[0].name}}']}
    script: {actions: {onRun: {command: echo}}}
",
        ),
        &[(
            "steps[3] -> hostRequirements -> attributes[0] -> anyOf[0]",
            "'Steps.Q.TaskParam.D' has a path type, so it is available only in fields resolved on the worker host. Use Steps.Q.TaskRawParam.D here.",
        )],
    );
}

#[test]
fn raw_path_values_are_available_in_template_scope_fields() {
    decode_ok(&template(
        "    enableIf: '{{len(Steps.P.RawOutput.Fs) > 0}}'
    let: ['f = Steps.P.RawOutput.F.name']
    parameterSpace:
      taskParameterDefinitions:
        - {name: S, type: PATH, range: '{{Steps.Q.TaskRawParam.D}}'}
        - {name: X, type: STRING, range: ['{{f}}', '{{Steps.P.RawOutput.Fs}}']}
    script: {actions: {onRun: {command: echo, timeout: '{{len(Steps.P.RawOutput.Fs) + 1}}'}}}
",
    ));
}

#[test]
fn path_values_are_available_in_host_scope_fields() {
    decode_ok(&template(
        "    stepEnvironments:
      - name: E
        variables: {A: '{{Steps.P.Output.F}}', B: '{{Steps.Q.TaskParam.D[0]}}'}
        script:
          let: ['g = Steps.P.Output.Fs']
          actions: {onEnter: {command: echo, args: ['{{g}}']}}
    script:
      embeddedFiles:
        - {name: Data, type: TEXT, data: '{{Steps.P.Output.F}}'}
      actions: {onRun: {command: echo, args: ['{{Steps.P.Output.F.name}}', '{{Steps.Q.TaskParam.D}}']}}
",
    ));
}

// ══════════════════════════════════════════════════════════════
// Types: scalar, list, optional
// ══════════════════════════════════════════════════════════════

#[test]
fn a_single_task_output_is_a_scalar() {
    assert_errs(
        &c_arg("    let: ['x = sum(Steps.P.Output.Count)']\n", "{{x}}"),
        &[(
            "steps[3] -> let[0]",
            "Invalid expression in let binding 'x': No matching signature for sum(int)\n  x = sum(Steps.P.Output.Count)\n      ^~~~~~~~~~~~~~~~~~~~~~~~~",
        )],
    );
}

#[test]
fn a_multi_task_output_is_a_list() {
    assert_errs(
        &c_arg("    let: ['x = Steps.Q.Output.T - 1']\n", "{{x}}"),
        &[(
            "steps[3] -> let[0]",
            "Invalid expression in let binding 'x': Cannot use '-' operator with list[float] and int\n  x = Steps.Q.Output.T - 1\n      ~~~~~~~~~~~~~~~~~^~~",
        )],
    );
}

#[test]
fn a_multi_task_output_reduces_and_task_params_align() {
    decode_ok(&c_arg(
        "    let:
      - best = max(Steps.Q.Output.T)
      - frames = Steps.Q.TaskParam.Frame
      - n = len(frames) + sum(frames)
",
        "{{best}} {{n}}",
    ));
}

#[test]
fn a_conditional_output_is_accepted_where_its_type_is_required() {
    // `Steps.R.Output.Go` is `bool?`, accepted where `bool` is required, and
    // checked when concrete (RFC Design Choice 8).
    decode_ok(&c_arg(
        "    let: ['x = 1 if Steps.R.Output.Go else 2']\n",
        "{{x}}",
    ));
}

#[test]
fn a_chunk_int_task_param_is_one_int_per_task() {
    let t = template_with(
        EXTENSIONS,
        "",
        "    parameterSpace:
      taskParameterDefinitions:
        - {name: K, type: 'CHUNK[INT]', range: '1-10', chunks: {defaultTaskCount: 5, rangeConstraint: CONTIGUOUS}}
    script: {actions: {onRun: {command: echo}}}
  - name: D
    dependencies: [{dependsOn: C}]
    let: ['total = sum(Steps.C.TaskParam.K) + Steps.C.TaskParam.K[0]']
    script: {actions: {onRun: {command: echo, args: ['{{total}}']}}}
",
    );
    decode_ok(&t);
}

// ══════════════════════════════════════════════════════════════
// enableIf: type bool, restricted symbols
// ══════════════════════════════════════════════════════════════

fn c_enable_if(value: &str) -> String {
    template(&format!(
        "    enableIf: {value}\n    let: ['run = true']\n{ECHO}"
    ))
}

#[test]
fn enable_if_accepts_bools_and_bool_expressions() {
    for value in [
        "true",
        "false",
        "'{{Param.Run}}'",
        "'{{RawParam.Scene != \"\"}}'",
        "'{{Job.Name == \"J\" and Step.Name == \"C\"}}'",
        "'{{Steps.P.Output.Count > 1}}'",
        // bool? is accepted where bool is required and checked when concrete.
        "'{{Steps.R.Output.Go}}'",
        "'{{ Param.Run }}'",
    ] {
        decode_ok(&c_enable_if(value));
    }
}

#[test]
fn enable_if_must_be_a_bool() {
    assert_errs(
        &c_enable_if("'{{Steps.P.Output.Count}}'"),
        &[(
            "steps[3] -> enableIf",
            "Failed to parse interpolation expression at [0, 24]. Cannot coerce int to bool\n  Steps.P.Output.Count\n  ~~~~~~~~~~~~~~~^~~~~",
        )],
    );
}

#[test]
fn enable_if_may_not_be_a_static_null() {
    assert_errs(
        &c_enable_if("'{{null}}'"),
        &[(
            "steps[3] -> enableIf",
            "Failed to parse interpolation expression at [0, 8]. Cannot coerce nulltype to bool\n  null\n  ^~~~",
        )],
    );
}

#[test]
fn enable_if_must_be_one_whole_field_expression() {
    let msg = "must be a bool, or a format string that is one expression and nothing else, such as '{{Param.Run}}'.";
    for value in [
        "'yes'",
        "'true'",
        "'{{Param.Run}} '",
        "'{{Param.Run}}{{Param.Run}}'",
    ] {
        assert_errs(&c_enable_if(value), &[("steps[3] -> enableIf", msg)]);
    }
    // The references are still checked.
    assert_errs(
        &c_enable_if("'x{{Steps.Nope.Output.A}}'"),
        &[
            ("steps[3] -> enableIf", msg),
            (
                "steps[3] -> enableIf",
                "'Steps.Nope.Output.A': there is no Step named 'Nope'.",
            ),
        ],
    );
}

#[test]
fn enable_if_may_not_read_let_task_session_or_path_params() {
    let cases = [
        (
            "'{{run}}'",
            "Failed to parse interpolation expression at [0, 7]. Undefined variable: 'run'.\n  run\n  ^~~",
        ),
        (
            "'{{Session.HasPathMappingRules}}'",
            "Failed to parse interpolation expression at [0, 31]. Undefined variable: 'Session.HasPathMappingRules'.\n  Session.HasPathMappingRules\n  ~~~~~~~~^~~~~~~~~~~~~~~~~~~",
        ),
        (
            "'{{Param.Scene != null}}'",
            "Failed to parse interpolation expression at [0, 23]. Undefined variable: 'Param.Scene'. Did you mean: RawParam.Scene\n  Param.Scene != null\n  ~~~~~~^~~~~",
        ),
    ];
    for (value, msg) in cases {
        assert_errs(&c_enable_if(value), &[("steps[3] -> enableIf", msg)]);
    }
    let t = template(
        "    enableIf: '{{Task.Param.X > 1}}'
    parameterSpace:
      taskParameterDefinitions: [{name: X, type: INT, range: [1]}]
    script: {actions: {onRun: {command: echo}}}
",
    );
    assert_errs(
        &t,
        &[(
            "steps[3] -> enableIf",
            "Failed to parse interpolation expression at [0, 20]. Undefined variable: 'Task.Param.X'.\n  Task.Param.X > 1\n  ~~~~~~~~~~~^",
        )],
    );
}

#[test]
fn job_environment_enable_if_scope() {
    let job_env = |value: &str| {
        template_with(
            EXTENSIONS,
            &format!(
                "jobEnvironments:\n  - {{name: JE, enableIf: {value}, variables: {{A: a}}}}\n"
            ),
            ECHO,
        )
    };
    decode_ok(&job_env("'{{Param.Run and Job.Name == \"J\"}}'"));
    assert_errs(
        &job_env("'{{Step.Name == \"C\"}}'"),
        &[(
            "jobEnvironments[0] -> enableIf",
            "Failed to parse interpolation expression at [0, 20]. Undefined variable: 'Step.Name'. Did you mean: Job.Name\n  Step.Name == \"C\"\n  ~~~~~^~~~",
        )],
    );
    assert_errs(
        &job_env("'{{Session.HasPathMappingRules}}'"),
        &[(
            "jobEnvironments[0] -> enableIf",
            "Failed to parse interpolation expression at [0, 31]. Undefined variable: 'Session.HasPathMappingRules'.\n  Session.HasPathMappingRules\n  ~~~~~~~~^~~~~~~~~~~~~~~~~~~",
        )],
    );
}

#[test]
fn step_environment_enable_if_scope() {
    let step_env = |value: &str| {
        template(&format!(
            "    let: ['use_env = true']
    stepEnvironments:
      - {{name: SE, enableIf: {value}, variables: {{A: a}}}}
{ECHO}"
        ))
    };
    decode_ok(&step_env(
        "'{{Steps.P.Output.Count > 0 and Step.Name == \"C\"}}'",
    ));
    assert_errs(
        &step_env("'{{use_env}}'"),
        &[(
            "steps[3] -> stepEnvironments[0] -> enableIf",
            "Failed to parse interpolation expression at [0, 11]. Undefined variable: 'use_env'.\n  use_env\n  ^~~~~~~",
        )],
    );
    assert_errs(
        &step_env("'{{Steps.P.Output.Count}}'"),
        &[(
            "steps[3] -> stepEnvironments[0] -> enableIf",
            "Failed to parse interpolation expression at [0, 24]. Cannot coerce int to bool\n  Steps.P.Output.Count\n  ~~~~~~~~~~~~~~~^~~~~",
        )],
    );
}

// ══════════════════════════════════════════════════════════════
// create_job: its own checks see the same Steps.* symbols
// ══════════════════════════════════════════════════════════════

/// Decode and create a Job with the parameter defaults. `R`'s `enableIf` is
/// made the literal `true`, which `create_job` instantiates (it refuses any
/// other `enableIf` until step input binding exists); `R`'s outputs stay
/// `T?`.
fn create(template: &str) -> Result<openjd_model::job::Job, openjd_model::ModelError> {
    let template = template.replacen("enableIf: '{{Param.Run}}'", "enableIf: true", 1);
    let jt = decode_ok(&template);
    let dir = std::env::temp_dir();
    let dir = dir.to_str().unwrap();
    let params = preprocess_job_parameters(
        &jt,
        &Default::default(),
        &[],
        &PathParameterOptions::new(dir, dir),
    )
    .unwrap();
    create_job(&jt, &params, &jt.default_validation_context())
}

#[test]
fn create_job_accepts_steps_in_carried_forward_fields() {
    let job = create(&template(
        "    stepEnvironments:
      - name: E
        variables: {A: '{{Steps.P.Output.F}}', B: '{{Steps.Q.TaskParam.D[0]}}'}
        script:
          let: ['g = Steps.P.Output.Fs']
          actions: {onEnter: {command: echo, args: ['{{g}}'], timeout: '{{Steps.P.Output.Count}}'}}
    script:
      let: ['s = Steps.Q.Output.T']
      embeddedFiles:
        - {name: Data, type: TEXT, data: '{{Steps.P.RawOutput.F}}'}
      actions:
        onRun:
          command: echo
          args: ['{{s}}', '{{Steps.P.Output.F.name}}', '{{Steps.R.Output.Go}}']
          timeout: '{{Steps.P.Output.Count + 60}}'
          cancelation: {mode: NOTIFY_THEN_TERMINATE, notifyPeriodInSeconds: '{{Steps.P.Output.Count}}'}
",
    ))
    .expect("create_job's carried-forward checks see the Steps.* symbols");
    // The unresolved placeholders are not part of the Step's resolved symbol
    // table: only concrete values go there.
    let c = &job.steps[3];
    let symtab = serde_json::to_value(c.resolved_symtab.as_ref().unwrap()).unwrap();
    assert!(
        !symtab.to_string().contains("Steps"),
        "resolved_symtab holds Steps.*: {symtab}"
    );
}

#[test]
fn create_job_still_reports_a_carried_forward_violation_next_to_steps() {
    // With timeout 0 static, the job-creation re-check fails; the Steps.*
    // symbol beside it must not mask the error.
    let template = template(
        "    script:
      actions:
        onRun:
          command: echo
          args: ['{{Steps.P.Output.Count}}']
          timeout: '{{0 if Param.Run else Steps.P.Output.Count}}'
",
    );
    let err = create(&template).expect_err("timeout 0 is invalid");
    assert_eq!(
        err.to_string(),
        expected(&[(
            "steps[3] -> script -> actions -> onRun -> timeout",
            "timeout must be > 0.",
        )])
    );
}

#[test]
fn create_job_refuses_materialization_fields_that_read_steps() {
    let msg = "reads Steps.* symbols, which are bound at step input binding; create_job does not implement step input binding yet.";
    let err = create(&template(
        "    let:
      - a = Param.Run
      - n = Steps.P.Output.Count
      - m = n * 2
    hostRequirements:
      amounts:
        - {name: amount.worker.vcpu, min: '{{m}}'}
        - {name: amount.worker.gpu, min: '{{Steps.P.Output.Count}}'}
    parameterSpace:
      taskParameterDefinitions:
        - {name: X, type: INT, range: [1, 2]}
        - {name: W, type: INT, range: '1-{{Steps.P.Output.Count}}'}
        - {name: Z, type: STRING, range: ['{{Steps.P.RawOutput.F}}']}
    script: {actions: {onRun: {command: echo, args: ['{{m}}']}}}
",
    ))
    .expect_err("create_job does not bind Step inputs");
    assert_eq!(
        err.to_string(),
        expected(&[
            ("steps[3] -> let[1]", msg),
            ("steps[3] -> hostRequirements", msg),
            (
                "steps[3] -> parameterSpace -> taskParameterDefinitions[1]",
                msg
            ),
            (
                "steps[3] -> parameterSpace -> taskParameterDefinitions[2]",
                msg
            ),
        ])
    );
}

#[test]
fn create_job_refusal_is_reported_with_other_steps_errors() {
    // A second consumer with a carried-forward violation: both Steps' errors
    // are reported together.
    let t = template_with(
        EXTENSIONS,
        "",
        "    let: ['n = Steps.P.Output.Count']
    script: {actions: {onRun: {command: echo}}}
  - name: D
    dependencies: [{dependsOn: P}]
    script: {actions: {onRun: {command: echo, args: ['{{Steps.P.Output.Count}}'], timeout: '{{0 if Param.Run else 1}}'}}}
",
    );
    let err = create(&t).expect_err("both Steps fail");
    assert_eq!(
        err.to_string(),
        expected(&[
            (
                "steps[3] -> let[0]",
                "reads Steps.* symbols, which are bound at step input binding; create_job does not implement step input binding yet.",
            ),
            (
                "steps[4] -> script -> actions -> onRun -> timeout",
                "timeout must be > 0.",
            ),
        ])
    );
}
