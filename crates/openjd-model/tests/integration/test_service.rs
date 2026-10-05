// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Integration tests for RFC 0009 `SERVICE` extension — schema, extension
//! gating, and structural validation (Template Schemas §1.1 item 8, §3 item
//! 6, §9–§9.7).
//!
//! These tests cover:
//!
//! 1. **Schema parse**: `services`, `requiresServices`, and every `<Service>`
//!    sub-object decode from YAML/JSON, with the numeric `@fmtstring` fields
//!    accepting either an integer or a format string.
//! 2. **Extension gating**: using `services`/`requiresServices` without
//!    `SERVICE`, or declaring `SERVICE` without `EXPR`, produces a specific,
//!    path-annotated validation error.
//! 3. **Structural validation** (§9.7 items 3–5 and 7 as far as they concern
//!    Services): list sizes, name uniqueness and collisions, identifier and
//!    `File` rules, port ranges, health-check/restart constraints, and the reuse
//!    of the `<Environment>`-shaped validators for `variables`,
//!    `hostRequirements`, embedded files and `<Action>`.
//! 4. **RFC "Basic Examples"**: the templates from the RFC, copied verbatim
//!    into `tests/fixtures/rfc0009/`.
//!
//! The `Service.*` format-string scope rules (§9 scope lists, §9.9 items
//! 1–2) and job creation of Services are covered in `test_service_scope.rs`
//! and `test_service_job_creation.rs`; Service scope (§9.1), reference
//! cycles, `dependencies` and the `runScope` default in
//! `test_service_scope_rules.rs`; `requiresServices` in
//! `test_service_requirements.rs`; `<Environment>.runScope`, the
//! `onWrapService*` hooks, and the Environment Template root changes in
//! `test_service_environments.rs`.
//!
//! Error assertions follow the repo convention of asserting on the full
//! Pydantic-style error path + message.

use openjd_model::template::service_scope::compute_service_scopes;
use openjd_model::template::{
    CompletedTasksPolicy, ServiceActions, ServiceHealthCheck, ServicePortProtocol,
    ServiceRestartPolicy, ServiceScope,
};
use openjd_model::{
    decode_environment_template, decode_job_template, CallerLimits, ModelExtension,
};

/// The extensions the Service examples need. `FEATURE_BUNDLE_1` is included
/// so the identifier limit test can show the FB1-raised limit.
const SERVICE_EXTS: &[&str] = &["EXPR", "SERVICE", "FEATURE_BUNDLE_1"];
/// Enables everything except `SERVICE`, to exercise the gating paths.
const NO_SERVICE_EXTS: &[&str] = &["EXPR", "FEATURE_BUNDLE_1"];

const RFC_VALKEY: &str = include_str!("../fixtures/rfc0009/valkey-shared-store.job.yaml");
const RFC_COORDINATOR: &str = include_str!("../fixtures/rfc0009/step-coordinator.job.yaml");
const RFC_QUEUE_CACHE_ENV: &str = include_str!("../fixtures/rfc0009/queue-cache.environment.yaml");
const RFC_QUEUE_CACHE_CONSUMER: &str =
    include_str!("../fixtures/rfc0009/queue-cache-consumer.job.yaml");
const RFC_REQUIRED_QUEUE_CACHE: &str =
    include_str!("../fixtures/rfc0009/required-queue-cache.job.yaml");
const RFC_METRICS_SINK: &str = include_str!("../fixtures/rfc0009/metrics-sink.job.yaml");

fn yaml_val(s: &str) -> serde_json::Value {
    serde_saphyr::from_str(s).unwrap()
}

fn expect_job_err(template: &str, allowed_exts: &[&str], expected_substrings: &[&str]) {
    let err = decode_job_template(
        yaml_val(template),
        Some(allowed_exts),
        &CallerLimits::default(),
    )
    .expect_err("Expected validation error");
    let msg = err.to_string();
    for line in expected_substrings {
        assert!(
            msg.contains(line),
            "Missing expected substring {line:?} in error output:\n{msg}"
        );
    }
}

fn expect_env_err(template: &str, allowed_exts: &[&str], expected_substrings: &[&str]) {
    let err = decode_environment_template(
        yaml_val(template),
        Some(allowed_exts),
        &CallerLimits::default(),
    )
    .expect_err("Expected validation error");
    let msg = err.to_string();
    for line in expected_substrings {
        assert!(
            msg.contains(line),
            "Missing expected substring {line:?} in error output:\n{msg}"
        );
    }
}

fn expect_job_ok(template: &str, allowed_exts: &[&str]) -> openjd_model::template::JobTemplate {
    decode_job_template(
        yaml_val(template),
        Some(allowed_exts),
        &CallerLimits::default(),
    )
    .expect("expected successful decode")
}

/// A job template whose `services` holds exactly `services` (a YAML list
/// body, indented two spaces) plus one trivial step. A handful of INT job
/// parameters are defined so the numeric `@fmtstring` fields can reference
/// them.
fn job_with_services(services: &str) -> String {
    format!(
        r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [SERVICE, EXPR]
name: Test
parameterDefinitions:
  - {{ name: P, type: INT, default: 1 }}
  - {{ name: T, type: INT, default: 1 }}
  - {{ name: I, type: INT, default: 1 }}
  - {{ name: Attempts2, type: INT, default: 1 }}
  - {{ name: MetricsPort, type: INT, default: 9100 }}
  - {{ name: Timeout, type: INT, default: 30 }}
  - {{ name: Attempts, type: INT, default: 2 }}
services:
{services}
steps:
  - name: S
    script:
      actions:
        onRun:
          command: run
"#
    )
}

/// A job template with one job service `Cache` whose body is `body`
/// (indented four spaces beneath the list item).
fn job_with_service_body(body: &str) -> String {
    job_with_services(&format!("  - name: Cache\n{body}"))
}

const MINIMAL_SERVICE_BODY: &str = r#"    ports:
      - name: main
    script:
      actions:
        onRun:
          command: valkey-server
"#;

// ════════════════════════════════════════════════════════════════════
// Extension name
// ════════════════════════════════════════════════════════════════════

#[test]
fn service_extension_name_round_trips() {
    assert_eq!(ModelExtension::Service.as_str(), "SERVICE");
    assert_eq!(
        "SERVICE".parse::<ModelExtension>().unwrap(),
        ModelExtension::Service
    );
    assert!(ModelExtension::ALL.contains(&ModelExtension::Service));
}

#[test]
fn template_profile_includes_service() {
    let jt = expect_job_ok(&job_with_service_body(MINIMAL_SERVICE_BODY), SERVICE_EXTS);
    assert!(jt.profile().has_extension(ModelExtension::Service));
    assert!(jt.profile().has_extension(ModelExtension::Expr));
}

#[test]
fn service_rejected_when_caller_does_not_support_it() {
    expect_job_err(
        &job_with_service_body(MINIMAL_SERVICE_BODY),
        NO_SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "extensions:\n\tUnsupported extension names: SERVICE",
        ],
    );
}

// ════════════════════════════════════════════════════════════════════
// Happy path — schema parse and defaults
// ════════════════════════════════════════════════════════════════════

