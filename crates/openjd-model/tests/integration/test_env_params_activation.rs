// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `ENV_PARAMS`: the `ALWAYS` and `BY_DEFAULT` values of an Environment's
//! `activation` (Template Schemas §4 item 4, item 6, and §3.2.2 item 6).
//!
//! `BY_DEFAULT`, the default, is entered by every Step's Sessions except a
//! Step for which the Environment's own `enableIf` is `false`. `ALWAYS`
//! cannot be disabled: its `enableIf` being `false` for a Step is an
//! error, reported at template validation for an `enableIf` known there to
//! be `false` (the scalar or an expression such as `"false"`, §7.6 rule 5)
//! and at job creation for any other, naming the Step and the Environment.
//! Because an `ALWAYS` Environment is entered or the Job is rejected, the
//! checks made before job creation that depend on whether a Step enters
//! it (the `WRAP_ACTIONS` single-layer rule, combined host requirements)
//! treat it as entered whatever its `enableIf`. Constraint 7 and the
//! unreachable attachment's error do not depend on any Environment's
//! `enableIf` (§3.2 constraint 7).

use openjd_model::job::{Document, EnvironmentRef, Job};
use openjd_model::template::EnvironmentTemplate;
use openjd_model::{
    check_environment_templates, create_job_with_environment_templates,
    AttachedEnvironmentTemplate, CreatedJob, ModelError,
};

use crate::env_params_common::{
    assert_errors, env_err, env_ok, job_err, job_ok, job_params_with, rendered_values, template_ctx,
};

/// Create the Job from `jt_src` with `env_srcs` attached, in order.
fn create(jt_src: &str, env_srcs: &[&str]) -> Result<CreatedJob, ModelError> {
    let jt = job_ok(jt_src);
    let envs: Vec<EnvironmentTemplate> = env_srcs.iter().map(|s| env_ok(s)).collect();
    let values = job_params_with(&jt, &[], &envs);
    let attached: Vec<AttachedEnvironmentTemplate<'_>> =
        envs.iter().map(AttachedEnvironmentTemplate::new).collect();
    create_job_with_environment_templates(&jt, &attached, &values, &template_ctx(&jt))
}

fn create_ok(jt_src: &str, env_srcs: &[&str]) -> CreatedJob {
    create(jt_src, env_srcs).unwrap_or_else(|e| panic!("job creation failed:\n{e}"))
}

fn create_err(jt_src: &str, env_srcs: &[&str], model: &str, expected: &[&str]) {
    let err = create(jt_src, env_srcs).expect_err("expected job creation to fail");
    assert_errors(&err.to_string(), model, expected);
}

/// Run `check_environment_templates` for `jt_src` with `env_srcs`.
fn check(jt_src: &str, env_srcs: &[&str]) -> Result<(), ModelError> {
    let jt = job_ok(jt_src);
    let envs: Vec<EnvironmentTemplate> = env_srcs.iter().map(|s| env_ok(s)).collect();
    let attached: Vec<AttachedEnvironmentTemplate<'_>> =
        envs.iter().map(AttachedEnvironmentTemplate::new).collect();
    check_environment_templates(&jt, &attached, &template_ctx(&jt)).map(|_| ())
}

/// The names of the Job Environments Step `step`'s Sessions enter.
fn entered(job: &Job, step: usize) -> Vec<String> {
    job.job_environments
        .iter()
        .flatten()
        .filter(|e| job.steps[step].enters_environment(&Document::JobTemplate, &e.name))
        .map(|e| e.name.clone())
        .collect()
}

/// The template-validation error for an `ALWAYS` Environment at `path`
/// whose `enableIf` is the literal `false`.
fn literal_false(path: &str, env: &str) -> String {
    format!(
        "{path} -> enableIf:\n\tEnvironment '{env}' has activation ALWAYS, so it cannot be \
         disabled, but its enableIf is false for every Step (Template Schemas §4 item 4)."
    )
}

