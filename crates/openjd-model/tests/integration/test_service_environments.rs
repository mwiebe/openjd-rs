// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Integration tests for the RFC 0009 `SERVICE` extension's changes to
//! `<Environment>`, `<EnvironmentActions>`, and the Environment Template
//! root (Template Schemas §1.2, §4 item 3, §4.3 WRAP_ACTIONS constraint 6,
//! §9.7 items 3 and 6).
//!
//! These tests cover:
//!
//! 1. **`runScope`** on `<Environment>`: schema parse, the `runs_in` /
//!    `effective_run_scope` accessors, extension gating, and the §4 item 3
//!    constraints (non-empty, recognized names, no duplicates) in job
//!    environments, step environments, and environment templates.
//! 2. **`onWrapService*` hooks** on `<EnvironmentActions>`: schema parse,
//!    gating on both `WRAP_ACTIONS` and `SERVICE`, and the default-timeout
//!    table.
//! 3. **Hooks follow `runScope`** (the RFC 0009 rule that replaces RFC
//!    0008's all-or-nothing rule when `SERVICE` is declared): every
//!    combination of default / `[TASK]` / `[SERVICE]` / `[TASK, SERVICE]`
//!    with missing and extra hooks, and the unchanged RFC 0008 rule when
//!    `SERVICE` is not declared.
//! 4. **Environment Template root**: `$schema`, `services` (gating, list
//!    constraints, reuse of the `<Service>` validator), optional
//!    `environment`, and the "at least one of" constraint.
//!
//! The `Service.*` format-string scope, `WrappedService.*`, and job creation
//! of Services are later milestones; nothing here depends on them.
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
    "onWrapServiceReadinessCheck",
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
// §4 item 3 — runScope: schema and accessors
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
fn run_scope_accepted_on_job_and_step_environments() {
    let jt = expect_job_ok(
        &job_template(
            "SERVICE, EXPR",
            "    runScope: [TASK]\n    variables: { K: v }\n",
            "        runScope: [SERVICE]\n        variables: { K: v }\n",
        ),
        SERVICE_EXTS,
    );
    let job_env = &jt.job_environments.as_ref().unwrap()[0];
    assert!(job_env.runs_in(RunScope::Task));
    assert!(!job_env.runs_in(RunScope::Service));
    let step_env = &jt.steps[0].step_environments.as_ref().unwrap()[0];
    assert!(!step_env.runs_in(RunScope::Task));
    assert!(step_env.runs_in(RunScope::Service));
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
// §4 item 3 — runScope: gating and constraints
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
            "steps[0] -> stepEnvironments[0] -> runScope[0]:\n\tunknown run scope name 'NOPE'; expected one of TASK, SERVICE.",
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
    assert!(actions.on_wrap_service_readiness_check.is_some());
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
        ("onWrapServiceReadinessCheck", Some(30)),
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
        EnvironmentActions::ON_WRAP_SERVICE_READINESS_CHECK_DEFAULT_TIMEOUT_SECONDS,
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
            "environment -> script -> actions -> onWrapServiceReadinessCheck:\n\tonWrapServiceReadinessCheck requires the WRAP_ACTIONS extension.",
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
            "environment -> script -> actions -> onWrapServiceReadinessCheck:\n\tonWrapServiceReadinessCheck requires the SERVICE extension.",
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
      onWrapServiceReadinessCheck: { command: "{{ WrappedAction.Command }}" }
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
      onWrapServiceReadinessCheck: { command: echo }
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
      onWrapServiceReadinessCheck: { command: echo }
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
      onWrapServiceReadinessCheck: { command: echo }
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

const SERVICE_GROUP_MISSING_ALL: &str = "a wrapping environment whose runScope includes SERVICE (default runScope: every kind of Session) must define onWrapServiceEnter, onWrapServiceRun, onWrapServiceReadinessCheck, and onWrapServiceExit; missing: onWrapServiceEnter, onWrapServiceRun, onWrapServiceReadinessCheck, onWrapServiceExit (RFC 0009).";

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
            "environment -> script -> actions -> onWrapServiceReadinessCheck:\n\tonWrapServiceReadinessCheck must not be defined: this environment's runScope (runScope: [TASK]) excludes SERVICE (RFC 0009).",
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
                        "onWrapServiceReadinessCheck"
                    ],
                    2
                )
            ),
        ),
        ALL_EXTS,
        &[
            "1 validation error for EnvironmentTemplate\n",
            "environment -> script -> actions:\n\ta wrapping environment whose runScope includes SERVICE (runScope: [SERVICE]) must define onWrapServiceEnter, onWrapServiceRun, onWrapServiceReadinessCheck, and onWrapServiceExit; missing: onWrapServiceEnter, onWrapServiceExit (RFC 0009).",
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
            "environment -> script -> actions:\n\ta wrapping environment whose runScope includes SERVICE (default runScope: every kind of Session) must define onWrapServiceEnter, onWrapServiceRun, onWrapServiceReadinessCheck, and onWrapServiceExit; missing: onWrapServiceEnter, onWrapServiceReadinessCheck, onWrapServiceExit (RFC 0009).",
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
    expect_job_err(
        &job_template(
            "SERVICE, EXPR, WRAP_ACTIONS",
            &format!(
                "    runScope: [TASK]\n{}",
                actions_body(&["onWrapEnvEnter", "onWrapEnvExit"], 4)
            ),
            &format!(
                "        runScope: [SERVICE]\n{}",
                actions_body(&all_seven(), 8)
            ),
        ),
        ALL_EXTS,
        &[
            "3 validation errors for JobTemplate\n",
            "jobEnvironments[0] -> script -> actions:\n\ta wrapping environment whose runScope includes TASK (runScope: [TASK]) must define onWrapTaskRun; missing: onWrapTaskRun (RFC 0009).",
            "steps[0] -> stepEnvironments[0] -> script -> actions -> onWrapTaskRun:\n\tonWrapTaskRun must not be defined: this environment's runScope (runScope: [SERVICE]) excludes TASK (RFC 0009).",
            // Two wrap layers in one session: the RFC 0008 single-layer rule
            // still applies alongside.
            "steps[0] -> stepEnvironments:\n\tonly one environment in the session stack may define any of onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit (RFC 0008).",
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
    // `jobServices`: the paths are rooted at `services[i]`.
    expect_env_err(
        r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, EXPR]
services:
  - name: File
    ports:
      - name: main
        port: 70000
    readinessCheck:
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
            "services[0] -> script -> actions:\n\tonReadinessCheck must be defined when readinessCheck.type is COMMAND.",
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
