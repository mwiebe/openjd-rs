// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `ENV_PARAMS`: an Environment's `exclusiveGroup` (Template Schemas §4 item
//! 7).
//!
//! For each Step, of the Environments its Sessions would enter that share a
//! group and whose `enableIf` is not `false` for it, only the first in the
//! Job's Environment order (attached ones first, then the Job Template's
//! own) is entered. The others are superseded, which disables them for the
//! Step (§3.2.2): they are not held to constraint 7 and the Step's values
//! for them are not checked, though an entry's keys still are. Job creation
//! applies the rule after every `enableIf` and before those checks (§7.4.1
//! step 7). Before then, a grouped Environment is known to be entered only
//! when every earlier one of its group is known not to be (§4, after item
//! 10), so the Job Template alone, whose attachments are not known, never
//! counts a grouped Job Environment as entered.

use openjd_model::job::{AttributeRequirement, Document, EnvironmentRef, HostRequirements, Job};
use openjd_model::template::EnvironmentTemplate;
use openjd_model::{
    check_environment_templates, create_job_with_environment_templates,
    AttachedEnvironmentTemplate, CallerLimits, CreatedJob, ModelError, ValidationContext,
};

use crate::env_params_common::{
    assert_errors, env_err, env_ok, job_err, job_ok, job_params_with, posix_ctx, rendered_values,
};

/// Create the Job from `jt_src` with `env_srcs` attached, in order.
fn create(jt_src: &str, env_srcs: &[&str]) -> Result<CreatedJob, ModelError> {
    let jt = job_ok(jt_src);
    let envs: Vec<EnvironmentTemplate> = env_srcs.iter().map(|s| env_ok(s)).collect();
    let values = job_params_with(&jt, &[], &envs);
    let attached: Vec<AttachedEnvironmentTemplate<'_>> =
        envs.iter().map(AttachedEnvironmentTemplate::new).collect();
    create_job_with_environment_templates(&jt, &attached, &values, &posix_ctx(&jt))
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
    check_with(jt_src, env_srcs, |c| c)
}

/// [`check`] with the context `ctx` makes of the default one.
fn check_with(
    jt_src: &str,
    env_srcs: &[&str],
    ctx: impl Fn(ValidationContext) -> ValidationContext,
) -> Result<(), ModelError> {
    let jt = job_ok(jt_src);
    let envs: Vec<EnvironmentTemplate> = env_srcs.iter().map(|s| env_ok(s)).collect();
    let attached: Vec<AttachedEnvironmentTemplate<'_>> =
        envs.iter().map(AttachedEnvironmentTemplate::new).collect();
    check_environment_templates(&jt, &attached, &ctx(posix_ctx(&jt))).map(|_| ())
}

/// [`create`] with the context `ctx` makes of the default one.
fn create_with(
    jt_src: &str,
    env_srcs: &[&str],
    ctx: impl Fn(ValidationContext) -> ValidationContext,
) -> Result<CreatedJob, ModelError> {
    let jt = job_ok(jt_src);
    let envs: Vec<EnvironmentTemplate> = env_srcs.iter().map(|s| env_ok(s)).collect();
    let values = job_params_with(&jt, &[], &envs);
    let attached: Vec<AttachedEnvironmentTemplate<'_>> =
        envs.iter().map(AttachedEnvironmentTemplate::new).collect();
    create_job_with_environment_templates(&jt, &attached, &values, &ctx(posix_ctx(&jt)))
}

fn check_ok(jt_src: &str, env_srcs: &[&str]) {
    check(jt_src, env_srcs).unwrap_or_else(|e| panic!("check failed:\n{e}"));
}

/// `check` and job creation both reject the submission with exactly
/// `expected`, the same message.
fn check_err_same_as_create(jt_src: &str, env_srcs: &[&str], expected: &[&str]) {
    let checked = check(jt_src, env_srcs).expect_err("expected check to fail");
    assert_errors(&checked.to_string(), "Submission", expected);
    let created = create(jt_src, env_srcs).expect_err("expected job creation to fail");
    assert_eq!(checked.to_string(), created.to_string());
}

/// The Environments Step `step`'s Sessions enter, as `name` for a Job
/// Environment and `name@i` for attachment `i`.
fn entered(created: &CreatedJob, step: usize) -> Vec<String> {
    let job: &Job = &created.job;
    job.job_environments
        .iter()
        .flatten()
        .zip(&created.environment_documents)
        .filter(|(e, d)| job.steps[step].enters_environment(d, &e.name))
        .map(|(e, d)| label(d, &e.name))
        .collect()
}

/// The Environments disabled for Step `step`, labelled as in [`entered`].
fn disabled(created: &CreatedJob, step: usize) -> Vec<String> {
    created.disabled_environments[step]
        .environments
        .iter()
        .map(|r: &EnvironmentRef| label(&r.document, &r.environment))
        .collect()
}

fn label(document: &Document, name: &str) -> String {
    match document {
        Document::JobTemplate => name.to_string(),
        Document::EnvironmentTemplate { index, .. } => format!("{name}@{index}"),
    }
}

const TYPED_ERROR: &str = "steps[0] -> dependencies[0] -> parameters -> N:\n\tcould not be \
                           resolved as a value of type INT: Cannot convert 'x' to int: invalid \
                           digit found in string";

const HEAD: &str = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
                    name: J\n";
const STEP: &str = "steps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n";

/// An Environment Template whose Environment is `body`, a YAML flow
/// mapping's contents.
fn env(body: &str) -> String {
    format!(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
         environment: {{{body}, variables: {{V: x}}}}\n"
    )
}

/// The constraint 7 error for Step 0 missing `P` of `label`, for Job
/// Environments (`job`) or attached ones.
fn missing(path: &str, job: bool, label: &str) -> String {
    missing_named(path, job, label, "P")
}

/// [`missing`] for parameter `name`.
fn missing_named(path: &str, job: bool, label: &str, name: &str) -> String {
    let why = if job {
        "every jobEnvironments entry with activation ALWAYS, each one with activation \
         BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST \
         that the Step requests; Template Schemas §3.2 constraint 7"
    } else {
        "every attached Environment with activation ALWAYS, each one with activation \
         BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST \
         that the Step requests; Template Schemas §3.2.1"
    };
    format!(
        "{path}:\n\tmust supply, in an 'environment:' dependency, every parameter without a \
         default of each Environment its Sessions enter ({why}); missing: '{name}' of \
         Environment {label}."
    )
}

// ── Template validation: the field ─────────────────────────────────

