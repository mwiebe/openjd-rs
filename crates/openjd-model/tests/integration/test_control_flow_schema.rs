// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `CONTROL_FLOW` (RFC 0010) schema, gating, and structural validation:
//! `outputVariables`, `enableIf`, and validation pass 11.
//!
//! Failure tests assert the full error output (count, path, and message), as
//! `AGENTS.md` § Test Quality Standard requires.

use openjd_model::template::{BoolOrFormatString, JobParameterDefinition};
use openjd_model::{
    decode_environment_template, decode_job_template, CallerLimits, JobParameterType,
    ModelExtension, OutputCollectMode,
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
        Err(e) => panic!("expected the template to decode, got:\n{e}"),
    }
}

/// Assert the complete error output: the count line and each `(path, message)`.
fn assert_errs(template: &str, model: &str, errors: &[(&str, &str)]) {
    let err = decode(template).expect_err("expected a validation error");
    assert_eq!(err.to_string(), expected(model, errors));
}

fn expected(model: &str, errors: &[(&str, &str)]) -> String {
    let n = errors.len();
    let word = if n == 1 { "error" } else { "errors" };
    let mut out = format!("Model validation error: {n} validation {word} for {model}");
    for (path, msg) in errors {
        if path.is_empty() {
            out.push_str(&format!("\n{model}: {msg}"));
        } else {
            out.push_str(&format!("\n{path}:\n\t{msg}"));
        }
    }
    out
}

/// Assert a decode-time (serde) failure, which has no path.
fn assert_decode_err(template: &str, message: &str) {
    let err = decode(template).expect_err("expected a decode error");
    assert_eq!(
        err.to_string(),
        format!("Validation error: 'jobtemplate-2023-09' failed checks: {message}")
    );
}

/// A one-Step job template with the given extensions and extra Step fields
/// (each a complete `key: value` line in flow style).
fn job(extensions: &str, step_fields: &[&str]) -> String {
    let mut fields = String::new();
    for f in step_fields {
        fields.push_str("    ");
        fields.push_str(f);
        fields.push('\n');
    }
    format!(
        "specificationVersion: jobtemplate-2023-09
extensions: {extensions}
name: J
steps:
  - name: S
{fields}    script:
      actions:
        onRun:
          command: echo
"
    )
}

/// A `CONTROL_FLOW` job template whose Step declares `outputs` (flow YAML).
fn with_outputs(outputs: &str) -> String {
    job(
        "[EXPR, CONTROL_FLOW]",
        &[&format!("outputVariables: {outputs}")],
    )
}

// ══════════════════════════════════════════════════════════════
// The extension
// ══════════════════════════════════════════════════════════════

#[test]
fn extension_name_round_trips() {
    use std::str::FromStr;
    assert_eq!(ModelExtension::ControlFlow.as_str(), "CONTROL_FLOW");
    assert_eq!(
        ModelExtension::from_str("CONTROL_FLOW"),
        Ok(ModelExtension::ControlFlow)
    );
    assert!(ModelExtension::ALL.contains(&ModelExtension::ControlFlow));
    assert_eq!(
        serde_json::to_value(ModelExtension::ControlFlow).unwrap(),
        serde_json::json!("CONTROL_FLOW")
    );
}

#[test]
fn control_flow_without_features_is_valid() {
    decode_ok(&job("[EXPR, CONTROL_FLOW]", &[]));
}

#[test]
fn control_flow_requires_expr() {
    assert_errs(
        &job("[CONTROL_FLOW]", &[]),
        "JobTemplate",
        &[(
            "extensions",
            "CONTROL_FLOW requires EXPR; both must be listed in the template's `extensions` (RFC 0010).",
        )],
    );
}

#[test]
fn control_flow_must_be_supported_by_the_caller() {
    let err = decode_job_template(
        yaml(&job("[EXPR, CONTROL_FLOW]", &[])),
        Some(&["EXPR"]),
        &CallerLimits::default(),
    )
    .expect_err("CONTROL_FLOW is not in the supported list");
    assert_eq!(
        err.to_string(),
        expected(
            "JobTemplate",
            &[("extensions", "Unsupported extension names: CONTROL_FLOW")]
        )
    );
}

// ══════════════════════════════════════════════════════════════
// Gating: each new field is an error at its own path without the extension
// ══════════════════════════════════════════════════════════════

#[test]
fn output_variables_require_the_extension() {
    assert_errs(
        &job("[EXPR]", &["outputVariables: [{name: 'N', type: INT}]"]),
        "JobTemplate",
        &[(
            "steps[0] -> outputVariables",
            "outputVariables requires the CONTROL_FLOW extension.",
        )],
    );
}