#[test]
fn minimal_service_decodes_with_spec_defaults() {
    let jt = expect_job_ok(&job_with_service_body(MINIMAL_SERVICE_BODY), SERVICE_EXTS);
    let services = jt.services.as_ref().expect("services decoded");
    assert_eq!(services.len(), 1);
    let svc = &services[0];
    assert_eq!(svc.name, "Cache");
    assert_eq!(svc.port_names().collect::<Vec<_>>(), vec!["main"]);
    assert!(svc.ports[0].port.is_none());

    // §9 item 6: the health check defaults to TCP_CONNECT on every port.
    assert!(svc.health_check.is_none());
    match svc.health_check() {
        ServiceHealthCheck::TcpConnect {
            ports,
            readiness_interval_seconds,
            readiness_timeout_seconds,
            health_interval_seconds,
            failure_threshold,
        } => {
            assert!(ports.is_none());
            assert!(readiness_interval_seconds.is_none());
            assert!(readiness_timeout_seconds.is_none());
            assert!(health_interval_seconds.is_none());
            assert!(failure_threshold.is_none());
        }
        other => panic!("unexpected default health check: {other:?}"),
    }
    assert_eq!(
        ServiceHealthCheck::DEFAULT_TCP_CONNECT_READINESS_INTERVAL_SECONDS,
        1
    );
    assert_eq!(
        ServiceHealthCheck::DEFAULT_COMMAND_READINESS_INTERVAL_SECONDS,
        5
    );
    assert_eq!(ServiceHealthCheck::DEFAULT_READY_TIMEOUT_SECONDS, 300);
    assert_eq!(ServiceHealthCheck::DEFAULT_HEALTH_INTERVAL_SECONDS, 30);
    assert_eq!(ServiceHealthCheck::DEFAULT_FAILURE_THRESHOLD, 3);

    // §9 item 7: restart policy defaults to { maxAttempts: 0, completedTasks: RERUN }.
    assert!(svc.restart_policy.is_none());
    let policy = svc.restart_policy();
    assert!(policy.max_attempts.is_none());
    assert_eq!(ServiceRestartPolicy::DEFAULT_MAX_ATTEMPTS, 0);
    assert_eq!(policy.completed_tasks(), CompletedTasksPolicy::Rerun);

    // RFC 0009 `<Action>` default-timeout table for <ServiceActions>.
    assert_eq!(ServiceActions::default_timeout_seconds("onEnter"), None);
    assert_eq!(ServiceActions::default_timeout_seconds("onRun"), None);
    assert_eq!(
        ServiceActions::default_timeout_seconds("onHealthCheck"),
        Some(30)
    );
    assert_eq!(ServiceActions::default_timeout_seconds("onExit"), Some(300));

    assert!(svc.dependencies.is_none());
    assert!(jt.requires_services.is_none());
}

#[test]
fn full_service_decodes_every_field() {
    let jt = expect_job_ok(
        &job_with_services(
            r#"
  - name: Cache
    description: "A Valkey store"
    let:
      - memory_mib = 8192
    hostRequirements:
      attributes:
        - name: attr.worker.preemptible
          anyOf: ["false"]
      amounts:
        - name: amount.worker.memory
          min: 8192
    ports:
      - name: main
        port: 6379
      - name: metrics
        port: "{{ Param.MetricsPort }}"
    healthCheck:
      type: TCP_CONNECT
      ports: [main]
      readinessTimeoutSeconds: 60
      healthIntervalSeconds: 10
      failureThreshold: 2
    restartPolicy:
      maxAttempts: 3
      completedTasks: KEEP
    variables:
      VALKEY_LOG_LEVEL: notice
    script:
      let:
        - data_dir = Session.WorkingDirectory
      actions:
        onEnter:
          command: bash
          args: ["{{ Service.File.Init }}"]
        onRun:
          command: valkey-server
          args: ["--port", "{{ Service.Cache.main.port }}"]
          cancelation:
            mode: NOTIFY_THEN_TERMINATE
            notifyPeriodInSeconds: 30
        onExit:
          command: valkey-cli
          args: ["save"]
          timeout: 120
      embeddedFiles:
        - name: Init
          type: TEXT
          runnable: true
          data: "echo init"
  - name: Coordinator
    ports:
      - name: api
    healthCheck:
      type: COMMAND
      readinessIntervalSeconds: 2
      readinessTimeoutSeconds: "{{ Param.Timeout }}"
    restartPolicy:
      maxAttempts: "{{ Param.Attempts }}"
    script:
      actions:
        onRun:
          command: coordinator
        onHealthCheck:
          command: coordinator
          args: [ping]
  - name: Logger
    ports:
      - name: ingest
    healthCheck:
      type: STDOUT
      readinessTimeoutSeconds: 10
    script:
      actions:
        onRun:
          command: logger
"#,
        ),
        SERVICE_EXTS,
    );
    let services = jt.services.as_ref().unwrap();
    assert_eq!(services.len(), 3);

    let cache = &services[0];
    assert_eq!(cache.description.as_ref().unwrap().0, "A Valkey store");
    assert_eq!(cache.let_bindings.as_ref().unwrap(), &["memory_mib = 8192"]);
    assert!(cache.host_requirements.is_some());
    assert_eq!(cache.ports[0].port.as_ref().unwrap().raw(), "6379");
    assert_eq!(
        cache.ports[1].port.as_ref().unwrap().raw(),
        "{{ Param.MetricsPort }}"
    );
    match cache.health_check.as_ref().unwrap() {
        ServiceHealthCheck::TcpConnect {
            ports,
            readiness_interval_seconds,
            readiness_timeout_seconds,
            health_interval_seconds,
            failure_threshold,
        } => {
            assert_eq!(ports.as_ref().unwrap(), &["main"]);
            assert!(readiness_interval_seconds.is_none());
            assert_eq!(readiness_timeout_seconds.as_ref().unwrap().raw(), "60");
            assert_eq!(health_interval_seconds.as_ref().unwrap().raw(), "10");
            assert_eq!(failure_threshold.as_ref().unwrap().raw(), "2");
        }
        other => panic!("unexpected health check {other:?}"),
    }
    let policy = cache.restart_policy();
    assert_eq!(policy.max_attempts.as_ref().unwrap().raw(), "3");
    assert_eq!(policy.completed_tasks(), CompletedTasksPolicy::Keep);
    assert_eq!(
        cache.variables.as_ref().unwrap()["VALKEY_LOG_LEVEL"].raw(),
        "notice"
    );
    assert_eq!(
        cache.script.let_bindings.as_ref().unwrap(),
        &["data_dir = Session.WorkingDirectory"]
    );
    let names: Vec<&str> = cache
        .script
        .actions
        .iter_named()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(names, vec!["onEnter", "onRun", "onExit"]);
    assert_eq!(
        cache
            .script
            .actions
            .on_exit
            .as_ref()
            .unwrap()
            .timeout
            .as_ref()
            .unwrap()
            .raw(),
        "120"
    );
    assert_eq!(
        cache.script.embedded_files.as_ref().unwrap()[0].name,
        "Init"
    );

    let coordinator = &services[1];
    match coordinator.health_check.as_ref().unwrap() {
        ServiceHealthCheck::Command {
            readiness_interval_seconds,
            readiness_timeout_seconds,
            health_interval_seconds,
            failure_threshold,
        } => {
            assert_eq!(readiness_interval_seconds.as_ref().unwrap().raw(), "2");
            assert_eq!(
                readiness_timeout_seconds.as_ref().unwrap().raw(),
                "{{ Param.Timeout }}"
            );
            assert!(health_interval_seconds.is_none());
            assert!(failure_threshold.is_none());
        }
        other => panic!("unexpected health check {other:?}"),
    }
    assert_eq!(
        coordinator.restart_policy().max_attempts.unwrap().raw(),
        "{{ Param.Attempts }}"
    );
    assert_eq!(
        coordinator.restart_policy().completed_tasks(),
        CompletedTasksPolicy::Rerun
    );
    assert!(coordinator.script.actions.on_health_check.is_some());

    let logger = &services[2];
    assert_eq!(logger.health_check().type_name(), "STDOUT");
    assert_eq!(
        logger
            .health_check()
            .readiness_timeout_seconds()
            .unwrap()
            .raw(),
        "10"
    );
}

