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
//! takes them in that order, and once one is entered does not evaluate the
//! later ones' `enableIf` for the Step (§7.4.1 step 6), before those checks
//! (step 7). Before then, a grouped Environment is known to be entered only
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
    // §4 item 7: only an Environment its enableIf does not disable takes
    // the group, so B is the group's first for the Step, and is held to
    // constraint 7.
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

// ── Audit round 3 ──────────────────────────────────────────────────

/// The unreachable-attachment error for the ambiguous `Amb` copy at
/// attachment `doc`, of two at `docs`, entered as `how` says.
fn unreachable_amb(doc: usize, docs: (usize, usize), how: &str) -> String {
    format!(
        "EnvironmentTemplate[{doc}] -> environment -> envParameterDefinitions:\n\tEnvironment \
         'Amb' is defined by 2 attached Environment Templates (EnvironmentTemplate[{}], \
         EnvironmentTemplate[{}]), so no 'environment:' dependency can reach it (Template \
         Schemas §3.2.1 item 3), and it has parameters without a default that no Step can \
         supply: P. {how}, and the submission is rejected (§3.2.1).",
        docs.0, docs.1
    )
}

/// The ambiguous-entry error for `Amb` at Step `step`'s `dependencies[dep]`.
fn ambiguous_amb(step: usize, dep: usize, docs: (usize, usize)) -> String {
    format!(
        "JobTemplate -> steps[{step}] -> dependencies[{dep}] -> environment:\n\tnames \
         Environment 'Amb', which 2 attached Environment Templates define \
         (EnvironmentTemplate[{}], EnvironmentTemplate[{}]); an entry must match exactly one \
         (Template Schemas §3.2.1 item 3).",
        docs.0, docs.1
    )
}

const GROUPED_ENTERED: &str = "Its activation is BY_DEFAULT and an earlier Environment of its \
                               exclusiveGroup does not supersede it, so Sessions enter it";

#[test]
fn an_ambiguous_copy_one_step_enters_first_is_an_unreachable_error() {
    // S1 names Amb, so its Sessions enter the first copy, which is its
    // group's first; S0 requests A, which supersedes both copies for S0.
    // Either way, some Step enters it.
    let amb =
        env("name: Amb, exclusiveGroup: G, envParameterDefinitions: [{name: P, type: STRING}]");
    let a = env("name: A, activation: ON_REQUEST, exclusiveGroup: G");
    let jt = format!(
        "{HEAD}steps:\n- name: S0\n  dependencies: [{{environment: A}}]\n  script: {{actions: \
         {{onRun: {{command: x}}}}}}\n- name: S1\n  dependencies: [{{environment: Amb}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    check_err_same_as_create(
        &jt,
        &[&a, &amb, &amb],
        &[
            &ambiguous_amb(1, 0, (1, 2)),
            &unreachable_amb(1, (1, 2), GROUPED_ENTERED),
        ],
    );
    // S1 declines X, so it enters the first copy; S2 enters X, which then
    // supersedes the copies for S2. Any Step entering it counts, not the
    // last.
    let x = env("name: X, exclusiveGroup: G");
    let jt = format!(
        "{HEAD}steps:\n- name: S1\n  dependencies: [{{environment: X, enableIf: false}}, \
         {{environment: Amb}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: S2\n  dependencies: [{{environment: Amb}}]\n  script: {{actions: {{onRun: \
         {{command: x}}}}}}\n"
    );
    check_err_same_as_create(
        &jt,
        &[&x, &amb, &amb],
        &[
            &ambiguous_amb(0, 1, (1, 2)),
            &ambiguous_amb(1, 0, (1, 2)),
            &unreachable_amb(1, (1, 2), GROUPED_ENTERED),
        ],
    );
}

