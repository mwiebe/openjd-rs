// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Integration tests for the RFC 0009 `SERVICE` extension's changes to
//! `<Environment>`, `<EnvironmentActions>`, and the Environment Template
//! root (Template Schemas §1.2, §4 items 3–4, §4.3 WRAP_ACTIONS constraint
//! 6, §9.7 items 3 and 6).
//!
//! These tests cover:
//!
//! 1. **`runScope`** on `<Environment>`: schema parse, the `runs_in` /
//!    `effective_run_scope` accessors, extension gating, and the §4 item 4
//!    constraints (non-empty, recognized names, no duplicates) in job
//!    environments, step environments, and environment templates.
//! 2. **`dependencies`** on `<Environment>` (§4 item 3, §9.9 item 14):
//!    extension gating, the Step Environment prohibition, and the list
//!    constraints (non-empty, `service` key naming a Service of the
//!    document or a requirement, no duplicates, not with an explicit
//!    `runScope` including `SERVICE`). What listing a Service *does* —
//!    visibility, scope, the `runScope` default — is covered in
//!    `test_service_scope.rs` and `test_service_scope_rules.rs`.
//! 3. **`onWrapService*` hooks** on `<EnvironmentActions>`: schema parse,
//!    gating on both `WRAP_ACTIONS` and `SERVICE`, and the default-timeout
//!    table.
//! 4. **Hooks follow `runScope`** (the RFC 0009 rule that replaces RFC
//!    0008's all-or-nothing rule when `SERVICE` is declared): every
//!    combination of default / `[TASK]` / `[SERVICE]` / `[TASK, SERVICE]`
//!    with missing and extra hooks, and the unchanged RFC 0008 rule when
//!    `SERVICE` is not declared.
//! 5. **Environment Template root**: `$schema`, `services` (gating, list
//!    constraints, reuse of the `<Service>` validator), optional
//!    `environment`, and the "at least one of" constraint.
//!
//! The Environments here (outside 2) list no Service and reference no
//! `Service.*` value, so an absent `runScope` means every kind of Session.
//! The dependency-dependent default (`[TASK]` for an Environment that lists
//! a Service, §4 item 4) is covered in `test_service_scope_rules.rs`; the
//! `Service.*` format-string scope and `WrappedService.*` in
//! `test_service_scope.rs`.
//!
//! Error assertions follow the repo convention of asserting on the full
//! Pydantic-style error path + message.

use openjd_model::template::{EnvironmentActions, RunScope};
use openjd_model::{decode_environment_template, decode_job_template, CallerLimits};

/// Everything the Service-aware wrap hooks need.
const ALL_EXTS: &[&str] = &["EXPR", "SERVICE", "WRAP_ACTIONS", "FEATURE_BUNDLE_1"];
/// `SERVICE` without `WRAP_ACTIONS`.
const SERVICE_EXTS: &[&str] = &["EXPR", "SERVICE"];
/// `WRAP_ACTIONS` without `SERVICE`.
const WRAP_EXTS: &[&str] = &["EXPR", "WRAP_ACTIONS"];
/// Neither.
const EXPR_ONLY: &[&str] = &["EXPR"];

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

fn expect_env_ok(
    template: &str,
    allowed_exts: &[&str],
) -> openjd_model::template::EnvironmentTemplate {
    decode_environment_template(
        yaml_val(template),
        Some(allowed_exts),
        &CallerLimits::default(),
    )
    .expect("expected successful decode")
}

/// An environment template declaring `exts` whose `environment:` body is
/// `env_body` (YAML, indented two spaces).
fn env_template(exts: &str, env_body: &str) -> String {
    format!(
        r#"
specificationVersion: "environment-2023-09"
extensions: [{exts}]
environment:
{env_body}
"#
    )
}

/// A job template declaring `exts` with one job environment (`job_env_body`,
/// indented four spaces beneath the list item) and one step with one step
/// environment (`step_env_body`, indented eight spaces).
fn job_template(exts: &str, job_env_body: &str, step_env_body: &str) -> String {
    format!(
        r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [{exts}]
name: Test
jobEnvironments:
  - name: JobEnv
{job_env_body}
steps:
  - name: S
    stepEnvironments:
      - name: StepEnv
{step_env_body}
    script:
      actions:
        onRun:
          command: run
"#
    )
}

const ECHO: &str = r#"{ command: echo }"#;

/// `actions:` body (indented `indent` spaces) defining exactly `hooks`.
fn actions_body(hooks: &[&str], indent: usize) -> String {
    let pad = " ".repeat(indent);
    let mut s = format!("{pad}script:\n{pad}  actions:\n");
    for hook in hooks {
        s.push_str(&format!("{pad}    {hook}: {ECHO}\n"));
    }
    s
}

const RFC0008_HOOKS: &[&str] = &["onWrapEnvEnter", "onWrapTaskRun", "onWrapEnvExit"];
const SERVICE_HOOKS: &[&str] = &[
    "onWrapServiceEnter",
    "onWrapServiceRun",
    "onWrapServiceHealthCheck",
    "onWrapServiceExit",
];
const ENV_HOOKS: &[&str] = &["onWrapEnvEnter", "onWrapEnvExit"];

fn all_seven() -> Vec<&'static str> {
    [RFC0008_HOOKS, SERVICE_HOOKS].concat()
}

