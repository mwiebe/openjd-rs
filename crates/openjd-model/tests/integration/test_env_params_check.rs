// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `ENV_PARAMS`: `check_environment_templates`, the checks of
//! `create_job_with_environment_templates`'s submission stage that need no
//! Job Parameter values (Template Schemas §3.2, last paragraph; prototype
//! plan A9).
//!
//! Each rejection is compared with what
//! `create_job_with_environment_templates` reports for the same submission,
//! so `openjd check --environment` and `openjd run` give the same messages.

use openjd_model::template::{EnvironmentTemplate, JobTemplate};
use openjd_model::{
    check_environment_templates, create_job_with_environment_templates,
    AttachedEnvironmentTemplate, ModelError, SubmissionWarning,
};

use crate::env_params_common::{assert_errors, env_ok, job_ok, job_params_with, posix_ctx};

const CONDA: &str = include_str!("../fixtures/env_params/conda-env-params.yaml");
const DOCKER: &str = include_str!("../fixtures/env_params/docker-env-params.yaml");
const VIDEO: &str = include_str!("../fixtures/env_params/video-sfm-composite.yaml");

fn attach(envs: &[EnvironmentTemplate]) -> Vec<AttachedEnvironmentTemplate<'_>> {
    envs.iter().map(AttachedEnvironmentTemplate::new).collect()
}

fn check(jt: &JobTemplate, envs: &[EnvironmentTemplate]) -> Result<Vec<String>, ModelError> {
    check_environment_templates(jt, &attach(envs), &posix_ctx(jt))
        .map(|w| w.iter().map(SubmissionWarning::to_string).collect())
}

/// What `openjd run` reports for the same submission:
/// `create_job_with_environment_templates`, with `params`.
fn apply(
    jt: &JobTemplate,
    envs: &[EnvironmentTemplate],
    params: &[(&str, &str)],
) -> Result<Vec<String>, ModelError> {
    let values = job_params_with(jt, params, envs);
    let ctx = posix_ctx(jt);
    create_job_with_environment_templates(jt, &attach(envs), &values, &ctx).map(|c| {
        c.warnings
            .iter()
            .map(SubmissionWarning::to_string)
            .collect()
    })
}

/// `check` rejects the submission with exactly `expected`, and job
/// creation rejects it with the same message.
fn check_err_same_as_apply(jt_src: &str, env_srcs: &[&str], expected: &[&str]) {
    let jt = job_ok(jt_src);
    let envs: Vec<EnvironmentTemplate> = env_srcs.iter().map(|s| env_ok(s)).collect();
    let checked = check(&jt, &envs).expect_err("expected check to reject the submission");
    assert_errors(&checked.to_string(), "Submission", expected);
    let applied = apply(&jt, &envs, &[]).expect_err("expected apply to reject the submission");
    assert_eq!(checked.to_string(), applied.to_string());
}

fn steps_naming(env: &str, required: bool, params: &str) -> String {
    format!(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  dependencies:
  - environment: {env}
    required: {required}
    parameters: {params}
  script: {{actions: {{onRun: {{command: x}}}}}}
"#
    )
}

// ── The RFC samples ─────────────────────────────────────────────────

#[test]
fn the_sample_job_passes_with_each_sample_environment_and_with_none() {
    let jt = job_ok(VIDEO);
    // The Conda entries' `enableIf` is an expression (`'Docker' not in
    // Job.EnvNames`), so `check` leaves them to job creation and warns only
    // for the Docker entries, when no Docker Environment is attached. Job
    // creation disables the Conda entries when Docker is attached.
    for (envs, warned) in [(&[CONDA][..], 4), (&[DOCKER], 0), (&[CONDA, DOCKER], 0)] {
        let envs: Vec<EnvironmentTemplate> = envs.iter().map(|s| env_ok(s)).collect();
        let warnings = check(&jt, &envs).unwrap_or_else(|e| panic!("check failed:\n{e}"));
        assert_eq!(warnings.len(), warned, "{warnings:?}");
        assert_eq!(warnings, apply(&jt, &envs, &[]).unwrap());
    }
    let docker_warning = "JobTemplate -> steps[0] -> dependencies[1] -> environment: names \
         Environment 'Docker', which is not a jobEnvironments entry of the Job Template, and no \
         Environment Template is attached. The entry has 'required: false', so it has no effect.";
    let warnings = check(&jt, &[]).unwrap();
    assert_eq!(warnings.len(), 4, "{warnings:?}");
    assert_eq!(warnings[0], docker_warning);
    // Job creation also enables the Conda entries, which find nothing.
    let applied = apply(&jt, &[], &[]).unwrap();
    assert_eq!(applied.len(), 8, "{applied:?}");
    assert_eq!(
        applied[0],
        "JobTemplate -> steps[0] -> dependencies[0] -> environment: names Environment 'Conda', \
         which is not a jobEnvironments entry of the Job Template, and no Environment Template \
         is attached. The entry has 'required: false', so it has no effect."
    );
    assert_eq!(applied[1], docker_warning);
}

// ── §3.2.1 items 1–3: resolution ────────────────────────────────────

#[test]
fn a_missing_required_environment_is_rejected() {
    check_err_same_as_apply(
        &steps_naming("Conda", true, "{Packages: x}"),
        &[],
        &[
            "JobTemplate -> steps[0] -> dependencies[0] -> environment:\n\tnames Environment \
           'Conda', which is not a jobEnvironments entry of the Job Template, and no \
           Environment Template is attached. The entry is required; set 'required: false' if \
           the Step can run without the Environment (Template Schemas §3.2.1 item 2).",
        ],
    );
}

#[test]
fn an_ambiguous_name_is_rejected() {
    check_err_same_as_apply(
        &steps_naming("Conda", false, "{Packages: x}"),
        &[CONDA, CONDA],
        &[
            "JobTemplate -> steps[0] -> dependencies[0] -> environment:\n\tnames Environment \
           'Conda', which 2 attached Environment Templates define (EnvironmentTemplate[0], \
           EnvironmentTemplate[1]); an entry must match exactly one (Template Schemas §3.2.1 \
           item 3).",
        ],
    );
}

