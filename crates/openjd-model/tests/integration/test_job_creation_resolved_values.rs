// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Resolved-value checks at job creation (`create_job`) — the
//! carried-forward session/task-scope format strings: action
//! `command`/`args`, environment `variables` values, and embedded-file
//! `data`. See the Resolved-Value Checks on Carried-Forward Fields
//! section of `specs/model/job-creation.md`.
//!
//! Every violation here depends only on job parameter values, so it is
//! not statically knowable at template validation (which sees an
//! unresolved `Param.*` and a lower bound of 0) but is fully decidable
//! at job creation, when the parameters are bound — before any worker
//! runs a task.
//!
//! Failure tests assert the full field path + message per the repo's
//! error-message test standard. Passing controls keep partially
//! unresolved strings (contributing 0 to the bound) accepted.

use openjd_expr::path_mapping::PathFormat;
use openjd_model::{create_job, decode_job_template, job, preprocess_job_parameters, CallerLimits};

fn yaml_val(s: &str) -> serde_json::Value {
    serde_saphyr::from_str(s).unwrap()
}

/// Decode with the *default* (uncapped) caller limits — so template
/// validation passes — then run `create_job` with `limits`, mirroring a service
/// that enforces stricter caller limits at submission than at check.
fn create_with_limits(
    template_json: &str,
    params: &[(&str, &str)],
    limits: CallerLimits,
) -> Result<job::Job, String> {
    let root = tempfile::TempDir::new().unwrap();
    let dir = root.path().to_str().unwrap();
    let v = yaml_val(template_json);
    let jt = decode_job_template(
        v,
        Some(&["EXPR", "FEATURE_BUNDLE_1", "WRAP_ACTIONS", "TASK_CHUNKING"]),
        &CallerLimits::default(),
    )
    .expect("template must pass validation under default limits");
    let input: std::collections::HashMap<String, openjd_expr::ExprValue> = params
        .iter()
        .map(|(k, v)| (k.to_string(), openjd_expr::ExprValue::String(v.to_string())))
        .collect();
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
    .map_err(|e| e.to_string())?;
    let mut ctx = jt.default_validation_context();
    ctx.caller_limits = limits;
    create_job(&jt, &processed, &ctx).map_err(|e| e.to_string())
}

fn create_default(template_json: &str, params: &[(&str, &str)]) -> Result<job::Job, String> {
    create_with_limits(template_json, params, CallerLimits::default())
}

fn assert_err_contains(result: Result<job::Job, String>, expected: &[&str]) {
    let msg = result.expect_err("expected create_job to fail");
    for line in expected {
        assert!(
            msg.contains(line),
            "Missing in error output: {line:?}\nGot:\n{msg}"
        );
    }
}

// ══════════════════════════════════════════════════════════════
// §4.4.2 environment variable values — ≤ 2048 resolved characters
// (spec-mandated, always on)
// ══════════════════════════════════════════════════════════════

fn job_env_var_template(value: &str) -> String {
    format!(
        r#"{{
        "specificationVersion": "jobtemplate-2023-09",
        "extensions": ["EXPR"],
        "name": "Test",
        "parameterDefinitions": [{{"name": "X", "type": "STRING"}}],
        "jobEnvironments": [{{"name": "Env", "variables": {{"FOO": "{value}"}}}}],
        "steps": [{{"name": "S", "script": {{"actions": {{"onRun": {{"command": "echo"}}}}}}}}]
    }}"#
    )
}

#[test]
fn job_env_variable_over_2048_from_param_fails_at_create_job() {
    // Template validation sees an unresolved Param.X (lower bound 0)
    // and passes; the bound value makes the violation decidable at job
    // creation.
    assert_err_contains(
        create_default(
            &job_env_var_template("{{Param.X}}"),
            &[("X", &"A".repeat(3000))],
        ),
        &[
            "jobEnvironments[0] -> variables -> FOO:",
            "resolves to at least 3000 characters, exceeding the maximum of 2048.",
        ],
    );
}