#[test]
fn an_ambiguous_attachment_with_an_enable_if_expression() {
    // Every Step names Amb, so no Step decides its enableIf. A certainly
    // unresolved one does not disable it, so both copies are entered; any
    // other expression may, so neither is known to be.
    let jt = format!(
        "{HEAD}steps:\n- name: S\n  dependencies: [{{environment: Amb}}]\n  script: \
         {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let amb = |enable_if: &str| {
        env(&format!(
            "name: Amb, envParameterDefinitions: [{{name: P, type: STRING}}], enableIf: \
             {enable_if}"
        ))
    };
    let unresolved = amb("\"Env.Param.P != ''\"");
    let how = "Its activation is BY_DEFAULT and its enableIf does not disable it, so Sessions \
               enter it";
    check_err_same_as_create(
        &jt,
        &[&unresolved, &unresolved],
        &[
            &ambiguous_amb(0, 0, (0, 1)),
            &unreachable_amb(0, (0, 1), how),
            &unreachable_amb(1, (0, 1), how),
        ],
    );
    let other = amb("\"Job.Name == 'J'\"");
    check_err_same_as_create(&jt, &[&other, &other], &[&ambiguous_amb(0, 0, (0, 1))]);
}

#[test]
fn the_single_layer_rule_counts_ambiguous_attachments_by_their_group() {
    // Two wrapping copies of an ambiguous Amb: in one group only the first
    // would be entered, and with enableIf false neither would.
    let jt = format!(
        "{HEAD}steps:\n- name: S\n  dependencies: [{{environment: Amb}}]\n  script: \
         {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    for amb in ["name: Amb, exclusiveGroup: G", "name: Amb, enableIf: false"] {
        let amb = wrapping_env(amb);
        check_err_same_as_create(&jt, &[&amb, &amb], &[&ambiguous_amb(0, 0, (0, 1))]);
    }
}

#[test]
fn check_applies_the_single_layer_rule_to_grouped_job_environments() {
    // Template validation counts no grouped Job Environment, so check,
    // which knows there are no attachments of their groups, does.
    let jt = |a: &str, b: &str| {
        format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, \
             EXPR]\nname: J\njobEnvironments:\n- {{name: A, {a}{WRAP}}}\n- {{name: B, {b}{WRAP}}}\n\
             {STEP}"
        )
    };
    let layers = "JobTemplate -> steps[0]:\n\tits Sessions would enter 2 Environments that \
                  define wrap hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'A', 'B'. A \
                  Session may have at most one Environment defining wrap hooks (RFC 0008).";
    for (a, b) in [
        ("exclusiveGroup: H, ", "exclusiveGroup: G, "),
        ("", "exclusiveGroup: G, "),
    ] {
        let jt = jt(a, b);
        job_ok(&jt);
        check_err_same_as_create(&jt, &[], &[layers]);
        check_err_same_as_create(&jt, &[&env("name: U")], &[layers]);
    }
}

// ── Audit round 4: a second Step decided differently from the first ─

#[test]
fn a_deferred_job_environment_is_decided_for_every_step() {
    // B shares group G with the attachment X, so the submission stage
    // decides it for each Step: superseded for both when X is entered, and
    // its missing P reported for both when X is disabled.
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: B, exclusiveGroup: G, envParameterDefinitions: \
         [{{name: P, type: INT}}], variables: {{V: b}}}}\nsteps:\n- name: S0\n  script: \
         {{actions: {{onRun: {{command: x}}}}}}\n- name: S1\n  script: {{actions: {{onRun: \
         {{command: x}}}}}}\n"
    );
    let created = create_ok(&jt, &[&env("name: X, exclusiveGroup: G")]);
    for step in 0..2 {
        assert_eq!(entered(&created, step), ["X@0"]);
        assert_eq!(disabled(&created, step), ["B"]);
    }
    check_err_same_as_create(
        &jt,
        &[&env("name: X, exclusiveGroup: G, enableIf: false")],
        &[
            &missing("JobTemplate -> steps[0]", true, "'B'"),
            &missing("JobTemplate -> steps[1]", true, "'B'"),
        ],
    );
}

#[test]
fn an_ambiguous_entry_in_a_later_step_gets_no_values_there() {
    // Only S1 names the ambiguous Gpu, so only S1's Sessions skip its
    // values and host requirements: the copies' GPU minimum does not
    // conflict with S1's maximum, and only the ambiguity is reported.
    let jt = format!(
        "{HEAD}steps:\n- name: S0\n  script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: S1\n  dependencies: [{{environment: Gpu}}]\n  hostRequirements: {{amounts: \
         [{{name: amount.worker.gpu, max: 1}}]}}\n  script: {{actions: {{onRun: {{command: \
         x}}}}}}\n"
    );
    // Gpu declares a parameter, so job creation gives a Step whose entry
    // for it is ambiguous no values for it, and skips its requirements.
    let gpu = env(
        "name: Gpu, envParameterDefinitions: [{name: N, type: INT, default: 2}], \
         hostRequirements: {amounts: [{name: amount.worker.gpu, min: 2}]}",
    );
    let ambiguous = "JobTemplate -> steps[1] -> dependencies[0] -> environment:\n\tnames \
                     Environment 'Gpu', which 2 attached Environment Templates define \
                     (EnvironmentTemplate[0], EnvironmentTemplate[1]); an entry must match \
                     exactly one (Template Schemas §3.2.1 item 3).";
    check_err_same_as_create(&jt, &[&gpu, &gpu], &[ambiguous]);
}