#[test]
fn needs_env_params_and_is_only_on_declaring_environments() {
    let gate = "exclusiveGroup on an Environment requires the ENV_PARAMS extension.";
    job_err(
        "specificationVersion: jobtemplate-2023-09\nextensions: [EXPR]\nname: J\n\
         jobEnvironments:\n- {name: A, exclusiveGroup: G, variables: {V: x}}\n\
         steps:\n- name: S\n  stepEnvironments:\n  - {name: L, exclusiveGroup: G, variables: \
         {V: x}}\n  script: {actions: {onRun: {command: x}}}\n",
        &[
            &format!("jobEnvironments[0] -> exclusiveGroup:\n\t{gate}"),
            &format!("steps[0] -> stepEnvironments[0] -> exclusiveGroup:\n\t{gate}"),
        ],
    );
    env_err(
        "specificationVersion: environment-2023-09\n\
         environment: {name: A, exclusiveGroup: G, variables: {V: x}}\n",
        &[&format!("environment -> exclusiveGroup:\n\t{gate}")],
    );
    // JT-187: not on a stepEnvironments entry.
    job_err(
        &format!(
            "{HEAD}steps:\n- name: S\n  stepEnvironments:\n  - {{name: L, exclusiveGroup: G, \
             variables: {{V: x}}}}\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        ),
        &[
            "steps[0] -> stepEnvironments[0] -> exclusiveGroup:\n\tis not permitted on a \
           stepEnvironments entry, which only its own Step's Sessions enter.",
        ],
    );
    // ET-48: an Environment Template and a Job Environment may have one.
    env_ok(&env("name: A, exclusiveGroup: G"));
    job_ok(&format!(
        "{HEAD}jobEnvironments:\n- {{name: A, exclusiveGroup: G, variables: {{V: x}}}}\n{STEP}"
    ));
}

#[test]
fn is_an_identifier() {
    // JT-188, §7.1: `[A-Za-z_][A-Za-z0-9_]*`, at most 64 characters
    // without FEATURE_BUNDLE_1.
    let jt = |group: &str| {
        format!(
            "{HEAD}jobEnvironments:\n- {{name: A, exclusiveGroup: '{group}', variables: {{V: \
             x}}}}\n{STEP}"
        )
    };
    let bad = |group: &str| {
        format!(
            "jobEnvironments[0] -> exclusiveGroup:\n\t'{group}' is not a valid exclusiveGroup: \
             it must match [A-Za-z_][A-Za-z0-9_]* and be at most 64 characters (Template \
             Schemas §7.1)."
        )
    };
    for group in ["Software Provider", "1G", "", "G-1"] {
        job_err(&jt(group), &[&bad(group)]);
    }
    let long = "G".repeat(65);
    job_err(&jt(&long), &[&bad(&long)]);
    job_ok(&jt(&"G".repeat(64)));
    job_ok(&jt("_g1"));
}

#[test]
fn is_not_allowed_on_an_always_environment() {
    // ET-49, constraint 1.
    let msg = |path: &str| {
        format!(
            "{path} -> exclusiveGroup:\n\tEnvironment 'A' has activation ALWAYS, which cannot \
             be disabled, so it must not have an exclusiveGroup (Template Schemas §4 item 7 \
             constraint 1)."
        )
    };
    env_err(
        &env("name: A, activation: ALWAYS, exclusiveGroup: G"),
        &[&msg("environment")],
    );
    job_err(
        &format!(
            "{HEAD}jobEnvironments:\n- {{name: A, activation: ALWAYS, exclusiveGroup: G, \
             variables: {{V: x}}}}\n{STEP}"
        ),
        &[&msg("jobEnvironments[0]")],
    );
    for activation in ["BY_DEFAULT", "ON_REQUEST"] {
        env_ok(&env(&format!(
            "name: A, activation: {activation}, exclusiveGroup: G"
        )));
    }
}

// ── Job creation: Job Environments ─────────────────────────────────

/// Job Environments `A` and `B` in group `G` (when `a_group` and `b_group`
/// are set), `B` with a parameter `P` without a `default`, and `A` with
/// `enableIf: a_enable_if`.
fn a_and_b(a_group: &str, b_group: &str, a_enable_if: &str) -> String {
    format!(
        "{HEAD}jobEnvironments:\n\
         - {{name: A, exclusiveGroup: {a_group}, enableIf: {a_enable_if}, variables: {{V: a}}}}\n\
         - {{name: B, exclusiveGroup: {b_group}, envParameterDefinitions: [{{name: P, type: \
         STRING}}], variables: {{V: b}}}}\n{STEP}"
    )
}

#[test]
fn only_the_first_job_environment_of_a_group_is_entered() {
    // JB-152: B is superseded, so it is disabled for the Step and is not
    // held to constraint 7.
    let jt = a_and_b("G", "G", "true");
    let created = create_ok(&jt, &[]);
    assert_eq!(entered(&created, 0), ["A"]);
    assert_eq!(disabled(&created, 0), ["B"]);
    assert_eq!(created.job.steps[0].environment_parameters, None);
    // JB-153: different groups are both entered, and B needs its value.
    create_err(
        &a_and_b("G", "H", "true"),
        &[],
        "JobTemplate",
        &[&missing("steps[0]", true, "'B'")],
    );
}

#[test]
fn a_disabled_job_environment_does_not_supersede() {
    // §4 item 7: the group is applied after each one's enableIf, so B is
    // the group's first for the Step, and is held to constraint 7.
    for a_enable_if in ["false", "\"Job.Name == 'other'\""] {
        create_err(
            &a_and_b("G", "G", a_enable_if),
            &[],
            "JobTemplate",
            &[&missing("steps[0]", true, "'B'")],
        );
    }
}

#[test]
fn template_validation_never_counts_a_grouped_job_environment_as_entered() {
    // Without the Environment Templates, an attached one of its group may
    // come first, so neither constraint 7 nor a value check is made for a
    // grouped Job Environment until the Job is created (§4, after item 10).
    // B is the group's first, so job creation reports it.
    let first = a_and_b("H", "G", "true");
    job_ok(&first);
    create_err(
        &first,
        &[],
        "JobTemplate",
        &[&missing("steps[0]", true, "'B'")],
    );
    // A literal value of the wrong type for a grouped Environment is
    // checked at job creation too, where it is entered.
    let typed = format!(
        "{HEAD}jobEnvironments:\n- {{name: A, exclusiveGroup: G, envParameterDefinitions: \
         [{{name: N, type: INT}}], variables: {{V: a}}}}\n\
         steps:\n- name: S\n  dependencies: [{{environment: A, parameters: {{N: '{{{{\"x\"}}}}'}}}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    job_ok(&typed);
    create_err(&typed, &[], "JobTemplate", &[TYPED_ERROR]);
    // Ungrouped, the same mistake is reported at template validation.
    job_err(
        &first.replace("exclusiveGroup: G, ", ""),
        &[&missing("steps[0]", true, "'B'")],
    );
}

#[test]
fn values_of_a_superseded_job_environment_are_not_checked_but_its_keys_are() {
    // §3.2.2: a superseded Environment is disabled for the Step, so its
    // values are not checked (constraint 6), but constraint 5 still
    // applies to the entry.
    let jt = |params: &str| {
        format!(
            "{HEAD}jobEnvironments:\n- {{name: A, exclusiveGroup: G, variables: {{V: a}}}}\n\
             - {{name: B, exclusiveGroup: G, envParameterDefinitions: [{{name: N, type: INT, \
             minValue: 5}}], variables: {{V: b}}}}\n\
             steps:\n- name: S\n  dependencies: [{{environment: B, parameters: {params}}}]\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    let created = create_ok(&jt("{N: '{{1}}'}"), &[]);
    assert_eq!(entered(&created, 0), ["A"]);
    let created = create_ok(&jt("{N: 1}"), &[]);
    assert_eq!(entered(&created, 0), ["A"]);
    job_err(
        &jt("{Q: 1}"),
        &[
            "steps[0] -> dependencies[0] -> parameters -> Q:\n\tEnvironment 'B' declares no \
           parameter 'Q'. Declared: N.",
        ],
    );
}

#[test]
fn each_step_decides_its_own_group() {
    // A's enableIf reads its parameter, so it is enabled for one Step and
    // disabled for the other, which then enters B.
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: A, exclusiveGroup: G, envParameterDefinitions: \
         [{{name: Use, type: STRING, default: 'yes'}}], enableIf: \"Env.Param.Use == 'yes'\", \
         variables: {{V: a}}}}\n- {{name: B, exclusiveGroup: G, variables: {{V: b}}}}\n\
         steps:\n- name: S1\n  script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: S2\n  dependencies: [{{environment: A, parameters: {{Use: 'no'}}}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let created = create_ok(&jt, &[]);
    assert_eq!(entered(&created, 0), ["A"]);
    assert_eq!(disabled(&created, 0), ["B"]);
    assert_eq!(entered(&created, 1), ["B"]);
    assert_eq!(disabled(&created, 1), ["A"]);
}

// ── Job creation: attached Environments ────────────────────────────

const CONDA: &str = "name: Conda, activation: ON_REQUEST, exclusiveGroup: SoftwareProvider, \
                     envParameterDefinitions: [{name: Packages, type: STRING, default: ''}], \
                     enableIf: \"Env.Param.Packages != ''\"";
const DOCKER: &str = "name: Docker, activation: ON_REQUEST, exclusiveGroup: SoftwareProvider, \
                      envParameterDefinitions: [{name: Image, type: STRING}]";

/// A Step with optional entries for Conda (with `packages`) and Docker.
fn conda_and_docker(packages: &str) -> String {
    format!(
        "{HEAD}steps:\n- name: S\n  dependencies:\n  - {{environment: Conda, required: false, \
         parameters: {{Packages: '{packages}'}}}}\n  - {{environment: Docker, required: false, \
         parameters: {{Image: img}}}}\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    )
}

#[test]
fn the_first_attached_environment_of_a_group_is_entered() {
    // JB-148, JB-149: the order of attachment decides.
    let (conda, docker) = (env(CONDA), env(DOCKER));
    let jt = conda_and_docker("colmap");
    let created = create_ok(&jt, &[&conda, &docker]);
    assert_eq!(entered(&created, 0), ["Conda@0"]);
    assert_eq!(disabled(&created, 0), ["Docker@1"]);
    assert_eq!(
        rendered_values(&created.job.steps[0]),
        ["EnvironmentTemplate[0]/Conda: Packages=string:colmap"]
    );
    let created = create_ok(&jt, &[&docker, &conda]);
    assert_eq!(entered(&created, 0), ["Docker@0"]);
    assert_eq!(disabled(&created, 0), ["Conda@1"]);
    // JB-150: Conda's enableIf is false for the Step, so it enters Docker.
    let created = create_ok(&conda_and_docker(""), &[&conda, &docker]);
    assert_eq!(entered(&created, 0), ["Docker@1"]);
    assert_eq!(disabled(&created, 0), ["Conda@0"]);
}

#[test]
fn a_superseded_attachment_is_not_held_to_constraint_7_or_its_values() {
    // JB-151: Docker's Image has no default and the Step does not supply
    // it, but Docker is superseded. A value Conda's definitions would
    // reject is also not checked for a superseded one.
    let by_default = |s: &str| s.replace("activation: ON_REQUEST, ", "");
    let jt = format!(
        "{HEAD}steps:\n- name: S\n  dependencies: [{{environment: Conda, parameters: {{Packages: \
         p}}}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let (conda, docker) = (by_default(&env(CONDA)), by_default(&env(DOCKER)));
    let created = create_ok(&jt, &[&conda, &docker]);
    assert_eq!(entered(&created, 0), ["Conda@0"]);
    check_ok(&jt, &[&conda, &docker]);
    // With Docker first, it is entered and its Image is required.
    check_err_same_as_create(
        &jt,
        &[&docker, &conda],
        &[&missing_named(
            "JobTemplate -> steps[0]",
            false,
            "'Docker' (EnvironmentTemplate[0])",
            "Image",
        )],
    );
    // An entry's keys are still checked for a superseded attachment.
    let jt = conda_and_docker("p").replace("Image: img", "Tag: img");
    let undeclared = "JobTemplate -> steps[0] -> dependencies[1] -> parameters -> Tag:\n\t\
                      Environment 'Docker' (EnvironmentTemplate[1]) declares no parameter 'Tag'. \
                      Declared: Image.";
    check_err_same_as_create(&jt, &[&env(CONDA), &env(DOCKER)], &[undeclared]);
}

#[test]
fn an_attached_environment_supersedes_a_job_environment_of_its_group() {
    // §1.1 item 7: the attached Environments come first in the Job's
    // Environment order, so they supersede the Job Template's own. B has a
    // parameter without a default that no Step supplies.
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: B, exclusiveGroup: G, envParameterDefinitions: \
         [{{name: P, type: STRING}}], variables: {{V: b}}}}\n{STEP}"
    );
    let x = |enable_if: &str| {
        env(&format!(
            "name: X, exclusiveGroup: G, enableIf: {enable_if}"
        ))
    };
    let created = create_ok(&jt, &[&x("true")]);
    assert_eq!(entered(&created, 0), ["X@0"]);
    assert_eq!(disabled(&created, 0), ["B"]);
    // X disabled, as a literal or by an expression: B is the group's first
    // and needs its value, reported at the Job Template's path.
    for enable_if in ["false", "\"Job.Name == 'other'\""] {
        create_err(
            &jt,
            &[&x(enable_if)],
            "Submission",
            &[&missing("JobTemplate -> steps[0]", true, "'B'")],
        );
    }
    // An attachment in another group does not, and B's mistake is then
    // reported by job creation from the Job Template alone, before the
    // attachments are applied.
    create_err(
        &jt,
        &[&env("name: X, exclusiveGroup: H")],
        "JobTemplate",
        &[&missing("steps[0]", true, "'B'")],
    );
}

#[test]
fn a_job_environment_left_for_the_submission_stage_keeps_its_place() {
    // B shares a group with the attachment X, so the submission stage
    // decides it; its values stay in written order among the Job
    // Environments', and C, superseded by B, is not entered.
    let jt = format!(
        "{HEAD}jobEnvironments:\n\
         - {{name: B, exclusiveGroup: G, envParameterDefinitions: [{{name: P, type: STRING, \
         default: b}}], variables: {{V: b}}}}\n\
         - {{name: C, exclusiveGroup: G, envParameterDefinitions: [{{name: P, type: STRING}}], \
         variables: {{V: c}}}}\n\
         - {{name: D, envParameterDefinitions: [{{name: P, type: STRING, default: d}}], \
         variables: {{V: d}}}}\n{STEP}"
    );
    let x = env("name: X, exclusiveGroup: G, enableIf: false");
    let created = create_ok(&jt, &[&x]);
    assert_eq!(entered(&created, 0), ["B", "D"]);
    assert_eq!(disabled(&created, 0), ["X@0", "C"]);
    assert_eq!(
        rendered_values(&created.job.steps[0]),
        ["JobTemplate/B: P=string:b", "JobTemplate/D: P=string:d"]
    );
    let created = create_ok(&jt, &[&env("name: X, exclusiveGroup: G")]);
    assert_eq!(entered(&created, 0), ["X@0", "D"]);
    assert_eq!(disabled(&created, 0), ["B", "C"]);
    assert_eq!(
        rendered_values(&created.job.steps[0]),
        ["JobTemplate/D: P=string:d"]
    );
}

#[test]
fn a_superseded_environments_host_requirements_are_not_combined() {
    let jt = conda_and_docker("p");
    let docker = env(&format!(
        "{DOCKER}, hostRequirements: {{attributes: [{{name: attr.worker.os.family, anyOf: \
         [linux]}}]}}"
    ));
    let created = create_ok(&jt, &[&env(CONDA), &docker]);
    assert_eq!(created.job.steps[0].host_requirements, None);
    let created = create_ok(&jt, &[&docker, &env(CONDA)]);
    assert_eq!(
        created.job.steps[0].host_requirements,
        Some(HostRequirements {
            amounts: None,
            attributes: Some(vec![AttributeRequirement {
                name: "attr.worker.os.family".to_string(),
                any_of: Some(vec!["linux".to_string()]),
                all_of: None,
            }]),
        })
    );
}

#[test]
fn a_superseded_unreachable_attachment_is_not_an_error() {
    // U is named like a Job Environment, so no entry reaches it; it has a
    // parameter without a default. Superseded by X for every Step, it is
    // not entered (§3.2.2), so neither check nor job creation reports it.
    let jt = format!("{HEAD}jobEnvironments:\n- {{name: U, variables: {{V: u}}}}\n{STEP}");
    let u = env("name: U, exclusiveGroup: G, envParameterDefinitions: [{name: P, type: STRING}]");
    let x = env("name: X, exclusiveGroup: G");
    create_ok(&jt, &[&x, &u]);
    check_ok(&jt, &[&x, &u]);
    // First of its group, it is entered.
    let unreachable = "EnvironmentTemplate[0] -> environment -> envParameterDefinitions:\n\t\
                       Environment 'U' has the same name as a jobEnvironments entry of the Job \
                       Template, so no 'environment:' dependency can reach it (Template Schemas \
                       §3.2.1 item 4), and it has parameters without a default that no Step can \
                       supply: P. Its activation is BY_DEFAULT and an earlier Environment of its \
                       exclusiveGroup does not supersede it, so Sessions enter it, and the \
                       submission is rejected (§3.2.1).";
    check_err_same_as_create(&jt, &[&u, &x], &[unreachable]);
}

// ── Before job creation: check_environment_templates ───────────────

#[test]
fn check_holds_only_the_known_first_of_a_group_to_constraint_7() {
    let jt = format!("{HEAD}{STEP}");
    let a = |enable_if: &str| {
        env(&format!(
            "name: A, exclusiveGroup: G, enableIf: {enable_if}, envParameterDefinitions: \
             [{{name: P, type: STRING}}]"
        ))
    };
    let b = env("name: B, exclusiveGroup: G, envParameterDefinitions: [{name: P, type: STRING}]");
    // A is known to be entered, so B is known not to be.
    check_err_same_as_create(
        &jt,
        &[&a("true"), &b],
        &[&missing(
            "JobTemplate -> steps[0]",
            false,
            "'A' (EnvironmentTemplate[0])",
        )],
    );
    // A is known not to be entered, so B is the group's first.
    check_err_same_as_create(
        &jt,
        &[&a("false"), &b],
        &[&missing(
            "JobTemplate -> steps[0]",
            false,
            "'B' (EnvironmentTemplate[1])",
        )],
    );
    // A is undetermined, so neither is known to be entered.
    let undetermined = a("\"Job.Name == 'J'\"");
    check_ok(&jt, &[&undetermined, &b]);
    create_err(
        &jt,
        &[&undetermined, &b],
        "Submission",
        &[&missing(
            "JobTemplate -> steps[0]",
            false,
            "'A' (EnvironmentTemplate[0])",
        )],
    );
}

const WRAP: &str = "script: {actions: {onWrapEnvEnter: {command: x}, onWrapTaskRun: {command: \
                    x}, onWrapEnvExit: {command: x}}}";

fn wrapping_env(body: &str) -> String {
    format!(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, EXPR]\n\
         environment: {{{body}, {WRAP}}}\n"
    )
}

#[test]
fn the_single_layer_rule_counts_only_the_first_of_a_group() {
    let jt = format!("{HEAD}{STEP}");
    // Two wrapping attachments in one group: only one is entered.
    let (a, b) = (
        wrapping_env("name: A, exclusiveGroup: G"),
        wrapping_env("name: B, exclusiveGroup: G"),
    );
    check_ok(&jt, &[&a, &b]);
    let created = create_ok(&jt, &[&a, &b]);
    assert_eq!(entered(&created, 0), ["A@0"]);
    // In two groups, both are.
    let c = wrapping_env("name: C, exclusiveGroup: H");
    let layers = "JobTemplate -> steps[0]:\n\tits Sessions would enter 2 Environments that \
                  define wrap hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'A' \
                  (EnvironmentTemplate[0]), 'C' (EnvironmentTemplate[1]). A Session may have at \
                  most one Environment defining wrap hooks (RFC 0008).";
    check_err_same_as_create(&jt, &[&a, &c], &[layers]);
    // Two wrapping Job Environments in one group: template validation
    // counts neither, and job creation enters only the first.
    let jt = format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, \
         EXPR]\nname: J\njobEnvironments:\n- {{name: A, exclusiveGroup: G, {WRAP}}}\n\
         - {{name: B, exclusiveGroup: G, {WRAP}}}\n{STEP}"
    );
    let created = create_ok(&jt, &[]);
    assert_eq!(entered(&created, 0), ["A"]);
    // One grouped and one not: template validation counts only the
    // ungrouped one, and job creation finds both.
    let jt = jt.replace("name: B, exclusiveGroup: G, ", "name: B, ");
    job_ok(&jt);
    create_err(
        &jt,
        &[],
        "Submission",
        &[
            "JobTemplate -> steps[0]:\n\tits Sessions would enter 2 Environments that define wrap \
           hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'A', 'B'. A Session may have \
           at most one Environment defining wrap hooks (RFC 0008).",
        ],
    );
}

