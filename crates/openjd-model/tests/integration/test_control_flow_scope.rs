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
    // Enough symbols that a hash-set order is very unlikely to be sorted.
    assert_errs(
        &c_arg(
            "",
            "{{[Steps.Q.Output.Zed, Steps.Nope.Output.E, Steps.Nope.Output.B, Steps.Q.Output.Ya, Steps.Nope.Output.D, Steps.Nope.Output.A, Steps.Nope.Output.C]}}",
        ),
        &[
            (ARG0, &nope("A")),
            (ARG0, &nope("B")),
            (ARG0, &nope("C")),
            (ARG0, &nope("D")),
            (ARG0, &nope("E")),
            (ARG0, "'Steps.Q.Output.Ya': Step 'Q' has no output variable 'Ya'."),
            (ARG0, "'Steps.Q.Output.Zed': Step 'Q' has no output variable 'Zed'."),
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
fn a_path_message_names_the_symbol_not_a_property_read_from_it() {
    assert_errs(
        &c_arg("    let: ['n = Steps.P.Output.F.name']\n", "{{n}}"),
        &[(
            "steps[3] -> let[0]",
            "Invalid expression in let binding 'n': 'Steps.P.Output.F' has a path type, so it is available only in fields resolved on the worker host. Use Steps.P.RawOutput.F here.",
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
        // Each alone: an unresolved left operand of `and` would hide an
        // undefined name on its right.
        "'{{Job.Name == \"J\"}}'",
        "'{{Step.Name == \"C\"}}'",
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
fn enable_if_text_resolves_to_a_string_coerced_to_bool() {
    // Expression Language §1.3.2 and §1.2.3: a literal, or text around an
    // expression, resolves to a string, converted with string -> bool.
    for value in [
        "'yes'",
        "'True'",
        "'off'",
        "'0'",
        "\"{{'y'}}es\"",
        // Not static: converted when it is evaluated.
        "'{{Param.Run}} '",
        "'{{Steps.P.Output.Count}}{{Param.Run}}'",
    ] {
        decode_ok(&c_enable_if(value));
    }
    for (value, text) in [
        ("'maybe'", "maybe"),
        ("'true '", "true "),
        ("'x{{1}}'", "x1"),
        ("'{{true}}{{true}}'", "truetrue"),
    ] {
        assert_errs(
            &c_enable_if(value),
            &[(
                "steps[3] -> enableIf",
                &format!("must resolve to a bool: Cannot convert '{text}' to bool."),
            )],
        );
    }
    // The references of a multi-segment string are still checked.
    assert_errs(
        &c_enable_if("'x{{Steps.Nope.Output.A}}'"),
        &[(
            "steps[3] -> enableIf",
            "'Steps.Nope.Output.A': there is no Step named 'Nope'.",
        )],
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
    decode_ok(&job_env("'{{Param.Run}}'"));
    decode_ok(&job_env("'{{Job.Name == \"J\"}}'"));
    decode_ok(&job_env("'{{RawParam.Scene != \"\"}}'"));
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
    decode_ok(&step_env("'{{Steps.P.Output.Count > 0}}'"));
    decode_ok(&step_env("'{{Step.Name == \"C\"}}'"));
    decode_ok(&step_env("'{{Job.Name == \"J\"}}'"));
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
    // The Step is instantiated. (Whether placeholders reach its table is not
    // observable here: a SerializedSymbolTable drops Unresolved values.)
    assert_eq!(job.steps[3].name, "C");
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

// ══════════════════════════════════════════════════════════════
// Every field family gets the specific message
// ══════════════════════════════════════════════════════════════

/// `'Steps.Nope.Output.<tag>': there is no Step named 'Nope'.`
fn nope(tag: &str) -> String {
    format!("'Steps.Nope.Output.{tag}': there is no Step named 'Nope'.")
}

#[test]
fn every_template_scope_field_family_explains_a_bad_reference() {
    let t = template(
        "    hostRequirements:
      amounts:
        - {name: '{{Steps.Nope.Output.AmtName}}', min: '{{Steps.Nope.Output.AmtMin}}', max: '{{Steps.Nope.Output.AmtMax}}'}
      attributes:
        - {name: '{{Steps.Nope.Output.AttrName}}', anyOf: ['{{Steps.Nope.Output.AnyOf}}']}
        - {name: attr.worker.os.family, allOf: ['{{Steps.Nope.Output.AllOf}}']}
    parameterSpace:
      taskParameterDefinitions:
        - {name: I, type: INT, range: '{{Steps.Nope.Output.IntRange}}'}
        - {name: D, type: PATH, range: ['{{Steps.Nope.Output.PathItem}}']}
        - {name: K, type: 'CHUNK[INT]', range: '{{Steps.Nope.Output.ChunkRange}}', chunks: {defaultTaskCount: '{{Steps.Nope.Output.Count}}', targetRuntimeSeconds: '{{Steps.Nope.Output.Target}}', rangeConstraint: CONTIGUOUS}}
    script:
      actions:
        onRun:
          command: echo
          timeout: '{{Steps.Nope.Output.Timeout}}'
          cancelation: {mode: NOTIFY_THEN_TERMINATE, notifyPeriodInSeconds: '{{Steps.Nope.Output.Notify}}'}
",
    );
    let hr = "steps[3] -> hostRequirements";
    let tpd = "steps[3] -> parameterSpace -> taskParameterDefinitions";
    let onrun = "steps[3] -> script -> actions -> onRun";
    let paths = [
        format!("{hr} -> amounts[0] -> name"),
        format!("{hr} -> amounts[0] -> min"),
        format!("{hr} -> amounts[0] -> max"),
        format!("{hr} -> attributes[0] -> name"),
        format!("{hr} -> attributes[0] -> anyOf[0]"),
        format!("{hr} -> attributes[1] -> allOf[0]"),
        format!("{tpd}[0] -> range"),
        format!("{tpd}[1] -> range[0]"),
        format!("{tpd}[2] -> range"),
        format!("{tpd}[2] -> chunks -> defaultTaskCount"),
        format!("{tpd}[2] -> chunks -> targetRuntimeSeconds"),
        format!("{onrun} -> timeout"),
        format!("{onrun} -> cancelation"),
    ];
    let tags = [
        "AmtName",
        "AmtMin",
        "AmtMax",
        "AttrName",
        "AnyOf",
        "AllOf",
        "IntRange",
        "PathItem",
        "ChunkRange",
        "Count",
        "Target",
        "Timeout",
        "Notify",
    ];
    let messages: Vec<String> = tags.iter().map(|t| nope(t)).collect();
    let errors: Vec<(&str, &str)> = paths
        .iter()
        .zip(&messages)
        .map(|(p, m)| (p.as_str(), m.as_str()))
        .collect();
    assert_errs(&t, &errors);
}

#[test]
fn every_step_environment_field_family_explains_a_bad_reference() {
    let t = template(
        "    stepEnvironments:
      - name: E
        variables: {V: '{{Steps.Nope.Output.Var}}'}
        script:
          let: ['x = Steps.Nope.Output.Let']
          actions:
            onEnter: {command: '{{Steps.Nope.Output.Cmd}}', args: ['{{Steps.Nope.Output.Arg}}'], timeout: '{{Steps.Nope.Output.Timeout}}'}
          embeddedFiles:
            - {name: Data, type: TEXT, data: '{{Steps.Nope.Output.Data}}'}
    script: {actions: {onRun: {command: echo}}}
",
    );
    let e = "steps[3] -> stepEnvironments[0]";
    let let_msg = format!("Invalid expression in let binding 'x': {}", nope("Let"));
    let (var, cmd, arg, timeout, data) = (
        nope("Var"),
        nope("Cmd"),
        nope("Arg"),
        nope("Timeout"),
        nope("Data"),
    );
    assert_errs(
        &t,
        &[
            (&format!("{e} -> script -> let[0]"), &let_msg),
            (&format!("{e} -> variables -> V"), &var),
            (
                &format!("{e} -> script -> actions -> onEnter -> command"),
                &cmd,
            ),
            (
                &format!("{e} -> script -> actions -> onEnter -> args[0]"),
                &arg,
            ),
            (
                &format!("{e} -> script -> actions -> onEnter -> timeout"),
                &timeout,
            ),
            (&format!("{e} -> script -> embeddedFiles[0] -> data"), &data),
        ],
    );
}

#[test]
fn simple_action_template_scope_fields_explain_a_bad_reference() {
    assert_errs(
        &template("    bash: {script: echo, timeout: '{{Steps.Nope.Output.Timeout}}'}\n"),
        &[("steps[3] -> bash -> timeout", &nope("Timeout"))],
    );
}

#[test]
fn a_bad_reference_in_a_let_read_by_a_range_is_reported_once() {
    assert_errs(
        &template(
            "    let: ['x = Steps.Nope.Output.A']
    parameterSpace:
      taskParameterDefinitions:
        - {name: I, type: INT, range: '{{x}}'}
    script: {actions: {onRun: {command: echo}}}
",
        ),
        &[(
            "steps[3] -> let[0]",
            &format!("Invalid expression in let binding 'x': {}", nope("A")),
        )],
    );
}

#[test]
fn a_bad_reference_in_a_let_of_a_step_with_host_requirements() {
    // Pass 8 evaluates a Step's `let` bindings once for its hostRequirements
    // and once for its other fields, and reports a failing binding from
    // both: a duplicate that predates CONTROL_FLOW (any failing `let`
    // shows it). Both carry the specific message.
    let msg = format!("Invalid expression in let binding 'x': {}", nope("A"));
    assert_errs(
        &template(
            "    let: ['x = Steps.Nope.Output.A']
    hostRequirements:
      amounts: [{name: amount.worker.vcpu, min: '{{x}}'}]
    script: {actions: {onRun: {command: echo}}}
",
        ),
        &[("steps[3] -> let[0]", &msg), ("steps[3] -> let[0]", &msg)],
    );
}

#[test]
fn a_bare_steps_symbol_is_incomplete() {
    assert_errs(
        &c_arg("", "{{len(Steps)}}"),
        &[(
            ARG0,
            "'Steps' is not a complete Steps.* reference; expected Steps.<StepName>.Output.<Name>, .RawOutput.<Name>, .TaskParam.<Name>, or .TaskRawParam.<Name>.",
        )],
    );
}

#[test]
fn a_field_without_steps_reports_that_before_an_incomplete_reference() {
    let t = template(ECHO).replacen("name: J\n", "name: '{{Steps.P}}'\n", 1);
    assert_errs(
        &t,
        &[(
            "name",
            "'Steps.P': Steps.* symbols are not available in this field. Only a Step's enableIf, let, hostRequirements, parameterSpace, stepEnvironments, and script may read them.",
        )],
    );
}

#[test]
fn a_non_identifier_step_is_reported_before_the_missing_dependency() {
    // Adding `dependsOn` would not help, so the Identifier rule wins.
    let t = "specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: J
steps:
  - name: Café
    outputVariables: [{name: Count, type: INT}]
    script: {actions: {onRun: {command: echo}}}
  - name: C
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
fn without_control_flow_no_steps_symbol_is_seeded_or_explained() {
    // A producer with Task parameters, so a seeded TaskParam would resolve.
    let t = "specificationVersion: jobtemplate-2023-09
extensions: [EXPR]
name: '{{Steps.P.TaskParam.X[0]}}'
steps:
  - name: P
    parameterSpace:
      taskParameterDefinitions: [{name: X, type: STRING, range: [a]}]
    script: {actions: {onRun: {command: echo}}}
  - name: C
    dependencies: [{dependsOn: P}]
    script: {actions: {onRun: {command: echo, args: ['{{Steps.P.TaskParam.X}}']}}}
";
    assert_eq!(
        decode(t).unwrap_err().to_string(),
        expected(&[
            (
                "name",
                "Failed to parse interpolation expression at [0, 26]. Undefined variable: 'Steps.P.TaskParam.X'.\n  Steps.P.TaskParam.X[0]\n  ~~~~~~~~~~~~~~~~~~^",
            ),
            (
                "steps[1] -> script -> actions -> onRun -> args[0]",
                "Failed to parse interpolation expression at [0, 23]. Undefined variable: 'Steps.P.TaskParam.X'.\n  Steps.P.TaskParam.X\n  ~~~~~~~~~~~~~~~~~~^",
            ),
        ])
    );
}

#[test]
fn create_job_refuses_each_materialization_field_family() {
    // One consumer per field family, each reading Steps.* in that field
    // alone, so a field the refusal scan missed would fail resolution with
    // a different message.
    let families = [
        ("hostRequirements: {amounts: [{name: '{{Steps.P.RawOutput.F.name}}', min: 1}]}", "hostRequirements"),
        ("hostRequirements: {amounts: [{name: amount.worker.vcpu, min: '{{Steps.P.Output.Count}}'}]}", "hostRequirements"),
        ("hostRequirements: {amounts: [{name: amount.worker.vcpu, max: '{{Steps.P.Output.Count}}'}]}", "hostRequirements"),
        ("hostRequirements: {attributes: [{name: '{{Steps.P.RawOutput.F.name}}', anyOf: [linux]}]}", "hostRequirements"),
        ("hostRequirements: {attributes: [{name: attr.worker.os.family, anyOf: ['{{Steps.P.RawOutput.F.name}}']}]}", "hostRequirements"),
        ("hostRequirements: {attributes: [{name: attr.worker.os.family, allOf: ['{{Steps.P.RawOutput.F.name}}']}]}", "hostRequirements"),
        ("parameterSpace: {taskParameterDefinitions: [{name: X, type: INT, range: '1-{{Steps.P.Output.Count}}'}]}", "parameterSpace -> taskParameterDefinitions[0]"),
        ("parameterSpace: {taskParameterDefinitions: [{name: X, type: 'CHUNK[INT]', range: '1-{{Steps.P.Output.Count}}', chunks: {defaultTaskCount: 1, rangeConstraint: CONTIGUOUS}}]}", "parameterSpace -> taskParameterDefinitions[0]"),
        ("parameterSpace: {taskParameterDefinitions: [{name: X, type: 'CHUNK[INT]', range: '1-5', chunks: {defaultTaskCount: '{{Steps.P.Output.Count}}', rangeConstraint: CONTIGUOUS}}]}", "parameterSpace -> taskParameterDefinitions[0]"),
        ("parameterSpace: {taskParameterDefinitions: [{name: X, type: 'CHUNK[INT]', range: '1-5', chunks: {defaultTaskCount: 1, targetRuntimeSeconds: '{{Steps.P.Output.Count}}', rangeConstraint: CONTIGUOUS}}]}", "parameterSpace -> taskParameterDefinitions[0]"),
        ("parameterSpace: {taskParameterDefinitions: [{name: X, type: FLOAT, range: [1.5, '{{Steps.P.Output.Count}}']}]}", "parameterSpace -> taskParameterDefinitions[0]"),
        ("parameterSpace: {taskParameterDefinitions: [{name: X, type: FLOAT, range: '{{[Steps.P.Output.Count]}}'}]}", "parameterSpace -> taskParameterDefinitions[0]"),
        ("parameterSpace: {taskParameterDefinitions: [{name: X, type: STRING, range: ['{{Steps.P.Output.Count}}']}]}", "parameterSpace -> taskParameterDefinitions[0]"),
        ("parameterSpace: {taskParameterDefinitions: [{name: X, type: STRING, range: '{{[string(Steps.P.Output.Count)]}}'}]}", "parameterSpace -> taskParameterDefinitions[0]"),
        ("parameterSpace: {taskParameterDefinitions: [{name: X, type: PATH, range: ['{{Steps.P.RawOutput.F}}']}]}", "parameterSpace -> taskParameterDefinitions[0]"),
        ("parameterSpace: {taskParameterDefinitions: [{name: X, type: PATH, range: '{{Steps.P.RawOutput.Fs}}'}]}", "parameterSpace -> taskParameterDefinitions[0]"),
    ];
    let mut steps = String::from(ECHO);
    for (k, (field, _)) in families.iter().enumerate() {
        steps.push_str(&format!(
            "  - name: D{k}\n    dependencies: [{{dependsOn: P}}]\n    {field}\n{ECHO}"
        ));
    }
    let err = create(&template(&steps)).expect_err("every D Step is refused");
    let msg = "reads Steps.* symbols, which are bound at step input binding; create_job does not implement step input binding yet.";
    let paths: Vec<String> = families
        .iter()
        .enumerate()
        .map(|(k, (_, at))| format!("steps[{}] -> {at}", k + 4))
        .collect();
    let errors: Vec<(&str, &str)> = paths.iter().map(|p| (p.as_str(), msg)).collect();
    assert_eq!(err.to_string(), expected(&errors));
}

// ══════════════════════════════════════════════════════════════
// Boundaries of the checks
// ══════════════════════════════════════════════════════════════

#[test]
fn a_symbol_that_only_starts_with_steps_is_not_under_the_steps_root() {
    assert_errs(
        &c_arg("", "{{StepsFoo}}"),
        &[(
            ARG0,
            "Failed to parse interpolation expression at [0, 12]. Undefined variable: 'StepsFoo'. Did you mean: Step.Name\n  StepsFoo\n  ^~~~~~~~",
        )],
    );
}

#[test]
fn a_bad_let_read_by_a_range_is_reported_once_for_calls_indexing_and_arithmetic() {
    assert_errs(
        &template(
            "    let: ['x = Steps.Nope.Output.A']
    parameterSpace:
      taskParameterDefinitions:
        - {name: S, type: STRING, range: ['{{x.upper()}}', '{{x[0]}}', '{{x + 1}}']}
    script: {actions: {onRun: {command: echo}}}
",
        ),
        &[(
            "steps[3] -> let[0]",
            &format!("Invalid expression in let binding 'x': {}", nope("A")),
        )],
    );
}

#[test]
fn environment_enable_if_may_not_read_a_path_param() {
    let msg = "Failed to parse interpolation expression at [0, 23]. Undefined variable: 'Param.Scene'. Did you mean: RawParam.Scene\n  Param.Scene != null\n  ~~~~~~^~~~~";
    let value = "'{{Param.Scene != null}}'";
    let job_env = template_with(
        EXTENSIONS,
        &format!("jobEnvironments:\n  - {{name: JE, enableIf: {value}, variables: {{A: a}}}}\n"),
        ECHO,
    );
    assert_errs(&job_env, &[("jobEnvironments[0] -> enableIf", msg)]);
    let step_env = template(&format!(
        "    stepEnvironments:\n      - {{name: SE, enableIf: {value}, variables: {{A: a}}}}\n{ECHO}"
    ));
    assert_errs(
        &step_env,
        &[("steps[3] -> stepEnvironments[0] -> enableIf", msg)],
    );
}

#[test]
fn without_control_flow_enable_if_is_only_gated() {
    // Pass 8 does not type-check an enableIf without the extension: the
    // gate is the only error, even for a value that would fail it.
    let t = "specificationVersion: jobtemplate-2023-09
extensions: [EXPR]
name: J
jobEnvironments:
  - {name: JE, enableIf: '{{Nope}}', variables: {A: a}}
steps:
  - name: S
    enableIf: 'maybe'
    stepEnvironments:
      - {name: SE, enableIf: '{{Nope}}', variables: {A: a}}
    script: {actions: {onRun: {command: echo}}}
";
    let gate = "enableIf requires the CONTROL_FLOW extension.";
    assert_errs(
        t,
        &[
            ("jobEnvironments[0] -> enableIf", gate),
            ("steps[0] -> enableIf", gate),
            ("steps[0] -> stepEnvironments[0] -> enableIf", gate),
        ],
    );
}

#[test]
fn a_dotted_step_name_does_not_collide_with_another_steps_symbols() {
    // `A.Output` is not an <Identifier>, so its symbols are not seeded and
    // cannot overlap Step `A`'s output `Output`.
    decode_ok(
        "specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: J
steps:
  - name: A
    outputVariables: [{name: Output, type: INT}]
    script: {actions: {onRun: {command: echo}}}
  - name: A.Output
    outputVariables: [{name: X, type: INT}]
    script: {actions: {onRun: {command: echo}}}
  - name: C
    dependencies: [{dependsOn: A}, {dependsOn: A.Output}]
    script: {actions: {onRun: {command: echo, args: ['{{Steps.A.Output.Output}}']}}}
",
    );
}

#[test]
fn check_order_identifier_before_self_and_existence_before_identifier() {
    let t = "specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: J
steps:
  - name: Café
    outputVariables: [{name: Num, type: INT}]
    script: {actions: {onRun: {command: echo, args: ['{{Steps.Café.Output.Num}}', '{{Steps.Thé.Output.Num}}']}}}
";
    let args = "steps[0] -> script -> actions -> onRun -> args";
    assert_errs(
        t,
        &[
            (
                &format!("{args}[0]"),
                "'Steps.Café.Output.Num': Step 'Café' cannot be referenced through Steps.*, because its name is not an <Identifier>.",
            ),
            (
                &format!("{args}[1]"),
                "'Steps.Thé.Output.Num': there is no Step named 'Thé'.",
            ),
        ],
    );
}

#[test]
fn check_order_dependency_before_member() {
    let t = template_with(
        EXTENSIONS,
        "",
        "    script: {actions: {onRun: {command: echo}}}
  - name: D
    script: {actions: {onRun: {command: echo, args: ['{{Steps.P.Bogus.A}}']}}}
",
    );
    assert_errs(
        &t,
        &[(
            "steps[4] -> script -> actions -> onRun -> args[0]",
            "'Steps.P.Bogus.A': Step 'P' is not a direct dependency of Step 'D'. Add 'dependsOn: P' to its dependencies to read Steps.P.*.",
        )],
    );
}

#[test]
fn enable_if_may_not_read_any_session_symbol() {
    // `Session.HasPathMappingRules` is covered above.
    assert_errs(
        &c_enable_if("'{{Session.WorkingDirectory != null}}'"),
        &[(
            "steps[3] -> enableIf",
            "Failed to parse interpolation expression at [0, 36]. Undefined variable: 'Session.WorkingDirectory'.\n  Session.WorkingDirectory != null\n  ~~~~~~~~^~~~~~~~~~~~~~~~",
        )],
    );
    assert_errs(
        &c_enable_if("'{{Session.PathMappingRulesFile != null}}'"),
        &[(
            "steps[3] -> enableIf",
            "Failed to parse interpolation expression at [0, 40]. Undefined variable: 'Session.PathMappingRulesFile'.\n  Session.PathMappingRulesFile != null\n  ~~~~~~~~^~~~~~~~~~~~~~~~~~~~",
        )],
    );
}

#[test]
fn environment_enable_if_may_not_read_session_env_file_or_its_own_let() {
    // RFC 0010 <Environment> enableIf constraints 2 and 3, at both
    // locations: no Session.*, no Env.*, no names bound by let (here the
    // Environment's own script let), and for a Step Environment no Task.*.
    let env = |value: &str| {
        format!(
            "{{name: E, enableIf: {value}, script: {{let: ['x = true'], embeddedFiles: [{{name: Cfg, type: TEXT, data: d}}], actions: {{onEnter: {{command: echo}}}}}}}}"
        )
    };
    let cases = [
        (
            "'{{Session.WorkingDirectory != null}}'",
            "Failed to parse interpolation expression at [0, 36]. Undefined variable: 'Session.WorkingDirectory'.\n  Session.WorkingDirectory != null\n  ~~~~~~~~^~~~~~~~~~~~~~~~",
        ),
        (
            "'{{Env.File.Cfg != null}}'",
            "Failed to parse interpolation expression at [0, 24]. Undefined variable: 'Env.File.Cfg'.\n  Env.File.Cfg != null\n  ~~~~~~~~~^~~",
        ),
        (
            "'{{x}}'",
            "Failed to parse interpolation expression at [0, 5]. Undefined variable: 'x'.\n  x\n  ^",
        ),
    ];
    for (value, msg) in cases {
        let job_env = template_with(
            EXTENSIONS,
            &format!("jobEnvironments:\n  - {}\n", env(value)),
            ECHO,
        );
        assert_errs(&job_env, &[("jobEnvironments[0] -> enableIf", msg)]);
        let step_env = template(&format!(
            "    stepEnvironments:\n      - {}\n{ECHO}",
            env(value)
        ));
        assert_errs(
            &step_env,
            &[("steps[3] -> stepEnvironments[0] -> enableIf", msg)],
        );
    }
    let step_env = template(&format!(
        "    parameterSpace:
      taskParameterDefinitions: [{{name: X, type: INT, range: [1]}}]
    stepEnvironments:
      - {}
{ECHO}",
        env("'{{Task.Param.X > 1}}'")
    ));
    assert_errs(
        &step_env,
        &[(
            "steps[3] -> stepEnvironments[0] -> enableIf",
            "Failed to parse interpolation expression at [0, 20]. Undefined variable: 'Task.Param.X'.\n  Task.Param.X > 1\n  ~~~~~~~~~~~^",
        )],
    );
}