#[test]
fn an_expression_entry_in_one_step_leaves_another_step_known() {
    // S0's entry for Conda has an enableIf expression, so Conda is
    // undetermined for S0 only; S1 has no entry, so its Sessions enter
    // Conda and its P is missing, before job creation as at it.
    let jt = format!(
        "{HEAD}parameterDefinitions: [{{name: Mode, type: STRING, default: 'on'}}]\n\
         steps:\n- name: S0\n  dependencies: [{{environment: Conda, enableIf: \"Param.Mode == \
         'on'\", parameters: {{P: x}}}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: S1\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let conda =
        env("name: Conda, exclusiveGroup: G, envParameterDefinitions: [{name: P, type: STRING}]");
    check_err_same_as_create(
        &jt,
        &[&conda],
        &[&missing(
            "JobTemplate -> steps[1]",
            false,
            "'Conda' (EnvironmentTemplate[0])",
        )],
    );
}

// ── Audit round 5 ──────────────────────────────────────────────────

#[test]
fn a_later_member_does_not_clear_what_an_earlier_one_decided() {
    // A is entered (or undetermined); B, disabled, comes between it and C.
    // C stays superseded (or undetermined), so check does not hold it to
    // constraint 7, and job creation enters A.
    for a in ["", "enableIf: \"Job.Name == 'J'\", "] {
        let jt = format!(
            "{HEAD}jobEnvironments:\n- {{name: A, exclusiveGroup: G, {a}variables: {{V: a}}}}\n\
             - {{name: B, exclusiveGroup: G, enableIf: false, variables: {{V: b}}}}\n\
             - {{name: C, exclusiveGroup: G, envParameterDefinitions: [{{name: P, type: \
             STRING}}], variables: {{V: c}}}}\n{STEP}"
        );
        check_ok(&jt, &[]);
        let created = create_ok(&jt, &[]);
        assert_eq!(entered(&created, 0), ["A"]);
        assert_eq!(disabled(&created, 0), ["B", "C"]);
    }
}

#[test]
fn a_deferred_job_environments_values_follow_earlier_ones() {
    // B, decided by the submission stage, is written after A and C, whose
    // values the create_job body computed.
    let jt = format!(
        "{HEAD}jobEnvironments:\n\
         - {{name: A, envParameterDefinitions: [{{name: Q, type: STRING, default: a}}], \
         variables: {{V: a}}}}\n\
         - {{name: C, envParameterDefinitions: [{{name: R, type: STRING, default: c}}], \
         variables: {{V: c}}}}\n\
         - {{name: B, exclusiveGroup: G, envParameterDefinitions: [{{name: N, type: STRING, \
         default: b}}], variables: {{V: b}}}}\n{STEP}"
    );
    let created = create_ok(&jt, &[&env("name: X, exclusiveGroup: G, enableIf: false")]);
    assert_eq!(
        rendered_values(&created.job.steps[0]),
        [
            "JobTemplate/A: Q=string:a",
            "JobTemplate/C: R=string:c",
            "JobTemplate/B: N=string:b"
        ]
    );
}

#[test]
fn check_decides_each_steps_job_environments_with_its_own_entries() {
    // S1 requests the Job Environment B; S0 does not. Only S1's Sessions
    // enter B, so only S1 has two wrap layers and only S1 misses B's P.
    let two_steps = "steps:\n- name: S0\n  script: {actions: {onRun: {command: x}}}\n\
                     - name: S1\n  dependencies: [{environment: B}]\n  script: {actions: \
                     {onRun: {command: x}}}\n";
    let jt = format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, \
         EXPR]\nname: J\njobEnvironments:\n- {{name: B, activation: ON_REQUEST, {WRAP}}}\n\
         {two_steps}"
    );
    let layers = "JobTemplate -> steps[1]:\n\tits Sessions would enter 2 Environments that \
                  define wrap hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'W' \
                  (EnvironmentTemplate[0]), 'B'. A Session may have at most one Environment \
                  defining wrap hooks (RFC 0008).";
    check_err_same_as_create(&jt, &[&wrapping_env("name: W")], &[layers]);
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: B, activation: ON_REQUEST, exclusiveGroup: G, \
         envParameterDefinitions: [{{name: P, type: STRING}}], variables: {{V: b}}}}\n\
         {two_steps}"
    );
    let err = check(&jt, &[]).expect_err("expected check to fail");
    assert_errors(
        &err.to_string(),
        "Submission",
        &[&missing("JobTemplate -> steps[1]", true, "'B'")],
    );
    create_err(
        &jt,
        &[],
        "JobTemplate",
        &[&missing("steps[1]", true, "'B'")],
    );
}

