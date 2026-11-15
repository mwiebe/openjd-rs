// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `CONTROL_FLOW` with `ENV_PARAMS` (RFC 0011 `<Environment>`): what an
//! `environment:` entry may read (item 2), the Environment selection made
//! at Job creation and applied once a Step's `enableIf` is `true` (item 3),
//! the checks made before Job creation only for a Step known to be enabled
//! (Template Schemas §7.6 rule 5), the value errors reported whatever a
//! Step's `enableIf` (RFC 0011 Open Question 16 (b)), and the
//! single-wrap-layer rule as soon as its outcome is determined (item 6).
//!
//! Failure tests assert each error's full path and message.

use openjd_expr::ExprValue;
use openjd_model::job::{Job, ProducerState, Step, StepBinding, StepInputs};
use openjd_model::outputs::TaskOutputs;
use openjd_model::template::{EnvironmentTemplate, JobTemplate};
use openjd_model::{
    advance_step_binding, check_environment_templates, create_job_with_environment_templates,
    decode_job_template, AttachedEnvironmentTemplate, CallerLimits, CreatedJob, EnvironmentScope,
    EnvironmentStatus, ModelError,
};

use crate::env_params_common::{
    assert_errors, env_ok, job_params_with, rendered_values, template_ctx, yaml,
};

/// The extensions the tests decode with.
const ALLOW: &[&str] = &[
    "EXPR",
    "ENV_PARAMS",
    "CONTROL_FLOW",
    "WRAP_ACTIONS",
    "FEATURE_BUNDLE_1",
];

const HEADER: &str = "specificationVersion: jobtemplate-2023-09
extensions: [EXPR, ENV_PARAMS, CONTROL_FLOW]
name: J
";

const WRAP_HEADER: &str = "specificationVersion: jobtemplate-2023-09
extensions: [EXPR, ENV_PARAMS, CONTROL_FLOW, WRAP_ACTIONS]
name: J
";

fn jt_ok(body: &str, header: &str) -> JobTemplate {
    let src = format!("{header}{body}");
    decode_job_template(yaml(&src), Some(ALLOW), &CallerLimits::default())
        .unwrap_or_else(|e| panic!("expected a valid Job Template, got:\n{e}\n\n{src}"))
}

fn check_err(body: &str, expected: &[&str]) {
    let src = format!("{HEADER}{body}");
    let err = decode_job_template(yaml(&src), Some(ALLOW), &CallerLimits::default())
        .expect_err("expected a validation error");
    assert_errors(&err.to_string(), "JobTemplate", expected);
}

fn create_with(
    jt: &JobTemplate,
    envs: &[EnvironmentTemplate],
    params: &[(&str, &str)],
) -> Result<CreatedJob, ModelError> {
    let values = job_params_with(jt, params, envs);
    let attached: Vec<AttachedEnvironmentTemplate<'_>> =
        envs.iter().map(AttachedEnvironmentTemplate::new).collect();
    create_job_with_environment_templates(jt, &attached, &values, &template_ctx(jt))
}

fn created(jt: &JobTemplate, params: &[(&str, &str)]) -> CreatedJob {
    create_with(jt, &[], params).unwrap_or_else(|e| panic!("job creation failed:\n{e}"))
}

fn step<'a>(job: &'a Job, name: &str) -> &'a Step {
    job.steps
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no Step {name}"))
}

fn index(job: &Job, name: &str) -> usize {
    job.steps.iter().position(|s| s.name == name).unwrap()
}

/// Advance Step `name` with producer `producer` succeeded with `outputs`.
fn advance(
    jt: &JobTemplate,
    envs: &[EnvironmentTemplate],
    params: &[(&str, &str)],
    job: &Job,
    name: &str,
    producer: &str,
    outputs: &[(&str, ExprValue)],
) -> Result<Step, openjd_model::job::BindingError> {
    let values = job_params_with(jt, params, envs);
    let task: TaskOutputs = outputs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    let mut inputs = StepInputs::default();
    inputs
        .add_producer(
            step(job, producer),
            ProducerState::Succeeded {
                task_outputs: &[task],
            },
        )
        .unwrap();
    advance_step_binding(
        jt,
        &values,
        &template_ctx(jt),
        job,
        index(job, name),
        &inputs,
        0,
    )
}

// ── Item 2: what an entry may read ────────────────────────────────

#[test]
fn an_entry_may_not_read_steps_loop_or_raw_loop_directly_or_through_let() {
    check_err(
        r#"
jobEnvironments:
- name: E
  activation: ON_REQUEST
  envParameterDefinitions: [{name: V, type: STRING}]
  script: {actions: {onEnter: {command: echo, args: ["{{Env.Param.V}}"]}}}
- name: E2
  activation: ON_REQUEST
  envParameterDefinitions: [{name: V, type: STRING}]
  script: {actions: {onEnter: {command: echo, args: ["{{Env.Param.V}}"]}}}
steps:
- name: A
  outputVariables: [{name: S, type: STRING}]
  script: {actions: {onRun: {command: echo, args: [a]}}}
- name: B
  let: ["t = Steps.A.Output.S", "u = t + 'x'"]
  dependencies:
  - {dependsOn: A}
  - {environment: E, parameters: {V: "{{Steps.A.Output.S}}"}}
  - {environment: E2, enableIf: "u == 'x'", parameters: {V: "{{u}}"}}
  script: {actions: {onRun: {command: echo, args: [b]}}}
- name: L
  loop:
    maxIterations: 2
    variables: [{name: N, type: INT, initial: "0", next: "{{Loop.N + 1}}"}]
    while: "Loop.N < 1"
    steps:
    - name: Body
      dependencies:
      - {environment: E, parameters: {V: "{{Loop.N}}"}}
      - {environment: E2, enableIf: "RawLoop.N == 0", parameters: {V: "x"}}
      script: {actions: {onRun: {command: echo, args: [b]}}}
"#,
        &[
            "steps[1] -> dependencies[1] -> parameters -> V:\n\t'Steps.A.Output.S': an \
             environment entry may not read Steps.*, Loop.*, or RawLoop.* (RFC 0011 \
             <Environment> item 2): which Environments a Step's Sessions enter, and its \
             Environment parameter values, depend only on values known when the Job is \
             created.",
            "steps[1] -> dependencies[2] -> parameters -> V:\n\t'u' is a let binding that reads \
             Steps.*, Loop.*, or RawLoop.*, which an environment entry may not read, directly \
             or through a let binding (RFC 0011 <Environment> item 2).",
            "steps[1] -> dependencies[2] -> enableIf:\n\t'u' is a let binding that reads \
             Steps.*, Loop.*, or RawLoop.*, which an environment entry may not read, directly \
             or through a let binding (RFC 0011 <Environment> item 2).",
            "steps[2] -> loop -> steps[0] -> dependencies[0] -> parameters -> V:\n\t'Loop.N': an \
             environment entry may not read Steps.*, Loop.*, or RawLoop.* (RFC 0011 \
             <Environment> item 2): which Environments a Step's Sessions enter, and its \
             Environment parameter values, depend only on values known when the Job is \
             created.",
            "steps[2] -> loop -> steps[0] -> dependencies[1] -> enableIf:\n\t'RawLoop.N': an \
             environment entry may not read Steps.*, Loop.*, or RawLoop.* (RFC 0011 \
             <Environment> item 2): which Environments a Step's Sessions enter, and its \
             Environment parameter values, depend only on values known when the Job is \
             created.",
        ],
    );
}

// ── Known to be enabled (§7.6 rule 5) ────────────────────────────

/// A Step whose Environment checks hold only if it runs: a disabled entry
/// for an `ALWAYS` Environment, constraints 5 and 6, and constraint 7.
fn selection_errors_step(enable_if: &str) -> String {
    format!(
        r#"
parameterDefinitions:
- {{name: F, type: BOOL, default: false}}
jobEnvironments:
- name: Lic
  activation: ALWAYS
  envParameterDefinitions: [{{name: Seats, type: INT}}]
  script: {{actions: {{onEnter: {{command: echo, args: ["{{{{Env.Param.Seats}}}}"]}}}}}}
- name: Opt
  activation: ON_REQUEST
  envParameterDefinitions: [{{name: N, type: INT, minValue: 1}}]
  script: {{actions: {{onEnter: {{command: echo, args: ["{{{{Env.Param.N}}}}"]}}}}}}
steps:
- name: S
  enableIf: {enable_if}
  dependencies:
  - {{environment: Lic, enableIf: "false"}}
  - {{environment: Opt, parameters: {{N: 0, Q: 1}}}}
  script: {{actions: {{onRun: {{command: echo, args: [s]}}}}}}
"#
    )
}

const SELECTION_ERRORS: &[&str] = &[
    "steps[0] -> dependencies[0] -> enableIf:\n\tis false, so Step 'S' would decline Environment \
     'Lic', whose activation is ALWAYS: no Step can leave out an ALWAYS Environment (Template \
     Schemas §3.2 item 3.2).",
    "steps[0] -> dependencies[1] -> parameters -> N:\n\tParameter 'N': value 0 is less than \
     minimum 1",
    "steps[0] -> dependencies[1] -> parameters -> Q:\n\tEnvironment 'Opt' declares no parameter \
     'Q'. Declared: N.",
    "steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a \
     default of each Environment that applies to it (every jobEnvironments entry with \
     activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, \
     and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 \
     constraint 7), whatever that Environment's own enableIf is; missing: 'Seats' of \
     Environment 'Lic' (the Step's 'environment:' dependency for it is disabled by its \
     enableIf, so it supplies nothing; Template Schemas §3.2 item 3.2).",
];

#[test]
fn selection_checks_are_made_at_template_validation_for_a_step_known_to_be_enabled() {
    check_err(
        &selection_errors_step("\"Param.F or true\""),
        SELECTION_ERRORS,
    );
}

#[test]
fn selection_checks_wait_for_job_creation_for_a_step_not_known_to_be_enabled() {
    let jt = jt_ok(&selection_errors_step("\"Param.F\""), HEADER);
    check_environment_templates(&jt, &[], &template_ctx(&jt))
        .unwrap_or_else(|e| panic!("expected no errors before Job creation, got:\n{e}"));
    // Skipped: its selection is never made.
    let job = created(&jt, &[("F", "false")]).job;
    assert_eq!(step(&job, "S").binding, StepBinding::Skipped);
    // Enabled at Job creation: its selection fails Job creation. The
    // disabled entry for `ALWAYS` stops the Step (§7.4.1 step 4), so
    // constraints 5 to 7 are not reported with it.
    let err = create_with(&jt, &[], &[("F", "true")]).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &[
            "steps[0] -> dependencies[0] -> enableIf:\n\tis false, so Step 'S' would decline \
             Environment 'Lic', whose activation is ALWAYS: no Step can leave out an ALWAYS \
             Environment (Template Schemas §3.2 item 3.2).",
        ],
    );
}

fn loop_body_requests(condition: &str) -> String {
    format!(
        r#"
parameterDefinitions:
- {{name: F, type: BOOL, default: false}}
jobEnvironments:
- name: Opt
  activation: ON_REQUEST
  envParameterDefinitions: [{{name: N, type: INT}}]
  script: {{actions: {{onEnter: {{command: echo, args: ["{{{{Env.Param.N}}}}"]}}}}}}
steps:
- name: L
  loop:
    maxIterations: 2
    {condition}
    steps:
    - name: Body
      dependencies: [{{environment: Opt}}]
      script: {{actions: {{onRun: {{command: echo, args: [b]}}}}}}
"#
    )
}

#[test]
fn a_body_step_is_known_to_be_enabled_only_when_its_loop_runs_its_first_iteration() {
    // An `until` loop runs its first iteration.
    check_err(
        &loop_body_requests("until: \"Loop.Iteration >= 0\""),
        &[
            "steps[0] -> loop -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every \
           parameter without a default of each Environment that applies to it (every \
           jobEnvironments entry with activation ALWAYS, each one with activation BY_DEFAULT \
           that the Step does not decline, and each one with activation ON_REQUEST that the \
           Step requests; Template Schemas §3.2 constraint 7), whatever that Environment's own \
           enableIf is; missing: 'N' of Environment 'Opt'.",
        ],
    );
    // A `while` that is not known may run none.
    jt_ok(&loop_body_requests("while: \"Param.F\""), HEADER);
}