fn with_service_hooks(base: &[&'static str]) -> Vec<&'static str> {
    let mut v = base.to_vec();
    v.extend_from_slice(SERVICE_HOOKS);
    v
}

// ════════════════════════════════════════════════════════════════════
// §4 item 4 — runScope: schema and accessors
// ════════════════════════════════════════════════════════════════════

#[test]
fn run_scope_absent_means_every_kind_of_session() {
    let et = expect_env_ok(
        &env_template("SERVICE, EXPR", "  name: E\n  variables: { K: v }\n"),
        SERVICE_EXTS,
    );
    let env = et.environment().expect("environment");
    assert!(env.run_scope.is_none());
    assert!(env.runs_in(RunScope::Task));
    assert!(env.runs_in(RunScope::Service));
    assert_eq!(
        env.effective_run_scope().collect::<Vec<_>>(),
        vec![RunScope::Task, RunScope::Service]
    );
}

#[test]
fn run_scope_task_only() {
    let et = expect_env_ok(
        &env_template(
            "SERVICE, EXPR",
            "  name: E\n  runScope: [TASK]\n  variables: { K: v }\n",
        ),
        SERVICE_EXTS,
    );
    let env = et.environment().unwrap();
    assert_eq!(env.run_scope.as_ref().unwrap(), &["TASK"]);
    assert!(env.runs_in(RunScope::Task));
    assert!(!env.runs_in(RunScope::Service));
    assert_eq!(
        env.effective_run_scope().collect::<Vec<_>>(),
        vec![RunScope::Task]
    );
}

#[test]
fn run_scope_service_only() {
    let et = expect_env_ok(
        &env_template(
            "SERVICE, EXPR",
            "  name: E\n  runScope: [SERVICE]\n  variables: { K: v }\n",
        ),
        SERVICE_EXTS,
    );
    let env = et.environment().unwrap();
    assert!(!env.runs_in(RunScope::Task));
    assert!(env.runs_in(RunScope::Service));
    assert_eq!(
        env.effective_run_scope().collect::<Vec<_>>(),
        vec![RunScope::Service]
    );
}

#[test]
fn run_scope_both_explicit_in_any_order() {
    let et = expect_env_ok(
        &env_template(
            "SERVICE, EXPR",
            "  name: E\n  runScope: [SERVICE, TASK]\n  variables: { K: v }\n",
        ),
        SERVICE_EXTS,
    );
    let env = et.environment().unwrap();
    assert!(env.runs_in(RunScope::Task));
    assert!(env.runs_in(RunScope::Service));
    // Reported in schema order regardless of the written order.
    assert_eq!(
        env.effective_run_scope().collect::<Vec<_>>(),
        vec![RunScope::Task, RunScope::Service]
    );
}

#[test]
fn run_scope_accepted_on_job_environments() {
    let jt = expect_job_ok(
        &job_template(
            "SERVICE, EXPR",
            "    runScope: [SERVICE]\n    variables: { K: v }\n",
            "        variables: { K: v }\n",
        ),
        SERVICE_EXTS,
    );
    let job_env = &jt.job_environments.as_ref().unwrap()[0];
    assert!(!job_env.runs_in(RunScope::Task));
    assert!(job_env.runs_in(RunScope::Service));
    let step_env = &jt.steps[0].step_environments.as_ref().unwrap()[0];
    assert!(step_env.run_scope.is_none());
}

/// §4 item 4 constraint 4 / §9.9 item 3: a Step Environment is entered only
/// by the Task Sessions of its Step, so it must not give a `runScope` at
/// all — not even `[TASK]`, the only kind that could enter it. The list
/// alone is the error, whatever it names.
const STEP_ENV_RUN_SCOPE_RULE: &str =
    "steps[0] -> stepEnvironments[0] -> runScope:\n\trunScope is not permitted on a Step \
     Environment: a Step Environment is entered only by the Task Sessions of its Step, so there \
     is no kind of Session for it to choose (Template Schemas §4 item 4 constraint 4).";

#[test]
fn run_scope_on_step_environment_is_rejected() {
    for run_scope in [
        "[TASK]",
        "[SERVICE]",
        "[TASK, SERVICE]",
        "[SERVICE, TASK]",
        "[]",
        "[NOPE]",
    ] {
        expect_job_err(
            &job_template(
                "SERVICE, EXPR",
                "    variables: { K: v }\n",
                &format!("        runScope: {run_scope}\n        variables: {{ K: v }}\n"),
            ),
            SERVICE_EXTS,
            &[
                "1 validation error for JobTemplate\n",
                STEP_ENV_RUN_SCOPE_RULE,
            ],
        );
    }
}

/// A Step Environment that gives a `runScope` *and* references `Service.*`
/// gets the constraint-4 error alone: the Step lists the Service, so the
/// references are in scope (a Step Environment is always a Task-Session
/// site), and the list is the one finding.
#[test]
fn run_scope_on_step_environment_reports_the_list_not_each_reference() {
    let template = r#"
specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, EXPR]
name: T
services:
  - name: Svc
    ports: [{ name: p }]
    healthCheck: { type: STDOUT }
    script: { actions: { onRun: { command: svc } } }
steps:
  - name: Work
    dependencies: [{ service: "Svc" }]
    stepEnvironments:
      - name: Cfg
        runScope: [SERVICE]
        variables:
          ADDR: "{{ join_host_port(Service.Svc.p.connectAddress, Service.Svc.p.port) }}"
          PORT: "{{ Service.Svc.p.port }}"
    script: { actions: { onRun: { command: echo, args: ["{{ Service.Svc.p.port }}"] } } }
"#;
    expect_job_err(
        template,
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            STEP_ENV_RUN_SCOPE_RULE,
        ],
    );
}

#[test]
fn run_scope_enum_spelling_and_parsing() {
    assert_eq!(RunScope::Task.as_str(), "TASK");
    assert_eq!(RunScope::Service.as_str(), "SERVICE");
    assert_eq!(RunScope::Task.to_string(), "TASK");
    assert_eq!("TASK".parse::<RunScope>().unwrap(), RunScope::Task);
    assert_eq!("SERVICE".parse::<RunScope>().unwrap(), RunScope::Service);
    assert_eq!(
        "task".parse::<RunScope>().unwrap_err(),
        "unknown run scope name 'task'"
    );
    assert_eq!(RunScope::ALL, [RunScope::Task, RunScope::Service]);
    // The serde spelling matches, for the job-side types that will carry it.
    assert_eq!(
        serde_json::to_string(&RunScope::Service).unwrap(),
        "\"SERVICE\""
    );
    assert_eq!(
        serde_json::from_str::<RunScope>("\"TASK\"").unwrap(),
        RunScope::Task
    );
}

// ════════════════════════════════════════════════════════════════════
// §4 item 4 — runScope: gating and constraints
// ════════════════════════════════════════════════════════════════════

#[test]
fn run_scope_requires_service_extension_in_env_template() {
    expect_env_err(
        &env_template(
            "EXPR",
            "  name: E\n  runScope: [TASK]\n  variables: { K: v }\n",
        ),
        EXPR_ONLY,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "environment -> runScope:\n\trunScope requires the SERVICE extension.",
        ],
    );
}

#[test]
fn run_scope_requires_service_extension_in_job_template() {
    expect_job_err(
        &job_template(
            "EXPR",
            "    runScope: [TASK]\n    variables: { K: v }\n",
            "        runScope: [SERVICE]\n        variables: { K: v }\n",
        ),
        EXPR_ONLY,
        &[
            "2 validation errors for JobTemplate\n",
            "jobEnvironments[0] -> runScope:\n\trunScope requires the SERVICE extension.",
            "steps[0] -> stepEnvironments[0] -> runScope:\n\trunScope requires the SERVICE extension.",
        ],
    );
}

#[test]
fn run_scope_gating_does_not_examine_the_list() {
    // Without SERVICE the contents are not validated: one error, not three.
    expect_env_err(
        &env_template(
            "EXPR",
            "  name: E\n  runScope: [BOGUS, BOGUS]\n  variables: { K: v }\n",
        ),
        EXPR_ONLY,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "environment -> runScope:\n\trunScope requires the SERVICE extension.",
        ],
    );
}

#[test]
fn run_scope_must_not_be_empty() {
    expect_env_err(
        &env_template(
            "SERVICE, EXPR",
            "  name: E\n  runScope: []\n  variables: { K: v }\n",
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "environment -> runScope:\n\tmust not be empty.",
        ],
    );
}

#[test]
fn run_scope_rejects_unknown_names_individually() {
    expect_env_err(
        &env_template(
            "SERVICE, EXPR",
            "  name: E\n  runScope: [TASK, WORKER, task]\n  variables: { K: v }\n",
        ),
        SERVICE_EXTS,
        &[
            "2 validation errors for EnvironmentTemplate\n",
            "environment -> runScope[1]:\n\tunknown run scope name 'WORKER'; expected one of TASK, SERVICE.",
            "environment -> runScope[2]:\n\tunknown run scope name 'task'; expected one of TASK, SERVICE.",
        ],
    );
}

#[test]
fn run_scope_rejects_duplicates() {
    expect_env_err(
        &env_template(
            "SERVICE, EXPR",
            "  name: E\n  runScope: [TASK, SERVICE, TASK]\n  variables: { K: v }\n",
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "environment -> runScope[2]:\n\tduplicate run scope name 'TASK'.",
        ],
    );
}

#[test]
fn run_scope_errors_in_job_template_paths() {
    expect_job_err(
        &job_template(
            "SERVICE, EXPR",
            "    runScope: [TASK, TASK]\n    variables: { K: v }\n",
            "        runScope: [NOPE]\n        variables: { K: v }\n",
        ),
        SERVICE_EXTS,
        &[
            "2 validation errors for JobTemplate\n",
            "jobEnvironments[0] -> runScope[1]:\n\tduplicate run scope name 'TASK'.",
            STEP_ENV_RUN_SCOPE_RULE,
        ],
    );
}