#[test]
fn job_env_variable_under_limit_passes() {
    create_default(&job_env_var_template("{{Param.X}}"), &[("X", "short")])
        .expect("under-limit value must pass");
}

#[test]
fn job_env_variable_with_unresolved_session_part_passes() {
    // Session.WorkingDirectory is only known on the worker: it
    // contributes 0 to the bound, and the concrete part is under 2048.
    create_default(
        &job_env_var_template("{{Session.WorkingDirectory}}/{{Param.X}}"),
        &[("X", "short")],
    )
    .expect("partially unresolved under-limit value must pass");
}

#[test]
fn job_env_variable_bound_over_limit_fails_despite_unresolved_part() {
    // The unresolved segment cannot shrink the resolved value below the
    // concrete segment's contribution.
    assert_err_contains(
        create_default(
            &job_env_var_template("{{Session.WorkingDirectory}}/{{Param.X}}"),
            &[("X", &"A".repeat(3000))],
        ),
        &[
            "jobEnvironments[0] -> variables -> FOO:",
            "resolves to at least 3001 characters, exceeding the maximum of 2048.",
        ],
    );
}

#[test]
fn step_env_variable_over_2048_from_param_fails_at_create_job() {
    let template = format!(
        r#"{{
        "specificationVersion": "jobtemplate-2023-09",
        "extensions": ["EXPR"],
        "name": "Test",
        "parameterDefinitions": [{{"name": "X", "type": "STRING"}}],
        "steps": [{{
            "name": "S",
            "stepEnvironments": [{{"name": "Env", "variables": {{"FOO": "{}"}}}}],
            "script": {{"actions": {{"onRun": {{"command": "echo"}}}}}}
        }}]
    }}"#,
        "{{Param.X}}"
    );
    assert_err_contains(
        create_default(&template, &[("X", &"A".repeat(3000))]),
        &[
            "steps[0] -> stepEnvironments[0] -> variables -> FOO:",
            "resolves to at least 3000 characters, exceeding the maximum of 2048.",
        ],
    );
}

#[test]
fn env_let_binding_value_flows_into_variable_check() {
    // The environment's `let` binding is evaluated into the check
    // symbol table, so a variable interpolating it is fully static once
    // parameters are bound.
    let template = format!(
        r#"{{
        "specificationVersion": "jobtemplate-2023-09",
        "extensions": ["EXPR"],
        "name": "Test",
        "parameterDefinitions": [{{"name": "X", "type": "STRING"}}],
        "jobEnvironments": [{{
            "name": "Env",
            "variables": {{"FOO": "{}"}},
            "script": {{
                "let": ["doubled = Param.X + Param.X"],
                "actions": {{"onEnter": {{"command": "echo"}}}}
            }}
        }}],
        "steps": [{{"name": "S", "script": {{"actions": {{"onRun": {{"command": "echo"}}}}}}}}]
    }}"#,
        "{{doubled}}"
    );
    assert_err_contains(
        create_default(&template, &[("X", &"A".repeat(1500))]),
        &[
            "jobEnvironments[0] -> variables -> FOO:",
            "resolves to at least 3000 characters, exceeding the maximum of 2048.",
        ],
    );
}

// ══════════════════════════════════════════════════════════════
// §5.1/§5.2 command and args — opt-in CallerLimits::max_resolved_arg_len
// ══════════════════════════════════════════════════════════════

fn arg_template(arg: &str) -> String {
    format!(
        r#"{{
        "specificationVersion": "jobtemplate-2023-09",
        "extensions": ["EXPR"],
        "name": "Test",
        "parameterDefinitions": [
            {{"name": "X", "type": "STRING"}},
            {{"name": "N", "type": "INT"}}
        ],
        "steps": [{{"name": "S", "script": {{"actions": {{"onRun": {{"command": "echo", "args": ["{arg}"]}}}}}}}}]
    }}"#
    )
}

