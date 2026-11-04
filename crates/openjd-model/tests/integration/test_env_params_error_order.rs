// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `ENV_PARAMS`: the order of job creation's errors (Template Schemas
//! §7.4.1).
//!
//! Job creation takes §7.4.1's steps 1–8 over the Job Template and the
//! attached Environment Templates together, and reports every error in one
//! `ModelValidation`: the template-validation-style checks first, then by
//! §7.4.1 step, then by Step, then by the Environment's place in the Job's
//! Environment order (an entry's error by its place in the Step's
//! `dependencies`). A Step with an error from steps 1–4 gets no checks from
//! steps 5–8; the other Steps do. The error is for `JobTemplate`, at paths
//! relative to it, without attached Environment Templates, and for
//! `Submission`, with every path starting at its document, with them.

use openjd_model::template::EnvironmentTemplate;
use openjd_model::{
    create_job_with_environment_templates, AttachedEnvironmentTemplate, CreatedJob, ModelError,
};

use crate::env_params_common::{env_ok, job_ok, job_params_with, posix_ctx};

/// Create the Job from `jt_src` with `env_srcs` attached, in order, and
/// Job Parameter values `params`.
fn create(
    jt_src: &str,
    env_srcs: &[&str],
    params: &[(&str, &str)],
) -> Result<CreatedJob, ModelError> {
    let jt = job_ok(jt_src);
    let envs: Vec<EnvironmentTemplate> = env_srcs.iter().map(|s| env_ok(s)).collect();
    let values = job_params_with(&jt, params, &envs);
    let attached: Vec<AttachedEnvironmentTemplate<'_>> =
        envs.iter().map(AttachedEnvironmentTemplate::new).collect();
    create_job_with_environment_templates(&jt, &attached, &values, &posix_ctx(&jt))
}

/// Job creation fails with exactly `expected`, the whole message, in order.
fn create_err(jt_src: &str, env_srcs: &[&str], params: &[(&str, &str)], expected: &str) {
    let err = create(jt_src, env_srcs, params).expect_err("expected job creation to fail");
    assert_eq!(err.to_string(), expected);
}

const HEAD: &str = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
                    name: J\n";

/// An Environment Template whose Environment is `body`, a YAML flow
/// mapping's contents.
fn env(body: &str) -> String {
    format!(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
         environment: {{{body}, variables: {{V: x}}}}\n"
    )
}

/// The probe of task 98f5063a: the Job Environment Lic, enabled by
/// `UseLic`, has `Seats` without a default, and Step S's entry for it
/// supplies none; its other entry, required by default, names `Missing`,
/// which no Environment defines.
const LIC_PROBE: &str = "specificationVersion: jobtemplate-2023-09\n\
extensions: [ENV_PARAMS, EXPR]\n\
name: Probe\n\
parameterDefinitions:\n\
- {name: UseLic, type: BOOL, default: true}\n\
jobEnvironments:\n\
- name: Lic\n  enableIf: \"Param.UseLic\"\n  envParameterDefinitions:\n  - {name: Seats, type: INT}\n  \
  variables: {SEATS: \"{{Env.Param.Seats}}\"}\n\
steps:\n\
- name: S\n  dependencies:\n  - {environment: Lic}\n  - {environment: Missing}\n  \
  script: {actions: {onRun: {command: echo, args: [S]}}}\n";

/// The step 3 error of [`LIC_PROBE`]'s `Missing` entry, at `path`.
fn missing_entry(path: &str, attached: &str) -> String {
    format!(
        "{path}:\n\tnames Environment 'Missing', which is not a jobEnvironments entry of the Job \
         Template, and {attached}. The entry is required; set 'required: false' if the Step can \
         run without the Environment (Template Schemas §3.2.1 item 2)."
    )
}