/// The job-creation error for an `ALWAYS` Environment at `path` whose
/// `enableIf` is `false` for `steps`.
fn false_for(path: &str, env: &str, steps: &str) -> String {
    format!(
        "{path} -> enableIf:\n\tEnvironment '{env}' has activation ALWAYS, so it cannot be \
         disabled, but its enableIf is false for {steps} (Template Schemas §4 item 4)."
    )
}

// ── Template validation ─────────────────────────────────────────────

/// A Job Template with one Job Environment `Lic` of `activation`, with
/// `enable_if`, and one Step.
fn one_env(activation: &str, enable_if: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         jobEnvironments:\n- {{name: Lic, activation: {activation}, enableIf: {enable_if}, \
         variables: {{V: x}}}}\n\
         steps:\n- name: A\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    )
}

#[test]
fn an_always_environment_with_a_literal_false_enable_if_is_rejected() {
    // JT-170, ET-44: a literal `false` is known at template validation
    // (§7.6 rule 5).
    job_err(
        &one_env("ALWAYS", "false"),
        &[&literal_false("jobEnvironments[0]", "Lic")],
    );
    env_err(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
         environment: {name: Lic, activation: ALWAYS, enableIf: false, variables: {V: x}}\n",
        &[&literal_false("environment", "Lic")],
    );
}

#[test]
fn a_literal_true_or_an_expression_enable_if_on_always_is_valid() {
    // JT-172: an expression that is not known at template validation is
    // decided when the Job is created, and a `true` result is not an
    // error.
    job_ok(&one_env("ALWAYS", "true"));
    job_ok(&one_env("ALWAYS", "\"Job.Name == 'J'\""));
    // A constant expression is known at template validation (§7.6 rule
    // 5): "true" is valid, and "false" is rejected as the scalar is.
    job_ok(&one_env("ALWAYS", "\"true\""));
    job_err(
        &one_env("ALWAYS", "\"false\""),
        &[&literal_false("jobEnvironments[0]", "Lic")],
    );
    env_ok(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
         environment: {name: Lic, activation: ALWAYS, enableIf: \"Job.Name != ''\", \
         variables: {V: x}}\n",
    );
}

#[test]
fn by_default_and_on_request_may_have_a_literal_false_enable_if() {
    // JT-171, ET-45: only ALWAYS cannot be disabled.
    job_ok(&one_env("BY_DEFAULT", "false"));
    job_ok(&one_env("ON_REQUEST", "false"));
    env_ok(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
         environment: {name: Lic, activation: BY_DEFAULT, enableIf: false, variables: {V: x}}\n",
    );
}

/// A Job Environment `Lic` of `activation` whose `enableIf` is an
/// expression over a Job Parameter, with a parameter without a `default`,
/// and a Step that does not supply it.
fn required_unsupplied(activation: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         parameterDefinitions: [{{name: UseLic, type: STRING, default: 'no'}}]\n\
         jobEnvironments:\n- name: Lic\n  activation: {activation}\n  \
         envParameterDefinitions: [{{name: Product, type: STRING}}]\n  \
         enableIf: \"Param.UseLic == 'yes'\"\n  variables: {{V: x}}\n\
         steps:\n- name: A\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    )
}

#[test]
fn constraint_7_treats_an_always_environment_as_entered_at_template_validation() {
    // §3.2.2 item 6: an ALWAYS Environment is entered by every Step's
    // Sessions or the Job is rejected, so the missing parameter is certain
    // whatever the enableIf resolves to.
    job_err(
        &required_unsupplied("ALWAYS"),
        &[
            "steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a \
           default of each Environment that applies to it (every jobEnvironments entry with activation ALWAYS, each one with \
           activation BY_DEFAULT that the Step does not decline, and each one \
           with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7), whatever that Environment's own enableIf is; missing: 'Product' of \
           Environment 'Lic'.",
        ],
    );
    // BY_DEFAULT: the enableIf may disable Lic for the Step, but Lic
    // applies to it whatever the enableIf, so the missing parameter is as
    // certain (§3.2 constraint 7).
    job_err(
        &required_unsupplied("BY_DEFAULT"),
        &[
            "steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a \
           default of each Environment that applies to it (every jobEnvironments entry with \
           activation ALWAYS, each one with activation BY_DEFAULT that the Step does not \
           decline, and each one with activation ON_REQUEST that the Step requests; Template \
           Schemas §3.2 constraint 7), whatever that Environment's own enableIf is; missing: \
           'Product' of Environment 'Lic'.",
        ],
    );
}

/// Two wrapping Job Environments of `activation`, each with an
/// expression `enableIf`.
fn two_wrappers(activation: &str) -> String {
    let wrapper = |name: &str| {
        format!(
            "- name: {name}\n  activation: {activation}\n  enableIf: \"Job.Name == 'J'\"\n  \
             script:\n    actions:\n      onWrapEnvEnter: {{command: x}}\n      \
             onWrapTaskRun: {{command: x}}\n      onWrapEnvExit: {{command: x}}\n"
        )
    };
    format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR, WRAP_ACTIONS]\n\
         name: J\njobEnvironments:\n{}{}\
         steps:\n- name: A\n  script: {{actions: {{onRun: {{command: x}}}}}}\n",
        wrapper("Outer"),
        wrapper("Inner")
    )
}

