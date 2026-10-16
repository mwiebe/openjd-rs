// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `CONTROL_FLOW` (RFC 0010) job-side types: `job::OutputVariable` and its
//! constraint checks, `StepBinding`, `PathInput`, the new `job::Step` fields
//! and their serde defaults, and `create_job` filling them.

use openjd_expr::path_mapping::PathFormat;
use openjd_expr::value::Float64;
use openjd_expr::{ExprType, ExprValue, RangeExpr};
use openjd_model::job::{
    OutputAllowedValues, OutputBound, OutputConstraints, OutputVariable, PathInput, Step,
    StepBinding,
};
use openjd_model::template::OutputVariableDefinition;
use openjd_model::{
    create_job, decode_job_template, CallerLimits, JobParameterType, JobParameterValues,
    OutputCollectMode,
};

fn yaml(s: &str) -> serde_json::Value {
    serde_saphyr::from_str(s).unwrap()
}

/// Parse one `<OutputVariableDefinition>` from flow YAML.
fn def(s: &str) -> OutputVariableDefinition {
    serde_json::from_value(yaml(s)).expect("valid output variable definition")
}

fn list_string(items: &[&str]) -> ExprValue {
    ExprValue::make_list(
        items
            .iter()
            .map(|s| ExprValue::String(s.to_string()))
            .collect(),
        ExprType::STRING,
    )
    .unwrap()
}

fn list_of_int_lists(items: &[&[i64]]) -> ExprValue {
    ExprValue::make_list(
        items
            .iter()
            .map(|v| ExprValue::ListInt(v.to_vec()))
            .collect(),
        ExprType::list(ExprType::INT),
    )
    .unwrap()
}

fn float(v: f64) -> ExprValue {
    ExprValue::Float(Float64::new(v).unwrap())
}

// ══════════════════════════════════════════════════════════════
// Building the job-side declaration
// ══════════════════════════════════════════════════════════════

#[test]
fn to_job_output_variable_holds_only_declared_constraints() {
    let ov = def(
        "{name: 'N', type: INT, collect: LAST, unit: frames, minValue: 1, allowedValues: [1, 2]}",
    )
    .to_job_output_variable();
    assert_eq!(
        ov,
        OutputVariable {
            name: "N".into(),
            param_type: JobParameterType::Int,
            collect: OutputCollectMode::Last,
            unit: Some("frames".into()),
            constraints: OutputConstraints {
                allowed_values: Some(OutputAllowedValues::Int(vec![1, 2])),
                min_value: Some(OutputBound::Int(1)),
                ..Default::default()
            },
        }
    );
}

#[test]
fn nested_item_constraints_are_kept() {
    let ov = def("{name: L, type: 'LIST[LIST[INT]]', maxLength: 4, item: {minLength: 1, item: {maxValue: 9}}}")
        .to_job_output_variable();
    assert_eq!(
        ov.constraints,
        OutputConstraints {
            max_length: Some(4),
            item: Some(Box::new(OutputConstraints {
                min_length: Some(1),
                item: Some(Box::new(OutputConstraints {
                    max_value: Some(OutputBound::Int(9)),
                    ..Default::default()
                })),
                ..Default::default()
            })),
            ..Default::default()
        }
    );
}

#[test]
fn an_empty_item_mapping_declares_nothing() {
    let ov = def("{name: L, type: 'LIST[INT]', item: {}}").to_job_output_variable();
    assert!(ov.constraints.is_empty());
}

#[test]
fn float_constraints_are_kept() {
    let ov = def("{name: F, type: FLOAT, minValue: 0.5, maxValue: 2, allowedValues: [0.5, 1.5]}")
        .to_job_output_variable();
    assert_eq!(
        ov.constraints,
        OutputConstraints {
            allowed_values: Some(OutputAllowedValues::Float(vec![
                Float64::new(0.5).unwrap(),
                Float64::new(1.5).unwrap()
            ])),
            min_value: Some(OutputBound::Float(Float64::new(0.5).unwrap())),
            max_value: Some(OutputBound::Float(Float64::new(2.0).unwrap())),
            ..Default::default()
        }
    );
}

// ══════════════════════════════════════════════════════════════
// check_constraints: only declared properties apply
// ══════════════════════════════════════════════════════════════