#[test]
fn run_scope_must_be_a_list() {
    expect_env_err(
        &env_template(
            "SERVICE, EXPR",
            "  name: E\n  runScope: TASK\n  variables: { K: v }\n",
        ),
        SERVICE_EXTS,
        &["invalid type: string \"TASK\", expected a sequence"],
    );
}

// ════════════════════════════════════════════════════════════════════
// §4 item 3 — dependencies: gating and constraints
// ════════════════════════════════════════════════════════════════════

/// A Job Template with one Service `X` (so that `service:X` resolves) used
/// by its Step, one Job Environment (`job_env_body`, indented four spaces)
/// and one Step Environment (`step_env_body`, indented eight spaces).
fn job_template_with_service(job_env_body: &str, step_env_body: &str) -> String {
    format!(
        r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [SERVICE, EXPR]
name: Test
jobEnvironments:
  - name: JobEnv
{job_env_body}
services:
  - name: X
    ports: [{{ name: main }}]
    script: {{ actions: {{ onRun: {{ command: serve }} }} }}
steps:
  - name: S
    dependencies: [{{ service: "X" }}]
    stepEnvironments:
      - name: StepEnv
{step_env_body}
    script:
      actions:
        onRun:
          command: run
"#
    )
}

/// An Environment Template with one Service `X` and `environment:` body
/// `env_body` (indented two spaces).
fn env_template_with_service(env_body: &str) -> String {
    format!(
        r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, EXPR]
services:
  - name: X
    ports: [{{ name: main }}]
    script: {{ actions: {{ onRun: {{ command: serve }} }} }}
environment:
{env_body}
"#
    )
}

const STEP_ENV_DEPENDENCIES_MSG: &str = "a Step Environment follows its Step's dependencies and \
    must not give a dependencies list of its own (Template Schemas §4 item 3 constraint 4).";

#[test]
fn environment_dependencies_requires_service_extension() {
    // Gated like runScope, in every position; the list is not examined.
    expect_env_err(
        &env_template(
            "EXPR",
            "  name: E\n  dependencies: [{ dependsOn: Bogus }, { dependsOn: Bogus }]\n  variables: { K: v }\n",
        ),
        EXPR_ONLY,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "environment -> dependencies:\n\tdependencies requires the SERVICE extension.",
        ],
    );
    expect_job_err(
        &job_template(
            "EXPR",
            "    dependencies: [{ service: \"X\" }]\n    variables: { K: v }\n",
            "        dependencies: [{ service: \"X\" }]\n        variables: { K: v }\n",
        ),
        EXPR_ONLY,
        &[
            "2 validation errors for JobTemplate\n",
            "jobEnvironments[0] -> dependencies:\n\tdependencies requires the SERVICE extension.",
            "steps[0] -> stepEnvironments[0] -> dependencies:\n\tdependencies requires the SERVICE extension.",
        ],
    );
}

#[test]
fn environment_dependencies_without_service_are_rejected_even_when_available() {
    // The template does not declare SERVICE: the field is unknown to it
    // whatever the caller allows.
    expect_job_err(
        &job_template(
            "EXPR",
            "    dependencies: [{ service: \"X\" }]\n    variables: { K: v }\n",
            "        variables: { K: v }\n",
        ),
        ALL_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "jobEnvironments[0] -> dependencies:\n\tdependencies requires the SERVICE extension.",
        ],
    );
}

#[test]
fn environment_dependencies_accepted_on_a_job_environment_and_an_environment_template() {
    let jt = expect_job_ok(
        &job_template_with_service(
            "    dependencies: [{ service: \"X\" }]\n    variables: { K: v }\n",
            "        variables: { K: v }\n",
        ),
        SERVICE_EXTS,
    );
    let env = &jt.job_environments.as_ref().unwrap()[0];
    assert_eq!(env.dependencies.as_ref().unwrap().len(), 1);
    assert_eq!(env.listed_services().collect::<Vec<_>>(), ["X"]);
    let et = expect_env_ok(
        &env_template_with_service(
            "  name: E\n  dependencies: [{ service: \"X\" }]\n  variables: { K: v }\n",
        ),
        SERVICE_EXTS,
    );
    let env = et.environment.as_ref().unwrap();
    assert_eq!(env.listed_services().collect::<Vec<_>>(), ["X"]);
}

#[test]
fn environment_dependencies_rejected_on_a_step_environment() {
    // §4 item 3 constraint 4 / §3 item 5 constraint 3: even one naming a
    // Service the Step itself lists.
    expect_job_err(
        &job_template_with_service(
            "    variables: { K: v }\n",
            "        dependencies: [{ service: \"X\" }]\n        variables: { K: v }\n",
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            &format!(
                "steps[0] -> stepEnvironments[0] -> dependencies:\n\t{STEP_ENV_DEPENDENCIES_MSG}"
            ),
        ],
    );
    // The prohibition is the whole report: the entries are not examined.
    expect_job_err(
        &job_template_with_service(
            "    variables: { K: v }\n",
            "        dependencies: [{ dependsOn: Nope }, { dependsOn: Nope }]\n        variables: { K: v }\n",
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            &format!("steps[0] -> stepEnvironments[0] -> dependencies:\n\t{STEP_ENV_DEPENDENCIES_MSG}"),
        ],
    );
}

#[test]
fn environment_dependencies_must_not_be_empty() {
    expect_job_err(
        &job_template_with_service(
            "    dependencies: []\n    variables: { K: v }\n",
            "        variables: { K: v }\n",
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "jobEnvironments[0] -> dependencies:\n\tmust not be empty.",
        ],
    );
    expect_env_err(
        &env_template_with_service("  name: E\n  dependencies: []\n  variables: { K: v }\n"),
        SERVICE_EXTS,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "environment -> dependencies:\n\tmust not be empty.",
        ],
    );
}

const STEP_NAME_MSG: &str = "dependency 'dependsOn: S' names a Step, but an Environment is \
    entered by Sessions, not scheduled; an Environment may depend only on a Service, as \
    'service: <name>'.";

#[test]
fn environment_dependencies_reject_a_step_name() {
    // §4 item 3 constraint 2 / §3.2 constraint 5: even a real Step's name.
    expect_job_err(
        &job_template_with_service(
            "    dependencies: [{ dependsOn: S }]\n    variables: { K: v }\n",
            "        variables: { K: v }\n",
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            &format!("jobEnvironments[0] -> dependencies[0]:\n\t{STEP_NAME_MSG}"),
        ],
    );
    expect_env_err(
        &env_template_with_service(
            "  name: E\n  dependencies: [{ dependsOn: S }]\n  variables: { K: v }\n",
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for EnvironmentTemplate\n",
            &format!("environment -> dependencies[0]:\n\t{STEP_NAME_MSG}"),
        ],
    );
}

#[test]
fn environment_dependencies_reject_an_unknown_service() {
    expect_job_err(
        &job_template_with_service(
            "    dependencies: [{ service: \"X\" }, { service: \"Nope\" }]\n    variables: { K: v }\n",
            "        variables: { K: v }\n",
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "jobEnvironments[0] -> dependencies[1]:\n\tdependency 'service: Nope' not found: no \
             Service of that name in services or requiresServices.",
        ],
    );
    expect_env_err(
        &env_template_with_service(
            "  name: E\n  dependencies: [{ service: \"Nope\" }]\n  variables: { K: v }\n",
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "environment -> dependencies[0]:\n\tdependency 'service: Nope' not found: no Service \
             of that name in this document's services.",
        ],
    );
}

