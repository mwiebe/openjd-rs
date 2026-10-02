// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Integration tests for the RFC 0009 `Service.*` format-string scope rules
//! (Template Schemas §7.3.1 `Service.*` rows, §9 scope list, §9.7 items 1–2,
//! §3.6.2 let tables, §4 item 3.2, §4.3.1 `WrappedService.*`).
//!
//! Every negative case asserts the full Pydantic-style error path and
//! message. A `Service.*` reference that is out of scope surfaces with the
//! scope rule it breaks when the Service is declared somewhere in the
//! document (`validate_v2023_09::service_scope`); a name declared nowhere
//! keeps the crate's ordinary `Undefined variable` error (with its
//! suggestion), exactly as an out-of-scope `WrappedStep.Name` does under
//! RFC 0008.

use openjd_model::{decode_environment_template, decode_job_template, CallerLimits};

const EXTS: &[&str] = &["EXPR", "SERVICE", "FEATURE_BUNDLE_1", "WRAP_ACTIONS"];

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

fn expect_env_ok(template: &str) {
    decode_environment_template(yaml_val(template), Some(EXTS), &CallerLimits::default())
        .expect("expected successful decode");
}

/// A job template with two Job Services `A` (port `p`) and `B` (port `q`),
/// one job environment, and one step `S` with a Step Service `C` (port `r`),
/// one step environment, and an `onRun`. Each `*_body` is the complete YAML
/// body of that Service after its `name` (so a test may redefine `ports`,
/// `script`, etc.); the defaults declare one port and a bare `onRun`. Each
/// body is indented to its position.
struct Tmpl<'a> {
    a_body: &'a str,
    b_body: &'a str,
    c_body: &'a str,
    job_env: &'a str,
    step_env: &'a str,
    step_extra: &'a str,
    on_run_args: &'a str,
}

const A_BODY: &str = "ports: [{ name: p }]\nscript:\n  actions:\n    onRun:\n      command: a";
const B_BODY: &str = "ports: [{ name: q }]\nscript:\n  actions:\n    onRun:\n      command: b";
const C_BODY: &str = "ports: [{ name: r }]\nscript:\n  actions:\n    onRun:\n      command: c";

impl Default for Tmpl<'_> {
    fn default() -> Self {
        Self {
            a_body: A_BODY,
            b_body: B_BODY,
            c_body: C_BODY,
            job_env: "variables: { K: v }",
            step_env: "variables: { K: v }",
            step_extra: "",
            on_run_args: "[x]",
        }
    }
}

fn template(t: &Tmpl<'_>) -> String {
    let indent = |body: &str, n: usize| -> String {
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
    };
    format!(
        r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [SERVICE, EXPR, FEATURE_BUNDLE_1, WRAP_ACTIONS]
name: Test
parameterDefinitions:
  - {{ name: Port, type: INT, default: 1 }}
  - {{ name: Dir, type: PATH, default: /tmp }}
jobEnvironments:
  - name: JobEnv
{job_env}
jobServices:
  - name: A
{a_body}
  - name: B
{b_body}
steps:
  - name: S
{step_extra}
    stepEnvironments:
      - name: StepEnv
{step_env}
    stepServices:
      - name: C
{c_body}
    script:
      actions:
        onRun:
          command: run
          args: {on_run_args}
"#,
        job_env = indent(t.job_env, 4),
        a_body = indent(t.a_body, 4),
        b_body = indent(t.b_body, 4),
        step_extra = indent(t.step_extra, 4),
        step_env = indent(t.step_env, 8),
        c_body = indent(t.c_body, 8),
        on_run_args = t.on_run_args,
    )
}

fn undefined(name: &str) -> String {
    format!("Undefined variable: '{name}'.")
}

/// The scope-rule messages of `validate_v2023_09::service_scope`.
fn step_service_out_of_scope(svc: &str, step: &str, here: &str) -> String {
    format!("Service '{svc}' is a Step Service of step '{step}' and is not in scope in {here}.")
}

fn later_in_list(svc: &str, list: &str, from: &str) -> String {
    format!(
        "Service '{svc}' is declared later in {list} than '{from}'; a Service may reference only \
         itself and earlier Services."
    )
}

fn bind_address_outside(svc: &str, port: &str) -> String {
    format!(
        "Service.{svc}.{port}.bindAddress is available only within the Service '{svc}' itself; use \
         connectAddress to reach it from elsewhere."
    )
}

fn env_in_service_sessions(env: &str) -> String {
    format!(
        "Environment '{env}' is entered in Service Sessions (its runScope includes SERVICE) and \
         may not reference Service.*; declare runScope: [TASK] if it configures Tasks."
    )
}

fn job_creation_field(field: &str) -> String {
    format!(
        "Service.* is not available in {field}: it is resolved at job creation, before any \
         Service has an endpoint."
    )
}

fn no_such_port(svc: &str, port: &str, declared: &str) -> String {
    format!("Service '{svc}' has no port '{port}'; declared ports: {declared}.")
}

const TASK_IN_SERVICE: &str = "Task.* is not available within a Service.";

// ════════════════════════════════════════════════════════════════════
// §9 scope list item 3–4 / §7.3.1: Steps see Job Services and their own
// ════════════════════════════════════════════════════════════════════

#[test]
fn step_script_sees_job_and_own_step_services() {
    expect_job_ok(&template(&Tmpl {
        on_run_args: r#"["{{ Service.A.p.port }}", "{{ Service.B.q.connectAddress }}", "{{ join_host_port(Service.C.r.connectAddress, Service.C.r.port) }}"]"#,
        ..Default::default()
    }));
}