#[test]
fn the_single_layer_rule_treats_an_always_environment_as_entered() {
    job_err(
        &two_wrappers("ALWAYS"),
        &[
            "jobEnvironments:\n\tonly one environment in the session stack may define any of \
           onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit (RFC 0008).",
        ],
    );
    // BY_DEFAULT: either enableIf may be false, so template validation
    // leaves the rule to job creation.
    job_ok(&two_wrappers("BY_DEFAULT"));
}

// ── Job creation ────────────────────────────────────────────────────

/// JB-128 and JB-129: a Job Environment `Lic` of `activation` whose
/// `enableIf` is false for the `default` none; Step A supplies arnold, and
/// Steps B and C take the default.
fn per_step(activation: &str, default: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         jobEnvironments:\n- name: Lic\n  activation: {activation}\n  \
         envParameterDefinitions: [{{name: Product, type: STRING, default: {default}}}]\n  \
         enableIf: \"Env.Param.Product != 'none'\"\n  variables: {{V: x}}\n\
         steps:\n\
         - name: A\n  dependencies: [{{environment: Lic, parameters: {{Product: arnold}}}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: B\n  script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: C\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    )
}

#[test]
fn an_always_environment_whose_enable_if_is_false_for_a_step_rejects_the_job() {
    // JB-128: one error for the Environment, naming every Step it is
    // false for.
    create_err(
        &per_step("ALWAYS", "none"),
        &[],
        "JobTemplate",
        &[&false_for("jobEnvironments[0]", "Lic", "Steps 'B' and 'C'")],
    );
    // One Step.
    let one = per_step("ALWAYS", "none").replace(
        "- name: C\n  script",
        "- name: C\n  dependencies: [{environment: Lic, parameters: {Product: maya}}]\n  script",
    );
    create_err(
        &one,
        &[],
        "JobTemplate",
        &[&false_for("jobEnvironments[0]", "Lic", "Step 'B'")],
    );
}

#[test]
fn an_always_environment_whose_enable_if_is_true_for_every_step_is_entered_by_all() {
    // JB-129.
    let created = create_ok(&per_step("ALWAYS", "maya"), &[]);
    for step in 0..3 {
        assert_eq!(entered(&created.job, step), ["Lic"]);
    }
    assert_eq!(
        rendered_values(&created.job.steps[0]),
        ["JobTemplate/Lic: Product=string:arnold"]
    );
    assert_eq!(
        rendered_values(&created.job.steps[1]),
        ["JobTemplate/Lic: Product=string:maya"]
    );
}