#[test]
fn environment_template_without_services_cannot_list_one() {
    // §1.2.2: a document with no `services` of its own has nothing to list
    // or reference, even a Service a scheduler may attach from elsewhere.
    expect_env_err(
        &env_template(
            "SERVICE, EXPR",
            "  name: E\n  dependencies: [{ service: \"Store\" }]\n  runScope: [TASK]\n  variables: { P: \"{{ Service.Store.main.port }}\" }\n",
        ),
        SERVICE_EXTS,
        &[
            "2 validation errors for EnvironmentTemplate\n",
            "environment -> dependencies[0]:\n\tdependency 'service: Store' not found: no Service \
             of that name in this document's services.",
            "environment -> variables -> P:\n\tFailed to parse interpolation expression at [",
            "Undefined variable: 'Service.Store.main.port'.",
        ],
    );
}

#[test]
fn environment_dependencies_reject_a_duplicate() {
    expect_job_err(
        &job_template_with_service(
            "    dependencies: [{ service: \"X\" }, { service: \"X\" }]\n    variables: { K: v }\n",
            "        variables: { K: v }\n",
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "jobEnvironments[0] -> dependencies[1]:\n\tduplicate dependency 'service: X'.",
        ],
    );
}

#[test]
fn environment_dependencies_reject_an_explicit_service_run_scope() {
    // §4 item 3 constraint 5 / item 4 constraint 2, with or without a
    // reference.
    for run_scope in ["[SERVICE]", "[TASK, SERVICE]", "[SERVICE, TASK]"] {
        expect_job_err(
            &job_template_with_service(
                &format!(
                    "    dependencies: [{{ service: \"X\" }}]\n    runScope: {run_scope}\n    variables: {{ K: v }}\n"
                ),
                "        variables: { K: v }\n",
            ),
            SERVICE_EXTS,
            &[
                "1 validation error for JobTemplate\n",
                "jobEnvironments[0] -> runScope:\n\tEnvironment 'JobEnv' is entered in Service \
                 Sessions (its runScope includes SERVICE) and may not depend on a Service; declare \
                 runScope: [TASK] if it configures Tasks.",
            ],
        );
    }
    expect_env_err(
        &env_template_with_service(
            "  name: E\n  dependencies: [{ service: \"X\" }]\n  runScope: [SERVICE]\n  variables: { K: v }\n",
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "environment -> runScope:\n\tEnvironment 'E' is entered in Service Sessions (its \
             runScope includes SERVICE) and may not depend on a Service; declare runScope: [TASK] \
             if it configures Tasks.",
        ],
    );
    // An explicit [TASK] is fine.
    expect_job_ok(
        &job_template_with_service(
            "    dependencies: [{ service: \"X\" }]\n    runScope: [TASK]\n    variables: { K: v }\n",
            "        variables: { K: v }\n",
        ),
        SERVICE_EXTS,
    );
}

#[test]
fn environment_dependencies_errors_are_reported_together() {
    // Every entry gets its own report (an unknown duplicate gets two), and
    // the runScope rule its own.
    expect_job_err(
        &job_template_with_service(
            "    dependencies: [{ dependsOn: S }, { service: \"Nope\" }, { service: \"Nope\" }]\n    runScope: [SERVICE]\n    variables: { K: v }\n",
            "        variables: { K: v }\n",
        ),
        SERVICE_EXTS,
        &[
            "5 validation errors for JobTemplate\n",
            &format!("jobEnvironments[0] -> dependencies[0]:\n\t{STEP_NAME_MSG}"),
            "jobEnvironments[0] -> dependencies[1]:\n\tdependency 'service: Nope' not found",
            "jobEnvironments[0] -> dependencies[2]:\n\tdependency 'service: Nope' not found",
            "jobEnvironments[0] -> dependencies[2]:\n\tduplicate dependency 'service: Nope'.",
            "jobEnvironments[0] -> runScope:\n\tEnvironment 'JobEnv' is entered in Service Sessions",
        ],
    );
}

#[test]
fn environment_dependencies_must_be_a_list_of_depends_on() {
    expect_env_err(
        &env_template_with_service(
            "  name: E\n  dependencies: [\"service:X\"]\n  variables: { K: v }\n",
        ),
        SERVICE_EXTS,
        &["invalid type: string \"service:X\", expected struct StepDependency"],
    );
}

// ════════════════════════════════════════════════════════════════════
// §4.3 — onWrapService* hooks: schema, gating, defaults
// ════════════════════════════════════════════════════════════════════

#[test]
fn all_seven_wrap_hooks_accepted_with_default_run_scope() {
    let et = expect_env_ok(
        &env_template(
            "SERVICE, EXPR, WRAP_ACTIONS",
            &format!("  name: Wrapper\n{}", actions_body(&all_seven(), 2)),
        ),
        ALL_EXTS,
    );
    let actions = &et.environment().unwrap().script.as_ref().unwrap().actions;
    assert!(actions.on_wrap_service_enter.is_some());
    assert!(actions.on_wrap_service_run.is_some());
    assert!(actions.on_wrap_service_health_check.is_some());
    assert!(actions.on_wrap_service_exit.is_some());
    assert!(actions.has_any_wrap_hook());
    assert!(actions.has_any_service_wrap_hook());
    assert!(actions.has_any_action());

    let names: Vec<&str> = actions.iter_named().map(|(n, _)| n).collect();
    assert_eq!(names, all_seven());
    let hook_names: Vec<&str> = actions.wrap_hooks().iter().map(|(n, _, _)| *n).collect();
    assert_eq!(hook_names, all_seven());
    let service_names: Vec<&str> = actions
        .service_wrap_hooks()
        .iter()
        .map(|(n, _)| *n)
        .collect();
    assert_eq!(service_names, SERVICE_HOOKS);
    assert_eq!(actions.named_slots().len(), 9);
}

#[test]
fn wrap_hook_default_timeouts_follow_the_wrapped_action() {
    // Template Schemas §5 timeout table, extended to the RFC 0009 hooks by
    // analogy with onWrapEnvExit taking onExit's default.
    for (name, expected) in [
        ("onEnter", None),
        ("onWrapEnvEnter", None),
        ("onWrapTaskRun", None),
        ("onWrapEnvExit", Some(300)),
        ("onWrapServiceEnter", None),
        ("onWrapServiceRun", None),
        ("onWrapServiceHealthCheck", Some(30)),
        ("onWrapServiceExit", Some(300)),
        ("onExit", Some(300)),
        ("onRun", None),
    ] {
        assert_eq!(
            EnvironmentActions::default_timeout_seconds(name),
            expected,
            "{name}"
        );
    }
    assert_eq!(EnvironmentActions::ON_EXIT_DEFAULT_TIMEOUT_SECONDS, 300);
    assert_eq!(
        EnvironmentActions::ON_WRAP_SERVICE_HEALTH_CHECK_DEFAULT_TIMEOUT_SECONDS,
        30
    );
}

#[test]
fn service_hooks_require_wrap_actions_when_only_service_declared() {
    expect_env_err(
        &env_template(
            "SERVICE, EXPR",
            &format!("  name: Wrapper\n{}", actions_body(SERVICE_HOOKS, 2)),
        ),
        SERVICE_EXTS,
        &[
            "4 validation errors for EnvironmentTemplate\n",
            "environment -> script -> actions -> onWrapServiceEnter:\n\tonWrapServiceEnter requires the WRAP_ACTIONS extension.",
            "environment -> script -> actions -> onWrapServiceRun:\n\tonWrapServiceRun requires the WRAP_ACTIONS extension.",
            "environment -> script -> actions -> onWrapServiceHealthCheck:\n\tonWrapServiceHealthCheck requires the WRAP_ACTIONS extension.",
            "environment -> script -> actions -> onWrapServiceExit:\n\tonWrapServiceExit requires the WRAP_ACTIONS extension.",
        ],
    );
}