fn arg_cap(n: usize) -> CallerLimits {
    CallerLimits {
        max_resolved_arg_len: Some(n),
        ..Default::default()
    }
}

#[test]
fn arg_over_cap_from_param_fails_at_create_job() {
    assert_err_contains(
        create_with_limits(
            &arg_template("{{Param.X}}"),
            &[("X", &"A".repeat(200)), ("N", "1")],
            arg_cap(100),
        ),
        &[
            "steps[0] -> script -> actions -> onRun -> args[0]:",
            "resolves to at least 200 characters, exceeding the maximum of 100.",
        ],
    );
}

#[test]
fn arg_expression_blowup_from_int_param_fails_at_create_job() {
    // `'A' * Param.N` is unbounded at template validation and
    // decidable the moment N is bound.
    assert_err_contains(
        create_with_limits(
            &arg_template("{{ 'A' * Param.N }}"),
            &[("X", "x"), ("N", "100000")],
            arg_cap(1024),
        ),
        &[
            "steps[0] -> script -> actions -> onRun -> args[0]:",
            "resolves to at least 100000 characters, exceeding the maximum of 1024.",
        ],
    );
}

#[test]
fn arg_under_cap_passes() {
    create_with_limits(
        &arg_template("{{Param.X}}"),
        &[("X", "short"), ("N", "1")],
        arg_cap(100),
    )
    .expect("under-cap arg must pass");
}

#[test]
fn arg_with_unresolved_task_part_contributes_zero() {
    // Task.Param values are unknown until a session runs a task; only
    // the concrete parts count toward the bound.
    let template = format!(
        r#"{{
        "specificationVersion": "jobtemplate-2023-09",
        "extensions": ["EXPR"],
        "name": "Test",
        "parameterDefinitions": [{{"name": "X", "type": "STRING"}}],
        "steps": [{{
            "name": "S",
            "parameterSpace": {{"taskParameterDefinitions": [{{"name": "Frame", "type": "INT", "range": "1-10"}}]}},
            "script": {{"actions": {{"onRun": {{"command": "echo", "args": ["--frame={}-{}"]}}}}}}
        }}]
    }}"#,
        "{{Task.Param.Frame}}", "{{Param.X}}"
    );
    create_with_limits(&template, &[("X", "short")], arg_cap(100))
        .expect("bound counts only concrete segments");
}

#[test]
fn arg_without_cap_passes_whatever_the_length() {
    // Group B: §5.2 sets no maximum — the default posture stays uncapped.
    create_default(
        &arg_template("{{Param.X}}"),
        &[("X", &"A".repeat(5000)), ("N", "1")],
    )
    .expect("no cap, no limit beyond the spec");
}

#[test]
fn command_over_cap_from_param_fails_at_create_job() {
    let template = format!(
        r#"{{
        "specificationVersion": "jobtemplate-2023-09",
        "extensions": ["EXPR"],
        "name": "Test",
        "parameterDefinitions": [{{"name": "X", "type": "STRING"}}],
        "steps": [{{"name": "S", "script": {{"actions": {{"onRun": {{"command": "{}"}}}}}}}}]
    }}"#,
        "{{Param.X}}"
    );
    assert_err_contains(
        create_with_limits(&template, &[("X", &"A".repeat(200))], arg_cap(100)),
        &[
            "steps[0] -> script -> actions -> onRun -> command:",
            "resolves to at least 200 characters, exceeding the maximum of 100.",
        ],
    );
}

#[test]
fn env_action_arg_over_cap_fails_at_create_job() {
    let template = format!(
        r#"{{
        "specificationVersion": "jobtemplate-2023-09",
        "extensions": ["EXPR"],
        "name": "Test",
        "parameterDefinitions": [{{"name": "X", "type": "STRING"}}],
        "jobEnvironments": [{{
            "name": "Env",
            "script": {{"actions": {{"onEnter": {{"command": "echo", "args": ["{}"]}}}}}}
        }}],
        "steps": [{{"name": "S", "script": {{"actions": {{"onRun": {{"command": "echo"}}}}}}}}]
    }}"#,
        "{{Param.X}}"
    );
    assert_err_contains(
        create_with_limits(&template, &[("X", &"A".repeat(200))], arg_cap(100)),
        &[
            "jobEnvironments[0] -> script -> actions -> onEnter -> args[0]:",
            "resolves to at least 200 characters, exceeding the maximum of 100.",
        ],
    );
}

