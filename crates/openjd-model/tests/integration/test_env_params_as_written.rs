// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `ENV_PARAMS` PATH and LIST[PATH] values written with a backslash, or of
//! any form, are read as written at every site that reads them: an
//! attached `default`, an Environment's own `enableIf` (a Job
//! Environment's and an attachment's, and the decision cache keyed by the
//! values), constraints at template validation and in
//! `check_environment_templates`, `enable_if_certainly_unresolved`,
//! `hostRequirements` reading `Env.RawParam`, and §3.2.2 equality
//! (Template Schemas §4.5 item 3 at spec af53020; Expression Language
//! §1.2.1). A conversion between path formats, or a re-added form check,
//! at any of these sites changes a result these tests pin.

use openjd_model::job::{Document, Job};
use openjd_model::template::{EnvironmentTemplate, JobTemplate};
use openjd_model::{
    check_environment_templates, create_job, create_job_with_environment_templates,
    AttachedEnvironmentTemplate, ModelError,
};

use crate::env_params_common::{
    env_ok, job_ok, job_params, job_params_with, rendered_values, template_ctx,
};

fn attach(envs: &[EnvironmentTemplate]) -> Vec<AttachedEnvironmentTemplate<'_>> {
    envs.iter().map(AttachedEnvironmentTemplate::new).collect()
}
fn check(jt: &JobTemplate, envs: &[EnvironmentTemplate]) -> Result<(), ModelError> {
    check_environment_templates(jt, &attach(envs), &template_ctx(jt)).map(|_| ())
}
fn apply(jt: &JobTemplate, envs: &[EnvironmentTemplate]) -> Result<Job, ModelError> {
    let values = job_params_with(jt, &[], envs);
    create_job_with_environment_templates(jt, &attach(envs), &values, &template_ctx(jt))
        .map(|c| c.job)
}
fn create(src: &str) -> Job {
    let jt = job_ok(src);
    create_job(&jt, &job_params(&jt, &[]), &template_ctx(&jt)).unwrap_or_else(|e| panic!("{e}"))
}
fn entered(job: &Job, step: usize) -> Vec<String> {
    job.job_environments
        .iter()
        .flatten()
        .filter(|e| job.steps[step].enters_environment(&Document::JobTemplate, &e.name))
        .map(|e| e.name.clone())
        .collect()
}

const STEP_E: &str = "steps:\n- name: S\n  dependencies: [{environment: E}]\n  script: {actions: {onRun: {command: x}}}\n";

/// an attached default with a backslash, as a
/// literal PATH, a single-expression LIST[PATH] and a multi-segment PATH.
#[test]
fn an_attached_default_with_a_backslash_is_the_value_as_written() {
    let env = env_ok(
        r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
parameterDefinitions: [{name: W, type: STRING, default: 'C:\y'}]
environment:
  name: E
  envParameterDefinitions:
  - {name: P, type: PATH, default: 'C:\x'}
  - {name: L, type: "LIST[PATH]", default: "{{ [Param.W] }}"}
  - {name: M, type: PATH, default: '{{Param.W}}\z'}
  variables: {D: "{{Env.Param.P}}"}
"#,
    );
    let jt = job_ok(&format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n{STEP_E}"
    ));
    check(&jt, std::slice::from_ref(&env)).unwrap();
    let job = apply(&jt, &[env]).unwrap();
    assert_eq!(
        rendered_values(&job.steps[0]),
        [r#"EnvironmentTemplate[0]/E: L=list[path]:["C:\\y"], M=path:C:\y\z, P=path:C:\x"#]
    );
}

fn eif_env(enable_if: &str, defs: &str) -> String {
    format!(
        "  activation: ON_REQUEST\n  envParameterDefinitions: {defs}\n  enableIf: \"{enable_if}\"\n  variables: {{V: x}}\n"
    )
}