#[test]
fn step_enable_if_requires_the_extension() {
    assert_errs(
        &job("[EXPR]", &["enableIf: true"]),
        "JobTemplate",
        &[(
            "steps[0] -> enableIf",
            "enableIf requires the CONTROL_FLOW extension.",
        )],
    );
}

#[test]
fn step_enable_if_requires_the_extension_with_no_extensions() {
    let t = "specificationVersion: jobtemplate-2023-09
name: J
steps:
  - name: S
    enableIf: false
    script: {actions: {onRun: {command: echo}}}
";
    assert_errs(
        t,
        "JobTemplate",
        &[(
            "steps[0] -> enableIf",
            "enableIf requires the CONTROL_FLOW extension.",
        )],
    );
}

#[test]
fn environment_enable_if_requires_the_extension() {
    let t = "specificationVersion: jobtemplate-2023-09
extensions: [EXPR]
name: J
jobEnvironments:
  - name: JE
    enableIf: true
    variables: {A: a}
steps:
  - name: S
    stepEnvironments:
      - name: SE
        enableIf: '{{Param.X}}'
        variables: {B: b}
    script: {actions: {onRun: {command: echo}}}
";
    assert_errs(
        t,
        "JobTemplate",
        &[
            (
                "jobEnvironments[0] -> enableIf",
                "enableIf requires the CONTROL_FLOW extension.",
            ),
            (
                "steps[0] -> stepEnvironments[0] -> enableIf",
                "enableIf requires the CONTROL_FLOW extension.",
            ),
        ],
    );
}

#[test]
fn every_gated_field_is_reported() {
    let t = "specificationVersion: jobtemplate-2023-09
extensions: [EXPR]
name: J
steps:
  - name: S
    enableIf: true
    outputVariables: [{name: 'N', type: INT}]
    script: {actions: {onRun: {command: echo}}}
";
    assert_errs(
        t,
        "JobTemplate",
        &[
            (
                "steps[0] -> enableIf",
                "enableIf requires the CONTROL_FLOW extension.",
            ),
            (
                "steps[0] -> outputVariables",
                "outputVariables requires the CONTROL_FLOW extension.",
            ),
        ],
    );
}

// ══════════════════════════════════════════════════════════════
// Environment Templates
// ══════════════════════════════════════════════════════════════

fn env_template(extensions: &str, env_fields: &str) -> String {
    format!(
        "specificationVersion: environment-2023-09
extensions: {extensions}
environment:
  name: E
{env_fields}  variables: {{A: a}}
"
    )
}

fn assert_env_errs(template: &str, supported: &[&str], errors: &[(&str, &str)]) {
    let err =
        decode_environment_template(yaml(template), Some(supported), &CallerLimits::default())
            .expect_err("expected a validation error");
    assert_eq!(err.to_string(), expected("EnvironmentTemplate", errors));
}

#[test]
fn environment_template_may_not_list_control_flow() {
    assert_env_errs(
        &env_template("[EXPR, CONTROL_FLOW]", ""),
        SUPPORTED,
        &[(
            "extensions",
            "CONTROL_FLOW is not allowed in an Environment Template; it applies to Job Templates only (RFC 0010).",
        )],
    );
}

#[test]
fn environment_template_may_not_use_enable_if() {
    assert_env_errs(
        &env_template("[EXPR]", "  enableIf: true\n"),
        SUPPORTED,
        &[(
            "environment -> enableIf",
            "enableIf is not allowed in an Environment Template (RFC 0010).",
        )],
    );
}

#[test]
fn environment_template_with_control_flow_and_enable_if_reports_both() {
    assert_env_errs(
        &env_template("[EXPR, CONTROL_FLOW]", "  enableIf: true\n"),
        SUPPORTED,
        &[
            (
                "extensions",
                "CONTROL_FLOW is not allowed in an Environment Template; it applies to Job Templates only (RFC 0010).",
            ),
            (
                "environment -> enableIf",
                "enableIf is not allowed in an Environment Template (RFC 0010).",
            ),
        ],
    );
}

#[test]
fn environment_template_without_control_flow_is_unchanged() {
    decode_environment_template(
        yaml(&env_template("[EXPR]", "")),
        Some(SUPPORTED),
        &CallerLimits::default(),
    )
    .expect("an environment template without CONTROL_FLOW features is valid");
}