// ══════════════════════════════════════════════════════════════
// §6.1.2 embedded file data — opt-in CallerLimits::max_resolved_data_len
// ══════════════════════════════════════════════════════════════

fn data_cap(n: usize) -> CallerLimits {
    CallerLimits {
        max_resolved_data_len: Some(n),
        ..Default::default()
    }
}

fn step_data_template() -> String {
    format!(
        r#"{{
        "specificationVersion": "jobtemplate-2023-09",
        "extensions": ["EXPR"],
        "name": "Test",
        "parameterDefinitions": [{{"name": "X", "type": "STRING"}}],
        "steps": [{{
            "name": "S",
            "script": {{
                "embeddedFiles": [{{"name": "F", "type": "TEXT", "data": "{}"}}],
                "actions": {{"onRun": {{"command": "echo"}}}}
            }}
        }}]
    }}"#,
        "{{Param.X}}"
    )
}

#[test]
fn embedded_file_data_over_cap_from_param_fails_at_create_job() {
    assert_err_contains(
        create_with_limits(
            &step_data_template(),
            &[("X", &"A".repeat(200))],
            data_cap(100),
        ),
        &[
            "steps[0] -> script -> embeddedFiles[0] -> data:",
            "resolves to at least 200 characters, exceeding the maximum of 100.",
        ],
    );
}

#[test]
fn embedded_file_data_under_cap_passes() {
    create_with_limits(&step_data_template(), &[("X", "short")], data_cap(100))
        .expect("under-cap data must pass");
}

#[test]
fn env_embedded_file_data_over_cap_fails_at_create_job() {
    let template = format!(
        r#"{{
        "specificationVersion": "jobtemplate-2023-09",
        "extensions": ["EXPR"],
        "name": "Test",
        "parameterDefinitions": [{{"name": "X", "type": "STRING"}}],
        "jobEnvironments": [{{
            "name": "Env",
            "script": {{
                "embeddedFiles": [{{"name": "F", "type": "TEXT", "data": "{}"}}],
                "actions": {{"onEnter": {{"command": "echo"}}}}
            }}
        }}],
        "steps": [{{"name": "S", "script": {{"actions": {{"onRun": {{"command": "echo"}}}}}}}}]
    }}"#,
        "{{Param.X}}"
    );
    assert_err_contains(
        create_with_limits(&template, &[("X", &"A".repeat(200))], data_cap(100)),
        &[
            "jobEnvironments[0] -> script -> embeddedFiles[0] -> data:",
            "resolves to at least 200 characters, exceeding the maximum of 100.",
        ],
    );
}

// ══════════════════════════════════════════════════════════════
// Environment action coverage: onExit and the RFC 0008 wrap hooks
// ══════════════════════════════════════════════════════════════

#[test]
fn env_on_exit_arg_over_cap_fails_at_create_job() {
    let template = format!(
        r#"{{
        "specificationVersion": "jobtemplate-2023-09",
        "extensions": ["EXPR"],
        "name": "Test",
        "parameterDefinitions": [{{"name": "X", "type": "STRING"}}],
        "jobEnvironments": [{{
            "name": "Env",
            "script": {{"actions": {{
                "onEnter": {{"command": "echo"}},
                "onExit": {{"command": "echo", "args": ["{}"]}}
            }}}}
        }}],
        "steps": [{{"name": "S", "script": {{"actions": {{"onRun": {{"command": "echo"}}}}}}}}]
    }}"#,
        "{{Param.X}}"
    );
    assert_err_contains(
        create_with_limits(&template, &[("X", &"A".repeat(200))], arg_cap(100)),
        &[
            "jobEnvironments[0] -> script -> actions -> onExit -> args[0]:",
            "resolves to at least 200 characters, exceeding the maximum of 100.",
        ],
    );
}