/// The pre-restructure spellings `jobServices` and `<StepTemplate>.stepServices`
/// are not properties: the RFC has one `services` list whose scope follows
/// the references (§9.1). Both are unknown fields at decode.
#[test]
fn job_services_and_step_services_are_unknown_fields() {
    let err = decode_job_template(
        yaml_val(
            r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [SERVICE, EXPR]
name: Test
jobServices:
  - name: Cache
    ports: [{name: main}]
    script: {actions: {onRun: {command: run}}}
steps:
  - name: S
    script: {actions: {onRun: {command: run}}}
"#,
        ),
        Some(SERVICE_EXTS),
        &CallerLimits::default(),
    )
    .expect_err("jobServices is not a property");
    assert!(
        err.to_string()
            .starts_with("Validation error: 'jobtemplate-2023-09' failed checks: unknown field `jobServices`, expected one of "),
        "got: {err}"
    );
    let err = decode_job_template(
        yaml_val(
            r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [SERVICE, EXPR]
name: Test
steps:
  - name: S
    stepServices:
      - name: Coordinator
        ports: [{name: api}]
        script: {actions: {onRun: {command: coordinator}}}
    script: {actions: {onRun: {command: run}}}
"#,
        ),
        Some(SERVICE_EXTS),
        &CallerLimits::default(),
    )
    .expect_err("stepServices is not a property");
    assert!(
        err.to_string()
            .starts_with("Validation error: 'jobtemplate-2023-09' failed checks: unknown field `stepServices`, expected one of "),
        "got: {err}"
    );
}

#[test]
fn ports_accept_integer_string_and_format_string() {
    let jt = expect_job_ok(
        &job_with_service_body(
            r#"    ports:
      - name: a
        port: 1
      - name: b
        port: "65535"
      - name: c
        port: "{{ Param.P }}"
      - name: d
        port: "{{ 6000 + 379 }}"
    script:
      actions:
        onRun:
          command: valkey-server
"#,
        ),
        SERVICE_EXTS,
    );
    let ports = &jt.services.as_ref().unwrap()[0].ports;
    assert_eq!(ports[0].port.as_ref().unwrap().raw(), "1");
    assert_eq!(ports[1].port.as_ref().unwrap().raw(), "65535");
    assert_eq!(ports[2].port.as_ref().unwrap().raw(), "{{ Param.P }}");
    assert_eq!(ports[3].port.as_ref().unwrap().raw(), "{{ 6000 + 379 }}");
}

// ════════════════════════════════════════════════════════════════════
// Extension gating — fields rejected without SERVICE; EXPR prerequisite
// ════════════════════════════════════════════════════════════════════

#[test]
fn job_services_rejected_without_extension() {
    expect_job_err(
        r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [EXPR]
name: Test
services:
  - name: Cache
    ports: [{name: main}]
    script: {actions: {onRun: {command: valkey-server}}}
steps:
  - name: S
    script: {actions: {onRun: {command: run}}}
"#,
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services:\n\tservices requires the SERVICE extension.",
        ],
    );
}

#[test]
fn requires_services_rejected_without_extension() {
    expect_job_err(
        r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [EXPR]
name: Test
requiresServices:
  - name: Cache
    ports: [{name: main}]
steps:
  - name: S
    script: {actions: {onRun: {command: run}}}
"#,
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "requiresServices:\n\trequiresServices requires the SERVICE extension.",
        ],
    );
}

#[test]
fn gating_error_does_not_examine_service_contents() {
    // Without the extension the list is rejected as a whole; an invalid
    // Service inside it is not additionally reported.
    expect_job_err(
        r#"
specificationVersion: "jobtemplate-2023-09"
name: Test
services:
  - name: File
    ports: []
    script: {actions: {onRun: {command: ""}}}
steps:
  - name: S
    script: {actions: {onRun: {command: run}}}
"#,
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services:\n\tservices requires the SERVICE extension.",
        ],
    );
}

#[test]
fn service_requires_expr_in_job_template() {
    expect_job_err(
        r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [SERVICE]
name: Test
services:
  - name: Cache
    ports: [{name: main}]
    script: {actions: {onRun: {command: valkey-server}}}
steps:
  - name: S
    script: {actions: {onRun: {command: run}}}
"#,
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "extensions:\n\tSERVICE requires EXPR; both must be listed in the template's `extensions` (RFC 0009).",
        ],
    );
}

#[test]
fn service_requires_expr_even_without_services() {
    // The prerequisite is on the declaration, not on use.
    expect_job_err(
        r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [SERVICE]
name: Test
steps:
  - name: S
    script: {actions: {onRun: {command: run}}}
"#,
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "extensions:\n\tSERVICE requires EXPR; both must be listed in the template's `extensions` (RFC 0009).",
        ],
    );
}

#[test]
fn service_requires_expr_in_environment_template() {
    expect_env_err(
        r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE]
environment:
  name: E
  variables:
    X: "1"
"#,
        SERVICE_EXTS,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "extensions:\n\tSERVICE requires EXPR; both must be listed in the template's `extensions` (RFC 0009).",
        ],
    );
}

// ════════════════════════════════════════════════════════════════════
// List constraints — §1.1 item 8, §3 item 6
// ════════════════════════════════════════════════════════════════════

fn n_services(n: usize, prefix: &str) -> String {
    (0..n)
        .map(|i| {
            format!(
                "  - name: {prefix}{i}\n    ports: [{{name: main}}]\n    script: {{actions: {{onRun: {{command: run}}}}}}\n"
            )
        })
        .collect()
}

#[test]
fn job_services_must_not_be_empty() {
    expect_job_err(
        &job_with_services("  []"),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services:\n\tmust not be empty.",
        ],
    );
}

#[test]
fn job_services_at_most_ten() {
    expect_job_ok(&job_with_services(&n_services(10, "S")), SERVICE_EXTS);
    expect_job_err(
        &job_with_services(&n_services(11, "S")),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services:\n\tmust not contain more than 10 elements.",
        ],
    );
}

#[test]
fn duplicate_job_service_names() {
    expect_job_err(
        &job_with_services(
            r#"  - name: Cache
    ports: [{name: main}]
    script: {actions: {onRun: {command: run}}}
  - name: Cache
    ports: [{name: main}]
    script: {actions: {onRun: {command: run}}}
"#,
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[1]:\n\tduplicate service name: 'Cache'",
        ],
    );
}

// ════════════════════════════════════════════════════════════════════
// <ServiceName> — §9.1
// ════════════════════════════════════════════════════════════════════

#[test]
fn service_name_must_be_identifier() {
    expect_job_err(
        &job_with_services(
            r#"  - name: "my-cache"
    ports: [{name: main}]
    script: {actions: {onRun: {command: run}}}
"#,
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> name:\n\t'my-cache' is not a valid identifier.",
        ],
    );
    expect_job_err(
        &job_with_services(
            r#"  - name: "1cache"
    ports: [{name: main}]
    script: {actions: {onRun: {command: run}}}
"#,
        ),
        SERVICE_EXTS,
        &["services[0] -> name:\n\t'1cache' is not a valid identifier."],
    );
    expect_job_err(
        &job_with_services(
            r#"  - name: ""
    ports: [{name: main}]
    script: {actions: {onRun: {command: run}}}
"#,
        ),
        SERVICE_EXTS,
        &["services[0] -> name:\n\t'' is not a valid identifier."],
    );
}

#[test]
fn service_name_must_not_be_file() {
    expect_job_err(
        &job_with_services(
            r#"  - name: File
    ports: [{name: main}]
    script: {actions: {onRun: {command: run}}}
"#,
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> name:\n\tmust not be 'File'; it is reserved for Service.File.* references.",
        ],
    );
}

#[test]
fn service_name_length_follows_identifier_limit() {
    let long = "S".repeat(65);
    let template = job_with_services(&format!(
        "  - name: {long}\n    ports: [{{name: main}}]\n    script: {{actions: {{onRun: {{command: run}}}}}}\n"
    ))
    .replace("extensions: [SERVICE, EXPR]", "extensions: [SERVICE, EXPR, FEATURE_BUNDLE_1]");
    // 65 characters is fine with FEATURE_BUNDLE_1 (limit 512)…
    expect_job_ok(&template, SERVICE_EXTS);
    // …and over the 64-character base limit without it.
    expect_job_err(
        &template.replace(
            "extensions: [SERVICE, EXPR, FEATURE_BUNDLE_1]",
            "extensions: [SERVICE, EXPR]",
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> name:\n\texceeds 64 characters.",
        ],
    );
}

