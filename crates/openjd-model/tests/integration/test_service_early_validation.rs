// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Early validation of SERVICE values (RFC 0009).
//!
//! The EXPR extension's principle is that anything the template alone
//! determines is checked at template validation (`openjd check`), not
//! deferred to job creation or the worker. These tests pin that principle
//! for every SERVICE surface that carries a format string: a value that
//! static evaluation can resolve — a literal expression, arithmetic on
//! literals, a `let` binding, a type error on a `Service.*` symbol — is
//! rejected by `decode_job_template` / `decode_environment_template`, while
//! a value that genuinely depends on a Job Parameter is accepted and left
//! to job creation. The companion tests in `test_service_job_creation.rs`
//! cover the deferred half.

use openjd_model::CallerLimits;
use openjd_model::{decode_environment_template, decode_job_template};

fn yaml_val(s: &str) -> serde_json::Value {
    serde_saphyr::from_str(s).unwrap()
}

const EXTS: &[&str] = &["EXPR", "SERVICE"];

fn job_err(s: &str, expected: &[&str]) {
    let err = decode_job_template(yaml_val(s), Some(EXTS), &CallerLimits::default())
        .expect_err("Expected validation error");
    let msg = err.to_string();
    for line in expected {
        assert!(
            msg.contains(line),
            "Missing in error output: {line:?}\nGot:\n{msg}"
        );
    }
}

fn job_ok(s: &str) {
    if let Err(e) = decode_job_template(yaml_val(s), Some(EXTS), &CallerLimits::default()) {
        panic!("Expected template to validate, got:\n{e}");
    }
}

fn env_err(s: &str, expected: &[&str]) {
    let err = decode_environment_template(yaml_val(s), Some(EXTS), &CallerLimits::default())
        .expect_err("Expected validation error");
    let msg = err.to_string();
    for line in expected {
        assert!(
            msg.contains(line),
            "Missing in error output: {line:?}\nGot:\n{msg}"
        );
    }
}

/// A Job Template with one Service whose port, ready timeout,
/// restart policy, variable, and consuming Task arg are supplied by the
/// caller, so each test varies exactly one field. The Step lists
/// `service:Store` in its `dependencies`, which both keeps the Service from
/// being unused and makes `Service.Store.*` visible in its script (§9.1).
fn job(port: &str, timeout: &str, max_attempts: &str, var: &str, task_arg: &str) -> String {
    format!(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, EXPR]
name: EarlyValidation
parameterDefinitions:
  - name: P
    type: INT
    default: 1
services:
  - name: Store
    let:
      - big = 70000
      - fine = 8080
    ports:
      - name: main
        port: {port}
    healthCheck:
      type: TCP_CONNECT
      readinessTimeoutSeconds: {timeout}
    restartPolicy:
      maxAttempts: {max_attempts}
    variables:
      V: {var}
    script:
      actions:
        onRun:
          command: sleep
          args: ["30"]
steps:
  - name: S
    dependencies:
      - dependsOn: "service:Store"
    script:
      actions:
        onRun:
          command: echo
          args: [{task_arg}]
"#
    )
}

const OK_PORT: &str = r#""{{ fine }}""#;
const OK_TIMEOUT: &str = "30";
const OK_ATTEMPTS: &str = "1";
const OK_VAR: &str = r#""x""#;
const OK_ARG: &str = r#""{{ Service.Store.main.port }}""#;

// ── Numeric @fmtstring fields (§9.2 note) ──────────────────────────────

