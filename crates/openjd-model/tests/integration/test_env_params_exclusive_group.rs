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
//! Step (§3.2.2): the Step's values for them are not checked, though an
//! entry's keys still are. A superseded Environment still applies to the
//! Step, so it is still held to constraint 7 (§3.2 constraint 7), checked
//! at §7.4.1 step 5, before the group is decided. Job creation takes the
//! group's members in order, and once one is entered does not evaluate the
//! later ones' `enableIf` for the Step (§7.4.1 step 6), before the value
//! checks (step 7). Before then, a grouped Environment is known to be
//! entered only when every earlier one of its group is known not to be (§4,
//! after item 10), so the Job Template alone, whose attachments are not
//! known, never counts a grouped Job Environment as entered for its value
//! checks; template validation still holds it to constraint 7.

use openjd_model::job::{AttributeRequirement, Document, EnvironmentRef, HostRequirements, Job};
use openjd_model::template::EnvironmentTemplate;
use openjd_model::{
    check_environment_templates, create_job_with_environment_templates,
    AttachedEnvironmentTemplate, CallerLimits, CreatedJob, ModelError, ValidationContext,
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
    check_environment_templates(&jt, &attached, &ctx(template_ctx(&jt))).map(|_| ())
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
    create_job_with_environment_templates(&jt, &attached, &values, &ctx(template_ctx(&jt)))
}

fn check_ok(jt_src: &str, env_srcs: &[&str]) {
    check(jt_src, env_srcs).unwrap_or_else(|e| panic!("check failed:\n{e}"));
}

/// `check` and job creation both reject the submission with exactly
/// `expected`, the same message.
fn check_err_same_as_create(jt_src: &str, env_srcs: &[&str], expected: &[&str]) {
    let checked = check(jt_src, env_srcs).expect_err("expected check to fail");
    assert_errors(&checked.to_string(), model(env_srcs), expected);
    let created = create(jt_src, env_srcs).expect_err("expected job creation to fail");
    assert_eq!(checked.to_string(), created.to_string());
}

/// The model an error names: shaped by the inputs, `Submission` with
/// attached Environment Templates and `JobTemplate` without.
fn model(env_srcs: &[&str]) -> &'static str {
    if env_srcs.is_empty() {
        "JobTemplate"
    } else {
        "Submission"
    }
}