// ════════════════════════════════════════════════════════════════════
// <ServicePort> — §9 item 5, §9.2
// ════════════════════════════════════════════════════════════════════

fn service_with_ports(ports: &str) -> String {
    job_with_service_body(&format!(
        "    ports:\n{ports}    script:\n      actions:\n        onRun:\n          command: run\n"
    ))
}

#[test]
fn ports_must_not_be_empty() {
    expect_job_err(
        &job_with_service_body("    ports: []\n    script: {actions: {onRun: {command: run}}}\n"),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> ports:\n\tmust not be empty.",
        ],
    );
}

#[test]
fn ports_required() {
    let err = decode_job_template(
        yaml_val(&job_with_service_body(
            "    script: {actions: {onRun: {command: run}}}\n",
        )),
        Some(SERVICE_EXTS),
        &CallerLimits::default(),
    )
    .expect_err("ports is required");
    let msg = err.to_string();
    assert!(msg.contains("missing field `ports`"), "got: {msg}");
}

#[test]
fn ports_at_most_ten() {
    let ten: String = (0..10).map(|i| format!("      - name: p{i}\n")).collect();
    expect_job_ok(&service_with_ports(&ten), SERVICE_EXTS);
    let eleven: String = (0..11).map(|i| format!("      - name: p{i}\n")).collect();
    expect_job_err(
        &service_with_ports(&eleven),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> ports:\n\tmust not contain more than 10 elements.",
        ],
    );
}

#[test]
fn duplicate_port_names() {
    expect_job_err(
        &service_with_ports("      - name: main\n      - name: main\n"),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> ports[1]:\n\tduplicate port name 'main'.",
        ],
    );
}

#[test]
fn port_name_must_be_identifier_and_not_file() {
    expect_job_err(
        &service_with_ports("      - name: \"main port\"\n"),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> ports[0] -> name:\n\t'main port' is not a valid identifier.",
        ],
    );
    expect_job_err(
        &service_with_ports("      - name: File\n"),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> ports[0] -> name:\n\tmust not be 'File'; it is reserved for Service.File.* references.",
        ],
    );
}

#[test]
fn port_number_range() {
    expect_job_err(
        &service_with_ports("      - name: main\n        port: 0\n"),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> ports[0] -> port:\n\tmust be between 1 and 65535.",
        ],
    );
    expect_job_err(
        &service_with_ports("      - name: main\n        port: 65536\n"),
        SERVICE_EXTS,
        &["services[0] -> ports[0] -> port:\n\tmust be between 1 and 65535."],
    );
    expect_job_err(
        &service_with_ports("      - name: main\n        port: -1\n"),
        SERVICE_EXTS,
        &["services[0] -> ports[0] -> port:\n\tmust be between 1 and 65535."],
    );
    expect_job_err(
        &service_with_ports("      - name: main\n        port: \"http\"\n"),
        SERVICE_EXTS,
        &["services[0] -> ports[0] -> port:\n\tmust be an integer."],
    );
    expect_job_err(
        &service_with_ports("      - name: main\n        port: 80.5\n"),
        SERVICE_EXTS,
        &["services[0] -> ports[0] -> port:\n\tmust be an integer."],
    );
}

// ════════════════════════════════════════════════════════════════════
// <ServiceHealthCheck> — §9.3, §9.7 item 4
// ════════════════════════════════════════════════════════════════════

/// A service with ports `main` and `metrics`, the given `healthCheck`
/// body (indented six spaces), and the given extra actions (indented eight
/// spaces) beside `onRun`.
fn service_with_health(health: &str, extra_actions: &str) -> String {
    job_with_service_body(&format!(
        r#"    ports:
      - name: main
      - name: metrics
    healthCheck:
{health}    script:
      actions:
        onRun:
          command: run
{extra_actions}"#
    ))
}

const ON_HEALTH_CHECK: &str = "        onHealthCheck:\n          command: probe\n";

#[test]
fn tcp_connect_ports_must_be_declared() {
    expect_job_err(
        &service_with_health("      type: TCP_CONNECT\n      ports: [main, admin]\n", ""),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> healthCheck -> ports[1]:\n\treferences undeclared port 'admin'.",
        ],
    );
}

#[test]
fn tcp_connect_ports_if_provided_not_empty() {
    expect_job_err(
        &service_with_health("      type: TCP_CONNECT\n      ports: []\n", ""),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> healthCheck -> ports:\n\tif provided, must not be empty.",
        ],
    );
}

#[test]
fn tcp_connect_declared_ports_accepted() {
    expect_job_ok(
        &service_with_health(
            "      type: TCP_CONNECT\n      ports: [metrics, main]\n      readinessTimeoutSeconds: 1\n",
            "",
        ),
        SERVICE_EXTS,
    );
}

/// §9.3 items 3–6: every numeric field of every form is a `<posinteger>`.
/// `readinessIntervalSeconds` is not a field of the `STDOUT` form (tested
/// separately); `failureThreshold` on `STDOUT` needs `healthIntervalSeconds`
/// beside it to reach the range check.
#[test]
fn health_numeric_fields_must_be_positive() {
    let cases: &[(&str, &str, &str, &str)] = &[
        ("TCP_CONNECT", "", "readinessIntervalSeconds", ""),
        ("TCP_CONNECT", "", "readinessTimeoutSeconds", ""),
        ("TCP_CONNECT", "", "healthIntervalSeconds", ""),
        ("TCP_CONNECT", "", "failureThreshold", ""),
        ("COMMAND", ON_HEALTH_CHECK, "readinessIntervalSeconds", ""),
        ("COMMAND", ON_HEALTH_CHECK, "readinessTimeoutSeconds", ""),
        ("COMMAND", ON_HEALTH_CHECK, "healthIntervalSeconds", ""),
        ("COMMAND", ON_HEALTH_CHECK, "failureThreshold", ""),
        ("STDOUT", "", "readinessTimeoutSeconds", ""),
        ("STDOUT", "", "healthIntervalSeconds", ""),
        (
            "STDOUT",
            "",
            "failureThreshold",
            "      healthIntervalSeconds: 5\n",
        ),
    ];
    for (ty, extra, field, companion) in cases {
        for bad in ["0", "-5"] {
            expect_job_err(
                &service_with_health(
                    &format!("      type: {ty}\n{companion}      {field}: {bad}\n"),
                    extra,
                ),
                SERVICE_EXTS,
                &[
                    "1 validation error for JobTemplate\n",
                    &format!("services[0] -> healthCheck -> {field}:\n\tmust be > 0."),
                ],
            );
        }
        expect_job_err(
            &service_with_health(
                &format!("      type: {ty}\n{companion}      {field}: \"soon\"\n"),
                extra,
            ),
            SERVICE_EXTS,
            &[&format!(
                "services[0] -> healthCheck -> {field}:\n\tmust be an integer."
            )],
        );
        // A format string is resolved at job creation, not checked here.
        expect_job_ok(
            &service_with_health(
                &format!("      type: {ty}\n{companion}      {field}: \"{{{{ Param.T }}}}\"\n"),
                extra,
            ),
            SERVICE_EXTS,
        );
        // A positive literal is accepted.
        expect_job_ok(
            &service_with_health(
                &format!("      type: {ty}\n{companion}      {field}: 7\n"),
                extra,
            ),
            SERVICE_EXTS,
        );
    }
}

