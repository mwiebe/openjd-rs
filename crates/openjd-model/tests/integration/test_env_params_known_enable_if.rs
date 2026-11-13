// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `ENV_PARAMS`: a Step must supply the parameters without a `default` of
//! every Environment that applies to it, whatever that Environment's own
//! `enableIf` (Template Schemas §3.2 constraint 7); the `enableIf` sees only
//! concrete values when the Job is created (§3.2.2 item 2); and before then
//! an `enableIf` is known when the Expression Language's rules for unknown
//! operands give it a value (§7.6 rule 5, Expression Language §2.1.6).

use openjd_model::create_job;
use openjd_model::template::EnvironmentTemplate;
use openjd_model::{
    check_environment_templates, create_job_with_environment_templates,
    AttachedEnvironmentTemplate, CreatedJob, ModelError,
};

use crate::env_params_common::{
    assert_errors, env_ok, job_err, job_ok, job_params, job_params_with, template_ctx,
};

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
    create_job_with_environment_templates(&jt, &attached, &values, &template_ctx(&jt))
}

/// The constraint 7 message for one Step and one Job Environment.
fn missing_job(step: usize, names: &str, env: &str) -> String {
    format!(
        "steps[{step}]:\n\tmust supply, in an 'environment:' dependency, every parameter without \
         a default of each Environment that applies to it (every jobEnvironments entry with \
         activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, \
         and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 \
         constraint 7), whatever that Environment's own enableIf is; missing: {names} of \
         Environment '{env}'."
    )
}

/// The repro of the RFC 0010 FCP readiness review, finding B3: Job
/// Environment `E` with `X` without a default, and a Step with no entry.
fn repro(enable_if: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [EXPR, ENV_PARAMS]\nname: j\n\
         parameterDefinitions:\n  - {{name: F, type: BOOL, default: true}}\n\
         jobEnvironments:\n  - name: E\n    enableIf: \"{enable_if}\"\n    \
         envParameterDefinitions:\n      - {{name: X, type: STRING}}\n    \
         script:\n      actions:\n        onEnter: {{command: echo, args: [ENTER, E]}}\n\
         steps:\n  - name: A\n    script:\n      actions:\n        \
         onRun: {{command: echo, args: [TASK, A]}}\n"
    )
}

#[test]
fn a_missing_parameter_is_an_error_whatever_the_enable_if() {
    // JT-190, JT-191: the enableIf would be false or true whatever X is,
    // but A does not supply X, which is an error in A's definition.
    for enable_if in [
        "not Param.F and Env.Param.X == 'a'",
        "Env.Param.X == 'a' and false",
        "Env.Param.X == 'a' or true",
        "false",
        "Param.F",
    ] {
        job_err(&repro(enable_if), &[&missing_job(0, "'X'", "E")]);
    }
}

#[test]
fn a_supplied_parameter_is_read_as_a_concrete_value() {
    // JB-184: A supplies X, and the enableIf is evaluated with it.
    let jt = repro("Env.Param.X == 'b'").replace(
        "  - name: A\n",
        "  - name: A\n    dependencies: [{environment: E, parameters: {X: a}}]\n",
    );
    let created = create(&jt, &[], &[]).unwrap();
    assert_eq!(created.job.steps[0].environments, Some(vec![]));
    let created = create(&jt.replace("X: a", "X: b"), &[], &[]).unwrap();
    assert_eq!(created.job.steps[0].environments, None);
}

/// A Job Environment `Lic` whose `enableIf` is `enable_if`, with an INT
/// parameter `Seats` that Step A supplies as `{{Param.N}}`.
fn seats(enable_if: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         parameterDefinitions:\n- {{name: N, type: STRING, default: many}}\n\
         - {{name: Use, type: STRING, default: 'no'}}\n\
         jobEnvironments:\n- name: Lic\n  activation: ON_REQUEST\n  enableIf: \"{enable_if}\"\n  \
         envParameterDefinitions: [{{name: Seats, type: INT}}]\n  variables: {{V: x}}\n\
         steps:\n- name: A\n  dependencies: [{{environment: Lic, parameters: {{Seats: \
         \"{{{{Param.N}}}}\"}}}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    )
}

const SEATS_NOT_INT: &str = "steps[0] -> dependencies[0] -> parameters -> Seats:\n\tcould not \
                             be resolved as a value of type INT: Cannot convert 'many' to int: \
                             invalid digit found in string\n  Param.N\n  ~~~~~~^";

