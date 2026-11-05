// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! The order of job creation's errors (Template Schemas §7.4.1), for every
//! Job Template, with or without `ENV_PARAMS`.
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
//!
//! [`create`], which most tests here use, also creates every failing Job
//! Template that lists `ENV_PARAMS` again with `ENV_PARAMS` removed in
//! code, and checks that the error is the same, so each of these tests also
//! pins the order for a Job Template without `ENV_PARAMS`. (When the
//! template has entries or Environment parameters, that twin is one
//! template validation would reject, a template built or edited in code.)

use openjd_model::template::EnvironmentTemplate;
use openjd_model::{
    create_job_with_environment_templates, AttachedEnvironmentTemplate, CreatedJob, ModelError,
};

use crate::env_params_common::{env_ok, job_ok, job_params_with, template_ctx};

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
    let result = create_job_with_environment_templates(&jt, &attached, &values, &template_ctx(&jt));
    // Every error order test is also a test of a Job Template without
    // ENV_PARAMS: the one-pass order applies to every Job Template, so with
    // ENV_PARAMS removed in code the template's errors are the same.
    if let Err(e) = &result {
        if lists_env_params(&jt) {
            let mut plain = jt.clone();
            plain
                .extensions
                .as_mut()
                .unwrap()
                .retain(|x| x.as_str() != "ENV_PARAMS");
            let twin = create_job_with_environment_templates(
                &plain,
                &attached,
                &values,
                &template_ctx(&plain),
            );
            assert_eq!(
                twin.err().map(|e| e.to_string()),
                Some(e.to_string()),
                "the same Job Template without ENV_PARAMS"
            );
        }
    }
    result
}