#[test]
fn service_hooks_require_service_when_only_wrap_actions_declared() {
    // The three RFC 0008 hooks are complete, so the all-or-nothing rule is
    // satisfied and the only errors are the four SERVICE gates.
    expect_env_err(
        &env_template(
            "WRAP_ACTIONS, EXPR",
            &format!("  name: Wrapper\n{}", actions_body(&all_seven(), 2)),
        ),
        WRAP_EXTS,
        &[
            "4 validation errors for EnvironmentTemplate\n",
            "environment -> script -> actions -> onWrapServiceEnter:\n\tonWrapServiceEnter requires the SERVICE extension.",
            "environment -> script -> actions -> onWrapServiceRun:\n\tonWrapServiceRun requires the SERVICE extension.",
            "environment -> script -> actions -> onWrapServiceHealthCheck:\n\tonWrapServiceHealthCheck requires the SERVICE extension.",
            "environment -> script -> actions -> onWrapServiceExit:\n\tonWrapServiceExit requires the SERVICE extension.",
        ],
    );
}

#[test]
fn service_hooks_require_both_extensions_when_neither_declared() {
    expect_env_err(
        &env_template(
            "EXPR",
            &format!(
                "  name: Wrapper\n{}",
                actions_body(&["onEnter", "onWrapServiceRun"], 2)
            ),
        ),
        EXPR_ONLY,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "environment -> script -> actions -> onWrapServiceRun:\n\tonWrapServiceRun requires the WRAP_ACTIONS and SERVICE extensions.",
        ],
    );
}

#[test]
fn service_hooks_gated_in_job_template_paths() {
    expect_job_err(
        &job_template(
            "SERVICE, EXPR",
            &actions_body(&["onWrapServiceExit"], 4),
            &actions_body(&["onWrapServiceEnter"], 8),
        ),
        SERVICE_EXTS,
        &[
            "2 validation errors for JobTemplate\n",
            "jobEnvironments[0] -> script -> actions -> onWrapServiceExit:\n\tonWrapServiceExit requires the WRAP_ACTIONS extension.",
            "steps[0] -> stepEnvironments[0] -> script -> actions -> onWrapServiceEnter:\n\tonWrapServiceEnter requires the WRAP_ACTIONS extension.",
        ],
    );
}

#[test]
fn service_hooks_see_wrapped_action_variables() {
    // Like every wrap hook, the Service hooks resolve at run time with
    // `WrappedAction.*` seeded, including round-trip timeout forwarding.
    expect_env_ok(
        &env_template(
            "SERVICE, EXPR, WRAP_ACTIONS, FEATURE_BUNDLE_1",
            r#"  name: Wrapper
  script:
    actions:
      onWrapEnvEnter: { command: echo }
      onWrapTaskRun: { command: echo }
      onWrapEnvExit: { command: echo }
      onWrapServiceEnter: { command: "{{ WrappedAction.Command }}", args: ["{{ WrappedAction.Args }}"] }
      onWrapServiceRun:
        command: "{{ WrappedAction.Command }}"
        timeout: "{{ WrappedAction.Timeout }}"
      onWrapServiceHealthCheck: { command: "{{ WrappedAction.Command }}" }
      onWrapServiceExit: { command: "{{ WrappedAction.Command }}" }
"#,
        ),
        ALL_EXTS,
    );
}

#[test]
fn service_hook_timing_fields_see_wrapped_service() {
    // A hook's `timeout` and `cancelation` resolve at run time against the
    // same symbol table as its command and args, so the hook's companion
    // group (`WrappedService.*` here) is in scope there too — not only
    // `WrappedAction.*`.
    expect_env_ok(
        &env_template(
            "SERVICE, EXPR, WRAP_ACTIONS, FEATURE_BUNDLE_1",
            r#"  name: Wrapper
  runScope: [SERVICE]
  script:
    actions:
      onWrapEnvEnter:
        command: echo
        timeout: "{{ 30 if WrappedEnv.Name == 'Conda' else 60 }}"
      onWrapEnvExit: { command: echo }
      onWrapServiceEnter: { command: echo }
      onWrapServiceRun:
        command: echo
        timeout: "{{ 60 if WrappedService.Name == 'Store' else WrappedAction.Timeout }}"
        cancelation:
          mode: NOTIFY_THEN_TERMINATE
          notifyPeriodInSeconds: "{{ 30 + len(WrappedService.Ports) }}"
      onWrapServiceHealthCheck: { command: echo }
      onWrapServiceExit: { command: echo }
"#,
        ),
        ALL_EXTS,
    );
}

#[test]
fn hook_timing_fields_reject_other_hooks_companion_groups() {
    // The companion group is per hook: `WrappedService.*` is not in scope
    // in an env hook's timing fields, nor `WrappedStep.Name` in a Service
    // hook's.
    expect_env_err(
        &env_template(
            "SERVICE, EXPR, WRAP_ACTIONS, FEATURE_BUNDLE_1",
            r#"  name: Wrapper
  script:
    actions:
      onWrapEnvEnter:
        command: echo
        timeout: "{{ WrappedService.Ports[0] }}"
      onWrapTaskRun: { command: echo }
      onWrapEnvExit: { command: echo }
      onWrapServiceEnter: { command: echo }
      onWrapServiceRun:
        command: echo
        timeout: "{{ 60 if WrappedStep.Name == 'Render' else 30 }}"
      onWrapServiceHealthCheck: { command: echo }
      onWrapServiceExit: { command: echo }
"#,
        ),
        ALL_EXTS,
        &[
            "2 validation errors for EnvironmentTemplate\n",
            "environment -> script -> actions -> onWrapEnvEnter -> timeout:\n\tFailed to parse interpolation expression at [0, 29]. Undefined variable: 'WrappedService.Ports'.",
            "environment -> script -> actions -> onWrapServiceRun -> timeout:\n\tFailed to parse interpolation expression at [0, 48]. Undefined variable: 'WrappedStep.Name'.",
        ],
    );
}

#[test]
fn wrapped_env_and_step_names_not_available_in_service_hooks() {
    expect_env_err(
        &env_template(
            "SERVICE, EXPR, WRAP_ACTIONS",
            r#"  name: Wrapper
  script:
    actions:
      onWrapEnvEnter: { command: echo }
      onWrapTaskRun: { command: echo }
      onWrapEnvExit: { command: echo }
      onWrapServiceEnter: { command: "{{ WrappedEnv.Name }}" }
      onWrapServiceRun: { command: "{{ WrappedStep.Name }}" }
      onWrapServiceHealthCheck: { command: echo }
      onWrapServiceExit: { command: echo }
"#,
        ),
        ALL_EXTS,
        &[
            "2 validation errors for EnvironmentTemplate\n",
            "environment -> script -> actions -> onWrapServiceEnter -> command:\n\tFailed to parse interpolation expression at [0, 21]. Undefined variable: 'WrappedEnv.Name'.",
            "environment -> script -> actions -> onWrapServiceRun -> command:\n\tFailed to parse interpolation expression at [0, 22]. Undefined variable: 'WrappedStep.Name'.",
        ],
    );
}

