// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Integration tests for the RFC 0009 `SERVICE` extension's changes to
//! `<Environment>`, `<EnvironmentActions>`, and the Environment Template
//! root (Template Schemas §1.2, §4 items 3–4, §4.3 WRAP_ACTIONS constraint
//! 6, §9.9).
//!
//! These tests cover:
//!
//! 1. **`runScope`**, a field of earlier drafts of RFC 0009, is an unknown
//!    property (`test_env_params_schema.rs`).
//! 2. **`dependencies`** on `<Environment>` (§4 item 3):
//!    extension gating, the Step Environment prohibition, and the list
//!    constraints (non-empty, `service` key naming a Service of the
//!    document or a requirement, no duplicates). What listing a Service
//!    *does* — visibility, scope, which Sessions enter the Environment — is
//!    covered in `test_service_scope.rs`, `test_service_scope_rules.rs`, and
//!    `test_service_composition.rs`.
//! 3. **`onWrapService*` hooks** on `<EnvironmentActions>`: schema parse,
//!    gating on both `WRAP_ACTIONS` and `SERVICE`, and the default-timeout
//!    table.
//! 4. **Wrap hook rule 1** (the RFC 0009 rule that replaces RFC 0008's
//!    all-or-nothing rule when `SERVICE` is declared): a wrapper that Task
//!    Sessions are known to enter needs the three Task hooks, one that
//!    Service Sessions are known to enter the six Service hooks, extra hooks
//!    are permitted, and the unchanged RFC 0008 rule without `SERVICE`.
//! 5. **Environment Template root**: `$schema`, `services` (gating, list
//!    constraints, reuse of the `<Service>` validator), optional
//!    `environment`, and the "at least one of" constraint.
//!
//! The `Service.*` format-string scope and `WrappedService.*` are covered
//! in `test_service_scope.rs`.
//!
//! Error assertions follow the repo convention of asserting on the full
//! Pydantic-style error path + message.

use openjd_model::template::EnvironmentActions;
use openjd_model::{decode_environment_template, decode_job_template, CallerLimits};

/// Everything the Service-aware wrap hooks need.
const ALL_EXTS: &[&str] = &[
    "EXPR",
    "ENV_PARAMS",
    "SERVICE",
    "WRAP_ACTIONS",
    "FEATURE_BUNDLE_1",
];
/// `SERVICE` without `WRAP_ACTIONS`.
const SERVICE_EXTS: &[&str] = &["EXPR", "ENV_PARAMS", "SERVICE"];
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
// §4 item 3 — dependencies: gating and constraints
// ════════════════════════════════════════════════════════════════════