fn lists_env_params(jt: &openjd_model::template::JobTemplate) -> bool {
    jt.extensions
        .iter()
        .flatten()
        .any(|x| x.as_str() == "ENV_PARAMS")
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
fn an_error_is_ordered_by_its_step_before_the_environment_order() {
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
    // Without the entry, both are reported, the single-layer rule (step 7)
    // before the host requirements (step 8).
    let fixed = jt.replace("  dependencies: [{environment: Missing}]\n", "");
    create_err(
        &fixed,
        &[],
        &[],
        "Model validation error: 2 validation errors for JobTemplate\nsteps[0]:\n\tits \
         Sessions would enter 2 Environments that define wrap hooks (onWrapEnvEnter, \
         onWrapTaskRun, onWrapEnvExit): 'W1', 'W2'. A Session may have at most one Environment \
         defining wrap hooks (RFC 0008).\nsteps[0]:\n\tStep 'S': no host can satisfy the \
         combined host requirements for amount 'amount.custom': jobEnvironments[2] -> \
         hostRequirements (Environment 'H') requires min 4, and steps[0] -> hostRequirements \
         requires max 2. Template Schemas §3.3.3.",
    );
}

#[test]
fn the_caller_limits_are_checked_only_when_no_error_was_found() {
    // The caller limits abort job creation, but only when no other error
    // was found. With an error (here a failed entry enableIf, §7.4.1 step
    // 2) they are not reported, and the errors are.
    let src = |enable_if: &str| {
        format!(
            "{HEAD}parameterDefinitions: [{{name: L, type: 'LIST[INT]', default: [1]}}]\n\
             jobEnvironments:\n- {{name: E, variables: {{V: e}}}}\nsteps:\n- name: S\n  \
             dependencies: [{{environment: E, enableIf: \"{enable_if}\"}}]\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    let ctx = |jt: &openjd_model::template::JobTemplate| {
        template_ctx(jt).with_caller_limits(openjd_model::CallerLimits {
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

#[test]
fn a_job_template_without_env_params_reports_its_errors_with_the_attachments_in_one_pass() {
    // Without ENV_PARAMS too, the Job Template's own errors and the ones
    // the attachment adds are found in one pass and reported together, in
    // §7.4.1 order, for Submission: the timeout (a template-validation-style
    // check) before the single-layer error (step 7) that the attachment
    // adds. Before, the timeout was reported alone, for JobTemplate.
    let jt = format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [WRAP_ACTIONS, EXPR, FEATURE_BUNDLE_1]\nname: J\n\
         parameterDefinitions: [{{name: T, type: INT, default: 0}}]\n\
         jobEnvironments:\n- {{name: W1, {WRAP}}}\nsteps:\n- name: S\n  \
         script: {{actions: {{onRun: {{command: x, timeout: '{{{{Param.T}}}}'}}}}}}\n"
    );
    let w2 = format!(
        "specificationVersion: environment-2023-09\nextensions: [WRAP_ACTIONS, EXPR]\n\
         environment: {{name: W2, {WRAP}}}\n"
    );
    create_err(
        &jt,
        &[&w2],
        &[],
        "Model validation error: 2 validation errors for Submission\n\
         JobTemplate -> steps[0] -> script -> actions -> onRun -> timeout:\n\ttimeout must be \
         > 0.\nJobTemplate -> steps[0]:\n\tits Sessions would enter 2 Environments that define \
         wrap hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'W2' \
         (EnvironmentTemplate[0]), 'W1'. A Session may have at most one Environment defining \
         wrap hooks (RFC 0008).",
    );
    // Without the attachment, the timeout alone, for JobTemplate, at the
    // Job Template's paths, as before.
    create_err(
        &jt,
        &[],
        &[],
        "Model validation error: 1 validation error for JobTemplate\n\
         steps[0] -> script -> actions -> onRun -> timeout:\n\ttimeout must be > 0.",
    );
    // With the timeout fixed, the attachment's single-layer error alone.
    create_err(
        &jt,
        &[&w2],
        &[("T", "5")],
        "Model validation error: 1 validation error for Submission\nJobTemplate -> steps[0]:\n\
         \tits Sessions would enter 2 Environments that define wrap hooks (onWrapEnvEnter, \
         onWrapTaskRun, onWrapEnvExit): 'W2' (EnvironmentTemplate[0]), 'W1'. A Session may have \
         at most one Environment defining wrap hooks (RFC 0008).",
    );
}

// ── Positions and stages, one at a time ──────────────────────────────
//
// Each test below has errors whose filing order differs from their report
// order, so it pins one key of the order.

/// An attached Environment `name` whose INT `N` (at least 5) has its
/// literal `default` edited after decoding to 1, so job creation's
/// template-validation-style check of the default rejects it.
fn below_minimum_default(name: &str) -> EnvironmentTemplate {
    let mut x = env_ok(&env(&format!(
        "name: {name}, envParameterDefinitions: [{{name: N, type: INT, minValue: 5, default: 5}}]"
    )));
    x.environment.env_parameter_definitions.as_mut().unwrap()[0].default = env_ok(&env(
        "name: O, envParameterDefinitions: [{name: N, type: INT, default: 1}]",
    ))
    .environment
    .env_parameter_definitions
    .unwrap()[0]
        .default
        .clone();
    x
}

/// Create `jt` with the decoded (and maybe edited) `envs` attached; the
/// error's text.
fn err_text_envs(jt: &str, envs: &[EnvironmentTemplate]) -> String {
    let jt = job_ok(jt);
    let values = job_params_with(&jt, &[], envs);
    let attached: Vec<_> = envs.iter().map(AttachedEnvironmentTemplate::new).collect();
    create_job_with_environment_templates(&jt, &attached, &values, &template_ctx(&jt))
        .expect_err("expected job creation to fail")
        .to_string()
}

/// Run `jt` with `envs` attached and return the error's text.
fn err_text(jt: &str, envs: &[&str], params: &[(&str, &str)]) -> String {
    create(jt, envs, params)
        .expect_err("expected job creation to fail")
        .to_string()
}

#[test]
fn order_step_4_by_entry_index() {
    // Both entries are disabled (by an expression, so template validation
    // leaves them) and name ALWAYS Environments: the attached one at entry
    // 0 is resolved by the submission stage, after job creation proper
    // found the Job Environment's at entry 1.
    let jt = format!(
        "{HEAD}parameterDefinitions: [{{name: B, type: BOOL, default: false}}]\n\
         jobEnvironments:\n- {{name: J, activation: ALWAYS, variables: {{V: j}}}}\nsteps:\n\
         - name: S\n  dependencies: [{{environment: X, enableIf: \"Param.B\"}}, {{environment: J, \
         enableIf: \"Param.B\"}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let x = env("name: X, activation: ALWAYS");
    assert_eq!(err_text(&jt, &[&x], &[]), "Model validation error: 2 validation errors for Submission\nJobTemplate -> steps[0] -> dependencies[0] -> enableIf:\n\tis false, so Step 'S' would decline Environment 'X' (EnvironmentTemplate[0]), whose activation is ALWAYS: no Step can leave out an ALWAYS Environment (Template Schemas §3.2 item 3.2).\nJobTemplate -> steps[0] -> dependencies[1] -> enableIf:\n\tis false, so Step 'S' would decline Environment 'J', whose activation is ALWAYS: no Step can leave out an ALWAYS Environment (Template Schemas §3.2 item 3.2).");
}

#[test]
fn order_step_5_by_environment_order() {
    // Entry 0 names the Job Environment J (an expression enableIf, so
    // template validation leaves its keys), entry 1 the attached X; both
    // supply an undeclared key. X comes first in the Job's order.
    let jt = format!(
        "{HEAD}parameterDefinitions: [{{name: B, type: BOOL, default: true}}]\n\
         jobEnvironments:\n- {{name: J, envParameterDefinitions: [{{name: P, type: STRING, \
         default: p}}], variables: {{V: j}}}}\nsteps:\n- name: S\n  dependencies: [{{environment: \
         J, enableIf: \"Param.B\", parameters: {{Q: 1}}}}, {{environment: X, parameters: {{Q: \
         1}}}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let x = env(
        "name: X, activation: ON_REQUEST, envParameterDefinitions: [{name: P, type: \
                 STRING, default: p}]",
    );
    assert_eq!(err_text(&jt, &[&x], &[]), "Model validation error: 2 validation errors for Submission\nJobTemplate -> steps[0] -> dependencies[1] -> parameters -> Q:\n\tEnvironment 'X' (EnvironmentTemplate[0]) declares no parameter 'Q'. Declared: P.\nJobTemplate -> steps[0] -> dependencies[0] -> parameters -> Q:\n\tEnvironment 'J' declares no parameter 'Q'. Declared: P.");
}

#[test]
fn order_step_6_by_environment_order() {
    // For Step S, the attachment X1, the Job Environment J, and the Step
    // Environment SE fail their enableIf; X0 does not. X1 is filed after J
    // was found, and SE before either.
    let jt = format!(
        "{HEAD}parameterDefinitions: [{{name: L, type: 'LIST[INT]', default: [1]}}]\n\
         jobEnvironments:\n- {{name: J, {FAILING}, variables: {{V: j}}}}\nsteps:\n- name: S\n  \
         stepEnvironments:\n  - {{name: SE, enableIf: \"Param.L[4] == 1\", variables: {{V: s}}}}\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let x0 = env("name: X0");
    let x1 = env(&format!("name: X1, {FAILING}"));
    assert_eq!(err_text(&jt, &[&x0, &x1], &[]), "Model validation error: 3 validation errors for Submission\nEnvironmentTemplate[1] -> environment -> enableIf:\n\tcould not be evaluated: Cannot convert 'abc' to int. Resolved for Step 'S'.\n  int(Env.Param.S) > 0\n  ^~~~~~~~~~~~~~~~\nJobTemplate -> jobEnvironments[0] -> enableIf:\n\tcould not be evaluated: Cannot convert 'abc' to int. Resolved for Step 'S'.\n  int(Env.Param.S) > 0\n  ^~~~~~~~~~~~~~~~\nJobTemplate -> steps[0] -> stepEnvironments[0] -> enableIf:\n\tcould not be evaluated: Index 4 out of bounds for list of length 1\n  Param.L[4] == 1\n  ~~~~~~~^~~");
}

#[test]
fn order_step_6_two_step_environments_of_one_step_by_their_order() {
    // Both Step Environments of Step S fail their enableIf (§7.4.1 step 6),
    // so both errors are reported, in the Step's stepEnvironments order,
    // whichever fails which way. Template Schemas §1.1 item 7's Job's
    // Environment order covers only the attached and Job Environments; the
    // prototype files a Step's Step Environments after all of them, in
    // their written order. A Step Environment declares no parameters, so
    // each enableIf reads a Job Parameter.
    let se_t = "{name: ST, enableIf: \"int(Param.T) > 0\", variables: {V: a}}";
    let se_l = "{name: SL, enableIf: \"Param.L[4] == 1\", variables: {V: b}}";
    let t_err = |i: usize| {
        format!(
            "steps[0] -> stepEnvironments[{i}] -> enableIf:\n\tcould not be evaluated: Cannot \
             convert 'abc' to int\n  int(Param.T) > 0\n  ^~~~~~~~~~~~"
        )
    };
    let l_err = |i: usize| {
        format!(
            "steps[0] -> stepEnvironments[{i}] -> enableIf:\n\tcould not be evaluated: Index 4 \
             out of bounds for list of length 1\n  Param.L[4] == 1\n  ~~~~~~~^~~"
        )
    };
    for (first, second, errors) in [
        (se_t, se_l, [t_err(0), l_err(1)]),
        (se_l, se_t, [l_err(0), t_err(1)]),
    ] {
        let jt = format!(
            "{HEAD}parameterDefinitions: [{{name: L, type: 'LIST[INT]', default: [1]}}, {{name: \
             T, type: STRING, default: abc}}]\nsteps:\n- name: S\n  stepEnvironments:\n  - \
             {first}\n  - {second}\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        );
        create_err(
            &jt,
            &[],
            &[],
            &format!(
                "Model validation error: 2 validation errors for JobTemplate\n{}",
                errors.join("\n")
            ),
        );
    }
}

#[test]
fn order_step_7_by_environment_order() {
    // J1 misses P, which is found after the Step's values; J2's value is
    // below its minimum, found while they are computed; the attachment X,
    // named like S's Step Environment, has a parameter without a default
    // that no entry can reach. By the Job's Environment order: X, J1, J2.
    let jt = format!(
        "{HEAD}parameterDefinitions: [{{name: B, type: BOOL, default: true}}]\n\
         jobEnvironments:\n\
         - {{name: J1, envParameterDefinitions: [{{name: P, type: STRING}}], enableIf: \
         \"Param.B\", variables: {{V: a}}}}\n\
         - {{name: J2, envParameterDefinitions: [{{name: N, type: INT, minValue: 5}}], \
         variables: {{V: b}}}}\nsteps:\n- name: S\n  dependencies: [{{environment: J2, \
         enableIf: \"Param.B\", parameters: {{N: 1}}}}]\n  stepEnvironments:\n  - {{name: X, \
         variables: {{V: s}}}}\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let x = env("name: X, envParameterDefinitions: [{name: Q, type: STRING}]");
    assert_eq!(err_text(&jt, &[&x], &[]), "Model validation error: 3 validation errors for Submission\nJobTemplate -> steps[0]:\n\tcannot supply the parameters without a default of Environment 'X' (EnvironmentTemplate[0]): Q. It has the same name as a stepEnvironments entry of this Step, so no 'environment:' dependency of this Step can reach it (Template Schemas §3.2.1 item 5), and its activation is BY_DEFAULT, so this Step's Sessions enter it.\nJobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every jobEnvironments entry with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7); missing: 'P' of Environment 'J1'.\nJobTemplate -> steps[0] -> dependencies[0] -> parameters -> N:\n\tParameter 'N': value 1 is less than minimum 5");
}

#[test]
fn order_step_8_by_step_then_environment() {
    // E1's requirements fail to resolve, for S0 and S1 alike (one error,
    // filed under S0); E0's conflict with each Step's own. S0's errors, then
    // S1's.
    let jt = String::from(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR, \
         FEATURE_BUNDLE_1]\nname: J\n\
         parameterDefinitions: [{name: L, type: 'LIST[INT]', default: [1]}]\n\
         jobEnvironments:\n\
         - {name: E0, hostRequirements: {amounts: [{name: amount.custom, min: 4}]}, \
         variables: {V: a}}\n\
         - {name: E1, hostRequirements: {amounts: [{name: amount.other, min: \
         \"{{Param.L[4]}}\"}]}, variables: {V: b}}\nsteps:\n\
         - name: S0\n  hostRequirements: {amounts: [{name: amount.custom, max: 2}]}\n  \
         script: {actions: {onRun: {command: x}}}\n\
         - name: S1\n  hostRequirements: {amounts: [{name: amount.custom, max: 2}]}\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    assert_eq!(err_text(&jt, &[], &[]), "Model validation error: 3 validation errors for JobTemplate\njobEnvironments[1] -> hostRequirements -> amounts[0] -> min:\n\tFormat string error: hostRequirements amount min: Index 4 out of bounds for list of length 1. Resolved for Steps 'S0' and 'S1'.\n  Param.L[4]\n  ~~~~~~~^~~\nsteps[0]:\n\tStep 'S0': no host can satisfy the combined host requirements for amount 'amount.custom': jobEnvironments[0] -> hostRequirements (Environment 'E0') requires min 4, and steps[0] -> hostRequirements requires max 2. Template Schemas §3.3.3.\nsteps[1]:\n\tStep 'S1': no host can satisfy the combined host requirements for amount 'amount.custom': jobEnvironments[0] -> hostRequirements (Environment 'E0') requires min 4, and steps[1] -> hostRequirements requires max 2. Template Schemas §3.3.3.");
}

#[test]
fn order_step_6_always_error_under_its_first_step() {
    // A (ALWAYS) is false only for S1, which supplies P: 'no'; J fails only
    // for S0, which does not supply S. S0's error comes first although A is
    // earlier in the Job's order.
    let jt = format!(
        "{HEAD}jobEnvironments:\n\
         - {{name: A, activation: ALWAYS, envParameterDefinitions: [{{name: P, type: STRING, \
         default: 'yes'}}], enableIf: \"Env.Param.P == 'yes'\", variables: {{V: a}}}}\n\
         - {{name: J, {FAILING}, variables: {{V: j}}}}\nsteps:\n\
         - name: S0\n  script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: S1\n  dependencies: [{{environment: A, parameters: {{P: 'no'}}}}, {{environment: \
         J, parameters: {{S: '1'}}}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    assert_eq!(err_text(&jt, &[], &[]), "Model validation error: 2 validation errors for JobTemplate\njobEnvironments[1] -> enableIf:\n\tcould not be evaluated: Cannot convert 'abc' to int. Resolved for Step 'S0'.\n  int(Env.Param.S) > 0\n  ^~~~~~~~~~~~~~~~\njobEnvironments[0] -> enableIf:\n\tEnvironment 'A' has activation ALWAYS, so it cannot be disabled, but its enableIf is false for Step 'S1' (Template Schemas §4 item 4).");
}

#[test]
fn order_stages_within_one_step() {
    // In one Step: a carried-forward check of its script (template-
    // validation style), an undeclared key (step 5), a failed enableIf
    // (step 6), and a missing value (step 7).
    let jt = format!(
        "{HEAD}parameterDefinitions: [{{name: L, type: 'LIST[INT]', default: [1]}}, {{name: B, \
         type: BOOL, default: true}}]\n\
         jobEnvironments:\n\
         - {{name: J, envParameterDefinitions: [{{name: P, type: STRING}}], enableIf: \
         \"Param.L[4] == 1\", variables: {{V: j}}}}\nsteps:\n\
         - name: S\n  dependencies: [{{environment: J, enableIf: \"Param.B\", parameters: \
         {{Z: 1}}}}]\n  script: {{actions: {{onRun: {{command: x, args: \
         ['{{{{Param.L[4]}}}}']}}}}}}\n"
    );
    assert_eq!(err_text(&jt, &[], &[]), "Model validation error: 4 validation errors for JobTemplate\nsteps[0] -> script -> actions -> onRun -> args[0]:\n\tFailed to parse interpolation expression at [0, 14]. Index 4 out of bounds for list of length 1\n  Param.L[4]\n  ~~~~~~~^~~\nsteps[0] -> dependencies[0] -> parameters -> Z:\n\tEnvironment 'J' declares no parameter 'Z'. Declared: P.\njobEnvironments[0] -> enableIf:\n\tcould not be evaluated: Index 4 out of bounds for list of length 1. Resolved for Step 'S'.\n  Param.L[4] == 1\n  ~~~~~~~^~~\nsteps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every jobEnvironments entry with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7); missing: 'P' of Environment 'J' (its own enableIf could not be evaluated for this Step, so the Environment is checked as if it is not disabled).");
}

#[test]
fn order_unreachable_before_host_requirements() {
    // U, attached under a Job Environment's name, has a parameter no Step
    // can supply (step 7, about no single Step); S's requirements conflict
    // with E's (step 8).
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: U, variables: {{V: u}}}}\n\
         - {{name: E, hostRequirements: {{amounts: [{{name: amount.custom, min: 4}}]}}, \
         variables: {{V: e}}}}\nsteps:\n- name: S\n  hostRequirements: {{amounts: [{{name: \
         amount.custom, max: 2}}]}}\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let u = env("name: U, envParameterDefinitions: [{name: P, type: STRING}]");
    assert_eq!(err_text(&jt, &[&u], &[]), "Model validation error: 2 validation errors for Submission\nEnvironmentTemplate[0] -> environment -> envParameterDefinitions:\n\tEnvironment 'U' has the same name as a jobEnvironments entry of the Job Template, so no 'environment:' dependency can reach it (Template Schemas §3.2.1 item 4), and it has parameters without a default that no Step can supply: P. Its activation is BY_DEFAULT, so every Session enters it, and the submission is rejected (§3.2.1).\nJobTemplate -> steps[0]:\n\tStep 'S': no host can satisfy the combined host requirements for amount 'amount.custom': JobTemplate -> jobEnvironments[1] -> hostRequirements (Environment 'E') requires min 4, and JobTemplate -> steps[0] -> hostRequirements requires max 2. Template Schemas §3.3.3.");
}

#[test]
fn an_uninstantiable_step_reports_the_errors_found_in_the_submissions_shape() {
    // S0's step-level let fails, so it cannot be instantiated and the
    // attachments are not applied; S1's entry enableIf failed before that.
    // With an attachment, the errors are for Submission.
    let jt = format!(
        "{HEAD}parameterDefinitions: [{{name: L, type: 'LIST[INT]', default: [1]}}]\n\
         jobEnvironments:\n- {{name: E, variables: {{V: e}}}}\nsteps:\n\
         - name: S0\n  let: [\"a = Param.L[4]\"]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: S1\n  dependencies: [{{environment: E, enableIf: \"Param.L[4] == 1\"}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    assert_eq!(err_text(&jt, &[&env("name: X")], &[]), "Model validation error: 2 validation errors for Submission\nJobTemplate -> steps[0] -> let[0]:\n\tInvalid expression in let binding 'a': Index 4 out of bounds for list of length 1\n  a = Param.L[4]\n      ~~~~~~~^~~\nJobTemplate -> steps[1] -> dependencies[0] -> enableIf:\n\tcould not be evaluated: Index 4 out of bounds for list of length 1\n  Param.L[4] == 1\n  ~~~~~~~^~~");
    assert_eq!(err_text(&jt, &[], &[]), "Model validation error: 2 validation errors for JobTemplate\nsteps[0] -> let[0]:\n\tInvalid expression in let binding 'a': Index 4 out of bounds for list of length 1\n  a = Param.L[4]\n      ~~~~~~~^~~\nsteps[1] -> dependencies[0] -> enableIf:\n\tcould not be evaluated: Index 4 out of bounds for list of length 1\n  Param.L[4] == 1\n  ~~~~~~~^~~");
}

#[test]
fn the_caller_limits_are_not_checked_after_a_template_validation_style_error() {
    let jt = job_ok(&format!(
        "{HEAD}parameterDefinitions: [{{name: L, type: 'LIST[INT]', default: [1]}}]\nsteps:\n\
         - name: S\n  script: {{actions: {{onRun: {{command: x, args: ['{{{{Param.L[4]}}}}']}}}}}}\n"
    ));
    let values = job_params_with(&jt, &[], &[]);
    let ctx = template_ctx(&jt).with_caller_limits(openjd_model::CallerLimits {
        max_task_count: Some(0),
        ..Default::default()
    });
    let err = openjd_model::create_job(&jt, &values, &ctx).expect_err("expected an error");
    assert_eq!(err.to_string(), "Model validation error: 1 validation error for JobTemplate\nsteps[0] -> script -> actions -> onRun -> args[0]:\n\tFailed to parse interpolation expression at [0, 14]. Index 4 out of bounds for list of length 1\n  Param.L[4]\n  ~~~~~~~^~~");
}

#[test]
fn order_step_4_by_entry_index_after_a_job_environment_entry() {
    // Entry 1 names the ALWAYS Job Environment J and entry 2 the ALWAYS
    // attachment X, both disabled: J's error first.
    let jt = format!(
        "{HEAD}parameterDefinitions: [{{name: B, type: BOOL, default: false}}]\n\
         jobEnvironments:\n- {{name: E, activation: ON_REQUEST, variables: {{V: e}}}}\n\
         - {{name: J, activation: ALWAYS, variables: {{V: j}}}}\nsteps:\n\
         - name: S\n  dependencies: [{{environment: E}}, {{environment: J, enableIf: \
         \"Param.B\"}}, {{environment: X, enableIf: \"Param.B\"}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let x = env("name: X, activation: ALWAYS");
    let decline = |dep: usize, label: &str| {
        format!(
            "JobTemplate -> steps[0] -> dependencies[{dep}] -> enableIf:\n\tis false, so Step \
             'S' would decline Environment {label}, whose activation is ALWAYS: no Step can \
             leave out an ALWAYS Environment (Template Schemas §3.2 item 3.2)."
        )
    };
    assert_eq!(
        err_text(&jt, &[&x], &[]),
        format!(
            "Model validation error: 2 validation errors for Submission\n{}\n{}",
            decline(1, "'J'"),
            decline(2, "'X' (EnvironmentTemplate[0])")
        )
    );
}

#[test]
fn order_step_7_an_attachments_value_before_a_later_ones_missing_value() {
    // X (first attached) has a value below its minimum, found while the
    // values are computed; Y (second) misses P, found after them.
    let jt = format!(
        "{HEAD}steps:\n- name: S\n  dependencies: [{{environment: X, parameters: {{N: 1}}}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let x = env(
        "name: X, activation: ON_REQUEST, envParameterDefinitions: [{name: N, type: INT, \
         minValue: 5}]",
    );
    let y = env("name: Y, envParameterDefinitions: [{name: P, type: STRING}]");
    assert_eq!(
        err_text(&jt, &[&x, &y], &[]),
        "Model validation error: 2 validation errors for Submission\n\
         JobTemplate -> steps[0] -> dependencies[0] -> parameters -> N:\n\tParameter 'N': value \
         1 is less than minimum 5\nJobTemplate -> steps[0]:\n\tmust supply, in an \
         'environment:' dependency, every parameter without a default of each Environment its \
         Sessions enter (every attached Environment with activation ALWAYS, each one with \
         activation BY_DEFAULT that the Step does not decline, and each one with activation \
         ON_REQUEST that the Step requests; Template Schemas §3.2.1); missing: 'P' of \
         Environment 'Y' (EnvironmentTemplate[1])."
    );
}

#[test]
fn order_the_single_layer_rule_after_the_steps_values_and_before_step_8() {
    // In Step S: J's value below its minimum (step 7, at J's place), the
    // single-layer rule over W1 and W2 (step 7, after the Environments),
    // and H's requirements failing to resolve (step 8).
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, \
              EXPR, FEATURE_BUNDLE_1]\nname: J\n\
              parameterDefinitions: [{name: L, type: 'LIST[INT]', default: [1]}]\n\
              jobEnvironments:\n\
              - {name: W1, script: {actions: {onWrapEnvEnter: {command: x}, onWrapTaskRun: \
              {command: x}, onWrapEnvExit: {command: x}}}}\n\
              - {name: W2, enableIf: \"Job.Name == 'J'\", script: {actions: {onWrapEnvEnter: \
              {command: x}, onWrapTaskRun: {command: x}, onWrapEnvExit: {command: x}}}}\n\
              - {name: J, envParameterDefinitions: [{name: N, type: INT, minValue: 5}], variables: \
              {V: j}}\n\
              - {name: H, hostRequirements: {amounts: [{name: amount.custom, min: \
              \"{{Param.L[4]}}\"}]}, variables: {V: h}}\nsteps:\n\
              - name: S\n  dependencies: [{environment: J, parameters: {N: \"{{Param.L[0]}}\"}}]\n  \
              script: {actions: {onRun: {command: x}}}\n";
    assert_eq!(err_text(jt, &[], &[]), "Model validation error: 3 validation errors for JobTemplate\nsteps[0] -> dependencies[0] -> parameters -> N:\n\tParameter 'N': value 1 is less than minimum 5\nsteps[0]:\n\tits Sessions would enter 2 Environments that define wrap hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'W1', 'W2'. A Session may have at most one Environment defining wrap hooks (RFC 0008).\njobEnvironments[3] -> hostRequirements -> amounts[0] -> min:\n\tFormat string error: hostRequirements amount min: Index 4 out of bounds for list of length 1. Resolved for Step 'S'.\n  Param.L[4]\n  ~~~~~~~^~~");
}

#[test]
fn order_literal_defaults_of_edited_templates() {
    // Decode rejects a literal default that fails its definition, so edit
    // the decoded templates: the Job Environment J's and the attachment
    // X's N default to 1, below their minimum. Both are template-
    // validation-style checks, first, the Job Template's before the
    // attachment's, and before Step S's missing P (step 7).
    let mut jt = job_ok(&format!(
        "{HEAD}jobEnvironments:\n- {{name: J, envParameterDefinitions: [{{name: N, type: INT, \
         minValue: 5, default: 5}}, {{name: P, type: STRING}}], enableIf: \"Job.Name == 'J'\", \
         variables: {{V: j}}}}\n{}",
        "steps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n"
    ));
    let mut x = env_ok(&env(
        "name: X, envParameterDefinitions: [{name: N, type: INT, minValue: 5, default: 5}]",
    ));
    let one = env_ok(&env(
        "name: O, envParameterDefinitions: [{name: N, type: INT, default: 1}]",
    ))
    .environment
    .env_parameter_definitions
    .unwrap()[0]
        .default
        .clone();
    jt.job_environments.as_mut().unwrap()[0]
        .env_parameter_definitions
        .as_mut()
        .unwrap()[0]
        .default = one.clone();
    x.environment.env_parameter_definitions.as_mut().unwrap()[0].default = one;
    let values = job_params_with(&jt, &[], std::slice::from_ref(&x));
    let attached = [AttachedEnvironmentTemplate::new(&x)];
    let err = create_job_with_environment_templates(&jt, &attached, &values, &template_ctx(&jt))
        .expect_err("expected job creation to fail");
    assert_eq!(err.to_string(), "Model validation error: 3 validation errors for Submission\nJobTemplate -> jobEnvironments[0] -> envParameterDefinitions[0] -> default:\n\tParameter 'N': value 1 is less than minimum 5\nEnvironmentTemplate[0] -> environment -> envParameterDefinitions[0] -> default:\n\tParameter 'N': value 1 is less than minimum 5\nJobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every jobEnvironments entry with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7); missing: 'P' of Environment 'J'.");
}

// ── Round 2: the stop rule's drop, first-Step filing, stages, positions ──

#[test]
fn a_stopped_step_loses_the_undeclared_keys_found_before_the_stop() {
    // Job creation proper finds the undeclared key Q (step 5) before the
    // submission stage finds Missing (step 3), which stops S.
    let jt = format!(
        "{HEAD}parameterDefinitions: [{{name: B, type: BOOL, default: true}}]\n\
         jobEnvironments:\n- {{name: A, envParameterDefinitions: [{{name: P, type: STRING, \
         default: p}}], variables: {{V: a}}}}\nsteps:\n- name: S\n  dependencies: \
         [{{environment: A, enableIf: \"Param.B\", parameters: {{Q: 2}}}}, {{environment: \
         Missing}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    assert_eq!(err_text(&jt, &[], &[]), "Model validation error: 1 validation error for JobTemplate\nsteps[0] -> dependencies[1] -> environment:\n\tnames Environment 'Missing', which is not a jobEnvironments entry of the Job Template, and no Environment Template is attached. The entry is required; set 'required: false' if the Step can run without the Environment (Template Schemas §3.2.1 item 2).");
}

#[test]
fn a_step_4_error_stops_the_step() {
    // The entry for the ALWAYS A is disabled (step 4), so SE's failing
    // enableIf (step 6) is not reported.
    let jt = format!(
        "{HEAD}parameterDefinitions: [{{name: B, type: BOOL, default: false}}, {{name: L, \
         type: 'LIST[INT]', default: [1]}}]\njobEnvironments:\n- {{name: A, activation: ALWAYS, \
         variables: {{V: a}}}}\nsteps:\n- name: S\n  dependencies: [{{environment: A, enableIf: \
         \"Param.B\"}}]\n  stepEnvironments:\n  - {{name: SE, enableIf: \"Param.L[4] == 1\", \
         variables: {{V: s}}}}\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    assert_eq!(err_text(&jt, &[], &[]), "Model validation error: 1 validation error for JobTemplate\nsteps[0] -> dependencies[0] -> enableIf:\n\tis false, so Step 'S' would decline Environment 'A', whose activation is ALWAYS: no Step can leave out an ALWAYS Environment (Template Schemas §3.2 item 3.2).");
}

/// Steps S0, S1 and S2; only S1 has `s1_extra` (YAML lines for the Step).
fn three_steps(s1_extra: &str) -> String {
    format!(
        "steps:\n- name: S0\n  script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: S1\n{s1_extra}  script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: S2\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    )
}

#[test]
fn an_enable_if_error_naming_two_steps_is_filed_under_the_first() {
    // J's enableIf fails for S0 and S2, one error filed under S0; S1, which
    // supplies S, has a failing Step Environment enableIf.
    let jt = format!(
        "{HEAD}parameterDefinitions: [{{name: L, type: 'LIST[INT]', default: [1]}}]\n\
         jobEnvironments:\n- {{name: J, {FAILING}, variables: {{V: j}}}}\n{}",
        three_steps(
            "  dependencies: [{environment: J, parameters: {S: '1'}}]\n  stepEnvironments:\n  \
             - {name: SE, enableIf: \"Param.L[4] == 1\", variables: {V: s}}\n"
        )
    );
    assert_eq!(err_text(&jt, &[], &[]), "Model validation error: 2 validation errors for JobTemplate\njobEnvironments[0] -> enableIf:\n\tcould not be evaluated: Cannot convert 'abc' to int. Resolved for Steps 'S0' and 'S2'.\n  int(Env.Param.S) > 0\n  ^~~~~~~~~~~~~~~~\nsteps[1] -> stepEnvironments[0] -> enableIf:\n\tcould not be evaluated: Index 4 out of bounds for list of length 1\n  Param.L[4] == 1\n  ~~~~~~~^~~");
}

#[test]
fn an_always_error_naming_two_steps_is_filed_under_the_first() {
    // A's enableIf is false for S0 and S2, which take P's default; S1
    // supplies 'yes' and has a failing Step Environment enableIf.
    let jt = format!(
        "{HEAD}parameterDefinitions: [{{name: L, type: 'LIST[INT]', default: [1]}}]\n\
         jobEnvironments:\n- {{name: A, activation: ALWAYS, envParameterDefinitions: [{{name: P, \
         type: STRING, default: 'no'}}], enableIf: \"Env.Param.P == 'yes'\", variables: {{V: \
         a}}}}\n{}",
        three_steps(
            "  dependencies: [{environment: A, parameters: {P: 'yes'}}]\n  stepEnvironments:\n  \
             - {name: SE, enableIf: \"Param.L[4] == 1\", variables: {V: s}}\n"
        )
    );
    assert_eq!(err_text(&jt, &[], &[]), "Model validation error: 2 validation errors for JobTemplate\njobEnvironments[0] -> enableIf:\n\tEnvironment 'A' has activation ALWAYS, so it cannot be disabled, but its enableIf is false for Steps 'S0' and 'S2' (Template Schemas §4 item 4).\nsteps[1] -> stepEnvironments[0] -> enableIf:\n\tcould not be evaluated: Index 4 out of bounds for list of length 1\n  Param.L[4] == 1\n  ~~~~~~~^~~");
}

#[test]
fn order_the_validation_and_step_2_stages() {
    // S0's entry enableIf fails (step 2); the attachment X's literal
    // default, edited below its minimum, fails its definition
    // (template-validation style), which comes first. S1's entry value
    // reads its `let` binding, evaluated once with POSIX semantics.
    let jt = job_ok(&format!(
        "{HEAD}parameterDefinitions: [{{name: L, type: 'LIST[INT]', default: [1]}}, {{name: Root, \
         type: STRING, default: /a/b}}, {{name: Base, type: STRING, default: /a}}]\n\
         jobEnvironments:\n- {{name: R, activation: ON_REQUEST, envParameterDefinitions: \
         [{{name: P, type: STRING}}], variables: {{V: r}}}}\nsteps:\n\
         - name: S0\n  dependencies: [{{environment: R, enableIf: \"Param.L[4] == 1\"}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: S1\n  let: [\"r = string(path(Param.Root).relative_to(Param.Base))\"]\n  dependencies: \
         [{{environment: R, parameters: {{P: \"{{{{r}}}}\"}}}}]\n  script: {{actions: {{onRun: \
         {{command: x}}}}}}\n"
    ));
    let mut x = env_ok(&env(
        "name: X, envParameterDefinitions: [{name: N, type: INT, minValue: 5, default: 5}]",
    ));
    x.environment.env_parameter_definitions.as_mut().unwrap()[0].default = env_ok(&env(
        "name: O, envParameterDefinitions: [{name: N, type: INT, default: 1}]",
    ))
    .environment
    .env_parameter_definitions
    .unwrap()[0]
        .default
        .clone();
    let create = |jt: &openjd_model::template::JobTemplate, params: &[(&str, &str)]| {
        let values = job_params_with(jt, params, std::slice::from_ref(&x));
        create_job_with_environment_templates(
            jt,
            &[AttachedEnvironmentTemplate::new(&x)],
            &values,
            &template_ctx(jt),
        )
        .expect_err("expected job creation to fail")
        .to_string()
    };
    assert_eq!(create(&jt, &[]), "Model validation error: 2 validation errors for Submission\nEnvironmentTemplate[0] -> environment -> envParameterDefinitions[0] -> default:\n\tParameter 'N': value 1 is less than minimum 5\nJobTemplate -> steps[0] -> dependencies[0] -> enableIf:\n\tcould not be evaluated: Index 4 out of bounds for list of length 1\n  Param.L[4] == 1\n  ~~~~~~~^~~");
    // When S1's binding fails, S1 is uninstantiable and the attachments
    // are not applied (`create_job_with_environment_templates`), so only
    // the Job Template's own checks are reported, in Step order.
    assert_eq!(create(&jt, &[("Base", "/c")]), "Model validation error: 2 validation errors for Submission\nJobTemplate -> steps[1] -> let[0]:\n\tInvalid expression in let binding 'r': relative_to failed: '/a/b' is not relative to '/c'\n  r = string(path(Param.Root).relative_to(Param.Base))\n             ~~~~~~~~~~~~~~~~~^~~~~~~~~~~~~~~~~~~~~~~\nJobTemplate -> steps[0] -> dependencies[0] -> enableIf:\n\tcould not be evaluated: Index 4 out of bounds for list of length 1\n  Param.L[4] == 1\n  ~~~~~~~^~~");
}

#[test]
fn order_step_5_before_an_earlier_environments_step_6_and_step_6_before_step_7() {
    // In S: the Job Environment J's undeclared key (step 5, at J's later
    // place), the attachment X's failed enableIf (step 6, at the first
    // place), and Y's missing P (step 7, at the second); and the Step
    // Environment SE's failed enableIf (step 6, after every Environment).
    let jt = format!(
        "{HEAD}parameterDefinitions: [{{name: B, type: BOOL, default: true}}, {{name: L, type: \
         'LIST[INT]', default: [1]}}]\njobEnvironments:\n- {{name: J, envParameterDefinitions: \
         [{{name: P, type: STRING, default: p}}], variables: {{V: j}}}}\nsteps:\n- name: S\n  \
         dependencies: [{{environment: J, enableIf: \"Param.B\", parameters: {{Q: 1}}}}]\n  \
         stepEnvironments:\n  - {{name: SE, enableIf: \"Param.L[4] == 1\", variables: {{V: \
         s}}}}\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let x = env(&format!("name: X, {FAILING}"));
    let y = env("name: Y, envParameterDefinitions: [{name: P, type: STRING}]");
    assert_eq!(err_text(&jt, &[&x, &y], &[]), "Model validation error: 4 validation errors for Submission\nJobTemplate -> steps[0] -> dependencies[0] -> parameters -> Q:\n\tEnvironment 'J' declares no parameter 'Q'. Declared: P.\nEnvironmentTemplate[0] -> environment -> enableIf:\n\tcould not be evaluated: Cannot convert 'abc' to int. Resolved for Step 'S'.\n  int(Env.Param.S) > 0\n  ^~~~~~~~~~~~~~~~\nJobTemplate -> steps[0] -> stepEnvironments[0] -> enableIf:\n\tcould not be evaluated: Index 4 out of bounds for list of length 1\n  Param.L[4] == 1\n  ~~~~~~~^~~\nJobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every attached Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1); missing: 'P' of Environment 'Y' (EnvironmentTemplate[1]).");
}

#[test]
fn order_step_2_by_entry_index_in_an_edited_template() {
    // Entries 1 and 3 have enableIfs that fail; entry 2 duplicates entry 0
    // (possible only in a template edited after decoding).
    let mut jt = job_ok(&format!(
        "{HEAD}parameterDefinitions: [{{name: L, type: 'LIST[INT]', default: [1]}}]\n\
         jobEnvironments:\n- {{name: E, variables: {{V: e}}}}\n- {{name: F, variables: {{V: \
         f}}}}\n- {{name: G, variables: {{V: g}}}}\nsteps:\n- name: S\n  dependencies: \
         [{{environment: E}}, {{environment: F, enableIf: \"Param.L[4] == 1\"}}, \
         {{environment: G, enableIf: \"Param.L[4] == 1\"}}]\n  script: {{actions: {{onRun: \
         {{command: x}}}}}}\n"
    ));
    let deps = jt.steps[0].dependencies.as_mut().unwrap();
    let e = deps[0].clone();
    deps.insert(2, e);
    let values = job_params_with(&jt, &[], &[]);
    let err = openjd_model::create_job(&jt, &values, &template_ctx(&jt))
        .expect_err("expected job creation to fail");
    assert_eq!(err.to_string(), "Model validation error: 3 validation errors for JobTemplate\nsteps[0] -> dependencies[1] -> enableIf:\n\tcould not be evaluated: Index 4 out of bounds for list of length 1\n  Param.L[4] == 1\n  ~~~~~~~^~~\nsteps[0] -> dependencies[2] -> environment:\n\tduplicate environment dependency 'E'.\nsteps[0] -> dependencies[3] -> enableIf:\n\tcould not be evaluated: Index 4 out of bounds for list of length 1\n  Param.L[4] == 1\n  ~~~~~~~^~~");
}

#[test]
fn order_step_3_a_disabled_entry_before_a_later_enabled_one() {
    // Entry 1 is disabled and names the ambiguous X; entry 2, enabled, names
    // Missing. Disabled entries are resolved first; entry 1's error is first
    // either way, entry 2's after it.
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: E, variables: {{V: e}}}}\nsteps:\n- name: S\n  \
         dependencies: [{{environment: E}}, {{environment: X, enableIf: false}}, {{environment: \
         Missing}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let x = env("name: X");
    assert_eq!(err_text(&jt, &[&x, &x], &[]), "Model validation error: 2 validation errors for Submission\nJobTemplate -> steps[0] -> dependencies[1] -> environment:\n\tnames Environment 'X', which 2 attached Environment Templates define (EnvironmentTemplate[0], EnvironmentTemplate[1]); an entry must match exactly one (Template Schemas §3.2.1 item 3).\nJobTemplate -> steps[0] -> dependencies[2] -> environment:\n\tnames Environment 'Missing', which is not a jobEnvironments entry of the Job Template, and none of the attached Environment Templates (EnvironmentTemplate[0], EnvironmentTemplate[1]) defines it. The entry is required; set 'required: false' if the Step can run without the Environment (Template Schemas §3.2.1 item 2).");
}

#[test]
fn order_step_7_attachments_constraint_7_by_its_first_environment() {
    // X0 needs nothing, X1 has a value below its minimum, X2 misses P: X1's
    // value first. Then with X0 missing P too: the constraint 7 error, which
    // names X0 first, comes before X1's value.
    let jt = format!(
        "{HEAD}steps:\n- name: S\n  dependencies: [{{environment: X1, parameters: {{N: 1}}}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let x0 = env("name: X0");
    let x1 = env(
        "name: X1, activation: ON_REQUEST, envParameterDefinitions: [{name: N, type: INT, \
         minValue: 5}]",
    );
    let x2 = env("name: X2, envParameterDefinitions: [{name: P, type: STRING}]");
    let x0p = env("name: X0, envParameterDefinitions: [{name: P, type: STRING}]");
    assert_eq!(err_text(&jt, &[&x0, &x1, &x2], &[]), "Model validation error: 2 validation errors for Submission\nJobTemplate -> steps[0] -> dependencies[0] -> parameters -> N:\n\tParameter 'N': value 1 is less than minimum 5\nJobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every attached Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1); missing: 'P' of Environment 'X2' (EnvironmentTemplate[2]).");
    assert_eq!(err_text(&jt, &[&x0p, &x1, &x2], &[]), "Model validation error: 2 validation errors for Submission\nJobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every attached Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1); missing: 'P' of Environment 'X0' (EnvironmentTemplate[0]); 'P' of Environment 'X2' (EnvironmentTemplate[2]).\nJobTemplate -> steps[0] -> dependencies[0] -> parameters -> N:\n\tParameter 'N': value 1 is less than minimum 5");
}

#[test]
fn a_stopped_step_does_not_resolve_an_environments_host_requirements() {
    // HR's requirements fail to resolve for every Step that enters it. S0
    // is stopped by Missing, so the error names S1 alone.
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR, \
              FEATURE_BUNDLE_1]\nname: J\n\
              parameterDefinitions: [{name: L, type: 'LIST[INT]', default: [1]}]\n\
              jobEnvironments:\n- {name: HR, hostRequirements: {amounts: [{name: amount.custom, \
              min: \"{{Param.L[4]}}\"}]}, variables: {V: h}}\nsteps:\n\
              - name: S0\n  dependencies: [{environment: Missing}]\n  script: {actions: {onRun: \
              {command: x}}}\n- name: S1\n  script: {actions: {onRun: {command: x}}}\n";
    assert_eq!(err_text(jt, &[], &[]), "Model validation error: 2 validation errors for JobTemplate\nsteps[0] -> dependencies[0] -> environment:\n\tnames Environment 'Missing', which is not a jobEnvironments entry of the Job Template, and no Environment Template is attached. The entry is required; set 'required: false' if the Step can run without the Environment (Template Schemas §3.2.1 item 2).\njobEnvironments[0] -> hostRequirements -> amounts[0] -> min:\n\tFormat string error: hostRequirements amount min: Index 4 out of bounds for list of length 1. Resolved for Step 'S1'.\n  Param.L[4]\n  ~~~~~~~^~~");
}

#[test]
fn an_uninstantiable_step_without_env_params_has_the_shape_the_inputs_give() {
    // No ENV_PARAMS: a failed step-level let binding is reported in the
    // shape the inputs give, as with ENV_PARAMS: for Submission, with the
    // document's path, when an Environment Template is attached (before,
    // for JobTemplate), and for JobTemplate, at its own paths, when none
    // is.
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [EXPR]\nname: J\n\
              parameterDefinitions: [{name: L, type: 'LIST[INT]', default: [1]}]\nsteps:\n\
              - name: S\n  let: [\"a = Param.L[4]\"]\n  script: {actions: {onRun: {command: \
              x}}}\n";
    let x = "specificationVersion: environment-2023-09\nenvironment: {name: X, variables: {V: \
             x}}\n";
    assert_eq!(err_text(jt, &[x], &[]), "Model validation error: 1 validation error for Submission\nJobTemplate -> steps[0] -> let[0]:\n\tInvalid expression in let binding 'a': Index 4 out of bounds for list of length 1\n  a = Param.L[4]\n      ~~~~~~~^~~");
    assert_eq!(err_text(jt, &[], &[]), "Model validation error: 1 validation error for JobTemplate\nsteps[0] -> let[0]:\n\tInvalid expression in let binding 'a': Index 4 out of bounds for list of length 1\n  a = Param.L[4]\n      ~~~~~~~^~~");
}

// ── Round 3 ──────────────────────────────────────────────────────────

#[test]
fn a_job_template_without_env_params_reports_its_attachments_errors_in_step_order() {
    // An ENV_PARAMS attachment A, whose enableIf fails and whose Y has no
    // default, attached to a Job Template without ENV_PARAMS: the errors
    // are in §7.4.1 order, the enableIf (step 6) before the constraint 7
    // error (step 7), which was found first. Before, they were in the order
    // they were found.
    let jt = "specificationVersion: jobtemplate-2023-09\nname: J\nsteps:\n- name: S\n  \
              script: {actions: {onRun: {command: x}}}\n";
    let a = env(
        "name: A, envParameterDefinitions: [{name: X, type: INT, default: 1}, {name: Y, type: \
         STRING}], enableIf: \"Env.Param.X / (Env.Param.X - Env.Param.X) > 0\"",
    );
    assert_eq!(err_text(jt, &[&a], &[]), "Model validation error: 2 validation errors for Submission\nEnvironmentTemplate[0] -> environment -> enableIf:\n\tcould not be evaluated: Division by zero. Resolved for Step 'S'.\n  Env.Param.X / (Env.Param.X - Env.Param.X) > 0\n  ~~~~~~~~~~~~^~~~~~~~~~~~~~~~~~~~~~~~~~~~~\nJobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every attached Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1); missing: 'Y' of Environment 'A' (EnvironmentTemplate[0]) (its own enableIf could not be evaluated for this Step, so the Environment is checked as if it is not disabled).");
}

#[test]
fn the_caller_limits_wait_for_the_job_environments_and_the_submission_stage() {
    // With ENV_PARAMS the Job Environments are decided in the submission
    // stage, so the caller limits are checked after it: the ALWAYS E's
    // enableIf error, and an unresolved entry's, are reported, not the task
    // count.
    let src = |deps: &str| {
        format!(
            "{HEAD}parameterDefinitions: [{{name: N, type: INT, default: 3}}]\n\
             jobEnvironments:\n- {{name: E, activation: ALWAYS, enableIf: \"Param.N < 2\", \
             variables: {{V: e}}}}\nsteps:\n- name: S\n{deps}  parameterSpace:\n    \
             taskParameterDefinitions: [{{name: T, type: INT, range: \"1-{{{{Param.N}}}}\"}}]\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    let limited = |src: &str| {
        let jt = job_ok(src);
        let values = job_params_with(&jt, &[], &[]);
        let ctx = template_ctx(&jt).with_caller_limits(openjd_model::CallerLimits {
            max_task_count: Some(2),
            ..Default::default()
        });
        openjd_model::create_job(&jt, &values, &ctx)
            .expect_err("expected an error")
            .to_string()
    };
    assert_eq!(limited(&src("")), "Model validation error: 1 validation error for JobTemplate\njobEnvironments[0] -> enableIf:\n\tEnvironment 'E' has activation ALWAYS, so it cannot be disabled, but its enableIf is false for Step 'S' (Template Schemas §4 item 4).");
    let missing = src("  dependencies: [{environment: Missing}]\n")
        .replace("activation: ALWAYS, enableIf: \"Param.N < 2\", ", "");
    assert_eq!(limited(&missing), "Model validation error: 1 validation error for JobTemplate\nsteps[0] -> dependencies[0] -> environment:\n\tnames Environment 'Missing', which is not a jobEnvironments entry of the Job Template, and no Environment Template is attached. The entry is required; set 'required: false' if the Step can run without the Environment (Template Schemas §3.2.1 item 2).");
    let ok = src("").replace("activation: ALWAYS, enableIf: \"Param.N < 2\", ", "");
    assert_eq!(limited(&ok), "Model validation error: 1 validation error for Template\nTemplate: Total task count (3) exceeds caller limit of 2.");
}

#[test]
fn the_script_and_environment_size_limits_wait_for_the_errors() {
    let jt = job_ok(&format!(
        "{HEAD}parameterDefinitions: [{{name: L, type: 'LIST[INT]', default: [1]}}]\n\
         jobEnvironments:\n- {{name: E, variables: {{V: e}}}}\nsteps:\n- name: S\n  \
         dependencies: [{{environment: E, enableIf: \"Param.L[4] == 1\"}}]\n  script: \
         {{actions: {{onRun: {{command: x}}}}}}\n"
    ));
    let values = job_params_with(&jt, &[], &[]);
    for limits in [
        openjd_model::CallerLimits {
            max_step_script_size: Some(1),
            ..Default::default()
        },
        openjd_model::CallerLimits {
            max_environment_size: Some(1),
            ..Default::default()
        },
    ] {
        let ctx = template_ctx(&jt).with_caller_limits(limits);
        let err = openjd_model::create_job(&jt, &values, &ctx).expect_err("expected an error");
        assert_eq!(
            err.to_string(),
            "Model validation error: 1 validation error for JobTemplate\n\
             steps[0] -> dependencies[0] -> enableIf:\n\tcould not be evaluated: Index 4 out of \
             bounds for list of length 1\n  Param.L[4] == 1\n  ~~~~~~~^~~"
        );
    }
}

#[test]
fn a_step_environments_enable_if_error_is_filed_under_its_own_step() {
    // S0 is stopped by its entry's failed enableIf; S1's Step Environment's
    // enableIf fails too, and is reported.
    let jt = format!(
        "{HEAD}parameterDefinitions: [{{name: L, type: 'LIST[INT]', default: [1]}}]\n\
         jobEnvironments:\n- {{name: E, variables: {{V: e}}}}\nsteps:\n- name: S0\n  \
         dependencies: [{{environment: E, enableIf: \"Param.L[4] == 1\"}}]\n  script: \
         {{actions: {{onRun: {{command: x}}}}}}\n- name: S1\n  stepEnvironments:\n  - {{name: \
         SE, enableIf: \"Param.L[4] == 1\", variables: {{V: s}}}}\n  script: {{actions: \
         {{onRun: {{command: x}}}}}}\n"
    );
    assert_eq!(err_text(&jt, &[], &[]), "Model validation error: 2 validation errors for JobTemplate\nsteps[0] -> dependencies[0] -> enableIf:\n\tcould not be evaluated: Index 4 out of bounds for list of length 1\n  Param.L[4] == 1\n  ~~~~~~~^~~\nsteps[1] -> stepEnvironments[0] -> enableIf:\n\tcould not be evaluated: Index 4 out of bounds for list of length 1\n  Param.L[4] == 1\n  ~~~~~~~^~~");
}

#[test]
fn a_stopped_step_keeps_the_later_steps_environments_and_values_aligned() {
    // S0 is stopped by Missing. S1 does not enter J (its enableIf is
    // false for S1) and enters G, whose requirement conflicts with S1's.
    let jt = format!(
        "{HEAD}jobEnvironments:\n\
         - {{name: J, envParameterDefinitions: [{{name: U, type: STRING, default: 'no'}}], \
         enableIf: \"Env.Param.U == 'yes'\", hostRequirements: {{amounts: [{{name: amount.j, \
         min: 4}}]}}, variables: {{V: j}}}}\n\
         - {{name: G, envParameterDefinitions: [{{name: M, type: INT, default: 1}}], \
         hostRequirements: {{amounts: [{{name: amount.g, min: 4}}]}}, variables: {{V: g}}}}\n\
         steps:\n- name: S0\n  dependencies: [{{environment: Missing}}, {{environment: J, \
         parameters: {{U: 'yes'}}}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: S1\n  hostRequirements: {{amounts: [{{name: amount.j, max: 2}}, {{name: \
         amount.g, max: 2}}]}}\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    assert_eq!(err_text(&jt, &[], &[]), "Model validation error: 2 validation errors for JobTemplate\nsteps[0] -> dependencies[0] -> environment:\n\tnames Environment 'Missing', which is not a jobEnvironments entry of the Job Template, and no Environment Template is attached. The entry is required; set 'required: false' if the Step can run without the Environment (Template Schemas §3.2.1 item 2).\nsteps[1]:\n\tStep 'S1': no host can satisfy the combined host requirements for amount 'amount.g': jobEnvironments[1] -> hostRequirements (Environment 'G') requires min 4, and steps[1] -> hostRequirements requires max 2. Template Schemas §3.3.3.");
    // The same with G attached.
    let jt2 = jt.replace(
        "\n- {name: G, envParameterDefinitions: [{name: M, type: INT, default: 1}], \
         hostRequirements: {amounts: [{name: amount.g, min: 4}]}, variables: {V: g}}",
        "",
    );
    let g = env(
        "name: G, envParameterDefinitions: [{name: M, type: INT, default: 1}], \
         hostRequirements: {amounts: [{name: amount.g, min: 4}]}",
    );
    assert_eq!(err_text(&jt2, &[&g], &[]), "Model validation error: 2 validation errors for Submission\nJobTemplate -> steps[0] -> dependencies[0] -> environment:\n\tnames Environment 'Missing', which is not a jobEnvironments entry of the Job Template, and none of the attached Environment Templates (EnvironmentTemplate[0]) defines it. The entry is required; set 'required: false' if the Step can run without the Environment (Template Schemas §3.2.1 item 2).\nJobTemplate -> steps[1]:\n\tStep 'S1': no host can satisfy the combined host requirements for amount 'amount.g': EnvironmentTemplate[0] -> environment -> hostRequirements (Environment 'G') requires min 4, and JobTemplate -> steps[1] -> hostRequirements requires max 2. Template Schemas §3.3.3.");
    // S1 enters K0, which supersedes K, whose requirement would conflict
    // with S1's.
    let jt3 = format!(
        "{HEAD}jobEnvironments:\n- {{name: K0, exclusiveGroup: G, variables: {{V: a}}}}\n\
         - {{name: K, exclusiveGroup: G, hostRequirements: {{amounts: [{{name: amount.k, min: \
         4}}]}}, variables: {{V: k}}}}\nsteps:\n- name: S0\n  dependencies: [{{environment: \
         Missing}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n- name: S1\n  \
         hostRequirements: {{amounts: [{{name: amount.k, max: 2}}]}}\n  script: {{actions: \
         {{onRun: {{command: x}}}}}}\n"
    );
    assert_eq!(err_text(&jt3, &[], &[]), "Model validation error: 1 validation error for JobTemplate\nsteps[0] -> dependencies[0] -> environment:\n\tnames Environment 'Missing', which is not a jobEnvironments entry of the Job Template, and no Environment Template is attached. The entry is required; set 'required: false' if the Step can run without the Environment (Template Schemas §3.2.1 item 2).");
}

#[test]
fn an_unreachable_steps_environment_error_is_filed_at_its_attachment() {
    // A, X1 and X are attached. X is named like S's Step Environment and
    // has P without a default (step 7, at X's place); X1 has a value below
    // its minimum (step 7, at X1's place); and J's enableIf fails (step 6).
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: J, {FAILING}, variables: {{V: j}}}}\nsteps:\n- \
         name: S\n  dependencies: [{{environment: X1, parameters: {{N: 1}}}}]\n  \
         stepEnvironments:\n  - {{name: X, variables: {{V: s}}}}\n  script: {{actions: {{onRun: \
         {{command: x}}}}}}\n"
    );
    let a = env("name: A");
    let x1 = env(
        "name: X1, activation: ON_REQUEST, envParameterDefinitions: [{name: N, type: INT, \
         minValue: 5}]",
    );
    let x = env("name: X, envParameterDefinitions: [{name: P, type: STRING}]");
    assert_eq!(err_text(&jt, &[&a, &x1, &x], &[]), "Model validation error: 3 validation errors for Submission\nJobTemplate -> jobEnvironments[0] -> enableIf:\n\tcould not be evaluated: Cannot convert 'abc' to int. Resolved for Step 'S'.\n  int(Env.Param.S) > 0\n  ^~~~~~~~~~~~~~~~\nJobTemplate -> steps[0] -> dependencies[0] -> parameters -> N:\n\tParameter 'N': value 1 is less than minimum 5\nJobTemplate -> steps[0]:\n\tcannot supply the parameters without a default of Environment 'X' (EnvironmentTemplate[2]): P. It has the same name as a stepEnvironments entry of this Step, so no 'environment:' dependency of this Step can reach it (Template Schemas §3.2.1 item 5), and its activation is BY_DEFAULT, so this Step's Sessions enter it.");
}

#[test]
fn the_job_environments_constraint_7_error_is_filed_at_its_first_environment() {
    // J0 misses P; J1 misses Q and has R below its minimum. The constraint
    // 7 error names J0 first, so it precedes J1's value.
    let jt = format!(
        "{HEAD}parameterDefinitions: [{{name: B, type: BOOL, default: true}}]\n\
         jobEnvironments:\n\
         - {{name: J0, envParameterDefinitions: [{{name: P, type: STRING}}], enableIf: \
         \"Param.B\", variables: {{V: a}}}}\n\
         - {{name: J1, envParameterDefinitions: [{{name: Q, type: STRING}}, {{name: R, type: \
         INT, minValue: 5}}], enableIf: \"Param.B\", variables: {{V: b}}}}\nsteps:\n- name: S\n  \
         dependencies: [{{environment: J1, enableIf: \"Param.B\", parameters: {{R: 1}}}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    assert_eq!(err_text(&jt, &[], &[]), "Model validation error: 2 validation errors for JobTemplate\nsteps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every jobEnvironments entry with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7); missing: 'P' of Environment 'J0'; 'Q' of Environment 'J1'.\nsteps[0] -> dependencies[0] -> parameters -> R:\n\tParameter 'R': value 1 is less than minimum 5");
}

#[test]
fn the_single_layer_and_conflict_errors_are_filed_under_their_step() {
    // S0 has two wrapping Step Environments (the second's enableIf is an
    // expression) and a conflict with E's requirement; S1 misses P of the
    // ON_REQUEST Q it requests, and E1's requirement fails to resolve for
    // it. S0's errors, then S1's.
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, \
              EXPR, FEATURE_BUNDLE_1]\nname: J\n\
              parameterDefinitions: [{name: L, type: 'LIST[INT]', default: [1]}, {name: B, \
              type: BOOL, default: true}]\n\
              jobEnvironments:\n\
              - {name: E, hostRequirements: {amounts: [{name: amount.custom, min: 4}]}, \
              variables: {V: e}}\n\
              - {name: Q, activation: ON_REQUEST, envParameterDefinitions: [{name: P, type: \
              STRING}], variables: {V: q}}\n\
              - {name: E1, activation: ON_REQUEST, hostRequirements: {amounts: [{name: \
              amount.other, min: \"{{Param.L[4]}}\"}]}, variables: {V: e1}}\nsteps:\n\
              - name: S0\n  hostRequirements: {amounts: [{name: amount.custom, max: 2}]}\n  \
              stepEnvironments:\n  - {name: W1, script: {actions: {onWrapEnvEnter: {command: x}, \
              onWrapTaskRun: {command: x}, onWrapEnvExit: {command: x}}}}\n  - {name: W2, \
              enableIf: \"Job.Name == 'J'\", script: {actions: {onWrapEnvEnter: {command: x}, \
              onWrapTaskRun: {command: x}, onWrapEnvExit: {command: x}}}}\n  script: {actions: \
              {onRun: {command: x}}}\n\
              - name: S1\n  dependencies: [{environment: Q, enableIf: \"Param.B\"}, \
              {environment: E1}]\n  hostRequirements: {amounts: [{name: amount.custom, max: \
              9}]}\n  script: {actions: {onRun: {command: x}}}\n";
    assert_eq!(err_text(jt, &[], &[]), "Model validation error: 4 validation errors for JobTemplate\nsteps[0]:\n\tits Sessions would enter 2 Environments that define wrap hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'W1' (stepEnvironments), 'W2' (stepEnvironments). A Session may have at most one Environment defining wrap hooks (RFC 0008).\nsteps[1]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every jobEnvironments entry with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7); missing: 'P' of Environment 'Q'.\nsteps[0]:\n\tStep 'S0': no host can satisfy the combined host requirements for amount 'amount.custom': jobEnvironments[0] -> hostRequirements (Environment 'E') requires min 4, and steps[0] -> hostRequirements requires max 2. Template Schemas §3.3.3.\njobEnvironments[2] -> hostRequirements -> amounts[0] -> min:\n\tFormat string error: hostRequirements amount min: Index 4 out of bounds for list of length 1. Resolved for Step 'S1'.\n  Param.L[4]\n  ~~~~~~~^~~");
}

#[test]
fn the_unreachable_attachments_errors_follow_the_attachment_order() {
    // Both attachments are named E, like a Job Environment, and each has a
    // parameter without a default: each gets two errors (named like a Job
    // Environment; defined twice), the first attachment's first.
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: E, activation: ON_REQUEST, variables: {{V: \
         e}}}}\n{}",
        "steps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n"
    );
    let e0 = env("name: E, envParameterDefinitions: [{name: X, type: INT}]");
    let e1 = env("name: E, envParameterDefinitions: [{name: Y, type: INT}]");
    assert_eq!(err_text(&jt, &[&e0, &e1], &[]), "Model validation error: 4 validation errors for Submission\nEnvironmentTemplate[0] -> environment -> envParameterDefinitions:\n\tEnvironment 'E' has the same name as a jobEnvironments entry of the Job Template, so no 'environment:' dependency can reach it (Template Schemas §3.2.1 item 4), and it has parameters without a default that no Step can supply: X. Its activation is BY_DEFAULT, so every Session enters it, and the submission is rejected (§3.2.1).\nEnvironmentTemplate[0] -> environment -> envParameterDefinitions:\n\tEnvironment 'E' is defined by 2 attached Environment Templates (EnvironmentTemplate[0], EnvironmentTemplate[1]), so no 'environment:' dependency can reach it (Template Schemas §3.2.1 item 3), and it has parameters without a default that no Step can supply: X. Its activation is BY_DEFAULT, so every Session enters it, and the submission is rejected (§3.2.1).\nEnvironmentTemplate[1] -> environment -> envParameterDefinitions:\n\tEnvironment 'E' has the same name as a jobEnvironments entry of the Job Template, so no 'environment:' dependency can reach it (Template Schemas §3.2.1 item 4), and it has parameters without a default that no Step can supply: Y. Its activation is BY_DEFAULT, so every Session enters it, and the submission is rejected (§3.2.1).\nEnvironmentTemplate[1] -> environment -> envParameterDefinitions:\n\tEnvironment 'E' is defined by 2 attached Environment Templates (EnvironmentTemplate[0], EnvironmentTemplate[1]), so no 'environment:' dependency can reach it (Template Schemas §3.2.1 item 3), and it has parameters without a default that no Step can supply: Y. Its activation is BY_DEFAULT, so every Session enters it, and the submission is rejected (§3.2.1).");
}

// ── Round 4 ──────────────────────────────────────────────────────────

/// A Job Template with the Job Environments R (P INT, at least 5) and F
/// (Q, with a default), the Job Parameters N (1), T (true) and B
/// (false), and Steps S0 (`s0`, YAML lines) and S1 (`s1`). An entry
/// `{environment: R, parameters: {P: "{{Param.N}}"}}` fails at §7.4.1
/// step 7.
fn s0_s1(s0: &str, s1: &str) -> String {
    format!(
        "{HEAD}parameterDefinitions: [{{name: N, type: INT, default: 1}}, {{name: T, type: BOOL, \
         default: true}}, {{name: B, type: BOOL, default: false}}]\njobEnvironments:\n\
         - {{name: R, activation: ON_REQUEST, envParameterDefinitions: [{{name: P, type: INT, \
         minValue: 5}}], variables: {{V: r}}}}\n- {{name: F, activation: ON_REQUEST, \
         envParameterDefinitions: [{{name: Q, type: INT, default: 1}}], variables: {{V: f}}}}\n\
         - {{name: A, activation: ALWAYS, variables: {{V: a}}}}\nsteps:\n- name: S0\n{s0}  script: \
         {{actions: {{onRun: {{command: x}}}}}}\n- name: S1\n{s1}  script: {{actions: {{onRun: \
         {{command: x}}}}}}\n"
    )
}

const R_BAD: &str = "{environment: R, parameters: {P: \"{{Param.N}}\"}}";

#[test]
fn a_later_steps_step_2_to_4_error_stops_that_step_not_the_first() {
    // S0's only error is its R value (step 7). S1 has an error from one of
    // steps 1–4 and its own R value: S1 is stopped, S0 is not.
    let s0 = format!("  dependencies: [{R_BAD}]\n");
    // Step 4: S1's entry for the ALWAYS A is disabled.
    let jt = s0_s1(
        &s0,
        &format!("  dependencies: [{{environment: A, enableIf: \"Param.B\"}}, {R_BAD}]\n"),
    );
    assert_eq!(err_text(&jt, &[], &[]), "Model validation error: 2 validation errors for JobTemplate\nsteps[1] -> dependencies[0] -> enableIf:\n\tis false, so Step 'S1' would decline Environment 'A', whose activation is ALWAYS: no Step can leave out an ALWAYS Environment (Template Schemas §3.2 item 3.2).\nsteps[0] -> dependencies[0] -> parameters -> P:\n\tParameter 'P': value 1 is less than minimum 5");
    // Step 3: S1's disabled entry names the ambiguous X.
    let jt = s0_s1(
        &s0,
        &format!("  dependencies: [{{environment: X, enableIf: \"Param.B\"}}, {R_BAD}]\n"),
    );
    let x = env("name: X");
    assert_eq!(err_text(&jt, &[&x, &x], &[]), "Model validation error: 2 validation errors for Submission\nJobTemplate -> steps[1] -> dependencies[0] -> environment:\n\tnames Environment 'X', which 2 attached Environment Templates define (EnvironmentTemplate[0], EnvironmentTemplate[1]); an entry must match exactly one (Template Schemas §3.2.1 item 3).\nJobTemplate -> steps[0] -> dependencies[0] -> parameters -> P:\n\tParameter 'P': value 1 is less than minimum 5");
    // Step 2: S1 has a duplicate entry (a template edited after decoding).
    let mut jt = job_ok(&s0_s1(
        &s0,
        &format!("  dependencies: [{R_BAD}, {{environment: F}}]\n"),
    ));
    let deps = jt.steps[1].dependencies.as_mut().unwrap();
    let f = deps[1].clone();
    deps.push(f);
    let values = job_params_with(&jt, &[], &[]);
    let err = openjd_model::create_job(&jt, &values, &template_ctx(&jt))
        .expect_err("expected job creation to fail");
    assert_eq!(err.to_string(), "Model validation error: 2 validation errors for JobTemplate\nsteps[1] -> dependencies[2] -> environment:\n\tduplicate environment dependency 'F'.\nsteps[0] -> dependencies[0] -> parameters -> P:\n\tParameter 'P': value 1 is less than minimum 5");
}

#[test]
fn a_later_steps_step_5_and_7_errors_are_kept_when_an_earlier_step_is_stopped() {
    // S0 is stopped by Missing. S1 has, in its own Environments: X's
    // undeclared key Z (step 5) and value below the minimum (step 7), Y's
    // missing P (step 7), R's value (step 7, a Job Environment), and F's
    // undeclared key Z (step 5, a Job Environment).
    let jt = s0_s1(
        "  dependencies: [{environment: Missing}]\n",
        &format!(
            "  dependencies: [{{environment: X, parameters: {{N: 1, Z: 1}}}}, {R_BAD}, \
             {{environment: F, enableIf: \"Param.T\", parameters: {{Z: 1}}}}]\n"
        ),
    );
    let x = env(
        "name: X, activation: ON_REQUEST, envParameterDefinitions: [{name: N, type: INT, \
         minValue: 5}]",
    );
    let y = env("name: Y, envParameterDefinitions: [{name: P, type: STRING}]");
    assert_eq!(err_text(&jt, &[&x, &y], &[]), "Model validation error: 6 validation errors for Submission\nJobTemplate -> steps[0] -> dependencies[0] -> environment:\n\tnames Environment 'Missing', which is not a jobEnvironments entry of the Job Template, and none of the attached Environment Templates (EnvironmentTemplate[0], EnvironmentTemplate[1]) defines it. The entry is required; set 'required: false' if the Step can run without the Environment (Template Schemas §3.2.1 item 2).\nJobTemplate -> steps[1] -> dependencies[0] -> parameters -> Z:\n\tEnvironment 'X' (EnvironmentTemplate[0]) declares no parameter 'Z'. Declared: N.\nJobTemplate -> steps[1] -> dependencies[2] -> parameters -> Z:\n\tEnvironment 'F' declares no parameter 'Z'. Declared: Q.\nJobTemplate -> steps[1] -> dependencies[0] -> parameters -> N:\n\tParameter 'N': value 1 is less than minimum 5\nJobTemplate -> steps[1]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every attached Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1); missing: 'P' of Environment 'Y' (EnvironmentTemplate[1]).\nJobTemplate -> steps[1] -> dependencies[1] -> parameters -> P:\n\tParameter 'P': value 1 is less than minimum 5");
}

#[test]
fn a_job_template_without_env_params_reports_its_attachments_errors_across_steps_in_step_order() {
    // Two Steps: each misses A's Y (step 7), and A's enableIf fails once
    // for both (step 6), filed under S0, so it comes first. Before, it came
    // after both, in the order the errors were found.
    let jt = "specificationVersion: jobtemplate-2023-09\nname: J\nsteps:\n- name: S0\n  \
              script: {actions: {onRun: {command: x}}}\n- name: S1\n  script: {actions: \
              {onRun: {command: x}}}\n";
    let a = env(
        "name: A, envParameterDefinitions: [{name: X, type: INT, default: 1}, {name: Y, type: \
         STRING}], enableIf: \"Env.Param.X / (Env.Param.X - Env.Param.X) > 0\"",
    );
    assert_eq!(err_text(jt, &[&a], &[]), "Model validation error: 3 validation errors for Submission\nEnvironmentTemplate[0] -> environment -> enableIf:\n\tcould not be evaluated: Division by zero. Resolved for Steps 'S0' and 'S1'.\n  Env.Param.X / (Env.Param.X - Env.Param.X) > 0\n  ~~~~~~~~~~~~^~~~~~~~~~~~~~~~~~~~~~~~~~~~~\nJobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every attached Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1); missing: 'Y' of Environment 'A' (EnvironmentTemplate[0]) (its own enableIf could not be evaluated for this Step, so the Environment is checked as if it is not disabled).\nJobTemplate -> steps[1]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every attached Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1); missing: 'Y' of Environment 'A' (EnvironmentTemplate[0]) (its own enableIf could not be evaluated for this Step, so the Environment is checked as if it is not disabled).");
}

#[test]
fn a_job_template_without_env_params_reports_a_caller_limit_only_when_there_is_no_other_error() {
    // As with ENV_PARAMS, the caller limits are checked after the
    // submission stage: the attachment's missing Y is reported, not the task
    // count (before, the task count was reported, and the attachment was
    // not applied). With Y given a default, the task count.
    let jt = job_ok(
        "specificationVersion: jobtemplate-2023-09\nname: J\nsteps:\n- name: S\n  \
         parameterSpace:\n    taskParameterDefinitions: [{name: T, type: INT, range: \"1-3\"}]\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    let limited = |a: &str| {
        let a = env_ok(&env(a));
        let values = job_params_with(&jt, &[], std::slice::from_ref(&a));
        let ctx = template_ctx(&jt).with_caller_limits(openjd_model::CallerLimits {
            max_task_count: Some(2),
            ..Default::default()
        });
        create_job_with_environment_templates(
            &jt,
            &[AttachedEnvironmentTemplate::new(&a)],
            &values,
            &ctx,
        )
        .expect_err("expected job creation to fail")
        .to_string()
    };
    assert_eq!(limited("name: A, envParameterDefinitions: [{name: Y, type: STRING}]"), "Model validation error: 1 validation error for Submission\nJobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every attached Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1); missing: 'Y' of Environment 'A' (EnvironmentTemplate[0]).");
    assert_eq!(limited("name: A, envParameterDefinitions: [{name: Y, type: STRING, default: y}]"), "Model validation error: 1 validation error for Template\nTemplate: Total task count (3) exceeds caller limit of 2.");
}

#[test]
fn the_environment_size_limit_applies_to_step_environments() {
    for head in [HEAD, "specificationVersion: jobtemplate-2023-09\nname: J\n"] {
        let jt = job_ok(&format!(
            "{head}steps:\n- name: S\n  stepEnvironments:\n  - {{name: SE, variables: {{V: \
             aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa}}}}\n  script: {{actions: {{onRun: \
             {{command: x}}}}}}\n"
        ));
        let values = job_params_with(&jt, &[], &[]);
        let ctx = template_ctx(&jt).with_caller_limits(openjd_model::CallerLimits {
            max_environment_size: Some(20),
            ..Default::default()
        });
        let err = openjd_model::create_job(&jt, &values, &ctx).expect_err("expected an error");
        assert_eq!(err.to_string(), "Model validation error: 1 validation error for Template\nTemplate: Environment 'SE' size (107 bytes) exceeds caller limit of 20 bytes.");
    }
}

#[test]
fn an_uninstantiable_step_leaves_the_attachments_unapplied() {
    // S0's let binding fails, so the submission stage does not run: S1's
    // unresolved entry (step 3) is not reported, its failing entry enableIf
    // (step 2) is.
    let src = |s1_entry: &str| {
        format!(
            "{HEAD}parameterDefinitions: [{{name: L, type: 'LIST[INT]', default: [1]}}]\n\
             steps:\n- name: S0\n  let: [\"z = Param.L[5]\"]\n  script: {{actions: {{onRun: \
             {{command: x}}}}}}\n- name: S1\n  dependencies: [{s1_entry}]\n  script: {{actions: \
             {{onRun: {{command: x}}}}}}\n"
        )
    };
    assert_eq!(err_text(&src("{environment: Missing}"), &[], &[]), "Model validation error: 1 validation error for JobTemplate\nsteps[0] -> let[0]:\n\tInvalid expression in let binding 'z': Index 5 out of bounds for list of length 1\n  z = Param.L[5]\n      ~~~~~~~^~~");
    assert_eq!(
        err_text(&src("{environment: Missing, enableIf: \"Param.L[4] == 1\"}"), &[], &[]),
        "Model validation error: 2 validation errors for JobTemplate\nsteps[0] -> let[0]:\n\tInvalid expression in let binding 'z': Index 5 out of bounds for list of length 1\n  z = Param.L[5]\n      ~~~~~~~^~~\nsteps[1] -> dependencies[0] -> enableIf:\n\tcould not be evaluated: Index 4 out of bounds for list of length 1\n  Param.L[4] == 1\n  ~~~~~~~^~~"
    );
}

// ── Round 5 ──────────────────────────────────────────────────────────

#[test]
fn an_attachments_value_error_precedes_its_own_constraint_7_error() {
    // A's N is below its minimum and its Y is missing: both at A's
    // position, the value (found first) before the constraint 7 error, and
    // both before the later B's value.
    let jt = format!(
        "{HEAD}steps:\n- name: S\n  dependencies: [{{environment: A, parameters: {{N: 1}}}}, \
         {{environment: B, parameters: {{N: 1}}}}]\n  script: {{actions: {{onRun: {{command: \
         x}}}}}}\n"
    );
    let a = env(
        "name: A, activation: ON_REQUEST, envParameterDefinitions: [{name: N, type: INT, \
         minValue: 5}, {name: Y, type: STRING}]",
    );
    let b = env(
        "name: B, activation: ON_REQUEST, envParameterDefinitions: [{name: N, type: INT, \
         minValue: 5}]",
    );
    assert_eq!(err_text(&jt, &[&a, &b], &[]), "Model validation error: 3 validation errors for Submission\nJobTemplate -> steps[0] -> dependencies[0] -> parameters -> N:\n\tParameter 'N': value 1 is less than minimum 5\nJobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every attached Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1); missing: 'Y' of Environment 'A' (EnvironmentTemplate[0]).\nJobTemplate -> steps[0] -> dependencies[1] -> parameters -> N:\n\tParameter 'N': value 1 is less than minimum 5");
}

#[test]
fn a_job_template_without_env_params_does_not_stop_a_step_on_an_attachments_definition_error() {
    // X's literal default, edited below its minimum, fails its definition;
    // B, ALWAYS, misses P for S. Both are reported, as before.
    let jt = job_ok(
        "specificationVersion: jobtemplate-2023-09\nname: J\nsteps:\n- name: S\n  script: \
         {actions: {onRun: {command: x}}}\n",
    );
    let mut x = env_ok(&env(
        "name: X, envParameterDefinitions: [{name: N, type: INT, minValue: 5, default: 5}]",
    ));
    x.environment.env_parameter_definitions.as_mut().unwrap()[0].default = env_ok(&env(
        "name: O, envParameterDefinitions: [{name: N, type: INT, default: 1}]",
    ))
    .environment
    .env_parameter_definitions
    .unwrap()[0]
        .default
        .clone();
    let b = env_ok(&env(
        "name: B, activation: ALWAYS, envParameterDefinitions: [{name: P, type: STRING}]",
    ));
    let envs = [x, b];
    let values = job_params_with(&jt, &[], &envs);
    let attached: Vec<_> = envs.iter().map(AttachedEnvironmentTemplate::new).collect();
    let err = create_job_with_environment_templates(&jt, &attached, &values, &template_ctx(&jt))
        .expect_err("expected job creation to fail");
    assert_eq!(err.to_string(), "Model validation error: 2 validation errors for Submission\nEnvironmentTemplate[0] -> environment -> envParameterDefinitions[0] -> default:\n\tParameter 'N': value 1 is less than minimum 5\nJobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every attached Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1); missing: 'P' of Environment 'B' (EnvironmentTemplate[1]).");
}

#[test]
fn a_value_equal_to_a_caller_limit_is_within_it() {
    let jt = job_ok(&format!(
        "{HEAD}jobEnvironments:\n- {{name: E, variables: {{V: e}}}}\nsteps:\n- name: S\n  \
         stepEnvironments:\n  - {{name: SE, variables: {{V: s}}}}\n  parameterSpace:\n    \
         taskParameterDefinitions: [{{name: T, type: INT, range: \"1-3\"}}]\n  script: \
         {{actions: {{onRun: {{command: x}}}}}}\n"
    ));
    let values = job_params_with(&jt, &[], &[]);
    let with = |limits: openjd_model::CallerLimits| {
        openjd_model::create_job(&jt, &values, &template_ctx(&jt).with_caller_limits(limits))
            .map(|_| ())
            .map_err(|e| e.to_string())
    };
    // Each size, measured by a limit of 1.
    let size = |message: Result<(), String>| -> usize {
        let m = message.expect_err("expected an error");
        let n = m.rsplit(" size (").next().unwrap();
        n[..n.find(' ').unwrap()].parse().unwrap()
    };
    let script = size(with(openjd_model::CallerLimits {
        max_step_script_size: Some(1),
        ..Default::default()
    }));
    let se = size(with(openjd_model::CallerLimits {
        max_environment_size: Some(1),
        ..Default::default()
    }));
    // SE at its size is within the limit, so the larger E is reported.
    let e_message = with(openjd_model::CallerLimits {
        max_environment_size: Some(se),
        ..Default::default()
    });
    assert!(e_message
        .as_ref()
        .unwrap_err()
        .contains("Environment 'E' size"));
    let e = size(e_message);
    for limits in [
        openjd_model::CallerLimits {
            max_task_count: Some(3),
            ..Default::default()
        },
        openjd_model::CallerLimits {
            max_step_script_size: Some(script),
            ..Default::default()
        },
        openjd_model::CallerLimits {
            max_environment_size: Some(e),
            ..Default::default()
        },
    ] {
        assert_eq!(with(limits), Ok(()));
    }
    assert_eq!(
        with(openjd_model::CallerLimits {
            max_task_count: Some(2),
            ..Default::default()
        }),
        Err("Model validation error: 1 validation error for Template\nTemplate: Total task count (3) exceeds caller limit of 2.".to_string())
    );
    // Both Environments exceed 1 byte: the Step Environment is reported.
    assert_eq!(
        with(openjd_model::CallerLimits {
            max_environment_size: Some(1),
            ..Default::default()
        }),
        Err(format!("Model validation error: 1 validation error for Template\nTemplate: Environment 'SE' size ({se} bytes) exceeds caller limit of 1 bytes."))
    );
}

// ── Round 6 ──────────────────────────────────────────────────────────

#[test]
fn a_step_environment_whose_enable_if_fails_counts_for_the_single_layer_rule() {
    // Inner's enableIf fails (step 6), so it is checked as if enabled: with
    // Outer, the Step enters two wrapping Environments (step 7).
    let jt = format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR, \
         WRAP_ACTIONS]\nname: J\nparameterDefinitions: [{{name: L, type: 'LIST[INT]', default: \
         [1]}}]\nsteps:\n- name: S\n  stepEnvironments:\n  - {{name: Outer, {WRAP}}}\n  - \
         {{name: Inner, enableIf: \"Param.L[4] == 1\", {WRAP}}}\n  script: {{actions: {{onRun: \
         {{command: x}}}}}}\n"
    );
    assert_eq!(err_text(&jt, &[], &[]), "Model validation error: 2 validation errors for JobTemplate\nsteps[0] -> stepEnvironments[1] -> enableIf:\n\tcould not be evaluated: Index 4 out of bounds for list of length 1\n  Param.L[4] == 1\n  ~~~~~~~^~~\nsteps[0]:\n\tits Sessions would enter 2 Environments that define wrap hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'Outer' (stepEnvironments), 'Inner' (stepEnvironments). A Session may have at most one Environment defining wrap hooks (RFC 0008).");
}

// ── Round 7 ──────────────────────────────────────────────────────────

#[test]
fn a_job_template_without_env_params_reports_its_own_staged_error_for_the_submission() {
    // A template edited after decoding: a duplicate entry, then ENV_PARAMS
    // removed. Its one error (§7.4.1 step 2) is the Job Template's own, for
    // Submission, at the document's path, since an Environment Template is
    // attached. Before, it was reported alone for JobTemplate.
    let mut jt = job_ok(&format!(
        "{HEAD}jobEnvironments:\n- {{name: E, variables: {{V: e}}}}\nsteps:\n- name: S\n  \
         dependencies: [{{environment: E}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    ));
    let deps = jt.steps[0].dependencies.as_mut().unwrap();
    let e = deps[0].clone();
    deps.push(e);
    jt.extensions
        .as_mut()
        .unwrap()
        .retain(|x| x.as_str() != "ENV_PARAMS");
    let x = env_ok(&env("name: X"));
    let values = job_params_with(&jt, &[], std::slice::from_ref(&x));
    let err = create_job_with_environment_templates(
        &jt,
        &[AttachedEnvironmentTemplate::new(&x)],
        &values,
        &template_ctx(&jt),
    )
    .expect_err("expected job creation to fail");
    assert_eq!(err.to_string(), "Model validation error: 1 validation error for Submission\nJobTemplate -> steps[0] -> dependencies[1] -> environment:\n\tduplicate environment dependency 'E'.");
    let values = job_params_with(&jt, &[], &[]);
    let err = create_job_with_environment_templates(&jt, &[], &values, &template_ctx(&jt))
        .expect_err("expected job creation to fail");
    assert_eq!(err.to_string(), "Model validation error: 1 validation error for JobTemplate\nsteps[0] -> dependencies[1] -> environment:\n\tduplicate environment dependency 'E'.");
}

// ── Every Job Template ───────────────────────────────

#[test]
fn without_env_params_job_and_attachment_errors_are_ordered_by_7_4_1_step_then_by_step() {
    // No ENV_PARAMS in the Job Template. S1's timeout is a
    // template-validation-style check, found first and reported first. A,
    // attached, misses Y for both Steps (step 7) and its enableIf fails
    // (step 6, filed under S0, after both Steps' step 7 errors were found);
    // B's hostRequirements fail for both (step 8, filed under S0). So the
    // order is by §7.4.1 step, then by Step, whatever the order the errors
    // were found in.
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [EXPR, FEATURE_BUNDLE_1]\nname: J\n\
              parameterDefinitions: [{name: T, type: INT, default: 0}]\nsteps:\n- name: S0\n  script: {actions: {onRun: \
              {command: x}}}\n- name: S1\n  script: {actions: {onRun: {command: x, timeout: \
              '{{Param.T}}'}}}\n";
    let a = env(
        "name: A, envParameterDefinitions: [{name: X, type: INT, default: 1}, {name: Y, type: \
         STRING}], enableIf: \"Env.Param.X / (Env.Param.X - Env.Param.X) > 0\"",
    );
    let b = "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR, \
             FEATURE_BUNDLE_1]\nenvironment: {name: B, envParameterDefinitions: [{name: L, \
             type: 'LIST[INT]', default: [1]}], hostRequirements: {amounts: [{name: \
             amount.worker.vcpu, min: \"{{Env.Param.L[4]}}\"}]}, variables: {V: b}}\n";
    let c7 = |step: usize| {
        format!(
            "JobTemplate -> steps[{step}]:\n\tmust supply, in an 'environment:' dependency, \
             every parameter without a default of each Environment its Sessions enter (every \
             attached Environment with activation ALWAYS, each one with activation BY_DEFAULT \
             that the Step does not decline, and each one with activation ON_REQUEST that the \
             Step requests; Template Schemas §3.2.1); missing: 'Y' of Environment 'A' \
             (EnvironmentTemplate[0]) (its own enableIf could not be evaluated for this Step, \
             so the Environment is checked as if it is not disabled)."
        )
    };
    let enable_if = "EnvironmentTemplate[0] -> environment -> enableIf:\n\tcould not be \
                     evaluated: Division by zero. Resolved for Steps 'S0' and 'S1'.\n  \
                     Env.Param.X / (Env.Param.X - Env.Param.X) > 0\n  \
                     ~~~~~~~~~~~~^~~~~~~~~~~~~~~~~~~~~~~~~~~~~";
    let host = "EnvironmentTemplate[1] -> environment -> hostRequirements -> amounts[0] -> \
                min:\n\tFormat string error: hostRequirements amount min: Index 4 out of bounds \
                for list of length 1. Resolved for Steps 'S0' and 'S1'.\n  Env.Param.L[4]\n  \
                ~~~~~~~~~~~^~~";
    assert_eq!(
        err_text(jt, &[&a, b], &[]),
        format!(
            "Model validation error: 5 validation errors for Submission\nJobTemplate -> \
             steps[1] -> script -> actions -> onRun -> timeout:\n\ttimeout must be > \
             0.\n{enable_if}\n{}\n{}\n{host}",
            c7(0),
            c7(1)
        )
    );
}

#[test]
fn a_job_template_without_env_params_and_without_attachments_reports_as_before() {
    // No ENV_PARAMS and nothing attached: the errors are the Job
    // Template's own, for JobTemplate, at its own paths, in the order they
    // are found (here every one is a template-validation-style check), and a
    // caller limit only when there is no other error.
    let src = |t: &str| {
        format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [EXPR, FEATURE_BUNDLE_1]\nname: J\n\
             parameterDefinitions: [{{name: T, type: INT, default: {t}}}]\nsteps:\n- name: S0\n  parameterSpace:\n    \
             taskParameterDefinitions: [{{name: P, type: INT, range: \"1-3\"}}]\n  script: \
             {{actions: {{onRun: {{command: x, timeout: '{{{{Param.T}}}}'}}}}}}\n- name: S1\n  \
             script: {{actions: {{onRun: {{command: x, timeout: '{{{{Param.T}}}}'}}}}}}\n"
        )
    };
    let limited = |src: &str| {
        let jt = job_ok(src);
        let values = job_params_with(&jt, &[], &[]);
        let ctx = template_ctx(&jt).with_caller_limits(openjd_model::CallerLimits {
            max_task_count: Some(2),
            ..Default::default()
        });
        create_job_with_environment_templates(&jt, &[], &values, &ctx)
            .expect_err("expected job creation to fail")
            .to_string()
    };
    assert_eq!(limited(&src("0")), "Model validation error: 2 validation errors for JobTemplate\nsteps[0] -> script -> actions -> onRun -> timeout:\n\ttimeout must be > 0.\nsteps[1] -> script -> actions -> onRun -> timeout:\n\ttimeout must be > 0.");
    assert_eq!(limited(&src("5")), "Model validation error: 1 validation error for Template\nTemplate: Total task count (4) exceeds caller limit of 2.");
}

#[test]
fn a_job_template_without_env_params_files_the_single_layer_rule_between_steps_6_and_8() {
    // No ENV_PARAMS in the Job Template, whose Job Environment W1 wraps.
    // Attached: A, whose enableIf fails (step 6); W2, which wraps too, so S
    // breaks the single-layer rule (step 7); and B, whose hostRequirements
    // fail (step 8). The single-layer error comes between them, by its
    // step.
    let jt = format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [WRAP_ACTIONS, EXPR]\nname: \
         J\njobEnvironments:\n- {{name: W1, {WRAP}}}\nsteps:\n- name: S\n  script: {{actions: \
         {{onRun: {{command: x}}}}}}\n"
    );
    let a = env(
        "name: A, envParameterDefinitions: [{name: X, type: INT, default: 1}], enableIf: \
         \"Env.Param.X / (Env.Param.X - Env.Param.X) > 0\"",
    );
    let w2 = format!(
        "specificationVersion: environment-2023-09\nextensions: [WRAP_ACTIONS, EXPR]\n\
         environment: {{name: W2, {WRAP}}}\n"
    );
    let b = B_HOST;
    assert_eq!(err_text(&jt, &[&a, &w2, b], &[]), "Model validation error: 3 validation errors for Submission\nEnvironmentTemplate[0] -> environment -> enableIf:\n\tcould not be evaluated: Division by zero. Resolved for Step 'S'.\n  Env.Param.X / (Env.Param.X - Env.Param.X) > 0\n  ~~~~~~~~~~~~^~~~~~~~~~~~~~~~~~~~~~~~~~~~~\nJobTemplate -> steps[0]:\n\tits Sessions would enter 2 Environments that define wrap hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'W2' (EnvironmentTemplate[1]), 'W1'. A Session may have at most one Environment defining wrap hooks (RFC 0008).\nEnvironmentTemplate[2] -> environment -> hostRequirements -> amounts[0] -> min:\n\tFormat string error: hostRequirements amount min: Index 4 out of bounds for list of length 1. Resolved for Step 'S'.\n  Env.Param.L[4]\n  ~~~~~~~~~~~^~~");
}

#[test]
fn a_stopped_step_of_a_job_template_without_env_params_does_not_stop_the_others() {
    // A Job Template edited in code: S0 has a duplicate entry (step 2),
    // then ENV_PARAMS is removed. S0 is stopped, so B's hostRequirements
    // (step 8) are resolved for S1 only, and both errors are reported, for
    // Submission.
    let mut jt = job_ok(&format!(
        "{HEAD}jobEnvironments:\n- {{name: E, variables: {{V: e}}}}\nsteps:\n- name: S0\n  \
         dependencies: [{{environment: E}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: S1\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    ));
    let deps = jt.steps[0].dependencies.as_mut().unwrap();
    let e = deps[0].clone();
    deps.push(e);
    jt.extensions
        .as_mut()
        .unwrap()
        .retain(|x| x.as_str() != "ENV_PARAMS");
    let b = env_ok(B_HOST);
    let values = job_params_with(&jt, &[], std::slice::from_ref(&b));
    let err = create_job_with_environment_templates(
        &jt,
        &[AttachedEnvironmentTemplate::new(&b)],
        &values,
        &template_ctx(&jt),
    )
    .expect_err("expected job creation to fail");
    assert_eq!(err.to_string(), "Model validation error: 2 validation errors for Submission\nJobTemplate -> steps[0] -> dependencies[1] -> environment:\n\tduplicate environment dependency 'E'.\nEnvironmentTemplate[0] -> environment -> hostRequirements -> amounts[0] -> min:\n\tFormat string error: hostRequirements amount min: Index 4 out of bounds for list of length 1. Resolved for Step 'S1'.\n  Env.Param.L[4]\n  ~~~~~~~~~~~^~~");
}

/// An attachment whose `hostRequirements` fail for every Step that enters
/// it (§7.4.1 step 8).
const B_HOST: &str = "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR, \
                      FEATURE_BUNDLE_1]\nenvironment: {name: B, envParameterDefinitions: [{name: \
                      L, type: 'LIST[INT]', default: [1]}], hostRequirements: {amounts: [{name: \
                      amount.worker.vcpu, min: \"{{Env.Param.L[4]}}\"}]}, variables: {V: b}}\n";

// ── Round 2: each key, for a Job Template without ENV_PARAMS ─────────
//
// The keys come from code shared with ENV_PARAMS. These tests pin each
// key's value for a Job Template without it: each has two errors whose
// order a change to that key would swap. (In most of them the errors are
// also found in that order, so they passed before the one-pass order too.)

/// A Job Template without ENV_PARAMS, listing `exts`, with `rest` (its
/// `jobEnvironments` and `steps`) after the name.
fn plain(exts: &str, rest: &str) -> String {
    format!("specificationVersion: jobtemplate-2023-09\nextensions: [{exts}]\nname: J\n{rest}")
}

/// A Step `name` with `extra` (lines indented two spaces) before its script.
fn step(name: &str, extra: &str) -> String {
    format!("- name: {name}\n{extra}  script: {{actions: {{onRun: {{command: x}}}}}}\n")
}

/// An attached Environment whose `enableIf` fails for every Step (step 6).
fn failing_enable_if(name: &str) -> String {
    env(&format!(
        "name: {name}, envParameterDefinitions: [{{name: X, type: INT, default: 1}}], enableIf: \
         \"Env.Param.X / (Env.Param.X - Env.Param.X) > 0\""
    ))
}

/// An attached Environment whose `hostRequirements` fail for every Step
/// that enters it (step 8).
fn failing_host(name: &str) -> String {
    B_HOST.replace("name: B,", &format!("name: {name},"))
}

/// An attached Environment with `Q` and no default for it (step 7 for
/// every Step that enters it).
fn missing_q(name: &str) -> String {
    env(&format!(
        "name: {name}, envParameterDefinitions: [{{name: Q, type: STRING}}]"
    ))
}

#[test]
fn without_env_params_an_unreachable_attachment_follows_the_steps_constraint_7_errors() {
    // U (first) is named like the Job Environment U, so no Step can supply
    // its P (step 7, about no Step); Y's missing Q is S's (step 7, Step 0),
    // and comes first.
    let jt = plain(
        "EXPR",
        &format!(
            "jobEnvironments:\n- {{name: U, variables: {{V: u}}}}\nsteps:\n{}",
            step("S", "")
        ),
    );
    let u = env("name: U, envParameterDefinitions: [{name: P, type: STRING}]");
    assert_eq!(err_text(&jt, &[&u, &missing_q("Y")], &[]), "Model validation error: 2 validation errors for Submission\nJobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every attached Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1); missing: 'Q' of Environment 'Y' (EnvironmentTemplate[1]).\nEnvironmentTemplate[0] -> environment -> envParameterDefinitions:\n\tEnvironment 'U' has the same name as a jobEnvironments entry of the Job Template, so no 'environment:' dependency can reach it (Template Schemas §3.2.1 item 4), and it has parameters without a default that no Step can supply: P. Its activation is BY_DEFAULT, so every Session enters it, and the submission is rejected (§3.2.1).");
}

#[test]
fn without_env_params_the_single_layer_rule_follows_the_steps_environments() {
    // W2 (first) wraps, as does the Job Environment W1; Y (second) misses Q.
    // The single-layer error comes after Y's, at the end of S's step 7.
    let jt = plain(
        "WRAP_ACTIONS, EXPR",
        &format!(
            "jobEnvironments:\n- {{name: W1, {WRAP}}}\nsteps:\n{}",
            step("S", "")
        ),
    );
    let w2 = format!(
        "specificationVersion: environment-2023-09\nextensions: [WRAP_ACTIONS, EXPR]\n\
         environment: {{name: W2, {WRAP}}}\n"
    );
    assert_eq!(err_text(&jt, &[&w2, &missing_q("Y")], &[]), "Model validation error: 2 validation errors for Submission\nJobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every attached Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1); missing: 'Q' of Environment 'Y' (EnvironmentTemplate[1]).\nJobTemplate -> steps[0]:\n\tits Sessions would enter 2 Environments that define wrap hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'W2' (EnvironmentTemplate[0]), 'W1'. A Session may have at most one Environment defining wrap hooks (RFC 0008).");
}

#[test]
fn without_env_params_an_attachment_named_like_a_step_environment_is_ordered_by_its_place() {
    // X (first) is named like S's Step Environment, so S cannot reach it
    // and cannot supply its P; Y (second) misses Q. Both are S's step 7
    // errors, X's first.
    let jt = plain(
        "EXPR",
        &format!(
            "steps:\n{}",
            step(
                "S",
                "  stepEnvironments:\n  - {name: X, variables: {V: x}}\n"
            )
        ),
    );
    let x = env("name: X, envParameterDefinitions: [{name: P, type: STRING}]");
    assert_eq!(err_text(&jt, &[&x, &missing_q("Y")], &[]), "Model validation error: 2 validation errors for Submission\nJobTemplate -> steps[0]:\n\tcannot supply the parameters without a default of Environment 'X' (EnvironmentTemplate[0]): P. It has the same name as a stepEnvironments entry of this Step, so no 'environment:' dependency of this Step can reach it (Template Schemas §3.2.1 item 5), and its activation is BY_DEFAULT, so this Step's Sessions enter it.\nJobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every attached Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1); missing: 'Q' of Environment 'Y' (EnvironmentTemplate[1]).");
}

#[test]
fn without_env_params_an_attachments_value_error_follows_a_later_ones_enable_if() {
    // A (first): its default resolves below its minValue (step 7). B
    // (second): its enableIf fails (step 6), so it comes first.
    let jt = plain("EXPR", &format!("steps:\n{}", step("S", "")));
    let a = "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
             parameterDefinitions: [{name: N, type: INT, default: 1}]\nenvironment: {name: A, \
             envParameterDefinitions: [{name: M, type: INT, minValue: 5, default: \
             \"{{Param.N}}\"}], variables: {V: a}}\n";
    assert_eq!(
        err_text(&jt, &[a, &failing_enable_if("B")], &[]),
        "Model validation error: 2 validation errors for Submission\nEnvironmentTemplate[1] -> environment -> enableIf:\n\tcould not be evaluated: Division by zero. Resolved for Step 'S'.\n  Env.Param.X / (Env.Param.X - Env.Param.X) > 0\n  ~~~~~~~~~~~~^~~~~~~~~~~~~~~~~~~~~~~~~~~~~\nEnvironmentTemplate[0] -> environment -> envParameterDefinitions[0] -> default:\n\tfor Step 'S', which does not supply this parameter: Parameter 'M': value 1 is less than minimum 5"
    );
}

#[test]
fn errors_of_one_step_follow_the_attachment_order() {
    // Two attachments with the same kind of error, at steps 6 and 8: each
    // pair in attachment order, with ENV_PARAMS in the Job Template or
    // without it.
    for exts in ["EXPR", "ENV_PARAMS, EXPR"] {
        let jt = plain(exts, &format!("steps:\n{}", step("S", "")));
        assert_eq!(
            err_text(
                &jt,
                &[&failing_enable_if("A1"), &failing_enable_if("A2")],
                &[]
            ),
            "Model validation error: 2 validation errors for Submission\nEnvironmentTemplate[0] -> environment -> enableIf:\n\tcould not be evaluated: Division by zero. Resolved for Step 'S'.\n  Env.Param.X / (Env.Param.X - Env.Param.X) > 0\n  ~~~~~~~~~~~~^~~~~~~~~~~~~~~~~~~~~~~~~~~~~\nEnvironmentTemplate[1] -> environment -> enableIf:\n\tcould not be evaluated: Division by zero. Resolved for Step 'S'.\n  Env.Param.X / (Env.Param.X - Env.Param.X) > 0\n  ~~~~~~~~~~~~^~~~~~~~~~~~~~~~~~~~~~~~~~~~~",
            "{exts}"
        );
        assert_eq!(
            err_text(&jt, &[&failing_host("H1"), &failing_host("H2")], &[]),
            "Model validation error: 2 validation errors for Submission\nEnvironmentTemplate[0] -> environment -> hostRequirements -> amounts[0] -> min:\n\tFormat string error: hostRequirements amount min: Index 4 out of bounds for list of length 1. Resolved for Step 'S'.\n  Env.Param.L[4]\n  ~~~~~~~~~~~^~~\nEnvironmentTemplate[1] -> environment -> hostRequirements -> amounts[0] -> min:\n\tFormat string error: hostRequirements amount min: Index 4 out of bounds for list of length 1. Resolved for Step 'S'.\n  Env.Param.L[4]\n  ~~~~~~~~~~~^~~",
            "{exts}"
        );
    }
}

#[test]
fn without_env_params_an_attachments_host_requirements_precede_the_first_steps_conflict() {
    // H's hostRequirements fail for S0 and S1 (filed under S0), and D's
    // literal min 4 conflicts with S0's max 2 (S0's step 8, after its
    // Environments).
    let jt = plain(
        "EXPR",
        &format!(
            "steps:\n{}{}",
            step(
                "S0",
                "  hostRequirements: {amounts: [{name: amount.custom, max: 2}]}\n"
            ),
            step("S1", "")
        ),
    );
    let d = env("name: D, hostRequirements: {amounts: [{name: amount.custom, min: 4}]}");
    assert_eq!(err_text(&jt, &[&failing_host("H"), &d], &[]), "Model validation error: 2 validation errors for Submission\nEnvironmentTemplate[0] -> environment -> hostRequirements -> amounts[0] -> min:\n\tFormat string error: hostRequirements amount min: Index 4 out of bounds for list of length 1. Resolved for Steps 'S0' and 'S1'.\n  Env.Param.L[4]\n  ~~~~~~~~~~~^~~\nJobTemplate -> steps[0]:\n\tStep 'S0': no host can satisfy the combined host requirements for amount 'amount.custom': EnvironmentTemplate[1] -> environment -> hostRequirements (Environment 'D') requires min 4, and JobTemplate -> steps[0] -> hostRequirements requires max 2. Template Schemas §3.3.3.");
}

// Job Templates without ENV_PARAMS built or edited in code, which template
// validation would reject: their entries and Job Environment parameters
// are checked as with ENV_PARAMS.

/// Decode `src` (which lists ENV_PARAMS), let `edit` change it, then
/// remove ENV_PARAMS, and create the Job with `envs` attached.
fn edited(
    src: &str,
    edit: impl FnOnce(&mut openjd_model::template::JobTemplate),
    envs: &[&str],
    params: &[(&str, &str)],
) -> String {
    let mut jt = job_ok(src);
    edit(&mut jt);
    jt.extensions
        .as_mut()
        .unwrap()
        .retain(|x| x.as_str() != "ENV_PARAMS");
    let envs: Vec<EnvironmentTemplate> = envs.iter().map(|s| env_ok(s)).collect();
    let values = job_params_with(&jt, params, &envs);
    let attached: Vec<_> = envs.iter().map(AttachedEnvironmentTemplate::new).collect();
    create_job_with_environment_templates(&jt, &attached, &values, &template_ctx(&jt))
        .expect_err("expected job creation to fail")
        .to_string()
}

/// R's P has no default; S's entry for R supplies it, until it is removed.
const R_SRC: &str = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR, \
                     FEATURE_BUNDLE_1]\nname: J\nparameterDefinitions: [{name: T, type: INT, \
                     default: 0}]\njobEnvironments:\n- name: R\n  envParameterDefinitions: \
                     [{name: P, type: STRING}]\n  variables: {V: r}\nsteps:\n- name: S\n  \
                     dependencies: [{environment: R, parameters: {P: p}}]\n  script: {actions: \
                     {onRun: {command: x, timeout: '{{Param.T}}'}}}\n";

fn drop_entries(jt: &mut openjd_model::template::JobTemplate) {
    jt.steps[0].dependencies = None;
}

#[test]
fn an_edited_job_template_without_env_params_and_attachments_reports_every_error() {
    // The timeout and R's missing P, together, for JobTemplate. Before the
    // one pass, the timeout alone.
    assert_eq!(edited(R_SRC, drop_entries, &[], &[]), "Model validation error: 2 validation errors for JobTemplate\nsteps[0] -> script -> actions -> onRun -> timeout:\n\ttimeout must be > 0.\nsteps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every jobEnvironments entry with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7); missing: 'P' of Environment 'R'.");
}

#[test]
fn an_edited_job_template_without_env_params_files_its_job_environments_constraint_7_at_step_7() {
    // R's missing P (step 7) before the attachment's hostRequirements (step
    // 8).
    assert_eq!(
        edited(R_SRC, drop_entries, &[B_HOST], &[("T", "5")]),
        "Model validation error: 2 validation errors for Submission\nJobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every jobEnvironments entry with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7); missing: 'P' of Environment 'R'.\nEnvironmentTemplate[0] -> environment -> hostRequirements -> amounts[0] -> min:\n\tFormat string error: hostRequirements amount min: Index 4 out of bounds for list of length 1. Resolved for Step 'S'.\n  Env.Param.L[4]\n  ~~~~~~~~~~~^~~"
    );
}

#[test]
fn an_edited_job_template_without_env_params_files_an_undeclared_key_at_step_5() {
    // S's entry for the attached X supplies Z, which X does not declare
    // (step 5), before A's failing enableIf (step 6).
    let src = format!(
        "{HEAD}steps:\n- name: S\n  dependencies: [{{environment: X, parameters: {{Z: 1}}}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let x = env("name: X, envParameterDefinitions: [{name: N, type: INT, default: 1}]");
    assert_eq!(
        edited(&src, |_| {}, &[&x, &failing_enable_if("A")], &[]),
        "Model validation error: 2 validation errors for Submission\nJobTemplate -> steps[0] -> dependencies[0] -> parameters -> Z:\n\tEnvironment 'X' (EnvironmentTemplate[0]) declares no parameter 'Z'. Declared: N.\nEnvironmentTemplate[1] -> environment -> enableIf:\n\tcould not be evaluated: Division by zero. Resolved for Step 'S'.\n  Env.Param.X / (Env.Param.X - Env.Param.X) > 0\n  ~~~~~~~~~~~~^~~~~~~~~~~~~~~~~~~~~~~~~~~~~"
    );
}

// ── Round 3 ──────────────────────────────────────────────────────────

#[test]
fn without_env_params_the_job_templates_validation_errors_precede_the_attachments_defaults() {
    // Both are template-validation-style checks: the Job Template's
    // timeout, then the attachment's literal default, edited below its
    // minimum.
    let jt = plain(
        "EXPR, FEATURE_BUNDLE_1",
        "parameterDefinitions: [{name: T, type: INT, default: 0}]\nsteps:\n- name: S\n  \
         script: {actions: {onRun: {command: x, timeout: '{{Param.T}}'}}}\n",
    );
    let a = below_minimum_default("A");
    assert_eq!(err_text_envs(&jt, &[a]), "Model validation error: 2 validation errors for Submission\nJobTemplate -> steps[0] -> script -> actions -> onRun -> timeout:\n\ttimeout must be > 0.\nEnvironmentTemplate[0] -> environment -> envParameterDefinitions[0] -> default:\n\tParameter 'N': value 1 is less than minimum 5");
}

#[test]
fn an_edited_job_template_without_env_params_stops_a_step_as_with_env_params() {
    // Each template of an ENV_PARAMS test, with ENV_PARAMS removed in code:
    // the stop rule and the uninstantiable Step give the same errors.
    let x = env("name: X");
    let cases: [(String, Vec<&str>); 3] = [
        // An undeclared key (step 5, found first) on a Step that its
        // unresolved entry (step 3) stops.
        (
            format!(
                "{HEAD}parameterDefinitions: [{{name: B, type: BOOL, default: true}}]\n\
                 jobEnvironments:\n- {{name: A, envParameterDefinitions: [{{name: P, type: \
                 STRING, default: p}}], variables: {{V: a}}}}\nsteps:\n- name: S\n  \
                 dependencies: [{{environment: A, enableIf: \"Param.B\", parameters: {{Q: \
                 2}}}}, {{environment: Missing}}]\n  script: {{actions: {{onRun: {{command: \
                 x}}}}}}\n"
            ),
            vec![&x],
        ),
        // A disabled entry for an ALWAYS Environment (step 4) stops S, so
        // SE's failing enableIf (step 6) is not reported.
        (
            format!(
                "{HEAD}parameterDefinitions: [{{name: B, type: BOOL, default: false}}, {{name: \
                 L, type: 'LIST[INT]', default: [1]}}]\njobEnvironments:\n- {{name: A, \
                 activation: ALWAYS, variables: {{V: a}}}}\nsteps:\n- name: S\n  \
                 dependencies: [{{environment: A, enableIf: \"Param.B\"}}]\n  \
                 stepEnvironments:\n  - {{name: SE, enableIf: \"Param.L[4] == 1\", variables: \
                 {{V: s}}}}\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
            ),
            vec![],
        ),
        // S0's let binding fails, so the attachments are not applied; S1's
        // failing entry enableIf (step 2) is still reported.
        (
            format!(
                "{HEAD}parameterDefinitions: [{{name: L, type: 'LIST[INT]', default: [1]}}]\n\
                 steps:\n- name: S0\n  let: [\"z = Param.L[5]\"]\n  script: {{actions: \
                 {{onRun: {{command: x}}}}}}\n- name: S1\n  dependencies: [{{environment: \
                 Missing, enableIf: \"Param.L[4] == 1\"}}]\n  script: {{actions: {{onRun: \
                 {{command: x}}}}}}\n"
            ),
            vec![],
        ),
    ];
    let mut texts = Vec::new();
    for (src, envs) in &cases {
        let text = edited(src, |_| {}, envs, &[]);
        assert_eq!(text, err_text(src, envs, &[]), "{src}");
        texts.push(text);
    }
    assert_eq!(texts[0], "Model validation error: 1 validation error for Submission\nJobTemplate -> steps[0] -> dependencies[1] -> environment:\n\tnames Environment 'Missing', which is not a jobEnvironments entry of the Job Template, and none of the attached Environment Templates (EnvironmentTemplate[0]) defines it. The entry is required; set 'required: false' if the Step can run without the Environment (Template Schemas §3.2.1 item 2).");
}

// ── After round 5 (low findings) ─────────────────────────────────────

/// A Job Template that lists no extension, with `steps`.
fn bare(steps: &str) -> String {
    format!("specificationVersion: jobtemplate-2023-09\nname: J\n{steps}")
}

/// Create `jt` with `envs` attached under `limits`; the error's text.
fn limited_err(jt: &str, envs: &[&str], limits: openjd_model::CallerLimits) -> String {
    let jt = job_ok(jt);
    let envs: Vec<EnvironmentTemplate> = envs.iter().map(|s| env_ok(s)).collect();
    let values = job_params_with(&jt, &[], &envs);
    let attached: Vec<_> = envs.iter().map(AttachedEnvironmentTemplate::new).collect();
    let ctx = template_ctx(&jt).with_caller_limits(limits);
    create_job_with_environment_templates(&jt, &attached, &values, &ctx)
        .expect_err("expected job creation to fail")
        .to_string()
}

#[test]
fn a_job_template_without_extensions_reports_its_errors_with_the_attachments() {
    // A 2023-09 Job Template with no extension: an argument longer than the
    // caller's limit (a template-validation-style check) and the attached
    // Y's missing Q, together, for Submission; without the attachment, the
    // argument alone, for JobTemplate.
    let jt = bare(
        "parameterDefinitions: [{name: P, type: STRING, default: abcdefghij}]\nsteps:\n- name: \
         S\n  script: {actions: {onRun: {command: x, args: ['{{Param.P}}']}}}\n",
    );
    let limits = || openjd_model::CallerLimits {
        max_resolved_arg_len: Some(4),
        ..Default::default()
    };
    assert_eq!(limited_err(&jt, &[&missing_q("Y")], limits()), "Model validation error: 2 validation errors for Submission\nJobTemplate -> steps[0] -> script -> actions -> onRun -> args[0]:\n\tresolves to at least 10 characters, exceeding the maximum of 4.\nJobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every attached Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1); missing: 'Q' of Environment 'Y' (EnvironmentTemplate[0]).");
    assert_eq!(limited_err(&jt, &[], limits()), "Model validation error: 1 validation error for JobTemplate\nsteps[0] -> script -> actions -> onRun -> args[0]:\n\tresolves to at least 10 characters, exceeding the maximum of 4.");
}

#[test]
fn a_job_template_without_extensions_keeps_the_keys() {
    // The keys for a Job Template that lists no extension: step 7 before
    // step 8, Validation before step 6, and an attachment's host
    // requirements under the first Step, before that Step's conflict.
    let s = bare(&format!("steps:\n{}", step("S", "")));
    let rel = below_minimum_default("R");
    let d = env("name: D, hostRequirements: {amounts: [{name: amount.custom, min: 4}]}");
    let two = bare(&format!(
        "steps:\n{}{}",
        step(
            "S0",
            "  hostRequirements: {amounts: [{name: amount.custom, max: 2}]}\n"
        ),
        step("S1", "")
    ));
    assert_eq!(err_text(&s, &[&failing_host("H"), &missing_q("Y")], &[]), "Model validation error: 2 validation errors for Submission\nJobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every attached Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1); missing: 'Q' of Environment 'Y' (EnvironmentTemplate[1]).\nEnvironmentTemplate[0] -> environment -> hostRequirements -> amounts[0] -> min:\n\tFormat string error: hostRequirements amount min: Index 4 out of bounds for list of length 1. Resolved for Step 'S'.\n  Env.Param.L[4]\n  ~~~~~~~~~~~^~~");
    assert_eq!(err_text_envs(&s, &[env_ok(&failing_enable_if("E")), rel]), "Model validation error: 2 validation errors for Submission\nEnvironmentTemplate[1] -> environment -> envParameterDefinitions[0] -> default:\n\tParameter 'N': value 1 is less than minimum 5\nEnvironmentTemplate[0] -> environment -> enableIf:\n\tcould not be evaluated: Division by zero. Resolved for Step 'S'.\n  Env.Param.X / (Env.Param.X - Env.Param.X) > 0\n  ~~~~~~~~~~~~^~~~~~~~~~~~~~~~~~~~~~~~~~~~~");
    assert_eq!(err_text(&two, &[&failing_host("H"), &d], &[]), "Model validation error: 2 validation errors for Submission\nEnvironmentTemplate[0] -> environment -> hostRequirements -> amounts[0] -> min:\n\tFormat string error: hostRequirements amount min: Index 4 out of bounds for list of length 1. Resolved for Steps 'S0' and 'S1'.\n  Env.Param.L[4]\n  ~~~~~~~~~~~^~~\nJobTemplate -> steps[0]:\n\tStep 'S0': no host can satisfy the combined host requirements for amount 'amount.custom': EnvironmentTemplate[1] -> environment -> hostRequirements (Environment 'D') requires min 4, and JobTemplate -> steps[0] -> hostRequirements requires max 2. Template Schemas §3.3.3.");
}

#[test]
fn a_job_template_built_without_extensions_stops_a_step_as_with_env_params() {
    // Two ENV_PARAMS tests' templates with every extension removed in code
    // (`extensions: None`): a step 4 error stops S, so SE's failing enableIf
    // is not reported; S0's duplicate entry stops S0, and H's
    // hostRequirements are still resolved for S1.
    let none =
        |src: &str, edit: &dyn Fn(&mut openjd_model::template::JobTemplate), envs: &[&str]| {
            let mut jt = job_ok(src);
            edit(&mut jt);
            jt.extensions = None;
            let envs: Vec<EnvironmentTemplate> = envs.iter().map(|s| env_ok(s)).collect();
            let values = job_params_with(&jt, &[], &envs);
            let attached: Vec<_> = envs.iter().map(AttachedEnvironmentTemplate::new).collect();
            create_job_with_environment_templates(&jt, &attached, &values, &template_ctx(&jt))
                .expect_err("expected job creation to fail")
                .to_string()
        };
    let step_4 = format!(
        "{HEAD}parameterDefinitions: [{{name: B, type: BOOL, default: false}}, {{name: L, type: \
         'LIST[INT]', default: [1]}}]\njobEnvironments:\n- {{name: A, activation: ALWAYS, \
         variables: {{V: a}}}}\nsteps:\n- name: S\n  dependencies: [{{environment: A, enableIf: \
         \"Param.B\"}}]\n  stepEnvironments:\n  - {{name: SE, enableIf: \"Param.L[4] == 1\", \
         variables: {{V: s}}}}\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    assert_eq!(none(&step_4, &|_| {}, &[]), "Model validation error: 1 validation error for JobTemplate\nsteps[0] -> dependencies[0] -> enableIf:\n\tis false, so Step 'S' would decline Environment 'A', whose activation is ALWAYS: no Step can leave out an ALWAYS Environment (Template Schemas §3.2 item 3.2).");
    let dup = format!(
        "{HEAD}jobEnvironments:\n- {{name: E, variables: {{V: e}}}}\nsteps:\n- name: S0\n  \
         dependencies: [{{environment: E}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: S1\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let push_dup = |jt: &mut openjd_model::template::JobTemplate| {
        let deps = jt.steps[0].dependencies.as_mut().unwrap();
        let e = deps[0].clone();
        deps.push(e);
    };
    assert_eq!(none(&dup, &push_dup, &[&failing_host("H")]), "Model validation error: 2 validation errors for Submission\nJobTemplate -> steps[0] -> dependencies[1] -> environment:\n\tduplicate environment dependency 'E'.\nEnvironmentTemplate[0] -> environment -> hostRequirements -> amounts[0] -> min:\n\tFormat string error: hostRequirements amount min: Index 4 out of bounds for list of length 1. Resolved for Step 'S1'.\n  Env.Param.L[4]\n  ~~~~~~~~~~~^~~");
}