#[test]
fn a_by_default_environment_leaves_out_a_step_its_enable_if_disables() {
    // JB-130: an explicit BY_DEFAULT behaves as an omitted activation.
    for src in [
        per_step("BY_DEFAULT", "none"),
        per_step("BY_DEFAULT", "none").replace("  activation: BY_DEFAULT\n", ""),
    ] {
        let created = create_ok(&src, &[]);
        assert_eq!(entered(&created.job, 0), ["Lic"]);
        assert_eq!(entered(&created.job, 1), Vec::<String>::new());
        assert_eq!(entered(&created.job, 2), Vec::<String>::new());
        assert_eq!(
            created.disabled_environments[1].environments,
            [EnvironmentRef {
                document: Document::JobTemplate,
                environment: "Lic".to_string(),
            }]
        );
    }
}

#[test]
fn an_always_environment_disabled_for_a_step_is_still_checked_as_entered() {
    // B takes Product's default none, for which the enableIf is false, and
    // Seats's default, which resolves to `many` and does not convert to
    // INT. With ALWAYS the Job is rejected for B either way, and the failed
    // default is reported with it rather than hidden by the enableIf.
    let src = |activation: &str| {
        per_step(activation, "none")
            .replace(
                "name: J\n",
                "name: J\nparameterDefinitions: [{name: S, type: STRING, default: many}]\n",
            )
            .replace(
                "[{name: Product, type: STRING, default: none}]",
                "[{name: Product, type: STRING, default: none}, \
                 {name: Seats, type: INT, default: \"{{Param.S}}\"}]",
            )
            .replace("{Product: arnold}", "{Product: arnold, Seats: 2}")
            .replace(
                "- name: C\n  script: {actions: {onRun: {command: x}}}\n",
                "",
            )
    };
    create_err(
        &src("ALWAYS"),
        &[],
        "JobTemplate",
        &[
            "jobEnvironments[0] -> envParameterDefinitions[1] -> default:\n\tfor Step 'B', which \
             does not supply this parameter: could not be resolved as a value of type INT: Cannot \
             convert 'many' to int: invalid digit found in string\n  Param.S\n  ~~~~~~^",
            &false_for("jobEnvironments[0]", "Lic", "Step 'B'"),
        ],
    );
    // BY_DEFAULT: Lic is disabled for B, so its default is not checked.
    let created = create_ok(&src("BY_DEFAULT"), &[]);
    assert_eq!(entered(&created.job, 1), Vec::<String>::new());
}

// ── Attached Environments ───────────────────────────────────────────

/// JB-131: Step A supplies Lic's Product, Step B takes the default.
const STEPS_FOR_ATTACHED: &str = "specificationVersion: jobtemplate-2023-09\n\
    extensions: [ENV_PARAMS, EXPR]\nname: J\nsteps:\n\
    - name: A\n  dependencies: [{environment: Lic, parameters: {Product: arnold}}]\n  \
    script: {actions: {onRun: {command: x}}}\n\
    - name: B\n  script: {actions: {onRun: {command: x}}}\n";

fn attached(activation: &str) -> String {
    format!(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
         environment:\n  name: Lic\n  activation: {activation}\n  \
         envParameterDefinitions: [{{name: Product, type: STRING, default: none}}]\n  \
         enableIf: \"Env.Param.Product != 'none'\"\n  variables: {{V: x}}\n"
    )
}

#[test]
fn an_attached_always_environment_whose_enable_if_is_false_for_a_step_rejects_the_job() {
    create_err(
        STEPS_FOR_ATTACHED,
        &[&attached("ALWAYS")],
        "Submission",
        &[&false_for(
            "EnvironmentTemplate[0] -> environment",
            "Lic",
            "Step 'B'",
        )],
    );
    // BY_DEFAULT leaves B out.
    let created = create_ok(STEPS_FOR_ATTACHED, &[&attached("BY_DEFAULT")]);
    let lic = EnvironmentRef {
        document: Document::environment_template(0, None),
        environment: "Lic".to_string(),
    };
    assert_eq!(created.job.steps[0].environments, None);
    assert_eq!(created.job.steps[1].environments, Some(vec![]));
    assert_eq!(created.disabled_environments[1].environments, [lic]);
}