#[test]
fn step_script_never_sees_bind_address() {
    // Even of its own Step Service: bindAddress is for the declaring
    // Service only (§7.3.1).
    expect_job_err(
        &template(&Tmpl {
            on_run_args: r#"["{{ Service.C.r.bindAddress }}"]"#,
            ..Default::default()
        }),
        &[
            "1 validation error for JobTemplate\n",
            "steps[0] -> script -> actions -> onRun -> args[0]:\n\tFailed to parse interpolation expression at [",
            &bind_address_outside("C", "r"),
        ],
    );
    expect_job_err(
        &template(&Tmpl {
            on_run_args: r#"["{{ Service.A.p.bindAddress }}"]"#,
            ..Default::default()
        }),
        &[
            "steps[0] -> script -> actions -> onRun -> args[0]:\n\tFailed to parse interpolation expression at [",
            &bind_address_outside("A", "p"),
        ],
    );
}

#[test]
fn step_script_cannot_see_another_steps_service() {
    let tmpl = template(&Tmpl::default()).replace(
        "    script:\n      actions:\n        onRun:\n          command: run\n          args: [x]\n",
        r#"    script:
      actions:
        onRun:
          command: run
  - name: Other
    script:
      actions:
        onRun:
          command: run
          args: ["{{ Service.C.r.port }}"]
"#,
    );
    expect_job_err(
        &tmpl,
        &[
            "1 validation error for JobTemplate\n",
            "steps[1] -> script -> actions -> onRun -> args[0]:\n\tFailed to parse interpolation expression at [",
            &step_service_out_of_scope("C", "S", "step 'Other'"),
        ],
    );
}

#[test]
fn undeclared_service_or_port_is_undefined() {
    // A Service declared nowhere keeps the generic message and its
    // suggestion; an undeclared port of a declared Service names the
    // declared ports; an unknown value of a declared port stays generic.
    expect_job_err(
        &template(&Tmpl {
            on_run_args: r#"["{{ Service.Nope.p.port }}", "{{ Service.A.nope.port }}", "{{ Service.A.p.nope }}"]"#,
            ..Default::default()
        }),
        &[
            "3 validation errors for JobTemplate\n",
            "steps[0] -> script -> actions -> onRun -> args[0]:\n\tFailed to parse interpolation expression at [",
            &format!("{} Did you mean: Service.A.p.port", undefined("Service.Nope.p.port")),
            "steps[0] -> script -> actions -> onRun -> args[1]:\n\tFailed to parse interpolation expression at [",
            &no_such_port("A", "nope", "p"),
            "steps[0] -> script -> actions -> onRun -> args[2]:\n\tFailed to parse interpolation expression at [",
            &undefined("Service.A.p.nope"),
        ],
    );
}

#[test]
fn service_name_typo_keeps_the_suggestion_but_a_declared_name_gets_the_rule() {
    // Exploratory report stumble S3 (`w05`): a reference to a *declared* later
    // Service must not be answered with "Did you mean" another Service.
    let tmpl = template(&Tmpl {
        a_body: "ports: [{ name: p }]\nvariables:\n  UP: \"{{ Service.B.q.port }}\"\n  TYPO: \"{{ Service.Bq.q.port }}\"\nscript:\n  actions:\n    onRun:\n      command: a",
        ..Default::default()
    });
    expect_job_err(
        &tmpl,
        &[
            "2 validation errors for JobTemplate\n",
            "jobServices[0] -> variables -> TYPO:\n\tFailed to parse interpolation expression at [",
            &format!(
                "{} Did you mean: Service.A.p.port",
                undefined("Service.Bq.q.port")
            ),
            "jobServices[0] -> variables -> UP:\n\tFailed to parse interpolation expression at [",
            &later_in_list("B", "jobServices", "A"),
        ],
    );
    let err = decode_job_template(yaml_val(&tmpl), Some(EXTS), &CallerLimits::default())
        .unwrap_err()
        .to_string();
    assert!(
        !err.contains("Did you mean: Service.A.p.port\n  Service.B.q.port"),
        "the declared Service B must not get a suggestion:\n{err}"
    );
}

#[test]
fn service_port_type_checks_as_int_and_addresses_as_string() {
    // `port` is unresolved[int]: arithmetic type-checks; a string method does not.
    expect_job_ok(&template(&Tmpl {
        on_run_args: r#"["{{ Service.A.p.port + 1 }}", "{{ Service.A.p.connectAddress.upper() }}"]"#,
        ..Default::default()
    }));
    expect_job_err(
        &template(&Tmpl {
            on_run_args: r#"["{{ Service.A.p.port.upper() }}"]"#,
            ..Default::default()
        }),
        &["steps[0] -> script -> actions -> onRun -> args[0]:\n\tFailed to parse interpolation expression at ["],
    );
}

#[test]
fn step_script_let_and_task_file_data_see_services() {
    let tmpl = template(&Tmpl::default()).replace(
        "    script:\n      actions:\n        onRun:\n          command: run\n          args: [x]\n",
        r#"    script:
      let:
        - url = 'http://' + join_host_port(Service.A.p.connectAddress, Service.A.p.port)
      actions:
        onRun:
          command: run
          args: ["{{ url }}", "{{ Task.File.Conf }}"]
      embeddedFiles:
        - name: Conf
          type: TEXT
          data: "host={{ Service.C.r.connectAddress }}"
"#,
    );
    expect_job_ok(&tmpl);
}