/// a Job Environment's and an attachment's own enableIf
/// read Env.RawParam of a backslash PATH and LIST[PATH] value as written.
#[test]
fn an_environments_enable_if_reads_a_backslash_raw_param_as_written() {
    let defs = r#"[{name: D, type: PATH, default: 'C:\d'}, {name: L, type: "LIST[PATH]", default: ['C:\d']}]"#;
    for cond in [
        r"Env.RawParam.D == 'C:\\\\d'",
        r"Env.RawParam.L[0] == 'C:\\\\d'",
    ] {
        let body = eif_env(cond, defs);
        let job = create(&format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             jobEnvironments:\n- name: E\n{body}{STEP_E}"
        ));
        assert_eq!(entered(&job, 0), ["E"], "job env: {cond}");
        let env = env_ok(&format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\nenvironment:\n  name: E\n{body}"
        ));
        let jt = job_ok(&format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n{STEP_E}"
        ));
        let job = apply(&jt, &[env]).unwrap();
        assert_eq!(
            rendered_values(&job.steps[0]),
            [r#"EnvironmentTemplate[0]/E: D=path:C:\d, L=list[path]:["C:\\d"]"#],
            "attached: {cond}"
        );
    }
}

/// a Job Environment's own
/// enableIf reads an empty, URI and relative PATH / LIST[PATH] value as given:
/// it is `false`, so E is disabled and its missing Q is no error.
#[test]
fn a_job_environments_enable_if_reads_path_values_of_any_form() {
    for value in ["''", "'s3://b/k'", "'rel/x'"] {
        let job = create(&format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             jobEnvironments:\n- name: E\n  activation: ON_REQUEST\n  envParameterDefinitions: \
             [{{name: P, type: PATH}}, {{name: L, type: \"LIST[PATH]\"}}, {{name: Q, type: STRING}}]\n  \
             enableIf: \"Env.RawParam.P != {value} or Env.RawParam.L != [{value}]\"\n  \
             variables: {{V: \"{{{{Env.Param.Q}}}}\"}}\n\
             steps:\n- name: S\n  dependencies: [{{environment: E, parameters: {{P: {value}, L: [{value}]}}}}]\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n"
        ));
        assert_eq!(entered(&job, 0), Vec::<String>::new(), "{value}");
    }
}

/// template validation checks constraints against
/// the literal as written, backslashes and slashes included.
#[test]
fn template_validation_checks_constraints_against_the_literal_as_written() {
    env_ok(
        r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions:
  - {name: P, type: PATH, allowedValues: ['C:\x'], default: 'C:\x'}
  - {name: L, type: "LIST[PATH]", item: {allowedValues: [/a, 'C:\b']}, default: [/a, 'C:\b']}
  variables: {V: x}
"#,
    );
}

/// check_environment_templates checks
/// an attachment's literal default and entry, and a grouped Job
/// Environment's entry, against constraints as written.
#[test]
fn check_checks_constraints_against_the_literal_as_written() {
    let env = env_ok(
        r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions:
  - {name: P, type: PATH, allowedValues: ['C:\x', /a], default: 'C:\x'}
  - {name: R, type: PATH, allowedValues: [/a], default: /a}
  - {name: Q, type: PATH, allowedValues: ['C:\x', /a]}
  variables: {V: x}
"#,
    );
    for q in [r"'C:\x'", "/a"] {
        let jt = job_ok(&format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             steps:\n- name: S\n  dependencies: [{{environment: E, parameters: {{Q: {q}}}}}]\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n"
        ));
        check(&jt, std::slice::from_ref(&env)).unwrap_or_else(|e| panic!("{q}: {e}"));
        let grouped = job_ok(&format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             jobEnvironments:\n- {{name: C, variables: {{V: c}}}}\n- {{name: J, exclusiveGroup: G, \
             envParameterDefinitions: [{{name: D, type: PATH, allowedValues: ['C:\\x', /a]}}], variables: {{V: j}}}}\n\
             steps:\n- name: S\n  dependencies: [{{environment: C}}, {{environment: J, parameters: {{D: {q}}}}}]\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n"
        ));
        check(&grouped, &[]).unwrap_or_else(|e| panic!("grouped {q}: {e}"));
    }
}