// ── Open Question 16 (b) ──────────────────────────────────────────

#[test]
fn a_computable_value_error_is_reported_whether_or_not_the_step_is_known_to_be_enabled() {
    let src = format!(
        "{}{}",
        "specificationVersion: jobtemplate-2023-09
extensions: [EXPR, ENV_PARAMS, CONTROL_FLOW, FEATURE_BUNDLE_1]
name: J
",
        r#"
parameterDefinitions:
- {name: F, type: BOOL, default: false}
steps:
- name: S
  enableIf: "Param.F"
  stepEnvironments:
  - name: SE
    enableIf: "false"
    script: {actions: {onEnter: {command: echo, args: [x], timeout: "{{1 - 1}}"}}}
  script: {actions: {onRun: {command: echo, args: [s], timeout: "{{2 - 2}}"}}}
"#
    );
    let err = decode_job_template(yaml(&src), Some(ALLOW), &CallerLimits::default())
        .expect_err("expected a validation error");
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &[
            "steps[0] -> script -> actions -> onRun -> timeout:\n\ttimeout must be > 0.",
            "steps[0] -> stepEnvironments[0] -> script -> actions -> onEnter -> timeout:\n\t\
             timeout must be > 0.",
        ],
    );
}

// ── Item 3: the selection at Job creation and at step input binding ─

#[test]
fn entry_values_read_the_steps_creation_scope_including_let_bindings() {
    let jt = jt_ok(
        r#"
parameterDefinitions:
- {name: P, type: STRING, default: a}
jobEnvironments:
- name: E
  activation: ON_REQUEST
  envParameterDefinitions: [{name: V, type: STRING}]
  script: {actions: {onEnter: {command: echo, args: ["{{Env.Param.V}}"]}}}
steps:
- name: S
  let: ["y = Param.P + '!'", "z = y + '?'"]
  dependencies:
  - {environment: E, parameters: {V: "{{y}}-{{Param.P}}-{{Step.Name}}-{{z}}"}}
  script: {actions: {onRun: {command: echo, args: [s]}}}
"#,
        HEADER,
    );
    let job = created(&jt, &[]).job;
    assert_eq!(
        rendered_values(step(&job, "S")),
        ["JobTemplate/E: V=string:a!-a-S-a!?"]
    );
}

/// Steps B and C wait on A's `enableIf` inputs, so their selections are
/// applied at step input binding.
const PENDING_SELECTION: &str = r#"
jobEnvironments:
- name: E
  activation: ON_REQUEST
  envParameterDefinitions: [{name: V, type: INT, minValue: 1}]
  hostRequirements:
    amounts: [{name: amount.worker.vcpu, max: 4}]
  script: {actions: {onEnter: {command: echo, args: ["E={{Env.Param.V}}"]}}}
steps:
- name: A
  outputVariables: [{name: Ok, type: BOOL}, {name: Cpu, type: INT}]
  script: {actions: {onRun: {command: echo, args: [a]}}}
- name: B
  enableIf: "Steps.A.Output.Ok"
  dependencies: [{dependsOn: A}, {environment: E, parameters: {V: 0}}]
  script: {actions: {onRun: {command: echo, args: [b]}}}
- name: C
  enableIf: "Steps.A.Output.Ok"
  hostRequirements:
    amounts: [{name: amount.worker.vcpu, min: "{{Steps.A.Output.Cpu}}"}]
  dependencies: [{dependsOn: A}, {environment: E, parameters: {V: 2}}]
  script: {actions: {onRun: {command: echo, args: [c]}}}
"#;

fn pending_selection() -> JobTemplate {
    jt_ok(
        PENDING_SELECTION,
        "specificationVersion: jobtemplate-2023-09
extensions: [EXPR, ENV_PARAMS, CONTROL_FLOW, FEATURE_BUNDLE_1]
name: J
",
    )
}

#[test]
fn a_pending_steps_selection_errors_fail_it_when_its_enable_if_becomes_true() {
    let jt = pending_selection();
    // Job creation does not report B's error: B may be skipped.
    let job = created(&jt, &[]).job;
    let StepBinding::PendingInputs(p) = &step(&job, "B").binding else {
        panic!("B should be pending");
    };
    let selection = p
        .environment_selection
        .as_ref()
        .expect("B keeps its selection");
    assert_eq!(selection.errors.len(), 1);
    let err = advance(
        &jt,
        &[],
        &[],
        &job,
        "B",
        "A",
        &[("Ok", ExprValue::Bool(true)), ("Cpu", ExprValue::Int(1))],
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Step 'B' failed in its enablement group:\nModel validation error: 1 validation error \
         for JobTemplate\nsteps[1] -> dependencies[1] -> parameters -> V:\n\tParameter 'V': \
         value 0 is less than minimum 1"
    );
    // Skipped, it never applies the selection.
    let skipped = advance(
        &jt,
        &[],
        &[],
        &job,
        "B",
        "A",
        &[("Ok", ExprValue::Bool(false)), ("Cpu", ExprValue::Int(1))],
    )
    .unwrap();
    assert_eq!(skipped.binding, StepBinding::Skipped);
}

#[test]
fn a_pending_step_gets_its_environments_and_values_when_its_selection_is_applied() {
    let jt = pending_selection();
    let job = created(&jt, &[]).job;
    // Before the selection is applied, C has no values.
    assert!(step(&job, "C").environment_parameters.is_none());
    let c = advance(
        &jt,
        &[],
        &[],
        &job,
        "C",
        "A",
        &[("Ok", ExprValue::Bool(true)), ("Cpu", ExprValue::Int(2))],
    )
    .unwrap();
    assert_eq!(c.binding, StepBinding::Bound);
    assert_eq!(rendered_values(&c), ["JobTemplate/E: V=int:2"]);
    // It enters E, the only Job Environment, so `environments` is `None`
    // ("every one").
    assert!(c.enters_environment(&openjd_model::job::Document::JobTemplate, "E"));
    assert!(c.environments.is_none());
    // The Step's own requirement and the Environment's are combined.
    let hr = serde_json::to_value(c.host_requirements.as_ref().unwrap()).unwrap();
    assert_eq!(
        hr["amounts"],
        serde_json::json!([{"name": "amount.worker.vcpu", "min": 2.0, "max": 4.0}])
    );
}

#[test]
fn an_environment_host_requirement_conflict_fails_host_selection_at_binding() {
    let jt = pending_selection();
    let job = created(&jt, &[]).job;
    let err = advance(
        &jt,
        &[],
        &[],
        &job,
        "C",
        "A",
        &[("Ok", ExprValue::Bool(true)), ("Cpu", ExprValue::Int(8))],
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Step 'C' failed in its host selection group:\nModel validation error: 1 validation \
         error for JobTemplate\nsteps[2]:\n\tStep 'C': no host can satisfy the combined host \
         requirements for amount 'amount.worker.vcpu': steps[2] -> hostRequirements requires \
         min 8, and jobEnvironments[0] -> hostRequirements (Environment 'E') requires max 4. \
         Template Schemas §3.3.3."
    );
}

#[test]
fn an_error_for_several_steps_is_reported_for_each_step_whose_selection_is_applied() {
    // B is listed first and pending, A is enabled at Job creation; the
    // ALWAYS Environment's `enableIf` is false for both. Job creation fails
    // for A alone, whatever the Step order.
    let jt = jt_ok(
        r#"
parameterDefinitions:
- {name: F, type: BOOL, default: true}
jobEnvironments:
- name: E
  activation: ALWAYS
  enableIf: "not Param.F"
  script: {actions: {onEnter: {command: echo, args: [e]}}}
steps:
- name: B
  enableIf: "Steps.A.Output.Ok"
  dependencies: [{dependsOn: A}]
  script: {actions: {onRun: {command: echo, args: [b]}}}
- name: A
  outputVariables: [{name: Ok, type: BOOL}]
  script: {actions: {onRun: {command: echo, args: [a]}}}
"#,
        HEADER,
    );
    let err = create_with(&jt, &[], &[]).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &[
            "jobEnvironments[0] -> enableIf:\n\tEnvironment 'E' has activation ALWAYS, so it \
           cannot be disabled, but its enableIf is false for Step 'A' (Template Schemas §4 \
           item 4).",
        ],
    );
}

#[test]
fn a_pending_steps_selection_errors_keep_their_document_paths() {
    let jt = jt_ok(
        r#"
jobEnvironments:
- name: E
  activation: ON_REQUEST
  envParameterDefinitions: [{name: K, type: INT}]
  script: {actions: {onEnter: {command: echo, args: ["{{Env.Param.K}}"]}}}
steps:
- name: A
  outputVariables: [{name: Ok, type: BOOL}]
  script: {actions: {onRun: {command: echo, args: [a]}}}
- name: S
  enableIf: "Steps.A.Output.Ok"
  dependencies:
  - {dependsOn: A}
  - {environment: E, parameters: {K: 4, Q: 1}}
  - {environment: W, parameters: {Z: 1}}
  script: {actions: {onRun: {command: echo, args: [s]}}}
"#,
        HEADER,
    );
    let w = env_ok(
        r#"
specificationVersion: environment-2023-09
extensions: [EXPR, ENV_PARAMS]
environment:
  name: W
  activation: ON_REQUEST
  envParameterDefinitions: [{name: Z, type: INT, minValue: 5}]
  script: {actions: {onEnter: {command: echo, args: ["{{Env.Param.Z}}"]}}}
"#,
    );
    let envs = [w];
    let job = create_with(&jt, &envs, &[]).unwrap().job;
    let err = advance(
        &jt,
        &envs,
        &[],
        &job,
        "S",
        "A",
        &[("Ok", ExprValue::Bool(true))],
    )
    .unwrap_err();
    let message = err.to_string();
    let (first, rest) = message.split_once('\n').unwrap();
    assert_eq!(first, "Step 'S' failed in its enablement group:");
    assert_errors(
        rest,
        "Submission",
        &[
            "JobTemplate -> steps[1] -> dependencies[1] -> parameters -> Q:\n\tEnvironment 'E' \
             declares no parameter 'Q'. Declared: K.",
            "JobTemplate -> steps[1] -> dependencies[2] -> parameters -> Z:\n\tParameter 'Z': \
             value 1 is less than minimum 5",
        ],
    );
}

// ── Item 4: Step Environment statuses ─────────────────────────────

#[test]
fn step_environment_statuses_say_what_the_remaining_group_decided() {
    let jt = jt_ok(
        r#"
steps:
- name: P
  outputVariables: [{name: Go, type: BOOL}]
  script: {actions: {onRun: {command: echo, args: [p]}}}
- name: S
  stepEnvironments:
  - {name: SOFF, enableIf: "false", script: {actions: {onEnter: {command: echo, args: [x]}}}}
  - {name: SON, script: {actions: {onEnter: {command: echo, args: [x]}}}}
  script: {actions: {onRun: {command: echo, args: [s]}}}
- name: C
  dependencies: [{dependsOn: P}]
  stepEnvironments:
  - {name: SE, enableIf: "Steps.P.Output.Go", script: {actions: {onEnter: {command: echo, args: [x]}}}}
  script: {actions: {onRun: {command: echo, args: [c]}}}
"#,
        HEADER,
    );
    let created = created(&jt, &[]);
    let statuses = |i: usize| -> Vec<(String, EnvironmentStatus)> {
        created.environment_statuses[i]
            .environments
            .iter()
            .filter(|s| s.scope == EnvironmentScope::Step)
            .map(|s| (s.name.clone(), s.status))
            .collect()
    };
    assert_eq!(
        statuses(1),
        [
            ("SOFF".to_string(), EnvironmentStatus::Disabled),
            ("SON".to_string(), EnvironmentStatus::Entered),
        ]
    );
    assert_eq!(created.disabled_environments[1].step_environments, ["SOFF"]);
    // C's remaining group waits on P, so SE is not decided.
    assert!(statuses(2).is_empty());
    assert!(created.disabled_environments[2]
        .step_environments
        .is_empty());
}