/// The constraint 7 error of Step `step` (at `path`) missing `name` of the
/// Job Environment `label`.
fn job_missing(path: &str, name: &str, label: &str) -> String {
    format!(
        "{path}:\n\tmust supply, in an 'environment:' dependency, every parameter without a \
         default of each Environment its Sessions enter (every jobEnvironments entry with \
         activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, \
         and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 \
         constraint 7); missing: '{name}' of Environment {label}."
    )
}

#[test]
fn the_lic_probe_reports_the_missing_entry_alone() {
    // Mark's example: before, the create_job body reported only the
    // constraint 7 error for Seats, and the unresolved entry was found only
    // once Seats was supplied. Now the unresolved entry (§7.4.1 step 3) is
    // reported, and it stops Step S after step 4, so its constraint 7 error
    // (step 7) is reported only once the entry resolves.
    create_err(
        LIC_PROBE,
        &[],
        &[],
        &format!(
            "Model validation error: 1 validation error for JobTemplate\n{}",
            missing_entry(
                "steps[0] -> dependencies[1] -> environment",
                "no Environment Template is attached"
            )
        ),
    );
    // With the entry fixed, the constraint 7 error.
    let fixed = LIC_PROBE.replace("  - {environment: Missing}\n", "");
    create_err(
        &fixed,
        &[],
        &[],
        &format!(
            "Model validation error: 1 validation error for JobTemplate\n{}",
            job_missing("steps[0]", "Seats", "'Lic'")
        ),
    );
    // With an Environment Template attached, the same error for Submission,
    // at the Job Template's paths.
    let other = env("name: Other, activation: ON_REQUEST");
    create_err(
        LIC_PROBE,
        &[&other],
        &[],
        &format!(
            "Model validation error: 1 validation error for Submission\n{}",
            missing_entry(
                "JobTemplate -> steps[0] -> dependencies[1] -> environment",
                "none of the attached Environment Templates (EnvironmentTemplate[0]) defines it"
            )
        ),
    );
}

#[test]
fn a_step_3_error_precedes_another_steps_step_7_error() {
    // Step A misses Seats (§7.4.1 step 7; its entry's enableIf is an
    // expression, so template validation leaves it to job creation); Step B
    // names Missing (step 3). Both are reported, B's first: by step before
    // Step order. B misses Seats too, which is not reported: the step 3
    // error stops B after step 4.
    let jt = format!(
        "{HEAD}parameterDefinitions: [{{name: UseLic, type: BOOL, default: true}}]\n\
         jobEnvironments:\n- {{name: Lic, activation: ON_REQUEST, envParameterDefinitions: \
         [{{name: Seats, type: INT}}], variables: {{V: v}}}}\nsteps:\n\
         - name: A\n  dependencies: [{{environment: Lic, enableIf: \"Param.UseLic\"}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: B\n  dependencies: [{{environment: Missing}}, {{environment: Lic, enableIf: \
         \"Param.UseLic\"}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    create_err(
        &jt,
        &[],
        &[],
        &format!(
            "Model validation error: 2 validation errors for JobTemplate\n{}\n{}",
            missing_entry(
                "steps[1] -> dependencies[0] -> environment",
                "no Environment Template is attached"
            ),
            job_missing("steps[0]", "Seats", "'Lic'")
        ),
    );
}

/// An Environment body whose `enableIf` fails when it is evaluated.
const FAILING: &str = "envParameterDefinitions: [{name: S, type: STRING, default: abc}], \
                       enableIf: \"int(Env.Param.S) > 0\"";