// ══════════════════════════════════════════════════════════════
// enableIf
// ══════════════════════════════════════════════════════════════

#[test]
fn enable_if_accepts_a_bool_or_a_format_string() {
    let jt = decode_ok(&job("[EXPR, CONTROL_FLOW]", &["enableIf: false"]));
    assert!(matches!(
        jt.steps[0].enable_if,
        Some(BoolOrFormatString::Bool(false))
    ));

    let jt = decode_ok(&job(
        "[EXPR, CONTROL_FLOW]",
        &["enableIf: '{{Job.Name == \"J\"}}'"],
    ));
    match &jt.steps[0].enable_if {
        Some(BoolOrFormatString::FormatString(fs)) => {
            assert_eq!(fs.raw(), "{{Job.Name == \"J\"}}")
        }
        other => panic!("expected a format string, got {other:?}"),
    }
}

#[test]
fn enable_if_rejects_other_json_types() {
    assert_decode_err(
        &job("[EXPR, CONTROL_FLOW]", &["enableIf: 1"]),
        "expected a bool or a format string, got a number",
    );
}

#[test]
fn enable_if_rejects_a_malformed_format_string() {
    assert_decode_err(
        &job("[EXPR, CONTROL_FLOW]", &["enableIf: '{{Param.X'"]),
        "Failed to parse interpolation expression at [0, 9]. Reason: Braces mismatch.\n  {{Param.X\n  ^~",
    );
}

#[test]
fn enable_if_on_job_and_step_environments_is_accepted() {
    let t = "specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: J
jobEnvironments:
  - name: JE
    enableIf: true
    variables: {A: a}
steps:
  - name: S
    stepEnvironments:
      - name: SE
        enableIf: false
        variables: {B: b}
    script: {actions: {onRun: {command: echo}}}
";
    let jt = decode_ok(t);
    assert!(matches!(
        jt.job_environments.as_ref().unwrap()[0].enable_if,
        Some(BoolOrFormatString::Bool(true))
    ));
    assert!(matches!(
        jt.steps[0].step_environments.as_ref().unwrap()[0].enable_if,
        Some(BoolOrFormatString::Bool(false))
    ));
}

// ══════════════════════════════════════════════════════════════
// outputVariables: types
// ══════════════════════════════════════════════════════════════

#[test]
fn every_output_variable_type_is_accepted() {
    for (ty, expected) in [
        ("INT", JobParameterType::Int),
        ("FLOAT", JobParameterType::Float),
        ("STRING", JobParameterType::String),
        ("PATH", JobParameterType::Path),
        ("BOOL", JobParameterType::Bool),
        ("RANGE_EXPR", JobParameterType::RangeExpr),
        ("LIST[INT]", JobParameterType::ListInt),
        ("LIST[FLOAT]", JobParameterType::ListFloat),
        ("LIST[STRING]", JobParameterType::ListString),
        ("LIST[PATH]", JobParameterType::ListPath),
        ("LIST[BOOL]", JobParameterType::ListBool),
        ("LIST[LIST[INT]]", JobParameterType::ListListInt),
    ] {
        let jt = decode_ok(&with_outputs(&format!("[{{name: V, type: '{ty}'}}]")));
        let ov = &jt.steps[0].output_variables.as_ref().unwrap()[0];
        assert_eq!(ov.name(), "V");
        assert_eq!(ov.job_param_type(), expected, "type {ty}");
        assert_eq!(ov.collect(), OutputCollectMode::Once);
        assert_eq!(ov.unit(), None);
    }
}

#[test]
fn output_variable_type_names_are_case_insensitive() {
    let jt = decode_ok(&with_outputs(
        "[{name: A, type: 'list[int]'}, {name: B, type: Float}]",
    ));
    let ovs = jt.steps[0].output_variables.as_ref().unwrap();
    assert_eq!(ovs[0].job_param_type(), JobParameterType::ListInt);
    assert_eq!(ovs[1].job_param_type(), JobParameterType::Float);
}

#[test]
fn unknown_output_variable_type() {
    assert_decode_err(
        &with_outputs("[{name: V, type: DICT}]"),
        "unknown output variable type: 'DICT'",
    );
}

#[test]
fn missing_output_variable_type() {
    assert_decode_err(
        &with_outputs("[{name: V}]"),
        "missing 'type' field in output variable definition",
    );
}

#[test]
fn output_variable_must_be_a_mapping() {
    assert_decode_err(
        &with_outputs("[Count]"),
        "expected an output variable definition mapping, got a string",
    );
}