/// An attachment named like the Job Template's Job Environment `Lic`, so
/// unreachable (§3.2.1 item 4), with a parameter without a `default` and an
/// `enableIf` that resolves `false`.
fn unreachable(activation: &str) -> String {
    format!(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
         parameterDefinitions: [{{name: UseLic, type: STRING, default: 'no'}}]\n\
         environment:\n  name: Lic\n  activation: {activation}\n  \
         envParameterDefinitions: [{{name: Product, type: STRING}}]\n  \
         enableIf: \"Param.UseLic == 'yes'\"\n  variables: {{V: x}}\n"
    )
}

const SHADOWING_JOB: &str = "specificationVersion: jobtemplate-2023-09\n\
    extensions: [ENV_PARAMS, EXPR]\nname: J\n\
    jobEnvironments:\n- {name: Lic, variables: {V: x}}\n\
    steps:\n- name: A\n  script: {actions: {onRun: {command: x}}}\n";

#[test]
fn an_unreachable_always_attachment_with_required_parameters_is_always_rejected() {
    // §3.2.1 item 4: with ALWAYS, every Step's Sessions enter it or its
    // enableIf is an error, so its parameter rejects the submission
    // whatever the enableIf resolves to.
    let unreachable_error = "EnvironmentTemplate[0] -> environment -> envParameterDefinitions:\n\t\
        Environment 'Lic' has the same name as a jobEnvironments entry of the Job Template, so no \
        'environment:' dependency can reach it (Template Schemas §3.2.1 item 4), and it has \
        parameters without a default that no Step can supply: Product. Its activation is \
        ALWAYS, so every Session enters it, and the submission is rejected (§3.2.1).";
    create_err(
        SHADOWING_JOB,
        &[&unreachable("ALWAYS")],
        "Submission",
        &[
            unreachable_error,
            &false_for("EnvironmentTemplate[0] -> environment", "Lic", "Step 'A'"),
        ],
    );
    // check_environment_templates reports it before any value is known.
    let err = check(SHADOWING_JOB, &[&unreachable("ALWAYS")]).unwrap_err();
    assert_errors(&err.to_string(), "Submission", &[unreachable_error]);
    // BY_DEFAULT, with its enableIf false (JB-123) or true (JB-124): it
    // applies to every Step whatever the enableIf (§3.2 constraint 7), so
    // it is rejected either way, and the message says so.
    let by_default = "EnvironmentTemplate[0] -> environment -> envParameterDefinitions:\n\t\
           Environment 'Lic' has the same name as a jobEnvironments entry of the Job Template, so \
           no 'environment:' dependency can reach it (Template Schemas §3.2.1 item 4), and it has \
           parameters without a default that no Step can supply: Product. Its activation is \
           BY_DEFAULT, so it applies to every Step whatever its enableIf and exclusiveGroup (§3.2 \
           constraint 7), and the submission is rejected (§3.2.1).";
    let enabled = unreachable("BY_DEFAULT").replace("default: 'no'", "default: 'yes'");
    for att in [unreachable("BY_DEFAULT"), enabled] {
        create_err(SHADOWING_JOB, &[&att], "Submission", &[by_default]);
        let err = check(SHADOWING_JOB, &[&att]).unwrap_err();
        assert_errors(&err.to_string(), "Submission", &[by_default]);
    }
}

#[test]
fn check_environment_templates_holds_an_attachment_to_constraint_7_whatever_its_enable_if() {
    // Constraint 7 for an attachment whose enableIf is an expression over
    // its own Job Parameter: certain for ALWAYS and for BY_DEFAULT, which
    // apply to the Step whatever the enableIf (§3.2 constraint 7).
    let att = |activation: &str| unreachable(activation).replace("name: Lic\n", "name: Conda\n");
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              steps:\n- name: A\n  script: {actions: {onRun: {command: x}}}\n";
    let err = check(jt, &[&att("ALWAYS")]).unwrap_err();
    assert_errors(
        &err.to_string(),
        "Submission",
        &[
            "JobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every \
           parameter without a default of each Environment that applies to it (every attached Environment with activation ALWAYS, each one with \
           activation BY_DEFAULT that the Step does not decline, and each one \
           with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1), whatever that Environment's own enableIf is; missing: 'Product' of \
           Environment 'Conda' (EnvironmentTemplate[0]).",
        ],
    );
    let err = check(jt, &[&att("BY_DEFAULT")]).unwrap_err();
    assert_errors(
        &err.to_string(),
        "Submission",
        &[
            "JobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every \
           parameter without a default of each Environment that applies to it (every attached \
           Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Step \
           does not decline, and each one with activation ON_REQUEST that the Step requests; \
           Template Schemas §3.2.1), whatever that Environment's own enableIf is; missing: \
           'Product' of Environment 'Conda' (EnvironmentTemplate[0]).",
        ],
    );
}