// ════════════════════════════════════════════════════════════════════
// §4.3 WRAP_ACTIONS constraint 6 — hooks follow runScope
// ════════════════════════════════════════════════════════════════════

const SERVICE_GROUP_MISSING_ALL: &str = "a wrapping environment whose runScope includes SERVICE (default runScope: every kind of Session) must define onWrapServiceEnter, onWrapServiceRun, onWrapServiceHealthCheck, and onWrapServiceExit; missing: onWrapServiceEnter, onWrapServiceRun, onWrapServiceHealthCheck, onWrapServiceExit (RFC 0009).";

#[test]
fn default_run_scope_requires_all_seven_hooks() {
    // RFC 0008's three hooks alone are no longer complete once SERVICE is
    // declared: the default runScope includes SERVICE.
    expect_env_err(
        &env_template(
            "SERVICE, EXPR, WRAP_ACTIONS",
            &format!("  name: Wrapper\n{}", actions_body(RFC0008_HOOKS, 2)),
        ),
        ALL_EXTS,
        &[
            "1 validation error for EnvironmentTemplate\n",
            &format!("environment -> script -> actions:\n\t{SERVICE_GROUP_MISSING_ALL}"),
        ],
    );
}

#[test]
fn default_run_scope_with_only_service_hooks_requires_env_and_task_hooks() {
    expect_env_err(
        &env_template(
            "SERVICE, EXPR, WRAP_ACTIONS",
            &format!("  name: Wrapper\n{}", actions_body(SERVICE_HOOKS, 2)),
        ),
        ALL_EXTS,
        &[
            "2 validation errors for EnvironmentTemplate\n",
            "environment -> script -> actions:\n\ta wrapping environment must define onWrapEnvEnter and onWrapEnvExit whatever its runScope; missing: onWrapEnvEnter, onWrapEnvExit (RFC 0009).",
            "environment -> script -> actions:\n\ta wrapping environment whose runScope includes TASK (default runScope: every kind of Session) must define onWrapTaskRun; missing: onWrapTaskRun (RFC 0009).",
        ],
    );
}

#[test]
fn task_run_scope_is_exactly_the_rfc0008_rule() {
    expect_env_ok(
        &env_template(
            "SERVICE, EXPR, WRAP_ACTIONS",
            &format!(
                "  name: Wrapper\n  runScope: [TASK]\n{}",
                actions_body(RFC0008_HOOKS, 2)
            ),
        ),
        ALL_EXTS,
    );
}

#[test]
fn task_run_scope_rejects_service_hooks() {
    expect_env_err(
        &env_template(
            "SERVICE, EXPR, WRAP_ACTIONS",
            &format!(
                "  name: Wrapper\n  runScope: [TASK]\n{}",
                actions_body(&with_service_hooks(RFC0008_HOOKS), 2)
            ),
        ),
        ALL_EXTS,
        &[
            "4 validation errors for EnvironmentTemplate\n",
            "environment -> script -> actions -> onWrapServiceEnter:\n\tonWrapServiceEnter must not be defined: this environment's runScope (runScope: [TASK]) excludes SERVICE (RFC 0009).",
            "environment -> script -> actions -> onWrapServiceRun:\n\tonWrapServiceRun must not be defined: this environment's runScope (runScope: [TASK]) excludes SERVICE (RFC 0009).",
            "environment -> script -> actions -> onWrapServiceHealthCheck:\n\tonWrapServiceHealthCheck must not be defined: this environment's runScope (runScope: [TASK]) excludes SERVICE (RFC 0009).",
            "environment -> script -> actions -> onWrapServiceExit:\n\tonWrapServiceExit must not be defined: this environment's runScope (runScope: [TASK]) excludes SERVICE (RFC 0009).",
        ],
    );
}

#[test]
fn task_run_scope_missing_task_hook() {
    expect_env_err(
        &env_template(
            "SERVICE, EXPR, WRAP_ACTIONS",
            &format!(
                "  name: Wrapper\n  runScope: [TASK]\n{}",
                actions_body(ENV_HOOKS, 2)
            ),
        ),
        ALL_EXTS,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "environment -> script -> actions:\n\ta wrapping environment whose runScope includes TASK (runScope: [TASK]) must define onWrapTaskRun; missing: onWrapTaskRun (RFC 0009).",
        ],
    );
}

#[test]
fn task_run_scope_missing_env_hooks() {
    expect_env_err(
        &env_template(
            "SERVICE, EXPR, WRAP_ACTIONS",
            &format!(
                "  name: Wrapper\n  runScope: [TASK]\n{}",
                actions_body(&["onEnter", "onWrapTaskRun", "onWrapEnvExit"], 2)
            ),
        ),
        ALL_EXTS,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "environment -> script -> actions:\n\ta wrapping environment must define onWrapEnvEnter and onWrapEnvExit whatever its runScope; missing: onWrapEnvEnter (RFC 0009).",
        ],
    );
}

#[test]
fn service_run_scope_requires_env_hooks_and_four_service_hooks() {
    let mut hooks: Vec<&str> = ENV_HOOKS.to_vec();
    hooks.extend_from_slice(SERVICE_HOOKS);
    expect_env_ok(
        &env_template(
            "SERVICE, EXPR, WRAP_ACTIONS",
            &format!(
                "  name: Wrapper\n  runScope: [SERVICE]\n{}",
                actions_body(&hooks, 2)
            ),
        ),
        ALL_EXTS,
    );
}

#[test]
fn service_run_scope_rejects_task_hook() {
    expect_env_err(
        &env_template(
            "SERVICE, EXPR, WRAP_ACTIONS",
            &format!(
                "  name: Wrapper\n  runScope: [SERVICE]\n{}",
                actions_body(&all_seven(), 2)
            ),
        ),
        ALL_EXTS,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "environment -> script -> actions -> onWrapTaskRun:\n\tonWrapTaskRun must not be defined: this environment's runScope (runScope: [SERVICE]) excludes TASK (RFC 0009).",
        ],
    );
}

#[test]
fn service_run_scope_missing_some_service_hooks() {
    expect_env_err(
        &env_template(
            "SERVICE, EXPR, WRAP_ACTIONS",
            &format!(
                "  name: Wrapper\n  runScope: [SERVICE]\n{}",
                actions_body(
                    &[
                        "onWrapEnvEnter",
                        "onWrapEnvExit",
                        "onWrapServiceRun",
                        "onWrapServiceHealthCheck"
                    ],
                    2
                )
            ),
        ),
        ALL_EXTS,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "environment -> script -> actions:\n\ta wrapping environment whose runScope includes SERVICE (runScope: [SERVICE]) must define onWrapServiceEnter, onWrapServiceRun, onWrapServiceHealthCheck, and onWrapServiceExit; missing: onWrapServiceEnter, onWrapServiceExit (RFC 0009).",
        ],
    );
}

#[test]
fn explicit_both_run_scope_requires_all_seven_hooks() {
    expect_env_ok(
        &env_template(
            "SERVICE, EXPR, WRAP_ACTIONS",
            &format!(
                "  name: Wrapper\n  runScope: [TASK, SERVICE]\n{}",
                actions_body(&all_seven(), 2)
            ),
        ),
        ALL_EXTS,
    );
    expect_env_err(
        &env_template(
            "SERVICE, EXPR, WRAP_ACTIONS",
            &format!(
                "  name: Wrapper\n  runScope: [TASK, SERVICE]\n{}",
                actions_body(&with_service_hooks(ENV_HOOKS), 2)
            ),
        ),
        ALL_EXTS,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "environment -> script -> actions:\n\ta wrapping environment whose runScope includes TASK (runScope: [TASK, SERVICE]) must define onWrapTaskRun; missing: onWrapTaskRun (RFC 0009).",
        ],
    );
}