#[test]
fn output_variable_name_must_be_an_identifier() {
    assert_decode_err(
        &with_outputs("[{name: 'Frame Count', type: INT}]"),
        "Validation error: Identifier 'Frame Count' does not match pattern [A-Za-z_][A-Za-z0-9_]*",
    );
}

#[test]
fn output_variable_name_length_follows_the_identifier_limit() {
    let name = "N".repeat(65);
    assert_errs(
        &with_outputs(&format!("[{{name: {name}, type: INT}}]")),
        "JobTemplate",
        &[(
            "steps[0] -> outputVariables[0]",
            "name exceeds 64 characters.",
        )],
    );
    // FEATURE_BUNDLE_1 raises the identifier limit, as for job parameters.
    decode_ok(&job(
        "[EXPR, CONTROL_FLOW, FEATURE_BUNDLE_1]",
        &[&format!("outputVariables: [{{name: {name}, type: INT}}]")],
    ));
}

#[test]
fn output_variable_description_is_kept() {
    let jt = decode_ok(&with_outputs(
        "[{name: T, type: FLOAT, description: Render time, unit: s}]",
    ));
    let ov = &jt.steps[0].output_variables.as_ref().unwrap()[0];
    assert_eq!(ov.description(), Some("Render time"));
    assert_eq!(ov.unit(), Some("s"));
    assert!(matches!(ov.definition(), JobParameterDefinition::FLOAT(_)));
}

// ══════════════════════════════════════════════════════════════
// outputVariables: list rules
// ══════════════════════════════════════════════════════════════

fn n_outputs(n: usize) -> String {
    let items: Vec<String> = (0..n)
        .map(|i| format!("{{name: V{i}, type: INT}}"))
        .collect();
    format!("[{}]", items.join(", "))
}

#[test]
fn thirty_two_outputs_are_accepted() {
    let jt = decode_ok(&with_outputs(&n_outputs(32)));
    assert_eq!(jt.steps[0].output_variables.as_ref().unwrap().len(), 32);
}

#[test]
fn thirty_three_outputs_are_rejected() {
    assert_errs(
        &with_outputs(&n_outputs(33)),
        "JobTemplate",
        &[(
            "steps[0] -> outputVariables",
            "must not contain more than 32 elements.",
        )],
    );
}

#[test]
fn empty_output_variables_are_rejected() {
    assert_errs(
        &with_outputs("[]"),
        "JobTemplate",
        &[(
            "steps[0] -> outputVariables",
            "if provided, must contain at least one element.",
        )],
    );
}

#[test]
fn duplicate_output_names_are_rejected() {
    assert_errs(
        &with_outputs("[{name: 'N', type: INT}, {name: M, type: INT}, {name: 'N', type: STRING}]"),
        "JobTemplate",
        &[(
            "steps[0] -> outputVariables[2]",
            "duplicate output variable name: 'N'",
        )],
    );
}

#[test]
fn output_names_are_per_step() {
    let t = "specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: J
parameterDefinitions: [{name: 'N', type: INT}]
steps:
  - name: A
    outputVariables: [{name: 'N', type: INT}]
    script: {actions: {onRun: {command: echo}}}
  - name: B
    outputVariables: [{name: 'N', type: INT}]
    script: {actions: {onRun: {command: echo}}}
";
    decode_ok(t);
}

// ══════════════════════════════════════════════════════════════
// outputVariables: properties an output may not have
// ══════════════════════════════════════════════════════════════

#[test]
fn default_is_not_allowed() {
    assert_errs(
        &with_outputs("[{name: 'N', type: INT, default: 3}]"),
        "JobTemplate",
        &[(
            "steps[0] -> outputVariables[0] -> default",
            "is not allowed in an output variable definition.",
        )],
    );
}

#[test]
fn user_interface_is_not_allowed() {
    assert_errs(
        &with_outputs("[{name: 'N', type: INT, userInterface: {control: SPIN_BOX}}]"),
        "JobTemplate",
        &[(
            "steps[0] -> outputVariables[0] -> userInterface",
            "is not allowed in an output variable definition.",
        )],
    );
}

#[test]
fn data_flow_is_not_allowed_on_paths() {
    assert_errs(
        &with_outputs(
            "[{name: P, type: PATH, dataFlow: OUT}, {name: L, type: 'LIST[PATH]', dataFlow: OUT}]",
        ),
        "JobTemplate",
        &[
            (
                "steps[0] -> outputVariables[0] -> dataFlow",
                "is not allowed in an output variable definition.",
            ),
            (
                "steps[0] -> outputVariables[1] -> dataFlow",
                "is not allowed in an output variable definition.",
            ),
        ],
    );
}

