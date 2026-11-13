// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `ENV_PARAMS`: a STRING or PATH effective value, and each element of a
//! LIST[STRING] or LIST[PATH] one, is at most 1024 characters (Template
//! Schemas §4.5 item 5). The limit is one of the parameter's constraints, so
//! a literal is checked at template validation, a resolved value when the
//! Job is created, and neither where the Environment is disabled for the
//! Step (§3.2.2 item 3).

use openjd_model::job::Job;
use openjd_model::{create_job, ModelError};

use crate::env_params_common::{assert_errors, job_err, job_ok, job_params, template_ctx};

fn create(jt_src: &str, params: &[(&str, &str)]) -> Result<Job, ModelError> {
    let jt = job_ok(jt_src);
    let values = job_params(&jt, params);
    create_job(&jt, &values, &template_ctx(&jt))
}

/// A Job Template whose Job Environment `E` has the parameter `V` of type
/// `ty` and the `enableIf` `enable_if`, and whose Step A supplies `value`
/// (YAML) for it. The Job Parameter `Half` defaults to 600 `y`s.
fn template(ty: &str, value: &str, enable_if: &str) -> String {
    let half = "y".repeat(600);
    format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [EXPR, ENV_PARAMS]\nname: j\n\
         parameterDefinitions:\n  - {{name: Half, type: STRING, default: \"{half}\"}}\n  \
         - {{name: F, type: BOOL, default: false}}\n\
         jobEnvironments:\n  - name: E\n    enableIf: \"{enable_if}\"\n    \
         envParameterDefinitions:\n      - {{name: V, type: \"{ty}\"}}\n    \
         script:\n      actions:\n        onEnter: {{command: echo, args: [ENTER, E]}}\n\
         steps:\n  - name: A\n    dependencies:\n      - environment: E\n        \
         parameters:\n          V: {value}\n    script:\n      actions:\n        \
         onRun: {{command: echo, args: [TASK, A]}}\n"
    )
}

fn value_err(path: &str, n: usize) -> String {
    format!(
        "{path}:\n\tParameter 'V': value has {n} characters, more than the 1024 an Environment \
         parameter value may have (Template Schemas §4.5 item 5)"
    )
}

fn item_err(path: &str, i: usize, n: usize) -> String {
    format!(
        "{path}:\n\tParameter 'V': item[{i}] has {n} characters, more than the 1024 an element \
         of an Environment parameter value may have (Template Schemas §4.5 item 5)"
    )
}

const ENTRY: &str = "steps[0] -> dependencies[0] -> parameters -> V";

/// The model name in the error header for a submission with an attached
/// Environment Template.
const MODEL: &str = "Submission";

#[test]
fn a_literal_value_may_have_1024_characters_counted_as_characters() {
    // JT-208. 'é' is two bytes in UTF-8, so a byte count would reject
    // 1024 of them.
    for ty in ["STRING", "PATH"] {
        let v = "é".repeat(1024);
        job_ok(&template(ty, &format!("\"{v}\""), "true"));
    }
}

#[test]
fn a_literal_value_of_1025_characters_is_rejected_at_template_validation() {
    // JT-209.
    for ty in ["STRING", "PATH"] {
        let v = "x".repeat(1025);
        job_err(
            &template(ty, &format!("\"{v}\""), "true"),
            &[&value_err(ENTRY, 1025)],
        );
    }
}

#[test]
fn each_element_of_a_list_literal_is_limited() {
    for ty in ["LIST[STRING]", "LIST[PATH]"] {
        let ok = "x".repeat(1024);
        let long = "x".repeat(1025);
        job_ok(&template(ty, &format!("[\"a\", \"{ok}\"]"), "true"));
        job_err(
            &template(ty, &format!("[\"a\", \"{long}\"]"), "true"),
            &[&item_err(ENTRY, 1, 1025)],
        );
    }
}

#[test]
fn a_literal_default_is_limited() {
    let long = "x".repeat(1025);
    let jt = format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [EXPR, ENV_PARAMS]\nname: j\n\
         jobEnvironments:\n  - name: E\n    envParameterDefinitions:\n      \
         - {{name: V, type: STRING, default: \"{long}\"}}\n      \
         - {{name: L, type: \"LIST[STRING]\", default: [\"{long}\"]}}\n    \
         variables: {{A: b}}\nsteps:\n  - name: A\n    script:\n      actions:\n        \
         onRun: {{command: echo, args: [TASK, A]}}\n"
    );
    job_err(
        &jt,
        &[
            &value_err(
                "jobEnvironments[0] -> envParameterDefinitions[0] -> default",
                1025,
            ),
            &item_err(
                "jobEnvironments[0] -> envParameterDefinitions[1] -> default",
                0,
                1025,
            )
            .replace("'V'", "'L'"),
        ],
    );
}

#[test]
fn a_resolved_value_is_limited_when_the_job_is_created() {
    // JB-193, JB-194: two copies of the 600-character Job Parameter.
    let err = create(
        &template("STRING", "\"{{Param.Half}}{{Param.Half}}\"", "true"),
        &[],
    )
    .unwrap_err()
    .to_string();
    assert_errors(&err, "JobTemplate", &[&value_err(ENTRY, 1200)]);
    let err = create(
        &template(
            "LIST[STRING]",
            "\"{{ ['a', Param.Half + Param.Half] }}\"",
            "true",
        ),
        &[],
    )
    .unwrap_err()
    .to_string();
    assert_errors(&err, "JobTemplate", &[&item_err(ENTRY, 1, 1200)]);
    // A resolved value of exactly 1024 characters is accepted.
    let created = create(
        &template("STRING", "\"{{Param.Half}}{{Param.Half}}\"", "true"),
        &[("Half", &"y".repeat(512))],
    );
    assert!(created.is_ok(), "{:?}", created.err());
}

#[test]
fn a_value_for_a_disabled_environment_is_not_checked() {
    // JB-195: the enableIf reads no parameter and is false for the Step.
    let created = create(
        &template("STRING", "\"{{Param.Half}}{{Param.Half}}\"", "Param.F"),
        &[],
    )
    .unwrap();
    // `Some([])`: the Step's Sessions enter none of the Job's Environments.
    assert_eq!(created.steps[0].environments, Some(vec![]));
}

#[test]
fn other_types_are_not_limited_here() {
    // An INT value has no length; a RANGE_EXPR's is its maxLength
    // (default 1024, §2.10), which this limit does not add to.
    job_ok(&template("INT", "7", "true"));
    let long_range = (1..=300)
        .map(|i| (i * 2).to_string())
        .collect::<Vec<_>>()
        .join(",");
    assert!(long_range.chars().count() > 1024);
    let jt = template("RANGE_EXPR", &format!("\"{long_range}\""), "true").replace(
        "type: \"RANGE_EXPR\"}",
        "type: \"RANGE_EXPR\", maxLength: 2000}",
    );
    job_ok(&jt);
}

#[test]
fn a_value_for_an_attached_environment_is_limited() {
    use openjd_model::template::EnvironmentTemplate;
    use openjd_model::{
        check_environment_templates, create_job_with_environment_templates,
        AttachedEnvironmentTemplate,
    };

    use crate::env_params_common::{env_ok, job_params_with};

    let long = "x".repeat(1025);
    let jt = job_ok(&format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [EXPR, ENV_PARAMS]\nname: j\n\
         steps:\n  - name: A\n    dependencies:\n      - environment: Ext\n        \
         parameters: {{V: \"{long}\"}}\n    script:\n      actions:\n        \
         onRun: {{command: echo, args: [TASK, A]}}\n"
    ));
    let env: EnvironmentTemplate = env_ok(
        "specificationVersion: environment-2023-09\nextensions: [EXPR, ENV_PARAMS]\n\
         environment:\n  name: Ext\n  envParameterDefinitions:\n    - {name: V, type: STRING}\n  \
         variables: {A: b}\n",
    );
    let values = job_params_with(&jt, &[], std::slice::from_ref(&env));
    let attached = [AttachedEnvironmentTemplate::new(&env)];
    let err = create_job_with_environment_templates(&jt, &attached, &values, &template_ctx(&jt))
        .unwrap_err()
        .to_string();
    assert_errors(
        &err,
        MODEL,
        &[&value_err(&format!("JobTemplate -> {ENTRY}"), 1025)],
    );
    // A validator given the Environment Template reports it too.
    let err = check_environment_templates(&jt, &attached, &template_ctx(&jt))
        .unwrap_err()
        .to_string();
    assert_errors(
        &err,
        MODEL,
        &[&value_err(&format!("JobTemplate -> {ENTRY}"), 1025)],
    );
}