#[test]
fn single_hook_reports_every_group_at_once() {
    // Defining one hook makes the environment a wrapping environment; all
    // three groups are then checked and reported together.
    expect_env_err(
        &env_template(
            "SERVICE, EXPR, WRAP_ACTIONS",
            &format!(
                "  name: Wrapper\n{}",
                actions_body(&["onWrapServiceRun"], 2)
            ),
        ),
        ALL_EXTS,
        &[
            "3 validation errors for EnvironmentTemplate\n",
            "environment -> script -> actions:\n\ta wrapping environment must define onWrapEnvEnter and onWrapEnvExit whatever its runScope; missing: onWrapEnvEnter, onWrapEnvExit (RFC 0009).",
            "environment -> script -> actions:\n\ta wrapping environment whose runScope includes TASK (default runScope: every kind of Session) must define onWrapTaskRun; missing: onWrapTaskRun (RFC 0009).",
            "environment -> script -> actions:\n\ta wrapping environment whose runScope includes SERVICE (default runScope: every kind of Session) must define onWrapServiceEnter, onWrapServiceRun, onWrapServiceHealthCheck, and onWrapServiceExit; missing: onWrapServiceEnter, onWrapServiceHealthCheck, onWrapServiceExit (RFC 0009).",
        ],
    );
}

#[test]
fn non_wrapping_environment_is_not_subject_to_the_rule() {
    // runScope: [SERVICE] with plain onEnter/onExit: no wrap hook, no rule.
    expect_env_ok(
        &env_template(
            "SERVICE, EXPR, WRAP_ACTIONS",
            &format!(
                "  name: Plain\n  runScope: [SERVICE]\n{}",
                actions_body(&["onEnter", "onExit"], 2)
            ),
        ),
        ALL_EXTS,
    );
}

#[test]
fn rule_applies_in_job_and_step_environments() {
    // A Step Environment gives no runScope and is entered only by Task
    // Sessions: it defines exactly RFC 0008's three hooks, so each
    // onWrapService* hook on it is rejected.
    expect_job_err(
        &job_template(
            "SERVICE, EXPR, WRAP_ACTIONS",
            &format!(
                "    runScope: [TASK]\n{}",
                actions_body(&["onWrapEnvEnter", "onWrapEnvExit"], 4)
            ),
            &actions_body(&all_seven(), 8),
        ),
        ALL_EXTS,
        &[
            "6 validation errors for JobTemplate\n",
            "jobEnvironments[0] -> script -> actions:\n\ta wrapping environment whose runScope includes TASK (runScope: [TASK]) must define onWrapTaskRun; missing: onWrapTaskRun (RFC 0009).",
            "steps[0] -> stepEnvironments[0] -> script -> actions -> onWrapServiceEnter:\n\tonWrapServiceEnter must not be defined: this environment's runScope (a Step Environment, entered only by Task Sessions) excludes SERVICE (RFC 0009).",
            "steps[0] -> stepEnvironments[0] -> script -> actions -> onWrapServiceRun:\n\tonWrapServiceRun must not be defined: this environment's runScope (a Step Environment, entered only by Task Sessions) excludes SERVICE (RFC 0009).",
            "steps[0] -> stepEnvironments[0] -> script -> actions -> onWrapServiceHealthCheck:\n\tonWrapServiceHealthCheck must not be defined: this environment's runScope (a Step Environment, entered only by Task Sessions) excludes SERVICE (RFC 0009).",
            "steps[0] -> stepEnvironments[0] -> script -> actions -> onWrapServiceExit:\n\tonWrapServiceExit must not be defined: this environment's runScope (a Step Environment, entered only by Task Sessions) excludes SERVICE (RFC 0009).",
            // Two wrap layers in one session: the RFC 0008 single-layer rule
            // still applies alongside.
            "steps[0] -> stepEnvironments:\n\tonly one environment in the session stack may define any of onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit (RFC 0008).",
        ],
    );
}

#[test]
fn step_environment_wrapper_defines_exactly_the_three_hooks() {
    // §4.3 constraint 6: a wrapping Step Environment with RFC 0008's three
    // hooks is valid beside a Service; one missing onWrapTaskRun is not.
    expect_job_ok(
        &job_template(
            "SERVICE, EXPR, WRAP_ACTIONS",
            "    variables: { K: v }\n",
            &actions_body(RFC0008_HOOKS, 8),
        ),
        ALL_EXTS,
    );
    expect_job_err(
        &job_template(
            "SERVICE, EXPR, WRAP_ACTIONS",
            "    variables: { K: v }\n",
            &actions_body(ENV_HOOKS, 8),
        ),
        ALL_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "steps[0] -> stepEnvironments[0] -> script -> actions:\n\ta wrapping environment whose runScope includes TASK (a Step Environment, entered only by Task Sessions) must define onWrapTaskRun; missing: onWrapTaskRun (RFC 0009).",
        ],
    );
}

#[test]
fn rule_uses_recognized_names_when_run_scope_is_also_invalid() {
    // An unknown name is reported by the runScope check; the hook rule sees
    // only the recognized names (here: TASK), so the three RFC 0008 hooks
    // are the right set and no hook error is added.
    expect_env_err(
        &env_template(
            "SERVICE, EXPR, WRAP_ACTIONS",
            &format!(
                "  name: Wrapper\n  runScope: [TASK, WORKER]\n{}",
                actions_body(RFC0008_HOOKS, 2)
            ),
        ),
        ALL_EXTS,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "environment -> runScope[1]:\n\tunknown run scope name 'WORKER'; expected one of TASK, SERVICE.",
        ],
    );
}

#[test]
fn rfc0008_rule_unchanged_without_service() {
    // WRAP_ACTIONS alone: three hooks complete, two of three rejected with
    // the RFC 0008 wording, and no RFC 0009 wording anywhere.
    expect_env_ok(
        &env_template(
            "WRAP_ACTIONS, EXPR",
            &format!("  name: Wrapper\n{}", actions_body(RFC0008_HOOKS, 2)),
        ),
        WRAP_EXTS,
    );
    let err = decode_environment_template(
        yaml_val(&env_template(
            "WRAP_ACTIONS, EXPR",
            &format!("  name: Wrapper\n{}", actions_body(ENV_HOOKS, 2)),
        )),
        Some(WRAP_EXTS),
        &CallerLimits::default(),
    )
    .expect_err("partial RFC 0008 set must be rejected")
    .to_string();
    assert!(
        err.contains("1 validation error for EnvironmentTemplate\n"),
        "{err}"
    );
    assert!(
        err.contains("environment -> script -> actions:\n\tan environment that defines any of onWrapEnvEnter, onWrapTaskRun, or onWrapEnvExit must define all three (RFC 0008)."),
        "{err}"
    );
    assert!(!err.contains("RFC 0009"), "{err}");
}

#[test]
fn rfc0008_rule_ignores_service_hooks_without_service() {
    // WRAP_ACTIONS alone with a stray Service hook and an incomplete RFC
    // 0008 set: the Service hook is a SERVICE gate error, and the
    // all-or-nothing count is over the three RFC 0008 hooks only.
    expect_env_err(
        &env_template(
            "WRAP_ACTIONS, EXPR",
            &format!(
                "  name: Wrapper\n{}",
                actions_body(&["onWrapTaskRun", "onWrapServiceRun"], 2)
            ),
        ),
        WRAP_EXTS,
        &[
            "2 validation errors for EnvironmentTemplate\n",
            "environment -> script -> actions -> onWrapServiceRun:\n\tonWrapServiceRun requires the SERVICE extension.",
            "environment -> script -> actions:\n\tan environment that defines any of onWrapEnvEnter, onWrapTaskRun, or onWrapEnvExit must define all three (RFC 0008).",
        ],
    );
}