// ── Item 6: the single-wrap-layer rule ───────────────────────────

#[test]
fn the_single_wrap_layer_rule_is_checked_as_soon_as_its_outcome_is_determined() {
    // B's remaining group waits on C, but its Sessions certainly enter two
    // wrapping Environments, so Job creation fails.
    let wrap = r#"
  script:
    actions:
      onWrapEnvEnter: {command: echo, args: [x]}
      onWrapTaskRun: {command: echo, args: [x]}
      onWrapEnvExit: {command: echo, args: [x]}"#;
    let jt = jt_ok(
        &format!(
            r#"
parameterDefinitions:
- {{name: W, type: BOOL, default: true}}
jobEnvironments:
- name: JW{wrap}
- name: JW2
  activation: ON_REQUEST{wrap}
steps:
- name: C
  outputVariables: [{{name: X, type: STRING}}]
  script: {{actions: {{onRun: {{command: echo, args: [c]}}}}}}
- name: B
  dependencies: [{{dependsOn: C}}, {{environment: JW2, enableIf: "Param.W"}}]
  let: ["c = Steps.C.Output.X"]
  script: {{actions: {{onRun: {{command: echo, args: ["{{{{c}}}}"]}}}}}}
"#
        ),
        WRAP_HEADER,
    );
    let err = create_with(&jt, &[], &[]).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &[
            "steps[1]:\n\tits Sessions would enter 2 Environments that define wrap hooks \
           (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'JW', 'JW2'. A Session may have at \
           most one Environment defining wrap hooks (RFC 0008).",
        ],
    );
    // With W false, B's Sessions enter one wrapping Environment.
    let job = created(&jt, &[("W", "false")]).job;
    assert!(matches!(
        step(&job, "B").binding,
        StepBinding::PendingInputs(_)
    ));
}

#[test]
fn the_single_wrap_layer_rule_waits_for_job_creation_for_a_step_not_known_to_be_enabled() {
    let hooks = |indent: &str| {
        [
            "script:",
            "  actions:",
            "    onWrapEnvEnter: {command: echo, args: [x]}",
            "    onWrapTaskRun: {command: echo, args: [x]}",
            "    onWrapEnvExit: {command: echo, args: [x]}",
        ]
        .iter()
        .map(|l| format!("\n{indent}{l}"))
        .collect::<String>()
    };
    let (job_wrap, step_wrap) = (hooks("  "), hooks("    "));
    let src = |enable_if: &str| {
        format!(
            r#"
parameterDefinitions:
- {{name: F, type: BOOL, default: false}}
jobEnvironments:
- name: JW{job_wrap}
steps:
- name: S
  enableIf: {enable_if}
  stepEnvironments:
  - name: SW{step_wrap}
  script: {{actions: {{onRun: {{command: echo, args: [s]}}}}}}
"#
        )
    };
    let decode = |body: &str| {
        decode_job_template(
            yaml(&format!("{WRAP_HEADER}{body}")),
            Some(ALLOW),
            &CallerLimits::default(),
        )
    };
    let err = decode(&src("\"Param.F or true\"")).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &[
            "steps[0] -> stepEnvironments:\n\tonly one environment in the session stack may \
             define any of onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit (RFC 0008).",
        ],
    );
    let jt = decode(&src("\"Param.F\"")).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        step(&created(&jt, &[]).job, "S").binding,
        StepBinding::Skipped
    );
}

// ── Round 2 ───────────────────────────────────────────────────────

const FB1_HEADER: &str = "specificationVersion: jobtemplate-2023-09
extensions: [EXPR, ENV_PARAMS, CONTROL_FLOW, FEATURE_BUNDLE_1]
name: J
";

const FB1_WRAP_HEADER: &str = "specificationVersion: jobtemplate-2023-09
extensions: [EXPR, ENV_PARAMS, CONTROL_FLOW, FEATURE_BUNDLE_1, WRAP_ACTIONS]
name: J
";

#[test]
fn a_steps_own_discarded_host_requirements_are_listed_first() {
    let jt = jt_ok(
        r#"
jobEnvironments:
- name: E
  hostRequirements:
    amounts:
    - {name: amount.worker.gpu, min: "{{ null }}"}
    - {name: amount.worker.disk.scratch, min: 1}
  script: {actions: {onEnter: {command: echo, args: [e]}}}
steps:
- name: S
  hostRequirements:
    amounts:
    - {name: amount.worker.vcpu, min: "{{ null }}", max: "{{ null }}"}
    - {name: amount.worker.memory, min: 1}
  script: {actions: {onRun: {command: echo, args: [s]}}}
"#,
        FB1_HEADER,
    );
    let created = created(&jt, &[]);
    let discarded: Vec<(Option<String>, String)> = created.discarded_host_requirements[0]
        .iter()
        .map(|d| {
            (
                d.environment.as_ref().map(|e| e.environment.clone()),
                d.name.clone(),
            )
        })
        .collect();
    assert_eq!(
        discarded,
        [
            (None, "amount.worker.vcpu".to_string()),
            (Some("E".to_string()), "amount.worker.gpu".to_string()),
        ]
    );
}

#[test]
fn an_abort_at_job_creation_keeps_the_selection_errors_found_before_it() {
    // C (bound first) has a selection error; A's own host requirement
    // conflicts with its Environment's. Both are reported.
    let jt = jt_ok(
        r#"
parameterDefinitions:
- {name: M, type: INT, default: 2}
jobEnvironments:
- name: E
  activation: ON_REQUEST
  hostRequirements:
    amounts: [{name: amount.worker.vcpu, min: 4}]
  script: {actions: {onEnter: {command: echo, args: [e]}}}
- name: H
  activation: ON_REQUEST
  envParameterDefinitions: [{name: Q, type: INT}]
  script: {actions: {onEnter: {command: echo, args: ["{{Env.Param.Q}}"]}}}
steps:
- name: C
  enableIf: "Param.M > 0"
  dependencies: [{environment: H}]
  script: {actions: {onRun: {command: echo, args: [c]}}}
- name: A
  enableIf: "Param.M > 0"
  dependencies: [{dependsOn: C}, {environment: E}]
  hostRequirements:
    amounts: [{name: amount.worker.vcpu, max: "{{Param.M}}"}]
  script: {actions: {onRun: {command: echo, args: [a]}}}
"#,
        FB1_HEADER,
    );
    let err = create_with(&jt, &[], &[]).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &[
            "steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a \
             default of each Environment that applies to it (every jobEnvironments entry with \
             activation ALWAYS, each one with activation BY_DEFAULT that the Step does not \
             decline, and each one with activation ON_REQUEST that the Step requests; Template \
             Schemas §3.2 constraint 7), whatever that Environment's own enableIf is; missing: \
             'Q' of Environment 'H'.",
            "steps[1]:\n\tStep 'A': no host can satisfy the combined host requirements for amount \
             'amount.worker.vcpu': jobEnvironments[0] -> hostRequirements (Environment 'E') \
             requires min 4, and steps[1] -> hostRequirements requires max 2. Template Schemas \
             §3.3.3.",
        ],
    );
}

fn wrap_hooks(indent: &str) -> String {
    [
        "script:",
        "  actions:",
        "    onWrapEnvEnter: {command: echo, args: [x]}",
        "    onWrapTaskRun: {command: echo, args: [x]}",
        "    onWrapEnvExit: {command: echo, args: [x]}",
    ]
    .iter()
    .map(|l| format!("\n{indent}{l}"))
    .collect()
}

/// A wrapping Job Environment JW that C declines, and Step B, whose
/// remaining group waits on C, with a wrapping Step Environment SW whose
/// `enableIf` is `sw_enable_if`. B's `enableIf` reads a Job Parameter, so
/// template validation leaves it to Job creation.
fn early_wrap_template(lets: &str, sw_enable_if: &str) -> JobTemplate {
    jt_ok(
        &format!(
            r#"
parameterDefinitions:
- {{name: S, type: STRING, default: "1"}}
jobEnvironments:
- name: JW{}
steps:
- name: C
  dependencies: [{{environment: JW, enableIf: "false"}}]
  outputVariables: [{{name: X, type: STRING}}]
  script: {{actions: {{onRun: {{command: echo, args: [c]}}}}}}
- name: B
  enableIf: "Param.S != ''"
  dependencies: [{{dependsOn: C}}]
  let: [{lets}"c = Steps.C.Output.X"]
  stepEnvironments:
  - name: SW
    enableIf: "{sw_enable_if}"{}
  script: {{actions: {{onRun: {{command: echo, args: ["{{{{c}}}}"]}}}}}}
"#,
            wrap_hooks("  "),
            wrap_hooks("    ")
        ),
        WRAP_HEADER,
    )
}

const B_TWO_LAYERS: &str = "steps[1]:\n\tits Sessions would enter 2 Environments that define wrap \
                            hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'JW', 'SW' \
                            (stepEnvironments). A Session may have at most one Environment \
                            defining wrap hooks (RFC 0008).";

#[test]
fn a_step_environment_known_to_be_entered_counts_in_the_early_wrap_check() {
    // Known `true` directly, and through a `let` that reads only concrete
    // values.
    for (lets, enable_if) in [
        ("", "Steps.C.Output.X == 'a' or true"),
        ("\"t = int(Param.S) > 0\", ", "t"),
    ] {
        let jt = early_wrap_template(lets, enable_if);
        let err = create_with(&jt, &[], &[]).unwrap_err();
        assert_errors(&err.to_string(), "JobTemplate", &[B_TWO_LAYERS]);
    }
    // Not known: B stays pending.
    let jt = early_wrap_template("", "Steps.C.Output.X == 'a'");
    let job = created(&jt, &[]).job;
    assert!(matches!(
        step(&job, "B").binding,
        StepBinding::PendingInputs(_)
    ));
}

#[test]
fn a_let_binding_that_fails_leaves_the_wrap_rule_undetermined() {
    // `n` fails for S=a, so the remaining group fails on it; the early
    // check reports nothing in its place.
    let jt = early_wrap_template("\"n = int(Param.S)\", ", "n > 0 or true");
    let job = created(&jt, &[("S", "a")]).job;
    assert!(matches!(
        step(&job, "B").binding,
        StepBinding::PendingInputs(_)
    ));
    let err = advance(
        &jt,
        &[],
        &[("S", "a")],
        &job,
        "B",
        "C",
        &[("X", ExprValue::String("x".into()))],
    )
    .unwrap_err();
    let message = err.to_string();
    assert!(
        message.starts_with("Step 'B' failed in its remaining group:")
            && message.contains("steps[1] -> let[0]:"),
        "{message}"
    );
}