/// A Job Template with one Service `X` (so that `service:X` resolves) used
/// by its Step, one Job Environment (`job_env_body`, indented four spaces)
/// and one Step Environment (`step_env_body`, indented eight spaces). `X`
/// declines the `BY_DEFAULT` `JobEnv`: when `JobEnv` lists `X`, entering it
/// would be the cycle `X -> X` (§3.2 constraint 3).
fn job_template_with_service(job_env_body: &str, step_env_body: &str) -> String {
    format!(
        r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [SERVICE, ENV_PARAMS, EXPR]
name: Test
jobEnvironments:
  - name: JobEnv
{job_env_body}
services:
  - name: X
    dependencies: [{{ environment: JobEnv, enableIf: false }}]
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
/// `env_body` (indented two spaces), whose name must be `E`. `X` declines
/// `E`: when `E` lists `X`, entering it would be the cycle `X -> X` (§3.2
/// constraint 3).
fn env_template_with_service(env_body: &str) -> String {
    format!(
        r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, ENV_PARAMS, EXPR]
services:
  - name: X
    dependencies: [{{ environment: E, enableIf: false }}]
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
    // Gated in every position; the list is not examined.
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
             Service of that name in services or externalServices.",
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
            "SERVICE, ENV_PARAMS, EXPR",
            "  name: E\n  dependencies: [{ service: \"Store\" }]\n  variables: { P: \"{{ Service.Store.main.port }}\" }\n",
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
fn environment_dependencies_errors_are_reported_together() {
    // Every entry gets its own report (an unknown duplicate gets two).
    expect_job_err(
        &job_template_with_service(
            "    dependencies: [{ dependsOn: S }, { service: \"Nope\" }, { service: \"Nope\" }]\n    variables: { K: v }\n",
            "        variables: { K: v }\n",
        ),
        SERVICE_EXTS,
        &[
            "4 validation errors for JobTemplate\n",
            &format!("jobEnvironments[0] -> dependencies[0]:\n\t{STEP_NAME_MSG}"),
            "jobEnvironments[0] -> dependencies[1]:\n\tdependency 'service: Nope' not found",
            "jobEnvironments[0] -> dependencies[2]:\n\tdependency 'service: Nope' not found",
            "jobEnvironments[0] -> dependencies[2]:\n\tduplicate dependency 'service: Nope'.",
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
fn all_seven_wrap_hooks_accepted() {
    let et = expect_env_ok(
        &env_template(
            "SERVICE, ENV_PARAMS, EXPR, WRAP_ACTIONS",
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
            "SERVICE, ENV_PARAMS, EXPR",
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
            "SERVICE, ENV_PARAMS, EXPR",
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
            "SERVICE, ENV_PARAMS, EXPR, WRAP_ACTIONS, FEATURE_BUNDLE_1",
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
            "SERVICE, ENV_PARAMS, EXPR, WRAP_ACTIONS, FEATURE_BUNDLE_1",
            r#"  name: Wrapper
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
            "SERVICE, ENV_PARAMS, EXPR, WRAP_ACTIONS, FEATURE_BUNDLE_1",
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
            "SERVICE, ENV_PARAMS, EXPR, WRAP_ACTIONS",
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
// §4.3 — wrap hook rule 1: every action inside a wrapper is wrapped
// ════════════════════════════════════════════════════════════════════

/// A Job Template with `SERVICE` and `WRAP_ACTIONS` whose one Job
/// Environment `W` (`env_extra` YAML, indented four spaces, beneath `name`)
/// defines `hooks`, one Service `Store` (with `svc_deps`, a YAML flow list
/// of extra `dependencies` entries, possibly empty) and one Step `S` that
/// lists the Service and has the extra `step_deps` entries.
fn wrap_job(env_extra: &str, hooks: &[&str], svc_deps: &str, step_deps: &str) -> String {
    let svc_deps = if svc_deps.is_empty() {
        String::new()
    } else {
        format!("    dependencies: [{svc_deps}]\n")
    };
    format!(
        r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [SERVICE, ENV_PARAMS, EXPR, WRAP_ACTIONS]
name: Test
jobEnvironments:
  - name: W
{env_extra}{}
services:
  - name: Store
{svc_deps}    ports: [{{ name: main }}]
    healthCheck: {{ type: STDOUT }}
    script:
      actions:
        onRun: {{ command: run }}
steps:
  - name: S
    dependencies: [{{ service: Store }}{step_deps}]
    script:
      actions:
        onRun:
          command: run
"#,
        actions_body(hooks, 4)
    )
}

const TASK_MISSING: &str = "jobEnvironments[0] -> script -> actions:\n\tthe Sessions of Step 'S' \
     enter this wrapping environment, which therefore must define onWrapEnvEnter, onWrapTaskRun, \
     and onWrapEnvExit; missing: ";

const SERVICE_MISSING: &str = "jobEnvironments[0] -> script -> actions:\n\tthe Sessions of \
     Service 'Store' enter this wrapping environment, which therefore must define \
     onWrapEnvEnter, onWrapEnvExit, onWrapServiceEnter, onWrapServiceRun, \
     onWrapServiceHealthCheck, and onWrapServiceExit; missing: ";

#[test]
fn wrapper_entered_by_steps_and_services_needs_all_seven_hooks() {
    // An ALWAYS wrapper with no enableIf: every Step and the Service (which
    // it does not list) enter it.
    expect_job_ok(&wrap_job("", &all_seven(), "", ""), ALL_EXTS);
    expect_job_err(
        &wrap_job("", RFC0008_HOOKS, "", ""),
        ALL_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            &format!(
                "{SERVICE_MISSING}onWrapServiceEnter, onWrapServiceRun, \
                 onWrapServiceHealthCheck, onWrapServiceExit (RFC 0009 wrap hook rule 1)."
            ),
        ],
    );
    // Only the Service hooks: the Task Session case reports the Task hooks
    // and the Service Session case the Environment hooks.
    expect_job_err(
        &wrap_job("", SERVICE_HOOKS, "", ""),
        ALL_EXTS,
        &[
            "2 validation errors for JobTemplate\n",
            &format!(
                "{TASK_MISSING}onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit (RFC 0009 wrap hook \
                 rule 1)."
            ),
            &format!("{SERVICE_MISSING}onWrapEnvEnter, onWrapEnvExit (RFC 0009 wrap hook rule 1)."),
        ],
    );
}

#[test]
fn wrapper_entered_only_by_steps_needs_the_three_task_hooks_and_may_define_more() {
    // ON_REQUEST, requested by the Step only: no Service Session enters it.
    let env = "    activation: ON_REQUEST\n";
    let step = ", { environment: W }";
    expect_job_ok(&wrap_job(env, RFC0008_HOOKS, "", step), ALL_EXTS);
    expect_job_ok(&wrap_job(env, &all_seven(), "", step), ALL_EXTS);
    expect_job_err(
        &wrap_job(env, &["onWrapEnvEnter", "onWrapEnvExit"], "", step),
        ALL_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            &format!("{TASK_MISSING}onWrapTaskRun (RFC 0009 wrap hook rule 1)."),
        ],
    );
    // A BY_DEFAULT wrapper that lists the Service, which declines it (its
    // Sessions entering it would be the cycle Store -> Store): only the
    // Step's Sessions enter it.
    let listed = "    dependencies: [{ service: Store }]\n";
    let declined = "{ environment: W, enableIf: false }";
    expect_job_ok(&wrap_job(listed, RFC0008_HOOKS, declined, ""), ALL_EXTS);
}

#[test]
fn wrapper_entered_only_by_services_needs_the_six_service_hooks() {
    let env = "    activation: ON_REQUEST\n";
    let svc = "{ environment: W }";
    let six = with_service_hooks(ENV_HOOKS);
    expect_job_ok(&wrap_job(env, &six, svc, ""), ALL_EXTS);
    // A hook no Session that enters it runs is permitted.
    expect_job_ok(&wrap_job(env, &all_seven(), svc, ""), ALL_EXTS);
    expect_job_err(
        &wrap_job(env, RFC0008_HOOKS, svc, ""),
        ALL_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            &format!(
                "{SERVICE_MISSING}onWrapServiceEnter, onWrapServiceRun, \
                 onWrapServiceHealthCheck, onWrapServiceExit (RFC 0009 wrap hook rule 1)."
            ),
        ],
    );
}

#[test]
fn wrapper_nobody_is_known_to_enter_is_checked_at_job_creation() {
    // ON_REQUEST and requested by no one: no kind of Session is known to
    // enter it, so nothing is required here.
    expect_job_ok(
        &wrap_job("    activation: ON_REQUEST\n", &["onWrapTaskRun"], "", ""),
        ALL_EXTS,
    );
    // An entry whose enableIf is an expression decides it at job creation.
    expect_job_ok(
        &wrap_job(
            "    activation: ON_REQUEST\n",
            &["onWrapTaskRun"],
            "",
            ", { environment: W, enableIf: \"Job.Name == 'x'\" }",
        ),
        ALL_EXTS,
    );
}

#[test]
fn step_environment_wrapper_needs_the_three_task_hooks_and_may_define_service_hooks() {
    let job = |hooks: &[&str]| {
        job_template(
            "SERVICE, ENV_PARAMS, EXPR, WRAP_ACTIONS",
            "    variables: { K: v }\n",
            &actions_body(hooks, 8),
        )
    };
    expect_job_ok(&job(RFC0008_HOOKS), ALL_EXTS);
    expect_job_ok(&job(&all_seven()), ALL_EXTS);
    expect_job_err(
        &job(&with_service_hooks(ENV_HOOKS)),
        ALL_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "steps[0] -> stepEnvironments[0] -> script -> actions:\n\tthe Sessions of Step 'S' \
             enter this wrapping environment, which therefore must define onWrapEnvEnter, \
             onWrapTaskRun, and onWrapEnvExit; missing: onWrapTaskRun (RFC 0009 wrap hook rule \
             1).",
        ],
    );
}

#[test]
fn environment_template_wrapper_is_not_rejected_for_lacking_hooks_on_its_own() {
    // Which Sessions enter an attached Environment is known only for the
    // combined Job (Template Schemas §1.2.2 item 4).
    for hooks in [
        RFC0008_HOOKS,
        &with_service_hooks(ENV_HOOKS),
        &["onWrapTaskRun"],
    ] {
        expect_env_ok(
            &env_template(
                "SERVICE, ENV_PARAMS, EXPR, WRAP_ACTIONS",
                &format!("  name: Wrapper\n{}", actions_body(hooks, 2)),
            ),
            ALL_EXTS,
        );
    }
}

#[test]
fn without_service_the_rfc0008_all_or_nothing_rule_is_unchanged() {
    expect_env_err(
        &env_template(
            "WRAP_ACTIONS, EXPR",
            &format!("  name: Wrapper\n{}", actions_body(&["onWrapTaskRun"], 2)),
        ),
        WRAP_EXTS,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "environment -> script -> actions:\n\tan environment that defines any of \
             onWrapEnvEnter, onWrapTaskRun, or onWrapEnvExit must define all three (RFC 0008).",
        ],
    );
}

#[test]
fn a_service_session_holds_at_most_one_known_wrap_layer() {
    let template = format!(
        r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [SERVICE, ENV_PARAMS, EXPR, WRAP_ACTIONS]
name: Test
jobEnvironments:
  - name: A
    activation: ON_REQUEST
{}  - name: B
    activation: ON_REQUEST
{}services:
  - name: Store
    dependencies: [{{ environment: A }}, {{ environment: B }}]
    ports: [{{ name: main }}]
    healthCheck: {{ type: STDOUT }}
    script:
      actions:
        onRun: {{ command: run }}
steps:
  - name: S
    dependencies: [{{ service: Store }}]
    script:
      actions:
        onRun:
          command: run
"#,
        actions_body(&with_service_hooks(ENV_HOOKS), 4),
        actions_body(&with_service_hooks(ENV_HOOKS), 4),
    );
    expect_job_err(
        &template,
        ALL_EXTS,
        &[
            "1 validation error for JobTemplate\n",
            "services[0]:\n\tonly one environment in the session stack may define any of \
             onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit (RFC 0008).",
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
extensions: [SERVICE, ENV_PARAMS, EXPR]
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
        &env_template(
            "SERVICE, ENV_PARAMS, EXPR",
            "  name: E\n  variables: { K: v }\n",
        ),
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
extensions: [SERVICE, ENV_PARAMS, EXPR]
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
}

#[test]
fn env_template_with_neither_rejected() {
    expect_env_err(
        r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, ENV_PARAMS, EXPR]
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
            "extensions:\n\tSERVICE requires EXPR and ENV_PARAMS; EXPR and ENV_PARAMS must also be listed in the template's `extensions` (RFC 0009).",
        ],
    );
}

#[test]
fn env_template_services_must_not_be_empty() {
    expect_env_err(
        r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, ENV_PARAMS, EXPR]
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
extensions: [SERVICE, ENV_PARAMS, EXPR]
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
extensions: [SERVICE, ENV_PARAMS, EXPR]
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

/// §3.2 constraints 1–2 for an Environment Template's Services: no
/// self-dependency, no duplicate `service:` entry (`validation.md`
/// "Dependencies, environment template").
#[test]
fn env_template_service_dependencies_reject_self_and_duplicates() {
    let svc = |name: &str, deps: &str| {
        format!(
            "  - name: {name}\n    ports: [{{ name: main }}]\n    dependencies: {deps}\n    \
             script: {{ actions: {{ onRun: {{ command: x }} }} }}\n"
        )
    };
    expect_env_err(
        &format!(
            "specificationVersion: \"environment-2023-09\"\nextensions: [SERVICE, ENV_PARAMS, EXPR]\nservices:\n{}",
            svc("A", "[{ service: A }]")
        ),
        SERVICE_EXTS,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "services[0] -> dependencies[0]:\n\tcannot depend on itself.",
        ],
    );
    expect_env_err(
        &format!(
            "specificationVersion: \"environment-2023-09\"\nextensions: [SERVICE, ENV_PARAMS, EXPR]\nservices:\n{}{}",
            svc("A", "[{ service: B }]"),
            svc("B", "[{ service: A }, { service: A }]")
        )
        .replace("    dependencies: [{ service: B }]\n", ""),
        SERVICE_EXTS,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "services[1] -> dependencies[1]:\n\tduplicate dependency 'service: A'.",
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
extensions: [SERVICE, ENV_PARAMS, EXPR]
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
extensions: [SERVICE, ENV_PARAMS, EXPR]
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
extensions: [SERVICE, ENV_PARAMS, EXPR, WRAP_ACTIONS]
services:{MINIMAL_SERVICE}
environment:
  name: Wrapper
{}
"#,
            actions_body(RFC0008_HOOKS, 2)
        ),
        ALL_EXTS,
    );
}