#[test]
fn range_expr_longer_than_1024_characters_is_accepted_without_max_length() {
    // A RANGE_EXPR job parameter has an implied 1024-character maximum; an
    // output has only the constraints it declares (RFC 0010 item 6).
    let long: String = (0..400)
        .map(|i| (i * 3).to_string())
        .collect::<Vec<_>>()
        .join(",");
    assert!(long.len() > 1024);
    let value = ExprValue::RangeExpr(long.parse::<RangeExpr>().unwrap());
    def("{name: R, type: RANGE_EXPR}")
        .check_constraints(&value)
        .expect("no maxLength declared");
    assert_eq!(
        def("{name: R, type: RANGE_EXPR, maxLength: 1024}").check_constraints(&value),
        Err(format!(
            "Output variable 'R': value length {} exceeds maximum 1024",
            value_len(&value)
        ))
    );
}

fn value_len(v: &ExprValue) -> usize {
    match v {
        ExprValue::RangeExpr(r) => r.to_string().chars().count(),
        _ => unreachable!(),
    }
}

#[test]
fn string_without_constraints_accepts_any_length() {
    def("{name: S, type: STRING}")
        .check_constraints(&ExprValue::String("x".repeat(5000)))
        .unwrap();
}

#[test]
fn int_constraints() {
    let d = def("{name: 'N', type: INT, minValue: 2, maxValue: 8, allowedValues: [2, 4, 8, 9]}");
    d.check_constraints(&ExprValue::Int(4)).unwrap();
    assert_eq!(
        d.check_constraints(&ExprValue::Int(1)),
        Err("Output variable 'N': value 1 is less than minimum 2".into())
    );
    assert_eq!(
        d.check_constraints(&ExprValue::Int(9)),
        Err("Output variable 'N': value 9 exceeds maximum 8".into())
    );
    assert_eq!(
        d.check_constraints(&ExprValue::Int(3)),
        Err("Output variable 'N': value 3 is not in allowed values".into())
    );
}

#[test]
fn float_constraint_messages() {
    let d = def("{name: F, type: FLOAT, minValue: 0.5, maxValue: 2.5, allowedValues: [1.5, 3]}");
    d.check_constraints(&float(1.5)).unwrap();
    assert_eq!(
        d.check_constraints(&float(0.25)),
        Err("Output variable 'F': value 0.25 is less than minimum 0.5".into())
    );
    assert_eq!(
        d.check_constraints(&float(3.0)),
        Err("Output variable 'F': value 3.0 exceeds maximum 2.5".into())
    );
    assert_eq!(
        d.check_constraints(&float(2.0)),
        Err("Output variable 'F': value 2.0 is not in allowed values".into())
    );
}

#[test]
fn string_constraints_count_characters() {
    let d = def(
        "{name: S, type: STRING, minLength: 2, maxLength: 3, allowedValues: [ab, 'µµµ', abcd]}",
    );
    d.check_constraints(&ExprValue::String("µµµ".into()))
        .unwrap();
    assert_eq!(
        d.check_constraints(&ExprValue::String("a".into())),
        Err("Output variable 'S': value length 1 is less than minimum 2".into())
    );
    assert_eq!(
        d.check_constraints(&ExprValue::String("abcd".into())),
        Err("Output variable 'S': value length 4 exceeds maximum 3".into())
    );
    assert_eq!(
        d.check_constraints(&ExprValue::String("xy".into())),
        Err("Output variable 'S': value 'xy' is not in allowed values".into())
    );
}

#[test]
fn path_constraints_apply_to_path_values() {
    let d = def("{name: P, type: PATH, maxLength: 6, allowedValues: [/a/b]}");
    d.check_constraints(&ExprValue::new_path("/a/b", PathFormat::Posix))
        .unwrap();
    assert_eq!(
        d.check_constraints(&ExprValue::new_path("/a/b/c/d", PathFormat::Posix)),
        Err("Output variable 'P': value length 8 exceeds maximum 6".into())
    );
    assert_eq!(
        d.check_constraints(&ExprValue::new_path("/x", PathFormat::Posix)),
        Err("Output variable 'P': value '/x' is not in allowed values".into())
    );
}

#[test]
fn bool_has_no_constraints() {
    def("{name: B, type: BOOL}")
        .check_constraints(&ExprValue::Bool(false))
        .unwrap();
}

