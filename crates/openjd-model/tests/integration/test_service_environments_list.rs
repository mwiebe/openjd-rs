// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Integration tests for RFC 0009 `<Service>.serviceEnvironments` (Template
//! Schemas §9 item 5, §9.7 items 3 and 5, §4.3 WRAP_ACTIONS constraint 6,
//! §7.3.1 scope rule 1): the `Environment`s entered only in the declaring
//! Service's Session, the analogue of a Step's `stepEnvironments`.
//!
//! Covered here:
//!
//! 1. **Schema and pass 11**: the list parses on `template::Service`; if
//!    provided it is non-empty; names are unique within the list and distinct
//!    from the Job Environments and (Step Service) the declaring Step's Step
//!    Environments; `runScope` must not be provided; every entry gets the
//!    ordinary `<Environment>` structural checks.
//! 2. **Pass 10**: the hooks-follow-`runScope` rule with the effective
//!    `[SERVICE]`, and the single-wrap-layer rule over a Service Session's
//!    stack.
//! 3. **Pass 8**: a Service Environment has the declaring Service's own
//!    scope (its ports with `bindAddress`, earlier Services' `port` /
//!    `connectAddress`, `Session.*`, PATH `Param.*`, `Job.Name` /
//!    `Step.Name`, its own `Env.File.*` and `let`), never `Task.*`,
//!    `Service.File.*`, or a later Service; the same in an Environment
//!    Template's `services[k]`.
//! 4. **Job creation**: `job::Service::service_environments` is populated
//!    with a `resolved_symtab` per Environment, the carried-forward
//!    re-checks run with parameters bound, `referenced_service_names`
//!    sees a reference made only from a Service Environment, external
//!    Services carry theirs through `apply_environment_templates`, and the
//!    RFC's Valkey example (with its `ValkeyConda` Service Environment)
//!    validates and instantiates.
//!
//! Every negative case asserts the full Pydantic-style error path and
//! message.

use openjd_expr::ExprValue;
use openjd_model::job::service_symbols::referenced_service_names;
use openjd_model::job::{self, RunScope};
use openjd_model::template::EnvironmentTemplate;
use openjd_model::{
    apply_environment_templates, create_job, decode_environment_template, decode_job_template,
    AttachedEnvironmentTemplate, CallerLimits, JobParameterInputValues, ModelError,
};

const EXTS: &[&str] = &["EXPR", "SERVICE", "FEATURE_BUNDLE_1", "WRAP_ACTIONS"];

const RFC_VALKEY: &str = include_str!("../fixtures/rfc0009/valkey-shared-store.job.yaml");

fn yaml_val(s: &str) -> serde_json::Value {
    serde_saphyr::from_str(s).unwrap()
}

fn expect_job_err(template: &str, expected: &[&str]) {
    let err = decode_job_template(yaml_val(template), Some(EXTS), &CallerLimits::default())
        .expect_err("Expected validation error");
    let msg = err.to_string();
    for line in expected {
        assert!(
            msg.contains(line),
            "Missing expected substring {line:?} in error output:\n{msg}"
        );
    }
}

fn expect_job_ok(template: &str) -> openjd_model::template::JobTemplate {
    decode_job_template(yaml_val(template), Some(EXTS), &CallerLimits::default())
        .expect("expected successful decode")
}

fn expect_env_err(template: &str, expected: &[&str]) {
    let err = decode_environment_template(yaml_val(template), Some(EXTS), &CallerLimits::default())
        .expect_err("Expected validation error");
    let msg = err.to_string();
    for line in expected {
        assert!(
            msg.contains(line),
            "Missing expected substring {line:?} in error output:\n{msg}"
        );
    }
}

fn expect_env_ok(template: &str) -> EnvironmentTemplate {
    decode_environment_template(yaml_val(template), Some(EXTS), &CallerLimits::default())
        .expect("expected successful decode")
}

fn undefined(name: &str) -> String {
    format!("Undefined variable: '{name}'.")
}