#[test]
fn wrap_hook_arg_over_cap_fails_at_create_job() {
    // The wrap hooks see their WrappedAction.* scope (unresolved,
    // contributing 0) alongside the concrete parameters.
    let template = format!(
        r#"{{
        "specificationVersion": "jobtemplate-2023-09",
        "extensions": ["WRAP_ACTIONS", "EXPR"],
        "name": "Test",
        "parameterDefinitions": [{{"name": "X", "type": "STRING"}}],
        "jobEnvironments": [{{
            "name": "Wrapper",
            "script": {{"actions": {{
                "onWrapEnvEnter": {{"command": "echo"}},
                "onWrapTaskRun": {{"command": "{}", "args": ["{}"]}},
                "onWrapEnvExit": {{"command": "echo"}}
            }}}}
        }}],
        "steps": [{{"name": "S", "script": {{"actions": {{"onRun": {{"command": "echo"}}}}}}}}]
    }}"#,
        "{{WrappedAction.Command}}", "--tag={{Param.X}}"
    );
    assert_err_contains(
        create_with_limits(&template, &[("X", &"A".repeat(200))], arg_cap(100)),
        &[
            "jobEnvironments[0] -> script -> actions -> onWrapTaskRun -> args[0]:",
            "resolves to at least 206 characters, exceeding the maximum of 100.",
        ],
    );
}

// ══════════════════════════════════════════════════════════════
// Compound errors: a budget exceedance nested inside an if/else whose
// test is unresolved must still be reported (kind is checked
// recursively through sub_errors)
// ══════════════════════════════════════════════════════════════

#[test]
fn budget_exceedance_inside_unresolved_conditional_is_reported() {
    // Both branches evaluate (the test is a Session.* placeholder) and
    // both blow the shared memory counter, producing a compound error
    // whose top-level kind is Other with the budget kinds nested in
    // sub_errors. The check must find them there.
    assert_err_contains(
        create_with_limits(
            &arg_template("{{ 'A' * Param.N if Session.HasPathMappingRules else 'B' }}"),
            &[("X", "x"), ("N", "10000000")],
            CallerLimits {
                max_eval_memory_bytes: Some(1024 * 1024),
                ..Default::default()
            },
        ),
        &[
            "steps[0] -> script -> actions -> onRun -> args[0]:",
            "exceeded limit",
        ],
    );
}

#[test]
fn operation_budget_inside_unresolved_conditional_is_reported() {
    assert_err_contains(
        create_with_limits(
            &arg_template("{{ sum([1] * int(Param.N)) if Session.HasPathMappingRules else 'B' }}"),
            &[("X", "x"), ("N", "1000")],
            CallerLimits {
                max_eval_operations: Some(50),
                ..Default::default()
            },
        ),
        &[
            "steps[0] -> script -> actions -> onRun -> args[0]:",
            "exceeded limit",
        ],
    );
}

// ══════════════════════════════════════════════════════════════
// SimpleAction sugar (§3.5, FEATURE_BUNDLE_1): the body is an
// embedded-file data value, the user args are argv entries — checked
// at the paths the author wrote, at both stages
// ══════════════════════════════════════════════════════════════

fn bash_template(body: &str, arg: &str) -> String {
    format!(
        r#"{{
        "specificationVersion": "jobtemplate-2023-09",
        "extensions": ["EXPR", "FEATURE_BUNDLE_1"],
        "name": "Test",
        "parameterDefinitions": [{{"name": "X", "type": "STRING"}}],
        "steps": [{{"name": "S", "bash": {{"script": "{body}", "args": ["{arg}"]}}}}]
    }}"#
    )
}

