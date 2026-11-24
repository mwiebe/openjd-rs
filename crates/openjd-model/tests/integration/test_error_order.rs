// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Validation and job-creation errors come out in the same order on every
//! run. A template's mapping keys (an Environment's `variables`, the keys
//! of a `<Cancelation>`) and the task parameters missing from a
//! combination expression are reported in name order.
//!
//! Each test decodes the same input many times in one process. Every
//! `HashMap` gets its own random hash seed, so a check that walked one
//! would give different orders across the iterations.
//!
//! Failure tests assert the full error message per the repo's
//! error-message test standard.

use openjd_expr::path_mapping::PathFormat;
use openjd_model::{
    create_job, decode_environment_template, decode_job_template, preprocess_job_parameters,
    CallerLimits,
};

const RUNS: usize = 32;

const EXTS: &[&str] = &["EXPR", "FEATURE_BUNDLE_1", "WRAP_ACTIONS"];

fn yaml_val(s: &str) -> serde_json::Value {
    serde_saphyr::from_str(s).unwrap()
}

/// Run `f` [`RUNS`] times, assert it gave the same message every time, and
/// return that message.
fn same_every_run(f: impl Fn() -> String) -> String {
    let first = f();
    for run in 1..RUNS {
        let again = f();
        assert_eq!(again, first, "run {run} differs from run 0");
    }
    first
}

fn job_err(template: &str) -> String {
    decode_job_template(yaml_val(template), Some(EXTS), &CallerLimits::default())
        .expect_err("expected a validation error")
        .to_string()
}

fn env_err(template: &str) -> String {
    decode_environment_template(yaml_val(template), Some(EXTS), &CallerLimits::default())
        .expect_err("expected a validation error")
        .to_string()
}

fn undefined(path: &str, name: &str) -> String {
    format!(
        "{path}:\n\tFailed to parse interpolation expression at [0, 10]. \
         Undefined variable: '{name}'.\n  {name}\n  ~~~~~^"
    )
}

#[test]
fn job_environment_variable_errors_are_in_key_order() {
    let template = r#"
specificationVersion: jobtemplate-2023-09
name: J
jobEnvironments:
- name: E
  variables:
    V3: "{{Nope.D}}"
    V0: "{{Nope.A}}"
    V5: "{{Nope.F}}"
    V1: "{{Nope.B}}"
    V4: "{{Nope.E}}"
    V2: "{{Nope.C}}"
steps:
- name: S
  script:
    actions:
      onRun:
        command: echo
"#;
    let msg = same_every_run(|| job_err(template));
    let mut expected =
        vec!["Model validation error: 6 validation errors for JobTemplate".to_string()];
    for (var, name) in [
        ("V0", "Nope.A"),
        ("V1", "Nope.B"),
        ("V2", "Nope.C"),
        ("V3", "Nope.D"),
        ("V4", "Nope.E"),
        ("V5", "Nope.F"),
    ] {
        expected.push(undefined(
            &format!("jobEnvironments[0] -> variables -> {var}"),
            name,
        ));
    }
    assert_eq!(msg, expected.join("\n"));
}

#[test]
fn step_environment_variable_name_errors_are_in_key_order() {
    // Structural checks on the names (pass 6) walk the same map.
    let template = r#"
specificationVersion: jobtemplate-2023-09
name: J
steps:
- name: S
  stepEnvironments:
  - name: E
    variables:
      9c: x
      2b: x
      5a: x
      0d: x
  script:
    actions:
      onRun:
        command: echo
"#;
    let msg = same_every_run(|| job_err(template));
    let mut expected =
        vec!["Model validation error: 4 validation errors for JobTemplate".to_string()];
    for var in ["0d", "2b", "5a", "9c"] {
        expected.push(format!(
            "steps[0] -> stepEnvironments[0] -> variables -> {var}:\n\t\
             variable name '{var}' cannot start with a digit."
        ));
    }
    assert_eq!(msg, expected.join("\n"));
}

#[test]
fn environment_template_variable_errors_are_in_key_order() {
    let template = r#"
specificationVersion: environment-2023-09
environment:
  name: E
  variables:
    B: "{{Nope.B}}"
    D: "{{Nope.D}}"
    A: "{{Nope.A}}"
    C: "{{Nope.C}}"
"#;
    let msg = same_every_run(|| env_err(template));
    let mut expected =
        vec!["Model validation error: 4 validation errors for EnvironmentTemplate".to_string()];
    for (var, name) in [
        ("A", "Nope.A"),
        ("B", "Nope.B"),
        ("C", "Nope.C"),
        ("D", "Nope.D"),
    ] {
        expected.push(undefined(
            &format!("environment -> variables -> {var}"),
            name,
        ));
    }
    assert_eq!(msg, expected.join("\n"));
}