#[test]
fn an_enable_if_that_reads_an_unconvertible_value_is_not_evaluated() {
    // §3.2.2 item 2: it reads Seats, whose value does not convert, so it is
    // not evaluated and Lic is not disabled: the value is the error. A
    // reference counts whether or not evaluation reaches it, so `true or`
    // in front of it changes nothing, and the evaluation errors an
    // evaluation would give (`int(...)` of a non-number) are not reported.
    for enable_if in [
        "Env.Param.Seats > 0",
        "Param.Use == 'yes' and Env.Param.Seats > 0",
        "true or Env.Param.Seats > 0",
        "Env.RawParam.Seats > 0",
    ] {
        let jt = seats(enable_if);
        job_ok(&jt);
        let err = create(&jt, &[], &[]).unwrap_err();
        assert_errors(&err.to_string(), "JobTemplate", &[SEATS_NOT_INT]);
    }
    // With a value that converts, it is evaluated: `true or ...` is true
    // and Lic is entered, and `false and ...` disables it.
    let created = create(&seats("true or Env.Param.Seats > 0"), &[], &[("N", "2")]).unwrap();
    assert_eq!(created.job.steps[0].environments, None);
    let created = create(&seats("false and Env.Param.Seats > 0"), &[], &[("N", "2")]).unwrap();
    assert_eq!(created.job.steps[0].environments, Some(vec![]));
}

#[test]
fn an_enable_if_that_does_not_read_an_unconvertible_value_is_evaluated() {
    // JB-110: the enableIf reads only a Job Parameter and is false, so Lic
    // is disabled for A, and Seats is not checked.
    let jt = seats("Param.Use == 'yes'");
    let created = create(&jt, &[], &[]).unwrap();
    assert_eq!(created.job.steps[0].environments, Some(vec![]));
    // True: Lic is not disabled, and Seats is checked.
    let err = create(&jt, &[], &[("Use", "yes")]).unwrap_err();
    assert_errors(&err.to_string(), "JobTemplate", &[SEATS_NOT_INT]);
}

/// A Job Template with one `ALWAYS` Job Environment `Audit` whose
/// `enableIf` is `enable_if`, over the BOOL Job Parameter `F`.
fn always(enable_if: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         parameterDefinitions: [{{name: F, type: BOOL, default: true}}]\n\
         jobEnvironments:\n- name: Audit\n  activation: ALWAYS\n  enableIf: \"{enable_if}\"\n  \
         variables: {{V: x}}\n\
         steps:\n- name: A\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    )
}

const AUDIT_FALSE: &str = "jobEnvironments[0] -> enableIf:\n\tEnvironment 'Audit' has activation \
                           ALWAYS, so it cannot be disabled, but its enableIf is false for every \
                           Step (Template Schemas §4 item 4).";

#[test]
fn an_enable_if_is_known_at_template_validation_by_the_rules_for_unknown_operands() {
    // JT-194, JT-198, JT-200: known to be false, so rejected.
    for enable_if in [
        "Param.F and false",
        "false and Param.F",
        "not (Param.F or true)",
        "not (true or Param.F)",
        "false if Param.F else false",
        "(Param.F and false) or (Param.F and false)",
        "false",
    ] {
        job_err(&always(enable_if), &[AUDIT_FALSE]);
    }
    // JT-195, JT-199, JT-201: not known, so valid; and known true.
    for enable_if in [
        "Param.F and true",
        "Param.F or false",
        "not Param.F",
        "true if Param.F else false",
        "Param.F and Param.F",
        "Param.F or true",
        "true if Param.F else true",
        "true",
    ] {
        job_ok(&always(enable_if));
    }
    // JB-185: not known at template validation, false at job creation.
    let jt = job_ok(&always("Param.F and true"));
    let err = create_job(&jt, &job_params(&jt, &[("F", "false")]), &template_ctx(&jt)).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &[
            "jobEnvironments[0] -> enableIf:\n\tEnvironment 'Audit' has activation ALWAYS, so it \
           cannot be disabled, but its enableIf is false for Step 'A' (Template Schemas §4 item \
           4).",
        ],
    );
}