/// §9.3 item 6 / §9.7 item 4: on a STDOUT check `failureThreshold` is
/// permitted only together with `healthIntervalSeconds`.
#[test]
fn stdout_failure_threshold_requires_health_interval() {
    expect_job_err(
        &service_with_health("      type: STDOUT\n      failureThreshold: 2\n", ""),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> healthCheck -> failureThreshold:\n\ta STDOUT health check gives \
             failureThreshold only together with healthIntervalSeconds; without a heartbeat \
             interval there is no probe for it to count.",
        ],
    );
    // A format string counts as given.
    expect_job_err(
        &service_with_health(
            "      type: STDOUT\n      failureThreshold: \"{{ Param.N }}\"\n",
            "",
        ),
        SERVICE_EXTS,
        &["services[0] -> healthCheck -> failureThreshold:\n\ta STDOUT health check gives"],
    );
    expect_job_ok(
        &service_with_health(
            "      type: STDOUT\n      healthIntervalSeconds: 15\n      failureThreshold: 2\n",
            "",
        ),
        SERVICE_EXTS,
    );
    expect_job_ok(
        &service_with_health("      type: STDOUT\n      healthIntervalSeconds: 15\n", ""),
        SERVICE_EXTS,
    );
}

/// §9.3 item 3: `readinessIntervalSeconds` does not apply to a STDOUT
/// check; it is not a property of that form and is rejected as unknown.
#[test]
fn stdout_readiness_interval_rejected() {
    let err = decode_job_template(
        yaml_val(&service_with_health(
            "      type: STDOUT\n      readinessIntervalSeconds: 1\n",
            "",
        )),
        Some(SERVICE_EXTS),
        &CallerLimits::default(),
    )
    .expect_err("readinessIntervalSeconds is not a STDOUT field");
    assert!(
        err.to_string()
            .contains("unknown field `readinessIntervalSeconds`"),
        "got: {err}"
    );
    // It is accepted on both other forms (§9.3 item 3: default 1 for
    // TCP_CONNECT, 5 for COMMAND).
    expect_job_ok(
        &service_with_health(
            "      type: TCP_CONNECT\n      readinessIntervalSeconds: 5\n",
            "",
        ),
        SERVICE_EXTS,
    );
    expect_job_ok(
        &service_with_health(
            "      type: COMMAND\n      readinessIntervalSeconds: 5\n",
            ON_HEALTH_CHECK,
        ),
        SERVICE_EXTS,
    );
}

#[test]
fn command_requires_on_health_check() {
    expect_job_err(
        &service_with_health("      type: COMMAND\n", ""),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> script -> actions:\n\tonHealthCheck must be defined when healthCheck.type is COMMAND.",
        ],
    );
}

#[test]
fn on_health_check_forbidden_unless_command() {
    // Explicit TCP_CONNECT.
    expect_job_err(
        &service_with_health("      type: TCP_CONNECT\n", ON_HEALTH_CHECK),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> script -> actions -> onHealthCheck:\n\tonHealthCheck must not be defined when healthCheck.type is TCP_CONNECT.",
        ],
    );
    // STDOUT.
    expect_job_err(
        &service_with_health("      type: STDOUT\n", ON_HEALTH_CHECK),
        SERVICE_EXTS,
        &["services[0] -> script -> actions -> onHealthCheck:\n\tonHealthCheck must not be defined when healthCheck.type is STDOUT."],
    );
    // The default (no healthCheck) is TCP_CONNECT.
    expect_job_err(
        &job_with_service_body(&format!(
            "    ports: [{{name: main}}]\n    script:\n      actions:\n        onRun:\n          command: run\n{ON_HEALTH_CHECK}"
        )),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> script -> actions -> onHealthCheck:\n\tonHealthCheck must not be defined when healthCheck.type is TCP_CONNECT.",
        ],
    );
}

#[test]
fn health_check_unknown_type_rejected() {
    let err = decode_job_template(
        yaml_val(&service_with_health("      type: HTTP\n", "")),
        Some(SERVICE_EXTS),
        &CallerLimits::default(),
    )
    .expect_err("unknown health check type");
    let msg = err.to_string();
    assert!(
        msg.contains("unknown variant `HTTP`, expected one of `TCP_CONNECT`, `COMMAND`, `STDOUT`"),
        "got: {msg}"
    );
}

#[test]
fn health_check_rejects_fields_of_other_types() {
    let err = decode_job_template(
        yaml_val(&service_with_health(
            "      type: STDOUT\n      ports: [main]\n",
            "",
        )),
        Some(SERVICE_EXTS),
        &CallerLimits::default(),
    )
    .expect_err("ports is TCP_CONNECT-only");
    assert!(
        err.to_string().contains("unknown field `ports`"),
        "got: {err}"
    );
    let err = decode_job_template(
        yaml_val(&service_with_health(
            "      type: COMMAND\n      ports: [main]\n",
            ON_HEALTH_CHECK,
        )),
        Some(SERVICE_EXTS),
        &CallerLimits::default(),
    )
    .expect_err("ports is TCP_CONNECT-only");
    assert!(
        err.to_string().contains("unknown field `ports`"),
        "got: {err}"
    );
}

/// The pre-RFC-revision spellings — `readinessCheck` on `<Service>`,
/// `onReadinessCheck` on `<ServiceActions>`, and `timeoutSeconds` /
/// `intervalSeconds` on the check — are unknown properties and are
/// rejected, like the conformance fixtures
/// `9.3--health-old-readiness-check-key` and
/// `9.3--health-old-timeout-seconds-key` require.
#[test]
fn old_readiness_spellings_rejected_as_unknown() {
    let cases = [
        (
            job_with_service_body(
                "    ports: [{name: main}]\n    readinessCheck:\n      type: TCP_CONNECT\n    \
                 script:\n      actions:\n        onRun:\n          command: run\n",
            ),
            "unknown field `readinessCheck`",
        ),
        (
            service_with_health("      type: TCP_CONNECT\n      timeoutSeconds: 60\n", ""),
            "unknown field `timeoutSeconds`",
        ),
        (
            service_with_health(
                "      type: COMMAND\n      intervalSeconds: 5\n",
                ON_HEALTH_CHECK,
            ),
            "unknown field `intervalSeconds`",
        ),
        (
            service_with_health(
                "      type: COMMAND\n",
                "        onReadinessCheck:\n          command: probe\n",
            ),
            "unknown field `onReadinessCheck`",
        ),
    ];
    for (template, expected) in cases {
        let err = decode_job_template(
            yaml_val(&template),
            Some(SERVICE_EXTS),
            &CallerLimits::default(),
        )
        .expect_err(expected);
        assert!(err.to_string().contains(expected), "got: {err}");
    }
}

// ════════════════════════════════════════════════════════════════════
// <ServicePort>.protocol — §9.2 item 3, §9 items 6.4 and 7, §9.7 items 4 and 8
// ════════════════════════════════════════════════════════════════════

/// A service with the given `ports` list body (indented six spaces) and
/// optional `healthCheck` body (indented six spaces; empty = omitted).
fn service_with_ports_and_health(ports: &str, health: &str) -> String {
    let health = if health.is_empty() {
        String::new()
    } else {
        format!("    healthCheck:\n{health}")
    };
    job_with_service_body(&format!(
        "    ports:\n{ports}{health}    script:\n      actions:\n        onRun:\n          command: run\n"
    ))
}

#[test]
fn port_protocol_defaults_to_tcp_and_accepts_both_literals() {
    let jt = expect_job_ok(
        &service_with_ports_and_health(
            "      - name: ingest\n        protocol: UDP\n      - name: api\n      - name: admin\n        protocol: TCP\n",
            "",
        ),
        SERVICE_EXTS,
    );
    let svc = &jt.services.as_ref().unwrap()[0];
    let protocols: Vec<ServicePortProtocol> = svc.ports.iter().map(|p| p.protocol).collect();
    assert_eq!(
        protocols,
        vec![
            ServicePortProtocol::Udp,
            ServicePortProtocol::Tcp,
            ServicePortProtocol::Tcp
        ]
    );
    assert_eq!(
        svc.tcp_port_names().collect::<Vec<_>>(),
        vec!["api", "admin"]
    );
}