// ════════════════════════════════════════════════════════════════════
// §1.2 — Environment Template root
// ════════════════════════════════════════════════════════════════════

const MINIMAL_SERVICE: &str = r#"
  - name: Cache
    ports:
      - name: main
    script:
      actions:
        onRun:
          command: valkey-server
"#;

#[test]
fn env_template_with_services_only() {
    let et = expect_env_ok(
        &format!(
            r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, EXPR]
services:{MINIMAL_SERVICE}
"#
        ),
        SERVICE_EXTS,
    );
    assert!(et.environment.is_none());
    assert!(et.environment().is_none());
    assert_eq!(et.services().len(), 1);
    assert_eq!(et.services()[0].name, "Cache");
    assert_eq!(et.services()[0].port_names().collect::<Vec<_>>(), ["main"]);
}

#[test]
fn env_template_with_environment_only_still_works() {
    let et = expect_env_ok(
        &env_template("SERVICE, EXPR", "  name: E\n  variables: { K: v }\n"),
        SERVICE_EXTS,
    );
    assert!(et.services.is_none());
    assert!(et.services().is_empty());
    assert_eq!(et.environment().unwrap().name, "E");
}

#[test]
fn env_template_with_both_environment_and_services() {
    let et = expect_env_ok(
        r#"
specificationVersion: "environment-2023-09"
$schema: "https://example.com/schema.json"
extensions: [SERVICE, EXPR]
parameterDefinitions:
  - name: Port
    type: INT
    default: 6379
services:
  - name: Cache
    ports:
      - name: main
        port: "{{ Param.Port }}"
    script:
      actions:
        onRun:
          command: valkey-server
environment:
  name: CacheClient
  runScope: [TASK]
  variables:
    VALKEY_PORT: "{{ Param.Port }}"
"#,
        SERVICE_EXTS,
    );
    assert_eq!(
        et.schema.as_deref(),
        Some("https://example.com/schema.json")
    );
    assert_eq!(et.services().len(), 1);
    let env = et.environment().unwrap();
    assert_eq!(env.name, "CacheClient");
    assert!(env.runs_in(RunScope::Task));
    assert!(!env.runs_in(RunScope::Service));
}

#[test]
fn env_template_with_neither_rejected() {
    expect_env_err(
        r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, EXPR]
parameterDefinitions:
  - name: Port
    type: INT
"#,
        SERVICE_EXTS,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "EnvironmentTemplate: must define at least one of 'environment' or 'services'.",
        ],
    );
    // Also without the extension, where `services` is not even an option.
    expect_env_err(
        r#"
specificationVersion: "environment-2023-09"
"#,
        EXPR_ONLY,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "EnvironmentTemplate: must define at least one of 'environment' or 'services'.",
        ],
    );
}

#[test]
fn env_template_schema_field_ignored_without_extensions() {
    let et = expect_env_ok(
        r#"
specificationVersion: "environment-2023-09"
$schema: "anything"
environment:
  name: E
  variables: { K: v }
"#,
        EXPR_ONLY,
    );
    assert_eq!(et.schema.as_deref(), Some("anything"));
}

#[test]
fn env_template_services_require_service_extension() {
    expect_env_err(
        &format!(
            r#"
specificationVersion: "environment-2023-09"
extensions: [EXPR]
services:{MINIMAL_SERVICE}
"#
        ),
        EXPR_ONLY,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "services:\n\tservices requires the SERVICE extension.",
        ],
    );
}

#[test]
fn env_template_services_gating_does_not_examine_the_list() {
    // An invalid Service inside an ungated list yields only the gate error.
    expect_env_err(
        r#"
specificationVersion: "environment-2023-09"
extensions: [EXPR]
services:
  - name: File
    ports: []
    script:
      actions:
        onRun:
          command: x
"#,
        EXPR_ONLY,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "services:\n\tservices requires the SERVICE extension.",
        ],
    );
}

#[test]
fn env_template_service_requires_expr() {
    expect_env_err(
        &format!(
            r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE]
services:{MINIMAL_SERVICE}
"#
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "extensions:\n\tSERVICE requires EXPR; both must be listed in the template's `extensions` (RFC 0009).",
        ],
    );
}

#[test]
fn env_template_services_must_not_be_empty() {
    expect_env_err(
        r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, EXPR]
services: []
"#,
        SERVICE_EXTS,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "services:\n\tmust not be empty.",
        ],
    );
}

#[test]
fn env_template_services_at_most_ten() {
    let mut services = String::new();
    for i in 0..11 {
        services.push_str(&format!(
            "  - name: S{i}\n    ports: [{{ name: p }}]\n    script: {{ actions: {{ onRun: {{ command: x }} }} }}\n"
        ));
    }
    expect_env_err(
        &format!(
            r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, EXPR]
services:
{services}
"#
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "services:\n\tmust not contain more than 10 elements.",
        ],
    );
}

#[test]
fn env_template_services_names_unique() {
    expect_env_err(
        &format!(
            r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, EXPR]
services:{MINIMAL_SERVICE}{MINIMAL_SERVICE}
"#
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "services[1]:\n\tduplicate service name: 'Cache'",
        ],
    );
}

#[test]
fn env_template_services_reuse_the_service_validator() {
    // The §9 structural checks apply to `services` exactly as to
    // `services`: the paths are rooted at `services[i]`.
    expect_env_err(
        r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, EXPR]
services:
  - name: File
    ports:
      - name: main
        port: 70000
    healthCheck:
      type: COMMAND
    script:
      actions:
        onRun:
          command: x
"#,
        SERVICE_EXTS,
        &[
            "3 validation errors for EnvironmentTemplate\n",
            "services[0] -> name:\n\tmust not be 'File'; it is reserved for Service.File.* references.",
            "services[0] -> ports[0] -> port:\n\tmust be between 1 and 65535.",
            "services[0] -> script -> actions:\n\tonHealthCheck must be defined when healthCheck.type is COMMAND.",
        ],
    );
}

#[test]
fn env_template_services_only_with_parameter_definitions_checked() {
    // The parameter-definition rules still apply to a services-only document.
    expect_env_err(
        &format!(
            r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, EXPR]
parameterDefinitions: []
services:{MINIMAL_SERVICE}
"#
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "parameterDefinitions, if provided, must contain at least one element.",
        ],
    );
}

#[test]
fn env_template_unknown_root_field_still_rejected() {
    expect_env_err(
        r#"
specificationVersion: "environment-2023-09"
environment:
  name: E
  variables: { K: v }
service:
  - name: typo
"#,
        EXPR_ONLY,
        &["unknown field `service`, expected one of `specificationVersion`, `$schema`, `extensions`, `parameterDefinitions`, `environment`, `services`"],
    );
}

#[test]
fn env_template_services_and_wrapping_environment_compose() {
    // A document that both supplies a Service and wraps Task Sessions.
    expect_env_ok(
        &format!(
            r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, EXPR, WRAP_ACTIONS]
services:{MINIMAL_SERVICE}
environment:
  name: Wrapper
  runScope: [TASK]
{}
"#,
            actions_body(RFC0008_HOOKS, 2)
        ),
        ALL_EXTS,
    );
}