fn indent(body: &str, n: usize) -> String {
    let pad = " ".repeat(n);
    body.lines()
        .map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                format!("{pad}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A job template with Job Services `A` (port `p`, with `serviceEnvironments`
/// `a_envs`) and `B` (port `q`, `b_envs`), one Job Environment `JobEnv`, and
/// a Step `S` with a Step Environment `StepEnv`, a Step Service `C` (port
/// `r`, `c_envs`), and a plain `onRun`. Each `*_envs` is the YAML body of
/// the `serviceEnvironments` list (its `- name:` items), or empty for none.
/// `step_extra` is inserted at the top of the Step (for `let`); `tail` is
/// appended to the document (for a second Step).
#[derive(Default)]
struct Tmpl<'a> {
    a_envs: &'a str,
    b_envs: &'a str,
    c_envs: &'a str,
    job_env: Option<&'a str>,
    step_env: Option<&'a str>,
    step_extra: &'a str,
    tail: &'a str,
}

fn template(t: &Tmpl<'_>) -> String {
    let list = |body: &str, n: usize| -> String {
        if body.is_empty() {
            String::new()
        } else {
            format!(
                "{}serviceEnvironments:\n{}\n",
                " ".repeat(n),
                indent(body, n + 2)
            )
        }
    };
    let job_env = t.job_env.unwrap_or("- name: JobEnv\n  variables: { K: v }");
    let step_env = t
        .step_env
        .unwrap_or("- name: StepEnv\n  variables: { K: v }");
    format!(
        r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [SERVICE, EXPR, FEATURE_BUNDLE_1, WRAP_ACTIONS]
name: Test
parameterDefinitions:
  - {{ name: Port, type: INT, default: 1 }}
  - {{ name: Dir, type: PATH, default: data }}
jobEnvironments:
{job_env}
jobServices:
  - name: A
{a_envs}    ports: [{{ name: p }}]
    script:
      actions:
        onRun:
          command: a
  - name: B
{b_envs}    ports: [{{ name: q }}]
    script:
      actions:
        onRun:
          command: b
steps:
  - name: S
{step_extra}
    stepEnvironments:
{step_env}
    stepServices:
      - name: C
{c_envs}        ports: [{{ name: r }}]
        script:
          actions:
            onRun:
              command: c
    script:
      actions:
        onRun:
          command: run
{tail}
"#,
        job_env = indent(job_env, 2),
        step_env = indent(step_env, 6),
        a_envs = list(t.a_envs, 4),
        b_envs = list(t.b_envs, 4),
        c_envs = list(t.c_envs, 8),
        step_extra = indent(t.step_extra, 4),
        tail = t.tail,
    )
}

/// An Environment whose `onEnter` has the given args.
fn env_with_args(name: &str, args: &str) -> String {
    format!(
        "- name: {name}\n  script:\n    actions:\n      onEnter:\n        command: setup\n        args: {args}"
    )
}

// ════════════════════════════════════════════════════════════════════
// §9 item 5: schema and pass 11 structure
// ════════════════════════════════════════════════════════════════════

#[test]
fn service_environments_parse_into_the_template_in_order() {
    let jt = expect_job_ok(&template(&Tmpl {
        a_envs: "- name: First\n  variables: { X: y }\n- name: Second\n  script:\n    actions:\n      onExit: { command: bye }",
        c_envs: "- name: StepSvcEnv\n  variables: { X: y }",
        ..Default::default()
    }));
    let a = &jt.job_services.as_ref().unwrap()[0];
    let names: Vec<&str> = a
        .service_environments
        .as_ref()
        .unwrap()
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(names, vec!["First", "Second"]);
    assert!(jt.job_services.as_ref().unwrap()[1]
        .service_environments
        .is_none());
    let c = &jt.steps[0].step_services.as_ref().unwrap()[0];
    assert_eq!(
        c.service_environments.as_ref().unwrap()[0].name,
        "StepSvcEnv"
    );
}

#[test]
fn service_environments_if_provided_must_not_be_empty() {
    let tmpl = template(&Tmpl::default()).replace(
        "  - name: A\n    ports",
        "  - name: A\n    serviceEnvironments: []\n    ports",
    );
    expect_job_err(
        &tmpl,
        &[
            "1 validation error for JobTemplate\n",
            "jobServices[0] -> serviceEnvironments:\n\tmust not be empty.",
        ],
    );
}

#[test]
fn duplicate_name_within_the_list() {
    expect_job_err(
        &template(&Tmpl {
            a_envs: "- name: Conda\n  variables: { X: y }\n- name: Conda\n  variables: { X: z }",
            ..Default::default()
        }),
        &[
            "1 validation error for JobTemplate\n",
            "jobServices[0] -> serviceEnvironments[1] -> name:\n\tduplicate environment name: 'Conda'",
        ],
    );
}

#[test]
fn service_environment_may_not_reuse_a_job_environment_name() {
    expect_job_err(
        &template(&Tmpl {
            b_envs: "- name: JobEnv\n  variables: { X: y }",
            ..Default::default()
        }),
        &[
            "1 validation error for JobTemplate\n",
            "jobServices[1] -> serviceEnvironments[0] -> name:\n\tduplicate environment name: 'JobEnv'",
        ],
    );
}

#[test]
fn step_service_environment_may_not_reuse_a_job_or_step_environment_name() {
    expect_job_err(
        &template(&Tmpl {
            c_envs: "- name: JobEnv\n  variables: { X: y }\n- name: StepEnv\n  variables: { X: y }",
            ..Default::default()
        }),
        &[
            "2 validation errors for JobTemplate\n",
            "steps[0] -> stepServices[0] -> serviceEnvironments[0] -> name:\n\tduplicate environment name: 'JobEnv'",
            "steps[0] -> stepServices[0] -> serviceEnvironments[1] -> name:\n\tduplicate environment name: 'StepEnv'",
        ],
    );
}

#[test]
fn different_services_and_other_steps_environments_may_share_a_name() {
    // The same Service Environment name in two Services; a Job Service's
    // Service Environment named like a Step Environment (a Job Service's
    // Session never enters any Step's Environments); and a Step Service's
    // Service Environment named like another Step's Step Environment.
    expect_job_ok(&template(&Tmpl {
        a_envs: "- name: Shared\n  variables: { X: y }\n- name: StepEnv\n  variables: { X: y }",
        b_envs: "- name: Shared\n  variables: { X: y }",
        c_envs:
            "- name: Shared\n  variables: { X: y }\n- name: OtherStepEnv\n  variables: { X: y }",
        tail: r#"  - name: Other
    stepEnvironments:
      - name: OtherStepEnv
        variables: { K: v }
    script:
      actions:
        onRun:
          command: run"#,
        ..Default::default()
    }));
}

#[test]
fn run_scope_must_not_be_provided_on_a_service_environment() {
    expect_job_err(
        &template(&Tmpl {
            a_envs: "- name: Conda\n  runScope: [SERVICE]\n  variables: { X: y }",
            c_envs: "- name: Conda\n  runScope: [TASK, SERVICE]\n  variables: { X: y }",
            ..Default::default()
        }),
        &[
            "2 validation errors for JobTemplate\n",
            "jobServices[0] -> serviceEnvironments[0] -> runScope:\n\tmust not be provided on a Service Environment: its scope is fixed to the declaring Service's Session (RFC 0009).",
            "steps[0] -> stepServices[0] -> serviceEnvironments[0] -> runScope:\n\tmust not be provided on a Service Environment: its scope is fixed to the declaring Service's Session (RFC 0009).",
        ],
    );
}

#[test]
fn service_environment_gets_the_ordinary_environment_structure_checks() {
    expect_job_err(
        &template(&Tmpl {
            a_envs: "- name: Empty\n- name: NoActions\n  script:\n    actions: {}\n- name: \"\"\n  variables: { X: y }",
            ..Default::default()
        }),
        &[
            "3 validation errors for JobTemplate\n",
            "jobServices[0] -> serviceEnvironments[0]:\n\tmust have at least one of 'script' or 'variables'.",
            "jobServices[0] -> serviceEnvironments[1] -> script -> actions:\n\tmust define at least one of onEnter or onExit, or the complete set of wrap hooks",
            "jobServices[0] -> serviceEnvironments[2] -> name:\n\tmust not be empty.",
        ],
    );
}

#[test]
fn service_environment_embedded_file_end_of_line_requires_feature_bundle_1() {
    let tmpl = template(&Tmpl {
        a_envs: "- name: Files\n  script:\n    actions:\n      onEnter: { command: setup }\n    embeddedFiles:\n      - { name: F, type: TEXT, data: hi, endOfLine: LF }",
        ..Default::default()
    })
    .replace(
        "extensions: [SERVICE, EXPR, FEATURE_BUNDLE_1, WRAP_ACTIONS]",
        "extensions: [SERVICE, EXPR, WRAP_ACTIONS]",
    );
    expect_job_err(
        &tmpl,
        &[
            "1 validation error for JobTemplate\n",
            "jobServices[0] -> serviceEnvironments[0] -> script -> embeddedFiles[0] -> endOfLine:\n\trequires the FEATURE_BUNDLE_1 extension.",
        ],
    );
}

// ════════════════════════════════════════════════════════════════════
// §4.3 constraint 6 with the effective runScope [SERVICE] (pass 10)
// ════════════════════════════════════════════════════════════════════

/// A wrapping Environment defining exactly `hooks`.
fn wrapper(name: &str, hooks: &[&str]) -> String {
    let mut body = format!("- name: {name}\n  script:\n    actions:\n");
    for hook in hooks {
        body.push_str(&format!(
            "      {hook}:\n        command: wrap\n        args: [\"{{{{ WrappedAction.Command }}}}\"]\n"
        ));
    }
    body.trim_end().to_string()
}

const SERVICE_WRAPPER_HOOKS: &[&str] = &[
    "onWrapEnvEnter",
    "onWrapEnvExit",
    "onWrapServiceEnter",
    "onWrapServiceRun",
    "onWrapServiceReadinessCheck",
    "onWrapServiceExit",
];

const SCOPE_TEXT: &str = "effective runScope: [SERVICE], a Service Environment";

#[test]
fn wrapping_service_environment_defines_the_service_session_hooks() {
    expect_job_ok(&template(&Tmpl {
        a_envs: &wrapper("Container", SERVICE_WRAPPER_HOOKS),
        c_envs: &wrapper("Container", SERVICE_WRAPPER_HOOKS),
        ..Default::default()
    }));
}

#[test]
fn wrapping_service_environment_must_not_define_on_wrap_task_run() {
    let mut hooks = SERVICE_WRAPPER_HOOKS.to_vec();
    hooks.push("onWrapTaskRun");
    expect_job_err(
        &template(&Tmpl {
            a_envs: &wrapper("Container", &hooks),
            ..Default::default()
        }),
        &[
            "1 validation error for JobTemplate\n",
            &format!(
                "jobServices[0] -> serviceEnvironments[0] -> script -> actions -> onWrapTaskRun:\n\tonWrapTaskRun must not be defined: this environment's runScope ({SCOPE_TEXT}) excludes TASK (RFC 0009)."
            ),
        ],
    );
}

#[test]
fn wrapping_service_environment_must_define_all_four_service_hooks() {
    // RFC 0008's three hooks alone: onWrapTaskRun is not called for, and
    // the four Service hooks are missing.
    expect_job_err(
        &template(&Tmpl {
            c_envs: &wrapper(
                "Container",
                &["onWrapEnvEnter", "onWrapTaskRun", "onWrapEnvExit"],
            ),
            ..Default::default()
        }),
        &[
            "2 validation errors for JobTemplate\n",
            &format!(
                "steps[0] -> stepServices[0] -> serviceEnvironments[0] -> script -> actions:\n\ta wrapping environment whose runScope includes SERVICE ({SCOPE_TEXT}) must define onWrapServiceEnter, onWrapServiceRun, onWrapServiceReadinessCheck, and onWrapServiceExit; missing: onWrapServiceEnter, onWrapServiceRun, onWrapServiceReadinessCheck, onWrapServiceExit (RFC 0009)."
            ),
            &format!(
                "steps[0] -> stepServices[0] -> serviceEnvironments[0] -> script -> actions -> onWrapTaskRun:\n\tonWrapTaskRun must not be defined: this environment's runScope ({SCOPE_TEXT}) excludes TASK (RFC 0009)."
            ),
        ],
    );
    // One Service hook missing, and the environment hooks missing.
    expect_job_err(
        &template(&Tmpl {
            a_envs: &wrapper(
                "Container",
                &[
                    "onWrapServiceEnter",
                    "onWrapServiceRun",
                    "onWrapServiceReadinessCheck",
                ],
            ),
            ..Default::default()
        }),
        &[
            "2 validation errors for JobTemplate\n",
            "jobServices[0] -> serviceEnvironments[0] -> script -> actions:\n\ta wrapping environment must define onWrapEnvEnter and onWrapEnvExit whatever its runScope; missing: onWrapEnvEnter, onWrapEnvExit (RFC 0009).",
            &format!(
                "jobServices[0] -> serviceEnvironments[0] -> script -> actions:\n\ta wrapping environment whose runScope includes SERVICE ({SCOPE_TEXT}) must define onWrapServiceEnter, onWrapServiceRun, onWrapServiceReadinessCheck, and onWrapServiceExit; missing: onWrapServiceExit (RFC 0009)."
            ),
        ],
    );
}

const SERVICE_SESSION_SINGLE_LAYER: &str = "only one environment in a Service Session's stack (the scope's environments whose runScope includes SERVICE, then this Service's serviceEnvironments) may define any wrap hook (RFC 0008, RFC 0009).";

#[test]
fn only_one_wrap_layer_per_service_session() {
    // Two wrapping Service Environments in one Service.
    let two = format!(
        "{}\n{}",
        wrapper("Outer", SERVICE_WRAPPER_HOOKS),
        wrapper("Inner", SERVICE_WRAPPER_HOOKS)
    );
    expect_job_err(
        &template(&Tmpl {
            a_envs: &two,
            ..Default::default()
        }),
        &[
            "1 validation error for JobTemplate\n",
            &format!("jobServices[0] -> serviceEnvironments:\n\t{SERVICE_SESSION_SINGLE_LAYER}"),
        ],
    );
    // A wrapping Job Environment entered in Service Sessions (the default
    // runScope) plus a wrapping Service Environment: two layers in the
    // Service Session of every Service that declares one.
    let job_wrapper = wrapper(
        "JobEnv",
        &[
            "onWrapEnvEnter",
            "onWrapTaskRun",
            "onWrapEnvExit",
            "onWrapServiceEnter",
            "onWrapServiceRun",
            "onWrapServiceReadinessCheck",
            "onWrapServiceExit",
        ],
    );
    expect_job_err(
        &template(&Tmpl {
            job_env: Some(&job_wrapper),
            b_envs: &wrapper("Container", SERVICE_WRAPPER_HOOKS),
            c_envs: &wrapper("Container", SERVICE_WRAPPER_HOOKS),
            ..Default::default()
        }),
        &[
            "2 validation errors for JobTemplate\n",
            &format!("jobServices[1] -> serviceEnvironments:\n\t{SERVICE_SESSION_SINGLE_LAYER}"),
            &format!(
                "steps[0] -> stepServices[0] -> serviceEnvironments:\n\t{SERVICE_SESSION_SINGLE_LAYER}"
            ),
        ],
    );
    // A Step Environment wrapping Service Sessions is in a Step Service's
    // stack, not a Job Service's.
    let step_wrapper = format!(
        "{}\n  runScope: [SERVICE]",
        wrapper("StepEnv", SERVICE_WRAPPER_HOOKS)
    );
    expect_job_err(
        &template(&Tmpl {
            step_env: Some(&step_wrapper),
            a_envs: &wrapper("Container", SERVICE_WRAPPER_HOOKS),
            c_envs: &wrapper("Container", SERVICE_WRAPPER_HOOKS),
            ..Default::default()
        }),
        &[
            "1 validation error for JobTemplate\n",
            &format!(
                "steps[0] -> stepServices[0] -> serviceEnvironments:\n\t{SERVICE_SESSION_SINGLE_LAYER}"
            ),
        ],
    );
}

#[test]
fn a_task_only_wrapper_in_scope_is_not_a_service_session_layer() {
    // A wrapping Job Environment with runScope [TASK] is never entered in a
    // Service Session, so a wrapping Service Environment is the only layer.
    let task_wrapper = format!(
        "{}\n  runScope: [TASK]",
        wrapper(
            "JobEnv",
            &["onWrapEnvEnter", "onWrapTaskRun", "onWrapEnvExit"]
        )
    );
    expect_job_ok(&template(&Tmpl {
        job_env: Some(&task_wrapper),
        a_envs: &wrapper("Container", SERVICE_WRAPPER_HOOKS),
        c_envs: &wrapper("Container", SERVICE_WRAPPER_HOOKS),
        ..Default::default()
    }));
}

// ════════════════════════════════════════════════════════════════════
// §7.3.1 scope rule 1 / §9 item 5: the declaring Service's own scope (pass 8)
// ════════════════════════════════════════════════════════════════════

#[test]
fn service_environment_sees_its_own_service_including_bind_address() {
    expect_job_ok(&template(&Tmpl {
        a_envs: &env_with_args(
            "Conda",
            r#"["{{ Service.A.p.port }}", "{{ Service.A.p.bindAddress }}", "{{ Service.A.p.connectAddress }}"]"#,
        ),
        c_envs: &env_with_args(
            "Conda",
            r#"["{{ Service.C.r.bindAddress }}", "{{ join_host_port(Service.C.r.connectAddress, Service.C.r.port) }}"]"#,
        ),
        ..Default::default()
    }));
}

#[test]
fn service_environment_sees_earlier_services_but_not_their_bind_address() {
    expect_job_ok(&template(&Tmpl {
        b_envs: &env_with_args(
            "Conda",
            r#"["{{ Service.A.p.port }}", "{{ Service.A.p.connectAddress }}"]"#,
        ),
        c_envs: &env_with_args(
            "Conda",
            r#"["{{ Service.A.p.port }}", "{{ Service.B.q.connectAddress }}"]"#,
        ),
        ..Default::default()
    }));
    expect_job_err(
        &template(&Tmpl {
            b_envs: &env_with_args("Conda", r#"["{{ Service.A.p.bindAddress }}"]"#),
            ..Default::default()
        }),
        &[
            "1 validation error for JobTemplate\n",
            "jobServices[1] -> serviceEnvironments[0] -> script -> actions -> onEnter -> args[0]:\n\tFailed to parse interpolation expression at [",
            &undefined("Service.A.p.bindAddress"),
        ],
    );
}

#[test]
fn service_environment_cannot_see_a_later_service_or_another_steps_service() {
    expect_job_err(
        &template(&Tmpl {
            a_envs: &env_with_args("Conda", r#"["{{ Service.B.q.port }}", "{{ Service.C.r.port }}"]"#),
            ..Default::default()
        }),
        &[
            "2 validation errors for JobTemplate\n",
            "jobServices[0] -> serviceEnvironments[0] -> script -> actions -> onEnter -> args[0]:\n\tFailed to parse interpolation expression at [",
            &undefined("Service.B.q.port"),
            "jobServices[0] -> serviceEnvironments[0] -> script -> actions -> onEnter -> args[1]:\n\tFailed to parse interpolation expression at [",
            &undefined("Service.C.r.port"),
        ],
    );
    // A Step Service's Service Environment cannot see another Step's Service.
    let tmpl = template(&Tmpl {
        c_envs: &env_with_args("Conda", r#"["{{ Service.D.s.port }}"]"#),
        tail: r#"  - name: Other
    stepServices:
      - name: D
        ports: [{ name: s }]
        script:
          actions:
            onRun:
              command: d
    script:
      actions:
        onRun:
          command: run"#,
        ..Default::default()
    });
    expect_job_err(
        &tmpl,
        &[
            "1 validation error for JobTemplate\n",
            "steps[0] -> stepServices[0] -> serviceEnvironments[0] -> script -> actions -> onEnter -> args[0]:\n\tFailed to parse interpolation expression at [",
            &undefined("Service.D.s.port"),
        ],
    );
}

#[test]
fn service_environment_sees_session_params_names_and_its_own_files_and_let() {
    expect_job_ok(&template(&Tmpl {
        a_envs: r#"- name: Conda
  variables:
    WD: "{{ Session.WorkingDirectory }}"
    HAS: "{{ Session.HasPathMappingRules }}"
    PORT: "{{ Param.Port + Service.A.p.port }}"
    DIR: "{{ Param.Dir }}"
    JOB: "{{ Job.Name }}"
  script:
    let:
      - url = 'http://' + join_host_port(Service.A.p.connectAddress, Service.A.p.port)
    actions:
      onEnter:
        command: bash
        args: ["{{ Env.File.Setup }}", "{{ url }}", "{{ Param.Dir }}"]
      onExit:
        command: rm
        args: ["{{ Env.File.Setup }}"]
    embeddedFiles:
      - name: Setup
        type: TEXT
        data: "bind {{ Service.A.p.bindAddress }} in {{ Session.WorkingDirectory }} for {{ Job.Name }}""#,
        c_envs: r#"- name: Conda
  variables:
    STEP: "{{ Step.Name }}"
    N: "{{ n }}"
  script:
    actions:
      onEnter:
        command: setup
        args: ["{{ Service.C.r.bindAddress }}", "{{ n }}"]"#,
        step_extra: "let:\n  - n = Param.Port * 2",
        ..Default::default()
    }));
}

#[test]
fn service_environment_never_sees_task_service_file_or_service_let_names() {
    expect_job_err(
        &template(&Tmpl {
            a_envs: &env_with_args(
                "Conda",
                r#"["{{ Task.Param.Frame }}", "{{ Service.File.Conf }}", "{{ svc_let }}"]"#,
            ),
            ..Default::default()
        })
        .replace(
            "  - name: A\n",
            "  - name: A\n    let:\n      - svc_let = 1\n",
        )
        .replace(
            "        onRun:\n          command: a\n",
            "        onRun:\n          command: a\n      embeddedFiles:\n        - { name: Conf, type: TEXT, data: x }\n",
        ),
        &[
            "3 validation errors for JobTemplate\n",
            "jobServices[0] -> serviceEnvironments[0] -> script -> actions -> onEnter -> args[0]:\n\tFailed to parse interpolation expression at [",
            &undefined("Task.Param.Frame"),
            "jobServices[0] -> serviceEnvironments[0] -> script -> actions -> onEnter -> args[1]:\n\tFailed to parse interpolation expression at [",
            &undefined("Service.File.Conf"),
            "jobServices[0] -> serviceEnvironments[0] -> script -> actions -> onEnter -> args[2]:\n\tFailed to parse interpolation expression at [",
            &undefined("svc_let"),
        ],
    );
}

#[test]
fn job_service_environment_does_not_see_step_name() {
    expect_job_err(
        &template(&Tmpl {
            a_envs: &env_with_args("Conda", r#"["{{ Step.Name }}"]"#),
            ..Default::default()
        }),
        &[
            "1 validation error for JobTemplate\n",
            "jobServices[0] -> serviceEnvironments[0] -> script -> actions -> onEnter -> args[0]:\n\tFailed to parse interpolation expression at [",
            &undefined("Step.Name"),
        ],
    );
}

#[test]
fn service_environment_timing_fields_resolve_at_job_creation() {
    // `timeout` is job-creation scope: Param.* (not PATH), Job.Name, and
    // for a Step Service the step-level let; never Session.* or Service.*.
    expect_job_ok(&template(&Tmpl {
        a_envs: r#"- name: Conda
  script:
    actions:
      onEnter:
        command: setup
        timeout: "{{ Param.Port * 10 }}""#,
        c_envs: r#"- name: Conda
  script:
    actions:
      onEnter:
        command: setup
        timeout: "{{ n }}""#,
        step_extra: "let:\n  - n = Param.Port * 2",
        ..Default::default()
    }));
    expect_job_err(
        &template(&Tmpl {
            a_envs: r#"- name: Conda
  script:
    actions:
      onEnter:
        command: setup
        timeout: "{{ Service.A.p.port }}""#,
            ..Default::default()
        }),
        &[
            "1 validation error for JobTemplate\n",
            "jobServices[0] -> serviceEnvironments[0] -> script -> actions -> onEnter -> timeout:\n\tFailed to parse interpolation expression at [",
            &undefined("Service.A.p.port"),
        ],
    );
}

#[test]
fn wrapping_service_environment_sees_wrapped_service_and_its_own_service() {
    // Unlike a Job or Step Environment entered in Service Sessions (§4 item
    // 3.2), a Service Environment's hooks see the declaring Service.
    let hooks = r#"- name: Container
  script:
    actions:
      onWrapEnvEnter: { command: w, args: ["{{ WrappedAction.Command }}", "{{ WrappedEnv.Name }}"] }
      onWrapEnvExit: { command: w, args: ["{{ WrappedAction.Command }}"] }
      onWrapServiceEnter: { command: w, args: ["{{ WrappedService.Name }}", "{{ Service.A.p.bindAddress }}"] }
      onWrapServiceRun: { command: w, args: ["{{ [string(p) for p in WrappedService.Ports].join(',') }}", "{{ Service.A.p.port }}"] }
      onWrapServiceReadinessCheck: { command: w, args: ["{{ WrappedAction.Command }}"] }
      onWrapServiceExit: { command: w, args: ["{{ WrappedService.BindAddresses[0] }}"] }"#;
    expect_job_ok(&template(&Tmpl {
        a_envs: hooks,
        ..Default::default()
    }));
}

#[test]
fn other_services_see_a_service_environment_only_through_its_service() {
    // Service B cannot reach A's Service Environment files; it sees A's
    // ports (§7.3.1 scope rule 2).
    expect_job_err(
        &template(&Tmpl {
            a_envs: r#"- name: Conda
  script:
    actions:
      onEnter: { command: setup, args: ["{{ Env.File.Setup }}"] }
    embeddedFiles:
      - { name: Setup, type: TEXT, data: x }"#,
            ..Default::default()
        })
        .replace(
            "        onRun:\n          command: b\n",
            "        onRun:\n          command: b\n          args: [\"{{ Service.A.p.port }}\", \"{{ Env.File.Setup }}\"]\n",
        ),
        &[
            "1 validation error for JobTemplate\n",
            "jobServices[1] -> script -> actions -> onRun -> args[1]:\n\tFailed to parse interpolation expression at [",
            &undefined("Env.File.Setup"),
        ],
    );
}

#[test]
fn job_environment_in_service_sessions_still_cannot_reference_services() {
    // §4 item 3.2 is unchanged for Job and Step Environments; only a
    // Service Environment is the exception.
    expect_job_err(
        &template(&Tmpl {
            job_env: Some(&env_with_args("JobEnv", r#"["{{ Service.A.p.port }}"]"#)),
            ..Default::default()
        }),
        &[
            "1 validation error for JobTemplate\n",
            "jobEnvironments[0] -> script -> actions -> onEnter -> args[0]:\n\tFailed to parse interpolation expression at [",
            &undefined("Service.A.p.port"),
        ],
    );
}

// ════════════════════════════════════════════════════════════════════
// Environment Template `services[k].serviceEnvironments` (§1.2.2)
// ════════════════════════════════════════════════════════════════════

fn env_template(first_envs: &str, second_envs: &str, environment: &str) -> String {
    let list = |body: &str| -> String {
        if body.is_empty() {
            String::new()
        } else {
            format!("    serviceEnvironments:\n{}\n", indent(body, 6))
        }
    };
    format!(
        r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, EXPR, FEATURE_BUNDLE_1, WRAP_ACTIONS]
parameterDefinitions:
  - {{ name: Size, type: INT, default: 1 }}
services:
  - name: First
{first}    ports: [{{ name: main }}]
    script:
      actions:
        onRun: {{ command: first }}
  - name: Second
{second}    ports: [{{ name: main }}]
    script:
      actions:
        onRun: {{ command: second }}
{environment}
"#,
        first = list(first_envs),
        second = list(second_envs),
    )
}

#[test]
fn environment_template_service_environments_have_the_documents_scope() {
    let et = expect_env_ok(&env_template(
        &env_with_args(
            "Conda",
            r#"["{{ Service.First.main.bindAddress }}", "{{ Param.Size }}", "{{ Job.Name }}", "{{ Session.WorkingDirectory }}"]"#,
        ),
        &env_with_args(
            "Conda",
            r#"["{{ Service.First.main.connectAddress }}", "{{ Service.Second.main.bindAddress }}"]"#,
        ),
        "",
    ));
    assert_eq!(
        et.services()[0].service_environments.as_ref().unwrap()[0].name,
        "Conda"
    );
    expect_env_err(
        &env_template(
            &env_with_args(
                "Conda",
                r#"["{{ Service.Second.main.port }}", "{{ Step.Name }}"]"#,
            ),
            "",
            "",
        ),
        &[
            "2 validation errors for EnvironmentTemplate\n",
            "services[0] -> serviceEnvironments[0] -> script -> actions -> onEnter -> args[0]:\n\tFailed to parse interpolation expression at [",
            &undefined("Service.Second.main.port"),
            "services[0] -> serviceEnvironments[0] -> script -> actions -> onEnter -> args[1]:\n\tFailed to parse interpolation expression at [",
            &undefined("Step.Name"),
        ],
    );
}

#[test]
fn environment_template_service_environments_structure_rules() {
    // Duplicate within the list, the document's own environment's name
    // (a Job Environment of every Job it is attached to), and runScope.
    expect_env_err(
        &env_template(
            "- name: Conda\n  variables: { X: y }\n- name: Conda\n  variables: { X: z }\n- name: Queue\n  runScope: [SERVICE]\n  variables: { X: y }",
            "",
            "environment:\n  name: Queue\n  variables: { K: v }",
        ),
        &[
            "3 validation errors for EnvironmentTemplate\n",
            "services[0] -> serviceEnvironments[1] -> name:\n\tduplicate environment name: 'Conda'",
            "services[0] -> serviceEnvironments[2] -> name:\n\tduplicate environment name: 'Queue'",
            "services[0] -> serviceEnvironments[2] -> runScope:\n\tmust not be provided on a Service Environment: its scope is fixed to the declaring Service's Session (RFC 0009).",
        ],
    );
    // The hooks-follow-runScope rule and the single layer per Service
    // Session, counting the document's own environment when it is entered in
    // Service Sessions.
    let doc_wrapper = wrapper(
        "Queue",
        &[
            "onWrapEnvEnter",
            "onWrapTaskRun",
            "onWrapEnvExit",
            "onWrapServiceEnter",
            "onWrapServiceRun",
            "onWrapServiceReadinessCheck",
            "onWrapServiceExit",
        ],
    );
    let mut with_task_run = SERVICE_WRAPPER_HOOKS.to_vec();
    with_task_run.push("onWrapTaskRun");
    expect_env_err(
        &env_template(
            &wrapper("Container", SERVICE_WRAPPER_HOOKS),
            &wrapper("Container", &with_task_run),
            &format!("environment:\n{}", doc_wrapper.replacen("- name", "  name", 1)),
        ),
        &[
            "3 validation errors for EnvironmentTemplate\n",
            &format!("services[0] -> serviceEnvironments:\n\t{SERVICE_SESSION_SINGLE_LAYER}"),
            &format!(
                "services[1] -> serviceEnvironments[0] -> script -> actions -> onWrapTaskRun:\n\tonWrapTaskRun must not be defined: this environment's runScope ({SCOPE_TEXT}) excludes TASK (RFC 0009)."
            ),
            &format!("services[1] -> serviceEnvironments:\n\t{SERVICE_SESSION_SINGLE_LAYER}"),
        ],
    );
}

// ════════════════════════════════════════════════════════════════════
// Job creation
// ════════════════════════════════════════════════════════════════════

fn create(template: &str, params: &[(&str, &str)]) -> Result<job::Job, ModelError> {
    let jt = expect_job_ok(template);
    let td = tempfile::TempDir::new().unwrap();
    let dir = td.path().to_str().unwrap();
    let input: JobParameterInputValues = params
        .iter()
        .map(|(k, v)| (k.to_string(), ExprValue::String(v.to_string())))
        .collect();
    let processed = openjd_model::preprocess_job_parameters(
        &jt,
        &input,
        &[],
        &openjd_model::PathParameterOptions::new(dir, dir),
    )
    .unwrap();
    create_job(&jt, &processed, &jt.default_validation_context())
}

fn create_ok(template: &str, params: &[(&str, &str)]) -> job::Job {
    create(template, params).expect("job creation should succeed")
}

fn symtab_of(st: &openjd_expr::SerializedSymbolTable) -> openjd_expr::SymbolTable {
    st.to_symtab(openjd_expr::path_mapping::PathFormat::host())
        .expect("symtab")
}

#[test]
fn job_service_service_environments_are_populated_with_resolved_symtabs() {
    let job = create_ok(
        &template(&Tmpl {
            a_envs: r#"- name: Conda
  variables:
    PORT: "{{ Param.Port + Service.A.p.port }}"
    DIR: "{{ Param.Dir }}"
  script:
    actions:
      onEnter: { command: setup, args: ["{{ Job.Name }}", "{{ Service.A.p.bindAddress }}"] }
- name: Plain
  variables: { K: v }"#,
            c_envs: r#"- name: Conda
  script:
    actions:
      onEnter: { command: setup, args: ["{{ Step.Name }}", "{{ n }}", "{{ Service.C.r.bindAddress }}"] }"#,
            step_extra: "let:\n  - n = Param.Port * 2",
            ..Default::default()
        }),
        &[("Port", "21")],
    );

    let a = &job.job_services.as_ref().unwrap()[0];
    let envs = a
        .service_environments
        .as_ref()
        .expect("serviceEnvironments");
    assert_eq!(envs.len(), 2);
    assert_eq!(envs[0].name, "Conda");
    assert_eq!(envs[0].run_scope, None);
    assert!(envs[0].runs_in(RunScope::Service));
    // The host-scope format strings are carried forward verbatim.
    let vars = envs[0].variables.as_ref().unwrap();
    assert_eq!(vars["PORT"].raw(), "{{ Param.Port + Service.A.p.port }}");
    // The resolved symtab holds exactly what the Environment references of
    // the job-creation table (Param.Port, RawParam.Dir for the PATH
    // parameter, Job.Name), as a Job Environment's does.
    let st = symtab_of(envs[0].resolved_symtab.as_ref().expect("resolvedSymTab"));
    assert_eq!(st.get_value("Param.Port"), Some(&ExprValue::Int(21)));
    assert_eq!(
        st.get_value("Job.Name"),
        Some(&ExprValue::String("Test".into()))
    );
    assert!(st.contains("RawParam.Dir"));
    assert!(
        !st.contains("Service.A.p.port"),
        "bound by the Service Session"
    );
    // An Environment that references nothing has an empty table.
    let plain = symtab_of(envs[1].resolved_symtab.as_ref().unwrap());
    assert_eq!(plain.keys().count(), 0);
    assert!(job.job_services.as_ref().unwrap()[1]
        .service_environments
        .is_none());

    // A Step Service's Service Environment resolves in the Step's scope.
    let c = &job.steps[0].step_services.as_ref().unwrap()[0];
    let c_envs = c.service_environments.as_ref().unwrap();
    let st = symtab_of(c_envs[0].resolved_symtab.as_ref().unwrap());
    assert_eq!(
        st.get_value("Step.Name"),
        Some(&ExprValue::String("S".into()))
    );
    assert_eq!(st.get_value("n"), Some(&ExprValue::Int(42)));

    // Serialization carries the list under the schema's key, and round-trips.
    let json = serde_json::to_value(a).unwrap();
    assert_eq!(json["serviceEnvironments"][0]["name"], "Conda");
    assert!(json["serviceEnvironments"][0].get("runScope").is_none());
    let back: job::Service = serde_json::from_value(json).unwrap();
    assert_eq!(&back, a);
    let b_json = serde_json::to_value(&job.job_services.as_ref().unwrap()[1]).unwrap();
    assert!(b_json.get("serviceEnvironments").is_none());
}

#[test]
fn service_environment_carried_forward_fields_are_rechecked_with_parameters_bound() {
    let tmpl = template(&Tmpl {
        a_envs: r#"- name: Conda
  variables:
    BIG: "{{ 'x' * Param.Port }}""#,
        ..Default::default()
    });
    create_ok(&tmpl, &[("Port", "2048")]);
    let err = create(&tmpl, &[("Port", "2049")])
        .expect_err("job creation should fail")
        .to_string();
    assert_eq!(
        err,
        "Model validation error: 1 validation error for JobTemplate\njobServices[0] -> serviceEnvironments[0] -> variables -> BIG:\n\tresolves to at least 2049 characters, exceeding the maximum of 2048."
    );
}

#[test]
fn referenced_service_names_includes_references_from_service_environments() {
    let job = create_ok(
        &template(&Tmpl {
            b_envs: &env_with_args("Conda", r#"["{{ Service.A.p.port }}"]"#),
            ..Default::default()
        }),
        &[],
    );
    let services = job.job_services.as_ref().unwrap();
    assert!(referenced_service_names(&services[0]).is_empty());
    // B's script references nothing; only its Service Environment does.
    assert_eq!(
        referenced_service_names(&services[1])
            .into_iter()
            .collect::<Vec<_>>(),
        vec!["A".to_string()]
    );
}

#[test]
fn external_services_carry_their_service_environments_through_submission() {
    let et = expect_env_ok(&env_template(
        &env_with_args(
            "Conda",
            r#"["{{ Service.First.main.bindAddress }}", "{{ Param.Size }}"]"#,
        ),
        "",
        "environment:\n  name: Queue\n  variables: { K: v }",
    ));
    let jt = expect_job_ok(&template(&Tmpl::default()));
    let td = tempfile::TempDir::new().unwrap();
    let dir = td.path().to_str().unwrap();
    let processed = openjd_model::preprocess_job_parameters(
        &jt,
        &JobParameterInputValues::new(),
        std::slice::from_ref(&et),
        &openjd_model::PathParameterOptions::new(dir, dir),
    )
    .unwrap();
    let job = create_job(&jt, &processed, &jt.default_validation_context()).unwrap();
    let attached: Vec<AttachedEnvironmentTemplate<'_>> = vec![(&et).into()];
    let applied =
        apply_environment_templates(&job, &attached, &processed, &CallerLimits::default())
            .expect("submission should succeed");
    let first = &applied.external_services[0];
    let envs = first
        .service_environments
        .as_ref()
        .expect("carried through");
    assert_eq!(envs[0].name, "Conda");
    let st = symtab_of(envs[0].resolved_symtab.as_ref().unwrap());
    assert_eq!(st.get_value("Param.Size"), Some(&ExprValue::Int(1)));
    assert!(applied.external_services[1].service_environments.is_none());
}

#[test]
fn rfc_valkey_example_validates_and_instantiates_with_its_service_environment() {
    let jt = decode_job_template(yaml_val(RFC_VALKEY), Some(EXTS), &CallerLimits::default())
        .expect("the RFC's Valkey example validates");
    let cache_t = &jt.job_services.as_ref().unwrap()[0];
    assert_eq!(
        cache_t.service_environments.as_ref().unwrap()[0].name,
        "ValkeyConda"
    );

    let job = create_ok(RFC_VALKEY, &[("FrameEnd", "10")]);
    let cache = &job.job_services.as_ref().unwrap()[0];
    let envs = cache.service_environments.as_ref().expect("ValkeyConda");
    assert_eq!(envs.len(), 1);
    assert_eq!(envs[0].name, "ValkeyConda");
    assert_eq!(envs[0].run_scope, None);
    let on_enter = envs[0]
        .script
        .as_ref()
        .unwrap()
        .actions
        .on_enter
        .as_ref()
        .expect("onEnter");
    assert_eq!(on_enter.command.raw(), "bash");
    assert!(on_enter.args.as_ref().unwrap()[1]
        .raw()
        .starts_with("conda create -y -p ./valkey-env"));
    assert!(envs[0].resolved_symtab.is_some());
    // The Valkey Service references no other Service, from either its
    // script or its Service Environment.
    assert!(referenced_service_names(cache).is_empty());
}
