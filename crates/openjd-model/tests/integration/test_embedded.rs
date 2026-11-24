// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Tests ported from Python v2023_09/test_embedded.py
//!
//! Additional embedded file parsing tests not already covered in
//! test_environment_template.rs and test_actions_and_steps.rs.
//! Uses job templates for limit enforcement (filename length) since
//! the Rust crate enforces limits in the job template validation path.
//!
//! Also covers validation of `Task.File.<name>` *references* from a step
//! script's action fields back to the embedded files it declares.

use openjd_model::CallerLimits;
use openjd_model::{decode_environment_template, decode_job_template};

fn yaml_val(s: &str) -> serde_json::Value {
    serde_saphyr::from_str(s).unwrap()
}

fn job_with_embedded(embedded_json: &str) -> serde_json::Value {
    yaml_val(&format!(
        r#"{{
        "specificationVersion": "jobtemplate-2023-09",
        "name": "Test",
        "steps": [{{
            "name": "S",
            "script": {{
                "embeddedFiles": [{embedded_json}],
                "actions": {{"onRun": {{"command": "foo", "args": ["{{{{Task.File.Foo}}}}"]}}}}
            }}
        }}]
    }}"#
    ))
}

fn env_with_embedded(embedded_json: &str) -> serde_json::Value {
    yaml_val(&format!(
        r#"{{
        "specificationVersion": "environment-2023-09",
        "environment": {{"name": "Foo", "script": {{
            "embeddedFiles": [{embedded_json}],
            "actions": {{"onEnter": {{"command": "foo"}}}}
        }}}}
    }}"#
    ))
}

fn job_ok(embedded_json: &str) {
    decode_job_template(
        job_with_embedded(embedded_json),
        None,
        &CallerLimits::default(),
    )
    .unwrap();
}

fn job_err(embedded_json: &str) {
    let err = decode_job_template(
        job_with_embedded(embedded_json),
        None,
        &CallerLimits::default(),
    )
    .expect_err(&format!("expected error for embedded: {embedded_json}"));
    let msg = err.to_string();
    assert!(
        msg.contains("embeddedFiles"),
        "Expected embeddedFiles error path, got: {msg}"
    );
}

fn env_ok(embedded_json: &str) {
    decode_environment_template(
        env_with_embedded(embedded_json),
        None,
        &CallerLimits::default(),
    )
    .unwrap();
}

fn env_err(embedded_json: &str) {
    let err = decode_environment_template(
        env_with_embedded(embedded_json),
        None,
        &CallerLimits::default(),
    )
    .expect_err(&format!("expected error for embedded: {embedded_json}"));
    let msg = err.to_string();
    assert!(
        !msg.is_empty(),
        "Expected non-empty error message, got: {msg}"
    );
}

// ══════════════════════════════════════════════════════════════
// Success cases — via environment template
// ══════════════════════════════════════════════════════════════