#[test]
fn port_protocol_is_a_case_sensitive_enum_not_a_format_string() {
    for bad in ["udp", "SCTP", "{{ Param.P }}"] {
        let err = decode_job_template(
            yaml_val(&service_with_ports_and_health(
                &format!("      - name: main\n        protocol: \"{bad}\"\n"),
                "",
            )),
            Some(SERVICE_EXTS),
            &CallerLimits::default(),
        )
        .expect_err("invalid protocol");
        assert!(
            err.to_string()
                .contains(&format!("unknown variant `{bad}`")),
            "got: {err}"
        );
    }
}

#[test]
fn tcp_connect_may_not_name_a_udp_port() {
    expect_job_err(
        &service_with_ports_and_health(
            "      - name: ingest\n        protocol: UDP\n      - name: api\n",
            "      type: TCP_CONNECT\n      ports: [api, ingest]\n",
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> healthCheck -> ports[1]:\n\tport 'ingest' has protocol UDP and cannot be probed by a TCP_CONNECT health check; only TCP ports may be named.",
        ],
    );
    // Naming the TCP port alone is fine.
    expect_job_ok(
        &service_with_ports_and_health(
            "      - name: ingest\n        protocol: UDP\n      - name: api\n",
            "      type: TCP_CONNECT\n      ports: [api]\n",
        ),
        SERVICE_EXTS,
    );
}

#[test]
fn all_udp_service_requires_stdout_or_command_health_check() {
    // Omitted: the default TCP_CONNECT has nothing to probe.
    expect_job_err(
        &service_with_ports_and_health("      - name: ingest\n        protocol: UDP\n", ""),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> healthCheck:\n\tthe default TCP_CONNECT health check has no TCP port to probe: none of the Service's ports has protocol TCP, so a healthCheck of type STDOUT or COMMAND is required.",
        ],
    );
    // Explicit TCP_CONNECT without `ports`: its default list is empty.
    expect_job_err(
        &service_with_ports_and_health(
            "      - name: ingest\n        protocol: UDP\n      - name: discovery\n        protocol: UDP\n",
            "      type: TCP_CONNECT\n      readinessTimeoutSeconds: 30\n",
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> healthCheck:\n\ta TCP_CONNECT health check has no TCP port to probe: none of the Service's ports has protocol TCP, so a healthCheck of type STDOUT or COMMAND is required.",
        ],
    );
    // STDOUT and COMMAND are accepted.
    expect_job_ok(
        &service_with_ports_and_health(
            "      - name: ingest\n        protocol: UDP\n",
            "      type: STDOUT\n      readinessTimeoutSeconds: 60\n",
        ),
        SERVICE_EXTS,
    );
    expect_job_ok(
        &job_with_service_body(
            "    ports:\n      - name: ingest\n        protocol: UDP\n    healthCheck:\n      type: COMMAND\n    script:\n      actions:\n        onRun:\n          command: run\n        onHealthCheck:\n          command: probe\n",
        ),
        SERVICE_EXTS,
    );
    // A mixed Service may omit the check: the default probes the TCP port.
    expect_job_ok(
        &service_with_ports_and_health(
            "      - name: ingest\n        protocol: UDP\n      - name: api\n",
            "",
        ),
        SERVICE_EXTS,
    );
}

#[test]
fn same_port_number_twice_in_one_protocol_is_rejected() {
    // TCP given and defaulted.
    expect_job_err(
        &service_with_ports_and_health(
            "      - name: main\n        port: 6379\n      - name: admin\n        port: 6379\n        protocol: TCP\n",
            "",
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> ports[1] -> port:\n\tTCP port 6379 is also used by port 'main'; two ports with the same protocol must not have the same port number.",
        ],
    );
    // UDP twice.
    expect_job_err(
        &service_with_ports_and_health(
            "      - name: ingest\n        port: 8125\n        protocol: UDP\n      - name: trace\n        port: 8125\n        protocol: UDP\n",
            "      type: STDOUT\n",
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> ports[1] -> port:\n\tUDP port 8125 is also used by port 'ingest'; two ports with the same protocol must not have the same port number.",
        ],
    );
}

#[test]
fn same_port_number_across_protocols_is_allowed() {
    let jt = expect_job_ok(
        &service_with_ports_and_health(
            "      - name: tcp\n        port: 5353\n      - name: udp\n        port: 5353\n        protocol: UDP\n",
            "",
        ),
        SERVICE_EXTS,
    );
    let svc = &jt.services.as_ref().unwrap()[0];
    assert_eq!(svc.ports[0].port.as_ref().unwrap().raw(), "5353");
    assert_eq!(svc.ports[1].port.as_ref().unwrap().raw(), "5353");
}

#[test]
fn duplicate_port_number_check_defers_format_strings_to_job_creation() {
    // A format-string number is not compared at template validation.
    expect_job_ok(
        &service_with_ports_and_health(
            "      - name: main\n        port: 6379\n      - name: admin\n        port: \"{{ Param.P }}\"\n",
            "",
        ),
        SERVICE_EXTS,
    );
}

// ════════════════════════════════════════════════════════════════════
// <ServiceRestartPolicy> — §9.4
// ════════════════════════════════════════════════════════════════════

fn service_with_restart_policy(policy: &str) -> String {
    job_with_service_body(&format!(
        "    ports: [{{name: main}}]\n    restartPolicy:\n{policy}    script: {{actions: {{onRun: {{command: run}}}}}}\n"
    ))
}

#[test]
fn max_attempts_must_be_non_negative() {
    expect_job_err(
        &service_with_restart_policy("      maxAttempts: -1\n"),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> restartPolicy -> maxAttempts:\n\tmust be >= 0.",
        ],
    );
    expect_job_err(
        &service_with_restart_policy("      maxAttempts: many\n"),
        SERVICE_EXTS,
        &["services[0] -> restartPolicy -> maxAttempts:\n\tmust be an integer."],
    );
    expect_job_ok(
        &service_with_restart_policy("      maxAttempts: 0\n"),
        SERVICE_EXTS,
    );
    expect_job_ok(
        &service_with_restart_policy("      maxAttempts: \"{{ Param.Attempts2 }}\"\n"),
        SERVICE_EXTS,
    );
}

#[test]
fn completed_tasks_must_be_keep_or_rerun() {
    let err = decode_job_template(
        yaml_val(&service_with_restart_policy(
            "      completedTasks: RETRY\n",
        )),
        Some(SERVICE_EXTS),
        &CallerLimits::default(),
    )
    .expect_err("invalid completedTasks");
    assert!(
        err.to_string()
            .contains("unknown variant `RETRY`, expected `KEEP` or `RERUN`"),
        "got: {err}"
    );
    let jt = expect_job_ok(
        &service_with_restart_policy("      completedTasks: KEEP\n"),
        SERVICE_EXTS,
    );
    assert_eq!(
        jt.services.as_ref().unwrap()[0]
            .restart_policy()
            .completed_tasks(),
        CompletedTasksPolicy::Keep
    );
    let jt = expect_job_ok(&service_with_restart_policy("      {}\n"), SERVICE_EXTS);
    assert_eq!(
        jt.services.as_ref().unwrap()[0]
            .restart_policy()
            .completed_tasks(),
        CompletedTasksPolicy::Rerun
    );
}

// ════════════════════════════════════════════════════════════════════
// Reused validators — variables, hostRequirements, actions, embedded files
// ════════════════════════════════════════════════════════════════════