// ── Audit round 1: the cases the first tests left out ──────────────

#[test]
fn an_attachment_the_step_does_not_enter_does_not_supersede() {
    // Conda is ON_REQUEST and no entry requests it, so the Step's Sessions
    // would not enter it, and Docker is its group's first: its Image is
    // required, before job creation as at it.
    let jt = format!("{HEAD}{STEP}");
    let docker = env(&DOCKER.replace("activation: ON_REQUEST, ", ""));
    check_err_same_as_create(
        &jt,
        &[&env(CONDA), &docker],
        &[&missing_named(
            "JobTemplate -> steps[0]",
            false,
            "'Docker' (EnvironmentTemplate[1])",
            "Image",
        )],
    );
}

#[test]
fn check_counts_a_grouped_job_environment_after_the_attachments() {
    // W wraps and is attached; X is attached in group `x_group`; the Job
    // Template's C does not wrap and B wraps in group G. B is superseded
    // only when X shares its group, and it sits at index 1 of the Job
    // Environments.
    let jt = |x_group: &str| {
        (
            format!(
                "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, \
                 WRAP_ACTIONS, EXPR]\nname: J\njobEnvironments:\n- {{name: C, variables: {{V: \
                 c}}}}\n- {{name: B, exclusiveGroup: G, {WRAP}}}\n{STEP}"
            ),
            env(&format!("name: X, exclusiveGroup: {x_group}")),
        )
    };
    let w = wrapping_env("name: W");
    let (shared, x) = jt("G");
    check_ok(&shared, &[&w, &x]);
    let created = create_ok(&shared, &[&w, &x]);
    assert_eq!(entered(&created, 0), ["W@0", "X@1", "C"]);
    let (other, x) = jt("H");
    let layers = "JobTemplate -> steps[0]:\n\tits Sessions would enter 2 Environments that \
                  define wrap hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'W' \
                  (EnvironmentTemplate[0]), 'B'. A Session may have at most one Environment \
                  defining wrap hooks (RFC 0008).";
    check_err_same_as_create(&other, &[&w, &x], &[layers]);
}