#[test]
fn errors_are_reported_by_step_then_step_then_environment() {
    // One submission failing at every stage:
    // - template-validation style: Step B's script reads Param.L[4];
    // - §7.4.1 step 2: Step B's entry for Y has an enableIf that fails, so
    //   B gets no checks from steps 5-8 (B also misses Q of K, unreported);
    // - step 6: for Step A, the enableIf of the attached X and of the Job
    //   Environment J fail;
    // - step 7: Step A misses P of the attached Y and Q of the Job
    //   Environment K;
    // - step 8: Step A's own host requirements conflict with H's.
    let jt = format!(
        "{HEAD}parameterDefinitions: [{{name: L, type: 'LIST[INT]', default: [1]}}]\n\
         jobEnvironments:\n\
         - {{name: J, {FAILING}, variables: {{V: j}}}}\n\
         - {{name: K, enableIf: \"Param.L[0] == 1\", envParameterDefinitions: [{{name: Q, \
         type: INT}}], variables: {{V: k}}}}\n\
         - {{name: H, hostRequirements: {{amounts: [{{name: amount.custom, min: 4}}]}}, \
         variables: {{V: h}}}}\n\
         steps:\n\
         - name: A\n  hostRequirements: {{amounts: [{{name: amount.custom, max: 2}}]}}\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: B\n  dependencies: [{{environment: Y, enableIf: \"Param.L[4] == 1\"}}]\n  \
         script: {{actions: {{onRun: {{command: x, args: ['{{{{Param.L[4]}}}}']}}}}}}\n"
    );
    let x = env(&format!("name: X, {FAILING}"));
    let y = env("name: Y, envParameterDefinitions: [{name: P, type: STRING}]");
    let failing = |path: &str| {
        format!(
            "{path} -> enableIf:\n\tcould not be evaluated: Cannot convert 'abc' to int. \
             Resolved for Step 'A'.\n  int(Env.Param.S) > 0\n  ^~~~~~~~~~~~~~~~"
        )
    };
    let errors = [
        "JobTemplate -> steps[1] -> script -> actions -> onRun -> args[0]:\n\tFailed to parse \
         interpolation expression at [0, 14]. Index 4 out of bounds for list of length 1\n  \
         Param.L[4]\n  ~~~~~~~^~~"
            .to_string(),
        "JobTemplate -> steps[1] -> dependencies[0] -> enableIf:\n\tcould not be evaluated: \
         Index 4 out of bounds for list of length 1\n  Param.L[4] == 1\n  ~~~~~~~^~~"
            .to_string(),
        failing("EnvironmentTemplate[0] -> environment"),
        failing("JobTemplate -> jobEnvironments[0]"),
        "JobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every \
         parameter without a default of each Environment its Sessions enter (every attached \
         Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Step \
         does not decline, and each one with activation ON_REQUEST that the Step requests; \
         Template Schemas §3.2.1); missing: 'P' of Environment 'Y' (EnvironmentTemplate[1])."
            .to_string(),
        job_missing("JobTemplate -> steps[0]", "Q", "'K'"),
        "JobTemplate -> steps[0]:\n\tStep 'A': no host can satisfy the combined host \
         requirements for amount 'amount.custom': JobTemplate -> jobEnvironments[2] -> \
         hostRequirements (Environment 'H') requires min 4, and JobTemplate -> steps[0] -> \
         hostRequirements requires max 2. Template Schemas §3.3.3."
            .to_string(),
    ];
    create_err(
        &jt,
        &[&x, &y],
        &[],
        &format!(
            "Model validation error: 7 validation errors for Submission\n{}",
            errors.join("\n")
        ),
    );
}