#[test]
fn literal_expression_port_out_of_range_is_rejected_at_validation() {
    job_err(
        &job(r#""{{ 70000 }}""#, OK_TIMEOUT, OK_ATTEMPTS, OK_VAR, OK_ARG),
        &["services[0] -> ports[0] -> port:\n\tmust be between 1 and 65535."],
    );
}

#[test]
fn arithmetic_on_literals_is_folded_and_range_checked() {
    job_err(
        &job(
            r#""{{ 65535 + 1 }}""#,
            OK_TIMEOUT,
            OK_ATTEMPTS,
            OK_VAR,
            OK_ARG,
        ),
        &["services[0] -> ports[0] -> port:\n\tmust be between 1 and 65535."],
    );
    job_ok(&job(
        r#""{{ 8000 + 80 }}""#,
        OK_TIMEOUT,
        OK_ATTEMPTS,
        OK_VAR,
        OK_ARG,
    ));
}

#[test]
fn let_bound_values_are_resolved_and_range_checked() {
    // `big` is bound to 70000 in the Service's `let`.
    job_err(
        &job(r#""{{ big }}""#, OK_TIMEOUT, OK_ATTEMPTS, OK_VAR, OK_ARG),
        &["services[0] -> ports[0] -> port:\n\tmust be between 1 and 65535."],
    );
    job_ok(&job(OK_PORT, OK_TIMEOUT, OK_ATTEMPTS, OK_VAR, OK_ARG));
}

#[test]
fn readiness_timeout_seconds_zero_is_rejected_at_validation() {
    job_err(
        &job(OK_PORT, r#""{{ 0 }}""#, OK_ATTEMPTS, OK_VAR, OK_ARG),
        &["services[0] -> healthCheck -> readinessTimeoutSeconds:\n\tmust be > 0."],
    );
}

#[test]
fn max_attempts_negative_and_non_integer_are_rejected_at_validation() {
    job_err(
        &job(OK_PORT, OK_TIMEOUT, r#""{{ -1 }}""#, OK_VAR, OK_ARG),
        &["services[0] -> restartPolicy -> maxAttempts:\n\tmust be >= 0."],
    );
    job_err(
        &job(OK_PORT, OK_TIMEOUT, r#""{{ 2.5 }}""#, OK_VAR, OK_ARG),
        &[
            "services[0] -> restartPolicy -> maxAttempts:",
            "  2.5\n  ^~~",
        ],
    );
}

#[test]
fn parameter_dependent_numeric_fields_are_deferred_to_job_creation() {
    // The template alone cannot determine these values (a submission may
    // override the default), so validation accepts them; job creation
    // checks the resolved value (see test_service_job_creation.rs).
    job_ok(&job(
        r#""{{ Param.P * 2 }}""#,
        r#""{{ Param.P }}""#,
        r#""{{ Param.P - 1 }}""#,
        OK_VAR,
        OK_ARG,
    ));
}

// ── Service variables (§4.4.2 length limit applied to Services) ────────

#[test]
fn service_variable_statically_too_long_is_rejected_at_validation() {
    job_err(
        &job(
            OK_PORT,
            OK_TIMEOUT,
            OK_ATTEMPTS,
            r#""{{ 'x' * 3000 }}""#,
            OK_ARG,
        ),
        &[
            "services[0] -> variables -> V:\n\tresolves to at least 3000 characters, exceeding the maximum of 2048.",
        ],
    );
}

// ── Static typing of Service.* symbols (§7.3.1, §7.4) ──────────────────
//
// Service.* values are unresolved until the Service is placed, but their
// types are known at validation: port is int, the addresses are strings.
// Misuse is a type error now, not a runtime failure on the worker.

#[test]
fn string_method_on_int_port_is_a_type_error_at_validation() {
    job_err(
        &job(
            OK_PORT,
            OK_TIMEOUT,
            OK_ATTEMPTS,
            OK_VAR,
            r#""{{ Service.Store.main.port.upper() }}""#,
        ),
        &[
            "steps[0] -> script -> actions -> onRun -> args[0]:",
            "upper() is not available for int. Available for: string",
            "  Service.Store.main.port.upper()\n  ~~~~~~~~~~~~~~~~~~~~~~~~^~~~~~~",
        ],
    );
}

#[test]
fn string_address_plus_int_is_a_type_error_at_validation() {
    job_err(
        &job(
            OK_PORT,
            OK_TIMEOUT,
            OK_ATTEMPTS,
            OK_VAR,
            r#""{{ Service.Store.main.connectAddress + 1 }}""#,
        ),
        &[
            "steps[0] -> script -> actions -> onRun -> args[0]:",
            "  Service.Store.main.connectAddress + 1\n  ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~^~~",
        ],
    );
}

#[test]
fn int_arithmetic_on_port_type_checks_at_validation() {
    job_ok(&job(
        OK_PORT,
        OK_TIMEOUT,
        OK_ATTEMPTS,
        OK_VAR,
        r#""{{ Service.Store.main.port + 1 }}""#,
    ));
}

#[test]
fn join_host_port_with_swapped_arguments_is_a_type_error_at_validation() {
    job_err(
        &job(
            OK_PORT,
            OK_TIMEOUT,
            OK_ATTEMPTS,
            OK_VAR,
            r#""{{ join_host_port(Service.Store.main.port, Service.Store.main.connectAddress) }}""#,
        ),
        &[
            "steps[0] -> script -> actions -> onRun -> args[0]:",
            "No matching signature for join_host_port(int, string)",
        ],
    );
}

#[test]
fn join_host_port_with_service_symbols_type_checks_at_validation() {
    job_ok(&job(
        OK_PORT,
        OK_TIMEOUT,
        OK_ATTEMPTS,
        OK_VAR,
        r#""http://{{ join_host_port(Service.Store.main.connectAddress, Service.Store.main.port) }}""#,
    ));
}

// ── Environment Templates get the same early checks ────────────────────

#[test]
fn environment_template_services_and_environment_are_checked_at_validation() {
    env_err(
        r#"
specificationVersion: environment-2023-09
extensions: [SERVICE, EXPR]
services:
  - name: Cache
    ports:
      - name: main
        port: "{{ 99999 }}"
    script:
      actions:
        onRun:
          command: sleep
          args: ["30"]
environment:
  name: Client
  runScope: [TASK]
  variables:
    CACHE_PORT: "{{ Service.Cache.main.port.upper() }}"
    CACHE_URL: "http://{{ join_host_port(Service.Cache.main.connectAddress, Service.Cache.main.port) }}"
"#,
        &[
            "2 validation errors for EnvironmentTemplate",
            "services[0] -> ports[0] -> port:\n\tmust be between 1 and 65535.",
            "environment -> variables -> CACHE_PORT:",
            "upper() is not available for int. Available for: string",
        ],
    );
}