// ── §7.4.1 step 6: a superseded Environment's enableIf is not evaluated ──

/// An Environment body whose `enableIf` fails when it is evaluated: its
/// `S` defaults to `abc`, which `int` cannot convert.
const FAILING: &str = "envParameterDefinitions: [{name: S, type: STRING, default: abc}], \
                       enableIf: \"int(Env.Param.S) > 0\"";

/// [`FAILING`], with a parameter `P` that has no default.
const FAILING_WITH_P: &str = "envParameterDefinitions: [{name: S, type: STRING, default: abc}, \
                              {name: P, type: STRING}], enableIf: \"int(Env.Param.S) > 0\"";

/// The error of [`FAILING`]'s `enableIf` for Step `S`, at `path`.
fn failing(path: &str) -> String {
    format!(
        "{path} -> enableIf:\n\tcould not be evaluated: Cannot convert 'abc' to int. Resolved \
         for Step 'S'.\n  int(Env.Param.S) > 0\n  ^~~~~~~~~~~~~~~~"
    )
}

#[test]
fn a_superseded_job_environments_enable_if_is_not_evaluated() {
    // §4 item 7, §7.4.1 step 6 (JB-154): A is entered, so B is superseded
    // and its enableIf, which would fail, is not evaluated for the Step.
    let jt = |a_enable_if: &str| {
        format!(
            "{HEAD}jobEnvironments:\n\
             - {{name: A, exclusiveGroup: G, enableIf: {a_enable_if}, variables: {{V: a}}}}\n\
             - {{name: B, exclusiveGroup: G, {FAILING}, variables: {{V: b}}}}\n{STEP}"
        )
    };
    let created = create_ok(&jt("true"), &[]);
    assert_eq!(entered(&created, 0), ["A"]);
    assert_eq!(disabled(&created, 0), ["B"]);
    // A disabled: B is its group's first, so its enableIf is evaluated.
    create_err(
        &jt("false"),
        &[],
        "JobTemplate",
        &[&failing("jobEnvironments[1]")],
    );
}

#[test]
fn a_superseded_attachments_enable_if_is_not_evaluated() {
    let x = |enable_if: &str| {
        env(&format!(
            "name: X, exclusiveGroup: G, enableIf: {enable_if}"
        ))
    };
    let y = env(&format!("name: Y, exclusiveGroup: G, {FAILING}"));
    let created = create_ok(&format!("{HEAD}{STEP}"), &[&x("true"), &y]);
    assert_eq!(entered(&created, 0), ["X@0"]);
    assert_eq!(disabled(&created, 0), ["Y@1"]);
    create_err(
        &format!("{HEAD}{STEP}"),
        &[&x("false"), &y],
        "Submission",
        &[&failing("EnvironmentTemplate[1] -> environment")],
    );
}

#[test]
fn an_attachment_supersedes_a_job_environment_without_evaluating_its_enable_if() {
    // The attached X comes first in the Job's Environment order. When it
    // is entered, the Job Environment B of its group is superseded before
    // its enableIf is evaluated; when X is disabled, B's enableIf is
    // evaluated, and its failure is reported at the Job Template's path.
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: B, exclusiveGroup: G, {FAILING}, variables: {{V: b}}}}\n\
         {STEP}"
    );
    let x = |enable_if: &str| {
        env(&format!(
            "name: X, exclusiveGroup: G, enableIf: {enable_if}"
        ))
    };
    let created = create_ok(&jt, &[&x("true")]);
    assert_eq!(entered(&created, 0), ["X@0"]);
    assert_eq!(disabled(&created, 0), ["B"]);
    create_err(
        &jt,
        &[&x("false")],
        "Submission",
        &[&failing("JobTemplate -> jobEnvironments[0]")],
    );
}