#[test]
fn a_value_of_the_wrong_type_is_rejected() {
    assert_eq!(
        def("{name: 'N', type: INT}").check_constraints(&ExprValue::String("1".into())),
        Err("Output variable 'N': value has type string, expected int".into())
    );
    assert_eq!(
        def("{name: B, type: BOOL}").check_constraints(&ExprValue::Int(1)),
        Err("Output variable 'B': value has type int, expected bool".into())
    );
    assert_eq!(
        def("{name: L, type: 'LIST[INT]'}").check_constraints(&list_string(&["a"])),
        Err("Output variable 'L': value has type list[string], expected list[int]".into())
    );
}

#[test]
fn list_length_and_item_constraints() {
    let d = def("{name: L, type: 'LIST[INT]', minLength: 1, maxLength: 3, item: {minValue: 0, allowedValues: [0, 1, 2, 5]}}");
    d.check_constraints(&ExprValue::ListInt(vec![0, 5]))
        .unwrap();
    assert_eq!(
        d.check_constraints(&ExprValue::ListInt(vec![])),
        Err("Output variable 'L': list length 0 is less than minimum 1".into())
    );
    assert_eq!(
        d.check_constraints(&ExprValue::ListInt(vec![0, 1, 2, 5])),
        Err("Output variable 'L': list length 4 exceeds maximum 3".into())
    );
    assert_eq!(
        d.check_constraints(&ExprValue::ListInt(vec![1, -1])),
        Err("Output variable 'L': item[1] -1 is less than minimum 0".into())
    );
    assert_eq!(
        d.check_constraints(&ExprValue::ListInt(vec![3])),
        Err("Output variable 'L': item[0] 3 is not in allowed values".into())
    );
}

#[test]
fn an_untyped_empty_list_satisfies_a_list_type() {
    let empty = ExprValue::make_list(vec![], ExprType::NULLTYPE).unwrap();
    def("{name: L, type: 'LIST[STRING]'}")
        .check_constraints(&empty)
        .unwrap();
}

#[test]
fn list_string_item_constraints() {
    let d =
        def("{name: L, type: 'LIST[STRING]', item: {maxLength: 2, allowedValues: [a, bb, ccc]}}");
    d.check_constraints(&list_string(&["a", "bb"])).unwrap();
    assert_eq!(
        d.check_constraints(&list_string(&["a", "ccc"])),
        Err("Output variable 'L': item[1] length 3 exceeds maximum 2".into())
    );
    assert_eq!(
        d.check_constraints(&list_string(&["d"])),
        Err("Output variable 'L': item[0] 'd' is not in allowed values".into())
    );
}

#[test]
fn list_path_accepts_path_and_string_lists() {
    let d = def("{name: L, type: 'LIST[PATH]', item: {maxLength: 4}}");
    let paths = ExprValue::make_list(
        vec![ExprValue::new_path("/a", PathFormat::Posix)],
        ExprType::PATH,
    )
    .unwrap();
    d.check_constraints(&paths).unwrap();
    assert_eq!(
        d.check_constraints(&list_string(&["/a/b/c"])),
        Err("Output variable 'L': item[0] length 6 exceeds maximum 4".into())
    );
}

#[test]
fn list_float_and_list_bool() {
    let d = def("{name: L, type: 'LIST[FLOAT]', item: {maxValue: 1.0}}");
    let v = ExprValue::make_list(vec![float(0.5), float(1.5)], ExprType::FLOAT).unwrap();
    assert_eq!(
        d.check_constraints(&v),
        Err("Output variable 'L': item[1] 1.5 exceeds maximum 1.0".into())
    );
    let b = def("{name: B, type: 'LIST[BOOL]', maxLength: 1}");
    assert_eq!(
        b.check_constraints(&ExprValue::ListBool(vec![true, false])),
        Err("Output variable 'B': list length 2 exceeds maximum 1".into())
    );
}

#[test]
fn list_list_int_constraints() {
    let d = def("{name: L, type: 'LIST[LIST[INT]]', item: {minLength: 1, maxLength: 2, item: {maxValue: 5}}}");
    d.check_constraints(&list_of_int_lists(&[&[1], &[2, 3]]))
        .unwrap();
    assert_eq!(
        d.check_constraints(&list_of_int_lists(&[&[1], &[]])),
        Err("Output variable 'L': item[1] length 0 is less than minimum 1".into())
    );
    assert_eq!(
        d.check_constraints(&list_of_int_lists(&[&[1, 2, 3]])),
        Err("Output variable 'L': item[0] length 3 exceeds maximum 2".into())
    );
    assert_eq!(
        d.check_constraints(&list_of_int_lists(&[&[1], &[2, 9]])),
        Err("Output variable 'L': item[1][1] 9 exceeds maximum 5".into())
    );
}