#[test]
fn every_unavailable_property_is_reported() {
    assert_errs(
        &with_outputs(
            "[{name: P, type: PATH, default: /x, userInterface: {control: CHOOSE_DIRECTORY}, dataFlow: IN}]",
        ),
        "JobTemplate",
        &[
            (
                "steps[0] -> outputVariables[0] -> default",
                "is not allowed in an output variable definition.",
            ),
            (
                "steps[0] -> outputVariables[0] -> userInterface",
                "is not allowed in an output variable definition.",
            ),
            (
                "steps[0] -> outputVariables[0] -> dataFlow",
                "is not allowed in an output variable definition.",
            ),
        ],
    );
}

#[test]
fn data_flow_on_a_non_path_type_is_an_unknown_field() {
    assert_decode_err(
        &with_outputs("[{name: S, type: STRING, dataFlow: OUT}]"),
        "unknown field `dataFlow`, expected one of `name`, `description`, `allowedValues`, `minLength`, `maxLength`, `collect`, `unit`",
    );
}

#[test]
fn object_type_is_accepted_on_paths() {
    let jt = decode_ok(&with_outputs(
        "[{name: P, type: PATH, objectType: FILE}, {name: L, type: 'LIST[PATH]', objectType: DIRECTORY}]",
    ));
    let ovs = jt.steps[0].output_variables.as_ref().unwrap();
    assert_eq!(
        ovs[0].definition().path_properties(),
        (Some(openjd_model::ObjectType::File), None)
    );
    assert_eq!(
        ovs[1].definition().path_properties(),
        (Some(openjd_model::ObjectType::Directory), None)
    );
}

// ══════════════════════════════════════════════════════════════
// outputVariables: constraint properties
// ══════════════════════════════════════════════════════════════

#[test]
fn constraints_valid_for_each_type_are_accepted() {
    decode_ok(&with_outputs(
        "[{name: I, type: INT, minValue: 0, maxValue: 10, allowedValues: [0, 5, 10]},
          {name: F, type: FLOAT, minValue: 0.5, maxValue: 2.5},
          {name: S, type: STRING, minLength: 1, maxLength: 4, allowedValues: [a, bb]},
          {name: P, type: PATH, minLength: 1, maxLength: 260},
          {name: R, type: RANGE_EXPR, minLength: 1, maxLength: 10},
          {name: LI, type: 'LIST[INT]', minLength: 1, maxLength: 3, item: {minValue: 0, maxValue: 9, allowedValues: [1, 2]}},
          {name: LF, type: 'LIST[FLOAT]', item: {minValue: 0.0, allowedValues: [1.5]}},
          {name: LS, type: 'LIST[STRING]', item: {minLength: 1, maxLength: 2, allowedValues: [x]}},
          {name: LP, type: 'LIST[PATH]', item: {maxLength: 10}},
          {name: LB, type: 'LIST[BOOL]', minLength: 1},
          {name: LL, type: 'LIST[LIST[INT]]', item: {minLength: 1, item: {maxValue: 3}}}]",
    ));
}

#[test]
fn min_value_greater_than_max_value_is_relabeled_for_outputs() {
    assert_errs(
        &with_outputs("[{name: 'N', type: INT, minValue: 10, maxValue: 1}]"),
        "JobTemplate",
        &[(
            "steps[0] -> outputVariables[0]",
            "Output variable 'N': minValue (10) > maxValue (1).",
        )],
    );
}

#[test]
fn string_constraint_rules_follow_job_parameters() {
    assert_errs(
        &with_outputs("[{name: S, type: STRING, minLength: 3, maxLength: 2, allowedValues: [a]}]"),
        "JobTemplate",
        &[
            (
                "steps[0] -> outputVariables[0]",
                "Output variable 'S': allowedValues[0] length 1 is less than minLength 3.",
            ),
            (
                "steps[0] -> outputVariables[0]",
                "Output variable 'S': minLength (3) > maxLength (2).",
            ),
        ],
    );
}