#[test]
fn step_level_let_cannot_reference_services() {
    // <StepTemplate>.let is a job-creation binding (§3.6.2).
    expect_job_err(
        &template(&Tmpl {
            step_extra: "let:\n  - port = Service.A.p.port",
            ..Default::default()
        }),
        &[&format!(
            "steps[0] -> let[0]:\n\tInvalid expression in let binding 'port': {}",
            job_creation_field("a let binding")
        )],
    );
}

#[test]
fn step_host_requirements_and_ranges_cannot_reference_services() {
    expect_job_err(
        &template(&Tmpl {
            step_extra: "hostRequirements:\n  attributes:\n    - name: attr.worker.os.family\n      anyOf: [\"{{ Service.A.p.connectAddress }}\"]\nparameterSpace:\n  taskParameterDefinitions:\n    - name: X\n      type: INT\n      range: \"1-{{ Service.A.p.port }}\"",
            ..Default::default()
        }),
        &[
            "2 validation errors for JobTemplate\n",
            "steps[0] -> hostRequirements -> attributes[0] -> anyOf[0]:\n\tFailed to parse interpolation expression at [",
            &job_creation_field("hostRequirements"),
            "steps[0] -> parameterSpace -> taskParameterDefinitions[0] -> range:\n\tFailed to parse interpolation expression at [",
            &job_creation_field("a parameterSpace range"),
        ],
    );
}

#[test]
fn step_action_timeout_cannot_reference_services() {
    // `timeout` is plain @fmtstring (job creation).
    let tmpl = template(&Tmpl::default()).replace(
        "          command: run\n          args: [x]\n",
        "          command: run\n          timeout: \"{{ Service.A.p.port }}\"\n",
    );
    expect_job_err(
        &tmpl,
        &[
            "steps[0] -> script -> actions -> onRun -> timeout:\n\tFailed to parse interpolation expression at [",
            &job_creation_field("timeout"),
        ],
    );
}

// ════════════════════════════════════════════════════════════════════
// §4 item 3.2 / §9.7 item 2: Environments and runScope
// ════════════════════════════════════════════════════════════════════

#[test]
fn environments_with_default_run_scope_cannot_reference_services() {
    // The default runScope includes SERVICE.
    expect_job_err(
        &template(&Tmpl {
            job_env: "variables: { HOST: \"{{ Service.A.p.connectAddress }}\" }",
            step_env: "variables: { PORT: \"{{ Service.C.r.port }}\" }",
            ..Default::default()
        }),
        &[
            "2 validation errors for JobTemplate\n",
            "jobEnvironments[0] -> variables -> HOST:\n\tFailed to parse interpolation expression at [",
            &env_in_service_sessions("JobEnv"),
            "steps[0] -> stepEnvironments[0] -> variables -> PORT:\n\tFailed to parse interpolation expression at [",
            &env_in_service_sessions("StepEnv"),
        ],
    );
}

#[test]
fn environments_with_explicit_service_run_scope_cannot_reference_services() {
    expect_job_err(
        &template(&Tmpl {
            job_env: "runScope: [SERVICE, TASK]\nvariables: { HOST: \"{{ Service.A.p.connectAddress }}\" }",
            ..Default::default()
        }),
        &[
            "1 validation error for JobTemplate\n",
            "jobEnvironments[0] -> variables -> HOST:\n\tFailed to parse interpolation expression at [",
            &env_in_service_sessions("JobEnv"),
        ],
    );
}

#[test]
fn task_scoped_environments_see_services() {
    expect_job_ok(&template(&Tmpl {
        job_env: "runScope: [TASK]\nvariables: { HOST: \"{{ Service.A.p.connectAddress }}\", PORT: \"{{ Service.B.q.port }}\" }",
        step_env: "runScope: [TASK]\nscript:\n  let:\n    - url = join_host_port(Service.C.r.connectAddress, Service.C.r.port)\n  actions:\n    onEnter:\n      command: echo\n      args: [\"{{ url }}\", \"{{ Service.A.p.port }}\", \"{{ Env.File.F }}\"]\n  embeddedFiles:\n    - name: F\n      type: TEXT\n      data: \"{{ Service.B.q.connectAddress }}\"",
        ..Default::default()
    }));
}

#[test]
fn job_environment_cannot_see_step_services() {
    expect_job_err(
        &template(&Tmpl {
            job_env: "runScope: [TASK]\nvariables: { PORT: \"{{ Service.C.r.port }}\" }",
            ..Default::default()
        }),
        &[
            "1 validation error for JobTemplate\n",
            "jobEnvironments[0] -> variables -> PORT:\n\tFailed to parse interpolation expression at [",
            &step_service_out_of_scope("C", "S", "Job Environment 'JobEnv'"),
        ],
    );
}

#[test]
fn task_scoped_environment_never_sees_bind_address() {
    expect_job_err(
        &template(&Tmpl {
            job_env: "runScope: [TASK]\nvariables: { BIND: \"{{ Service.A.p.bindAddress }}\" }",
            ..Default::default()
        }),
        &[
            "jobEnvironments[0] -> variables -> BIND:\n\tFailed to parse interpolation expression at [",
            &bind_address_outside("A", "p"),
        ],
    );
}