#[test]
fn check_follows_an_earlier_member_known_not_entered_or_undetermined() {
    // A, B and C wrap; A and B are in group G. When A is known not to be
    // entered, B is its group's first, so B and C make two layers. When an
    // entry whose enableIf is an expression may request A, B is
    // undetermined, and the rule is left to job creation.
    let jt = format!("{HEAD}{STEP}");
    let (b, c) = (
        wrapping_env("name: B, exclusiveGroup: G"),
        wrapping_env("name: C, exclusiveGroup: H"),
    );
    let layers = "JobTemplate -> steps[0]:\n\tits Sessions would enter 2 Environments that \
                  define wrap hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'B' \
                  (EnvironmentTemplate[1]), 'C' (EnvironmentTemplate[2]). A Session may have \
                  at most one Environment defining wrap hooks (RFC 0008).";
    for a in [
        "name: A, exclusiveGroup: G, enableIf: false",
        "name: A, exclusiveGroup: G, activation: ON_REQUEST",
    ] {
        check_err_same_as_create(&jt, &[&wrapping_env(a), &b, &c], &[layers]);
    }
    let requests_a = format!(
        "{HEAD}parameterDefinitions: [{{name: UseA, type: STRING, default: 'no'}}]\n\
         steps:\n- name: S\n  dependencies: [{{environment: A, enableIf: \"Param.UseA == \
         'yes'\"}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let a = wrapping_env("name: A, exclusiveGroup: G, activation: ON_REQUEST");
    check_ok(&requests_a, &[&a, &b, &c]);
    create_err(&requests_a, &[&a, &b, &c], "Submission", &[layers]);
}

#[test]
fn a_job_environment_decided_by_the_submission_stage_reports_at_job_template_paths() {
    // B shares group G with the attachment X, which is disabled, so the
    // submission stage checks B's values and constraint 7, at the Job
    // Template's paths, with the jobEnvironments wording and the
    // unresolved note.
    let x = env("name: X, exclusiveGroup: G, enableIf: false");
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: B, exclusiveGroup: G, envParameterDefinitions: \
         [{{name: N, type: INT, minValue: 5}}], variables: {{V: b}}}}\n\
         steps:\n- name: S\n  dependencies: [{{environment: B, parameters: {{N: 1}}}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    check_err_same_as_create(&jt, &[&x], &[DEFERRED_VALUE_ERROR]);
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: B, exclusiveGroup: G, envParameterDefinitions: \
         [{{name: P, type: STRING}}], enableIf: \"Env.Param.P != ''\", variables: {{V: b}}}}\n\
         {STEP}"
    );
    check_err_same_as_create(
        &jt,
        &[&x],
        &[&format!(
            "{}{UNRESOLVED_NOTE}.",
            missing("JobTemplate -> steps[0]", true, "'B'").trim_end_matches('.')
        )],
    );
}