#[test]
fn a_constraint_for_another_type_is_an_unknown_field() {
    assert_decode_err(
        &with_outputs("[{name: S, type: STRING, minValue: 1}]"),
        "unknown field `minValue`, expected one of `name`, `description`, `allowedValues`, `minLength`, `maxLength`, `collect`, `unit`",
    );
    assert_decode_err(
        &with_outputs("[{name: B, type: BOOL, allowedValues: [true]}]"),
        "unknown field `allowedValues`, expected one of `name`, `description`, `collect`, `unit`",
    );
    // PATH lists objectType and dataFlow's absence.
    assert_decode_err(
        &with_outputs("[{name: P, type: PATH, minValue: 1}]"),
        "unknown field `minValue`, expected one of `name`, `description`, `allowedValues`, `minLength`, `maxLength`, `objectType`, `collect`, `unit`",
    );
}

#[test]
fn an_unknown_field_in_an_item_mapping_keeps_its_own_list() {
    assert_decode_err(
        &with_outputs("[{name: L, type: 'LIST[INT]', item: {minLength: 1}}]"),
        "unknown field `minLength`, expected one of `allowedValues`, `minValue`, `maxValue`",
    );
}

// ══════════════════════════════════════════════════════════════
// outputVariables: collect
// ══════════════════════════════════════════════════════════════

#[test]
fn collect_modes_are_parsed() {
    let jt = decode_ok(&with_outputs(
        "[{name: A, type: INT}, {name: B, type: INT, collect: ONCE}, {name: C, type: INT, collect: LAST}, {name: D, type: 'LIST[INT]', collect: APPEND}]",
    ));
    let modes: Vec<_> = jt.steps[0]
        .output_variables
        .as_ref()
        .unwrap()
        .iter()
        .map(|o| o.collect())
        .collect();
    assert_eq!(
        modes,
        [
            OutputCollectMode::Once,
            OutputCollectMode::Once,
            OutputCollectMode::Last,
            OutputCollectMode::Append
        ]
    );
}

#[test]
fn collect_append_requires_a_list_type() {
    assert_errs(
        &with_outputs("[{name: 'N', type: INT, collect: APPEND}, {name: R, type: RANGE_EXPR, collect: APPEND}]"),
        "JobTemplate",
        &[
            (
                "steps[0] -> outputVariables[0] -> collect",
                "APPEND requires a LIST[...] type; 'INT' is not a list type.",
            ),
            (
                "steps[0] -> outputVariables[1] -> collect",
                "APPEND requires a LIST[...] type; 'RANGE_EXPR' is not a list type.",
            ),
        ],
    );
}

#[test]
fn collect_append_on_every_list_type_is_accepted() {
    for ty in [
        "LIST[INT]",
        "LIST[FLOAT]",
        "LIST[STRING]",
        "LIST[PATH]",
        "LIST[BOOL]",
        "LIST[LIST[INT]]",
    ] {
        decode_ok(&with_outputs(&format!(
            "[{{name: V, type: '{ty}', collect: APPEND}}]"
        )));
    }
}

#[test]
fn unknown_collect_mode() {
    assert_decode_err(
        &with_outputs("[{name: 'N', type: INT, collect: FIRST}]"),
        "collect: unknown variant `FIRST`, expected one of `ONCE`, `LAST`, `APPEND`",
    );
}

#[test]
fn collect_mode_is_case_sensitive() {
    assert_decode_err(
        &with_outputs("[{name: 'N', type: INT, collect: last}]"),
        "collect: unknown variant `last`, expected one of `ONCE`, `LAST`, `APPEND`",
    );
}

// ══════════════════════════════════════════════════════════════
// outputVariables: unit
// ══════════════════════════════════════════════════════════════

#[test]
fn unit_of_32_characters_is_accepted() {
    // Counted in characters, not bytes: 32 two-byte characters.
    let unit = "µ".repeat(32);
    let jt = decode_ok(&with_outputs(&format!(
        "[{{name: 'N', type: INT, unit: '{unit}'}}]"
    )));
    assert_eq!(
        jt.steps[0].output_variables.as_ref().unwrap()[0].unit(),
        Some(unit.as_str())
    );
}

#[test]
fn unit_of_33_characters_is_rejected() {
    assert_errs(
        &with_outputs(&format!(
            "[{{name: 'N', type: INT, unit: '{}'}}]",
            "u".repeat(33)
        )),
        "JobTemplate",
        &[(
            "steps[0] -> outputVariables[0] -> unit",
            "exceeds 32 characters.",
        )],
    );
}

#[test]
fn empty_unit_is_rejected() {
    assert_errs(
        &with_outputs("[{name: 'N', type: INT, unit: ''}]"),
        "JobTemplate",
        &[(
            "steps[0] -> outputVariables[0] -> unit",
            "must not be empty.",
        )],
    );
}