#[test]
fn environment_action_timeout_cannot_reference_services() {
    expect_job_err(
        &template(&Tmpl {
            job_env: "runScope: [TASK]\nscript:\n  actions:\n    onEnter:\n      command: echo\n      timeout: \"{{ Service.A.p.port }}\"",
            ..Default::default()
        }),
        &[
            "jobEnvironments[0] -> script -> actions -> onEnter -> timeout:\n\tFailed to parse interpolation expression at [",
            &job_creation_field("timeout"),
        ],
    );
}

// ════════════════════════════════════════════════════════════════════
// §9 scope list items 1–2: within a Service
// ════════════════════════════════════════════════════════════════════

#[test]
fn service_sees_itself_including_bind_address_and_earlier_services() {
    expect_job_ok(&template(&Tmpl {
        b_body: "ports: [{ name: q }]\nvariables:\n  BIND: \"{{ Service.B.q.bindAddress }}\"\n  SELF: \"{{ join_host_port(Service.B.q.connectAddress, Service.B.q.port) }}\"\n  UPSTREAM: \"{{ Service.A.p.connectAddress }}:{{ Service.A.p.port }}\"\nscript:\n  actions:\n    onRun:\n      command: b",
        c_body: "ports: [{ name: r }]\nvariables:\n  OWN: \"{{ Service.C.r.bindAddress }}\"\n  JOB_A: \"{{ Service.A.p.port }}\"\n  JOB_B: \"{{ Service.B.q.connectAddress }}\"\nscript:\n  actions:\n    onRun:\n      command: c",
        ..Default::default()
    }));
}

#[test]
fn service_cannot_reference_a_later_service() {
    expect_job_err(
        &template(&Tmpl {
            a_body: "ports: [{ name: p }]\nvariables:\n  LATER: \"{{ Service.B.q.port }}\"\nscript:\n  actions:\n    onRun:\n      command: a",
            ..Default::default()
        }),
        &[
            "1 validation error for JobTemplate\n",
            "jobServices[0] -> variables -> LATER:\n\tFailed to parse interpolation expression at [",
            &later_in_list("B", "jobServices", "A"),
        ],
    );
}

#[test]
fn job_service_cannot_reference_a_step_service() {
    expect_job_err(
        &template(&Tmpl {
            b_body: "ports: [{ name: q }]\nvariables:\n  STEP: \"{{ Service.C.r.port }}\"\nscript:\n  actions:\n    onRun:\n      command: b",
            ..Default::default()
        }),
        &[
            "jobServices[1] -> variables -> STEP:\n\tFailed to parse interpolation expression at [",
            &step_service_out_of_scope("C", "S", "Service 'B'"),
        ],
    );
}

#[test]
fn service_cannot_see_another_services_bind_address() {
    expect_job_err(
        &template(&Tmpl {
            b_body: "ports: [{ name: q }]\nvariables:\n  BIND: \"{{ Service.A.p.bindAddress }}\"\nscript:\n  actions:\n    onRun:\n      command: b",
            ..Default::default()
        }),
        &[
            "jobServices[1] -> variables -> BIND:\n\tFailed to parse interpolation expression at [",
            &bind_address_outside("A", "p"),
        ],
    );
}

#[test]
fn step_services_are_forward_only_too() {
    let tmpl = template(&Tmpl {
        c_body: "ports: [{ name: r }]\nvariables: { LATER: \"{{ Service.D.s.port }}\" }\nscript:\n  actions:\n    onRun:\n      command: c",
        ..Default::default()
    })
    .replace(
        "          command: c\n",
        "          command: c\n      - name: D\n        ports: [{ name: s }]\n        variables: { EARLIER: \"{{ Service.C.r.port }}\" }\n        script:\n          actions:\n            onRun:\n              command: d\n",
    );
    expect_job_err(
        &tmpl,
        &[
            "1 validation error for JobTemplate\n",
            "steps[0] -> stepServices[0] -> variables -> LATER:\n\tFailed to parse interpolation expression at [",
            &later_in_list("D", "stepServices", "C"),
        ],
    );
}

#[test]
fn service_actions_embedded_files_and_script_let_see_services() {
    expect_job_ok(&template(&Tmpl {
        a_body: &format!(
            "ports: [{{ name: p }}]\n{}",
            r#"script:
  let:
    - listen = join_host_port(Service.A.p.bindAddress, Service.A.p.port)
    - conf = Service.File.Conf
  actions:
    onEnter:
      command: init
      args: ["{{ conf }}", "{{ Session.WorkingDirectory }}"]
    onRun:
      command: a
      args: ["--listen", "{{ listen }}", "--conf", "{{ Service.File.Conf }}"]
    onHealthCheck:
      command: probe
      args: ["{{ Service.A.p.connectAddress }}"]
    onExit:
      command: report
      args: ["{{ Service.A.p.port }}"]
  embeddedFiles:
    - name: Conf
      type: TEXT
      data: "bind={{ Service.A.p.bindAddress }} port={{ Service.A.p.port }}"
healthCheck:
  type: COMMAND"#
        ),
        ..Default::default()
    }));
}

