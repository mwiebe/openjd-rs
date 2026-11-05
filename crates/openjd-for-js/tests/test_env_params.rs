// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! `createJob` with `ENV_PARAMS` templates: the caller's path options
//! apply to Job Parameters only, never to Environment parameter values,
//! which evaluate with POSIX semantics and are used as given; the
//! returned Job has no
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

/// A PATH value of any form, relative ones included, is used as given
/// whatever `pathFormat` is (Template Schemas §4.5 item 3 at spec af53020).
#[test]
fn a_path_value_of_any_form_is_accepted_whatever_the_path_format() {
    let mut windows = JsPathParameterOptions::new("C:\\tmpl", "C:\\cwd");
    windows.set_path_format(JsPathFormat::Windows);
    let posix = JsPathParameterOptions::new("/tmpl", "/cwd");
    for opts in [&windows, &posix] {
        for value in [
            r"'C:\tools'",
            "/tools",
            "'tools'",
            "'C:tools'",
            r"'\tools'",
            r"'\\srv\s'",
        ] {
            create(&WINDOWS_PATH.replace(r"'C:\tools'", value), opts)
                .unwrap_or_else(|e| panic!("{value}: {e}"));
        }
    }
}

/// `pathFormat` applies to Job Parameters only: Environment parameter
/// format strings are TEMPLATE-scope fields and evaluate with POSIX
/// semantics (Expression Language §1.2.1), so `path('/a') / 'b'` is
/// `/a/b` with either option. (The value itself is checked from JS, in
/// `js-tests/smoke.test.js`.)
#[test]
fn the_path_format_option_does_not_reach_environment_parameters() {
    let template = WINDOWS_PATH.replace(r"'C:\tools'", r#""{{ path('/a') / 'b' }}""#);
    create(&template, &JsPathParameterOptions::new("/tmpl", "/cwd")).expect("POSIX joins with /");
    let mut windows = JsPathParameterOptions::new("C:\\tmpl", "C:\\cwd");
    windows.set_path_format(JsPathFormat::Windows);
    create(&template, &windows).expect("WINDOWS evaluates the same");
}

/// A URI PATH value is used as given, whatever `allowUriPathValues` is,
/// which applies to Job Parameters only.
#[test]
fn a_uri_environment_parameter_value_is_accepted_whatever_the_uri_option() {
    let template = WINDOWS_PATH.replace(r"'C:\tools'", "'s3://bucket/key'");
    let mut opts = JsPathParameterOptions::new("/tmpl", "/cwd");
    create(&template, &opts).expect("a URI is used as given");
    opts.set_allow_uri_path_values(true);
    create(&template, &opts).expect("a URI is used as given");
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
        "Model validation error: 1 validation error for JobTemplate\n\
         steps[0] -> dependencies[0] -> environment:\n\
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
        "Model validation error: 1 validation error for JobTemplate\n\
         steps[0]:\n\
         \tStep 'S': no host can satisfy the combined host requirements for attribute \
         'attr.worker.os.family', of which a host has exactly one value: the anyOf and allOf \
         values of steps[0] -> \
         hostRequirements and jobEnvironments[0] -> hostRequirements \
         (Environment 'Linux') have no value in common. Template Schemas §3.3.3."
    );
    create(&HOST_REQUIREMENTS.replace("SCOPE", "ON_REQUEST"), &opts)
        .expect("an Environment the Step does not enter contributes nothing");
}