const DEFERRED_VALUE_ERROR: &str = "JobTemplate -> steps[0] -> dependencies[0] -> parameters -> \
                                    N:\n\tParameter 'N': value 1 is less than minimum 5";

const UNRESOLVED_NOTE: &str = " (its own enableIf depends on a value that is missing or does \
                               not convert, so it does not disable the Environment; Template \
                               Schemas §3.2.2)";

#[test]
fn an_unresolved_enable_if_does_not_disable_so_it_supersedes() {
    // §4 item 7: only an enableIf that is false lets the next one in. A's
    // enableIf reads its Q, which has no value, so it is unresolved and A
    // is entered: B is superseded and only A's Q is missing.
    let jt = format!(
        "{HEAD}jobEnvironments:\n\
         - {{name: A, exclusiveGroup: G, envParameterDefinitions: [{{name: Q, type: STRING}}], \
         enableIf: \"Env.Param.Q != ''\", variables: {{V: a}}}}\n\
         - {{name: B, exclusiveGroup: G, envParameterDefinitions: [{{name: P, type: STRING}}], \
         variables: {{V: b}}}}\n{STEP}"
    );
    job_ok(&jt);
    create_err(
        &jt,
        &[],
        "JobTemplate",
        &[&format!(
            "{}{UNRESOLVED_NOTE}.",
            missing_named("steps[0]", true, "'A'", "Q").trim_end_matches('.')
        )],
    );
}