#[test]
fn service_file_is_scoped_to_the_declaring_service() {
    expect_job_err(
        &template(&Tmpl {
            a_body: "ports: [{ name: p }]\nscript:\n  actions:\n    onRun:\n      command: a\n  embeddedFiles:\n    - name: Conf\n      type: TEXT\n      data: x",
            b_body: "ports: [{ name: q }]\nvariables:\n  CONF: \"{{ Service.File.Conf }}\"\nscript:\n  actions:\n    onRun:\n      command: b",
            on_run_args: r#"["{{ Service.File.Conf }}"]"#,
            ..Default::default()
        }),
        &[
            "2 validation errors for JobTemplate\n",
            "jobServices[1] -> variables -> CONF:\n\tFailed to parse interpolation expression at [",
            &undefined("Service.File.Conf"),
            "steps[0] -> script -> actions -> onRun -> args[0]:\n\tFailed to parse interpolation expression at [",
        ],
    );
}

#[test]
fn task_values_are_never_available_within_a_service() {
    let tmpl = template(&Tmpl {
        c_body: "ports: [{ name: r }]\nvariables:\n  T: \"{{ Task.Param.Frame }}\"\nscript:\n  actions:\n    onRun:\n      command: c",
        step_extra: "parameterSpace:\n  taskParameterDefinitions:\n    - name: Frame\n      type: INT\n      range: \"1-3\"",
        ..Default::default()
    });
    expect_job_err(
        &tmpl,
        &[
            "1 validation error for JobTemplate\n",
            "steps[0] -> stepServices[0] -> variables -> T:\n\tFailed to parse interpolation expression at [",
            TASK_IN_SERVICE,
        ],
    );
}

// ════════════════════════════════════════════════════════════════════
// §9 item 3 / §9.2 / §9.7 item 2: <Service>.let, hostRequirements and the
// numeric fields are job-creation scope
// ════════════════════════════════════════════════════════════════════

#[test]
fn service_let_cannot_reference_session_or_service_values() {
    expect_job_err(
        &template(&Tmpl {
            a_body: "ports: [{ name: p }]\nlet:\n  - port = Service.A.p.port\n  - wd = Session.WorkingDirectory\n  - dir = Param.Dir\nscript:\n  actions:\n    onRun:\n      command: a",
            ..Default::default()
        }),
        &[
            "3 validation errors for JobTemplate\n",
            &format!(
                "jobServices[0] -> let[0]:\n\tInvalid expression in let binding 'port': {}",
                job_creation_field("a let binding")
            ),
            "jobServices[0] -> let[1]:\n\tInvalid expression in let binding 'wd': Undefined variable: 'Session.WorkingDirectory'.",
            "jobServices[0] -> let[2]:\n\tInvalid expression in let binding 'dir': Undefined variable: 'Param.Dir'.",
        ],
    );
}

#[test]
fn service_let_sees_params_job_name_and_for_a_step_service_step_scope() {
    expect_job_ok(&template(&Tmpl {
        a_body: "let:\n  - port = Param.Port + 1\n  - raw_dir = RawParam.Dir\n  - label = Job.Name\nports:\n  - name: p\n    port: \"{{ port }}\"\nvariables:\n  LABEL: \"{{ label }}\"\n  DIR: \"{{ raw_dir }}\"\nscript:\n  actions:\n    onRun:\n      command: a",
        c_body: "ports: [{ name: r }]\nlet:\n  - tag = Job.Name + '/' + Step.Name + '/' + string(n)\nvariables:\n  TAG: \"{{ tag }}\"\n  N: \"{{ n }}\"\nscript:\n  actions:\n    onRun:\n      command: c",
        step_extra: "let:\n  - n = 3",
        ..Default::default()
    }));
}

#[test]
fn job_service_let_cannot_see_step_name() {
    expect_job_err(
        &template(&Tmpl {
            a_body: "ports: [{ name: p }]\nlet:\n  - s = Step.Name\nscript:\n  actions:\n    onRun:\n      command: a",
            ..Default::default()
        }),
        &["jobServices[0] -> let[0]:\n\tInvalid expression in let binding 's': Undefined variable: 'Step.Name'."],
    );
}

#[test]
fn service_lets_may_not_shadow_enclosing_scopes() {
    expect_job_err(
        &template(&Tmpl {
            c_body: "ports: [{ name: r }]\nlet:\n  - n = 4\nscript:\n  let:\n    - n = 5\n  actions:\n    onRun:\n      command: c",
            step_extra: "let:\n  - n = 3",
            ..Default::default()
        }),
        &[
            "2 validation errors for JobTemplate\n",
            "steps[0] -> stepServices[0] -> let[0]:\n\t'n' shadows enclosing scope.",
            "steps[0] -> stepServices[0] -> script -> let[0]:\n\t'n' shadows enclosing scope.",
        ],
    );
}

#[test]
fn service_lets_require_expr() {
    let err = decode_job_template(
        yaml_val(
            r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [SERVICE]
name: Test
jobServices:
  - name: A
    let: [x = 1]
    ports: [{ name: p }]
    script:
      let: [y = 2]
      actions:
        onRun:
          command: a
steps:
  - name: S
    script:
      actions:
        onRun:
          command: run
"#,
        ),
        Some(EXTS),
        &CallerLimits::default(),
    )
    .expect_err("expected validation error")
    .to_string();
    for line in [
        "jobServices[0] -> let:\n\t'let' requires the EXPR extension.",
        "jobServices[0] -> script -> let:\n\t'let' requires the EXPR extension.",
    ] {
        assert!(err.contains(line), "missing {line:?} in:\n{err}");
    }
}