#[test]
fn a_job_environment_decided_by_the_submission_stage_notes_a_failed_enable_if() {
    // X is disabled, so the submission stage evaluates B's enableIf, which
    // fails. B is then checked as if it is not disabled, so its missing P
    // is reported with the note, before the enableIf error.
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: B, exclusiveGroup: G, envParameterDefinitions: \
         [{{name: S, type: STRING, default: abc}}, {{name: P, type: STRING}}], enableIf: \
         \"int(Env.Param.S) > 0\", variables: {{V: b}}}}\n{STEP}"
    );
    let x = env("name: X, exclusiveGroup: G, enableIf: false");
    let note = " (its own enableIf could not be evaluated for this Step, so the Environment is \
                checked as if it is not disabled)";
    create_err(
        &jt,
        &[&x],
        "Submission",
        &[
            &format!(
                "{}{note}.",
                missing("JobTemplate -> steps[0]", true, "'B'").trim_end_matches('.')
            ),
            &failing("JobTemplate -> jobEnvironments[0]"),
        ],
    );
}

#[test]
fn a_deferred_job_environment_its_enable_if_disables_does_not_supersede() {
    // X, attached, is disabled, so the submission stage decides B and C.
    // B's own enableIf disables it, so it is left out and does not take
    // the group: C is the group's first.
    let jt = format!(
        "{HEAD}jobEnvironments:\n\
         - {{name: B, exclusiveGroup: G, enableIf: false, variables: {{V: b}}}}\n\
         - {{name: C, exclusiveGroup: G, variables: {{V: c}}}}\n{STEP}"
    );
    let x = env("name: X, exclusiveGroup: G, enableIf: false");
    let created = create_ok(&jt, &[&x]);
    assert_eq!(entered(&created, 0), ["C"]);
    assert_eq!(disabled(&created, 0), ["X@0", "B"]);
}

#[test]
fn a_failed_enable_if_takes_the_group_in_the_submission_stage() {
    // §4 item 7: a failed enableIf does not disable the Environment, so it
    // remains in the Step's set and the later members are superseded
    // without their enableIf being evaluated: only the first failure is
    // reported, for attachments and for deferred Job Environments alike.
    let y = |name: &str| env(&format!("name: {name}, exclusiveGroup: G, {FAILING}"));
    create_err(
        &format!("{HEAD}{STEP}"),
        &[&y("X"), &y("Y")],
        "Submission",
        &[&failing("EnvironmentTemplate[0] -> environment")],
    );
    let jt = format!(
        "{HEAD}jobEnvironments:\n\
         - {{name: B, exclusiveGroup: G, {FAILING}, variables: {{V: b}}}}\n\
         - {{name: C, exclusiveGroup: G, {FAILING}, variables: {{V: c}}}}\n{STEP}"
    );
    create_err(
        &jt,
        &[&env("name: X, exclusiveGroup: G, enableIf: false")],
        "Submission",
        &[&failing("JobTemplate -> jobEnvironments[0]")],
    );
}

#[test]
fn each_step_decides_which_enable_ifs_of_a_group_are_evaluated() {
    // A's enableIf reads U, which only Step S0 supplies as 'yes'. For S0,
    // A is entered and B is superseded without its enableIf being
    // evaluated; for S, A is disabled, so B's enableIf is evaluated and
    // fails. The error names only S.
    let a = "{name: A, exclusiveGroup: G, envParameterDefinitions: [{name: U, type: STRING, \
             default: 'no'}], enableIf: \"Env.Param.U == 'yes'\", variables: {V: a}}";
    let steps = "steps:\n\
                 - name: S0\n  dependencies: [{environment: A, parameters: {U: 'yes'}}]\n  \
                 script: {actions: {onRun: {command: x}}}\n\
                 - name: S\n  script: {actions: {onRun: {command: x}}}\n";
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {a}\n\
         - {{name: B, exclusiveGroup: G, {FAILING}, variables: {{V: b}}}}\n{steps}"
    );
    create_err(&jt, &[], "JobTemplate", &[&failing("jobEnvironments[1]")]);
    // The same with an attached X of the group, disabled, so that the
    // submission stage decides A and B for each Step.
    let x = env("name: X, exclusiveGroup: G, enableIf: false");
    create_err(
        &jt,
        &[&x],
        "Submission",
        &[&failing("JobTemplate -> jobEnvironments[1]")],
    );
    // Both Steps evaluate B when neither supplies U: one error names both.
    let steps = "steps:\n- name: S0\n  script: {actions: {onRun: {command: x}}}\n\
                 - name: S\n  script: {actions: {onRun: {command: x}}}\n";
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {a}\n\
         - {{name: B, exclusiveGroup: G, {FAILING}, variables: {{V: b}}}}\n{steps}"
    );
    let both = failing("JobTemplate -> jobEnvironments[1]")
        .replace("for Step 'S'", "for Steps 'S0' and 'S'");
    create_err(&jt, &[&x], "Submission", &[&both]);
}