#[test]
fn within_a_step_errors_follow_the_environment_order_not_the_order_found() {
    // X (first attached) misses P, which is found after the Step's values;
    // Y (second) has a value below its minimum, found while they are
    // computed. X's error comes first.
    let jt = format!(
        "{HEAD}steps:\n- name: S\n  dependencies: [{{environment: Y, parameters: {{N: 1}}}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let x = env("name: X, envParameterDefinitions: [{name: P, type: STRING}]");
    let y = env(
        "name: Y, activation: ON_REQUEST, envParameterDefinitions: [{name: N, type: INT, \
         minValue: 5}]",
    );
    create_err(
        &jt,
        &[&x, &y],
        &[],
        "Model validation error: 2 validation errors for Submission\n\
         JobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every \
         parameter without a default of each Environment its Sessions enter (every attached \
         Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Step \
         does not decline, and each one with activation ON_REQUEST that the Step requests; \
         Template Schemas §3.2.1); missing: 'P' of Environment 'X' (EnvironmentTemplate[0]).\n\
         JobTemplate -> steps[0] -> dependencies[0] -> parameters -> N:\n\tParameter 'N': value \
         1 is less than minimum 5",
    );
    // A Step's entries' errors follow the entries' order, though disabled
    // entries are resolved first.
    let jt = format!(
        "{HEAD}steps:\n- name: S\n  dependencies: [{{environment: Missing}}, {{environment: \
         Amb, enableIf: false}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let amb = env("name: Amb");
    create_err(
        &jt,
        &[&amb, &amb],
        &[],
        &format!(
            "Model validation error: 2 validation errors for Submission\n{}\n\
             JobTemplate -> steps[0] -> dependencies[1] -> environment:\n\tnames Environment \
             'Amb', which 2 attached Environment Templates define (EnvironmentTemplate[0], \
             EnvironmentTemplate[1]); an entry must match exactly one (Template Schemas §3.2.1 \
             item 3).",
            missing_entry(
                "JobTemplate -> steps[0] -> dependencies[0] -> environment",
                "none of the attached Environment Templates (EnvironmentTemplate[0], \
                 EnvironmentTemplate[1]) defines it"
            )
        ),
    );
}

#[test]
fn an_error_naming_several_steps_is_filed_under_the_first() {
    // X, attached, fails its enableIf only for Step S1 (S0 supplies S); the
    // Job Environment J fails for S0. By Step before Environment, J's error
    // comes first.
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: J, {FAILING}, variables: {{V: j}}}}\nsteps:\n\
         - name: S0\n  dependencies: [{{environment: X, parameters: {{S: '1'}}}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: S1\n  dependencies: [{{environment: J, parameters: {{S: '1'}}}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let x = env(&format!("name: X, {FAILING}"));
    let failing = |path: &str, step: &str| {
        format!(
            "{path} -> enableIf:\n\tcould not be evaluated: Cannot convert 'abc' to int. \
             Resolved for Step '{step}'.\n  int(Env.Param.S) > 0\n  ^~~~~~~~~~~~~~~~"
        )
    };
    create_err(
        &jt,
        &[&x],
        &[],
        &format!(
            "Model validation error: 2 validation errors for Submission\n{}\n{}",
            failing("JobTemplate -> jobEnvironments[0]", "S0"),
            failing("EnvironmentTemplate[0] -> environment", "S1")
        ),
    );
}

/// The wrap hooks that make an Environment a layer (RFC 0008).
const WRAP: &str = "script: {actions: {onWrapEnvEnter: {command: x}, onWrapTaskRun: {command: \
                    x}, onWrapEnvExit: {command: x}}}";

#[test]
fn a_stopped_step_gets_no_single_layer_or_host_requirements_check() {
    // Step S enters the wrapping W1 and W2 (W2's enableIf is an expression,
    // so template validation leaves the rule to job creation), and H, whose
    // requirements conflict with S's own. Its entry naming Missing stops it
    // after §7.4.1 step 4, so only that is reported.
    let jt = format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, \
         EXPR]\nname: J\njobEnvironments:\n\
         - {{name: W1, {WRAP}}}\n- {{name: W2, enableIf: \"Job.Name == 'J'\", {WRAP}}}\n\
         - {{name: H, hostRequirements: {{amounts: [{{name: amount.custom, min: 4}}]}}, \
         variables: {{V: h}}}}\nsteps:\n- name: S\n  dependencies: [{{environment: Missing}}]\n  \
         hostRequirements: {{amounts: [{{name: amount.custom, max: 2}}]}}\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    create_err(
        &jt,
        &[],
        &[],
        &format!(
            "Model validation error: 1 validation error for JobTemplate\n{}",
            missing_entry(
                "steps[0] -> dependencies[0] -> environment",
                "no Environment Template is attached"
            )
        ),
    );
    // Without the entry, both are reported.
    let fixed = jt.replace("  dependencies: [{environment: Missing}]\n", "");
    let err = create(&fixed, &[], &[]).expect_err("expected job creation to fail");
    let err = err.to_string();
    assert!(
        err.starts_with("Model validation error: 2 validation errors for JobTemplate\nsteps[0]:\n\tits Sessions would enter 2 Environments"),
        "{err}"
    );
    assert!(
        err.contains("no host can satisfy the combined host requirements"),
        "{err}"
    );
}

#[test]
fn the_caller_limits_are_checked_only_when_job_creation_proper_found_no_error() {
    // The caller limits abort job creation, after the Job Template's own
    // §7.4.1 steps and before the attachments are applied. With an error
    // found already (here a failed entry enableIf, §7.4.1 step 2) they are
    // not checked, and the errors are reported.
    let src = |enable_if: &str| {
        format!(
            "{HEAD}parameterDefinitions: [{{name: L, type: 'LIST[INT]', default: [1]}}]\n\
             jobEnvironments:\n- {{name: E, variables: {{V: e}}}}\nsteps:\n- name: S\n  \
             dependencies: [{{environment: E, enableIf: \"{enable_if}\"}}]\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    let ctx = |jt: &openjd_model::template::JobTemplate| {
        posix_ctx(jt).with_caller_limits(openjd_model::CallerLimits {
            max_task_count: Some(0),
            ..Default::default()
        })
    };
    let jt = job_ok(&src("Param.L[4] == 1"));
    let values = job_params_with(&jt, &[], &[]);
    let err = openjd_model::create_job(&jt, &values, &ctx(&jt)).expect_err("expected an error");
    assert_eq!(
        err.to_string(),
        "Model validation error: 1 validation error for JobTemplate\n\
         steps[0] -> dependencies[0] -> enableIf:\n\tcould not be evaluated: Index 4 out of \
         bounds for list of length 1\n  Param.L[4] == 1\n  ~~~~~~~^~~"
    );
    let jt = job_ok(&src("Param.L[0] == 1"));
    let values = job_params_with(&jt, &[], &[]);
    let err = openjd_model::create_job(&jt, &values, &ctx(&jt)).expect_err("expected an error");
    assert_eq!(
        err.to_string(),
        "Model validation error: 1 validation error for Template\nTemplate: Total task count \
         (1) exceeds caller limit of 0."
    );
}

#[test]
fn a_stopped_step_gets_no_single_layer_check_of_its_step_environments() {
    // S's two wrapping Step Environments are both entered (Inner's enableIf
    // is an expression, so template validation leaves the rule to job
    // creation), but S's entry naming Missing stops it after §7.4.1 step 4.
    let jt = format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, \
         EXPR]\nname: J\nsteps:\n- name: S\n  dependencies: [{{environment: Missing}}]\n  \
         stepEnvironments:\n  - {{name: Outer, {WRAP}}}\n  - {{name: Inner, enableIf: \
         \"Job.Name == 'J'\", {WRAP}}}\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    create_err(
        &jt,
        &[],
        &[],
        &format!(
            "Model validation error: 1 validation error for JobTemplate\n{}",
            missing_entry(
                "steps[0] -> dependencies[0] -> environment",
                "no Environment Template is attached"
            )
        ),
    );
    let fixed = jt.replace("  dependencies: [{environment: Missing}]\n", "");
    create_err(
        &fixed,
        &[],
        &[],
        "Model validation error: 1 validation error for JobTemplate\nsteps[0]:\n\tits Sessions \
         would enter 2 Environments that define wrap hooks (onWrapEnvEnter, onWrapTaskRun, \
         onWrapEnvExit): 'Outer' (stepEnvironments), 'Inner' (stepEnvironments). A Session may \
         have at most one Environment defining wrap hooks (RFC 0008).",
    );
}