#[test]
fn two_attachments_of_one_name_that_no_entry_names_are_accepted() {
    // Plan A11.
    let jt = job_ok(
        "specificationVersion: jobtemplate-2023-09\nname: J\n\
         steps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n",
    );
    let envs = [env_ok(CONDA), env_ok(CONDA)];
    assert_eq!(check(&jt, &envs).unwrap(), Vec::<String>::new());
}

// ── Constraints 5–7 against the resolved Environment ────────────────

#[test]
fn an_undeclared_key_is_rejected() {
    check_err_same_as_apply(
        &steps_naming("Conda", false, "{Packages: x, Bogus: y}"),
        &[CONDA],
        &[
            "JobTemplate -> steps[0] -> dependencies[0] -> parameters -> Bogus:\n\tEnvironment \
           'Conda' (EnvironmentTemplate[0]) declares no parameter 'Bogus'. Declared: Packages, \
           Channels.",
        ],
    );
}

#[test]
fn a_literal_of_the_wrong_type_is_rejected() {
    let wrong_type = "JobTemplate -> steps[0] -> dependencies[0] -> parameters -> Packages:\n\tis \
                      not a valid STRING value: invalid type: integer `7`, expected a string";
    // Without an `enableIf` the Environment is certainly not disabled, so
    // check rejects the value, as job creation does.
    let conda = CONDA.replace("  enableIf: \"Env.Param.Packages != ''\"\n", "");
    assert_ne!(conda, CONDA);
    check_err_same_as_apply(
        &steps_naming("Conda", false, "{Packages: 7}"),
        &[&conda],
        &[wrong_type],
    );
    // The sample's `enableIf` reads only Packages, a literal that does not
    // convert, so it is certainly unresolved for the Step: the Environment
    // is not disabled, and check rejects the value too (Template Schemas
    // §3.2.2).
    check_err_same_as_apply(
        &steps_naming("Conda", false, "{Packages: 7}"),
        &[CONDA],
        &[wrong_type],
    );
    // An `enableIf` that also reads a Job Parameter is undetermined before
    // job creation (§7.5 rule 5), so check leaves the value to job
    // creation, which rejects it.
    let conda = CONDA.replace(
        "\"Env.Param.Packages != ''\"",
        "\"Env.Param.Packages != '' and Param.CondaPackages != 'x'\"",
    );
    assert_ne!(conda, CONDA);
    let jt = job_ok(&steps_naming("Conda", false, "{Packages: 7}"));
    let envs = vec![env_ok(&conda)];
    assert_eq!(check(&jt, &envs).unwrap(), Vec::<String>::new());
    let applied = apply(&jt, &envs, &[]).expect_err("expected apply to reject the submission");
    assert_errors(&applied.to_string(), "Submission", &[wrong_type]);
}

#[test]
fn a_relative_literal_path_is_rejected() {
    let env = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions: [{name: Dir, type: PATH, default: /d}]
  variables: {D: "{{Env.Param.Dir}}"}
"#;
    check_err_same_as_apply(
        &steps_naming("E", true, "{Dir: rel/dir}"),
        &[env],
        &[
            "JobTemplate -> steps[0] -> dependencies[0] -> parameters -> Dir:\n\tmust be an \
           absolute path in the submission path format (POSIX), since an Environment parameter \
           has no directory to be relative to; got 'rel/dir'.",
        ],
    );
}

#[test]
fn a_parameter_without_a_default_must_be_supplied_by_every_step() {
    let env = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions: [{name: Need, type: STRING}]
  variables: {N: "{{Env.Param.Need}}"}
"#;
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: Gives
  dependencies: [{environment: E, parameters: {Need: n}}]
  script: {actions: {onRun: {command: x}}}
- name: Omits
  script: {actions: {onRun: {command: x}}}
"#;
    check_err_same_as_apply(
        jt,
        &[env],
        &[
            "JobTemplate -> steps[1]:\n\tmust supply, in an 'environment:' dependency, every \
           parameter without a default of each Environment its Sessions enter (every attached \
           Environment with activation ALWAYS, and each one with activation ON_REQUEST that the \
           Step requests; Template Schemas §3.2.1); missing: 'Need' of Environment 'E' \
           (EnvironmentTemplate[0]).",
        ],
    );
}

#[test]
fn a_list_parameter_written_as_a_multi_segment_format_string_is_rejected() {
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  dependencies:
  - environment: Docker
    parameters: {Mounts: "/a{{ 'b' }}"}
  script: {actions: {onRun: {command: x}}}
"#;
    check_err_same_as_apply(
        jt,
        &[DOCKER],
        &[
            "JobTemplate -> steps[0] -> dependencies[0] -> parameters -> Mounts:\n\ta LIST[PATH] \
           parameter has no string form, so a format-string value for it must be a single \
           '{{ }}' expression.",
        ],
    );
}

#[test]
fn format_string_values_are_left_to_submission() {
    // An INT parameter given a format string that will not convert to an
    // INT for these Job Parameter values: check cannot know, apply
    // rejects.
    let env = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions: [{name: N, type: INT, default: 1}]
  variables: {N: "{{Env.Param.N}}"}
"#;
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: S, type: STRING, default: "abc"}]
steps:
- name: S
  dependencies: [{environment: E, parameters: {N: "{{Param.S}}"}}]
  script: {actions: {onRun: {command: x}}}
"#;
    let jt = job_ok(jt);
    let envs = [env_ok(env)];
    assert_eq!(check(&jt, &envs).unwrap(), Vec::<String>::new());
    let err = apply(&jt, &envs, &[]).unwrap_err().to_string();
    assert_errors(
        &err,
        "Submission",
        &[
            "JobTemplate -> steps[0] -> dependencies[0] -> parameters -> N:\n\tcould not be \
           resolved as a value of type INT: Cannot convert 'abc' to int: invalid digit found in \
           string\n  Param.S\n  ~~~~~~^",
        ],
    );
    // With a value that parses, apply accepts it too.
    assert!(apply(&jt, &envs, &[("S", "5")]).is_ok());
}