/// A Job Template whose Step A has an entry with `enable_if` for the
/// `activation` Job Environment `Lic`, which has `Product` without a
/// default; the entry supplies nothing.
fn entry(activation: &str, enable_if: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         parameterDefinitions: [{{name: F, type: BOOL, default: true}}]\n\
         jobEnvironments:\n- name: Lic\n  activation: {activation}\n  \
         envParameterDefinitions: [{{name: Product, type: STRING}}]\n  variables: {{V: x}}\n\
         steps:\n- name: A\n  dependencies: [{{environment: Lic, enableIf: \"{enable_if}\"}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    )
}

#[test]
fn an_entry_known_to_be_enabled_or_disabled_is_decided_at_template_validation() {
    // JT-196, JT-152: known to be true, so the entry requests Lic, and
    // constraint 7 rejects it.
    for enable_if in ["Param.F or true", "true", "true if Param.F else true"] {
        job_err(
            &entry("ON_REQUEST", enable_if),
            &[&missing_job(0, "'Product'", "Lic")],
        );
    }
    // JT-197: not known; disabled, it would request nothing.
    for enable_if in ["Param.F or false", "Param.F", "not Param.F"] {
        job_ok(&entry("ON_REQUEST", enable_if));
    }
    // Known to be false: it requests nothing, and declines a BY_DEFAULT
    // one, so Lic does not apply to A.
    for activation in ["ON_REQUEST", "BY_DEFAULT"] {
        job_ok(&entry(activation, "Param.F and false"));
    }
    // JT-202: known to be false for an ALWAYS one, an error.
    job_err(
        &entry("ALWAYS", "Param.F and false"),
        &[
            "steps[0] -> dependencies[0] -> enableIf:\n\tis false, so Step 'A' would decline \
             Environment 'Lic', whose activation is ALWAYS: no Step can leave out an ALWAYS \
             Environment (Template Schemas §3.2 item 3.2).",
            &format!(
                "{} (the Step's 'environment:' dependency for it is disabled by its enableIf, so \
                 it supplies nothing; Template Schemas §3.2 item 3.2).",
                missing_job(0, "'Product'", "Lic").trim_end_matches('.')
            ),
        ],
    );
}

#[test]
fn a_step_that_declines_or_does_not_request_needs_no_values() {
    // JT-192: A declines the BY_DEFAULT E, whose enableIf reads X; B
    // supplies X.
    let jt = repro("Env.Param.X == 'a'").replace(
        "  - name: A\n",
        "  - name: A\n    dependencies: [{environment: E, enableIf: false}]\n",
    ) + "  - name: B\n    dependencies: [{environment: E, parameters: {X: a}}]\n    \
         script: {actions: {onRun: {command: x}}}\n";
    job_ok(&jt);
    let created = create(&jt, &[], &[]).unwrap();
    assert_eq!(created.job.steps[0].environments, Some(vec![]));
    assert_eq!(created.job.steps[1].environments, None);
    // JT-193: an ON_REQUEST one that A does not request.
    job_ok(
        &repro("Env.Param.X == 'a'")
            .replace("  - name: E\n", "  - name: E\n    activation: ON_REQUEST\n"),
    );
}

#[test]
fn an_attached_environment_applies_whatever_its_enable_if() {
    // JB-116: an attached BY_DEFAULT Environment whose enableIf is false
    // still applies to A, which must supply Product.
    let env = "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
               environment:\n  name: Lic\n  enableIf: \"Param.F and false\"\n  \
               envParameterDefinitions: [{name: Product, type: STRING}]\n  variables: {V: x}\n";
    let env = env.replace(
        "environment:\n",
        "parameterDefinitions: [{name: F, type: BOOL, default: true}]\nenvironment:\n",
    );
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              steps:\n- name: A\n  script: {actions: {onRun: {command: x}}}\n";
    let missing = "JobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, \
                   every parameter without a default of each Environment that applies to it \
                   (every attached Environment with activation ALWAYS, each one with activation \
                   BY_DEFAULT that the Step does not decline, and each one with activation \
                   ON_REQUEST that the Step requests; Template Schemas §3.2.1), whatever that \
                   Environment's own enableIf is; missing: 'Product' of Environment 'Lic' \
                   (EnvironmentTemplate[0]).";
    let err = create(jt, &[&env], &[]).unwrap_err();
    assert_errors(&err.to_string(), "Submission", &[missing]);
    let jt = job_ok(jt);
    let envs = [env_ok(&env)];
    let attached: Vec<AttachedEnvironmentTemplate<'_>> =
        envs.iter().map(AttachedEnvironmentTemplate::new).collect();
    let err = check_environment_templates(&jt, &attached, &template_ctx(&jt)).unwrap_err();
    assert_errors(&err.to_string(), "Submission", &[missing]);
}