/// enable_if_certainly_unresolved reads a backslash
/// literal as written, so the enableIf stays unresolved and the missing
/// Q is reported by check_environment_templates.
#[test]
fn check_reads_a_backslash_literal_in_an_enable_if_as_written() {
    for (defs, cond, supplied) in [
        (
            "{name: P, type: PATH}",
            r"Env.RawParam.P != 'C:\\\\x'",
            r"{P: 'C:\x'}",
        ),
        (
            "{name: P, type: \"LIST[PATH]\"}",
            r"Env.RawParam.P != ['C:\\\\x']",
            r"{P: ['C:\x']}",
        ),
    ] {
        let env = env_ok(&format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\nenvironment:\n  name: E\n  \
             envParameterDefinitions:\n  - {defs}\n  - {{name: Q, type: STRING}}\n  \
             enableIf: \"{cond} or Env.Param.Q == 'z'\"\n  variables: {{D: \"{{{{Env.Param.Q}}}}\"}}\n"
        ));
        let jt = job_ok(&format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             steps:\n- name: S\n  dependencies: [{{environment: E, parameters: {supplied}}}]\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n"
        ));
        let err = check(&jt, &[env]).expect_err("expected check to report the missing Q");
        assert!(
            err.to_string().contains("missing: 'Q' of Environment 'E'"),
            "{err}"
        );
    }
}

/// hostRequirements read Env.RawParam of a backslash PATH as written.
#[test]
fn host_requirements_read_a_backslash_raw_param_as_written() {
    let job = create(&format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         jobEnvironments:\n- name: E\n  envParameterDefinitions: [{{name: D, type: PATH, default: 'C:\\d'}}]\n  \
         hostRequirements:\n    attributes: [{{name: attr.custom.dir, anyOf: [\"{{{{ 'bs' if Env.RawParam.D.count('/') == 0 else 'fs' }}}}\"]}}]\n  \
         variables: {{V: x}}\n{STEP_E}"
    ));
    let attrs = job.steps[0]
        .host_requirements
        .as_ref()
        .and_then(|h| h.attributes.clone())
        .unwrap();
    assert_eq!(attrs[0].any_of, Some(vec!["bs".to_string()]));
}

/// an attachment's enableIf decisions are keyed by its PATH
/// values: two Steps that differ only in P are decided separately.
#[test]
fn an_attachments_enable_if_is_decided_per_path_value() {
    let env = env_ok(
        r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions: [{name: P, type: PATH}]
  enableIf: "Env.RawParam.P != '/off'"
  variables: {V: x}
"#,
    );
    let jt = job_ok(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\nsteps:\n\
         - name: A\n  dependencies: [{environment: E, parameters: {P: /on}}]\n  script: {actions: {onRun: {command: x}}}\n\
         - name: B\n  dependencies: [{environment: E, parameters: {P: /off}}]\n  script: {actions: {onRun: {command: x}}}\n",
    );
    let job = apply(&jt, &[env]).unwrap();
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["EnvironmentTemplate[0]/E: P=path:/on"]
    );
    assert_eq!(rendered_values(&job.steps[1]), Vec::<String>::new());
}

/// PATH values that differ by a trailing separator, or
/// '' and '/', are different values.
#[test]
fn a_trailing_separator_and_an_empty_path_make_different_values() {
    let job = create(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         jobEnvironments:\n- name: E\n  envParameterDefinitions: [{name: P, type: PATH, default: /p}]\n  variables: {V: v}\n\
         steps:\n\
         - name: A\n  dependencies: [{environment: E, parameters: {P: /a/}}]\n  script: {actions: {onRun: {command: x}}}\n\
         - name: B\n  dependencies: [{environment: E, parameters: {P: /a}}]\n  script: {actions: {onRun: {command: x}}}\n\
         - name: C\n  dependencies: [{environment: E, parameters: {P: ''}}]\n  script: {actions: {onRun: {command: x}}}\n\
         - name: D\n  dependencies: [{environment: E, parameters: {P: /}}]\n  script: {actions: {onRun: {command: x}}}\n",
    );
    let s = &job.steps;
    assert!(!s[0].environment_values_equal(&s[1]), "trailing separator");
    assert!(!s[2].environment_values_equal(&s[3]), "empty and root");
}