/// `check` rejects the submission with exactly `checked`, and job creation
/// with exactly `created`: job creation takes no step after §7.4.1 step 4
/// for a Step with an error from steps 1–4, while `check` reports every
/// violation it is certain of.
fn check_err_and_create_err(jt_src: &str, env_srcs: &[&str], checked: &[&str], created: &[&str]) {
    let err = check(jt_src, env_srcs).expect_err("expected check to fail");
    assert_errors(&err.to_string(), "Submission", checked);
    create_err(jt_src, env_srcs, "Submission", created);
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
         default of each Environment that applies to it ({why}), whatever that Environment's own enableIf is; missing: '{name}' of \
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
    // JB-152: B is superseded, so it is disabled for the Step and its
    // values are not stored. It still applies to the Step, so the Step
    // supplies its P (§3.2 constraint 7).
    let jt = a_and_b("G", "G", "true").replace(
        STEP,
        "steps:\n- name: S\n  dependencies: [{environment: B, parameters: {P: p}}]\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    let created = create_ok(&jt, &[]);
    assert_eq!(entered(&created, 0), ["A"]);
    assert_eq!(disabled(&created, 0), ["B"]);
    assert_eq!(created.job.steps[0].environment_parameters, None);
    // Without P, template validation reports it, superseded or not; JB-153:
    // in different groups both are entered.
    for b_group in ["G", "H"] {
        job_err(
            &a_and_b("G", b_group, "true"),
            &[&missing("steps[0]", true, "'B'")],
        );
    }
}

#[test]
fn a_disabled_job_environment_does_not_supersede() {
    // §4 item 7: only an Environment its enableIf does not disable takes
    // the group, so B is the group's first for the Step, and is entered.
    // B applies to the Step whatever A does, so template validation holds
    // it to constraint 7 (§3.2 constraint 7); the Step supplies its P.
    for a_enable_if in ["false", "\"Job.Name == 'other'\""] {
        job_err(
            &a_and_b("G", "G", a_enable_if),
            &[&missing("steps[0]", true, "'B'")],
        );
        let jt = a_and_b("G", "G", a_enable_if).replace(
            STEP,
            "steps:\n- name: S\n  dependencies: [{environment: B, parameters: {P: p}}]\n  \
             script: {actions: {onRun: {command: x}}}\n",
        );
        let created = create_ok(&jt, &[]);
        assert_eq!(entered(&created, 0), ["B"]);
        assert_eq!(disabled(&created, 0), ["A"]);
    }
}

#[test]
fn template_validation_holds_a_grouped_job_environment_to_constraint_7_but_not_its_values() {
    // Without the Environment Templates, an attached one of its group may
    // come first, so no value check is made for a grouped Job Environment
    // until the Job is created (§4, after item 10). Constraint 7 does not
    // depend on the group (§3.2 constraint 7), so template validation
    // reports B's missing P, grouped or not.
    let first = a_and_b("H", "G", "true");
    job_err(&first, &[&missing("steps[0]", true, "'B'")]);
    // A literal value of the wrong type for a grouped Environment is
    // checked at job creation, where it is entered.
    let typed = format!(
        "{HEAD}jobEnvironments:\n- {{name: A, exclusiveGroup: G, envParameterDefinitions: \
         [{{name: N, type: INT}}], variables: {{V: a}}}}\n\
         steps:\n- name: S\n  dependencies: [{{environment: A, parameters: {{N: '{{{{\"x\"}}}}'}}}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    job_ok(&typed);
    create_err(&typed, &[], "JobTemplate", &[TYPED_ERROR]);
    // Ungrouped, the missing value is reported at template validation too.
    job_err(
        &first.replace("exclusiveGroup: G, ", ""),
        &[&missing("steps[0]", true, "'B'")],
    );
}

#[test]
fn values_of_a_superseded_job_environment_are_not_checked_but_its_keys_are() {
    // §3.2.2: a superseded Environment is disabled for the Step, so its
    // values are not checked (constraint 6), but constraint 5 still
    // applies to the entry, and constraint 7 to the Environment (§3.2
    // constraint 7).
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
            &missing_named("steps[0]", true, "'B'", "N"),
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
fn a_superseded_attachment_is_held_to_constraint_7_but_not_its_values() {
    // JB-151: Docker's Image has no default and the Step does not supply
    // it. Docker is superseded, but it still applies to the Step, so the
    // Step must supply Image (§3.2.1, §3.2 constraint 7), whichever comes
    // first.
    let by_default = |s: &str| s.replace("activation: ON_REQUEST, ", "");
    let jt = format!(
        "{HEAD}steps:\n- name: S\n  dependencies: [{{environment: Conda, parameters: {{Packages: \
         p}}}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let (conda, docker) = (by_default(&env(CONDA)), by_default(&env(DOCKER)));
    check_err_same_as_create(
        &jt,
        &[&conda, &docker],
        &[&missing_named(
            "JobTemplate -> steps[0]",
            false,
            "'Docker' (EnvironmentTemplate[1])",
            "Image",
        )],
    );
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
    // A value Docker's definitions reject is not checked while Docker is
    // superseded; with Docker first, it is entered and the value rejected.
    let jt = conda_and_docker("p").replace("Image: img", "Image: bad");
    let docker = env(&DOCKER.replace("type: STRING}", "type: STRING, allowedValues: [ok]}"));
    assert_ne!(docker, env(DOCKER));
    let created = create_ok(&jt, &[&env(CONDA), &docker]);
    assert_eq!(entered(&created, 0), ["Conda@0"]);
    assert_eq!(disabled(&created, 0), ["Docker@1"]);
    create_err(
        &jt,
        &[&docker, &env(CONDA)],
        "Submission",
        &[
            "JobTemplate -> steps[0] -> dependencies[1] -> parameters -> Image:\n\tParameter \
           'Image': value 'bad' is not in allowed values",
        ],
    );
    // An entry's keys are still checked for a superseded attachment, and
    // the entry, which supplies no Image, still requests Docker, so its
    // missing Image is reported too.
    let jt = conda_and_docker("p").replace("Image: img", "Tag: img");
    let undeclared = "JobTemplate -> steps[0] -> dependencies[1] -> parameters -> Tag:\n\t\
                      Environment 'Docker' (EnvironmentTemplate[1]) declares no parameter 'Tag'. \
                      Declared: Image.";
    check_err_same_as_create(
        &jt,
        &[&env(CONDA), &env(DOCKER)],
        &[
            undeclared,
            &missing_named(
                "JobTemplate -> steps[0]",
                false,
                "'Docker' (EnvironmentTemplate[1])",
                "Image",
            ),
        ],
    );
}

#[test]
fn an_attached_environment_supersedes_a_job_environment_of_its_group() {
    // §1.1 item 7: the attached Environments come first in the Job's
    // Environment order, so they supersede the Job Template's own. B has a
    // parameter without a default, which the Step supplies.
    let jt = |deps: &str| {
        format!(
            "{HEAD}jobEnvironments:\n- {{name: B, exclusiveGroup: G, envParameterDefinitions: \
             [{{name: P, type: STRING}}], variables: {{V: b}}}}\n\
             steps:\n- name: S\n{deps}  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    let supplied = jt("  dependencies: [{environment: B, parameters: {P: p}}]\n");
    let x = |enable_if: &str| {
        env(&format!(
            "name: X, exclusiveGroup: G, enableIf: {enable_if}"
        ))
    };
    let created = create_ok(&supplied, &[&x("true")]);
    assert_eq!(entered(&created, 0), ["X@0"]);
    assert_eq!(disabled(&created, 0), ["B"]);
    // X disabled, as a literal or by an expression: B is the group's first.
    for enable_if in ["false", "\"Job.Name == 'other'\""] {
        let created = create_ok(&supplied, &[&x(enable_if)]);
        assert_eq!(entered(&created, 0), ["B"]);
        assert_eq!(disabled(&created, 0), ["X@0"]);
    }
    // An attachment in another group does not supersede B.
    let created = create_ok(&supplied, &[&env("name: X, exclusiveGroup: H")]);
    assert_eq!(entered(&created, 0), ["X@0", "B"]);
    // Without P, B is held to constraint 7 whichever is entered (§3.2
    // constraint 7): at template validation, and, with an entry whose
    // enableIf leaves B to job creation, at job creation, at the Job
    // Template's path, since with an attachment every path starts with its
    // document.
    job_err(&jt(""), &[&missing("steps[0]", true, "'B'")]);
    let deferred = jt("  dependencies: [{environment: B, enableIf: \"Job.Name == 'J'\"}]\n");
    for enable_if in ["true", "false"] {
        create_err(
            &deferred,
            &[&x(enable_if)],
            "Submission",
            &[&missing("JobTemplate -> steps[0]", true, "'B'")],
        );
    }
}

#[test]
fn a_job_environment_left_for_the_submission_stage_keeps_its_place() {
    // B shares a group with the attachment X, so the submission stage
    // decides it; its values stay in written order among the Job
    // Environments', and C, superseded by B, is not entered, though the
    // Step supplies its P, since C applies to it (§3.2 constraint 7).
    let jt = format!(
        "{HEAD}jobEnvironments:\n\
         - {{name: B, exclusiveGroup: G, envParameterDefinitions: [{{name: P, type: STRING, \
         default: b}}], variables: {{V: b}}}}\n\
         - {{name: C, exclusiveGroup: G, envParameterDefinitions: [{{name: P, type: STRING}}], \
         variables: {{V: c}}}}\n\
         - {{name: D, envParameterDefinitions: [{{name: P, type: STRING, default: d}}], \
         variables: {{V: d}}}}\n\
         steps:\n- name: S\n  dependencies: [{{environment: C, parameters: {{P: c}}}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
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
fn a_superseded_unreachable_attachment_is_an_error() {
    // U is named like a Job Environment, so no entry reaches it; it has a
    // parameter without a default. It applies to every Step whatever its
    // group (§3.2 constraint 7), so check and job creation report it,
    // whether X supersedes it or it comes first.
    let jt = format!("{HEAD}jobEnvironments:\n- {{name: U, variables: {{V: u}}}}\n{STEP}");
    let u = env("name: U, exclusiveGroup: G, envParameterDefinitions: [{name: P, type: STRING}]");
    let x = env("name: X, exclusiveGroup: G");
    check_err_same_as_create(&jt, &[&x, &u], &[&unreachable_u(1)]);
    check_err_same_as_create(&jt, &[&u, &x], &[&unreachable_u(0)]);
}

// ── Before job creation: check_environment_templates ───────────────

#[test]
fn check_holds_every_member_of_a_group_to_constraint_7() {
    // A and B share a group and both have P without a default. Both apply
    // to S whatever A's enableIf and whichever is entered (§3.2.1, §3.2
    // constraint 7), so check and job creation report both, whether A is
    // known to be entered, known not to be, or undetermined.
    let jt = format!("{HEAD}{STEP}");
    let a = |enable_if: &str| {
        env(&format!(
            "name: A, exclusiveGroup: G, enableIf: {enable_if}, envParameterDefinitions: \
             [{{name: P, type: STRING}}]"
        ))
    };
    let b = env("name: B, exclusiveGroup: G, envParameterDefinitions: [{name: P, type: STRING}]");
    for enable_if in ["true", "false", "\"Job.Name == 'J'\""] {
        check_err_same_as_create(
            &jt,
            &[&a(enable_if), &b],
            &[&missing(
                "JobTemplate -> steps[0]",
                false,
                "'A' (EnvironmentTemplate[0]); 'P' of Environment 'B' (EnvironmentTemplate[1])",
            )],
        );
    }
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
        "JobTemplate",
        &[
            "steps[0]:\n\tits Sessions would enter 2 Environments that define wrap \
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
    // entry whose enableIf reads a Job Parameter, and so is not known
    // before job creation (§7.6 rule 5), may request A, B is undetermined,
    // and the rule is left to job creation.
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
    // Template's paths, with the jobEnvironments wording.
    let x = env("name: X, exclusiveGroup: G, enableIf: false");
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: B, exclusiveGroup: G, envParameterDefinitions: \
         [{{name: N, type: INT, minValue: 5}}], variables: {{V: b}}}}\n\
         steps:\n- name: S\n  dependencies: [{{environment: B, parameters: {{N: 1}}}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    check_err_same_as_create(&jt, &[&x], &[DEFERRED_VALUE_ERROR]);
    // The Step's entry for B has an enableIf that reads Job.Name, so
    // template validation and check leave B to job creation, which reports
    // its missing P.
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: B, exclusiveGroup: G, envParameterDefinitions: \
         [{{name: P, type: STRING}}], enableIf: \"Env.Param.P != ''\", variables: {{V: b}}}}\n\
         steps:\n- name: S\n  dependencies: [{{environment: B, enableIf: \"Job.Name == 'J'\"}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    check_ok(&jt, &[&x]);
    create_err(
        &jt,
        &[&x],
        "Submission",
        &[&missing("JobTemplate -> steps[0]", true, "'B'")],
    );
}

const DEFERRED_VALUE_ERROR: &str = "JobTemplate -> steps[0] -> dependencies[0] -> parameters -> \
                                    N:\n\tParameter 'N': value 1 is less than minimum 5";

#[test]
fn an_enable_if_that_reads_a_missing_value_does_not_disable_so_it_supersedes() {
    // §4 item 7: only an enableIf that is false lets the next one in. A's
    // enableIf reads its Q, which has no value, so it is not evaluated
    // (§7.4.1 step 6) and A is entered: B is superseded, so B's value below
    // its minimum is not checked, and only A's Q is missing. S's entry for
    // A has an enableIf that reads Job.Name, so template validation leaves
    // A to job creation.
    let jt = |a_entry: &str| {
        format!(
            "{HEAD}jobEnvironments:\n\
             - {{name: A, exclusiveGroup: G, envParameterDefinitions: [{{name: Q, type: STRING}}], \
             enableIf: \"Env.Param.Q != ''\", variables: {{V: a}}}}\n\
             - {{name: B, exclusiveGroup: G, envParameterDefinitions: [{{name: P, type: STRING}}, \
             {{name: N, type: INT, minValue: 5, default: 5}}], variables: {{V: b}}}}\n\
             steps:\n- name: S\n  dependencies: [{{environment: B, parameters: {{P: p, N: 1}}}}\
             {a_entry}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    let deferred = jt(", {environment: A, enableIf: \"Job.Name == 'J'\"}");
    job_ok(&deferred);
    create_err(
        &deferred,
        &[],
        "JobTemplate",
        &[&missing_named("steps[0]", true, "'A'", "Q")],
    );
    // Without that entry, template validation reports the missing Q,
    // whatever A's enableIf is (§3.2 constraint 7).
    job_err(&jt(""), &[&missing_named("steps[0]", true, "'A'", "Q")]);
}

#[test]
fn an_ambiguous_attachment_takes_its_group() {
    // Two attachments define Amb (§3.2.1 item 3); the entry naming it is
    // rejected. Amb would be entered, so check does not check the value of
    // B, later in its group, and reports only the ambiguity; job creation
    // stops S after the ambiguous entry, so it reports only the ambiguity
    // too. With Amb in another group, B is its group's first, so check
    // rejects B's value.
    let jt = format!(
        "{HEAD}steps:\n- name: S\n  dependencies: [{{environment: Amb}}, {{environment: B, \
         parameters: {{N: 1}}}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let b = env(
        "name: B, exclusiveGroup: G, envParameterDefinitions: [{name: N, type: INT, minValue: \
         5, default: 5}]",
    );
    let ambiguous = "JobTemplate -> steps[0] -> dependencies[0] -> environment:\n\tnames \
                     Environment 'Amb', which 2 attached Environment Templates define \
                     (EnvironmentTemplate[0], EnvironmentTemplate[1]); an entry must match \
                     exactly one (Template Schemas §3.2.1 item 3).";
    let amb = env("name: Amb, exclusiveGroup: G");
    check_err_same_as_create(&jt, &[&amb, &amb, &b], &[ambiguous]);
    let amb = env("name: Amb, exclusiveGroup: H");
    check_err_and_create_err(
        &jt,
        &[&amb, &amb, &b],
        &[
            ambiguous,
            "JobTemplate -> steps[0] -> dependencies[1] -> parameters -> N:\n\tParameter 'N': \
             value 1 is less than minimum 5",
        ],
        &[ambiguous],
    );
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
fn a_grouped_job_environment_is_held_to_constraint_7_whether_or_not_it_is_first() {
    // J applies to S whatever its group (§3.2 constraint 7), so template
    // validation reports its missing N. With S's entry for J left to job
    // creation by an enableIf that reads Job.Name, job creation reports it
    // too, whether J is its group's first (U is ungrouped) or X, attached
    // in its group, comes first.
    let jt = |deps: &str| {
        format!(
            "{HEAD}jobEnvironments:\n- {{name: J, exclusiveGroup: G, envParameterDefinitions: \
             [{{name: N, type: INT}}], variables: {{V: j}}}}\n\
             steps:\n- name: S\n{deps}  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    job_err(&jt(""), &[&missing_named("steps[0]", true, "'J'", "N")]);
    let deferred = jt("  dependencies: [{environment: J, enableIf: \"Job.Name == 'J'\"}]\n");
    let u = env("name: U");
    let x = env("name: X, exclusiveGroup: G");
    for att in [&u, &x] {
        check_ok(&deferred, &[att]);
        create_err(
            &deferred,
            &[att],
            "Submission",
            &[&missing_named("JobTemplate -> steps[0]", true, "'J'", "N")],
        );
    }
    // With N supplied, J is entered unless X comes first.
    let supplied = jt("  dependencies: [{environment: J, parameters: {N: 1}}]\n");
    let created = create_ok(&supplied, &[&u]);
    assert_eq!(entered(&created, 0), ["U@0", "J"]);
    let created = create_ok(&supplied, &[&x]);
    assert_eq!(entered(&created, 0), ["X@0"]);
    assert_eq!(disabled(&created, 0), ["J"]);
}

#[test]
fn check_reports_a_grouped_unreachable_attachment_whatever_comes_first() {
    // U is named like a Job Environment, so no entry reaches it. It applies
    // to every Step whatever its group (§3.2 constraint 7), so check reports
    // it as job creation does, whether Off, of its group, is disabled or
    // entered.
    let jt = format!("{HEAD}jobEnvironments:\n- {{name: U, variables: {{V: u}}}}\n{STEP}");
    let u = env("name: U, exclusiveGroup: G, envParameterDefinitions: [{name: P, type: STRING}]");
    for off in [
        "name: Off, exclusiveGroup: G, enableIf: false",
        "name: Off, exclusiveGroup: G",
    ] {
        check_err_same_as_create(&jt, &[&env(off), &u], &[&unreachable_u(1)]);
    }
}

#[test]
fn a_failed_entry_enable_if_stops_the_step_of_a_grouped_job_environment() {
    // J1's entry's enableIf fails, which fails job creation and stops S
    // (§7.4.1 step 2), so J2's value below its minimum is not checked. J2
    // applies to S whatever its group, so S supplies its N; without it,
    // template validation reports it (§3.2 constraint 7).
    let jt = |j2_entry: &str| {
        format!(
            "{HEAD}parameterDefinitions: [{{name: L, type: 'LIST[INT]', default: [1]}}]\n\
             jobEnvironments:\n- {{name: J1, exclusiveGroup: G, variables: {{V: a}}}}\n\
             - {{name: J2, exclusiveGroup: G, envParameterDefinitions: [{{name: N, type: INT, \
             minValue: 5}}], variables: {{V: b}}}}\n\
             steps:\n- name: S\n  dependencies: [{{environment: J1, enableIf: \"Param.L[4] == \
             1\"}}{j2_entry}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    create_err(
        &jt(", {environment: J2, parameters: {N: 1}}"),
        &[],
        "JobTemplate",
        &[
            "steps[0] -> dependencies[0] -> enableIf:\n\tcould not be evaluated: Index 4 out of \
           bounds for list of length 1",
        ],
    );
    job_err(&jt(""), &[&missing_named("steps[0]", true, "'J2'", "N")]);
}

#[test]
fn a_grouped_job_environment_is_held_to_constraint_7_unless_its_entry_is_unknown() {
    // With no attachment of its group, J is its group's first. Whatever its
    // enableIf, it applies to S (§3.2 constraint 7), so template validation
    // reports its missing values when S's entries for it are known: none,
    // or one without an enableIf, which supplies P. One whose enableIf is
    // an expression leaves J to job creation, and check leaves it too.
    let jt = |enable_if: &str, deps: &str| {
        format!(
            "{HEAD}jobEnvironments:\n- {{name: C, variables: {{V: c}}}}\n- {{name: J, \
             exclusiveGroup: G, envParameterDefinitions: [{{name: P, type: STRING}}, {{name: Q, \
             type: STRING}}], enableIf: {enable_if}, variables: {{V: j}}}}\n\
             steps:\n- name: S\n{deps}  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    for enable_if in ["\"Env.Param.P != ''\"", "\"Job.Name == 'other'\""] {
        job_err(
            &jt(enable_if, ""),
            &[&missing_named("steps[0]", true, "'J'", "P', 'Q")],
        );
    }
    job_err(
        &jt(
            "\"Env.Param.P != ''\"",
            "  dependencies: [{environment: J, parameters: {P: x}}]\n",
        ),
        &[&missing_named("steps[0]", true, "'J'", "Q")],
    );
    // U is not entered (no entry requests it), so a wrong index into the
    // Step's states would read a state other than J's.
    let u = env("name: U, activation: ON_REQUEST");
    let deferred = jt(
        "\"Env.Param.P != ''\"",
        "  dependencies: [{environment: J, enableIf: \"Job.Name == 'J'\"}]\n",
    );
    check_ok(&deferred, &[&u]);
    create_err(
        &deferred,
        &[&u],
        "Submission",
        &[&missing_named(
            "JobTemplate -> steps[0]",
            true,
            "'J'",
            "P', 'Q",
        )],
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
         parameters without a default that no Step can supply: P. {WHATEVER}, and the \
         submission is rejected (§3.2.1)."
    )
}

#[test]
fn an_unreachable_attachment_superseded_for_one_step_is_an_error() {
    // S1 requests A, which then supersedes U for S1; S0 does not. U applies
    // to both whatever its group (§3.2 constraint 7), so it is an error.
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
    // superseded, and its values are not checked before then; here it is
    // superseded, so its value below its minimum is not checked at all.
    let jt = format!(
        "{HEAD}parameterDefinitions: [{{name: UseA, type: STRING, default: 'yes'}}]\n\
         steps:\n- name: S\n  dependencies: [{{environment: A, enableIf: \"Param.UseA == \
         'yes'\"}}, {{environment: B, parameters: {{N: 1}}}}]\n  script: {{actions: {{onRun: \
         {{command: x}}}}}}\n"
    );
    let a = env("name: A, activation: ON_REQUEST, exclusiveGroup: G");
    let b = env(
        "name: B, exclusiveGroup: G, envParameterDefinitions: [{name: N, type: INT, minValue: \
         5, default: 5}]",
    );
    check_ok(&jt, &[&a, &b]);
    let created = create_ok(&jt, &[&a, &b]);
    assert_eq!(entered(&created, 0), ["A@0"]);
    assert_eq!(disabled(&created, 0), ["B@1"]);
    // B applies to S whatever A does, so a parameter without a default is
    // reported by check and job creation alike (§3.2.1, §3.2 constraint 7).
    let b = env(
        "name: B, exclusiveGroup: G, envParameterDefinitions: [{name: N, type: INT, minValue: \
         5, default: 5}, {name: P, type: STRING}]",
    );
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
fn an_ambiguous_attachment_does_not_lift_constraint_7_from_its_group() {
    // No entry resolves to an ambiguous Amb. Whether Amb is entered (a
    // BY_DEFAULT one), is not (an ON_REQUEST one, or one whose enableIf is
    // false), B of its group still applies to S (§3.2 constraint 7), so
    // check reports B's missing P. The ambiguous entry stops S after
    // §7.4.1 step 4 at job creation, so job creation reports the ambiguity
    // alone.
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
        "name: Amb, exclusiveGroup: G",
    ] {
        let amb = env(amb);
        check_err_and_create_err(
            &jt,
            &[&amb, &amb, &b],
            &[ambiguous, &b_missing],
            &[ambiguous],
        );
    }
}

#[test]
fn every_ambiguous_copy_is_an_unreachable_error_whatever_its_group() {
    // Every Step's entry for Amb is ambiguous. Both copies apply to S
    // whatever their group (§3.2 constraint 7), so check reports both,
    // ungrouped or in one group.
    let jt = format!(
        "{HEAD}steps:\n- name: S\n  dependencies: [{{environment: Amb}}]\n  script: \
         {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let ambiguous = "JobTemplate -> steps[0] -> dependencies[0] -> environment:\n\tnames \
                     Environment 'Amb', which 2 attached Environment Templates define \
                     (EnvironmentTemplate[0], EnvironmentTemplate[1]); an entry must match \
                     exactly one (Template Schemas §3.2.1 item 3).";
    let ungrouped = env("name: Amb, envParameterDefinitions: [{name: P, type: STRING}]");
    let grouped =
        env("name: Amb, exclusiveGroup: G, envParameterDefinitions: [{name: P, type: STRING}]");
    let expr = jt.replace(
        "[{environment: Amb}]",
        "[{environment: Amb, enableIf: \"Job.Name == 'J'\"}]",
    );
    // At job creation the ambiguous entry stops S, the only Step, after
    // §7.4.1 step 4, so only the ambiguity is reported there.
    for amb in [&ungrouped, &grouped] {
        check_err_and_create_err(
            &jt,
            &[amb, amb],
            &[
                ambiguous,
                &unreachable_amb(0, (0, 1), UNDECLINED),
                &unreachable_amb(1, (0, 1), UNDECLINED),
            ],
            &[ambiguous],
        );
    }
    // An entry whose `enableIf` is not known: disabled, it would decline
    // both copies (§3.2.1 item 3), so check reports nothing (§7.6 rule 5).
    // Job creation finds it enabled.
    check_ok(&expr, &[&ungrouped, &ungrouped]);
    create_err(&expr, &[&ungrouped, &ungrouped], "Submission", &[ambiguous]);
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

/// §4.5 item 3 (spec af53020) for a grouped Job Environment's LIST[PATH]
/// literal value under `check_environment_templates`: elements of any
/// form, relative ones included, pass, as at job creation.
#[test]
fn check_accepts_a_grouped_list_path_value_of_any_form() {
    let src = grouped_path("X")
        .replace("{name: D, type: PATH}", "{name: D, type: \"LIST[PATH]\"}")
        .replace(
            "D: 'X'",
            r"D: ['C:\x', '/y', '\\srv\s', 'rel', 'C:z', '\w', '', 's3://b/k']",
        );
    check(&src, &[]).unwrap();
    create_with(&src, &[], |c| c).unwrap();
}

#[test]
fn check_applies_the_value_checks_to_a_grouped_job_environment() {
    // As job creation does: the definition's constraints and the caller's
    // limits, and the error at the entry's own index. No form of PATH value
    // is an error (§4.5 item 3): a relative path, a drive-relative one, an
    // empty one and a URI pass both.
    for value in ["relative/x", "C:x", "s3://bucket/key", r"C:\x", ""] {
        check(&grouped_path(value), &[]).unwrap_or_else(|e| panic!("{value}: {e}"));
        create_with(&grouped_path(value), &[], |c| c).unwrap_or_else(|e| panic!("{value}: {e}"));
    }
    let constrained = grouped_path("relative/x").replace(
        "{name: D, type: PATH}",
        "{name: D, type: PATH, allowedValues: [/ok]}",
    );
    let not_allowed = "steps[0] -> dependencies[1] -> parameters -> D:\n\tParameter 'D': value \
                       'relative/x' is not in allowed values";
    let err = check(&constrained, &[]).expect_err("expected check to fail");
    assert_errors(&err.to_string(), "JobTemplate", &[not_allowed]);
    create_err(&constrained, &[], "JobTemplate", &[not_allowed]);
    // A constant enableIf is known `true` before job creation, so the value
    // is checked; one whose evaluation exceeds the caller's limit is not
    // known, so J is not known to be entered and the value is left to job
    // creation.
    let constant = constrained.replace(
        "exclusiveGroup: G, envParameterDefinitions",
        "exclusiveGroup: G, enableIf: \"1 + 1 == 2\", envParameterDefinitions",
    );
    assert_ne!(constant, constrained);
    let limited = |c: ValidationContext| {
        c.with_caller_limits(CallerLimits {
            max_eval_operations: Some(1),
            ..Default::default()
        })
    };
    check_with(&constant, &[], limited).unwrap();
    let err = check(&constant, &[]).expect_err("expected check to fail");
    assert_errors(&err.to_string(), "JobTemplate", &[not_allowed]);
    create_err(&constant, &[], "JobTemplate", &[not_allowed]);
}

// ── Audit round 3 ──────────────────────────────────────────────────

/// The unreachable-attachment error for the ambiguous `Amb` copy at
/// attachment `doc`, of two at `docs`, entered as `how` says.
fn unreachable_amb(doc: usize, docs: (usize, usize), how: &str) -> String {
    format!(
        "EnvironmentTemplate[{doc}] -> environment -> envParameterDefinitions:\n\tEnvironment 'Amb' \
         is defined by 2 attached Environment Templates (EnvironmentTemplate[{}], \
         EnvironmentTemplate[{}]), so no enabled 'environment:' dependency can reach it (Template \
         Schemas §3.2.1 item 3), and it has parameters without a default that no Step can supply: \
         P. {how}, and the submission is rejected (§3.2.1).",
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

/// How the unreachable error says a grouped `BY_DEFAULT` attachment
/// applies to the Steps (§3.2 constraint 7).
const WHATEVER: &str = "Its activation is BY_DEFAULT, so it applies to every Step whatever its \
                        enableIf and exclusiveGroup (§3.2 constraint 7)";

/// How the unreachable error says an ambiguous `BY_DEFAULT` attachment
/// applies to a Step, since a disabled entry could decline it (§3.2.1 item
/// 3).
const UNDECLINED: &str = "Its activation is BY_DEFAULT, and a Step does not decline it, so it \
                          applies to that Step whatever its enableIf and exclusiveGroup (§3.2 \
                          constraint 7)";

#[test]
fn an_ambiguous_copy_that_applies_to_a_step_not_stopped_is_an_unreachable_error() {
    // S1 names Amb; S0 requests A, which supersedes both copies for S0.
    // Both copies apply to every Step whatever their group (§3.2
    // constraint 7), so check reports both. At job creation the ambiguous
    // entry stops S1 after §7.4.1 step 4, but S0 is not stopped and both
    // copies apply to it, so job creation reports both too.
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
            &unreachable_amb(1, (1, 2), UNDECLINED),
            &unreachable_amb(2, (1, 2), UNDECLINED),
        ],
    );
    // S1 declines X and names Amb; S2 names Amb. Both Steps are stopped at
    // job creation, so only check reports the copies.
    let x = env("name: X, exclusiveGroup: G");
    let jt = format!(
        "{HEAD}steps:\n- name: S1\n  dependencies: [{{environment: X, enableIf: false}}, \
         {{environment: Amb}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: S2\n  dependencies: [{{environment: Amb}}]\n  script: {{actions: {{onRun: \
         {{command: x}}}}}}\n"
    );
    check_err_and_create_err(
        &jt,
        &[&x, &amb, &amb],
        &[
            &ambiguous_amb(0, 1, (1, 2)),
            &ambiguous_amb(1, 0, (1, 2)),
            &unreachable_amb(1, (1, 2), UNDECLINED),
            &unreachable_amb(2, (1, 2), UNDECLINED),
        ],
        &[&ambiguous_amb(0, 1, (1, 2)), &ambiguous_amb(1, 0, (1, 2))],
    );
}

#[test]
fn an_ambiguous_attachment_with_an_enable_if_expression() {
    // Every Step names Amb, so no entry reaches either copy, and both apply
    // to S whatever their enableIf (§3.2 constraint 7): check reports both
    // copies' unreachable P, for an enableIf that reads P and for one that
    // reads Job.Name. At job creation the ambiguous entry stops S, the only
    // Step, after §7.4.1 step 4, so no Step that is not stopped applies
    // them, and only the ambiguity is reported.
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
    for enable_if in ["\"Env.Param.P != ''\"", "\"Job.Name == 'J'\""] {
        let amb = amb(enable_if);
        check_err_and_create_err(
            &jt,
            &[&amb, &amb],
            &[
                &ambiguous_amb(0, 0, (0, 1)),
                &unreachable_amb(0, (0, 1), UNDECLINED),
                &unreachable_amb(1, (0, 1), UNDECLINED),
            ],
            &[&ambiguous_amb(0, 0, (0, 1))],
        );
    }
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
        check_err_same_as_create(&jt, &[], &[layers.trim_start_matches("JobTemplate -> ")]);
        check_err_same_as_create(&jt, &[&env("name: U")], &[layers]);
    }
}

// ── Audit round 4: a second Step decided differently from the first ─

#[test]
fn a_deferred_job_environment_is_decided_for_every_step() {
    // B shares group G with the attachment X, so the submission stage
    // decides it for each Step: superseded for both when X is entered, and
    // entered by both when X is disabled.
    let jt = |entry: &str| {
        format!(
            "{HEAD}jobEnvironments:\n- {{name: B, exclusiveGroup: G, envParameterDefinitions: \
             [{{name: P, type: INT}}], variables: {{V: b}}}}\nsteps:\n- name: S0\n  dependencies: \
             [{entry}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n- name: S1\n  \
             dependencies: [{entry}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    let supplied = jt("{environment: B, parameters: {P: 1}}");
    let created = create_ok(&supplied, &[&env("name: X, exclusiveGroup: G")]);
    for step in 0..2 {
        assert_eq!(entered(&created, step), ["X@0"]);
        assert_eq!(disabled(&created, step), ["B"]);
    }
    let created = create_ok(
        &supplied,
        &[&env("name: X, exclusiveGroup: G, enableIf: false")],
    );
    for step in 0..2 {
        assert_eq!(entered(&created, step), ["B"]);
        assert_eq!(disabled(&created, step), ["X@0"]);
    }
    // B applies to both Steps either way, so its missing P is reported for
    // both (§3.2 constraint 7): at job creation, when the entries' enableIf
    // leaves B to it, superseded or not.
    let deferred = jt("{environment: B, enableIf: \"Job.Name == 'J'\"}");
    for x in [
        "name: X, exclusiveGroup: G",
        "name: X, exclusiveGroup: G, enableIf: false",
    ] {
        create_err(
            &deferred,
            &[&env(x)],
            "Submission",
            &[
                &missing("JobTemplate -> steps[0]", true, "'B'"),
                &missing("JobTemplate -> steps[1]", true, "'B'"),
            ],
        );
    }
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
    // S0's entry for Conda has an enableIf that reads a Job Parameter, so
    // whether Conda applies to S0 is not known before job creation; S1 has
    // no entry, so Conda applies to S1 and its P is missing, before job
    // creation as at it.
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
    // C stays superseded (or undetermined), so check does not check C's
    // value below its minimum, and job creation enters A, leaving C
    // superseded. C applies to S, so S supplies its P (§3.2 constraint 7).
    for a in ["", "enableIf: \"Job.Name == 'J'\", "] {
        let jt = format!(
            "{HEAD}jobEnvironments:\n- {{name: A, exclusiveGroup: G, {a}variables: {{V: a}}}}\n\
             - {{name: B, exclusiveGroup: G, enableIf: false, variables: {{V: b}}}}\n\
             - {{name: C, exclusiveGroup: G, envParameterDefinitions: [{{name: P, type: \
             STRING}}, {{name: N, type: INT, minValue: 5, default: 5}}], variables: {{V: c}}}}\n\
             steps:\n- name: S\n  dependencies: [{{environment: C, parameters: {{P: p, N: 1}}}}]\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n"
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
    // S1 requests B; S0 does not. Only S1's Sessions enter B, so only S1
    // has two wrap layers and only S1 misses B's P: as an attachment, by
    // check and job creation, and as a grouped Job Environment, by template
    // validation, which holds it to constraint 7 whatever its group (§3.2
    // constraint 7).
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
    let b = "name: B, activation: ON_REQUEST, exclusiveGroup: G, envParameterDefinitions: \
             [{name: P, type: STRING}]";
    check_err_same_as_create(
        &format!("{HEAD}{two_steps}"),
        &[&env(b)],
        &[&missing(
            "JobTemplate -> steps[1]",
            false,
            "'B' (EnvironmentTemplate[0])",
        )],
    );
    job_err(
        &format!("{HEAD}jobEnvironments:\n- {{{b}, variables: {{V: b}}}}\n{two_steps}"),
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
fn a_job_environment_decided_by_the_submission_stage_reports_constraint_7_before_its_failed_enable_if(
) {
    // X is disabled, so the submission stage evaluates B's enableIf, which
    // reads only S, whose default converts, and fails. B is then checked as
    // if it is not disabled. Its missing P (§7.4.1 step 5) is reported,
    // with no note, before the enableIf error (step 6). S's entry for B has
    // an enableIf that reads Job.Name, so template validation leaves B to
    // job creation.
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: B, exclusiveGroup: G, envParameterDefinitions: \
         [{{name: S, type: STRING, default: abc}}, {{name: P, type: STRING}}], enableIf: \
         \"int(Env.Param.S) > 0\", variables: {{V: b}}}}\n\
         steps:\n- name: S\n  dependencies: [{{environment: B, enableIf: \"Job.Name == 'J'\"}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let x = env("name: X, exclusiveGroup: G, enableIf: false");
    let err = create(&jt, &[&x]).expect_err("expected job creation to fail");
    assert_eq!(
        err.to_string(),
        format!(
            "Model validation error: 2 validation errors for Submission\n{}\n{}",
            missing("JobTemplate -> steps[0]", true, "'B'"),
            failing("JobTemplate -> jobEnvironments[0]")
        )
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
    // superseded: B's value below its minimum is not checked. The Step
    // supplies B's P, since B applies to it (§3.2 constraint 7); without it,
    // template validation reports it.
    let jt = |b_params: &str| {
        format!(
            "{HEAD}jobEnvironments:\n\
             - {{name: A, exclusiveGroup: G, {FAILING}, variables: {{V: a}}}}\n\
             - {{name: B, exclusiveGroup: G, envParameterDefinitions: [{{name: P, type: STRING}}, \
             {{name: N, type: INT, minValue: 5, default: 5}}], variables: {{V: b}}}}\n\
             steps:\n- name: S\n  dependencies: [{{environment: B, parameters: {b_params}}}]\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    create_err(
        &jt("{P: p, N: 1}"),
        &[],
        "JobTemplate",
        &[&failing("jobEnvironments[0]")],
    );
    job_err(&jt("{N: 1}"), &[&missing("steps[0]", true, "'B'")]);
}

#[test]
fn an_enable_if_that_reads_a_missing_value_takes_the_group_in_the_submission_stage() {
    // The first member's enableIf reads its Q, which has no value, so it is
    // not evaluated: the member is entered, and the later one is
    // superseded, so its value below its minimum is not checked. Both apply
    // to the Step, so both missing values are reported (§3.2 constraint 7).
    let unresolved = "envParameterDefinitions: [{name: Q, type: STRING}], enableIf: \
                      \"Env.Param.Q != ''\"";
    let x = env(&format!("name: X, exclusiveGroup: G, {unresolved}"));
    let y = env(
        "name: Y, exclusiveGroup: G, envParameterDefinitions: [{name: P, type: STRING}, {name: \
         N, type: INT, minValue: 5, default: 5}]",
    );
    create_err(
        &format!(
            "{HEAD}steps:\n- name: S\n  dependencies: [{{environment: Y, parameters: {{N: 1}}}}]\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n"
        ),
        &[&x, &y],
        "Submission",
        &[&missing_named(
            "JobTemplate -> steps[0]",
            false,
            "'X' (EnvironmentTemplate[0]); 'P' of Environment 'Y' (EnvironmentTemplate[1])",
            "Q",
        )],
    );
    // The same for deferred Job Environments, after a disabled attachment.
    // S's entries have an enableIf that reads Job.Name, so template
    // validation leaves B and C to job creation.
    let jt = format!(
        "{HEAD}jobEnvironments:\n\
         - {{name: B, exclusiveGroup: G, {unresolved}, variables: {{V: b}}}}\n\
         - {{name: C, exclusiveGroup: G, envParameterDefinitions: [{{name: P, type: STRING}}, \
         {{name: N, type: INT, minValue: 5, default: 5}}], variables: {{V: c}}}}\n\
         steps:\n- name: S\n  dependencies: [{{environment: B, enableIf: \"Job.Name == 'J'\"}}, \
         {{environment: C, enableIf: \"Job.Name == 'J'\", parameters: {{N: 1}}}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    create_err(
        &jt,
        &[&env("name: X, exclusiveGroup: G, enableIf: false")],
        "Submission",
        &[&missing_named(
            "JobTemplate -> steps[0]",
            true,
            "'B'; 'P' of Environment 'C'",
            "Q",
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
fn the_unreachable_attachments_error_precedes_a_deferred_job_environments_failed_enable_if() {
    // B, deferred behind the disabled attachment X, fails its enableIf
    // (§7.4.1 step 6); U, attached under a Job Environment's name, has a
    // parameter no Step can supply, filed at step 5 after every Step's
    // errors of step 5. So U's error comes first.
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
            "Model validation error: 2 validation errors for Submission\n\
             EnvironmentTemplate[1] -> environment -> envParameterDefinitions:\n\tEnvironment \
             'U' has the same name as a jobEnvironments entry of the Job Template, so no \
             'environment:' dependency can reach it (Template Schemas §3.2.1 item 4), and it \
             has parameters without a default that no Step can supply: P. Its activation is \
             BY_DEFAULT, so every Session enters it, and the submission is rejected (§3.2.1).\n{}",
            failing("JobTemplate -> jobEnvironments[1]")
        )
    );
}

#[test]
fn an_unreachable_attachment_superseded_for_a_step_is_reported_once_with_its_enable_if() {
    // U's enableIf fails for S0. For S1, A comes first in the group and
    // supersedes U, whose enableIf is not evaluated there, so the enableIf
    // error names S0 alone. U applies to both Steps whatever its enableIf
    // and group (§3.2 constraint 7), so its unreachable P is an error,
    // filed at §7.4.1 step 5, before the enableIf error (step 6).
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
            "Model validation error: 2 validation errors for Submission\n{}\n{}",
            unreachable_u(1),
            failing("EnvironmentTemplate[1] -> environment").replace("Step 'S'", "Step 'S0'")
        )
    );
}

/// Steps S0, which has no entries, and S1, whose entry for `env`
/// supplies `params`.
fn s0_and_s1_with(env: &str, params: &str) -> String {
    format!(
        "steps:\n- name: S0\n  script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: S1\n  dependencies: [{{environment: {env}, parameters: {{{params}}}}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    )
}

#[test]
fn the_enable_if_is_decided_per_step_and_constraint_7_is_reported_for_each() {
    // The constraint 7 message no longer carries notes about the
    // Environment's own enableIf (§3.2 constraint 7); what stays per Step
    // is the enableIf itself. S0 takes S's default, so the enableIf fails
    // for S0 alone (or, reading the missing Q, is not evaluated for S0); S1
    // supplies a value that makes it true. Both Steps miss P, reported for
    // each.
    let x = env("name: X, exclusiveGroup: G, enableIf: false");
    let unresolved_with_p = "envParameterDefinitions: [{name: Q, type: STRING}, {name: P, \
                             type: STRING}], enableIf: \"Env.Param.Q != ''\"";
    // The Steps' entries for B have an enableIf that reads Job.Name, so
    // template validation leaves B to job creation.
    let job = |body: &str, params: &str| {
        format!(
            "{HEAD}jobEnvironments:\n- {{name: B, exclusiveGroup: G, {body}, variables: {{V: b}}}}\n\
             steps:\n- name: S0\n  dependencies: [{{environment: B, enableIf: \"Job.Name == 'J'\"}}]\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n\
             - name: S1\n  dependencies: [{{environment: B, enableIf: \"Job.Name == 'J'\", \
             parameters: {{{params}}}}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    // A deferred Job Environment whose enableIf fails for S0.
    create_err(
        &job(FAILING_WITH_P, "S: '5'"),
        &[&x],
        "Submission",
        &[
            &missing("JobTemplate -> steps[0]", true, "'B'"),
            &missing("JobTemplate -> steps[1]", true, "'B'"),
            &failing("JobTemplate -> jobEnvironments[0]").replace("Step 'S'", "Step 'S0'"),
        ],
    );
    // A deferred Job Environment whose enableIf reads the missing Q for S0.
    create_err(
        &job(unresolved_with_p, "Q: q"),
        &[&x],
        "Submission",
        &[
            &missing_named("JobTemplate -> steps[0]", true, "'B'", "Q', 'P"),
            &missing("JobTemplate -> steps[1]", true, "'B'"),
        ],
    );
    // The same for an attachment.
    let y = |body: &str| env(&format!("name: Y, {body}"));
    create_err(
        &format!("{HEAD}{}", s0_and_s1_with("Y", "S: '5'")),
        &[&y(FAILING_WITH_P)],
        "Submission",
        &[
            &missing(
                "JobTemplate -> steps[0]",
                false,
                "'Y' (EnvironmentTemplate[0])",
            ),
            &missing(
                "JobTemplate -> steps[1]",
                false,
                "'Y' (EnvironmentTemplate[0])",
            ),
            &failing("EnvironmentTemplate[0] -> environment").replace("Step 'S'", "Step 'S0'"),
        ],
    );
    create_err(
        &format!("{HEAD}{}", s0_and_s1_with("Y", "Q: q")),
        &[&y(unresolved_with_p)],
        "Submission",
        &[
            &missing_named(
                "JobTemplate -> steps[0]",
                false,
                "'Y' (EnvironmentTemplate[0])",
                "Q', 'P",
            ),
            &missing(
                "JobTemplate -> steps[1]",
                false,
                "'Y' (EnvironmentTemplate[0])",
            ),
        ],
    );
}

#[test]
fn a_deferred_job_environments_values_are_kept_for_every_step() {
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: B, exclusiveGroup: G, envParameterDefinitions: \
         [{{name: N, type: STRING, default: b}}], variables: {{V: b}}}}\n{}",
        two_plain_steps()
    );
    let created = create_ok(&jt, &[&env("name: X, exclusiveGroup: G, enableIf: false")]);
    for step in 0..2 {
        assert_eq!(
            rendered_values(&created.job.steps[step]),
            ["JobTemplate/B: N=string:b"],
            "step {step}"
        );
    }
}

#[test]
fn within_a_step_the_attachments_errors_come_before_the_deferred_ones() {
    // Y (group H) is attached and missing P; B, deferred behind the
    // disabled X, is missing P and has a value below its minimum. The
    // constraint 7 errors (§7.4.1 step 5), Y's then B's, come before B's
    // value (step 7). S's entry for B has an enableIf that reads Job.Name,
    // so template validation leaves B to job creation.
    let jt = format!(
        "{HEAD}jobEnvironments:\n- {{name: B, exclusiveGroup: G, envParameterDefinitions: \
         [{{name: N, type: INT, minValue: 5}}, {{name: P, type: STRING}}], variables: {{V: b}}}}\n\
         steps:\n- name: S\n  dependencies: [{{environment: B, enableIf: \"Job.Name == 'J'\", \
         parameters: {{N: 1}}}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    let y = env("name: Y, exclusiveGroup: H, envParameterDefinitions: [{name: P, type: STRING}]");
    let x = env("name: X, exclusiveGroup: G, enableIf: false");
    let err = create(&jt, &[&y, &x]).expect_err("expected job creation to fail");
    assert_eq!(
        err.to_string(),
        format!(
            "Model validation error: 3 validation errors for Submission\n{}\n{}\n\
             JobTemplate -> steps[0] -> dependencies[0] -> parameters -> N:\n\tParameter 'N': \
             value 1 is less than minimum 5",
            missing(
                "JobTemplate -> steps[0]",
                false,
                "'Y' (EnvironmentTemplate[0])"
            ),
            missing("JobTemplate -> steps[0]", true, "'B'"),
        )
    );
}

#[test]
fn a_failed_entry_enable_if_stops_every_step_it_fails_for() {
    // As a_failed_entry_enable_if_stops_the_step_of_a_grouped_job_environment,
    // for two Steps: each reports its entry's error, and neither J2's
    // value. Without J2's N, template validation reports it for both.
    let jt = |j2_entry: &str| {
        format!(
            "{HEAD}parameterDefinitions: [{{name: L, type: 'LIST[INT]', default: [1]}}]\n\
             jobEnvironments:\n- {{name: J1, exclusiveGroup: G, variables: {{V: a}}}}\n\
             - {{name: J2, exclusiveGroup: G, envParameterDefinitions: [{{name: N, type: INT, \
             minValue: 5}}], variables: {{V: b}}}}\nsteps:\n\
             - name: S0\n  dependencies: [{{environment: J1, enableIf: \"Param.L[4] == 1\"}}\
             {j2_entry}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n\
             - name: S1\n  dependencies: [{{environment: J1, enableIf: \"Param.L[4] == 1\"}}\
             {j2_entry}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    let failed = |step: usize| {
        format!(
            "steps[{step}] -> dependencies[0] -> enableIf:\n\tcould not be evaluated: Index 4 out \
             of bounds for list of length 1"
        )
    };
    create_err(
        &jt(", {environment: J2, parameters: {N: 1}}"),
        &[],
        "JobTemplate",
        &[&failed(0), &failed(1)],
    );
    job_err(
        &jt(""),
        &[
            &missing_named("steps[0]", true, "'J2'", "N"),
            &missing_named("steps[1]", true, "'J2'", "N"),
        ],
    );
}

#[test]
fn a_deferred_job_environments_failed_default_names_the_step_that_takes_it() {
    // B is deferred behind the disabled X. N's default fails; S0 supplies
    // N and S1 takes the default, so the error names S1.
    let jt = format!(
        "{HEAD}parameterDefinitions: [{{name: L, type: 'LIST[INT]', default: [1]}}]\n\
         jobEnvironments:\n- {{name: B, exclusiveGroup: G, envParameterDefinitions: [{{name: N, \
         type: INT, default: '{{{{Param.L[4]}}}}'}}], variables: {{V: b}}}}\nsteps:\n\
         - name: S0\n  dependencies: [{{environment: B, parameters: {{N: 1}}}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: S1\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    create_err(
        &jt,
        &[&env("name: X, exclusiveGroup: G, enableIf: false")],
        "Submission",
        &[
            "JobTemplate -> jobEnvironments[0] -> envParameterDefinitions[0] -> default:\n\tfor \
           Step 'S1', which does not supply this parameter: could not be resolved as a value of \
           type INT: Index 4 out of bounds for list of length 1",
        ],
    );
}