#[test]
fn unit_with_a_control_character_is_rejected() {
    for unit in ["\"m\\tm\"", "\"mm\\u007f\"", "\"\\u0085s\""] {
        assert_errs(
            &with_outputs(&format!("[{{name: 'N', type: INT, unit: {unit}}}]")),
            "JobTemplate",
            &[(
                "steps[0] -> outputVariables[0] -> unit",
                "contains control characters.",
            )],
        );
    }
}

#[test]
fn unit_must_be_a_string() {
    assert_decode_err(
        &with_outputs("[{name: 'N', type: INT, unit: 5}]"),
        "unit: expected a string, got a number",
    );
}

// ══════════════════════════════════════════════════════════════
// outputVariables: shape rules with a parameterSpace
// ══════════════════════════════════════════════════════════════

#[test]
fn list_list_int_on_a_step_with_a_parameter_space_is_rejected() {
    assert_errs(
        &job(
            "[EXPR, CONTROL_FLOW]",
            &[
                "parameterSpace: {taskParameterDefinitions: [{name: F, type: INT, range: [1, 2]}]}",
                "outputVariables: [{name: L, type: 'LIST[LIST[INT]]'}]",
            ],
        ),
        "JobTemplate",
        &[(
            "steps[0] -> outputVariables[0] -> type",
            "LIST[LIST[INT]] is not allowed in a Step with a parameterSpace: each output of a multi-Task Step is collected into a list, and EXPR allows two levels of list nesting.",
        )],
    );
}

#[test]
fn other_list_outputs_on_a_step_with_a_parameter_space_are_accepted() {
    decode_ok(&job(
        "[EXPR, CONTROL_FLOW]",
        &[
            "parameterSpace: {taskParameterDefinitions: [{name: F, type: INT, range: [1, 2]}]}",
            "outputVariables: [{name: L, type: 'LIST[INT]'}]",
        ],
    ));
}

fn chunked(range: &str, with_outputs: bool) -> String {
    let mut fields = vec![format!(
        "parameterSpace: {{taskParameterDefinitions: [{{name: F, type: 'CHUNK[INT]', range: {range}, chunks: {{defaultTaskCount: 2, rangeConstraint: NONCONTIGUOUS}}}}]}}"
    )];
    if with_outputs {
        fields.push("outputVariables: [{name: T, type: FLOAT, unit: s}]".to_string());
    }
    let refs: Vec<&str> = fields.iter().map(String::as_str).collect();
    job("[EXPR, CONTROL_FLOW, TASK_CHUNKING]", &refs)
}

#[test]
fn outputs_on_a_chunked_step_are_accepted() {
    decode_ok(&chunked("'1-10'", true));
    decode_ok(&chunked("[5, 1, 3, 2]", true));
}

#[test]
fn chunk_literal_list_with_duplicates_and_outputs_is_rejected() {
    assert_errs(
        &chunked("[1, 2, 2, 3]", true),
        "JobTemplate",
        &[(
            "steps[0] -> parameterSpace -> taskParameterDefinitions[0] -> range",
            "must have distinct values in a Step with outputVariables, so that each value names one Task; value 2 appears more than once.",
        )],
    );
}

#[test]
fn chunk_literal_list_names_every_repeated_value() {
    assert_errs(
        &chunked("[7, 3, 7, 3, 3, 1]", true),
        "JobTemplate",
        &[(
            "steps[0] -> parameterSpace -> taskParameterDefinitions[0] -> range",
            "must have distinct values in a Step with outputVariables, so that each value names one Task; values 3, 7 appear more than once.",
        )],
    );
}

#[test]
fn chunk_literal_list_with_duplicates_without_outputs_is_accepted() {
    decode_ok(&chunked("[1, 2, 2, 3]", false));
}

#[test]
fn chunk_computed_range_with_outputs_is_not_checked_at_validation() {
    // A computed range is checked when it is materialized, not here.
    decode_ok(&chunked("'{{[1, 1, 2]}}'", true));
}

#[test]
fn int_literal_list_duplicates_with_outputs_are_accepted() {
    // The distinct-values rule is about CHUNK[INT] only.
    decode_ok(&job(
        "[EXPR, CONTROL_FLOW]",
        &[
            "parameterSpace: {taskParameterDefinitions: [{name: F, type: INT, range: [1, 1]}]}",
            "outputVariables: [{name: T, type: INT}]",
        ],
    ));
}

// ══════════════════════════════════════════════════════════════
// Boundaries, indices, and interactions
// ══════════════════════════════════════════════════════════════