#[test]
fn a_selection_applied_at_creation_is_kept_by_later_binding_calls() {
    // S's `enableIf` is absent, so its selection is applied at Job
    // creation; its Tasks wait on P.
    let jt = jt_ok(
        &format!(
            r#"
jobEnvironments:
- name: E
  activation: ON_REQUEST
  envParameterDefinitions: [{{name: V, type: STRING}}]
  hostRequirements:
    amounts: [{{name: amount.worker.vcpu, max: 4}}]
  script: {{actions: {{onEnter: {{command: echo, args: ["{{{{Env.Param.V}}}}"]}}}}}}
- name: F
  activation: ON_REQUEST
  script: {{actions: {{onEnter: {{command: echo, args: [f]}}}}}}
- name: W
  activation: ON_REQUEST{}
steps:
- name: P
  outputVariables: [{{name: N, type: INT}}]
  script: {{actions: {{onRun: {{command: echo, args: [p]}}}}}}
- name: S
  dependencies: [{{dependsOn: P}}, {{environment: E, parameters: {{V: hello}}}}, {{environment: W}}]
  parameterSpace:
    taskParameterDefinitions: [{{name: I, type: INT, range: "1-{{{{Steps.P.Output.N}}}}"}}]
  hostRequirements:
    amounts: [{{name: amount.worker.vcpu, min: 2}}]
  stepEnvironments:
  - name: SW
    enableIf: "Steps.P.Output.N > 5"{}
  script: {{actions: {{onRun: {{command: echo, args: [s]}}}}}}
"#,
            wrap_hooks("  "),
            wrap_hooks("    ")
        ),
        FB1_WRAP_HEADER,
    );
    let job = created(&jt, &[]).job;
    let s = advance(&jt, &[], &[], &job, "S", "P", &[("N", ExprValue::Int(2))]).unwrap();
    assert_eq!(s.binding, StepBinding::Bound);
    assert_eq!(rendered_values(&s), ["JobTemplate/E: V=string:hello"]);
    let doc = openjd_model::job::Document::JobTemplate;
    assert!(s.enters_environment(&doc, "E"));
    assert!(!s.enters_environment(&doc, "F"));
    let hr = serde_json::to_value(s.host_requirements.as_ref().unwrap()).unwrap();
    assert_eq!(
        hr["amounts"],
        serde_json::json!([{"name": "amount.worker.vcpu", "min": 2.0, "max": 4.0}])
    );
    // The selection's wrap layer W is still counted at the later call.
    let err = advance(&jt, &[], &[], &job, "S", "P", &[("N", ExprValue::Int(6))]).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Step 'S' failed in its remaining group:\nModel validation error: 1 validation error \
         for JobTemplate\nsteps[1]:\n\tits Sessions would enter 2 Environments that define wrap \
         hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'W', 'SW' (stepEnvironments). \
         A Session may have at most one Environment defining wrap hooks (RFC 0008)."
    );
}

#[test]
fn an_environment_host_requirement_error_is_reported_for_each_applied_step() {
    // E's requirement fails to resolve for every Step; P is enabled at
    // Job creation, A is pending (listed before P's dependents), B is
    // enabled. Job creation reports it for P and B; A's copy waits.
    let jt = jt_ok(
        r#"
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: Index, type: INT, default: 5}]
  hostRequirements:
    amounts: [{name: amount.worker.vcpu, min: "{{ [1][Env.Param.Index] }}"}]
  script: {actions: {onEnter: {command: echo, args: [e]}}}
steps:
- name: A
  enableIf: "Steps.P.Output.Ok"
  dependencies: [{dependsOn: P}]
  script: {actions: {onRun: {command: echo, args: [a]}}}
- name: P
  outputVariables: [{name: Ok, type: BOOL}]
  script: {actions: {onRun: {command: echo, args: [p]}}}
- name: B
  script: {actions: {onRun: {command: echo, args: [b]}}}
"#,
        FB1_HEADER,
    );
    let err = create_with(&jt, &[], &[]).unwrap_err().to_string();
    assert!(
        err.starts_with("Model validation error: 2 validation errors for JobTemplate"),
        "{err}"
    );
    assert_eq!(err.matches("Resolved for Step 'P'.").count(), 1, "{err}");
    assert_eq!(err.matches("Resolved for Step 'B'.").count(), 1, "{err}");
    assert!(!err.contains("'A'"), "{err}");
}

#[test]
fn check_environment_templates_leaves_a_step_not_known_to_be_enabled_to_job_creation() {
    let src = |enable_if: &str| {
        format!(
            r#"
jobEnvironments:
- name: Lic
  activation: ALWAYS
  script: {{actions: {{onEnter: {{command: echo, args: [l]}}}}}}
steps:
- name: P
  outputVariables: [{{name: Ok, type: BOOL}}]
  script: {{actions: {{onRun: {{command: echo, args: [p]}}}}}}
- name: S
  enableIf: {enable_if}
  dependencies:
  - {{dependsOn: P}}
  - {{environment: Missing}}
  - {{environment: X, enableIf: "false"}}
  script: {{actions: {{onRun: {{command: echo, args: [s]}}}}}}
"#
        )
    };
    let x = env_ok(
        r#"
specificationVersion: environment-2023-09
extensions: [EXPR, ENV_PARAMS]
environment:
  name: X
  activation: ALWAYS
  script: {actions: {onEnter: {command: echo, args: [x]}}}
"#,
    );
    let attached = [AttachedEnvironmentTemplate::new(&x)];
    let known = jt_ok(&src("\"true\""), HEADER);
    let err = check_environment_templates(&known, &attached, &template_ctx(&known)).unwrap_err();
    assert_errors(
        &err.to_string(),
        "Submission",
        &[
            "JobTemplate -> steps[1] -> dependencies[1] -> environment:\n\tnames Environment \
             'Missing', which is not a jobEnvironments entry of the Job Template, and none of \
             the attached Environment Templates (EnvironmentTemplate[0]) defines it. The entry \
             is required; set 'required: false' if the Step can run without the Environment \
             (Template Schemas §3.2.1 item 2).",
            "JobTemplate -> steps[1] -> dependencies[2] -> enableIf:\n\tis false, so Step 'S' \
             would decline Environment 'X' (EnvironmentTemplate[0]), whose activation is \
             ALWAYS: no Step can leave out an ALWAYS Environment (Template Schemas §3.2 item \
             3.2).",
        ],
    );
    let pending = jt_ok(&src("\"Steps.P.Output.Ok\""), HEADER);
    check_environment_templates(&pending, &attached, &template_ctx(&pending))
        .unwrap_or_else(|e| panic!("expected no errors before Job creation, got:\n{e}"));
}

#[test]
fn check_environment_templates_reports_an_ambiguous_entry_only_for_a_step_known_to_be_enabled() {
    // An ambiguous name is an error only for an enabled entry (Template
    // Schemas §3.2.1 item 3), so before Job creation it is reported only
    // for a Step known to be enabled (RFC 0011 `<Environment>` item 3).
    let src = |enable_if: &str| {
        format!(
            r#"
steps:
- name: P
  outputVariables: [{{name: Ok, type: BOOL}}]
  script: {{actions: {{onRun: {{command: echo, args: [p]}}}}}}
- name: S
  enableIf: {enable_if}
  dependencies:
  - {{dependsOn: P}}
  - {{environment: X, required: false}}
  script: {{actions: {{onRun: {{command: echo, args: [s]}}}}}}
"#
        )
    };
    let envs = [attached_x("ON_REQUEST", ""), attached_x("ON_REQUEST", "")];
    let attached: Vec<_> = envs.iter().map(AttachedEnvironmentTemplate::new).collect();
    let known = jt_ok(&src("\"true\""), HEADER);
    let err = check_environment_templates(&known, &attached, &template_ctx(&known)).unwrap_err();
    assert_errors(
        &err.to_string(),
        "Submission",
        &[
            "JobTemplate -> steps[1] -> dependencies[1] -> environment:\n\tnames Environment 'X', \
           which 2 attached Environment Templates define (EnvironmentTemplate[0], \
           EnvironmentTemplate[1]); an entry must match exactly one (Template Schemas §3.2.1 \
           item 3).",
        ],
    );
    let pending = jt_ok(&src("\"Steps.P.Output.Ok\""), HEADER);
    check_environment_templates(&pending, &attached, &template_ctx(&pending))
        .unwrap_or_else(|e| panic!("expected no errors before Job creation, got:\n{e}"));
}

#[test]
fn an_entry_value_of_a_step_not_known_to_be_enabled_gets_only_its_scope_check() {
    let src = |value: &str| {
        format!(
            r#"
parameterDefinitions:
- {{name: X, type: STRING, default: abc}}
jobEnvironments:
- name: E
  activation: ON_REQUEST
  envParameterDefinitions: [{{name: N, type: INT}}]
  script: {{actions: {{onEnter: {{command: echo, args: ["{{{{Env.Param.N}}}}"]}}}}}}
steps:
- name: A
  outputVariables: [{{name: Ok, type: BOOL}}]
  script: {{actions: {{onRun: {{command: echo, args: [a]}}}}}}
- name: S
  enableIf: "Steps.A.Output.Ok"
  dependencies: [{{dependsOn: A}}, {{environment: E, parameters: {{N: "{value}"}}}}]
  script: {{actions: {{onRun: {{command: echo, args: [s]}}}}}}
"#
        )
    };
    // A value that does not convert is left to the Step's selection.
    jt_ok(&src("{{ Param.X }}"), HEADER);
    check_err(
        &src("{{ Nope }}"),
        &["steps[1] -> dependencies[1] -> parameters -> N:\n\tUndefined variable: 'Nope'."],
    );
}

#[test]
fn a_body_step_is_not_known_to_be_enabled_when_its_loops_enable_if_is_not() {
    let src = loop_body_requests("until: \"Loop.Iteration >= 0\"").replace(
        "- name: L\n  loop:",
        "- name: L\n  enableIf: \"Param.F\"\n  loop:",
    );
    jt_ok(&src, HEADER);
}

#[test]
fn created_job_lists_are_kept_for_a_step_whose_remaining_group_waits() {
    // S's selection is applied at creation (no enableIf), but its remaining
    // group waits on P; A's `enableIf` waits on P.
    let jt = jt_ok(
        r#"
jobEnvironments:
- name: E
  activation: ON_REQUEST
  script: {actions: {onEnter: {command: echo, args: [e]}}}
- name: G
  enableIf: "false"
  script: {actions: {onEnter: {command: echo, args: [g]}}}
steps:
- name: P
  outputVariables: [{name: Go, type: BOOL}]
  script: {actions: {onRun: {command: echo, args: [p]}}}
- name: S
  dependencies: [{dependsOn: P}, {environment: E}]
  stepEnvironments:
  - {name: SE, enableIf: "Steps.P.Output.Go", script: {actions: {onEnter: {command: echo, args: [x]}}}}
  script: {actions: {onRun: {command: echo, args: [s]}}}
- name: A
  enableIf: "Steps.P.Output.Go"
  dependencies: [{dependsOn: P}, {environment: E}]
  script: {actions: {onRun: {command: echo, args: [a]}}}
"#,
        HEADER,
    );
    let created = created(&jt, &[]);
    let statuses = |i: usize| -> Vec<(String, EnvironmentStatus)> {
        created.environment_statuses[i]
            .environments
            .iter()
            .map(|s| (s.name.clone(), s.status))
            .collect()
    };
    assert_eq!(
        statuses(1),
        [
            ("E".to_string(), EnvironmentStatus::Entered),
            ("G".to_string(), EnvironmentStatus::Disabled),
        ]
    );
    let names: Vec<&str> = created.disabled_environments[1]
        .environments
        .iter()
        .map(|e| e.environment.as_str())
        .collect();
    assert_eq!(names, ["G"]);
    // A's selection is not applied at creation.
    assert!(statuses(2).is_empty());
    assert!(created.disabled_environments[2].environments.is_empty());
}

// ── Round 3 ───────────────────────────────────────────────────────

#[test]
fn a_pending_steps_disabled_entry_for_an_always_environment_fails_its_binding() {
    // The error is a §7.4.1 step 4 one, which stops the Step's later
    // checks; its selection still carries it.
    let jt = jt_ok(
        r#"
jobEnvironments:
- name: Lic
  activation: ALWAYS
  script: {actions: {onEnter: {command: echo, args: [l]}}}
steps:
- name: A
  outputVariables: [{name: Ok, type: BOOL}]
  script: {actions: {onRun: {command: echo, args: [a]}}}
- name: B
  enableIf: "Steps.A.Output.Ok"
  dependencies: [{dependsOn: A}, {environment: Lic, enableIf: "false"}]
  script: {actions: {onRun: {command: echo, args: [b]}}}
"#,
        HEADER,
    );
    let job = created(&jt, &[]).job;
    let err = advance(
        &jt,
        &[],
        &[],
        &job,
        "B",
        "A",
        &[("Ok", ExprValue::Bool(true))],
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Step 'B' failed in its enablement group:\nModel validation error: 1 validation error \
         for JobTemplate\nsteps[1] -> dependencies[1] -> enableIf:\n\tis false, so Step 'B' \
         would decline Environment 'Lic', whose activation is ALWAYS: no Step can leave out an \
         ALWAYS Environment (Template Schemas §3.2 item 3.2)."
    );
}

