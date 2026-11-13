// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! `createJob` refuses a Job with a loop (`CONTROL_FLOW`, RFC 0013): the
//! bindings expose only Steps, though template decoding accepts loops.

use openjd_for_js::model::{create_job_with_map, decode_job_template_str, JsPathParameterOptions};

#[test]
fn create_job_refuses_a_job_with_a_loop() {
    let template = r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: T
steps:
- name: L
  loop:
    until: "Loop.Iteration >= 1"
    maxIterations: 3
    steps:
    - name: W
      script:
        actions:
          onRun:
            command: echo
"#;
    let template = decode_job_template_str(template, None, None, None).expect("a loop decodes");
    let path_opts = JsPathParameterOptions::new("/tmpl", "/cwd");
    let err = match create_job_with_map(&template, Default::default(), &path_opts, None) {
        Ok(_) => panic!("a Job with a loop must be refused"),
        Err(e) => e,
    };
    assert_eq!(
        err,
        "createJob does not support loops (CONTROL_FLOW, RFC 0013) yet: the Job has loop 'L'."
    );
}