#[test]
fn output_variable_name_of_64_characters_is_accepted() {
    decode_ok(&with_outputs(&format!(
        "[{{name: {}, type: INT}}]",
        "N".repeat(64)
    )));
}

#[test]
fn output_names_are_case_sensitive() {
    decode_ok(&with_outputs(
        "[{name: a, type: INT}, {name: A, type: INT}]",
    ));
}

#[test]
fn explicit_null_properties_are_omitted() {
    let jt = decode_ok(&with_outputs(
        "[{name: P, type: PATH, collect: null, unit: null, default: null, userInterface: null, dataFlow: null}]",
    ));
    let ov = &jt.steps[0].output_variables.as_ref().unwrap()[0];
    assert_eq!(ov.collect(), OutputCollectMode::Once);
    assert_eq!(ov.unit(), None);
}

#[test]
fn gating_errors_name_the_offending_element() {
    let t = "specificationVersion: jobtemplate-2023-09
extensions: [EXPR]
name: J
jobEnvironments:
  - {name: JE0, variables: {A: a}}
  - {name: JE1, enableIf: true, variables: {A: a}}
steps:
  - name: S0
    script: {actions: {onRun: {command: echo}}}
  - name: S1
    stepEnvironments:
      - {name: SE0, variables: {B: b}}
      - {name: SE1, enableIf: true, variables: {B: b}}
    outputVariables: [{name: V, type: INT}]
    script: {actions: {onRun: {command: echo}}}
";
    assert_errs(
        t,
        "JobTemplate",
        &[
            (
                "jobEnvironments[1] -> enableIf",
                "enableIf requires the CONTROL_FLOW extension.",
            ),
            (
                "steps[1] -> stepEnvironments[1] -> enableIf",
                "enableIf requires the CONTROL_FLOW extension.",
            ),
            (
                "steps[1] -> outputVariables",
                "outputVariables requires the CONTROL_FLOW extension.",
            ),
        ],
    );
}

#[test]
fn chunk_duplicate_values_error_names_the_parameter() {
    assert_errs(
        &job(
            "[EXPR, CONTROL_FLOW, TASK_CHUNKING]",
            &[
                "parameterSpace: {taskParameterDefinitions: [{name: S, type: STRING, range: [a]}, {name: F, type: 'CHUNK[INT]', range: [4, 4], chunks: {defaultTaskCount: 1, rangeConstraint: NONCONTIGUOUS}}]}",
                "outputVariables: [{name: T, type: INT}]",
            ],
        ),
        "JobTemplate",
        &[(
            "steps[0] -> parameterSpace -> taskParameterDefinitions[1] -> range",
            "must have distinct values in a Step with outputVariables, so that each value names one Task; value 4 appears more than once.",
        )],
    );
}

#[test]
fn chunk_duplicate_values_are_not_reported_without_task_chunking() {
    // Pass 9 rejects the CHUNK[INT] parameter itself; no second error.
    let t = job(
        "[EXPR, CONTROL_FLOW]",
        &[
            "parameterSpace: {taskParameterDefinitions: [{name: F, type: 'CHUNK[INT]', range: [1, 1], chunks: {defaultTaskCount: 1, rangeConstraint: NONCONTIGUOUS}}]}",
            "outputVariables: [{name: T, type: INT}]",
        ],
    );
    let err = decode(&t).expect_err("CHUNK[INT] needs TASK_CHUNKING");
    assert_eq!(err.to_string(), CHUNK_WITHOUT_TASK_CHUNKING);
}

const CHUNK_WITHOUT_TASK_CHUNKING: &str =
    "Model validation error: 2 validation errors for JobTemplate
steps[0] -> parameterSpace -> taskParameterDefinitions[0]:
\ttask parameter type 'CHUNK[INT]' is not allowed.
steps[0] -> parameterSpace -> taskParameterDefinitions[0]:
\tCHUNK[INT] requires the TASK_CHUNKING extension.";

#[test]
fn decode_errors_name_the_json_kind() {
    assert_decode_err(
        &job("[EXPR, CONTROL_FLOW]", &["enableIf: [true]"]),
        "expected a bool or a format string, got a list",
    );
    assert_decode_err(
        &job("[EXPR, CONTROL_FLOW]", &["enableIf: {a: 1}"]),
        "expected a bool or a format string, got a mapping",
    );
    assert_decode_err(
        &with_outputs("[null]"),
        "expected an output variable definition mapping, got null",
    );
    assert_decode_err(
        &with_outputs("[{name: V, type: INT, unit: true}]"),
        "unit: expected a string, got a bool",
    );
}