#[test]
fn simple_action_body_over_data_cap_fails_at_create_job_at_the_sugar_path() {
    assert_err_contains(
        create_with_limits(
            &bash_template("echo {{Param.X}}", "ok"),
            &[("X", &"A".repeat(200))],
            data_cap(100),
        ),
        &[
            "steps[0] -> bash -> script:",
            "resolves to at least 205 characters, exceeding the maximum of 100.",
        ],
    );
}

#[test]
fn simple_action_arg_over_arg_cap_fails_at_create_job_at_the_sugar_path() {
    assert_err_contains(
        create_with_limits(
            &bash_template("echo ok", "{{Param.X}}"),
            &[("X", &"A".repeat(200))],
            arg_cap(100),
        ),
        &[
            "steps[0] -> bash -> args[0]:",
            "resolves to at least 200 characters, exceeding the maximum of 100.",
        ],
    );
}

#[test]
fn simple_action_under_caps_passes() {
    create_with_limits(
        &bash_template("echo {{Param.X}}", "{{Param.X}}"),
        &[("X", "short")],
        CallerLimits {
            max_resolved_arg_len: Some(100),
            max_resolved_data_len: Some(100),
            ..Default::default()
        },
    )
    .expect("under-cap SimpleAction must pass");
}

#[test]
fn simple_action_body_over_data_cap_fails_at_template_validation() {
    // Pass 8 validates the same sugar fields the job-creation check
    // does — a fully static violation fails `check`, not just
    // submission.
    let v = yaml_val(&bash_template("{{ 'A' * 200 }}", "ok"));
    let err = decode_job_template(
        v,
        Some(&["EXPR", "FEATURE_BUNDLE_1"]),
        &CallerLimits {
            max_resolved_data_len: Some(100),
            ..Default::default()
        },
    )
    .expect_err("static over-cap body must fail validation");
    let msg = err.to_string();
    for line in [
        "steps[0] -> bash -> script:",
        "resolves to at least 200 characters, exceeding the maximum of 100.",
    ] {
        assert!(msg.contains(line), "Missing: {line:?}\nGot:\n{msg}");
    }
}

#[test]
fn simple_action_undefined_reference_fails_at_template_validation() {
    // Previously the sugar fields were never reference-checked at
    // template validation at all.
    let v = yaml_val(&bash_template("echo {{Param.Undefined}}", "ok"));
    let err = decode_job_template(
        v,
        Some(&["EXPR", "FEATURE_BUNDLE_1"]),
        &CallerLimits::default(),
    )
    .expect_err("undefined variable in a SimpleAction body must fail validation");
    let msg = err.to_string();
    for line in ["steps[0] -> bash -> script:", "Undefined variable"] {
        assert!(msg.contains(line), "Missing: {line:?}\nGot:\n{msg}");
    }
}

// ══════════════════════════════════════════════════════════════
// Aggregation: violations report together for the whole template,
// mirroring template validation
// ══════════════════════════════════════════════════════════════

#[test]
fn violations_across_steps_and_job_environments_report_together() {
    let template = format!(
        r#"{{
        "specificationVersion": "jobtemplate-2023-09",
        "extensions": ["EXPR"],
        "name": "Test",
        "parameterDefinitions": [{{"name": "X", "type": "STRING"}}],
        "jobEnvironments": [{{"name": "Env", "variables": {{"FOO": "{p}"}}}}],
        "steps": [
            {{"name": "A", "script": {{"actions": {{"onRun": {{"command": "echo", "args": ["{p}"]}}}}}}}},
            {{"name": "B", "script": {{"actions": {{"onRun": {{"command": "echo"}}}}}}}},
            {{"name": "C", "script": {{"actions": {{"onRun": {{"command": "echo", "args": ["{p}"]}}}}}}}}
        ]
    }}"#,
        p = "{{Param.X}}"
    );
    let msg = create_with_limits(&template, &[("X", &"A".repeat(3000))], arg_cap(100))
        .expect_err("expected violations");
    // One error message carries every violation: both steps' args and
    // the job environment's variable — no fix-one-resubmit round trips.
    for line in [
        "steps[0] -> script -> actions -> onRun -> args[0]:",
        "steps[2] -> script -> actions -> onRun -> args[0]:",
        "jobEnvironments[0] -> variables -> FOO:",
    ] {
        assert!(msg.contains(line), "Missing: {line:?}\nGot:\n{msg}");
    }
}