// ── Attachment-level conditions ─────────────────────────────────────

#[test]
fn an_attachment_shadowed_by_a_job_environment_warns_when_the_job_lists_env_params() {
    let shadowing = |extensions: &str| {
        format!(
            "specificationVersion: jobtemplate-2023-09\n{extensions}name: J\n\
             jobEnvironments:\n- name: Conda\n  variables: {{A: a}}\n\
             steps:\n- name: S\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    let envs = [env_ok(CONDA)];

    let jt = job_ok(&shadowing("extensions: [ENV_PARAMS, EXPR]\n"));
    let warnings = check(&jt, &envs).unwrap();
    assert_eq!(
        warnings,
        [
            "EnvironmentTemplate[0] -> environment -> name: Environment 'Conda' has the same name \
          as a jobEnvironments entry of the Job Template, so no 'environment:' dependency of the \
          Job Template can reach it (Template Schemas §3.2.1 item 4)."
        ]
    );
    assert_eq!(warnings, apply(&jt, &envs, &[]).unwrap());

    // Without ENV_PARAMS the Job Template has no entries that could reach
    // any attachment, and the submission behaves as before the extension:
    // no warning (RFC Backward compatibility).
    let jt = job_ok(&shadowing(""));
    assert_eq!(check(&jt, &envs).unwrap(), Vec::<String>::new());
    assert_eq!(apply(&jt, &envs, &[]).unwrap(), Vec::<String>::new());
}

#[test]
fn an_attachments_uri_default_needs_the_callers_permission() {
    // A URI is a PATH value (§4.5 PATH paragraph) when the caller permits
    // URI path values, whatever the Job Template lists.
    let env = "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
               environment:\n  name: Data\n  envParameterDefinitions: [{name: Where, type: PATH, \
               default: \"s3://bucket/key\"}]\n  variables: {W: \"{{Env.Param.Where}}\"}\n";
    let jt = "specificationVersion: jobtemplate-2023-09\nname: J\n\
              steps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n";
    check_err_same_as_apply(
        jt,
        &[env],
        &[
            "EnvironmentTemplate[0] -> environment -> envParameterDefinitions[0] -> default:\n\tis a \
           URI ('s3://bucket/key'), and URI path values are not permitted for this submission.",
        ],
    );
    let jt = job_ok(jt);
    let envs = [env_ok(env)];
    let ctx = posix_ctx(&jt).with_allow_uri_path_values(true);
    assert!(check_environment_templates(&jt, &attach(&envs), &ctx).is_ok());
}

#[test]
fn a_bad_literal_default_of_an_attachment_is_rejected() {
    // Valid at template validation (any string is a PATH there); relative
    // in every submission path format.
    let env = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions: [{name: Dir, type: PATH, default: rel}]
  variables: {D: "{{Env.Param.Dir}}"}
"#;
    check_err_same_as_apply(
        "specificationVersion: jobtemplate-2023-09\nname: J\n\
         steps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n",
        &[env],
        &[
            "EnvironmentTemplate[0] -> environment -> envParameterDefinitions[0] -> default:\n\tmust \
           be an absolute path in the submission path format (POSIX), since an Environment \
           parameter has no directory to be relative to; got 'rel'.",
        ],
    );
}

#[test]
fn an_attachment_named_like_a_step_environment_cannot_be_supplied() {
    let env = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions: [{name: Need, type: STRING}]
  variables: {N: "{{Env.Param.Need}}"}
"#;
    let jt = r#"
specificationVersion: jobtemplate-2023-09
name: J
steps:
- name: S
  stepEnvironments: [{name: E, variables: {A: a}}]
  script: {actions: {onRun: {command: x}}}
"#;
    check_err_same_as_apply(
        jt,
        &[env],
        &[
            "JobTemplate -> steps[0]:\n\tcannot supply the parameters without a default of \
           Environment 'E' (EnvironmentTemplate[0]): Need. It has the same name as a \
           stepEnvironments entry of this Step, so no 'environment:' dependency of this Step \
           can reach it (Template Schemas §3.2.1 item 5), and its activation is ALWAYS, so every \
           Session enters it.",
        ],
    );
}

#[test]
fn labels_name_the_attachments() {
    let jt = job_ok(&steps_naming("Conda", false, "{Bogus: y}"));
    let envs = [env_ok(CONDA)];
    let attached = [AttachedEnvironmentTemplate::new(&envs[0]).with_label("conda.yaml")];
    let err = check_environment_templates(&jt, &attached, &posix_ctx(&jt)).unwrap_err();
    assert_errors(
        &err.to_string(),
        "Submission",
        &[
            "JobTemplate -> steps[0] -> dependencies[0] -> parameters -> Bogus:\n\tEnvironment \
           'Conda' (conda.yaml) declares no parameter 'Bogus'. Declared: Packages, Channels.",
        ],
    );
}

// ── Constraint 7 names a disabled entry (B2-3) ──────────────────────

const DISABLED_NOTE: &str = " (the Step's 'environment:' dependency for it is disabled by its \
     enableIf, so it supplies nothing; Template Schemas §3.2 item 5)";

const REQUIRED_ATTACHED: &str = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: Conda
  envParameterDefinitions: [{name: Packages, type: STRING}]
  variables: {V: "{{Env.Param.Packages}}"}
"#;

fn entry_with_enable_if(enable_if: &str) -> String {
    format!(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{{name: Mode, type: STRING, default: "on"}}]
steps:
- name: S
  dependencies:
  - environment: Conda
    enableIf: {enable_if}
    parameters: {{Packages: x}}
  script: {{actions: {{onRun: {{command: x}}}}}}
"#
    )
}

fn attached_missing(note: &str) -> String {
    format!(
        "JobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every \
         parameter without a default of each Environment its Sessions enter (every attached \
         Environment with activation ALWAYS, and each one with activation ON_REQUEST that the Step \
         requests; Template Schemas §3.2.1); missing: 'Packages' of Environment 'Conda' \
         (EnvironmentTemplate[0]){note}."
    )
}

#[test]
fn a_literally_disabled_entry_is_named_in_the_missing_parameters_message() {
    check_err_same_as_apply(
        &entry_with_enable_if("false"),
        &[REQUIRED_ATTACHED],
        &[&attached_missing(DISABLED_NOTE)],
    );
}

#[test]
fn an_entry_disabled_by_an_expression_is_named_at_job_creation() {
    let jt = job_ok(&entry_with_enable_if(r#""Param.Mode == 'on'""#));
    let envs = [env_ok(REQUIRED_ATTACHED)];
    // `check` cannot tell whether the entry is enabled.
    assert_eq!(check(&jt, &envs).unwrap(), Vec::<String>::new());
    apply(&jt, &envs, &[]).expect("enabled, the entry supplies Packages");
    let err = apply(&jt, &envs, &[("Mode", "off")]).unwrap_err();
    assert_errors(
        &err.to_string(),
        "Submission",
        &[&attached_missing(DISABLED_NOTE)],
    );
}

#[test]
fn an_absent_entry_gets_no_disabled_note() {
    let src = entry_with_enable_if("true").replace(
        "  - environment: Conda\n    enableIf: true\n    parameters: {Packages: x}\n",
        "  - environment: Other\n    required: false\n",
    );
    let jt = job_ok(&src);
    let envs = [env_ok(REQUIRED_ATTACHED)];
    let err = check(&jt, &envs).unwrap_err();
    assert_errors(&err.to_string(), "Submission", &[&attached_missing("")]);
}

// ── WRAP_ACTIONS single-layer rule with attached Environments ───────

fn wrapping_attached(name: &str, scope: &str) -> String {
    format!(
        r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR, WRAP_ACTIONS]
environment:
  name: {name}
  activation: {scope}
  script:
    actions:
      onWrapEnvEnter: {{command: x}}
      onWrapTaskRun: {{command: x}}
      onWrapEnvExit: {{command: x}}
"#
    )
}

fn wrapping_job(dependencies: &str) -> String {
    format!(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR, WRAP_ACTIONS]
name: J
parameterDefinitions: [{{name: Mode, type: STRING, default: "off"}}]
jobEnvironments:
- name: Own
  script:
    actions:
      onWrapEnvEnter: {{command: x}}
      onWrapTaskRun: {{command: x}}
      onWrapEnvExit: {{command: x}}
steps:
- name: S
{dependencies}  script: {{actions: {{onRun: {{command: x}}}}}}
"#
    )
}

const TWO_LAYERS: &str = "JobTemplate -> steps[0]:\n\tits Sessions would enter 2 Environments \
     that define wrap hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'Docker' \
     (EnvironmentTemplate[0]), 'Own'. A Session may have at most one Environment defining wrap \
     hooks (RFC 0008).";

#[test]
fn an_attached_wrapper_with_activation_all_is_a_second_layer() {
    check_err_same_as_apply(
        &wrapping_job(""),
        &[&wrapping_attached("Docker", "ALWAYS")],
        &[TWO_LAYERS],
    );
}

#[test]
fn a_requested_on_request_attached_wrapper_is_a_second_layer() {
    let deps = "  dependencies: [{environment: Docker}]\n";
    check_err_same_as_apply(
        &wrapping_job(deps),
        &[&wrapping_attached("Docker", "ON_REQUEST")],
        &[TWO_LAYERS],
    );
}

#[test]
fn an_unrequested_or_deferred_attached_wrapper_passes_check() {
    let att = [env_ok(&wrapping_attached("Docker", "ON_REQUEST"))];
    // Not requested: never entered.
    let jt = job_ok(&wrapping_job(""));
    assert_eq!(check(&jt, &att).unwrap(), Vec::<String>::new());
    apply(&jt, &att, &[]).unwrap();
    // Requested by an entry whose enableIf is an expression: decided at
    // job creation, which rejects it when the entry is enabled.
    let jt = job_ok(&wrapping_job(
        "  dependencies:\n  - environment: Docker\n    enableIf: \"Param.Mode == 'on'\"\n",
    ));
    assert_eq!(check(&jt, &att).unwrap(), Vec::<String>::new());
    apply(&jt, &att, &[]).unwrap();
    let err = apply(&jt, &att, &[("Mode", "on")]).unwrap_err();
    assert_errors(&err.to_string(), "Submission", &[TWO_LAYERS]);
}

// ── Literal host requirements combined (§3.3.3) ─────────────────────

fn gpu_attached(min: &str) -> String {
    format!(
        r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR, FEATURE_BUNDLE_1]
environment:
  name: Gpu
  envParameterDefinitions: [{{name: N, type: INT, default: 2}}]
  hostRequirements:
    amounts: [{{name: amount.worker.gpu, min: "{min}"}}]
    attributes: [{{name: attr.worker.os.family, anyOf: [linux]}}]
  variables: {{V: v}}
"#
    )
}

fn gpu_job(step_requirements: &str) -> String {
    format!(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR, FEATURE_BUNDLE_1]
name: J
steps:
- name: S
  hostRequirements:
{step_requirements}  script: {{actions: {{onRun: {{command: x}}}}}}
"#
    )
}

#[test]
fn a_literal_amount_conflict_fails_check_as_at_job_creation() {
    check_err_same_as_apply(
        &gpu_job("    amounts: [{name: amount.worker.gpu, max: 1}]\n"),
        &[&gpu_attached("2")],
        &[
            "JobTemplate -> steps[0]:\n\tStep 'S': no host can satisfy the combined host \
           requirements for amount 'amount.worker.gpu': EnvironmentTemplate[0] -> environment \
           -> hostRequirements (Environment 'Gpu') requires min 2, and JobTemplate -> steps[0] \
           -> hostRequirements requires max 1. Template Schemas §3.3.3.",
        ],
    );
}

#[test]
fn a_literal_any_of_conflict_fails_check_as_at_job_creation() {
    check_err_same_as_apply(
        &gpu_job("    attributes: [{name: attr.worker.os.family, anyOf: [windows]}]\n"),
        &[&gpu_attached("2")],
        &[
            "JobTemplate -> steps[0]:\n\tStep 'S': no host can satisfy the combined host \
           requirements for attribute 'attr.worker.os.family', of which a host has exactly one value: the anyOf and allOf values of JobTemplate \
           -> steps[0] -> hostRequirements and EnvironmentTemplate[0] -> environment -> \
           hostRequirements (Environment 'Gpu') have no value in common. Template Schemas \
           §3.3.3.",
        ],
    );
}

#[test]
fn a_non_literal_requirement_is_left_to_job_creation() {
    // The Environment's gpu amount reads Env.Param.N, so check skips it.
    let jt = job_ok(&gpu_job(
        "    amounts: [{name: amount.worker.gpu, max: 1}]\n",
    ));
    let att = [env_ok(&gpu_attached("{{ Env.Param.N }}"))];
    assert_eq!(check(&jt, &att).unwrap(), Vec::<String>::new());
    let err = apply(&jt, &att, &[]).unwrap_err();
    assert_errors(
        &err.to_string(),
        "Submission",
        &[
            "JobTemplate -> steps[0]:\n\tStep 'S': no host can satisfy the combined host \
           requirements for amount 'amount.worker.gpu': EnvironmentTemplate[0] -> environment \
           -> hostRequirements (Environment 'Gpu') requires min 2, and JobTemplate -> steps[0] \
           -> hostRequirements requires max 1. Template Schemas §3.3.3.",
        ],
    );
    // A non-literal Step requirement is skipped, so the literal sources
    // alone show no conflict.
    let jt = job_ok(&gpu_job(
        "    amounts: [{name: amount.worker.gpu, max: \"{{ 1 }}\"}]\n",
    ));
    assert_eq!(
        check(&jt, &[env_ok(&gpu_attached("2"))]).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn the_literal_requirements_of_a_partly_literal_source_are_combined() {
    // The Environment's gpu amount is an expression, its os family anyOf
    // is literal, and the Step's conflicts with that.
    check_err_same_as_apply(
        &gpu_job("    attributes: [{name: attr.worker.os.family, anyOf: [windows]}]\n"),
        &[&gpu_attached("{{ Env.Param.N }}")],
        &[
            "JobTemplate -> steps[0]:\n\tStep 'S': no host can satisfy the combined host \
           requirements for attribute 'attr.worker.os.family', of which a host has exactly one value: the anyOf and allOf values of JobTemplate \
           -> steps[0] -> hostRequirements and EnvironmentTemplate[0] -> environment -> \
           hostRequirements (Environment 'Gpu') have no value in common. Template Schemas \
           §3.3.3.",
        ],
    );
}

#[test]
fn an_environment_a_step_does_not_enter_contributes_nothing_at_check() {
    let src = gpu_job("    amounts: [{name: amount.worker.gpu, max: 1}]\n");
    let att = gpu_attached("2").replace("  name: Gpu\n", "  name: Gpu\n  activation: ON_REQUEST\n");
    let jt = job_ok(&src);
    let envs = [env_ok(&att)];
    assert_eq!(check(&jt, &envs).unwrap(), Vec::<String>::new());
    apply(&jt, &envs, &[]).unwrap();
    // A literal-false entry does not request it either.
    let jt = job_ok(&src.replace(
        "  hostRequirements:\n",
        "  dependencies: [{environment: Gpu, enableIf: false}]\n  hostRequirements:\n",
    ));
    assert_eq!(check(&jt, &envs).unwrap(), Vec::<String>::new());
    // Requested, it conflicts.
    check_err_same_as_apply(
        &src.replace(
            "  hostRequirements:\n",
            "  dependencies: [{environment: Gpu}]\n  hostRequirements:\n",
        ),
        &[&att],
        &[
            "JobTemplate -> steps[0]:\n\tStep 'S': no host can satisfy the combined host \
           requirements for amount 'amount.worker.gpu': EnvironmentTemplate[0] -> environment \
           -> hostRequirements (Environment 'Gpu') requires min 2, and JobTemplate -> steps[0] \
           -> hostRequirements requires max 1. Template Schemas §3.3.3.",
        ],
    );
}

#[test]
fn compatible_literal_requirements_pass_check() {
    let jt = job_ok(&gpu_job(
        "    amounts: [{name: amount.worker.gpu, max: 4}]\n",
    ));
    let att = [env_ok(&gpu_attached("2"))];
    assert_eq!(check(&jt, &att).unwrap(), Vec::<String>::new());
    apply(&jt, &att, &[]).unwrap();
}

// ── Audit round 1: the Environments a Step certainly enters ─────────

const DOCKER_OWN: &str = "JobTemplate -> steps[0]:\n\tits Sessions would enter 2 Environments \
     that define wrap hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'Docker' \
     (EnvironmentTemplate[0]), 'Own'. A Session may have at most one Environment defining wrap \
     hooks (RFC 0008).";

#[test]
fn an_on_request_job_environment_counts_as_a_layer_when_literally_requested() {
    let on_request = |deps: &str| {
        wrapping_job(deps).replace("- name: Own\n", "- name: Own\n  activation: ON_REQUEST\n")
    };
    let att = [env_ok(&wrapping_attached("Docker", "ALWAYS"))];
    // Not requested: one layer.
    let jt = job_ok(&on_request(""));
    assert_eq!(check(&jt, &att).unwrap(), Vec::<String>::new());
    // Requested by an entry whose enableIf is absent: two layers.
    check_err_same_as_apply(
        &on_request("  dependencies: [{environment: Own}]\n"),
        &[&wrapping_attached("Docker", "ALWAYS")],
        &[DOCKER_OWN],
    );
    // Requested by an entry whose enableIf is an expression: left to job
    // creation, which rejects it when the entry is enabled.
    let jt = job_ok(&on_request(
        "  dependencies:\n  - environment: Own\n    enableIf: \"Param.Mode == 'on'\"\n",
    ));
    assert_eq!(check(&jt, &att).unwrap(), Vec::<String>::new());
    apply(&jt, &att, &[]).unwrap();
    let err = apply(&jt, &att, &[("Mode", "on")]).unwrap_err();
    assert_errors(&err.to_string(), "Submission", &[DOCKER_OWN]);
}

#[test]
fn a_wrapping_step_environment_counts_as_a_layer_with_an_attached_one() {
    let src = wrapping_job("")
        .replace(
            "jobEnvironments:\n- name: Own\n",
            "jobEnvironments:\n- name: Own\n  activation: ON_REQUEST\n",
        )
        .replace(
            "- name: S\n",
            "- name: S\n  stepEnvironments:\n  - name: Inner\n    script:\n      actions:\n        \
             onWrapEnvEnter: {command: x}\n        onWrapTaskRun: {command: x}\n        \
             onWrapEnvExit: {command: x}\n",
        );
    check_err_same_as_apply(
        &src,
        &[&wrapping_attached("Docker", "ALWAYS")],
        &[
            "JobTemplate -> steps[0]:\n\tits Sessions would enter 2 Environments that define wrap \
           hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'Docker' \
           (EnvironmentTemplate[0]), 'Inner' (stepEnvironments). A Session may have at most one \
           Environment defining wrap hooks (RFC 0008).",
        ],
    );
}

/// The Gpu attachment with one more requirement written as `extra`.
fn gpu_attached_with(extra_amount: &str, extra_attribute: &str) -> String {
    gpu_attached("2")
        .replace(
            "    amounts: [{name: amount.worker.gpu, min: \"2\"}]\n",
            &format!("    amounts:\n    - {{name: amount.worker.gpu, min: \"2\"}}\n{extra_amount}"),
        )
        .replace(
            "    attributes: [{name: attr.worker.os.family, anyOf: [linux]}]\n",
            &format!(
                "    attributes:\n    - {{name: attr.worker.os.family, anyOf: [linux]}}\n{extra_attribute}"
            ),
        )
}

const GPU_CONFLICT: &str = "JobTemplate -> steps[0]:\n\tStep 'S': no host can satisfy the \
     combined host requirements for amount 'amount.worker.gpu': EnvironmentTemplate[0] -> \
     environment -> hostRequirements (Environment 'Gpu') requires min 2, and JobTemplate -> \
     steps[0] -> hostRequirements requires max 1. Template Schemas §3.3.3.";

#[test]
fn a_non_literal_name_or_value_does_not_hide_the_literal_requirements_beside_it() {
    let jt_src = gpu_job("    amounts: [{name: amount.worker.gpu, max: 1}]\n");
    let jt = job_ok(&jt_src);
    for (amount, attribute) in [
        (
            "    - {name: \"amount.custom.c{{ Env.Param.N }}\", min: 1}\n",
            "",
        ),
        (
            "",
            "    - {name: \"attr.custom.c{{ Env.Param.N }}\", anyOf: [x]}\n",
        ),
        (
            "",
            "    - {name: attr.custom.a, anyOf: [\"v{{ Env.Param.N }}\"]}\n",
        ),
        (
            "",
            "    - {name: attr.custom.b, allOf: [\"v{{ Env.Param.N }}\"]}\n",
        ),
    ] {
        let env = gpu_attached_with(amount, attribute);
        let envs = [env_ok(&env)];
        let err = check(&jt, &envs).unwrap_err();
        assert_errors(&err.to_string(), "Submission", &[GPU_CONFLICT]);
        assert_eq!(
            err.to_string(),
            apply(&jt, &envs, &[]).unwrap_err().to_string(),
            "{env}"
        );
    }
}

fn own_gpu_job(scope: &str, deps: &str) -> String {
    format!(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{{name: Mode, type: STRING, default: "off"}}]
jobEnvironments:
- name: Own
  activation: {scope}
  hostRequirements:
    amounts: [{{name: amount.worker.gpu, min: 2}}]
  variables: {{V: v}}
steps:
- name: S
  hostRequirements:
    amounts: [{{name: amount.worker.gpu, max: 1}}]
{deps}  script: {{actions: {{onRun: {{command: x}}}}}}
"#
    )
}

/// An attachment with an unrelated literal requirement, so the Step has
/// an attached source.
const UNRELATED: &str = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: Linux
  hostRequirements:
    attributes: [{name: attr.worker.os.family, anyOf: [linux]}]
  variables: {V: v}
"#;

const OWN_CONFLICT: &str = "JobTemplate -> steps[0]:\n\tStep 'S': no host can satisfy the \
     combined host requirements for amount 'amount.worker.gpu': JobTemplate -> \
     jobEnvironments[0] -> hostRequirements (Environment 'Own') requires min 2, and JobTemplate \
     -> steps[0] -> hostRequirements requires max 1. Template Schemas §3.3.3.";

#[test]
fn a_job_environments_literal_requirements_combine_when_the_step_enters_it() {
    // ALWAYS: entered.
    check_err_same_as_apply(&own_gpu_job("ALWAYS", ""), &[UNRELATED], &[OWN_CONFLICT]);
    // ON_REQUEST, requested by an entry whose enableIf is absent.
    check_err_same_as_apply(
        &own_gpu_job("ON_REQUEST", "  dependencies: [{environment: Own}]\n"),
        &[UNRELATED],
        &[OWN_CONFLICT],
    );
    // ON_REQUEST, not requested: not entered.
    let envs = [env_ok(UNRELATED)];
    let jt = job_ok(&own_gpu_job("ON_REQUEST", ""));
    assert_eq!(check(&jt, &envs).unwrap(), Vec::<String>::new());
    apply(&jt, &envs, &[]).unwrap();
    // ON_REQUEST, requested by an entry whose enableIf is an
    // expression: left to job creation.
    let jt = job_ok(&own_gpu_job(
        "ON_REQUEST",
        "  dependencies:\n  - environment: Own\n    enableIf: \"Param.Mode == 'on'\"\n",
    ));
    assert_eq!(check(&jt, &envs).unwrap(), Vec::<String>::new());
    apply(&jt, &envs, &[]).unwrap();
    let err = apply(&jt, &envs, &[("Mode", "on")]).unwrap_err();
    assert_errors(&err.to_string(), "Submission", &[OWN_CONFLICT]);
}

#[test]
fn an_ambiguous_attachment_with_parameters_contributes_no_host_requirements_at_check() {
    // Job creation computes no values for an attachment an ambiguous entry
    // names, so it skips its requirements; check reports only the
    // ambiguity, as job creation does.
    let jt = gpu_job("    amounts: [{name: amount.worker.gpu, max: 1}]\n").replace(
        "  hostRequirements:\n",
        "  dependencies: [{environment: Gpu}]\n  hostRequirements:\n",
    );
    check_err_same_as_apply(
        &jt,
        &[&gpu_attached("2"), &gpu_attached("2")],
        &[
            "JobTemplate -> steps[0] -> dependencies[0] -> environment:\n\tnames Environment \
           'Gpu', which 2 attached Environment Templates define (EnvironmentTemplate[0], \
           EnvironmentTemplate[1]); an entry must match exactly one (Template Schemas §3.2.1 \
           item 3).",
        ],
    );
}

#[test]
fn an_all_attachment_named_by_an_enable_if_expression_still_combines_at_check() {
    // The Step's Sessions enter an ALWAYS attachment whatever the entry's
    // enableIf decides, so its literal requirements conflict either way.
    let jt = gpu_job("    amounts: [{name: amount.worker.gpu, max: 1}]\n")
        .replace(
            "name: J\n",
            "name: J\nparameterDefinitions: [{name: Mode, type: STRING, default: \"off\"}]\n",
        )
        .replace(
            "  hostRequirements:\n",
            "  dependencies:\n  - environment: Gpu\n    enableIf: \"Param.Mode == 'on'\"\n  \
             hostRequirements:\n",
        );
    check_err_same_as_apply(&jt, &[&gpu_attached("2")], &[GPU_CONFLICT]);
    let jt = job_ok(&jt);
    let envs = [env_ok(&gpu_attached("2"))];
    let err = apply(&jt, &envs, &[("Mode", "on")]).unwrap_err();
    assert_errors(&err.to_string(), "Submission", &[GPU_CONFLICT]);
}

#[test]
fn an_ambiguous_attachment_without_parameters_still_combines_at_check() {
    // Job creation needs no values for an attachment without parameters,
    // so it combines its requirements despite the ambiguous entry, and so
    // does check.
    let jt = gpu_job("    amounts: [{name: amount.worker.gpu, max: 1}]\n").replace(
        "  hostRequirements:\n",
        "  dependencies: [{environment: Gpu}]\n  hostRequirements:\n",
    );
    let att = gpu_attached("2").replace(
        "  envParameterDefinitions: [{name: N, type: INT, default: 2}]\n",
        "",
    );
    check_err_same_as_apply(
        &jt,
        &[&att, &att],
        &[
            "JobTemplate -> steps[0] -> dependencies[0] -> environment:\n\tnames Environment \
             'Gpu', which 2 attached Environment Templates define (EnvironmentTemplate[0], \
             EnvironmentTemplate[1]); an entry must match exactly one (Template Schemas §3.2.1 \
             item 3).",
            // Both attachments are entered; the first sets the greatest min.
            GPU_CONFLICT,
        ],
    );
}

#[test]
fn a_job_environment_without_wrap_hooks_is_not_a_layer_at_check() {
    let src = wrapping_job("").replace(
        "- name: Own\n  script:\n    actions:\n      onWrapEnvEnter: {command: x}\n      \
         onWrapTaskRun: {command: x}\n      onWrapEnvExit: {command: x}\n",
        "- name: Plain\n  variables: {A: b}\n",
    );
    assert!(src.contains("- name: Plain\n"), "{src}");
    let jt = job_ok(&src);
    let att = [env_ok(&wrapping_attached("Docker", "ALWAYS"))];
    assert_eq!(check(&jt, &att).unwrap(), Vec::<String>::new());
    apply(&jt, &att, &[]).unwrap();
}

#[test]
fn a_step_environment_without_wrap_hooks_is_not_a_layer_at_check() {
    let src = wrapping_job("")
        .replace(
            "jobEnvironments:\n- name: Own\n",
            "jobEnvironments:\n- name: Own\n  activation: ON_REQUEST\n",
        )
        .replace(
            "- name: S\n",
            "- name: S\n  stepEnvironments:\n  - name: Plain\n    variables: {P: p}\n",
        );
    let jt = job_ok(&src);
    let att = [env_ok(&wrapping_attached("Docker", "ALWAYS"))];
    assert_eq!(check(&jt, &att).unwrap(), Vec::<String>::new());
    apply(&jt, &att, &[]).unwrap();
}

#[test]
fn check_reports_the_wrap_layer_error_before_the_host_requirements_conflict() {
    // As job creation does (job-creation.md, the submission stage).
    let att = wrapping_attached("W", "ON_REQUEST").replace(
        "  activation: ON_REQUEST\n",
        "  activation: ON_REQUEST\n  hostRequirements:\n    \
         attributes: [{name: attr.worker.os.family, anyOf: [linux]}]\n",
    );
    let src = wrapping_job("  dependencies: [{environment: W}]\n").replace(
        "  script: {actions: {onRun: {command: x}}}\n",
        "  hostRequirements:\n    attributes: [{name: attr.worker.os.family, anyOf: [windows]}]\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    check_err_same_as_apply(
        &src,
        &[&att],
        &[
            "JobTemplate -> steps[0]:\n\tits Sessions would enter 2 Environments that define \
             wrap hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'W' \
             (EnvironmentTemplate[0]), 'Own'. A Session may have at most one Environment \
             defining wrap hooks (RFC 0008).",
            "JobTemplate -> steps[0]:\n\tStep 'S': no host can satisfy the combined host \
             requirements for attribute 'attr.worker.os.family', of which a host has exactly one value: the anyOf and allOf values of \
             JobTemplate -> steps[0] -> hostRequirements and EnvironmentTemplate[0] -> \
             environment -> hostRequirements (Environment 'W') have no value in common. \
             Template Schemas §3.3.3.",
        ],
    );
    let jt = job_ok(&src);
    let envs = [env_ok(&att)];
    let checked = check(&jt, &envs).unwrap_err().to_string();
    let wrap = checked.find("would enter 2 Environments").unwrap();
    let host = checked.find("no host can satisfy").unwrap();
    assert!(wrap < host, "{checked}");
}

// ── Audit round 6: indexes past the first ───────────────────────────

const LINUX_ATTACHED: &str = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: Linux
  hostRequirements:
    attributes: [{name: attr.worker.os.family, anyOf: [linux]}]
  variables: {V: v}
"#;

#[test]
fn second_attachments_and_job_environments_are_combined_with_their_own_paths() {
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: First
  variables: {V: v}
- name: Own
  hostRequirements:
    attributes: [{name: attr.worker.os.family, anyOf: [windows]}]
  variables: {V: v}
steps:
- name: S
  hostRequirements:
    amounts: [{name: amount.worker.gpu, max: 1}]
  script: {actions: {onRun: {command: x}}}
"#;
    check_err_same_as_apply(
        jt,
        &[LINUX_ATTACHED, &gpu_attached("2")],
        &[
            "JobTemplate -> steps[0]:\n\tStep 'S': no host can satisfy the combined host \
             requirements for amount 'amount.worker.gpu': EnvironmentTemplate[1] -> environment \
             -> hostRequirements (Environment 'Gpu') requires min 2, and JobTemplate -> \
             steps[0] -> hostRequirements requires max 1. Template Schemas §3.3.3.",
            "JobTemplate -> steps[0]:\n\tStep 'S': no host can satisfy the combined host \
             requirements for attribute 'attr.worker.os.family', of which a host has exactly one value: the anyOf and allOf values of \
             EnvironmentTemplate[0] -> environment -> hostRequirements (Environment 'Linux'), \
             EnvironmentTemplate[1] -> environment -> hostRequirements (Environment 'Gpu'), \
             JobTemplate -> jobEnvironments[1] -> hostRequirements (Environment 'Own') have no \
             value in common. Template Schemas §3.3.3.",
        ],
    );
}

#[test]
fn a_second_steps_literal_conflict_is_reported_at_that_step() {
    let jt = gpu_job("    amounts: [{name: amount.worker.gpu, max: 1}]\n");
    let jt = jt.replace(
        "steps:\n- name: S\n",
        "steps:\n- name: First\n  script: {actions: {onRun: {command: x}}}\n- name: S\n  \
         dependencies: [{environment: Gpu}]\n",
    );
    let att = gpu_attached("2").replace("  name: Gpu\n", "  name: Gpu\n  activation: ON_REQUEST\n");
    check_err_same_as_apply(
        &jt,
        &[&att],
        &[&GPU_CONFLICT.replace("steps[0]", "steps[1]")],
    );
}

#[test]
fn a_second_steps_wrap_layers_are_checked_with_its_own_entries() {
    let src = wrapping_job("").replace(
        "steps:\n- name: S\n",
        "steps:\n- name: First\n  script: {actions: {onRun: {command: x}}}\n- name: S\n  \
         dependencies: [{environment: Docker}]\n",
    );
    check_err_same_as_apply(
        &src,
        &[&wrapping_attached("Docker", "ON_REQUEST")],
        &[&TWO_LAYERS.replace("steps[0]", "steps[1]")],
    );
}

#[test]
fn a_second_steps_disabled_entry_is_noted_for_that_step() {
    let src = entry_with_enable_if("false").replace(
        "steps:\n- name: S\n",
        "steps:\n- name: First\n  dependencies: [{environment: Conda, parameters: {Packages: y}}]\n  \
         script: {actions: {onRun: {command: x}}}\n- name: S\n",
    );
    check_err_same_as_apply(
        &src,
        &[REQUIRED_ATTACHED],
        &[&attached_missing(DISABLED_NOTE).replace("steps[0]", "steps[1]")],
    );
    // The expression form, decided at job creation, notes the second Step.
    let src = entry_with_enable_if(r#""Param.Mode == 'on'""#).replace(
        "steps:\n- name: S\n",
        "steps:\n- name: First\n  dependencies: [{environment: Conda, parameters: {Packages: y}}]\n  \
         script: {actions: {onRun: {command: x}}}\n- name: S\n",
    );
    let jt = job_ok(&src);
    let envs = [env_ok(REQUIRED_ATTACHED)];
    let err = apply(&jt, &envs, &[("Mode", "off")]).unwrap_err();
    assert_errors(
        &err.to_string(),
        "Submission",
        &[&attached_missing(DISABLED_NOTE).replace("steps[0]", "steps[1]")],
    );
}

#[test]
fn the_sample_docker_requirements_combine_per_step() {
    // The spec sample's Docker Environment requires Linux, and a GPU only
    // for Steps whose Gpus value is non-empty (SolveSfM, which also has its
    // own `min: 1`); every Step of the video sample requests it.
    let jt = job_ok(VIDEO);
    let envs = [env_ok(DOCKER)];
    let values = job_params_with(&jt, &[], &envs);
    let created =
        create_job_with_environment_templates(&jt, &attach(&envs), &values, &posix_ctx(&jt))
            .unwrap();
    let gpu_min = |step: &str| {
        let s = created.job.steps.iter().find(|s| s.name == step).unwrap();
        let hr = s.host_requirements.as_ref().unwrap();
        let attrs = hr.attributes.as_ref().unwrap();
        assert_eq!(attrs[0].name, "attr.worker.os.family", "{step}");
        assert_eq!(attrs[0].any_of, Some(vec!["linux".to_string()]), "{step}");
        let amounts = hr.amounts.as_ref().unwrap();
        assert_eq!(amounts[0].name, "amount.worker.gpu", "{step}");
        amounts[0].min
    };
    assert_eq!(gpu_min("DecodeFrames"), Some(0.0));
    assert_eq!(gpu_min("SolveSfM"), Some(1.0));
}