// ── Checks before job creation that treat ALWAYS as entered ─────────

#[test]
fn constraint_7_holds_an_always_environment_with_a_literal_false_enable_if() {
    // §3.2.2 item 6: the literal false is its own error, and the
    // Environment is still checked as entered, so its parameter without a
    // default is reported too.
    let src =
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
               jobEnvironments:\n- name: Lic\n  activation: ALWAYS\n  enableIf: false\n  \
               envParameterDefinitions: [{name: P, type: STRING}]\n  variables: {V: x}\n\
               steps:\n- name: A\n  script: {actions: {onRun: {command: x}}}\n";
    job_err(
        src,
        &[
            &literal_false("jobEnvironments[0]", "Lic"),
            "steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a \
             default of each Environment that applies to it (every jobEnvironments entry with activation ALWAYS, each one with \
             activation BY_DEFAULT that the Step does not decline, and each one \
             with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7), whatever that Environment's own enableIf is; missing: 'P' of Environment \
             'Lic'.",
        ],
    );
    // BY_DEFAULT: disabled for every Step, but it applies to Step A, which
    // must supply P whatever the enableIf (§3.2 constraint 7).
    job_err(
        &src.replace("activation: ALWAYS", "activation: BY_DEFAULT"),
        &[
            "steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a \
           default of each Environment that applies to it (every jobEnvironments entry with \
           activation ALWAYS, each one with activation BY_DEFAULT that the Step does not \
           decline, and each one with activation ON_REQUEST that the Step requests; Template \
           Schemas §3.2 constraint 7), whatever that Environment's own enableIf is; missing: \
           'P' of Environment 'Lic'.",
        ],
    );
}

#[test]
fn a_value_for_an_always_environment_is_checked_against_its_target_type() {
    // Pass 8 gives a format-string value its parameter's type as the
    // target where the Environment is certainly not disabled. A list does
    // not convert to INT.
    let src = |activation: &str| {
        format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             jobEnvironments:\n- name: Lic\n  activation: {activation}\n  \
             enableIf: \"Job.Name == 'J'\"\n  \
             envParameterDefinitions: [{{name: N, type: INT, default: 1}}]\n  variables: {{V: x}}\n\
             steps:\n- name: A\n  dependencies: [{{environment: Lic, parameters: {{N: \
             \"{{{{ [1, 2] }}}}\"}}}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    job_err(
        &src("ALWAYS"),
        &[
            "steps[0] -> dependencies[0] -> parameters -> N:\n\tFailed to parse interpolation \
           expression at [0, 12]. Cannot coerce list[int] to int\n  [1, 2]\n  ^~~~~~",
        ],
    );
    job_ok(&src("BY_DEFAULT"));
}

/// A wrapping Environment of `activation` with an expression `enableIf`,
/// for a `jobEnvironments` entry (`job: true`) or an Environment
/// Template's `environment`.
fn wrapper(name: &str, activation: &str) -> String {
    format!(
        "  name: {name}\n  activation: {activation}\n  enableIf: \"Job.Name == 'J'\"\n  \
         script:\n    actions:\n      onWrapEnvEnter: {{command: x}}\n      \
         onWrapTaskRun: {{command: x}}\n      onWrapEnvExit: {{command: x}}\n"
    )
}