#[test]
fn service_script_let_scope() {
    expect_job_ok(&template(&Tmpl {
        a_body: "ports: [{ name: p }]\nlet:\n  - base = 'x'\nscript:\n  let:\n    - wd = Session.WorkingDirectory\n    - own = Service.A.p.bindAddress\n    - dir = Param.Dir\n    - name = base + Job.Name\n  actions:\n    onRun:\n      command: a\n      args: [\"{{ wd }}\", \"{{ own }}\", \"{{ dir }}\", \"{{ name }}\"]",
        ..Default::default()
    }));
    expect_job_err(
        &template(&Tmpl {
            a_body: "ports: [{ name: p }]\nscript:\n  let:\n    - later = Service.B.q.port\n  actions:\n    onRun:\n      command: a",
            ..Default::default()
        }),
        &[&format!(
            "jobServices[0] -> script -> let[0]:\n\tInvalid expression in let binding 'later': {}",
            job_creation_field("a let binding")
        )],
    );
}

#[test]
fn service_host_requirements_cannot_reference_services_or_session() {
    expect_job_err(
        &template(&Tmpl {
            b_body: "ports: [{ name: q }]\nhostRequirements:\n  attributes:\n    - name: attr.worker.os.family\n      anyOf: [\"{{ Service.A.p.connectAddress }}\"]\n  amounts:\n    - name: amount.worker.memory\n      min: \"{{ Service.B.q.port }}\"\nscript:\n  actions:\n    onRun:\n      command: b",
            ..Default::default()
        }),
        &[
            "2 validation errors for JobTemplate\n",
            "jobServices[1] -> hostRequirements -> amounts[0] -> min:\n\tFailed to parse interpolation expression at [",
            &job_creation_field("hostRequirements"),
            "jobServices[1] -> hostRequirements -> attributes[0] -> anyOf[0]:\n\tFailed to parse interpolation expression at [",
            &job_creation_field("hostRequirements"),
        ],
    );
}

#[test]
fn service_host_requirements_see_let_and_params_not_path_params() {
    expect_job_ok(&template(&Tmpl {
        a_body: "ports: [{ name: p }]\nlet:\n  - mem = Param.Port * 1024\nhostRequirements:\n  amounts:\n    - name: amount.worker.memory\n      min: \"{{ mem }}\"\n  attributes:\n    - name: attr.worker.preemptible\n      anyOf: [\"false\"]\nscript:\n  actions:\n    onRun:\n      command: a",
        ..Default::default()
    }));
    expect_job_err(
        &template(&Tmpl {
            a_body: "ports: [{ name: p }]\nhostRequirements:\n  attributes:\n    - name: attr.x\n      anyOf: [\"{{ Param.Dir }}\"]\nscript:\n  actions:\n    onRun:\n      command: a",
            ..Default::default()
        }),
        &[
            "jobServices[0] -> hostRequirements -> attributes[0] -> anyOf[0]:\n\tFailed to parse interpolation expression at [",
            &undefined("Param.Dir"),
        ],
    );
}

#[test]
fn path_params_available_only_in_service_variables_and_script() {
    expect_job_ok(&template(&Tmpl {
        a_body: "ports: [{ name: p }]\nvariables:\n  DIR: \"{{ Param.Dir }}\"\nscript:\n  actions:\n    onRun:\n      command: a\n      args: [\"{{ Param.Dir }}\", \"{{ apply_path_mapping(Param.Dir) }}\"]",
        ..Default::default()
    }));
}

#[test]
fn numeric_fields_cannot_reference_session_or_service_values() {
    expect_job_err(
        &template(&Tmpl {
            a_body: "ports:\n  - name: p\n    port: \"{{ Service.A.p.port }}\"\nhealthCheck:\n  type: COMMAND\n  readinessIntervalSeconds: \"{{ Session.WorkingDirectory }}\"\n  readinessTimeoutSeconds: \"{{ Service.A.p.port }}\"\n  healthIntervalSeconds: \"{{ Session.WorkingDirectory }}\"\n  failureThreshold: \"{{ Service.A.p.port }}\"\nrestartPolicy:\n  maxAttempts: \"{{ Param.Dir }}\"\nscript:\n  actions:\n    onRun:\n      command: a\n    onHealthCheck:\n      command: probe",
            ..Default::default()
        }),
        &[
            "6 validation errors for JobTemplate\n",
            "jobServices[0] -> ports[0] -> port:\n\tFailed to parse interpolation expression at [",
            &job_creation_field("port"),
            "jobServices[0] -> healthCheck -> readinessIntervalSeconds:\n\tFailed to parse interpolation expression at [",
            &undefined("Session.WorkingDirectory"),
            "jobServices[0] -> healthCheck -> readinessTimeoutSeconds:\n\tFailed to parse interpolation expression at [",
            &job_creation_field("readinessTimeoutSeconds"),
            "jobServices[0] -> healthCheck -> healthIntervalSeconds:\n\tFailed to parse interpolation expression at [",
            "jobServices[0] -> healthCheck -> failureThreshold:\n\tFailed to parse interpolation expression at [",
            &job_creation_field("failureThreshold"),
            "jobServices[0] -> restartPolicy -> maxAttempts:\n\tFailed to parse interpolation expression at [",
            &undefined("Param.Dir"),
        ],
    );
}