#[test]
fn an_ambiguous_attachment_takes_its_group() {
    // Two attachments define Amb (§3.2.1 item 3); the entry naming it is
    // rejected. Amb would be entered, so B, later in its group, is not
    // checked either, and only the ambiguity is reported.
    let jt = format!(
        "{HEAD}steps:\n- name: S\n  dependencies: [{{environment: Amb}}]\n  script: \
         {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let amb = env("name: Amb, exclusiveGroup: G");
    let b = env("name: B, exclusiveGroup: G, envParameterDefinitions: [{name: P, type: STRING}]");
    let ambiguous = "JobTemplate -> steps[0] -> dependencies[0] -> environment:\n\tnames \
                     Environment 'Amb', which 2 attached Environment Templates define \
                     (EnvironmentTemplate[0], EnvironmentTemplate[1]); an entry must match \
                     exactly one (Template Schemas §3.2.1 item 3).";
    check_err_same_as_create(&jt, &[&amb, &amb, &b], &[ambiguous]);
}

#[test]
fn a_literal_false_attachment_an_expression_entry_names_is_not_entered() {
    // A's own enableIf is false, so it is not entered whatever the entry
    // naming it resolves to, and B is its group's first.
    let jt = format!(
        "{HEAD}parameterDefinitions: [{{name: UseA, type: STRING, default: 'yes'}}]\n\
         steps:\n- name: S\n  dependencies: [{{environment: A, enableIf: \"Param.UseA == \
         'yes'\"}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let a = env("name: A, activation: ON_REQUEST, exclusiveGroup: G, enableIf: false");
    let b = env("name: B, exclusiveGroup: G, envParameterDefinitions: [{name: P, type: STRING}]");
    check_err_same_as_create(
        &jt,
        &[&a, &b],
        &[&missing(
            "JobTemplate -> steps[0]",
            false,
            "'B' (EnvironmentTemplate[1])",
        )],
    );
}

#[test]
fn feature_bundle_1_raises_the_length_limit_to_512() {
    // §7.1: an <Identifier> may be 512 characters with FEATURE_BUNDLE_1.
    let jt = |group: &str| {
        format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR, \
             FEATURE_BUNDLE_1]\nname: J\njobEnvironments:\n- {{name: A, exclusiveGroup: \
             '{group}', variables: {{V: x}}}}\n{STEP}"
        )
    };
    job_ok(&jt(&"G".repeat(100)));
    job_ok(&jt(&"G".repeat(512)));
    let long = "G".repeat(513);
    job_err(
        &jt(&long),
        &[&format!(
            "jobEnvironments[0] -> exclusiveGroup:\n\t'{long}' is not a valid exclusiveGroup: \
             it must match [A-Za-z_][A-Za-z0-9_]* and be at most 512 characters (Template \
             Schemas §7.1)."
        )],
    );
}

#[test]
fn a_grouped_job_environments_wrap_hooks_still_need_the_extension() {
    let jt = format!("{HEAD}jobEnvironments:\n- {{name: A, exclusiveGroup: G, {WRAP}}}\n{STEP}");
    let gate = |hook: &str| {
        format!(
            "jobEnvironments[0] -> script -> actions -> {hook}:\n\t{hook} requires the \
             WRAP_ACTIONS extension."
        )
    };
    job_err(
        &jt,
        &[
            &gate("onWrapEnvEnter"),
            &gate("onWrapTaskRun"),
            &gate("onWrapEnvExit"),
        ],
    );
}

#[test]
fn check_holds_a_grouped_job_environment_first_in_its_group_to_constraint_7() {
    // With the attachments known, J is its group's first unless X, attached
    // in its group, comes first.
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: J, exclusiveGroup: G, envParameterDefinitions: \
         [{{name: N, type: INT}}], variables: {{V: j}}}}\n{STEP}"
    );
    let u = env("name: U");
    let err = check(&jt, &[&u]).expect_err("expected check to fail");
    assert_errors(
        &err.to_string(),
        "Submission",
        &[&missing_named("JobTemplate -> steps[0]", true, "'J'", "N")],
    );
    // Job creation reports it from the Job Template, since no attachment
    // shares its group.
    create_err(
        &jt,
        &[&u],
        "JobTemplate",
        &[&missing_named("steps[0]", true, "'J'", "N")],
    );
    let x = env("name: X, exclusiveGroup: G");
    check_ok(&jt, &[&x]);
    let created = create_ok(&jt, &[&x]);
    assert_eq!(entered(&created, 0), ["X@0"]);
    assert_eq!(disabled(&created, 0), ["J"]);
}

#[test]
fn check_reports_an_unreachable_attachment_first_of_its_group() {
    // U is named like a Job Environment, so no entry reaches it. Off, of
    // its group, is known not to be entered, so U is entered.
    let jt = format!("{HEAD}jobEnvironments:\n- {{name: U, variables: {{V: u}}}}\n{STEP}");
    let off = env("name: Off, exclusiveGroup: G, enableIf: false");
    let u = env("name: U, exclusiveGroup: G, envParameterDefinitions: [{name: P, type: STRING}]");
    let unreachable = "EnvironmentTemplate[1] -> environment -> envParameterDefinitions:\n\t\
                       Environment 'U' has the same name as a jobEnvironments entry of the Job \
                       Template, so no 'environment:' dependency can reach it (Template Schemas \
                       §3.2.1 item 4), and it has parameters without a default that no Step can \
                       supply: P. Its activation is BY_DEFAULT and an earlier Environment of its \
                       exclusiveGroup does not supersede it, so Sessions enter it, and the \
                       submission is rejected (§3.2.1).";
    check_err_same_as_create(&jt, &[&off, &u], &[unreachable]);
}

