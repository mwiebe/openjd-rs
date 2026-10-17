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
    GroupState, OutputAllowedValues, OutputBound, OutputConstraints, OutputVariable, PathInput,
    PendingBinding, Step, StepBinding,
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
fn a_bool_output_has_no_constraints() {
    let d = def("{name: B, type: BOOL}");
    assert!(d.constraints().is_empty());
    assert_eq!(
        serde_json::to_value(d.to_job_output_variable()).unwrap(),
        serde_json::json!({"name": "B", "type": "BOOL"})
    );
}

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
    let d = def("{name: L, type: 'LIST[LIST[INT]]', maxLength: 4, item: {minLength: 1, item: {maxValue: 9}}}");
    // The accessor and the job form agree.
    assert_eq!(*d.constraints(), d.to_job_output_variable().constraints);
    let ov = d.to_job_output_variable();
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

#[test]
fn quoted_strings_in_check_messages_are_cut_and_escaped() {
    let r = def("{name: R, type: RANGE_EXPR}");
    assert_eq!(
        r.check_constraints(&ExprValue::String("a\u{1b}b\0".into())),
        Err("Output variable 'R': value 'a\\u{1b}b\\0' is not a valid range expression".into())
    );
    let s = def("{name: S, type: STRING, allowedValues: [a]}");
    let long = format!("{}\n", "é".repeat(250));
    assert_eq!(
        s.check_constraints(&ExprValue::String(long)),
        Err(format!(
            "Output variable 'S': value '{}...' is not in allowed values",
            "é".repeat(200)
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
    assert_eq!(
        def("{name: P, type: PATH, minLength: 3}")
            .check_constraints(&ExprValue::new_path("/a", PathFormat::Posix)),
        Err("Output variable 'P': value length 2 is less than minimum 3".into())
    );
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
    assert_eq!(
        def("{name: L, type: 'LIST[STRING]', item: {minLength: 2}}")
            .check_constraints(&list_string(&["ab", "a"])),
        Err("Output variable 'L': item[1] length 1 is less than minimum 2".into())
    );
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
    assert_eq!(
        def("{name: L, type: 'LIST[PATH]', item: {minLength: 2}}")
            .check_constraints(&list_string(&["a"])),
        Err("Output variable 'L': item[0] length 1 is less than minimum 2".into())
    );
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
    // The outer index comes first.
    assert_eq!(
        d.check_constraints(&list_of_int_lists(&[&[1], &[7, 2]])),
        Err("Output variable 'L': item[1][0] 7 exceeds maximum 5".into())
    );
    assert_eq!(
        d.check_constraints(&list_of_int_lists(&[&[1, 6]])),
        Err("Output variable 'L': item[0][1] 6 exceeds maximum 5".into())
    );
}

#[test]
fn a_non_empty_list_list_int_is_checked_element_by_element() {
    // Whatever the outer hint, each element is checked against list[int].
    let d = def("{name: L, type: 'LIST[LIST[INT]]'}");
    let two_untyped_empties = ExprValue::make_list(
        vec![
            ExprValue::make_list(vec![], ExprType::NULLTYPE).unwrap(),
            ExprValue::make_list(vec![], ExprType::NULLTYPE).unwrap(),
        ],
        ExprType::NULLTYPE,
    )
    .unwrap();
    d.check_constraints(&two_untyped_empties).unwrap();
    let strings = ExprValue::make_list(vec![list_string(&["a"])], ExprType::NULLTYPE).unwrap();
    assert_eq!(
        d.check_constraints(&strings),
        Err("Output variable 'L': item[0] has type list[string], expected list[int]".into())
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
    assert_eq!(
        def("{name: L, type: 'LIST[LIST[INT]]', collect: APPEND, item: {item: {minValue: 0}}}")
            .check_element(&ExprValue::ListInt(vec![0, -1])),
        Err("Output variable 'L': element item[1] -1 is less than minimum 0".into())
    );
}

#[test]
fn check_element_on_a_scalar_output_is_an_error() {
    assert_eq!(
        def("{name: 'N', type: INT}").check_element(&ExprValue::Int(1)),
        Err("Output variable 'N': INT is not a list type, so it has no elements".into())
    );
    assert_eq!(
        def("{name: S, type: STRING, minLength: 2}").check_length(1, true),
        Err("Output variable 'S': STRING is not a list type, so it has no elements".into())
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
        def("{name: T, type: FLOAT, unit: s, minValue: 0, allowedValues: [0, 1.5]}")
            .to_job_output_variable(),
        def("{name: K, type: 'LIST[STRING]', collect: APPEND, maxLength: 9, item: {allowedValues: [a]}}")
            .to_job_output_variable(),
        def("{name: C, type: 'LIST[INT]', item: {minValue: 1, allowedValues: [1, 2]}}")
            .to_job_output_variable(),
    ]);
    step.binding = StepBinding::PendingInputs(PendingBinding {
        enablement: GroupState::Known(()),
        tasks: GroupState::Waiting {
            on: vec!["Steps.P.Output.N".into()],
        },
        host_requirements: GroupState::Known(None),
        remaining: GroupState::Waiting { on: vec![] },
        producers: vec!["P".into()],
    });
    step.path_inputs = vec![PathInput {
        symbol: "Steps.P.RawOutput.Dirs".into(),
        ty: ExprType::list(ExprType::PATH),
    }];
    let value = serde_json::to_value(&step).unwrap();
    assert_eq!(
        value["outputVariables"],
        serde_json::json!([
            {"name": "T", "type": "FLOAT", "unit": "s",
             "constraints": {"minValue": {"float": 0.0}, "allowedValues": {"float": [0.0, 1.5]}}},
            {"name": "K", "type": "LIST[STRING]", "collect": "APPEND",
             "constraints": {"maxLength": 9, "item": {"allowedValues": {"string": ["a"]}}}},
            {"name": "C", "type": "LIST[INT]",
             "constraints": {"item": {"minValue": {"int": 1}, "allowedValues": {"int": [1, 2]}}}}
        ])
    );
    assert_eq!(
        value["binding"],
        serde_json::json!({
            "state": "PENDING_INPUTS",
            "enablement": {"known": null},
            "tasks": {"waiting": {"on": ["Steps.P.Output.N"]}},
            "hostRequirements": {"known": null},
            "remaining": {"waiting": {"on": []}},
            "producers": ["P"]
        })
    );
    assert_eq!(
        value["pathInputs"],
        serde_json::json!([{"symbol": "Steps.P.RawOutput.Dirs", "type": "list[path]"}])
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
        serde_json::from_value::<PathInput>(serde_json::json!({"symbol": "S", "type": "list["}))
            .unwrap_err();
    assert_eq!(err.to_string(), PATH_INPUT_TYPE_ERROR);
}

/// The `ExprType::parse` message for the malformed type `list[`.
const PATH_INPUT_TYPE_ERROR: &str = "Unknown type string: list[";

#[test]
fn output_variable_type_is_serialized_as_the_spec_string() {
    let ov = def("{name: L, type: 'list[list[int]]'}").to_job_output_variable();
    let v = serde_json::to_value(&ov).unwrap();
    // No `collect` (ONCE) and no `constraints` (none declared).
    assert_eq!(
        v,
        serde_json::json!({"name": "L", "type": "LIST[LIST[INT]]"})
    );
    assert_eq!(serde_json::from_value::<OutputVariable>(v).unwrap(), ov);
    // Reading accepts the spec string in any case, and rejects other names.
    let lower: OutputVariable =
        serde_json::from_value(serde_json::json!({"name": "L", "type": "list[int]"})).unwrap();
    assert_eq!(lower.param_type, JobParameterType::ListInt);
    let err = serde_json::from_value::<OutputVariable>(
        serde_json::json!({"name": "L", "type": "ListInt"}),
    )
    .unwrap_err();
    assert_eq!(err.to_string(), "unknown output variable type: 'ListInt'");
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
    for (mode, spec) in [
        (OutputCollectMode::Once, "ONCE"),
        (OutputCollectMode::Last, "LAST"),
        (OutputCollectMode::Append, "APPEND"),
    ] {
        assert_eq!(mode.as_spec_str(), spec);
        assert_eq!(mode.to_string(), spec);
    }
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

// ══════════════════════════════════════════════════════════════
// Boundaries and remaining list types
// ══════════════════════════════════════════════════════════════

#[test]
fn bounds_are_inclusive() {
    let f = def("{name: F, type: FLOAT, minValue: 0.5, maxValue: 2.5}");
    f.check_constraints(&float(0.5)).unwrap();
    f.check_constraints(&float(2.5)).unwrap();
    let i = def("{name: I, type: INT, minValue: -1, maxValue: 1}");
    i.check_constraints(&ExprValue::Int(-1)).unwrap();
    i.check_constraints(&ExprValue::Int(1)).unwrap();
    let s = def("{name: S, type: STRING, minLength: 2, maxLength: 2}");
    s.check_constraints(&ExprValue::String("ab".into()))
        .unwrap();
    let l = def("{name: L, type: 'LIST[INT]', minLength: 2, maxLength: 2}");
    l.check_constraints(&ExprValue::ListInt(vec![1, 2]))
        .unwrap();
    l.check_length(2, false).unwrap();
    l.check_length(2, true).unwrap();
}

#[test]
fn list_length_constraints_apply_to_every_list_type() {
    let floats = |n: usize| {
        ExprValue::make_list((0..n).map(|_| float(1.5)).collect(), ExprType::FLOAT).unwrap()
    };
    let paths = |n: usize| {
        ExprValue::make_list(
            (0..n)
                .map(|_| ExprValue::new_path("/p", PathFormat::Posix))
                .collect(),
            ExprType::PATH,
        )
        .unwrap()
    };
    let strings = |n: usize| list_string(&vec!["s"; n]);
    let cases: [(&str, &dyn Fn(usize) -> ExprValue); 5] = [
        ("LIST[STRING]", &strings),
        ("LIST[PATH]", &paths),
        ("LIST[FLOAT]", &floats),
        ("LIST[BOOL]", &|n| ExprValue::ListBool(vec![true; n])),
        ("LIST[LIST[INT]]", &|n| {
            list_of_int_lists(&vec![&[1i64][..]; n])
        }),
    ];
    for (ty, make) in cases {
        let d = def(&format!(
            "{{name: L, type: '{ty}', minLength: 2, maxLength: 3}}"
        ));
        d.check_constraints(&make(2)).unwrap();
        assert_eq!(
            d.check_constraints(&make(1)),
            Err("Output variable 'L': list length 1 is less than minimum 2".into()),
            "{ty}"
        );
        assert_eq!(
            d.check_constraints(&make(4)),
            Err("Output variable 'L': list length 4 exceeds maximum 3".into()),
            "{ty}"
        );
    }
}

#[test]
fn list_float_item_allowed_values_and_min_value() {
    let d = def("{name: L, type: 'LIST[FLOAT]', item: {minValue: 1.0, allowedValues: [1.0, 2.5]}}");
    d.check_constraints(
        &ExprValue::make_list(vec![float(1.0), float(2.5)], ExprType::FLOAT).unwrap(),
    )
    .unwrap();
    assert_eq!(
        d.check_constraints(&ExprValue::make_list(vec![float(0.5)], ExprType::FLOAT).unwrap()),
        Err("Output variable 'L': item[0] 0.5 is less than minimum 1.0".into())
    );
    assert_eq!(
        d.check_constraints(&ExprValue::make_list(vec![float(2.0)], ExprType::FLOAT).unwrap()),
        Err("Output variable 'L': item[0] 2.0 is not in allowed values".into())
    );
}

#[test]
fn a_typed_empty_list_must_match_the_declared_type() {
    let empty_list_of_string_lists =
        ExprValue::make_list(vec![], ExprType::list(ExprType::STRING)).unwrap();
    assert_eq!(
        def("{name: L, type: 'LIST[STRING]'}").check_constraints(&empty_list_of_string_lists),
        Err("Output variable 'L': value has type list[list[string]], expected list[string]".into())
    );
    assert_eq!(
        def("{name: L, type: 'LIST[INT]'}").check_constraints(&list_string(&[])),
        Err("Output variable 'L': value has type list[string], expected list[int]".into())
    );
    assert_eq!(
        def("{name: L, type: 'LIST[LIST[INT]]'}").check_constraints(
            &ExprValue::make_list(vec![], ExprType::list(ExprType::STRING)).unwrap()
        ),
        Err(
            "Output variable 'L': value has type list[list[string]], expected list[list[int]]"
                .into()
        )
    );
    // A typed empty list of the declared type is fine.
    def("{name: L, type: 'LIST[LIST[INT]]'}")
        .check_constraints(&ExprValue::make_list(vec![], ExprType::list(ExprType::INT)).unwrap())
        .unwrap();
}

#[test]
fn check_element_for_each_list_element_type() {
    // A mismatched element names the declared element type.
    for (ty, expected) in [
        ("LIST[STRING]", "string"),
        ("LIST[INT]", "int"),
        ("LIST[FLOAT]", "float"),
        ("LIST[BOOL]", "bool"),
    ] {
        let found = if ty == "LIST[STRING]" || ty == "LIST[FLOAT]" {
            ExprValue::Bool(true)
        } else {
            ExprValue::String("x".into())
        };
        let kind = if matches!(found, ExprValue::Bool(_)) {
            "bool"
        } else {
            "string"
        };
        assert_eq!(
            def(&format!("{{name: E, type: '{ty}', collect: APPEND}}")).check_element(&found),
            Err(format!(
                "Output variable 'E': element has type {kind}, expected {expected}"
            )),
            "{ty}"
        );
    }
    def("{name: B, type: 'LIST[BOOL]', collect: APPEND}")
        .check_element(&ExprValue::Bool(true))
        .unwrap();
    let p = def("{name: P, type: 'LIST[PATH]', collect: APPEND, item: {allowedValues: [/a]}}");
    p.check_element(&ExprValue::new_path("/a", PathFormat::Posix))
        .unwrap();
    p.check_element(&ExprValue::String("/a".into())).unwrap();
    assert_eq!(
        p.check_element(&ExprValue::Int(3)),
        Err("Output variable 'P': element has type int, expected path".into())
    );
    let ints = ExprValue::make_list(vec![ExprValue::Int(3)], ExprType::INT).unwrap();
    assert_eq!(
        def("{name: P, type: 'LIST[PATH]'}").check_constraints(&ints),
        Err("Output variable 'P': value has type list[int], expected list[path]".into())
    );
    assert_eq!(
        p.check_element(&ExprValue::new_path("/b", PathFormat::Posix)),
        Err("Output variable 'P': element '/b' is not in allowed values".into())
    );
    let l = def("{name: L, type: 'LIST[LIST[INT]]', collect: APPEND, item: {minLength: 0}}");
    l.check_element(&ExprValue::make_list(vec![], ExprType::NULLTYPE).unwrap())
        .unwrap();
    assert_eq!(
        l.check_element(&ExprValue::String("x".into())),
        Err("Output variable 'L': element has type string, expected list[int]".into())
    );
    assert_eq!(
        l.check_element(&list_string(&[])),
        Err("Output variable 'L': element has type list[string], expected list[int]".into())
    );
    assert_eq!(
        l.check_element(&ExprValue::make_list(vec![], ExprType::list(ExprType::INT)).unwrap()),
        Err("Output variable 'L': element has type list[list[int]], expected list[int]".into())
    );
}

#[test]
fn range_expr_value_written_as_a_string_is_checked_as_written() {
    assert_eq!(
        def("{name: R, type: RANGE_EXPR, minLength: 4}")
            .check_constraints(&ExprValue::String("1-2".into())),
        Err("Output variable 'R': value length 3 is less than minimum 4".into())
    );
    let d = def("{name: R, type: RANGE_EXPR, maxLength: 5}");
    d.check_constraints(&ExprValue::String("1-3".into()))
        .unwrap();
    // `1,2,3` is the range `1-3`, but its written length is checked.
    assert_eq!(
        d.check_constraints(&ExprValue::String("1,2,3,4".into())),
        Err("Output variable 'R': value length 7 exceeds maximum 5".into())
    );
    assert_eq!(
        d.check_constraints(&ExprValue::String("a-b".into())),
        Err("Output variable 'R': value 'a-b' is not a valid range expression".into())
    );
    // Counted in characters: U+3000 is whitespace the range parser accepts,
    // one character of three bytes.
    let wide = "1-3\u{3000}";
    assert!(wide.parse::<RangeExpr>().is_ok());
    def("{name: R, type: RANGE_EXPR, maxLength: 4}")
        .check_constraints(&ExprValue::String(wide.into()))
        .unwrap();
}

// ══════════════════════════════════════════════════════════════
// create_job does not evaluate enableIf yet
// ══════════════════════════════════════════════════════════════

fn create(template: &str) -> Result<openjd_model::job::Job, openjd_model::ModelError> {
    let jt = decode_job_template(
        yaml(template),
        Some(&["EXPR", "CONTROL_FLOW"]),
        &CallerLimits::default(),
    )
    .expect("the template is valid");
    create_job(
        &jt,
        &JobParameterValues::new(),
        &jt.default_validation_context(),
    )
}

#[test]
fn create_job_accepts_enable_if_true() {
    let job = create(
        "specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: J
jobEnvironments: [{name: JE, enableIf: true, variables: {A: a}}]
steps:
  - name: S
    enableIf: true
    stepEnvironments: [{name: SE, enableIf: true, variables: {B: b}}]
    script: {actions: {onRun: {command: echo}}}
",
    )
    .expect("enableIf: true is instantiated as enabled");
    assert_eq!(job.steps.len(), 1);
}

#[test]
fn create_job_decides_a_static_enable_if() {
    let job = create(
        "specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: J
jobEnvironments:
  - {name: JE0, variables: {A: a}}
  - {name: JE1, enableIf: false, variables: {A: a}}
steps:
  - name: S0
    script: {actions: {onRun: {command: echo}}}
  - name: S1
    enableIf: '{{len(Job.Name) == 1}}'
    stepEnvironments:
      - {name: SE0, enableIf: true, variables: {B: b}}
      - {name: SE1, enableIf: false, variables: {B: b}}
    script: {actions: {onRun: {command: echo}}}
  - name: S2
    enableIf: '{{len(Job.Name) != 1}}'
    script: {actions: {onRun: {command: echo}}}
",
    )
    .expect("a static enableIf is decided at job creation");
    let enabled: Vec<bool> = job
        .job_environments
        .as_ref()
        .unwrap()
        .iter()
        .map(|e| e.enabled)
        .collect();
    assert_eq!(enabled, [true, false]);
    assert_eq!(job.steps[0].binding, StepBinding::Bound);
    assert_eq!(job.steps[1].binding, StepBinding::Bound);
    let enabled: Vec<bool> = job.steps[1]
        .step_environments
        .as_ref()
        .unwrap()
        .iter()
        .map(|e| e.enabled)
        .collect();
    assert_eq!(enabled, [true, false]);
    assert_eq!(job.steps[2].binding, StepBinding::Skipped);
    assert_eq!(job.steps[2].parameter_space, None);
    assert_eq!(job.steps[2].resolved_symtab, None);
    assert_eq!(job.steps[2].tasks_known(), Some(0));
    // A disabled Environment serializes `enabled: false`; an enabled one
    // leaves it out, so existing jobs serialize as before.
    let envs = serde_json::to_value(&job.job_environments).unwrap();
    assert_eq!(envs[0].get("enabled"), None);
    assert_eq!(envs[1]["enabled"], serde_json::json!(false));
}

fn z_zero() -> JobParameterValues {
    [(
        "Z".to_string(),
        openjd_model::JobParameterValue {
            param_type: JobParameterType::Int,
            value: ExprValue::Int(0),
        },
    )]
    .into_iter()
    .collect()
}

#[test]
fn a_statically_skipped_step_checks_none_of_its_fields() {
    // Its script timeout and its Environment's variable would both fail.
    let jt = decode_job_template(
        yaml(
            "specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW, FEATURE_BUNDLE_1]
name: J
parameterDefinitions: [{name: Z, type: INT}]
steps:
  - name: S
    enableIf: false
    let: ['q = 1 // Param.Z']
    parameterSpace:
      taskParameterDefinitions: [{name: X, type: INT, range: '{{[1 // Param.Z]}}'}]
    stepEnvironments:
      - {name: SE, variables: {V: '{{1 // Param.Z}}'}}
    script: {actions: {onRun: {command: echo, timeout: '{{Param.Z}}'}}}
",
        ),
        Some(&["EXPR", "CONTROL_FLOW", "FEATURE_BUNDLE_1"]),
        &CallerLimits::default(),
    )
    .unwrap();
    let job = create_job(&jt, &z_zero(), &jt.default_validation_context())
        .expect("a skipped Step evaluates nothing else");
    assert_eq!(job.steps[0].binding, StepBinding::Skipped);
}

#[test]
fn a_disabled_job_environment_is_not_checked() {
    // JE's variable would fail; S0's timeout fails and is reported alone.
    let jt = decode_job_template(
        yaml(
            "specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW, FEATURE_BUNDLE_1]
name: J
parameterDefinitions: [{name: Z, type: INT}]
jobEnvironments:
  - {name: JE, enableIf: '{{Param.Z > 0}}', variables: {V: '{{1 // Param.Z}}'}}
steps:
  - name: S0
    script: {actions: {onRun: {command: echo, timeout: '{{Param.Z}}'}}}
  - name: S1
    enableIf: false
    script: {actions: {onRun: {command: echo}}}
",
        ),
        Some(&["EXPR", "CONTROL_FLOW", "FEATURE_BUNDLE_1"]),
        &CallerLimits::default(),
    )
    .unwrap();
    let err = create_job(&jt, &z_zero(), &jt.default_validation_context())
        .expect_err("S0's timeout is 0");
    assert_eq!(
        err.to_string(),
        "Model validation error: 1 validation error for JobTemplate\n\
         steps[0] -> script -> actions -> onRun -> timeout:\n\ttimeout must be > 0."
    );
}

#[test]
fn a_failing_job_environment_enable_if_fails_create_job() {
    let jt = decode_job_template(
        yaml(
            "specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: J
parameterDefinitions: [{name: Z, type: INT}]
jobEnvironments:
  - {name: JE, enableIf: '{{1 // Param.Z > 0}}', variables: {V: v}}
steps:
  - name: S0
    script: {actions: {onRun: {command: echo}}}
",
        ),
        Some(&["EXPR", "CONTROL_FLOW"]),
        &CallerLimits::default(),
    )
    .unwrap();
    let err =
        create_job(&jt, &z_zero(), &jt.default_validation_context()).expect_err("division by zero");
    assert_eq!(
        err.to_string(),
        "Model validation error: 1 validation error for JobTemplate\n\
         jobEnvironments[0] -> enableIf:\n\tDivision by zero\n  1 // Param.Z > 0\n  ~~^~~~~~~~~~"
    );
}

#[test]
fn constraints_accessor_returns_the_declared_constraints() {
    let d = def("{name: 'N', type: INT, minValue: 1, allowedValues: [1, 2]}");
    assert_eq!(
        *d.constraints(),
        OutputConstraints {
            allowed_values: Some(OutputAllowedValues::Int(vec![1, 2])),
            min_value: Some(OutputBound::Int(1)),
            ..Default::default()
        }
    );
}

#[test]
fn string_outputs_accept_path_values_as_strings() {
    // As for STRING and LIST[STRING] job parameter values.
    let s = def("{name: S, type: STRING, maxLength: 2}");
    assert_eq!(
        s.check_constraints(&ExprValue::new_path("/abc", PathFormat::Posix)),
        Err("Output variable 'S': value length 4 exceeds maximum 2".into())
    );
    let l = def("{name: L, type: 'LIST[STRING]', item: {allowedValues: [/a]}}");
    let paths = ExprValue::make_list(
        vec![ExprValue::new_path("/a", PathFormat::Posix)],
        ExprType::PATH,
    )
    .unwrap();
    l.check_constraints(&paths).unwrap();
}

#[test]
fn deserialized_constraints_of_the_wrong_form_have_no_effect() {
    let ov: OutputVariable = serde_json::from_value(serde_json::json!({
        "name": "A", "type": "INT",
        "constraints": {"minValue": {"float": 100.0}, "allowedValues": {"string": ["a"]}}
    }))
    .unwrap();
    ov.check_constraints(&ExprValue::Int(5)).unwrap();
}