#[test]
fn numeric_fields_static_resolution_is_range_checked() {
    expect_job_err(
        &template(&Tmpl {
            a_body: "let:\n  - big = 70000\n  - neg = -1\nports:\n  - name: p\n    port: \"{{ big }}\"\nhealthCheck:\n  type: COMMAND\n  readinessIntervalSeconds: \"{{ 0 }}\"\n  readinessTimeoutSeconds: \"{{ 'soon' }}\"\n  healthIntervalSeconds: \"{{ neg }}\"\n  failureThreshold: \"{{ 0 }}\"\nrestartPolicy:\n  maxAttempts: \"{{ neg }}\"\nscript:\n  actions:\n    onRun:\n      command: a\n    onHealthCheck:\n      command: probe",
            ..Default::default()
        }),
        &[
            "6 validation errors for JobTemplate\n",
            "jobServices[0] -> ports[0] -> port:\n\tmust be between 1 and 65535.",
            "jobServices[0] -> healthCheck -> readinessIntervalSeconds:\n\tmust be > 0.",
            "jobServices[0] -> healthCheck -> readinessTimeoutSeconds:\n\tFailed to parse interpolation expression at [",
            "jobServices[0] -> healthCheck -> healthIntervalSeconds:\n\tmust be > 0.",
            "jobServices[0] -> healthCheck -> failureThreshold:\n\tmust be > 0.",
            "jobServices[0] -> restartPolicy -> maxAttempts:\n\tmust be >= 0.",
        ],
    );
    // A whole-field null means "not provided" and is accepted.
    expect_job_ok(&template(&Tmpl {
        a_body: "ports:\n  - name: p\n    port: \"{{ null }}\"\nhealthCheck:\n  type: STDOUT\n  readinessTimeoutSeconds: \"{{ null }}\"\n  healthIntervalSeconds: \"{{ null }}\"\n  failureThreshold: \"{{ null }}\"\nrestartPolicy:\n  maxAttempts: \"{{ null }}\"\nscript:\n  actions:\n    onRun:\n      command: a",
        ..Default::default()
    }));
}

#[test]
fn service_action_timeout_is_job_creation_scope() {
    expect_job_ok(&template(&Tmpl {
        a_body: "ports: [{ name: p }]\nlet:\n  - t = 60\nscript:\n  actions:\n    onRun:\n      command: a\n      timeout: \"{{ t * Param.Port }}\"\n      cancelation:\n        mode: NOTIFY_THEN_TERMINATE\n        notifyPeriodInSeconds: \"{{ t }}\"",
        ..Default::default()
    }));
    expect_job_err(
        &template(&Tmpl {
            a_body: "ports: [{ name: p }]\nscript:\n  actions:\n    onRun:\n      command: a\n      timeout: \"{{ Service.A.p.port }}\"",
            ..Default::default()
        }),
        &[
            "jobServices[0] -> script -> actions -> onRun -> timeout:\n\tFailed to parse interpolation expression at [",
            &job_creation_field("timeout"),
        ],
    );
}

#[test]
fn service_comprehension_variables_cannot_shadow_lets() {
    expect_job_err(
        &template(&Tmpl {
            a_body: "ports: [{ name: p }]\nlet:\n  - n = 1\nscript:\n  actions:\n    onRun:\n      command: a\n      args: [\"{{ [n for n in [1, 2]] }}\"]",
            ..Default::default()
        }),
        &["jobServices[0] -> script -> actions -> onRun -> args[0]:\n\t"],
    );
}

// ════════════════════════════════════════════════════════════════════
// §4.3.1 WrappedService.*
// ════════════════════════════════════════════════════════════════════

fn wrapper_env(service_hook_args: &str, task_hook_args: &str) -> String {
    format!(
        r#"runScope: [TASK, SERVICE]
script:
  actions:
    onEnter: {{ command: echo }}
    onWrapEnvEnter: {{ command: echo }}
    onWrapTaskRun:
      command: echo
      args: {task_hook_args}
    onWrapEnvExit: {{ command: echo }}
    onWrapServiceEnter:
      command: echo
      args: {service_hook_args}
    onWrapServiceRun:
      command: echo
      args: {service_hook_args}
    onWrapServiceHealthCheck:
      command: echo
      args: {service_hook_args}
    onWrapServiceExit:
      command: echo
      args: {service_hook_args}"#
    )
}

#[test]
fn wrapped_service_available_in_the_four_service_hooks() {
    expect_job_ok(&template(&Tmpl {
        job_env: &wrapper_env(
            r#"["{{ WrappedService.Name }}", "{{ WrappedAction.Command }}", "{{ flatten([['-p', string(p) + ':' + string(p)] for p in WrappedService.Ports]) }}", "{{ WrappedService.PortNames[0] }}", "{{ join_host_port(WrappedService.BindAddresses[0], WrappedService.Ports[0]) }}"]"#,
            "[x]",
        ),
        ..Default::default()
    }));
}