#[test]
fn data_min_length() {
    env_ok(r#"{"name": "Foo", "type": "TEXT", "data": "1"}"#);
}

#[test]
fn data_long_length() {
    let data = "x".repeat(32 * 1024);
    env_ok(&format!(
        r#"{{"name": "Foo", "type": "TEXT", "data": "{data}"}}"#
    ));
}

#[test]
fn filename_min_length() {
    env_ok(r#"{"name": "Foo", "type": "TEXT", "data": "hello", "filename": "x"}"#);
}

#[test]
fn filename_max_length_env() {
    let name = "x".repeat(64);
    env_ok(&format!(
        r#"{{"name": "Foo", "type": "TEXT", "data": "hello", "filename": "{name}"}}"#
    ));
}

#[test]
fn runnable_true() {
    env_ok(r#"{"name": "Foo", "type": "TEXT", "data": "hello", "runnable": true}"#);
}

#[test]
fn runnable_false() {
    env_ok(r#"{"name": "Foo", "type": "TEXT", "data": "hello", "runnable": false}"#);
}

// ══════════════════════════════════════════════════════════════
// Failure cases — via environment template (serde/structural)
// ══════════════════════════════════════════════════════════════

#[test]
fn runnable_must_be_bool() {
    let v = env_with_embedded(
        r#"{"name": "Foo", "type": "TEXT", "data": "hello", "runnable": "True"}"#,
    );
    let err = decode_environment_template(v, None, &CallerLimits::default())
        .expect_err("runnable must be bool");
    let msg = err.to_string();
    assert!(
        msg.contains("expected a boolean"),
        "Expected boolean type error, got: {msg}"
    );
}

#[test]
fn type_case_sensitive() {
    let v = env_with_embedded(r#"{"name": "Foo", "type": "text", "data": "hello"}"#);
    let err = decode_environment_template(v, None, &CallerLimits::default())
        .expect_err("type is case-sensitive");
    let msg = err.to_string();
    assert!(
        msg.contains("unknown variant `text`, expected `TEXT`"),
        "Expected unknown variant error, got: {msg}"
    );
}

#[test]
fn data_empty() {
    env_err(r#"{"name": "Foo", "type": "TEXT", "data": ""}"#);
}

#[test]
fn filename_empty() {
    env_err(r#"{"name": "Foo", "type": "TEXT", "data": "hello", "filename": ""}"#);
}

#[test]
fn filename_with_forward_slash() {
    env_err(r#"{"name": "Foo", "type": "TEXT", "data": "hello", "filename": "dir/file.txt"}"#);
}

#[test]
fn filename_with_backslash() {
    env_err(r#"{"name": "Foo", "type": "TEXT", "data": "hello", "filename": "dir\\file.txt"}"#);
}

// ══════════════════════════════════════════════════════════════
// Filename length limits — via job template (where limits are enforced)
// ══════════════════════════════════════════════════════════════

#[test]
fn filename_max_length_job() {
    let name = "x".repeat(64);
    job_ok(&format!(
        r#"{{"name": "Foo", "type": "TEXT", "data": "hello", "filename": "{name}"}}"#
    ));
}

#[test]
fn filename_too_long_job() {
    let name = "x".repeat(65);
    job_err(&format!(
        r#"{{"name": "Foo", "type": "TEXT", "data": "hello", "filename": "{name}"}}"#
    ));
}
// ══════════════════════════════════════════════════════════════
// Task.File.<name> reference validation
//
// RFC 0005 declares `Task.File.<name>` as `path` typed and shows property
// access on one directly inside a format string:
//
//     echo "Script: {{Task.File.Run.name}}"
//
// The embedded-file key is only the FIRST dotted segment after `Task.File.`;
// any further segments are property access on the resulting path value. A
// validator that looks the whole dotted tail up as an embedded-file key
// rejects the form the RFC itself uses.
// ══════════════════════════════════════════════════════════════

/// Job template declaring one embedded file named `file_name`, with `command`
/// and the single onRun arg supplied separately, so a test can vary the
/// `Task.File.*` reference — and its position — independently of the embedded
/// file that is actually declared. Both fields are scanned by the validator.
fn job_with_command_and_arg(file_name: &str, command: &str, arg: &str) -> serde_json::Value {
    yaml_val(&format!(
        r#"{{
        "specificationVersion": "jobtemplate-2023-09",
        "extensions": ["EXPR"],
        "name": "Test",
        "steps": [{{
            "name": "S",
            "script": {{
                "embeddedFiles": [{{"name": "{file_name}", "type": "TEXT", "filename": "run.txt", "data": "contents"}}],
                "actions": {{"onRun": {{"command": "{command}", "args": ["{arg}"]}}}}
            }}
        }}]
    }}"#
    ))
}

fn decode_template(v: serde_json::Value) -> Result<(), String> {
    decode_job_template(v, Some(&["EXPR"]), &CallerLimits::default())
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Reference placed in the onRun `args`.
fn decode_with_arg(file_name: &str, arg: &str) -> Result<(), String> {
    decode_template(job_with_command_and_arg(file_name, "foo", arg))
}

/// Reference placed in the onRun `command`.
fn decode_with_command(file_name: &str, command: &str) -> Result<(), String> {
    decode_template(job_with_command_and_arg(file_name, command, "arg"))
}

#[test]
fn task_file_direct_reference_accepted() {
    // Baseline: the bare reference with no property access.
    decode_with_arg("Run", "{{Task.File.Run}}").expect("bare Task.File.<name> must be accepted");
}

#[test]
fn task_file_direct_property_access_accepted() {
    // The exact form RFC 0005 uses. `.name` is a property of the path value,
    // not part of the embedded-file key.
    decode_with_arg("Run", "{{Task.File.Run.name}}")
        .expect("direct .name property access on Task.File.<name> must be accepted");
}

#[test]
fn task_file_direct_property_access_suffix_accepted() {
    decode_with_arg("Run", "{{Task.File.Run.suffix}}")
        .expect("direct .suffix property access on Task.File.<name> must be accepted");
}

#[test]
fn task_file_direct_property_access_parent_accepted() {
    decode_with_arg("Run", "{{Task.File.Run.parent}}")
        .expect("direct .parent property access on Task.File.<name> must be accepted");
}

#[test]
fn task_file_nested_property_chain_accepted() {
    // Only the FIRST segment is the file key — not "all but the last". A
    // chained property access must not be mistaken for a longer file key.
    decode_with_arg("Run", "{{Task.File.Run.parent.name}}")
        .expect("chained property access on Task.File.<name> must be accepted");
}

#[test]
fn task_file_property_access_in_command_accepted() {
    // The validator scans onRun.command as well as args.
    decode_with_command("Run", "{{Task.File.Run.name}}")
        .expect("property access in onRun.command must be accepted");
}

// ── Negative controls: an undefined embedded file must still be rejected ──
//
// Note: these templates also fail the format-string symbol resolution, which
// independently reports `Undefined variable`. So the load-bearing part of each
// assertion below is the *message content*, not merely that decoding failed.

#[test]
fn task_file_unknown_name_rejected() {
    let msg = decode_with_arg("Run", "{{Task.File.Missing}}").expect_err("expected decode failure");
    assert!(
        msg.contains("steps[0] -> script:\n\treferences undefined embedded file 'Missing'."),
        "Expected undefined-embedded-file error at steps[0] -> script naming 'Missing', got: {msg}"
    );
}

#[test]
fn task_file_unknown_name_with_property_rejected() {
    // Still an undefined embedded file, and the error must name the file key
    // ('Missing') rather than the property-access tail ('Missing.name').
    let msg =
        decode_with_arg("Run", "{{Task.File.Missing.name}}").expect_err("expected decode failure");
    assert!(
        msg.contains("steps[0] -> script:\n\treferences undefined embedded file 'Missing'."),
        "Expected error to name the embedded-file key 'Missing', got: {msg}"
    );
    // The property tail must not be folded into the embedded-file key. Only
    // this specific message is constrained: the format-string validator also
    // reports the whole dotted path as an undefined variable, which is correct.
    assert!(
        !msg.contains("undefined embedded file 'Missing.name'"),
        "Property tail must not be treated as part of the file key, got: {msg}"
    );
}

#[test]
fn task_file_unknown_name_in_command_rejected() {
    let msg = decode_with_command("Run", "{{Task.File.Missing.name}}")
        .expect_err("expected decode failure");
    assert!(
        msg.contains("steps[0] -> script:\n\treferences undefined embedded file 'Missing'."),
        "Expected undefined-embedded-file error for a reference in command, got: {msg}"
    );
}

#[test]
fn embedded_file_name_with_dot_rejected() {
    // Load-bearing invariant for the first-segment split above: an embedded
    // file name is an identifier, so it can never itself contain a dot. If
    // this ever changed, `Task.File.a.b` would become ambiguous.
    let msg = decode_with_arg("My.File", "{{Task.File.Run}}")
        .expect_err("dotted embedded file name must be rejected");
    assert!(
        msg.contains("'My.File' is not a valid identifier"),
        "Expected invalid-identifier error for a dotted embedded file name, got: {msg}"
    );
}

// ══════════════════════════════════════════════════════════════
// Embedded file names `x` and `x.y` in one script
// ══════════════════════════════════════════════════════════════
//
// `x.y` is not an `<Identifier>`, and `Task.File.x.y` / `Env.File.x.y`
// would nest under the path `x` binds. Validation reports the name error
// and nothing else: `x` still resolves, and no panic escapes from the
// symbol-table insert. Both orders are covered, because which name binds
// first decides which insert conflicts.

const DOTTED_PAIRS: [(&str, &str); 2] = [("x", "x.y"), ("x.y", "x")];

fn files_json(first: &str, second: &str) -> String {
    format!(
        r#"[{{"name": "{first}", "type": "TEXT", "data": "a"}}, {{"name": "{second}", "type": "TEXT", "data": "b"}}]"#
    )
}

fn dotted_index(first: &str) -> usize {
    if first.contains('.') {
        0
    } else {
        1
    }
}

fn dotted_name_error(template_kind: &str, files_path: &str, first: &str) -> String {
    format!(
        "Model validation error: 1 validation error for {template_kind}\n\
         {files_path}[{}] -> name:\n\t'x.y' is not a valid identifier.",
        dotted_index(first)
    )
}

#[test]
fn step_script_files_x_and_x_dot_y_is_a_name_error() {
    for (first, second) in DOTTED_PAIRS {
        let files = files_json(first, second);
        let err = decode_job_template(
            yaml_val(&format!(
                r#"{{
                "specificationVersion": "jobtemplate-2023-09",
                "name": "J",
                "steps": [{{"name": "S", "script": {{
                    "embeddedFiles": {files},
                    "actions": {{"onRun": {{"command": "{{{{Task.File.x}}}}"}}}}
                }}}}]
            }}"#
            )),
            None,
            &CallerLimits::default(),
        )
        .expect_err("expected a validation error")
        .to_string();
        assert_eq!(
            err,
            dotted_name_error("JobTemplate", "steps[0] -> script -> embeddedFiles", first),
            "files {first}, {second}"
        );
    }
}

#[test]
fn job_env_files_x_and_x_dot_y_is_a_name_error() {
    for (first, second) in DOTTED_PAIRS {
        let files = files_json(first, second);
        let err = decode_job_template(
            yaml_val(&format!(
                r#"{{
                "specificationVersion": "jobtemplate-2023-09",
                "name": "J",
                "jobEnvironments": [{{"name": "E", "script": {{
                    "embeddedFiles": {files},
                    "actions": {{"onEnter": {{"command": "{{{{Env.File.x}}}}"}}}}
                }}}}],
                "steps": [{{"name": "S", "script": {{"actions": {{"onRun": {{"command": "echo"}}}}}}}}]
            }}"#
            )),
            None,
            &CallerLimits::default(),
        )
        .expect_err("expected a validation error")
        .to_string();
        assert_eq!(
            err,
            dotted_name_error(
                "JobTemplate",
                "jobEnvironments[0] -> script -> embeddedFiles",
                first
            ),
            "files {first}, {second}"
        );
    }
}

#[test]
fn step_env_files_x_and_x_dot_y_is_a_name_error() {
    for (first, second) in DOTTED_PAIRS {
        let files = files_json(first, second);
        let err = decode_job_template(
            yaml_val(&format!(
                r#"{{
                "specificationVersion": "jobtemplate-2023-09",
                "name": "J",
                "steps": [{{
                    "name": "S",
                    "stepEnvironments": [{{"name": "E", "script": {{
                        "embeddedFiles": {files},
                        "actions": {{"onEnter": {{"command": "{{{{Env.File.x}}}}"}}}}
                    }}}}],
                    "script": {{"actions": {{"onRun": {{"command": "echo"}}}}}}
                }}]
            }}"#
            )),
            None,
            &CallerLimits::default(),
        )
        .expect_err("expected a validation error")
        .to_string();
        assert_eq!(
            err,
            dotted_name_error(
                "JobTemplate",
                "steps[0] -> stepEnvironments[0] -> script -> embeddedFiles",
                first
            ),
            "files {first}, {second}"
        );
    }
}

#[test]
fn env_template_files_x_and_x_dot_y_is_a_name_error() {
    for (first, second) in DOTTED_PAIRS {
        let files = files_json(first, second);
        let err = decode_environment_template(
            yaml_val(&format!(
                r#"{{
                "specificationVersion": "environment-2023-09",
                "environment": {{"name": "E", "script": {{
                    "embeddedFiles": {files},
                    "actions": {{"onEnter": {{"command": "{{{{Env.File.x}}}}"}}}}
                }}}}
            }}"#
            )),
            None,
            &CallerLimits::default(),
        )
        .expect_err("expected a validation error")
        .to_string();
        assert_eq!(
            err,
            dotted_name_error(
                "EnvironmentTemplate",
                "environment -> script -> embeddedFiles",
                first
            ),
            "files {first}, {second}"
        );
    }
}