#[test]
fn a_failed_enable_if_takes_the_group_in_the_create_job_body() {
    // A's enableIf fails, so A remains in the Step's set and B is
    // superseded: B's missing P is not reported.
    let jt = format!(
        "{HEAD}jobEnvironments:\n\
         - {{name: A, exclusiveGroup: G, {FAILING}, variables: {{V: a}}}}\n\
         - {{name: B, exclusiveGroup: G, envParameterDefinitions: [{{name: P, type: STRING}}], \
         variables: {{V: b}}}}\n{STEP}"
    );
    create_err(&jt, &[], "JobTemplate", &[&failing("jobEnvironments[0]")]);
}

#[test]
fn an_unresolved_enable_if_takes_the_group_in_the_submission_stage() {
    // The first member's enableIf reads its Q, which has no value, so it is
    // unresolved: the member is entered, and the later one, which needs
    // P, is superseded. Only the first's Q is missing.
    let unresolved = "envParameterDefinitions: [{name: Q, type: STRING}], enableIf: \
                      \"Env.Param.Q != ''\"";
    let x = env(&format!("name: X, exclusiveGroup: G, {unresolved}"));
    let y = env("name: Y, exclusiveGroup: G, envParameterDefinitions: [{name: P, type: STRING}]");
    create_err(
        &format!("{HEAD}{STEP}"),
        &[&x, &y],
        "Submission",
        &[&format!(
            "{}{UNRESOLVED_NOTE}.",
            missing_named(
                "JobTemplate -> steps[0]",
                false,
                "'X' (EnvironmentTemplate[0])",
                "Q"
            )
            .trim_end_matches('.')
        )],
    );
    // The same for deferred Job Environments, after a disabled attachment.
    let jt = format!(
        "{HEAD}jobEnvironments:\n\
         - {{name: B, exclusiveGroup: G, {unresolved}, variables: {{V: b}}}}\n\
         - {{name: C, exclusiveGroup: G, envParameterDefinitions: [{{name: P, type: STRING}}], \
         variables: {{V: c}}}}\n{STEP}"
    );
    create_err(
        &jt,
        &[&env("name: X, exclusiveGroup: G, enableIf: false")],
        "Submission",
        &[&format!(
            "{}{UNRESOLVED_NOTE}.",
            missing_named("JobTemplate -> steps[0]", true, "'B'", "Q").trim_end_matches('.')
        )],
    );
}

#[test]
fn a_deferred_job_environments_failed_enable_if_follows_the_attachments() {
    // The attachment Y (group H) and the Job Environment B (group G, which
    // the disabled attachment X shares, so the submission stage decides it)
    // both fail. The attachments' failures are reported first.
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: B, exclusiveGroup: G, {FAILING}, variables: {{V: b}}}}\n\
         {STEP}"
    );
    let y = env(&format!("name: Y, exclusiveGroup: H, {FAILING}"));
    let x = env("name: X, exclusiveGroup: G, enableIf: false");
    let err = create(&jt, &[&y, &x]).expect_err("expected job creation to fail");
    assert_eq!(
        err.to_string(),
        format!(
            "Model validation error: 2 validation errors for Submission\n{}\n{}",
            failing("EnvironmentTemplate[0] -> environment"),
            failing("JobTemplate -> jobEnvironments[0]")
        )
    );
}