/// C has a selection error, A and B the aborts; A's `let` error is
/// collected before the abort.
fn abort_template(b_enable_if: &str) -> JobTemplate {
    jt_ok(
        &format!(
            r#"
parameterDefinitions:
- {{name: Z, type: INT, default: 0}}
- {{name: M, type: INT, default: 2}}
jobEnvironments:
- name: H
  activation: ON_REQUEST
  envParameterDefinitions: [{{name: Q, type: INT}}]
  script: {{actions: {{onEnter: {{command: echo, args: ["{{{{Env.Param.Q}}}}"]}}}}}}
steps:
- name: A
  let: ["x = 10 // Param.Z"]
  script: {{actions: {{onRun: {{command: echo, args: ["{{{{x}}}}"]}}}}}}
- name: C
  enableIf: "Param.M > 0"
  dependencies: [{{dependsOn: A}}, {{environment: H}}]
  script: {{actions: {{onRun: {{command: echo, args: [c]}}}}}}
- name: B
  enableIf: "{b_enable_if}"
  dependencies: [{{dependsOn: C}}, {{environment: H}}]
  script: {{actions: {{onRun: {{command: echo, args: [b]}}}}}}
"#
        ),
        HEADER,
    )
}

const C_MISSING_Q: &str = "steps[1]:\n\tmust supply, in an 'environment:' dependency, every \
                           parameter without a default of each Environment that applies to it \
                           (every jobEnvironments entry with activation ALWAYS, each one with \
                           activation BY_DEFAULT that the Step does not decline, and each one \
                           with activation ON_REQUEST that the Step requests; Template Schemas \
                           §3.2 constraint 7), whatever that Environment's own enableIf is; \
                           missing: 'Q' of Environment 'H'.";

#[test]
fn an_abort_is_reported_with_every_error_found_before_it_but_not_its_own_selection() {
    // B's `enableIf` fails, so its selection (also missing Q) is never
    // made; A's `let` error and C's selection error are reported with it.
    let jt = abort_template("10 // Param.Z > 0");
    let err = create_with(&jt, &[], &[]).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &[
            "steps[0] -> let[0]:\n\tInvalid expression in let binding 'x': Division by zero",
            C_MISSING_Q,
            "steps[2] -> enableIf:\n\tcould not be evaluated: Division by zero",
        ],
    );
}

#[test]
fn an_abort_with_attached_environment_templates_is_reported_for_the_submission() {
    let jt = abort_template("10 // Param.Z > 0");
    let w = env_ok(
        r#"
specificationVersion: environment-2023-09
extensions: [EXPR, ENV_PARAMS]
environment:
  name: W
  activation: ON_REQUEST
  script: {actions: {onEnter: {command: echo, args: [w]}}}
"#,
    );
    let err = create_with(&jt, &[w], &[]).unwrap_err();
    assert_errors(
        &err.to_string(),
        "Submission",
        &[
            "JobTemplate -> steps[0] -> let[0]:\n\tInvalid expression in let binding 'x': \
             Division by zero",
            &format!("JobTemplate -> {C_MISSING_Q}"),
            "JobTemplate -> steps[2] -> enableIf:\n\tcould not be evaluated: Division by zero",
        ],
    );
}

#[test]
fn an_unrelated_let_that_fails_does_not_stop_the_early_wrap_check() {
    // `y` fails for S=a but SW's `enableIf` does not read it.
    let jt = early_wrap_template("\"y = int(Param.S)\", ", "true");
    let err = create_with(&jt, &[], &[("S", "a")]).unwrap_err();
    assert_errors(&err.to_string(), "JobTemplate", &[B_TWO_LAYERS]);
    // A `let` that reads the failing one is undetermined too.
    let jt = early_wrap_template("\"n = int(Param.S)\", \"m = n + 1\", ", "m > 0 or true");
    let job = created(&jt, &[("S", "a")]).job;
    assert!(matches!(
        step(&job, "B").binding,
        StepBinding::PendingInputs(_)
    ));
}

#[test]
fn an_unreachable_attachment_is_rejected_when_every_step_is_stopped_and_skipped() {
    // S's required entry names nothing (a step 3 error, which stops it),
    // and S is skipped; the attachment named like Job Environment E, with
    // a parameter no Step can supply, is still rejected (RFC 0011
    // `<Environment>` item 3, "What stays at Job creation").
    let jt = jt_ok(
        r#"
parameterDefinitions:
- {name: F, type: BOOL, default: false}
jobEnvironments:
- name: E
  activation: ON_REQUEST
  script: {actions: {onEnter: {command: echo, args: [e]}}}
steps:
- name: S
  enableIf: "Param.F"
  dependencies: [{environment: X}]
  script: {actions: {onRun: {command: echo, args: [s]}}}
"#,
        HEADER,
    );
    let e = env_ok(
        r#"
specificationVersion: environment-2023-09
extensions: [EXPR, ENV_PARAMS]
environment:
  name: E
  activation: ALWAYS
  envParameterDefinitions: [{name: P, type: STRING}]
  script: {actions: {onEnter: {command: echo, args: ["{{Env.Param.P}}"]}}}
"#,
    );
    let err = create_with(&jt, &[e], &[]).unwrap_err().to_string();
    assert!(
        err.starts_with(
            "Model validation error: 1 validation error for Submission\n\
                         EnvironmentTemplate[0] -> environment -> envParameterDefinitions:\n\t\
                         Environment 'E' has the same name as a jobEnvironments entry"
        ),
        "{err}"
    );
}

// ── Round 4 ───────────────────────────────────────────────────────

#[test]
fn a_failed_resolved_default_is_reported_for_each_step_whose_selection_is_applied() {
    // S is skipped, T runs: T's selection still fails on E's default.
    let jt = jt_ok(
        r#"
parameterDefinitions:
- {name: F, type: BOOL, default: false}
- {name: V, type: STRING, default: ab}
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: D, type: STRING, maxLength: 1, default: "{{Param.V}}"}]
  script: {actions: {onEnter: {command: echo, args: ["{{Env.Param.D}}"]}}}
steps:
- name: S
  enableIf: "Param.F"
  script: {actions: {onRun: {command: echo, args: [s]}}}
- name: T
  script: {actions: {onRun: {command: echo, args: [t]}}}
"#,
        HEADER,
    );
    let err = create_with(&jt, &[], &[]).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &[
            "jobEnvironments[0] -> envParameterDefinitions[0] -> default:\n\tfor Step 'T', which \
           does not supply this parameter: Parameter 'D': value length 2 exceeds maximum 1",
        ],
    );
}

#[test]
fn an_ambiguous_entry_of_a_step_whose_enable_if_fails_is_not_checked() {
    // An ambiguous name is an error only for an enabled entry (Template
    // Schemas §3.2.1 item 3), so it is part of the Step's Environment
    // selection, which a Step whose `enableIf` fails never makes.
    let jt = jt_ok(
        r#"
parameterDefinitions:
- {name: S, type: STRING, default: a}
steps:
- name: S
  enableIf: "int(Param.S) > 0"
  dependencies: [{environment: X}]
  script: {actions: {onRun: {command: echo, args: [s]}}}
"#,
        HEADER,
    );
    let envs = [attached_x("ON_REQUEST", ""), attached_x("ON_REQUEST", "")];
    let err = create_with(&jt, &envs, &[]).unwrap_err().to_string();
    assert_eq!(
        err,
        "Model validation error: 1 validation error for Submission\nJobTemplate -> steps[0] -> \
         enableIf:\n\tcould not be evaluated: Cannot convert 'a' to int\n  int(Param.S) > 0\n  \
         ^~~~~~~~~~~~"
    );
}

#[test]
fn the_early_wrap_check_leaves_out_attribute_reads_of_a_failing_let() {
    // `q` fails for S=a; reading `q.name`, directly or through `m`, is not
    // determined.
    let q = "\"q = path('/a/b') if int(Param.S) > 0 else path('/x')\", ";
    for (lets, enable_if) in [
        (q.to_string(), "q.name == 'z' or true"),
        (format!("{q}\"m = q.name\", "), "m == 'z' or true"),
    ] {
        let jt = early_wrap_template(&lets, enable_if);
        let job = created(&jt, &[("S", "a")]).job;
        assert!(
            matches!(step(&job, "B").binding, StepBinding::PendingInputs(_)),
            "{lets}"
        );
        let err = create_with(&jt, &[], &[("S", "1")]).unwrap_err();
        assert_errors(&err.to_string(), "JobTemplate", &[B_TWO_LAYERS]);
    }
}

#[test]
fn the_early_wrap_check_counts_only_wrapping_step_environments() {
    let jt = jt_ok(
        &format!(
            r#"
jobEnvironments:
- name: JW{}
steps:
- name: C
  dependencies: [{{environment: JW, enableIf: "false"}}]
  outputVariables: [{{name: X, type: STRING}}]
  script: {{actions: {{onRun: {{command: echo, args: [c]}}}}}}
- name: B
  enableIf: "Param.S != ''"
  dependencies: [{{dependsOn: C}}]
  let: ["c = Steps.C.Output.X"]
  stepEnvironments:
  - {{name: NW, script: {{actions: {{onEnter: {{command: echo, args: [nw]}}}}}}}}
  script: {{actions: {{onRun: {{command: echo, args: ["{{{{c}}}}"]}}}}}}
parameterDefinitions:
- {{name: S, type: STRING, default: "1"}}
"#,
            wrap_hooks("  ")
        ),
        WRAP_HEADER,
    );
    let job = created(&jt, &[]).job;
    assert!(matches!(
        step(&job, "B").binding,
        StepBinding::PendingInputs(_)
    ));
}

#[test]
fn the_early_wrap_check_fails_the_remaining_group_at_binding() {
    // B's enableIf is decided at creation; SW's waits on C, and its
    // remaining group waits on D too. Once C's value makes SW certain, B
    // fails though D is still missing.
    let jt = jt_ok(
        &format!(
            r#"
parameterDefinitions:
- {{name: S, type: STRING, default: "1"}}
jobEnvironments:
- name: JW{}
steps:
- name: C
  dependencies: [{{environment: JW, enableIf: "false"}}]
  outputVariables: [{{name: X, type: STRING}}]
  script: {{actions: {{onRun: {{command: echo, args: [c]}}}}}}
- name: D
  dependencies: [{{environment: JW, enableIf: "false"}}]
  outputVariables: [{{name: Y, type: STRING}}]
  script: {{actions: {{onRun: {{command: echo, args: [d]}}}}}}
- name: B
  enableIf: "Param.S != ''"
  dependencies: [{{dependsOn: C}}, {{dependsOn: D}}]
  let: ["d = Steps.D.Output.Y"]
  stepEnvironments:
  - name: SW
    enableIf: "Steps.C.Output.X == 'a'"{}
  script: {{actions: {{onRun: {{command: echo, args: ["{{{{d}}}}"]}}}}}}
"#,
            wrap_hooks("  "),
            wrap_hooks("    ")
        ),
        WRAP_HEADER,
    );
    let job = created(&jt, &[]).job;
    let err = advance(
        &jt,
        &[],
        &[],
        &job,
        "B",
        "C",
        &[("X", ExprValue::String("a".into()))],
    )
    .unwrap_err();
    assert_eq!(
        err.group,
        openjd_model::job::MaterializationGroup::Remaining
    );
    assert_eq!(
        err.to_string(),
        "Step 'B' failed in its remaining group:\nModel validation error: 1 validation error \
         for JobTemplate\nsteps[2]:\n\tits Sessions would enter 2 Environments that define wrap \
         hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'JW', 'SW' (stepEnvironments). \
         A Session may have at most one Environment defining wrap hooks (RFC 0008)."
    );
}