#[test]
fn service_variables_follow_environment_variable_rules() {
    expect_job_err(
        &job_with_service_body(
            "    ports: [{name: main}]\n    variables: {}\n    script: {actions: {onRun: {command: run}}}\n",
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> variables:\n\tif provided, must not be empty.",
        ],
    );
    expect_job_err(
        &job_with_service_body(
            "    ports: [{name: main}]\n    variables:\n      1X: a\n    script: {actions: {onRun: {command: run}}}\n",
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> variables -> 1X:\n\tvariable name '1X' cannot start with a digit.",
        ],
    );
    expect_job_err(
        &job_with_service_body(
            "    ports: [{name: main}]\n    variables:\n      X: \"a\\u0000b\"\n    script: {actions: {onRun: {command: run}}}\n",
        ),
        SERVICE_EXTS,
        &[
            "services[0] -> variables -> X:\n\tvalue contains a NUL byte, which cannot be represented in a process environment.",
        ],
    );
}

#[test]
fn service_host_requirements_follow_step_rules() {
    expect_job_err(
        &job_with_service_body(
            "    ports: [{name: main}]\n    hostRequirements: {}\n    script: {actions: {onRun: {command: run}}}\n",
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> hostRequirements:\n\tmust have at least one of amounts or attributes.",
        ],
    );
    expect_job_err(
        &job_with_service_body(
            r#"    ports: [{name: main}]
    hostRequirements:
      amounts:
        - name: amount.worker.memory
          min: -1
    script: {actions: {onRun: {command: run}}}
"#,
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> hostRequirements -> amounts[0] -> min:\n\tmust be non-negative.",
        ],
    );
}

#[test]
fn attr_worker_preemptible_is_a_standard_attribute() {
    // RFC 0009 adds `attr.worker.preemptible` (values "true"/"false") to
    // the standard attribute table. It is not gated by SERVICE.
    expect_job_ok(
        r#"
specificationVersion: "jobtemplate-2023-09"
name: Test
steps:
  - name: S
    hostRequirements:
      attributes:
        - name: attr.worker.preemptible
          anyOf: ["false"]
    script: {actions: {onRun: {command: run}}}
"#,
        NO_SERVICE_EXTS,
    );
    expect_job_err(
        r#"
specificationVersion: "jobtemplate-2023-09"
name: Test
steps:
  - name: S
    hostRequirements:
      attributes:
        - name: attr.worker.preemptible
          anyOf: ["maybe"]
    script: {actions: {onRun: {command: run}}}
"#,
        NO_SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "steps[0] -> hostRequirements -> attributes[0] -> anyOf:\n\tvalue 'maybe' is not valid for attr.worker.preemptible.",
        ],
    );
    expect_job_err(
        &job_with_service_body(
            r#"    ports: [{name: main}]
    hostRequirements:
      attributes:
        - name: attr.worker.preemptible
          anyOf: ["no"]
    script: {actions: {onRun: {command: run}}}
"#,
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> hostRequirements -> attributes[0] -> anyOf:\n\tvalue 'no' is not valid for attr.worker.preemptible.",
        ],
    );
}

#[test]
fn service_actions_follow_action_rules() {
    expect_job_err(
        &job_with_service_body(
            "    ports: [{name: main}]\n    script: {actions: {onRun: {command: \"\"}}}\n",
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> script -> actions -> onRun -> command:\n\tmust not be empty.",
        ],
    );
    expect_job_err(
        &job_with_service_body(
            r#"    ports: [{name: main}]
    script:
      actions:
        onEnter:
          command: init
          args: []
        onRun:
          command: run
        onExit:
          command: cleanup
          timeout: 0
"#,
        ),
        SERVICE_EXTS,
        &[
            "2 validation errors for JobTemplate\n",
            "services[0] -> script -> actions -> onEnter -> args:\n\tif provided, must not be empty.",
            "services[0] -> script -> actions -> onExit:\n\ttimeout must be > 0.",
        ],
    );
}

#[test]
fn service_on_run_required() {
    let err = decode_job_template(
        yaml_val(&job_with_service_body(
            "    ports: [{name: main}]\n    script: {actions: {onEnter: {command: init}}}\n",
        )),
        Some(SERVICE_EXTS),
        &CallerLimits::default(),
    )
    .expect_err("onRun is required");
    assert!(
        err.to_string().contains("missing field `onRun`"),
        "got: {err}"
    );
}

#[test]
fn service_rejects_unknown_fields() {
    let err = decode_job_template(
        yaml_val(&job_with_service_body(
            "    ports: [{name: main}]\n    replicas: 2\n    script: {actions: {onRun: {command: run}}}\n",
        )),
        Some(SERVICE_EXTS),
        &CallerLimits::default(),
    )
    .expect_err("unknown field");
    assert!(
        err.to_string().contains("unknown field `replicas`"),
        "got: {err}"
    );
    let err = decode_job_template(
        yaml_val(&job_with_service_body(
            "    ports: [{name: main, transport: UDP}]\n    script: {actions: {onRun: {command: run}}}\n",
        )),
        Some(SERVICE_EXTS),
        &CallerLimits::default(),
    )
    .expect_err("unknown port field");
    assert!(
        err.to_string().contains("unknown field `transport`"),
        "got: {err}"
    );
    let err = decode_job_template(
        yaml_val(&job_with_service_body(
            "    ports: [{name: main}]\n    script: {actions: {onRun: {command: run}, onWrapTaskRun: {command: w}}}\n",
        )),
        Some(SERVICE_EXTS),
        &CallerLimits::default(),
    )
    .expect_err("unknown action");
    assert!(
        err.to_string().contains("unknown field `onWrapTaskRun`"),
        "got: {err}"
    );
}

/// `serviceEnvironments` is not a `<Service>` property: the RFC rejected a
/// Service-scoped Environment list (Rejected Ideas, "`serviceEnvironments`,
/// a Service-scoped Environment list") in favor of provisioning in
/// `onEnter`. It is an unknown field like any other, in a Job Template and
/// in an Environment Template's `services`.
#[test]
fn service_environments_is_not_a_property() {
    const SERVICE_ENVIRONMENTS: &str = r#"    serviceEnvironments:
      - name: Provision
        script:
          actions:
            onEnter: { command: init }
    ports: [{name: main}]
    script: {actions: {onRun: {command: run}}}
"#;
    let err = decode_job_template(
        yaml_val(&job_with_service_body(SERVICE_ENVIRONMENTS)),
        Some(SERVICE_EXTS),
        &CallerLimits::default(),
    )
    .expect_err("serviceEnvironments is not a property");
    assert!(
        err.to_string()
            .contains("unknown field `serviceEnvironments`"),
        "got: {err}"
    );

    let err = decode_environment_template(
        yaml_val(&format!(
            "specificationVersion: environment-2023-09\nextensions: [SERVICE, EXPR]\nservices:\n  - name: Cache\n{SERVICE_ENVIRONMENTS}"
        )),
        Some(SERVICE_EXTS),
        &CallerLimits::default(),
    )
    .expect_err("serviceEnvironments is not a property");
    assert!(
        err.to_string()
            .contains("unknown field `serviceEnvironments`"),
        "got: {err}"
    );
}

#[test]
fn service_embedded_files_follow_embedded_file_rules() {
    expect_job_err(
        &job_with_service_body(
            r#"    ports: [{name: main}]
    script:
      actions:
        onRun:
          command: run
      embeddedFiles: []
"#,
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> script -> embeddedFiles:\n\tmust not be empty.",
        ],
    );
    expect_job_err(
        &job_with_service_body(
            r#"    ports: [{name: main}]
    script:
      actions:
        onRun:
          command: run
      embeddedFiles:
        - name: Run
          type: TEXT
          data: "a"
        - name: Run
          type: TEXT
"#,
        ),
        SERVICE_EXTS,
        &[
            "2 validation errors for JobTemplate\n",
            "services[0] -> script -> embeddedFiles[1]:\n\tduplicate embedded file name 'Run'.",
            "services[0] -> script -> embeddedFiles[1]:\n\tembedded file 'Run' is missing 'data' field.",
        ],
    );
    expect_job_err(
        &job_with_service_body(
            r#"    ports: [{name: main}]
    script:
      actions:
        onRun:
          command: run
      embeddedFiles:
        - name: Run
          type: TEXT
          filename: "../escape"
          data: "a"
"#,
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0] -> script -> embeddedFiles[0] -> filename:\n\tmust not contain path separators.",
        ],
    );
}