/// A Job Template with two Steps, S0 and S1, without entries.
fn two_plain_steps() -> &'static str {
    "steps:\n- name: S0\n  script: {actions: {onRun: {command: x}}}\n\
     - name: S1\n  script: {actions: {onRun: {command: x}}}\n"
}

#[test]
fn every_step_applies_the_group_rule() {
    // The group rule holds for each Step, not only the first, in the
    // create_job body, for attachments, and for deferred Job Environments.
    let steps = two_plain_steps();
    let ab = format!(
        "{HEAD}jobEnvironments:\n- {{name: A, exclusiveGroup: G, variables: {{V: a}}}}\n\
         - {{name: B, exclusiveGroup: G, variables: {{V: b}}}}\n{steps}"
    );
    let created = create_ok(&ab, &[]);
    let attached = create_ok(
        &format!("{HEAD}{steps}"),
        &[
            &env("name: X, exclusiveGroup: G"),
            &env("name: Y, exclusiveGroup: G"),
        ],
    );
    let deferred = create_ok(&ab, &[&env("name: X, exclusiveGroup: G, enableIf: false")]);
    for step in 0..2 {
        assert_eq!(entered(&created, step), ["A"], "step {step}");
        assert_eq!(disabled(&created, step), ["B"], "step {step}");
        assert_eq!(entered(&attached, step), ["X@0"], "step {step}");
        assert_eq!(disabled(&attached, step), ["Y@1"], "step {step}");
        assert_eq!(entered(&deferred, step), ["A"], "step {step}");
        assert_eq!(disabled(&deferred, step), ["X@0", "B"], "step {step}");
    }
}

#[test]
fn a_deferred_job_environments_failed_enable_if_precedes_the_unreachable_attachments() {
    // B, deferred behind the disabled attachment X, fails its enableIf; U,
    // attached under a Job Environment's name, has a parameter no Step can
    // supply. The enableIf error comes first.
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: U, variables: {{V: u}}}}\n\
         - {{name: B, exclusiveGroup: G, {FAILING}, variables: {{V: b}}}}\n{STEP}"
    );
    let x = env("name: X, exclusiveGroup: G, enableIf: false");
    let u = env("name: U, envParameterDefinitions: [{name: P, type: STRING}]");
    let err = create(&jt, &[&x, &u]).expect_err("expected job creation to fail");
    assert_eq!(
        err.to_string(),
        format!(
            "Model validation error: 2 validation errors for Submission\n{}\n\
             EnvironmentTemplate[1] -> environment -> envParameterDefinitions:\n\tEnvironment \
             'U' has the same name as a jobEnvironments entry of the Job Template, so no \
             'environment:' dependency can reach it (Template Schemas §3.2.1 item 4), and it \
             has parameters without a default that no Step can supply: P. Its activation is \
             BY_DEFAULT, so every Session enters it, and the submission is rejected (§3.2.1).",
            failing("JobTemplate -> jobEnvironments[1]")
        )
    );
}

#[test]
fn a_superseded_step_does_not_count_as_deciding_an_unreachable_attachment() {
    // U's enableIf fails for S0. For S1, A comes first in the group and
    // supersedes U, whose enableIf is not evaluated there. So every Step
    // that decided U failed to, and the message says so.
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: U, variables: {{V: u}}}}\nsteps:\n- name: S0\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n- name: S1\n  dependencies: \
         [{{environment: A}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let a = env("name: A, activation: ON_REQUEST, exclusiveGroup: G");
    let u = env(&format!("name: U, exclusiveGroup: G, {FAILING_WITH_P}"));
    let err = create(&jt, &[&a, &u]).expect_err("expected job creation to fail");
    assert_eq!(
        err.to_string(),
        format!(
            "Model validation error: 2 validation errors for Submission\n{}\n\
             EnvironmentTemplate[1] -> environment -> envParameterDefinitions:\n\tEnvironment \
             'U' has the same name as a jobEnvironments entry of the Job Template, so no \
             'environment:' dependency can reach it (Template Schemas §3.2.1 item 4), and it \
             has parameters without a default that no Step can supply: P. Its activation is \
             BY_DEFAULT and its enableIf could not be evaluated, so it is checked as if \
             Sessions enter it, and the submission is rejected (§3.2.1).",
            failing("EnvironmentTemplate[1] -> environment").replace("Step 'S'", "Step 'S0'")
        )
    );
}