#[test]
fn an_abort_is_filed_with_its_own_steps_errors() {
    // A (steps[0]) aborts in host selection; C (steps[1]), bound before it,
    // fails to resolve H's requirement. A's error comes first.
    let jt = jt_ok(
        r#"
parameterDefinitions:
- {name: M, type: INT, default: 2}
jobEnvironments:
- name: E
  activation: ON_REQUEST
  hostRequirements:
    amounts: [{name: amount.worker.vcpu, min: 4}]
  script: {actions: {onEnter: {command: echo, args: [e]}}}
- name: H
  activation: ON_REQUEST
  envParameterDefinitions: [{name: Index, type: INT, default: 5}]
  hostRequirements:
    amounts: [{name: amount.worker.memory, min: "{{ [1][Env.Param.Index] }}"}]
  script: {actions: {onEnter: {command: echo, args: [h]}}}
steps:
- name: A
  enableIf: "Param.M > 0"
  dependencies: [{dependsOn: C}, {environment: E}]
  hostRequirements:
    amounts: [{name: amount.worker.vcpu, max: "{{Param.M}}"}]
  script: {actions: {onRun: {command: echo, args: [a]}}}
- name: C
  enableIf: "Param.M > 0"
  dependencies: [{environment: H}]
  script: {actions: {onRun: {command: echo, args: [c]}}}
"#,
        FB1_HEADER,
    );
    let err = create_with(&jt, &[], &[]).unwrap_err().to_string();
    let a = err.find("Step 'A': no host can satisfy").expect(&err);
    let c = err.find("Resolved for Step 'C'.").expect(&err);
    assert!(a < c, "{err}");
}

#[test]
fn a_loops_abort_is_reported_with_the_errors_found_before_it() {
    let jt = jt_ok(
        r#"
parameterDefinitions:
- {name: S, type: STRING, default: a}
- {name: M, type: INT, default: 0}
steps:
- name: A
  let: ["n = int(Param.S)"]
  script: {actions: {onRun: {command: echo, args: ["{{n}}"]}}}
- name: L
  dependencies: [{dependsOn: A}]
  loop:
    maxIterations: "{{Param.M}}"
    until: "true"
    steps:
    - name: B
      script: {actions: {onRun: {command: echo, args: [b]}}}
"#,
        HEADER,
    );
    let err = create_with(&jt, &[], &[]).unwrap_err().to_string();
    assert!(
        err.starts_with("Model validation error: 2 validation errors for JobTemplate"),
        "{err}"
    );
    assert!(
        err.contains("steps[0] -> let[0]:\n\tInvalid expression in let binding 'n'"),
        "{err}"
    );
    assert!(err.contains("steps[1] -> loop -> maxIterations:"), "{err}");
}

#[test]
fn step_environment_statuses_are_reported_for_a_pending_step_whose_remaining_group_is_known() {
    let jt = jt_ok(
        r#"
parameterDefinitions:
- {name: F, type: BOOL, default: false}
steps:
- name: P
  outputVariables: [{name: R, type: RANGE_EXPR}]
  script: {actions: {onRun: {command: echo, args: [p]}}}
- name: S
  dependencies: [{dependsOn: P}]
  parameterSpace:
    taskParameterDefinitions: [{name: I, type: INT, range: "{{Steps.P.Output.R}}"}]
  stepEnvironments:
  - {name: SE, enableIf: "Param.F", script: {actions: {onEnter: {command: echo, args: [x]}}}}
  script: {actions: {onRun: {command: echo, args: [s]}}}
"#,
        HEADER,
    );
    let created = created(&jt, &[]);
    assert!(matches!(
        step(&created.job, "S").binding,
        StepBinding::PendingInputs(_)
    ));
    let statuses: Vec<(String, EnvironmentStatus)> = created.environment_statuses[1]
        .environments
        .iter()
        .map(|s| (s.name.clone(), s.status))
        .collect();
    assert_eq!(statuses, [("SE".to_string(), EnvironmentStatus::Disabled)]);
    assert_eq!(created.disabled_environments[1].step_environments, ["SE"]);
}

#[test]
fn an_environment_enable_if_error_is_reported_once_per_step() {
    let jt = jt_ok(
        r#"
parameterDefinitions:
- {name: I, type: INT, default: 5}
jobEnvironments:
- name: E
  enableIf: "[1][Param.I] > 0"
  script: {actions: {onEnter: {command: echo, args: [e]}}}
steps:
- name: A
  script: {actions: {onRun: {command: echo, args: [a]}}}
- name: B
  script: {actions: {onRun: {command: echo, args: [b]}}}
"#,
        HEADER,
    );
    let err = create_with(&jt, &[], &[]).unwrap_err().to_string();
    assert!(
        err.starts_with("Model validation error: 2 validation errors for JobTemplate"),
        "{err}"
    );
    assert_eq!(err.matches("Resolved for Step 'A'.").count(), 1, "{err}");
    assert_eq!(err.matches("Resolved for Step 'B'.").count(), 1, "{err}");
}

#[test]
fn a_step_environment_enable_if_that_fails_is_not_counted_as_a_wrap_layer() {
    let jt = jt_ok(
        &format!(
            r#"
jobEnvironments:
- name: JW{}
steps:
- name: P
  dependencies: [{{environment: JW, enableIf: "false"}}]
  outputVariables: [{{name: Go, type: BOOL}}]
  script: {{actions: {{onRun: {{command: echo, args: [p]}}}}}}
- name: B
  dependencies: [{{dependsOn: P}}]
  stepEnvironments:
  - name: SW
    enableIf: "None if Steps.P.Output.Go else False"{}
  script: {{actions: {{onRun: {{command: echo, args: [b]}}}}}}
"#,
            wrap_hooks("  "),
            wrap_hooks("    ")
        ),
        WRAP_HEADER,
    );
    let job = created(&jt, &[]).job;
    let err = advance(
        &jt,
        &[],
        &[],
        &job,
        "B",
        "P",
        &[("Go", ExprValue::Bool(true))],
    )
    .unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("1 validation error for JobTemplate"),
        "{message}"
    );
    assert!(
        message.contains("steps[1] -> stepEnvironments[0] -> enableIf:"),
        "{message}"
    );
}

// ── Round 5 ───────────────────────────────────────────────────────

fn attached_x(activation: &str, params: &str) -> EnvironmentTemplate {
    env_ok(&format!(
        r#"
specificationVersion: environment-2023-09
extensions: [EXPR, ENV_PARAMS]
environment:
  name: X
  activation: {activation}{params}
  script: {{actions: {{onEnter: {{command: echo, args: [x]}}}}}}
"#
    ))
}

#[test]
fn an_ambiguous_entry_is_not_checked_for_a_skipped_step() {
    // Whether its own `enableIf` fails or is absent, an entry of a skipped
    // Step is never checked, an ambiguous name included (Template Schemas
    // §3.2.1 item 3, RFC 0011 `<Environment>` item 3).
    for entry_enable_if in [", enableIf: \"int(Param.S) > 0\"", ""] {
        let jt = jt_ok(
            &format!(
                r#"
parameterDefinitions:
- {{name: F, type: BOOL, default: false}}
- {{name: S, type: STRING, default: abc}}
steps:
- name: A
  enableIf: "Param.F"
  dependencies: [{{environment: X{entry_enable_if}}}]
  script: {{actions: {{onRun: {{command: echo, args: [a]}}}}}}
"#
            ),
            HEADER,
        );
        let envs = [attached_x("ON_REQUEST", ""), attached_x("ON_REQUEST", "")];
        let job = create_with(&jt, &envs, &[])
            .unwrap_or_else(|e| panic!("{entry_enable_if}: {e}"))
            .job;
        assert!(
            matches!(step(&job, "A").binding, StepBinding::Skipped),
            "{entry_enable_if}"
        );
    }
}

#[test]
fn an_entry_warning_is_reported_only_for_a_step_whose_selection_is_applied() {
    let src = |enable_if: &str| {
        format!(
            r#"
parameterDefinitions:
- {{name: F, type: BOOL, default: false}}
steps:
- name: P
  outputVariables: [{{name: Ok, type: BOOL}}]
  script: {{actions: {{onRun: {{command: echo, args: [p]}}}}}}
- name: A
  enableIf: {enable_if}
  dependencies:
  - {{dependsOn: P}}
  - {{environment: X, required: false, parameters: {{K: 1}}}}
  - {{environment: Missing, required: false}}
  script: {{actions: {{onRun: {{command: echo, args: [a]}}}}}}
"#
        )
    };
    let envs = [attached_x("ON_REQUEST", "")];
    let attached = [AttachedEnvironmentTemplate::new(&envs[0])];
    // Skipped at creation, pending, or not known to be enabled: none.
    for enable_if in ["\"Param.F\"", "\"Steps.P.Output.Ok\""] {
        let jt = jt_ok(&src(enable_if), HEADER);
        assert!(
            create_with(&jt, &envs, &[]).unwrap().warnings.is_empty(),
            "{enable_if}"
        );
        let warnings = check_environment_templates(&jt, &attached, &template_ctx(&jt)).unwrap();
        assert!(warnings.is_empty(), "{enable_if}");
    }
    // Enabled at creation: both.
    let jt = jt_ok(&src("\"Param.F\""), HEADER);
    assert_eq!(
        create_with(&jt, &envs, &[("F", "true")])
            .unwrap()
            .warnings
            .len(),
        2
    );
    let jt = jt_ok(&src("\"true\""), HEADER);
    let warnings = check_environment_templates(&jt, &attached, &template_ctx(&jt)).unwrap();
    assert_eq!(warnings.len(), 2);
}

#[test]
fn a_failed_resolved_default_of_an_attachment_is_reported_for_each_step() {
    let jt = jt_ok(
        r#"
parameterDefinitions:
- {name: F, type: BOOL, default: false}
steps:
- name: A
  enableIf: "Param.F"
  script: {actions: {onRun: {command: echo, args: [a]}}}
- name: B
  script: {actions: {onRun: {command: echo, args: [b]}}}
"#,
        HEADER,
    );
    let lic = env_ok(
        r#"
specificationVersion: environment-2023-09
extensions: [EXPR, ENV_PARAMS]
parameterDefinitions:
- {name: S, type: STRING, default: many}
environment:
  name: Lic
  envParameterDefinitions: [{name: Seats, type: INT, default: "{{Param.S}}"}]
  script: {actions: {onEnter: {command: echo, args: ["{{Env.Param.Seats}}"]}}}
"#,
    );
    let err = create_with(&jt, &[lic], &[]).unwrap_err().to_string();
    assert!(
        err.contains(
            "EnvironmentTemplate[0] -> environment -> envParameterDefinitions[0] -> default:\n\t\
             for Step 'B', which does not supply this parameter:"
        ),
        "{err}"
    );
    assert!(!err.contains("'A'"), "{err}");
}

#[test]
fn a_remaining_group_error_unrelated_to_step_environments_keeps_the_wrap_check() {
    // B's own timeout fails and its Sessions enter two wrapping
    // Environments: both are reported.
    let jt = jt_ok(
        &format!(
            r#"
parameterDefinitions:
- {{name: T, type: INT, default: 0}}
- {{name: R, type: BOOL, default: true}}
jobEnvironments:
- name: W1{}
- name: W2
  activation: ON_REQUEST{}
steps:
- name: B
  enableIf: "Param.R"
  dependencies: [{{environment: W2, enableIf: "Param.R"}}]
  script: {{actions: {{onRun: {{command: echo, args: [b], timeout: "{{{{Param.T}}}}"}}}}}}
"#,
            wrap_hooks("  "),
            wrap_hooks("  ")
        ),
        FB1_WRAP_HEADER,
    );
    let err = create_with(&jt, &[], &[]).unwrap_err().to_string();
    assert!(
        err.starts_with("Model validation error: 2 validation errors for JobTemplate"),
        "{err}"
    );
    assert!(err.contains("timeout must be > 0."), "{err}");
    assert!(err.contains("'W1', 'W2'"), "{err}");
}