#[test]
fn a_failed_entry_enable_if_still_takes_its_group() {
    // J1's entry's enableIf fails, which fails job creation. J1 may be
    // entered, so J2, later in its group, adds no error of its own.
    let jt = format!(
        "{HEAD}parameterDefinitions: [{{name: L, type: 'LIST[INT]', default: [1]}}]\n\
         jobEnvironments:\n- {{name: J1, exclusiveGroup: G, variables: {{V: a}}}}\n\
         - {{name: J2, exclusiveGroup: G, envParameterDefinitions: [{{name: N, type: INT}}], \
         variables: {{V: b}}}}\n\
         steps:\n- name: S\n  dependencies: [{{environment: J1, enableIf: \"Param.L[4] == \
         1\"}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    create_err(
        &jt,
        &[],
        "JobTemplate",
        &[
            "steps[0] -> dependencies[0] -> enableIf:\n\tcould not be evaluated: Index 4 out of \
           bounds for list of length 1",
        ],
    );
}

#[test]
fn check_decides_a_grouped_job_environment_as_template_validation_would() {
    // With no attachment of its group, J is its group's first. Whether the
    // Step enters it is decided from its enableIf and the Step's entry as
    // written: entered when its enableIf is certainly unresolved (with the
    // unresolved note), and undetermined when it is another expression or
    // the entry's enableIf is one.
    let jt = |enable_if: &str, deps: &str| {
        format!(
            "{HEAD}jobEnvironments:\n- {{name: C, variables: {{V: c}}}}\n- {{name: J, \
             exclusiveGroup: G, envParameterDefinitions: [{{name: P, type: STRING}}, {{name: Q, \
             type: STRING}}], enableIf: {enable_if}, variables: {{V: j}}}}\n\
             steps:\n- name: S\n{deps}  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    // U is not entered (no entry requests it), so a wrong index into the
    // Step's states would read a state other than J's.
    let u = env("name: U, activation: ON_REQUEST");
    let unresolved = jt("\"Env.Param.P != ''\"", "");
    let err = check(&unresolved, &[&u]).expect_err("expected check to fail");
    let names = "every jobEnvironments entry with activation ALWAYS, each one with activation \
                 BY_DEFAULT that the Step does not decline, and each one with activation \
                 ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7";
    assert_errors(
        &err.to_string(),
        "Submission",
        &[&format!(
            "JobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every \
             parameter without a default of each Environment its Sessions enter ({names}); \
             missing: 'P', 'Q' of Environment 'J'{UNRESOLVED_NOTE}."
        )],
    );
    // The entry supplies P, so the enableIf is known to be true, which
    // stays undetermined before job creation; job creation finds Q missing.
    let known = jt(
        "\"Env.Param.P != ''\"",
        "  dependencies: [{environment: J, parameters: {P: x}}]\n",
    );
    check_ok(&known, &[&u]);
    create_err(
        &known,
        &[&u],
        "JobTemplate",
        &[&missing_named("steps[0]", true, "'J'", "Q")],
    );
    // Another expression, or an entry whose enableIf is an expression.
    check_ok(&jt("\"Job.Name == 'other'\"", ""), &[&u]);
    check_ok(
        &jt(
            "\"Env.Param.P != ''\"",
            "  dependencies: [{environment: J, enableIf: \"Job.Name == 'J'\"}]\n",
        ),
        &[&u],
    );
}

#[test]
fn disabled_environments_are_in_job_order() {
    // S, superseded by the attachment X, is decided after D, which its own
    // enableIf disables, but is listed first.
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: S, exclusiveGroup: G, variables: {{V: s}}}}\n\
         - {{name: D, enableIf: false, variables: {{V: d}}}}\n{STEP}"
    );
    let created = create_ok(&jt, &[&env("name: X, exclusiveGroup: G")]);
    assert_eq!(entered(&created, 0), ["X@0"]);
    assert_eq!(disabled(&created, 0), ["S", "D"]);
}

// ── Audit round 2 ──────────────────────────────────────────────────

/// The unreachable-attachment error for `U` at attachment `doc`.
fn unreachable_u(doc: usize) -> String {
    format!(
        "EnvironmentTemplate[{doc}] -> environment -> envParameterDefinitions:\n\tEnvironment \
         'U' has the same name as a jobEnvironments entry of the Job Template, so no \
         'environment:' dependency can reach it (Template Schemas §3.2.1 item 4), and it has \
         parameters without a default that no Step can supply: P. Its activation is BY_DEFAULT \
         and an earlier Environment of its exclusiveGroup does not supersede it, so Sessions \
         enter it, and the submission is rejected (§3.2.1)."
    )
}