// ══════════════════════════════════════════════════════════════
// CHUNK[INT] task parameters bind as RangeExpr in the check symtab,
// matching what the session binds at run time — so a field referencing
// one is genuinely evaluated (not silently skipped as a type error)
// ══════════════════════════════════════════════════════════════

#[test]
fn chunk_int_task_parameter_reference_is_evaluated_not_skipped() {
    let template = format!(
        r#"{{
        "specificationVersion": "jobtemplate-2023-09",
        "extensions": ["EXPR", "TASK_CHUNKING"],
        "name": "Test",
        "parameterDefinitions": [{{"name": "X", "type": "STRING"}}],
        "steps": [{{
            "name": "S",
            "parameterSpace": {{"taskParameterDefinitions": [
                {{"name": "Frames", "type": "CHUNK[INT]", "range": "1-100",
                  "chunks": {{"defaultTaskCount": 10, "rangeConstraint": "CONTIGUOUS"}}}}
            ]}},
            "script": {{"actions": {{"onRun": {{
                "command": "echo",
                "args": ["--frames={f}", "{p}"]
            }}}}}}
        }}]
    }}"#,
        f = "{{Task.Param.Frames}}",
        p = "{{Param.X}}"
    );
    // The unresolved CHUNK[INT] reference contributes 0; the concrete
    // parameter still trips the cap — proof the arg was evaluated
    // rather than skipped on a type error.
    assert_err_contains(
        create_with_limits(&template, &[("X", &"A".repeat(200))], arg_cap(100)),
        &[
            "steps[0] -> script -> actions -> onRun -> args[1]:",
            "resolves to at least 200 characters, exceeding the maximum of 100.",
        ],
    );
}

// ══════════════════════════════════════════════════════════════
// Evaluation budgets — CallerLimits::max_eval_memory_bytes applies to
// job creation's evaluations, as it does to template validation and
// the session runtime
// ══════════════════════════════════════════════════════════════

#[test]
fn lowered_memory_budget_fails_carried_forward_expression_at_create_job() {
    // `'A' * Param.N` with a large bound N exceeds a lowered memory
    // budget during the job-creation evaluation — the spec's own lever
    // against expression blowups, applied at this stage too.
    assert_err_contains(
        create_with_limits(
            &arg_template("{{ 'A' * Param.N }}"),
            &[("X", "x"), ("N", "10000000")],
            CallerLimits {
                max_eval_memory_bytes: Some(1024 * 1024),
                ..Default::default()
            },
        ),
        &[
            "steps[0] -> script -> actions -> onRun -> args[0]:",
            "Expression memory usage (10000136 bytes) exceeded limit (1048576 bytes)",
        ],
    );
}

#[test]
fn lowered_memory_budget_fails_job_name_resolution() {
    let template = format!(
        r#"{{
        "specificationVersion": "jobtemplate-2023-09",
        "extensions": ["EXPR"],
        "name": "{}",
        "parameterDefinitions": [{{"name": "N", "type": "INT"}}],
        "steps": [{{"name": "S", "script": {{"actions": {{"onRun": {{"command": "echo"}}}}}}}}]
    }}"#,
        "{{ 'A' * Param.N }}"
    );
    let err = create_with_limits(
        &template,
        &[("N", "10000000")],
        CallerLimits {
            max_eval_memory_bytes: Some(1024 * 1024),
            ..Default::default()
        },
    )
    .expect_err("expected job name resolution to exceed the memory budget");
    assert!(err.contains("Failed to resolve job name"), "Got:\n{err}");
}