// ── Round 6 ───────────────────────────────────────────────────────

#[test]
fn the_early_wrap_check_reads_pending_outputs_with_their_declared_types() {
    // `Steps.A.Output.Ok or true` is known `true` for a BOOL `Ok`.
    let jt = jt_ok(
        &format!(
            r#"
parameterDefinitions:
- {{name: F, type: BOOL, default: true}}
jobEnvironments:
- name: W1
  enableIf: "Param.F"{}
steps:
- name: A
  dependencies: [{{environment: W1, enableIf: "false"}}]
  outputVariables: [{{name: Ok, type: BOOL}}]
  script: {{actions: {{onRun: {{command: echo, args: [a]}}}}}}
- name: S
  dependencies: [{{dependsOn: A}}]
  stepEnvironments:
  - name: W2
    enableIf: "Steps.A.Output.Ok or true"{}
  script: {{actions: {{onRun: {{command: echo, args: [s]}}}}}}
"#,
            wrap_hooks("  "),
            wrap_hooks("    ")
        ),
        WRAP_HEADER,
    );
    let err = create_with(&jt, &[], &[]).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &[
            "steps[1]:\n\tits Sessions would enter 2 Environments that define wrap hooks \
           (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'W1', 'W2' (stepEnvironments). A \
           Session may have at most one Environment defining wrap hooks (RFC 0008).",
        ],
    );
}

#[test]
fn a_pending_steps_selection_carries_its_entry_warnings() {
    let jt = jt_ok(
        r#"
steps:
- name: A
  outputVariables: [{name: Ok, type: BOOL}]
  script: {actions: {onRun: {command: echo, args: [a]}}}
- name: B
  enableIf: "Steps.A.Output.Ok"
  dependencies: [{dependsOn: A}, {environment: Missing, required: false}]
  script: {actions: {onRun: {command: echo, args: [b]}}}
"#,
        HEADER,
    );
    let created = create_with(&jt, &[attached_x("ON_REQUEST", "")], &[]).unwrap();
    assert!(created.warnings.is_empty());
    let StepBinding::PendingInputs(p) = &step(&created.job, "B").binding else {
        panic!("B should be pending");
    };
    let warnings: Vec<String> = p
        .environment_selection
        .as_ref()
        .unwrap()
        .warnings
        .iter()
        .map(|w| w.message.clone())
        .collect();
    assert_eq!(
        warnings,
        [
            "names Environment 'Missing', which is not a jobEnvironments entry of the Job \
          Template, and none of the attached Environment Templates (EnvironmentTemplate[0]) \
          defines it. The entry has 'required: false', so it has no effect."
        ]
    );
}

#[test]
fn a_host_requirement_conflict_names_the_steps_own_source_from_its_document() {
    let jt = jt_ok(
        r#"
parameterDefinitions:
- {name: M, type: INT, default: 2}
steps:
- name: A
  outputVariables: [{name: Ok, type: BOOL}]
  script: {actions: {onRun: {command: echo, args: [a]}}}
- name: C
  enableIf: "Steps.A.Output.Ok"
  dependencies: [{dependsOn: A}, {environment: R, parameters: {P: 4}}]
  hostRequirements:
    amounts: [{name: amount.worker.vcpu, max: "{{Param.M}}"}]
  script: {actions: {onRun: {command: echo, args: [c]}}}
"#,
        FB1_HEADER,
    );
    let r = env_ok(
        r#"
specificationVersion: environment-2023-09
extensions: [EXPR, ENV_PARAMS, FEATURE_BUNDLE_1]
environment:
  name: R
  activation: ON_REQUEST
  envParameterDefinitions: [{name: P, type: INT}]
  hostRequirements:
    amounts: [{name: amount.worker.vcpu, min: "{{Env.Param.P}}"}]
  script: {actions: {onEnter: {command: echo, args: [r]}}}
"#,
    );
    let envs = [r];
    let job = create_with(&jt, &envs, &[]).unwrap().job;
    let err = advance(
        &jt,
        &envs,
        &[],
        &job,
        "C",
        "A",
        &[("Ok", ExprValue::Bool(true))],
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains(
            "EnvironmentTemplate[0] -> environment -> hostRequirements (Environment 'R') requires \
             min 4, and JobTemplate -> steps[1] -> hostRequirements requires max 2."
        ),
        "{err}"
    );
}

#[test]
fn a_failing_literal_default_is_reported_once() {
    // A literal default edited below its minValue after decoding, on an
    // ALWAYS attachment that every Step takes.
    let jt = jt_ok(
        r#"
steps:
- name: S1
  script: {actions: {onRun: {command: echo, args: [s1]}}}
- name: S2
  script: {actions: {onRun: {command: echo, args: [s2]}}}
"#,
        HEADER,
    );
    let src = |default: u32| {
        format!(
            r#"
specificationVersion: environment-2023-09
extensions: [EXPR, ENV_PARAMS]
environment:
  name: X
  activation: ALWAYS
  envParameterDefinitions: [{{name: N, type: INT, minValue: {min}, default: {default}}}]
  script: {{actions: {{onEnter: {{command: echo, args: ["{{{{Env.Param.N}}}}"]}}}}}}
"#,
            min = if default == 1 { 1 } else { 5 }
        )
    };
    let mut x = env_ok(&src(6));
    // The default of a decodable template with minValue 1, moved onto X,
    // whose minValue is 5.
    let one = env_ok(&src(1))
        .environment
        .env_parameter_definitions
        .unwrap()[0]
        .default
        .clone();
    x.environment.env_parameter_definitions.as_mut().unwrap()[0].default = one;
    let err = create_with(&jt, &[x], &[]).unwrap_err();
    assert_errors(
        &err.to_string(),
        "Submission",
        &[
            "EnvironmentTemplate[0] -> environment -> envParameterDefinitions[0] -> default:\n\t\
           Parameter 'N': value 1 is less than minimum 5",
        ],
    );
}

#[test]
fn an_undecided_entry_naming_a_job_environment_is_not_checked_against_attachments() {
    let jt = jt_ok(
        r#"
parameterDefinitions:
- {name: F, type: BOOL, default: false}
- {name: S, type: STRING, default: abc}
jobEnvironments:
- name: X
  activation: ON_REQUEST
  script: {actions: {onEnter: {command: echo, args: [x]}}}
steps:
- name: A
  enableIf: "Param.F"
  dependencies: [{environment: X, enableIf: "int(Param.S) > 0"}]
  script: {actions: {onRun: {command: echo, args: [a]}}}
"#,
        HEADER,
    );
    let envs = [attached_x("ON_REQUEST", ""), attached_x("ON_REQUEST", "")];
    create_with(&jt, &envs, &[]).unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn an_ambiguous_enabled_entry_fails_the_steps_selection() {
    // At Job creation for a Step enabled then, and at step input binding
    // for one whose `enableIf` is decided then (Template Schemas §3.2.1
    // item 3, RFC 0011 `<Environment>` item 3).
    let jt = jt_ok(
        r#"
parameterDefinitions:
- {name: F, type: BOOL, default: true}
steps:
- name: P
  outputVariables: [{name: Ok, type: BOOL}]
  script: {actions: {onRun: {command: echo, args: [p]}}}
- name: A
  enableIf: "Param.F"
  dependencies: [{environment: X, required: false}]
  script: {actions: {onRun: {command: echo, args: [a]}}}
- name: B
  enableIf: "Steps.P.Output.Ok"
  dependencies: [{dependsOn: P}, {environment: X}]
  script: {actions: {onRun: {command: echo, args: [b]}}}
"#,
        HEADER,
    );
    let envs = [
        attached_x("ON_REQUEST", ""),
        attached_x("ON_REQUEST", ""),
        attached_x("ON_REQUEST", ""),
    ];
    let err = create_with(&jt, &envs, &[]).unwrap_err().to_string();
    assert_eq!(
        err,
        "Model validation error: 1 validation error for Submission\nJobTemplate -> steps[1] -> \
         dependencies[0] -> environment:\n\tnames Environment 'X', which 3 attached Environment \
         Templates define (EnvironmentTemplate[0], EnvironmentTemplate[1], \
         EnvironmentTemplate[2]); an entry must match exactly one (Template Schemas §3.2.1 item \
         3)."
    );
    let job = create_with(&jt, &envs, &[("F", "false")])
        .unwrap_or_else(|e| panic!("{e}"))
        .job;
    let err = advance(
        &jt,
        &envs,
        &[("F", "false")],
        &job,
        "B",
        "P",
        &[("Ok", ExprValue::Bool(true))],
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Step 'B' failed in its enablement group:\nModel validation error: 1 validation error \
         for Submission\nJobTemplate -> steps[2] -> dependencies[1] -> environment:\n\tnames \
         Environment 'X', which 3 attached Environment Templates define \
         (EnvironmentTemplate[0], EnvironmentTemplate[1], EnvironmentTemplate[2]); an entry \
         must match exactly one (Template Schemas §3.2.1 item 3)."
    );
}

#[test]
fn without_control_flow_a_failed_entry_enable_if_is_reported_alone() {
    let src = r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, ENV_PARAMS]
name: J
parameterDefinitions:
- {name: S, type: STRING, default: abc}
steps:
- name: A
  dependencies: [{environment: X, enableIf: "int(Param.S) > 0"}]
  script: {actions: {onRun: {command: echo, args: [a]}}}
"#;
    let jt = decode_job_template(yaml(src), Some(ALLOW), &CallerLimits::default()).unwrap();
    let envs = [attached_x("ON_REQUEST", ""), attached_x("ON_REQUEST", "")];
    let err = create_with(&jt, &envs, &[]).unwrap_err().to_string();
    assert!(
        err.starts_with("Model validation error: 1 validation error for Submission"),
        "{err}"
    );
    assert!(err.contains("dependencies[0] -> enableIf:"), "{err}");
}

#[test]
fn a_loop_may_not_have_an_environment_entry() {
    check_err(
        r#"
jobEnvironments:
- name: E
  activation: ON_REQUEST
  script: {actions: {onEnter: {command: echo, args: [e]}}}
steps:
- name: L
  dependencies: [{environment: E}]
  loop:
    until: "true"
    maxIterations: 1
    steps:
    - name: B
      script: {actions: {onRun: {command: echo, args: [b]}}}
"#,
        &[
            "steps[0] -> dependencies[0]:\n\ta loop's dependencies may not have an 'environment' \
           entry: a loop has no Sessions. Put the entry on the body Steps that need the \
           Environment (RFC 0011).",
        ],
    );
}

// ── Round 7 ───────────────────────────────────────────────────────

/// Step S's entry reads `x`, which fails for D=0; S's `enableIf` is
/// `s_enable_if`, and a loop L has a body Step B with the same entry.
fn failing_let_template(entry: &str, s_enable_if: &str, loop_enable_if: &str) -> JobTemplate {
    jt_ok(
        &format!(
            r#"
parameterDefinitions:
- {{name: D, type: INT, default: 0}}
jobEnvironments:
- name: JE
  activation: ON_REQUEST
  envParameterDefinitions: [{{name: Q, type: INT}}]
  script: {{actions: {{onEnter: {{command: echo, args: ["{{{{Env.Param.Q}}}}"]}}}}}}
steps:
- name: S
  enableIf: "{s_enable_if}"
  let: ["x = 10 // Param.D"]
  dependencies: [{{environment: JE, {entry}}}]
  script: {{actions: {{onRun: {{command: echo, args: [s]}}}}}}
- name: L
  enableIf: "{loop_enable_if}"
  loop:
    until: "true"
    maxIterations: 1
    steps:
    - name: B
      let: ["x = 10 // Param.D"]
      dependencies: [{{environment: JE, {entry}}}]
      script: {{actions: {{onRun: {{command: echo, args: [b]}}}}}}
"#
        ),
        HEADER,
    )
}