#[test]
fn job_creation_variable_errors_are_in_key_order() {
    // Each value passes template validation (Param.X is unresolved there)
    // and exceeds the 2048-character limit once X is bound.
    let template = r#"
specificationVersion: jobtemplate-2023-09
extensions: [EXPR]
name: J
parameterDefinitions:
- {name: X, type: INT}
jobEnvironments:
- name: E
  variables:
    FOO: "{{ 'f' * (Param.X * 1000) }}"
    BAR: "{{ 'b' * (Param.X * 1000) }}"
    QUX: "{{ 'q' * (Param.X * 1000) }}"
    BAZ: "{{ 'z' * (Param.X * 1000) }}"
steps:
- name: S
  script:
    actions:
      onRun:
        command: echo
"#;
    let root = tempfile::TempDir::new().unwrap();
    let dir = root.path().to_str().unwrap();
    let input: std::collections::HashMap<String, openjd_expr::ExprValue> =
        [("X".to_string(), openjd_expr::ExprValue::String("3".into()))]
            .into_iter()
            .collect();
    let msg = same_every_run(|| {
        let jt = decode_job_template(yaml_val(template), Some(EXTS), &CallerLimits::default())
            .expect("template must pass validation");
        let processed = preprocess_job_parameters(
            &jt,
            &input,
            &[],
            &openjd_model::PathParameterOptions {
                job_template_dir: dir,
                current_working_dir: dir,
                allow_template_dir_walk_up: true,
                path_format: PathFormat::host(),
                allow_uri_path_values: true,
            },
        )
        .unwrap();
        create_job(&jt, &processed, &jt.default_validation_context())
            .expect_err("expected create_job to fail")
            .to_string()
    });
    let mut expected =
        vec!["Model validation error: 4 validation errors for JobTemplate".to_string()];
    for var in ["BAR", "BAZ", "FOO", "QUX"] {
        expected.push(format!(
            "jobEnvironments[0] -> variables -> {var}:\n\tresolves to at least 3000 characters, \
             exceeding the maximum of 2048."
        ));
    }
    assert_eq!(msg, expected.join("\n"));
}

#[test]
fn unknown_cancelation_key_reported_is_the_first_in_key_order() {
    for (mode, extra) in [
        ("TERMINATE", "TERMINATE accepts no additional fields"),
        ("NOTIFY_THEN_TERMINATE", "expected `notifyPeriodInSeconds`"),
        ("\"{{X}}\"", "expected `notifyPeriodInSeconds`"),
    ] {
        let template = format!(
            r#"
specificationVersion: jobtemplate-2023-09
name: J
steps:
- name: S
  script:
    actions:
      onRun:
        command: echo
        cancelation:
          mode: {mode}
          k3: 1
          k0: 1
          bogus: 1
          k2: 1
          k1: 1
"#
        );
        let msg = same_every_run(|| job_err(&template));
        assert_eq!(
            msg,
            format!(
                "Validation error: 'jobtemplate-2023-09' failed checks: \
                 unknown field `bogus`, {extra}"
            )
        );
    }
}

#[test]
fn unknown_job_cancelation_key_reported_is_the_first_in_key_order() {
    // `job::CancelationMode` deserializes job documents directly, so the
    // map it sees keeps the document's order; it reports the key that
    // sorts first, whatever the document order.
    for mode in ["TERMINATE", "NOTIFY_THEN_TERMINATE", "{{X}}"] {
        let json = format!(r#"{{"mode": "{mode}", "zz": 1, "k1": 1, "aa": 1, "k0": 1}}"#);
        let msg = same_every_run(|| {
            serde_json::from_str::<openjd_model::job::CancelationMode>(&json)
                .expect_err("expected an unknown-field error")
                .to_string()
        });
        assert_eq!(msg, "unknown field `aa`");
    }
}

#[test]
fn missing_combination_parameters_are_in_name_order() {
    let template = r#"
specificationVersion: jobtemplate-2023-09
name: J
steps:
- name: S
  parameterSpace:
    taskParameterDefinitions:
    - {name: E, type: INT, range: [1]}
    - {name: A, type: INT, range: [1]}
    - {name: D, type: INT, range: [1]}
    - {name: B, type: INT, range: [1]}
    - {name: C, type: INT, range: [1]}
    combination: "A"
  script:
    actions:
      onRun:
        command: echo
"#;
    let msg = same_every_run(|| job_err(template));
    let mut expected =
        vec!["Model validation error: 4 validation errors for JobTemplate".to_string()];
    for name in ["B", "C", "D", "E"] {
        expected.push(format!(
            "steps[0] -> parameterSpace -> combination:\n\t\
             parameter '{name}' missing from combination expression."
        ));
    }
    assert_eq!(msg, expected.join("\n"));
}

#[test]
fn a_duplicated_task_parameter_is_reported_missing_once() {
    let msg = job_err(
        r#"
specificationVersion: jobtemplate-2023-09
name: J
steps:
- name: S
  parameterSpace:
    taskParameterDefinitions:
    - {name: A, type: INT, range: [1]}
    - {name: B, type: INT, range: [1]}
    - {name: B, type: INT, range: [1]}
    combination: "A"
  script:
    actions:
      onRun:
        command: echo
"#,
    );
    assert_eq!(
        msg,
        "Model validation error: 2 validation errors for JobTemplate\n\
         steps[0] -> parameterSpace -> taskParameterDefinitions[2]:\n\t\
         duplicate task parameter name 'B'.\n\
         steps[0] -> parameterSpace -> combination:\n\t\
         parameter 'B' missing from combination expression."
    );
}