// ══════════════════════════════════════════════════════════════
// The APPEND split: check_element and check_length
// ══════════════════════════════════════════════════════════════

#[test]
fn check_length_applies_min_length_only_at_exit() {
    let d = def("{name: L, type: 'LIST[INT]', collect: APPEND, minLength: 2, maxLength: 3}");
    // While the list grows, a short list is fine.
    d.check_length(0, false).unwrap();
    d.check_length(1, false).unwrap();
    assert_eq!(
        d.check_length(1, true),
        Err("Output variable 'L': list length 1 is less than minimum 2".into())
    );
    d.check_length(2, true).unwrap();
    // maxLength applies at every length.
    assert_eq!(
        d.check_length(4, false),
        Err("Output variable 'L': list length 4 exceeds maximum 3".into())
    );
    assert_eq!(
        d.check_length(4, true),
        Err("Output variable 'L': list length 4 exceeds maximum 3".into())
    );
}

#[test]
fn check_element_applies_item_constraints() {
    let d = def("{name: L, type: 'LIST[INT]', collect: APPEND, item: {maxValue: 5}}");
    d.check_element(&ExprValue::Int(5)).unwrap();
    assert_eq!(
        d.check_element(&ExprValue::Int(6)),
        Err("Output variable 'L': element 6 exceeds maximum 5".into())
    );
    assert_eq!(
        d.check_element(&ExprValue::String("x".into())),
        Err("Output variable 'L': element has type string, expected int".into())
    );
}

#[test]
fn check_element_on_a_list_list_int_output() {
    let d = def("{name: L, type: 'LIST[LIST[INT]]', collect: APPEND, item: {maxLength: 1, item: {minValue: 0}}}");
    d.check_element(&ExprValue::ListInt(vec![3])).unwrap();
    assert_eq!(
        d.check_element(&ExprValue::ListInt(vec![1, 2])),
        Err("Output variable 'L': element length 2 exceeds maximum 1".into())
    );
    assert_eq!(
        d.check_element(&ExprValue::ListInt(vec![-1])),
        Err("Output variable 'L': element item[0] -1 is less than minimum 0".into())
    );
}

#[test]
fn check_element_on_a_scalar_output_is_an_error() {
    assert_eq!(
        def("{name: 'N', type: INT}").check_element(&ExprValue::Int(1)),
        Err("Output variable 'N': a INT output has no elements".into())
    );
}

// ══════════════════════════════════════════════════════════════
// Serde: the new job::Step fields and their defaults
// ══════════════════════════════════════════════════════════════

const STEP_JSON: &str = r#"{"name":"S","script":{"actions":{"onRun":{"command":"echo"}}}}"#;

#[test]
fn a_step_serialized_before_control_flow_deserializes_with_defaults() {
    let step: Step = serde_json::from_str(STEP_JSON).unwrap();
    assert_eq!(step.output_variables, None);
    assert_eq!(step.binding, StepBinding::Bound);
    assert!(step.path_inputs.is_empty());
}

#[test]
fn default_fields_are_not_serialized() {
    let step: Step = serde_json::from_str(STEP_JSON).unwrap();
    let value = serde_json::to_value(&step).unwrap();
    let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
    for absent in ["outputVariables", "binding", "pathInputs"] {
        assert!(
            !keys.iter().any(|k| *k == absent),
            "`{absent}` should be skipped, got {value}"
        );
    }
}

