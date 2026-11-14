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
    assert_eq!(
        created.discarded_host_requirements,
        [vec![openjd_model::DiscardedHostRequirement {
            environment: None,
            kind: openjd_model::HostRequirementKind::Amount,
            name: "amount.worker.vcpu".to_string(),
        }]]
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