fn wrapping_job(job_env: Option<&str>) -> String {
    let envs = job_env.map_or(String::new(), |activation| {
        format!("jobEnvironments:\n-{}", &wrapper("Own", activation)[1..])
    });
    format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR, WRAP_ACTIONS]\n\
         name: J\n{envs}steps:\n- name: A\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    )
}

fn wrapping_attached(activation: &str) -> String {
    format!(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR, WRAP_ACTIONS]\n\
         environment:\n{}",
        wrapper("Att", activation)
    )
}

#[test]
fn check_environment_templates_counts_always_wrappers_as_entered() {
    let layers = "JobTemplate -> steps[0]:\n\tits Sessions would enter 2 Environments that define \
                  wrap hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'Att' \
                  (EnvironmentTemplate[0]), 'Own'. A Session may have at most one Environment \
                  defining wrap hooks (RFC 0008).";
    // A literal-true BY_DEFAULT on the other side, so only the ALWAYS
    // one's expression enableIf is in question.
    let certain = |s: String| s.replace("enableIf: \"Job.Name == 'J'\"", "enableIf: true");
    // An attached ALWAYS wrapper with a certainly entered Job Environment.
    let err = check(
        &certain(wrapping_job(Some("BY_DEFAULT"))),
        &[&wrapping_attached("ALWAYS")],
    )
    .unwrap_err();
    assert_errors(&err.to_string(), "Submission", &[layers]);
    // A Job Environment ALWAYS wrapper with a certainly entered attachment.
    let err = check(
        &wrapping_job(Some("ALWAYS")),
        &[&certain(wrapping_attached("BY_DEFAULT"))],
    )
    .unwrap_err();
    assert_errors(&err.to_string(), "Submission", &[layers]);
    // BY_DEFAULT with an expression enableIf on either side: left to job
    // creation.
    check(
        &certain(wrapping_job(Some("BY_DEFAULT"))),
        &[&wrapping_attached("BY_DEFAULT")],
    )
    .unwrap();
    check(
        &wrapping_job(Some("BY_DEFAULT")),
        &[&certain(wrapping_attached("BY_DEFAULT"))],
    )
    .unwrap();
}

/// A Job Template whose Step allows at most 2 of `amount.custom`, with a
/// Job Environment `Own` of `activation` (if any) that requires at least 4
/// and has an expression `enableIf`.
fn host_job(job_env: Option<&str>) -> String {
    let envs = job_env.map_or(String::new(), |activation| {
        format!(
            "jobEnvironments:\n- name: Own\n  activation: {activation}\n  \
             enableIf: \"Job.Name == 'J'\"\n  \
             hostRequirements: {{amounts: [{{name: amount.custom, min: 4}}]}}\n  \
             variables: {{V: x}}\n"
        )
    });
    format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         {envs}steps:\n- name: A\n  hostRequirements: {{amounts: [{{name: amount.custom, max: 2}}]}}\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    )
}

fn host_attached(activation: &str) -> String {
    format!(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
         environment:\n  name: Att\n  activation: {activation}\n  enableIf: \"Job.Name == 'J'\"\n  \
         hostRequirements: {{amounts: [{{name: amount.custom, min: 4}}]}}\n  variables: {{V: x}}\n"
    )
}

#[test]
fn check_environment_templates_combines_an_always_environments_literal_host_requirements() {
    // `job` is the Job Template's path prefix: none without attachments.
    let conflict = |job: &str, source: &str, env: &str| {
        format!(
            "{job}steps[0]:\n\tStep 'A': no host can satisfy the combined host \
             requirements for amount 'amount.custom': {source} -> hostRequirements \
             (Environment '{env}') requires min 4, and {job}steps[0] -> \
             hostRequirements requires max 2. Template Schemas §3.3.3."
        )
    };
    // A Job Environment.
    let err = check(&host_job(Some("ALWAYS")), &[]).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &[&conflict("", "jobEnvironments[0]", "Own")],
    );
    check(&host_job(Some("BY_DEFAULT")), &[]).unwrap();
    // An attachment.
    let err = check(&host_job(None), &[&host_attached("ALWAYS")]).unwrap_err();
    assert_errors(
        &err.to_string(),
        "Submission",
        &[&conflict(
            "JobTemplate -> ",
            "EnvironmentTemplate[0] -> environment",
            "Att",
        )],
    );
    check(&host_job(None), &[&host_attached("BY_DEFAULT")]).unwrap();
}