#[test]
fn control_flow_fields_round_trip() {
    let mut step: Step = serde_json::from_str(STEP_JSON).unwrap();
    step.output_variables = Some(vec![
        def("{name: T, type: FLOAT, unit: s, minValue: 0}").to_job_output_variable(),
        def("{name: K, type: 'LIST[STRING]', collect: APPEND, item: {maxLength: 9}}")
            .to_job_output_variable(),
    ]);
    step.binding = StepBinding::PendingInputs {
        waiting_on: vec!["Steps.P.Output.N".into()],
    };
    step.path_inputs = vec![PathInput {
        symbol: "Steps.P.RawOutput.Dirs".into(),
        ty: ExprType::list(ExprType::PATH),
    }];
    let value = serde_json::to_value(&step).unwrap();
    assert_eq!(
        value["outputVariables"],
        serde_json::json!([
            {"name": "T", "type": "Float", "unit": "s", "constraints": {"minValue": {"float": 0.0}}},
            {"name": "K", "type": "ListString", "collect": "APPEND", "constraints": {"item": {"maxLength": 9}}}
        ])
    );
    assert_eq!(
        value["binding"],
        serde_json::json!({"state": "PENDING_INPUTS", "waitingOn": ["Steps.P.Output.N"]})
    );
    assert_eq!(
        value["pathInputs"],
        serde_json::json!([{"symbol": "Steps.P.RawOutput.Dirs", "ty": "list[path]"}])
    );
    let back: Step = serde_json::from_value(value).unwrap();
    assert_eq!(back, step);
}

#[test]
fn skipped_binding_round_trips() {
    let v = serde_json::to_value(StepBinding::Skipped).unwrap();
    assert_eq!(v, serde_json::json!({"state": "SKIPPED"}));
    assert_eq!(
        serde_json::from_value::<StepBinding>(v).unwrap(),
        StepBinding::Skipped
    );
    assert!(!StepBinding::Skipped.is_bound());
    assert!(StepBinding::Bound.is_bound());
}

#[test]
fn path_input_with_an_invalid_type_is_rejected() {
    let err =
        serde_json::from_value::<PathInput>(serde_json::json!({"symbol": "S", "ty": "list["}))
            .unwrap_err();
    assert!(err.to_string().contains("list["), "got: {err}");
}

#[test]
fn job_parameter_type_round_trips() {
    for ty in [
        JobParameterType::String,
        JobParameterType::Int,
        JobParameterType::Float,
        JobParameterType::Path,
        JobParameterType::Bool,
        JobParameterType::RangeExpr,
        JobParameterType::ListString,
        JobParameterType::ListInt,
        JobParameterType::ListFloat,
        JobParameterType::ListPath,
        JobParameterType::ListBool,
        JobParameterType::ListListInt,
    ] {
        let v = serde_json::to_value(ty).unwrap();
        assert_eq!(serde_json::from_value::<JobParameterType>(v).unwrap(), ty);
    }
}

#[test]
fn output_collect_mode_spec_strings() {
    assert_eq!(OutputCollectMode::default(), OutputCollectMode::Once);
    assert_eq!(OutputCollectMode::Once.to_string(), "ONCE");
    assert_eq!(OutputCollectMode::Last.as_spec_str(), "LAST");
    assert_eq!(
        serde_json::to_value(OutputCollectMode::Append).unwrap(),
        serde_json::json!("APPEND")
    );
}

// ══════════════════════════════════════════════════════════════
// create_job
// ══════════════════════════════════════════════════════════════

#[test]
fn create_job_fills_the_control_flow_fields() {
    let jt = decode_job_template(
        yaml(
            "specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: J
steps:
  - name: P
    outputVariables:
      - {name: 'N', type: INT, minValue: 0, unit: frames}
      - {name: Keys, type: 'LIST[STRING]', collect: APPEND}
    script: {actions: {onRun: {command: echo}}}
  - name: Q
    dependencies: [{dependsOn: P}]
    script: {actions: {onRun: {command: echo}}}
",
        ),
        Some(&["EXPR", "CONTROL_FLOW"]),
        &CallerLimits::default(),
    )
    .unwrap();
    let job = create_job(
        &jt,
        &JobParameterValues::new(),
        &jt.default_validation_context(),
    )
    .unwrap();
    let p = &job.steps[0];
    assert_eq!(
        p.output_variables,
        Some(vec![
            OutputVariable {
                name: "N".into(),
                param_type: JobParameterType::Int,
                collect: OutputCollectMode::Once,
                unit: Some("frames".into()),
                constraints: OutputConstraints {
                    min_value: Some(OutputBound::Int(0)),
                    ..Default::default()
                },
            },
            OutputVariable {
                name: "Keys".into(),
                param_type: JobParameterType::ListString,
                collect: OutputCollectMode::Append,
                unit: None,
                constraints: OutputConstraints::default(),
            },
        ])
    );
    assert_eq!(p.binding, StepBinding::Bound);
    assert!(p.path_inputs.is_empty());
    let q = &job.steps[1];
    assert_eq!(q.output_variables, None);
    assert_eq!(q.binding, StepBinding::Bound);
}