#[test]
fn an_unreachable_attachment_one_step_enters_is_an_error() {
    // S1 requests A, which then supersedes U for S1; S0 does not, so S0's
    // Sessions enter U.
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: U, variables: {{V: u}}}}\nsteps:\n- name: S0\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n- name: S1\n  dependencies: \
         [{{environment: A}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let a = env("name: A, activation: ON_REQUEST, exclusiveGroup: G");
    let u = env("name: U, exclusiveGroup: G, envParameterDefinitions: [{name: P, type: STRING}]");
    check_err_same_as_create(&jt, &[&a, &u], &[&unreachable_u(1)]);
}

#[test]
fn an_attachment_an_expression_entry_may_request_is_undetermined() {
    // Whether the entry requests A is decided at job creation, so B may be
    // superseded and is not checked before then; here it is.
    let jt = format!(
        "{HEAD}parameterDefinitions: [{{name: UseA, type: STRING, default: 'yes'}}]\n\
         steps:\n- name: S\n  dependencies: [{{environment: A, enableIf: \"Param.UseA == \
         'yes'\"}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let a = env("name: A, activation: ON_REQUEST, exclusiveGroup: G");
    let b = env("name: B, exclusiveGroup: G, envParameterDefinitions: [{name: P, type: STRING}]");
    check_ok(&jt, &[&a, &b]);
    let created = create_ok(&jt, &[&a, &b]);
    assert_eq!(entered(&created, 0), ["A@0"]);
}

#[test]
fn an_ambiguous_attachment_is_entered_by_its_activation_alone() {
    // No entry resolves to an ambiguous Amb. An ON_REQUEST one is not
    // entered, so B is its group's first; a BY_DEFAULT one whose enableIf
    // is false is not entered either; a BY_DEFAULT one is, so B is
    // superseded.
    let jt = format!(
        "{HEAD}steps:\n- name: S\n  dependencies: [{{environment: Amb}}]\n  script: \
         {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let b = env("name: B, exclusiveGroup: G, envParameterDefinitions: [{name: P, type: STRING}]");
    let ambiguous = "JobTemplate -> steps[0] -> dependencies[0] -> environment:\n\tnames \
                     Environment 'Amb', which 2 attached Environment Templates define \
                     (EnvironmentTemplate[0], EnvironmentTemplate[1]); an entry must match \
                     exactly one (Template Schemas §3.2.1 item 3).";
    let b_missing = missing(
        "JobTemplate -> steps[0]",
        false,
        "'B' (EnvironmentTemplate[2])",
    );
    for amb in [
        "name: Amb, activation: ON_REQUEST, exclusiveGroup: G",
        "name: Amb, exclusiveGroup: G, enableIf: false",
    ] {
        let amb = env(amb);
        check_err_same_as_create(&jt, &[&amb, &amb, &b], &[ambiguous, &b_missing]);
    }
    let amb = env("name: Amb, exclusiveGroup: G");
    check_err_same_as_create(&jt, &[&amb, &amb, &b], &[ambiguous]);
}

#[test]
fn only_the_first_ambiguous_copy_of_a_group_is_an_unreachable_error() {
    // Every Step's entry for Amb is ambiguous, so no Step decides either
    // copy. Ungrouped, both are entered and both need P; in one group, the
    // second copy is superseded.
    let jt = format!(
        "{HEAD}steps:\n- name: S\n  dependencies: [{{environment: Amb}}]\n  script: \
         {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let ambiguous = "JobTemplate -> steps[0] -> dependencies[0] -> environment:\n\tnames \
                     Environment 'Amb', which 2 attached Environment Templates define \
                     (EnvironmentTemplate[0], EnvironmentTemplate[1]); an entry must match \
                     exactly one (Template Schemas §3.2.1 item 3).";
    let unreachable = |doc: usize, how: &str| {
        format!(
            "EnvironmentTemplate[{doc}] -> environment -> envParameterDefinitions:\n\t\
             Environment 'Amb' is defined by 2 attached Environment Templates \
             (EnvironmentTemplate[0], EnvironmentTemplate[1]), so no 'environment:' \
             dependency can reach it (Template Schemas §3.2.1 item 3), and it has parameters \
             without a default that no Step can supply: P. {how}, and the submission is \
             rejected (§3.2.1)."
        )
    };
    let every = "Its activation is BY_DEFAULT, so every Session enters it";
    let grouped = "Its activation is BY_DEFAULT and an earlier Environment of its \
                   exclusiveGroup does not supersede it, so Sessions enter it";
    let amb = env("name: Amb, envParameterDefinitions: [{name: P, type: STRING}]");
    check_err_same_as_create(
        &jt,
        &[&amb, &amb],
        &[ambiguous, &unreachable(0, every), &unreachable(1, every)],
    );
    let amb =
        env("name: Amb, exclusiveGroup: G, envParameterDefinitions: [{name: P, type: STRING}]");
    check_err_same_as_create(&jt, &[&amb, &amb], &[ambiguous, &unreachable(0, grouped)]);
    // With an expression entry naming it, no Step decides it either, and it
    // is still an error, without a group.
    let expr = jt.replace(
        "[{environment: Amb}]",
        "[{environment: Amb, enableIf: \"Job.Name == 'J'\"}]",
    );
    let amb = env("name: Amb, envParameterDefinitions: [{name: P, type: STRING}]");
    check_err_same_as_create(
        &expr,
        &[&amb, &amb],
        &[ambiguous, &unreachable(0, every), &unreachable(1, every)],
    );
}

/// A Job Template whose grouped Job Environment J declares a PATH `D`,
/// with C, ungrouped, before it, and a Step that names C then J with
/// `D: value`.
fn grouped_path(value: &str) -> String {
    format!(
        "{HEAD}jobEnvironments:\n- {{name: C, variables: {{V: c}}}}\n- {{name: J, \
         exclusiveGroup: G, envParameterDefinitions: [{{name: D, type: PATH}}], variables: {{V: \
         j}}}}\nsteps:\n- name: S\n  dependencies: [{{environment: C}}, {{environment: J, \
         parameters: {{D: '{value}'}}}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    )
}

#[test]
fn check_applies_the_submission_policy_to_a_grouped_job_environment() {
    // As job creation does: the path format, URI values, and the caller's
    // limits, and the error at the entry's own index.
    let relative = grouped_path("relative/x");
    let err = check(&relative, &[]).expect_err("expected check to fail");
    assert_errors(
        &err.to_string(),
        "Submission",
        &[&format!("JobTemplate -> {}", not_absolute("relative/x"))],
    );
    create_err(
        &relative,
        &[],
        "JobTemplate",
        &[&not_absolute("relative/x")],
    );
    let windows = |c: ValidationContext| {
        c.with_submission_path_format(openjd_expr::path_mapping::PathFormat::Windows)
    };
    check_with(&grouped_path("C:\\x"), &[], windows).unwrap();
    create_with(&grouped_path("C:\\x"), &[], windows).unwrap();
    let err = check(&grouped_path("C:\\x"), &[]).expect_err("expected check to fail");
    assert_errors(
        &err.to_string(),
        "Submission",
        &[&format!("JobTemplate -> {}", not_absolute("C:\\x"))],
    );
    let uri = |c: ValidationContext| c.with_allow_uri_path_values(true);
    check_with(&grouped_path("s3://bucket/key"), &[], uri).unwrap();
    create_with(&grouped_path("s3://bucket/key"), &[], uri).unwrap();
    let err = check(&grouped_path("s3://bucket/key"), &[]).expect_err("expected check to fail");
    assert_errors(
        &err.to_string(),
        "Submission",
        &[
            "JobTemplate -> steps[0] -> dependencies[1] -> parameters -> D:\n\tis a URI \
           ('s3://bucket/key'), and URI path values are not permitted for this submission.",
        ],
    );
    // An enableIf whose evaluation exceeds the caller's limit is not
    // certainly unresolved, so J is not known to be entered.
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: J, exclusiveGroup: G, envParameterDefinitions: \
         [{{name: P, type: STRING}}], enableIf: \"Env.Param.P != ''\", variables: {{V: j}}}}\n\
         {STEP}"
    );
    let limited = |c: ValidationContext| {
        c.with_caller_limits(CallerLimits {
            max_eval_operations: Some(1),
            ..Default::default()
        })
    };
    check_with(&jt, &[], limited).unwrap();
    let err = check(&jt, &[]).expect_err("expected check to fail");
    assert_errors(
        &err.to_string(),
        "Submission",
        &[&format!(
            "{}{UNRESOLVED_NOTE}.",
            missing("JobTemplate -> steps[0]", true, "'J'").trim_end_matches('.')
        )],
    );
}

/// The error for a PATH value `value` of J's `D` that is not absolute in
/// the POSIX submission path format.
fn not_absolute(value: &str) -> String {
    format!(
        "steps[0] -> dependencies[1] -> parameters -> D:\n\tmust be an absolute path in the \
         submission path format (POSIX), since an Environment parameter has no directory to be \
         relative to; got '{value}'."
    )
}