#[test]
fn errors_accumulate_across_services() {
    expect_job_err(
        &job_with_services(
            r#"  - name: A
    ports: [{name: main}, {name: main}]
    script: {actions: {onRun: {command: run}}}
  - name: B
    ports: [{name: main}]
    healthCheck:
      type: TCP_CONNECT
      ports: [nope]
    script: {actions: {onRun: {command: run}}}
"#,
        ),
        SERVICE_EXTS,
        &[
            "2 validation errors for JobTemplate\n",
            "services[0] -> ports[1]:\n\tduplicate port name 'main'.",
            "services[1] -> healthCheck -> ports[0]:\n\treferences undeclared port 'nope'.",
        ],
    );
}

// ════════════════════════════════════════════════════════════════════
// RFC 0009 "Basic Examples" — verbatim fixtures in tests/fixtures/rfc0009/
// ════════════════════════════════════════════════════════════════════

/// A fixture decodes as a job template, as written in the RFC.
fn expect_fixture_job_ok(fixture: &str) -> openjd_model::template::JobTemplate {
    expect_job_ok(fixture, SERVICE_EXTS)
}

/// The Step's embedded file references `Service.Cache.main.connectAddress`
/// and `.port`, and the Service's own `onRun` references its `bindAddress`:
/// every `Service.*` reference in the RFC example resolves.
#[test]
fn rfc_example_valkey_shared_store_verbatim() {
    let jt = expect_fixture_job_ok(RFC_VALKEY);
    assert_eq!(jt.services.as_ref().unwrap()[0].name, "Cache");
}

/// `RenderTiles`'s `onRun` args use
/// `join_host_port(Service.Coordinator.api.connectAddress, ...)`, and the
/// Service's `onRun` uses its own `bindAddress`. Only `RenderTiles`
/// references the Service, so its scope is that one Step (§9.1).
#[test]
fn rfc_example_step_coordinator_verbatim() {
    let jt = expect_fixture_job_ok(RFC_COORDINATOR);
    assert_eq!(jt.services.as_ref().unwrap()[0].name, "Coordinator");
    let scopes = compute_service_scopes(&jt).unwrap();
    assert_eq!(
        scopes.get("Coordinator").unwrap().scope,
        ServiceScope::steps(["RenderTiles"])
    );
}

/// `requiresServices` declares `Cache` with port `main`; the Step uses
/// `Service.Cache.main.connectAddress` and `.port` (§1.1, §9.9).
#[test]
fn rfc_example_required_queue_cache_verbatim() {
    let jt = expect_fixture_job_ok(RFC_REQUIRED_QUEUE_CACHE);
    assert!(jt.services.is_none());
    let reqs = jt.requires_services();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].name, "Cache");
    assert_eq!(reqs[0].port_names().collect::<Vec<_>>(), vec!["main"]);
    assert_eq!(reqs[0].ports[0].protocol, ServicePortProtocol::Tcp);
}

/// A UDP `ingest` port and a TCP `api` port; both Steps reference the
/// Service, so its scope is both Steps.
#[test]
fn rfc_example_metrics_sink_verbatim() {
    let jt = expect_fixture_job_ok(RFC_METRICS_SINK);
    let svc = &jt.services.as_ref().unwrap()[0];
    assert_eq!(svc.ports[0].protocol, ServicePortProtocol::Udp);
    assert_eq!(svc.ports[1].protocol, ServicePortProtocol::Tcp);
    let scopes = compute_service_scopes(&jt).unwrap();
    assert_eq!(
        scopes.get("Metrics").unwrap().scope,
        ServiceScope::steps(["RenderFrames", "Report"])
    );
}

/// The Environment Template's `environment` (`runScope: [TASK]`) references
/// the document's own Service in its `variables`, and the Service's `onRun`
/// references its `bindAddress` (§1.2.2).
#[test]
fn rfc_example_queue_cache_environment_verbatim() {
    let et = decode_environment_template(
        yaml_val(RFC_QUEUE_CACHE_ENV),
        Some(SERVICE_EXTS),
        &CallerLimits::default(),
    )
    .expect("expected successful decode");
    assert_eq!(et.services()[0].name, "Cache");
    assert_eq!(et.environment.as_ref().unwrap().name, "CacheClient");
}

#[test]
fn rfc_example_queue_cache_consumer_verbatim() {
    // The consumer job template does not use SERVICE at all and decodes
    // as-is, with or without the extension available.
    let jt = expect_fixture_job_ok(RFC_QUEUE_CACHE_CONSUMER);
    assert!(jt.services.is_none());
    assert!(!jt.profile().has_extension(ModelExtension::Service));
    expect_job_ok(RFC_QUEUE_CACHE_CONSUMER, NO_SERVICE_EXTS);
}

/// The RFC job examples with the Step scripts' `Service.*` references
/// replaced by literals: the Service declarations validate on their own,
/// independent of the scope rules.
#[test]
fn rfc_example_valkey_shared_store_services_validate() {
    let template = RFC_VALKEY
        .replace("'{{ Service.Cache.main.connectAddress }}'", "localhost")
        .replace(
            "--valkey-port {{ Service.Cache.main.port }}",
            "--valkey-port 6379",
        );
    // The Service's own onRun args keep their Service.* references; only
    // the Step script's were replaced.
    assert!(template.contains("{{ Service.Cache.main.bindAddress }}"));
    let jt = expect_fixture_job_ok(&template);
    let cache = &jt.services.as_ref().unwrap()[0];
    assert_eq!(cache.name, "Cache");
    assert_eq!(cache.port_names().collect::<Vec<_>>(), vec!["main"]);
    assert_eq!(cache.health_check().type_name(), "TCP_CONNECT");
    assert_eq!(
        cache
            .health_check()
            .readiness_timeout_seconds()
            .unwrap()
            .raw(),
        "60"
    );
    let policy = cache.restart_policy();
    assert_eq!(policy.max_attempts.as_ref().unwrap().raw(), "3");
    assert_eq!(policy.completed_tasks(), CompletedTasksPolicy::Keep);
    assert_eq!(
        cache.script.actions.on_run.args.as_ref().unwrap()[1].raw(),
        "{{ Service.Cache.main.port }}"
    );
}

#[test]
fn rfc_example_step_coordinator_services_validate() {
    let template = RFC_COORDINATOR.replace(
        "\"http://{{ join_host_port(Service.Coordinator.api.connectAddress, Service.Coordinator.api.port) }}\"",
        "\"http://localhost:8080\"",
    );
    let jt = expect_fixture_job_ok(&template);
    let coordinator = &jt.services.as_ref().unwrap()[0];
    assert_eq!(coordinator.name, "Coordinator");
    assert_eq!(
        coordinator.port_names().collect::<Vec<_>>(),
        vec!["api", "metrics"]
    );
    assert_eq!(coordinator.health_check().type_name(), "STDOUT");
    assert_eq!(
        coordinator
            .health_check()
            .readiness_timeout_seconds()
            .unwrap()
            .raw(),
        "120"
    );
    let policy = coordinator.restart_policy();
    assert_eq!(policy.max_attempts.as_ref().unwrap().raw(), "1");
    assert_eq!(policy.completed_tasks(), CompletedTasksPolicy::Rerun);
    let names: Vec<&str> = coordinator
        .script
        .actions
        .iter_named()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(names, vec!["onEnter", "onRun", "onExit"]);
}
