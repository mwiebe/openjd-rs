// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! `createJob` with `ENV_PARAMS` templates: the caller's path options
//! reach the Environment parameter PATH checks, the returned Job has no
//! unresolved `environment:` entries, since `createJob` takes no
//! Environment Templates, and Environment `hostRequirements` combine with
//! a Step's.
//!
//! Each test names its extensions explicitly, so the tests do not change
//! when the default supported-extensions list does. The Job's JSON shape is checked from JS in
//! `js-tests/smoke.test.js`, since `Job.toJSON` needs the wasm boundary.

use openjd_for_js::expr::JsPathFormat;
use openjd_for_js::model::{create_job_with_map, decode_job_template_str, JsPathParameterOptions};

fn exts() -> Vec<String> {
    ["ENV_PARAMS", "EXPR"].map(String::from).to_vec()
}

fn create(template: &str, opts: &JsPathParameterOptions) -> Result<(), String> {
    let template =
        decode_job_template_str(template, None, None, Some(&exts())).expect("template decodes");
    create_job_with_map(&template, Default::default(), opts, None).map(|_| ())
}

const WINDOWS_PATH: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: P, type: PATH, default: 'C:\tools'}]
  variables: {V: v}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#;

#[test]
fn the_path_format_option_is_the_submission_path_format() {
    let mut opts = JsPathParameterOptions::new("C:\\tmpl", "C:\\cwd");
    opts.set_path_format(JsPathFormat::Windows);
    create(WINDOWS_PATH, &opts).expect("a Windows path is absolute for a Windows submission");

    let posix = JsPathParameterOptions::new("/tmpl", "/cwd");
    let err = create(WINDOWS_PATH, &posix).unwrap_err();
    assert_eq!(
        err,
        "Model validation error: 1 validation error for JobTemplate\n\
         jobEnvironments[0] -> envParameterDefinitions[0] -> default:\n\
         \tmust be an absolute path in the submission path format (POSIX), since an Environment \
         parameter has no directory to be relative to; got 'C:\\tools'."
    );
}

#[test]
fn the_allow_uri_option_reaches_environment_parameters() {
    let template = WINDOWS_PATH.replace(r"'C:\tools'", "'s3://bucket/key'");
    let mut opts = JsPathParameterOptions::new("/tmpl", "/cwd");
    let err = create(&template, &opts).unwrap_err();
    assert_eq!(
        err,
        "Model validation error: 1 validation error for JobTemplate\n\
         jobEnvironments[0] -> envParameterDefinitions[0] -> default:\n\
         \tis a URI ('s3://bucket/key'), and URI path values are not permitted for this \
         submission."
    );
    opts.set_allow_uri_path_values(true);
    create(&template, &opts).expect("URIs are permitted");
}

const EXTERNAL: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  dependencies: [{environment: Conda, required: REQUIRED}]
  script: {actions: {onRun: {command: x}}}
"#;

#[test]
fn a_required_external_environment_is_an_error_with_no_attachments() {
    let opts = JsPathParameterOptions::new("/tmpl", "/cwd");
    let err = create(&EXTERNAL.replace("REQUIRED", "true"), &opts).unwrap_err();
    assert_eq!(
        err,
        "Model validation error: 1 validation error for Submission\n\
         JobTemplate -> steps[0] -> dependencies[0] -> environment:\n\
         \tnames Environment 'Conda', which is not a jobEnvironments entry of the Job Template, \
         and no Environment Template is attached. The entry is required; set 'required: false' \
         if the Step can run without the Environment (Template Schemas §3.2.1 item 2)."
    );
    create(&EXTERNAL.replace("REQUIRED", "false"), &opts).expect("an optional entry is ignored");
}

const HOST_REQUIREMENTS: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: Linux
  activation: SCOPE
  hostRequirements:
    attributes: [{name: attr.worker.os.family, anyOf: [linux]}]
  variables: {V: v}
steps:
- name: S
  hostRequirements:
    attributes: [{name: attr.worker.os.family, anyOf: [windows]}]
  script: {actions: {onRun: {command: x}}}
"#;

#[test]
fn create_job_combines_environment_host_requirements() {
    // An ALWAYS Environment's requirements combine with the Step's (Template
    // Schemas §3.3.3); an unrequested ON_REQUEST one's do not.
    let opts = JsPathParameterOptions::new("/tmpl", "/cwd");
    let err = create(&HOST_REQUIREMENTS.replace("SCOPE", "ALWAYS"), &opts).unwrap_err();
    assert_eq!(
        err,
        "Model validation error: 1 validation error for Submission\n\
         JobTemplate -> steps[0]:\n\
         \tStep 'S': no host can satisfy the combined host requirements for attribute \
         'attr.worker.os.family', of which a host has exactly one value: the anyOf and allOf \
         values of JobTemplate -> steps[0] -> \
         hostRequirements and JobTemplate -> jobEnvironments[0] -> hostRequirements \
         (Environment 'Linux') have no value in common. Template Schemas §3.3.3."
    );
    create(&HOST_REQUIREMENTS.replace("SCOPE", "ON_REQUEST"), &opts)
        .expect("an Environment the Step does not enter contributes nothing");
}