#[test]
fn wrapped_service_not_available_in_task_or_env_hooks() {
    expect_job_err(
        &template(&Tmpl {
            job_env: &wrapper_env("[x]", r#"["{{ WrappedService.Name }}"]"#),
            ..Default::default()
        }),
        &[
            "1 validation error for JobTemplate\n",
            "jobEnvironments[0] -> script -> actions -> onWrapTaskRun -> args[0]:\n\tFailed to parse interpolation expression at [",
            &undefined("WrappedService.Name"),
        ],
    );
    let tmpl = template(&Tmpl {
        job_env: &wrapper_env("[x]", "[x]"),
        ..Default::default()
    })
    .replace(
        "    onWrapEnvExit: { command: echo }",
        "    onWrapEnvExit: { command: echo, args: [\"{{ WrappedService.Ports }}\"] }",
    );
    expect_job_err(
        &tmpl,
        &[
            "jobEnvironments[0] -> script -> actions -> onWrapEnvExit -> args[0]:\n\tFailed to parse interpolation expression at [",
            &undefined("WrappedService.Ports"),
        ],
    );
}

#[test]
fn wrapped_step_and_env_names_not_available_in_service_hooks() {
    expect_job_err(
        &template(&Tmpl {
            job_env: &wrapper_env(r#"["{{ WrappedStep.Name }}"]"#, "[x]"),
            ..Default::default()
        }),
        &[
            "4 validation errors for JobTemplate\n",
            "jobEnvironments[0] -> script -> actions -> onWrapServiceEnter -> args[0]:\n\tFailed to parse interpolation expression at [",
            "jobEnvironments[0] -> script -> actions -> onWrapServiceExit -> args[0]:\n\tFailed to parse interpolation expression at [",
            &undefined("WrappedStep.Name"),
        ],
    );
}

#[test]
fn wrapped_service_lists_type_check() {
    // Ports is list[int]: indexing and arithmetic type-check; a string
    // method on an element does not.
    expect_job_err(
        &template(&Tmpl {
            job_env: &wrapper_env(r#"["{{ WrappedService.Ports[0].upper() }}"]"#, "[x]"),
            ..Default::default()
        }),
        &["jobEnvironments[0] -> script -> actions -> onWrapServiceEnter -> args[0]:\n\tFailed to parse interpolation expression at ["],
    );
}

#[test]
fn wrapping_environment_does_not_see_services_in_service_sessions() {
    // A wrapper entered in Service Sessions has SERVICE in its runScope,
    // so no Service.* value is in scope — not even in the Service hooks.
    expect_job_err(
        &template(&Tmpl {
            job_env: &wrapper_env(r#"["{{ Service.A.p.port }}"]"#, "[x]"),
            ..Default::default()
        }),
        &[
            "4 validation errors for JobTemplate\n",
            "jobEnvironments[0] -> script -> actions -> onWrapServiceRun -> args[0]:\n\tFailed to parse interpolation expression at [",
            &env_in_service_sessions("JobEnv"),
        ],
    );
}

// ════════════════════════════════════════════════════════════════════
// Environment templates (§1.2.2)
// ════════════════════════════════════════════════════════════════════

fn env_template(services: &str, environment: &str) -> String {
    format!(
        r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, EXPR, FEATURE_BUNDLE_1]
parameterDefinitions:
  - {{ name: Mem, type: INT, default: 1024 }}
services:
{services}
{environment}
"#
    )
}

const TWO_SERVICES: &str = r#"  - name: A
    ports: [{ name: p }]
    variables: { OWN: "{{ Service.A.p.bindAddress }}" }
    script:
      actions:
        onRun:
          command: a
  - name: B
    ports: [{ name: q }]
    variables: { UP: "{{ Service.A.p.connectAddress }}" }
    script:
      actions:
        onRun:
          command: b
"#;

#[test]
fn env_template_environment_sees_every_service_when_task_scoped() {
    expect_env_ok(&env_template(
        TWO_SERVICES,
        "environment:\n  name: Client\n  runScope: [TASK]\n  variables:\n    A: \"{{ Service.A.p.connectAddress }}\"\n    B: \"{{ Service.B.q.port }}\"\n    J: \"{{ Job.Name }}\"",
    ));
}

#[test]
fn env_template_environment_with_default_run_scope_cannot_reference_services() {
    expect_env_err(
        &env_template(
            TWO_SERVICES,
            "environment:\n  name: Client\n  variables:\n    A: \"{{ Service.A.p.connectAddress }}\"",
        ),
        &[
            "1 validation error for EnvironmentTemplate\n",
            "environment -> variables -> A:\n\tFailed to parse interpolation expression at [",
            &env_in_service_sessions("Client"),
        ],
    );
}

#[test]
fn env_template_services_are_forward_only_and_never_see_step_name() {
    expect_env_err(
        &env_template(
            "  - name: A\n    ports: [{ name: p }]\n    let: [s = Step.Name]\n    variables: { LATER: \"{{ Service.B.q.port }}\" }\n    script:\n      actions:\n        onRun:\n          command: a\n  - name: B\n    ports: [{ name: q }]\n    script:\n      actions:\n        onRun:\n          command: b\n",
            "",
        ),
        &[
            "2 validation errors for EnvironmentTemplate\n",
            "services[0] -> let[0]:\n\tInvalid expression in let binding 's': Undefined variable: 'Step.Name'.",
            "services[0] -> variables -> LATER:\n\tFailed to parse interpolation expression at [",
            &later_in_list("B", "services", "A"),
        ],
    );
}

#[test]
fn env_template_services_only_document_validates_service_scopes() {
    expect_env_ok(&env_template(TWO_SERVICES, ""));
    expect_env_err(
        &env_template(
            "  - name: A\n    ports: [{ name: p }]\n    hostRequirements:\n      amounts:\n        - name: amount.worker.memory\n          min: \"{{ Param.Mem }}\"\n    script:\n      actions:\n        onRun:\n          command: a\n          args: [\"{{ Service.A.nope.port }}\"]\n",
            "",
        ),
        &[
            "1 validation error for EnvironmentTemplate\n",
            "services[0] -> script -> actions -> onRun -> args[0]:\n\tFailed to parse interpolation expression at [",
            &no_such_port("A", "nope", "p"),
        ],
    );
}