#[test]
fn a_missing_parameter_an_enable_if_reads_is_reported_alike_for_always_and_by_default() {
    // Lic's enableIf reads Product, which has no default and which no Step
    // supplies. The missing value is a constraint 7 error whatever the
    // enableIf (§3.2 constraint 7), and the enableIf is not evaluated with
    // it (§3.2.2 item 2): template validation, check_environment_templates,
    // and job creation report it alike, for ALWAYS and for BY_DEFAULT.
    let missing = |prefix: &str, reason: &str, env: &str| {
        format!(
            "{prefix}steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter \
             without a default of each Environment that applies to it (every {reason} with \
             activation ALWAYS, each one with activation BY_DEFAULT that the Step does not \
             decline, and each one with activation ON_REQUEST that the Step requests; Template \
             Schemas {}), whatever that Environment's own enableIf is; missing: 'Product' of \
             Environment {env}.",
            if reason == "jobEnvironments entry" {
                "§3.2 constraint 7"
            } else {
                "§3.2.1"
            },
        )
    };
    // A Job Environment, at template validation (which then stops job
    // creation).
    let job = |activation: &str| {
        format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             jobEnvironments:\n- name: Lic\n  activation: {activation}\n  \
             envParameterDefinitions: [{{name: Product, type: STRING}}]\n  \
             enableIf: \"Env.Param.Product != 'none'\"\n  variables: {{V: x}}\n\
             steps:\n- name: A\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    for activation in ["ALWAYS", "BY_DEFAULT"] {
        job_err(
            &job(activation),
            &[&missing("", "jobEnvironments entry", "'Lic'")],
        );
    }
    // An attachment, in check_environment_templates and at job creation.
    let att = |activation: &str| {
        format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
             environment:\n  name: Lic\n  activation: {activation}\n  \
             envParameterDefinitions: [{{name: Product, type: STRING}}]\n  \
             enableIf: \"Env.Param.Product != 'none'\"\n  variables: {{V: x}}\n"
        )
    };
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              steps:\n- name: A\n  script: {actions: {onRun: {command: x}}}\n";
    for activation in ["ALWAYS", "BY_DEFAULT"] {
        let expected = missing(
            "JobTemplate -> ",
            "attached Environment",
            "'Lic' (EnvironmentTemplate[0])",
        );
        let err = check(jt, &[&att(activation)]).unwrap_err();
        assert_errors(&err.to_string(), "Submission", &[&expected]);
        create_err(jt, &[&att(activation)], "Submission", &[&expected]);
    }
}

#[test]
fn an_always_attachment_named_like_a_step_environment_says_always() {
    // §3.2.1 item 5: Step A's Step Environment hides the attachment from
    // it, and the attachment's activation decides that A's Sessions enter
    // it; the message names that activation.
    let att = |activation: &str| {
        format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
             environment:\n  name: X\n  activation: {activation}\n  \
             envParameterDefinitions: [{{name: S, type: STRING}}]\n  variables: {{V: v}}\n"
        )
    };
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              steps:\n- name: A\n  stepEnvironments: [{name: X, variables: {Q: r}}]\n  \
              script: {actions: {onRun: {command: x}}}\n";
    for activation in ["ALWAYS", "BY_DEFAULT"] {
        create_err(
            jt,
            &[&att(activation)],
            "Submission",
            &[&format!(
                "JobTemplate -> steps[0]:\n\tcannot supply the parameters without a default of \
                 Environment 'X' (EnvironmentTemplate[0]): S. It has the same name as a \
                 stepEnvironments entry of this Step, so no 'environment:' dependency of this \
                 Step can reach it (Template Schemas §3.2.1 item 5), and its activation is \
                 {activation}, so it applies to this Step (§3.2 constraint 7)."
            )],
        );
    }
}