const S_LET_ERROR: &str = "steps[0] -> let[0]:\n\tInvalid expression in let binding 'x': Division \
                           by zero";

#[test]
fn a_let_an_entry_reads_that_fails_fails_the_steps_selection() {
    for entry in [
        r#"enableIf: "x > 0", parameters: {Q: 1}"#,
        r#"parameters: {Q: "{{x}}"}"#,
    ] {
        // Enabled at creation: the let error, as without CONTROL_FLOW.
        let jt = failing_let_template(entry, "true", "false");
        let err = create_with(&jt, &[], &[]).unwrap_err();
        assert_errors(&err.to_string(), "JobTemplate", &[S_LET_ERROR]);
        // Skipped, and a skipped loop's body Step: nothing.
        let jt = failing_let_template(entry, "Param.D > 0", "false");
        let job = created(&jt, &[]).job;
        assert_eq!(step(&job, "S").binding, StepBinding::Skipped, "{entry}");
    }
}

#[test]
fn a_let_an_entry_reads_that_fails_fails_a_body_step_instance() {
    let jt = failing_let_template(
        r#"enableIf: "x > 0", parameters: {Q: 1}"#,
        "Param.D > 0",
        "true",
    );
    let job = created(&jt, &[]).job;
    let values = job_params_with(&jt, &[], &[]);
    let progress = openjd_model::job::LoopProgress::new(&job.loops[0]).unwrap();
    let err = openjd_model::start_iteration(
        &jt,
        &values,
        &template_ctx(&jt),
        &job,
        0,
        &progress,
        &StepInputs::default(),
        0,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.starts_with(
            "Step 'L[0].B' failed in its enablement group:\nModel validation error: 1 validation \
             error for JobTemplate\nsteps[1] -> loop -> steps[0] -> let[0]:\n\tInvalid \
             expression in let binding 'x': Division by zero"
        ),
        "{err}"
    );
}

#[test]
fn a_pending_steps_failed_entry_enable_if_fails_its_binding() {
    let jt = jt_ok(
        r#"
parameterDefinitions:
- {name: S, type: STRING, default: abc}
jobEnvironments:
- name: E
  activation: ON_REQUEST
  script: {actions: {onEnter: {command: echo, args: [e]}}}
steps:
- name: A
  outputVariables: [{name: Ok, type: BOOL}]
  script: {actions: {onRun: {command: echo, args: [a]}}}
- name: B
  enableIf: "Steps.A.Output.Ok"
  dependencies: [{dependsOn: A}, {environment: E, enableIf: "int(Param.S) > 0"}]
  script: {actions: {onRun: {command: echo, args: [b]}}}
"#,
        HEADER,
    );
    let job = created(&jt, &[]).job;
    let err = advance(
        &jt,
        &[],
        &[],
        &job,
        "B",
        "A",
        &[("Ok", ExprValue::Bool(true))],
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.starts_with(
            "Step 'B' failed in its enablement group:\nModel validation error: 1 validation \
             error for JobTemplate\nsteps[1] -> dependencies[1] -> enableIf:\n\tcould not be \
             evaluated: "
        ),
        "{err}"
    );
}

// ── Round 8 ───────────────────────────────────────────────────────

#[test]
fn a_failing_let_an_entry_reads_indirectly_or_with_others_fails_the_selection() {
    for (lets, entry, error) in [
        (
            r#"["x = 10 // Param.D", "y = x + 1"]"#,
            r#"enableIf: "y > 0", parameters: {Q: 1}"#,
            "steps[0] -> let[0]:\n\tInvalid expression in let binding 'x': Division by zero",
        ),
        (
            r#"["x = 10 // Param.D", "y = x + 1"]"#,
            r#"parameters: {Q: "{{y}}"}"#,
            "steps[0] -> let[0]:\n\tInvalid expression in let binding 'x': Division by zero",
        ),
        (
            r#"["g = 1", "x = 10 // Param.D"]"#,
            r#"enableIf: "g > 0 and x > 0", parameters: {Q: 1}"#,
            "steps[0] -> let[1]:\n\tInvalid expression in let binding 'x': Division by zero",
        ),
    ] {
        let jt = jt_ok(
            &format!(
                r#"
parameterDefinitions:
- {{name: D, type: INT, default: 0}}
jobEnvironments:
- name: JE
  activation: ON_REQUEST
  envParameterDefinitions: [{{name: Q, type: INT}}]
  script: {{actions: {{onEnter: {{command: echo, args: ["{{{{Env.Param.Q}}}}"]}}}}}}
steps:
- name: S
  enableIf: "true"
  let: {lets}
  dependencies: [{{environment: JE, {entry}}}]
  script: {{actions: {{onRun: {{command: echo, args: [s]}}}}}}
"#
            ),
            HEADER,
        );
        let err = create_with(&jt, &[], &[]).unwrap_err();
        assert_errors(&err.to_string(), "JobTemplate", &[error]);
    }
}

// ── Ambiguous names in a Step's Environment selection ──

/// Two attached `X` with `activation`, each with a parameter `V` that has
/// no `default`, so no Step can supply it (Template Schemas §3.2.1 item 3).
fn ambiguous_required_x(activation: &str) -> [EnvironmentTemplate; 2] {
    let params = "\n  envParameterDefinitions: [{name: V, type: STRING}]";
    [
        attached_x(activation, params),
        attached_x(activation, params),
    ]
}

/// The two attachment errors for [`ambiguous_required_x`], with `entered`
/// the sentence on how the attachment applies to the Steps.
fn ambiguous_required_errors(entered: &str) -> String {
    let one = |doc: usize| {
        format!(
            "EnvironmentTemplate[{doc}] -> environment -> envParameterDefinitions:\n\tEnvironment \
             'X' is defined by 2 attached Environment Templates (EnvironmentTemplate[0], \
             EnvironmentTemplate[1]), so no enabled 'environment:' dependency can reach it \
             (Template Schemas §3.2.1 item 3), and it has parameters without a default that no \
             Step can supply: V. {entered}, and the submission is rejected (§3.2.1)."
        )
    };
    format!(
        "Model validation error: 2 validation errors for Submission\n{}\n{}",
        one(0),
        one(1)
    )
}

const BY_DEFAULT_ENTERED: &str = "Its activation is BY_DEFAULT, so it applies to every Step that \
                                  does not decline it with a disabled entry, and at least one \
                                  Step does not decline it";

fn ambiguous_required_template(enable_if: &str) -> JobTemplate {
    jt_ok(
        &format!(
            r#"
parameterDefinitions:
- {{name: F, type: BOOL, default: false}}
steps:
- name: P
  outputVariables: [{{name: Ok, type: BOOL}}]
  dependencies: [{{environment: X, enableIf: "false"}}]
  script: {{actions: {{onRun: {{command: echo, args: [p]}}}}}}
- name: S
  enableIf: {enable_if}
  dependencies: [{{dependsOn: P}}]
  script: {{actions: {{onRun: {{command: echo, args: [s]}}}}}}
"#
        ),
        HEADER,
    )
}

#[test]
fn an_ambiguous_attachment_no_step_can_supply_fails_only_a_step_it_applies_to_that_runs() {
    // `BY_DEFAULT` X ×2: P declines both with a disabled entry (§3.2.1 item
    // 3), so only S's selection meets X's constraint 7 (RFC 0011
    // `<Environment>` item 3).
    let envs = ambiguous_required_x("BY_DEFAULT");
    let expected = ambiguous_required_errors(BY_DEFAULT_ENTERED);
    // Skipped at Job creation: the Job is created.
    let skipped = ambiguous_required_template("\"Param.F\"");
    let job = create_with(&skipped, &envs, &[])
        .unwrap_or_else(|e| panic!("{e}"))
        .job;
    assert_eq!(step(&job, "S").binding, StepBinding::Skipped);
    // Enabled at Job creation: Job creation fails.
    let err = create_with(&skipped, &envs, &[("F", "true")]).unwrap_err();
    assert_eq!(err.to_string(), expected);
    // Decided at step input binding: S fails then.
    let pending = ambiguous_required_template("\"Steps.P.Output.Ok\"");
    let job = create_with(&pending, &envs, &[])
        .unwrap_or_else(|e| panic!("{e}"))
        .job;
    let err = advance(
        &pending,
        &envs,
        &[],
        &job,
        "S",
        "P",
        &[("Ok", ExprValue::Bool(true))],
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("Step 'S' failed in its enablement group:\n{expected}")
    );
    // Before Job creation: only for a Step known to be enabled.
    let attached: Vec<_> = envs.iter().map(AttachedEnvironmentTemplate::new).collect();
    check_environment_templates(&pending, &attached, &template_ctx(&pending))
        .unwrap_or_else(|e| panic!("expected no errors before Job creation, got:\n{e}"));
    let known = ambiguous_required_template("\"true\"");
    let err = check_environment_templates(&known, &attached, &template_ctx(&known)).unwrap_err();
    assert_eq!(err.to_string(), expected);
}

#[test]
fn an_ambiguous_always_attachment_no_step_can_supply_is_not_checked_for_a_skipped_step() {
    // `ALWAYS` X ×2 and a single Step: a skipped Step has no Environment
    // selection, so the Job is created; an enabled one fails it.
    let envs = ambiguous_required_x("ALWAYS");
    let jt = jt_ok(
        r#"
parameterDefinitions:
- {name: F, type: BOOL, default: false}
steps:
- name: S
  enableIf: "Param.F"
  script: {actions: {onRun: {command: echo, args: [s]}}}
"#,
        HEADER,
    );
    create_with(&jt, &envs, &[]).unwrap_or_else(|e| panic!("{e}"));
    let err = create_with(&jt, &envs, &[("F", "true")]).unwrap_err();
    assert_eq!(
        err.to_string(),
        ambiguous_required_errors("Its activation is ALWAYS, so every Session enters it")
    );
}

#[test]
fn a_disabled_entry_with_an_ambiguous_name_declines_each_match_in_a_steps_selection() {
    // §3.2.1 item 3: not an error, before Job creation, at Job creation, or
    // at step input binding, and each `BY_DEFAULT` match is declined.
    let jt = jt_ok(
        r#"
parameterDefinitions:
- {name: F, type: BOOL, default: false}
steps:
- name: P
  outputVariables: [{name: Ok, type: BOOL}]
  script: {actions: {onRun: {command: echo, args: [p]}}}
- name: A
  dependencies: [{environment: X, enableIf: "Param.F"}]
  script: {actions: {onRun: {command: echo, args: [a]}}}
- name: B
  enableIf: "Steps.P.Output.Ok"
  dependencies: [{dependsOn: P}, {environment: X, enableIf: "false"}]
  script: {actions: {onRun: {command: echo, args: [b]}}}
"#,
        HEADER,
    );
    let envs = [attached_x("BY_DEFAULT", ""), attached_x("BY_DEFAULT", "")];
    let attached: Vec<_> = envs.iter().map(AttachedEnvironmentTemplate::new).collect();
    check_environment_templates(&jt, &attached, &template_ctx(&jt))
        .unwrap_or_else(|e| panic!("expected no errors before Job creation, got:\n{e}"));
    let job = create_with(&jt, &envs, &[])
        .unwrap_or_else(|e| panic!("{e}"))
        .job;
    let b = advance(
        &jt,
        &envs,
        &[],
        &job,
        "B",
        "P",
        &[("Ok", ExprValue::Bool(true))],
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(b.binding, StepBinding::Bound);
    for doc in 0..2 {
        let document = openjd_model::job::Document::environment_template(doc, None);
        // P has no entry, so it enters both; A and B decline both.
        assert!(
            step(&job, "P").enters_environment(&document, "X"),
            "P {doc}"
        );
        assert!(
            !step(&job, "A").enters_environment(&document, "X"),
            "A {doc}"
        );
        assert!(!b.enters_environment(&document, "X"), "B {doc}");
    }
}
