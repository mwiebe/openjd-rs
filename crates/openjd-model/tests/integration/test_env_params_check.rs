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

use crate::env_params_common::{
    assert_errors, env_ok, job_ok, job_params_with, rendered_values, template_ctx,
};

const CONDA: &str = include_str!("../fixtures/env_params/conda-env-params.yaml");
const DOCKER: &str = include_str!("../fixtures/env_params/docker-env-params.yaml");
const VIDEO: &str = include_str!("../fixtures/env_params/video-sfm-composite.yaml");

fn attach(envs: &[EnvironmentTemplate]) -> Vec<AttachedEnvironmentTemplate<'_>> {
    envs.iter().map(AttachedEnvironmentTemplate::new).collect()
}

fn check(jt: &JobTemplate, envs: &[EnvironmentTemplate]) -> Result<Vec<String>, ModelError> {
    check_environment_templates(jt, &attach(envs), &template_ctx(jt))
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
    let ctx = template_ctx(jt);
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
    // Shaped by the inputs: `Submission` with attachments, `JobTemplate`
    // without.
    let model = if envs.is_empty() {
        "JobTemplate"
    } else {
        "Submission"
    };
    assert_errors(&checked.to_string(), model, expected);
    let applied = apply(&jt, &envs, &[]).expect_err("expected apply to reject the submission");
    assert_eq!(checked.to_string(), applied.to_string());
}

/// `check` rejects the submission with exactly `checked`, and job creation
/// with exactly `applied`: a Step with an error from §7.4.1 steps 1–4 gets
/// no checks from steps 5–8 at job creation, while `check` reports every
/// violation it is certain of.
fn check_err_and_apply_err(jt_src: &str, env_srcs: &[&str], checked: &[&str], applied: &[&str]) {
    let jt = job_ok(jt_src);
    let envs: Vec<EnvironmentTemplate> = env_srcs.iter().map(|s| env_ok(s)).collect();
    let err = check(&jt, &envs).expect_err("expected check to reject the submission");
    assert_errors(&err.to_string(), "Submission", checked);
    let err = apply(&jt, &envs, &[]).expect_err("expected apply to reject the submission");
    assert_errors(&err.to_string(), "Submission", applied);
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
    // Each Step has a `required: false` entry for each of Conda and
    // Docker, so `check` warns for the four entries of whichever one is not
    // attached. With both attached, their shared exclusiveGroup decides
    // which one each Step enters (§4 item 7).
    for (envs, warned) in [
        (&[CONDA][..], 4),
        (&[DOCKER], 4),
        (&[CONDA, DOCKER], 0),
        (&[DOCKER, CONDA], 0),
    ] {
        let envs: Vec<EnvironmentTemplate> = envs.iter().map(|s| env_ok(s)).collect();
        let warnings = check(&jt, &envs).unwrap_or_else(|e| panic!("check failed:\n{e}"));
        assert_eq!(warnings.len(), warned, "{warnings:?}");
        assert_eq!(warnings, apply(&jt, &envs, &[]).unwrap());
    }
    let docker_warning = "steps[0] -> dependencies[1] -> environment: names \
         Environment 'Docker', which is not a jobEnvironments entry of the Job Template, and no \
         Environment Template is attached. The entry has 'required: false', so it has no effect.";
    let warnings = check(&jt, &[]).unwrap();
    assert_eq!(warnings.len(), 8, "{warnings:?}");
    assert_eq!(
        warnings[0],
        "steps[0] -> dependencies[0] -> environment: names Environment 'Conda', which is not a jobEnvironments entry of the Job Template, and no Environment Template \
         is attached. The entry has 'required: false', so it has no effect."
    );
    assert_eq!(warnings[1], docker_warning);
    assert_eq!(warnings, apply(&jt, &[], &[]).unwrap());
}

// ── §3.2.1 items 1–3: resolution ────────────────────────────────────

#[test]
fn a_missing_required_environment_is_rejected() {
    check_err_same_as_apply(
        &steps_naming("Conda", true, "{Packages: x}"),
        &[],
        &[
            "steps[0] -> dependencies[0] -> environment:\n\tnames Environment \
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

/// An Environment Template written before ENV_PARAMS, declaring no
/// parameters.
const OLD_CONDA: &str = "specificationVersion: environment-2023-09\n\
                         environment: {name: Conda, script: {actions: {onEnter: {command: x}}}}\n";

#[test]
fn an_optional_entrys_parameters_to_an_environment_that_declares_none_are_ignored() {
    // §3.2.1 item 1: check and job creation both accept it, with the same
    // one warning, and a wrong-typed literal in the ignored values is not
    // checked.
    let jt = job_ok(&steps_naming(
        "Conda",
        false,
        "{Packages: [a, b], Channels: x}",
    ));
    let envs = vec![env_ok(OLD_CONDA)];
    let expected = [
        "JobTemplate -> steps[0] -> dependencies[0] -> parameters: Environment 'Conda' \
                     (EnvironmentTemplate[0]) declares no parameters, and this entry has \
                     'required: false', so its parameters are ignored: Channels, Packages. The \
                     Step's Sessions get no values from it (Template Schemas §3.2.1 item 1).",
    ];
    assert_eq!(check(&jt, &envs).unwrap(), expected);
    assert_eq!(apply(&jt, &envs, &[]).unwrap(), expected);
}

#[test]
fn a_required_entrys_parameters_to_an_environment_that_declares_none_are_rejected() {
    check_err_same_as_apply(
        &steps_naming("Conda", true, "{Packages: x}"),
        &[OLD_CONDA],
        &[
            "JobTemplate -> steps[0] -> dependencies[0] -> parameters:\n\tEnvironment 'Conda' \
           (EnvironmentTemplate[0]) declares no parameters, so this entry must not supply any; \
           it supplies Packages.",
        ],
    );
}

#[test]
fn a_disabled_optional_entry_to_an_environment_that_declares_none_gets_no_warning() {
    // §3.2.1 item 1 applies to an enabled entry only: a disabled one
    // supplies nothing whatever it names (§3.2 item 3.2), so neither check
    // nor job creation warns about its parameters.
    let jt = steps_naming("Conda", false, "{Packages: x}").replace(
        "    required: false\n",
        "    required: false\n    enableIf: false\n",
    );
    let jt = job_ok(&jt);
    let envs = vec![env_ok(OLD_CONDA)];
    assert_eq!(check(&jt, &envs).unwrap(), Vec::<String>::new());
    assert_eq!(apply(&jt, &envs, &[]).unwrap(), Vec::<String>::new());
}

/// Two Steps, the first with no entry; the second with entries for `Conda`
/// and `Other`, each `required: false` with parameters.
const TWO_STEPS_TWO_ENTRIES: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
- name: T
  dependencies:
  - {environment: Conda, required: false, parameters: {P: a}}
  - {environment: Other, required: false, parameters: {Q: b}}
  script: {actions: {onRun: {command: x}}}
"#;

#[test]
fn every_ignored_entry_of_a_later_step_gets_its_own_warning_with_the_documents_label() {
    // Each enabled `required: false` entry is decided on its own, on any
    // Step, and the warning names the attachment by its label.
    let jt = job_ok(TWO_STEPS_TWO_ENTRIES);
    let envs = vec![
        env_ok(OLD_CONDA),
        env_ok(&OLD_CONDA.replace("name: Conda", "name: Other")),
    ];
    let attached = vec![
        AttachedEnvironmentTemplate::new(&envs[0]).with_label("queue/conda.yaml"),
        AttachedEnvironmentTemplate::new(&envs[1]),
    ];
    let expected = [
        "JobTemplate -> steps[1] -> dependencies[0] -> parameters: Environment 'Conda' \
         (queue/conda.yaml) declares no parameters, and this entry has 'required: false', so its \
         parameters are ignored: P. The Step's Sessions get no values from it (Template Schemas \
         §3.2.1 item 1).",
        "JobTemplate -> steps[1] -> dependencies[1] -> parameters: Environment 'Other' \
         (EnvironmentTemplate[1]) declares no parameters, and this entry has 'required: false', \
         so its parameters are ignored: Q. The Step's Sessions get no values from it (Template \
         Schemas §3.2.1 item 1).",
    ];
    let checked: Vec<String> = check_environment_templates(&jt, &attached, &template_ctx(&jt))
        .unwrap()
        .iter()
        .map(SubmissionWarning::to_string)
        .collect();
    assert_eq!(checked, expected);
    let values = job_params_with(&jt, &[], &envs);
    let created =
        create_job_with_environment_templates(&jt, &attached, &values, &template_ctx(&jt)).unwrap();
    let applied: Vec<String> = created
        .warnings
        .iter()
        .map(SubmissionWarning::to_string)
        .collect();
    assert_eq!(applied, expected);
}

#[test]
fn an_ignored_entry_still_requests_an_on_request_environment() {
    // §3.2.1 item 1: the entry still refers to the attachment, so the Step
    // requests the ON_REQUEST `Lic`, and its literal host requirements
    // combine with the Step's (§3.3.3). They conflict, which both `check`
    // and job creation report.
    let lic = "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
               environment: {name: Lic, activation: ON_REQUEST, \
               hostRequirements: {amounts: [{name: amount.worker.vcpu, min: 8}]}, \
               script: {actions: {onEnter: {command: x}}}}\n";
    let jt = steps_naming("Lic", false, "{Product: maya}").replace(
        "  script:",
        "  hostRequirements: {amounts: [{name: amount.worker.vcpu, max: 4}]}\n  script:",
    );
    let jt_t = job_ok(&jt);
    let envs = vec![env_ok(lic)];
    let checked = check(&jt_t, &envs).expect_err("expected a host requirements conflict");
    let applied = apply(&jt_t, &envs, &[]).expect_err("expected a host requirements conflict");
    assert_eq!(checked.to_string(), applied.to_string());
    assert!(
        checked.to_string().contains("amount.worker.vcpu"),
        "{checked}"
    );
    // Without the conflict, both accept it with the warning, and the
    // Step's Sessions enter `Lic`.
    let jt_t = job_ok(&jt.replace("max: 4", "max: 16"));
    let warnings = check(&jt_t, &envs).unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].contains("Environment 'Lic' (EnvironmentTemplate[0]) declares no parameters"),
        "{warnings:?}"
    );
    let values = job_params_with(&jt_t, &[], &envs);
    let created =
        create_job_with_environment_templates(&jt_t, &attach(&envs), &values, &template_ctx(&jt_t))
            .unwrap();
    assert_eq!(
        created.environment_statuses[0].environments[0].status,
        openjd_model::EnvironmentStatus::Entered
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

/// `check` accepts the submission, and job creation creates it; returns
/// each Step's effective values ([`rendered_values`]) and the warnings.
fn check_ok_and_apply_ok(jt: &JobTemplate, envs: &[EnvironmentTemplate]) -> Vec<Vec<String>> {
    check(jt, envs).unwrap_or_else(|e| panic!("check rejected the submission:\n{e}"));
    let values = job_params_with(jt, &[], envs);
    let created =
        create_job_with_environment_templates(jt, &attach(envs), &values, &template_ctx(jt))
            .unwrap_or_else(|e| panic!("job creation rejected the submission:\n{e}"));
    created.job.steps.iter().map(rendered_values).collect()
}

#[test]
fn a_relative_literal_path_is_accepted_by_check_and_job_creation() {
    // §4.5 item 3 (spec af53020): a PATH value is used as given, so `check`
    // and job creation agree that a Step's relative literal is no error,
    // and the value is stored unchanged.
    let env = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions: [{name: Dir, type: PATH, default: /d}]
  variables: {D: "{{Env.Param.Dir}}"}
"#;
    let jt = job_ok(&steps_naming("E", true, "{Dir: rel/dir}"));
    assert_eq!(
        check_ok_and_apply_ok(&jt, &[env_ok(env)]),
        [["EnvironmentTemplate[0]/E: Dir=path:rel/dir"]]
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
           parameter without a default of each Environment its Sessions enter (every attached Environment with activation ALWAYS, each one with \
           activation BY_DEFAULT that the Step does not decline, and each one \
           with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1); missing: 'Need' of Environment 'E' \
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
    // any attachment, and the submission gets no warning it did not get
    // before the extension (RFC Backward compatibility).
    let jt = job_ok(&shadowing(""));
    assert_eq!(check(&jt, &envs).unwrap(), Vec::<String>::new());
    assert_eq!(apply(&jt, &envs, &[]).unwrap(), Vec::<String>::new());
}

#[test]
fn an_attachments_uri_default_is_used_as_given() {
    // A URI is one more form of PATH value (§4.5 item 3), with no caller
    // policy, whatever the Job Template lists.
    let env = "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
               environment:\n  name: Data\n  envParameterDefinitions: [{name: Where, type: PATH, \
               default: \"s3://bucket/key\"}]\n  variables: {W: \"{{Env.Param.Where}}\"}\n";
    let jt = job_ok(
        "specificationVersion: jobtemplate-2023-09\nname: J\n\
         steps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n",
    );
    assert_eq!(
        check_ok_and_apply_ok(&jt, &[env_ok(env)]),
        [["EnvironmentTemplate[0]/Data: Where=path:s3://bucket/key"]]
    );
}

#[test]
fn an_attachments_relative_literal_default_is_used_as_given() {
    // Valid at template validation, and at job creation since spec af53020:
    // the Step that takes it gets it unchanged.
    let env = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions: [{name: Dir, type: PATH, default: rel}]
  variables: {D: "{{Env.Param.Dir}}"}
"#;
    let jt = job_ok(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         steps:\n- name: S\n  dependencies: [{environment: E}]\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    assert_eq!(
        check_ok_and_apply_ok(&jt, &[env_ok(env)]),
        [["EnvironmentTemplate[0]/E: Dir=path:rel"]]
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
           can reach it (Template Schemas §3.2.1 item 5), and its activation is BY_DEFAULT, so this Step's Sessions enter it.",
        ],
    );
}

#[test]
fn labels_name_the_attachments() {
    let jt = job_ok(&steps_naming("Conda", false, "{Bogus: y}"));
    let envs = [env_ok(CONDA)];
    let attached = [AttachedEnvironmentTemplate::new(&envs[0]).with_label("conda.yaml")];
    let err = check_environment_templates(&jt, &attached, &template_ctx(&jt)).unwrap_err();
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
     enableIf, so it supplies nothing; Template Schemas §3.2 item 3.2)";

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

/// `REQUIRED_ATTACHED` with `activation: ALWAYS`, which no Step can leave
/// out, so a disabled entry for it is an error and it is still checked as
/// entered, with the disabled note.
fn required_attached_always() -> String {
    REQUIRED_ATTACHED.replace("  name: Conda\n", "  name: Conda\n  activation: ALWAYS\n")
}

/// The §3.2 item 3.2 error for Step `step`'s disabled entry `dependencies[j]`
/// naming the attached `ALWAYS` Environment `label`.
fn always_entry_disabled(step_index: usize, step: &str, label: &str) -> String {
    format!(
        "JobTemplate -> steps[{step_index}] -> dependencies[0] -> enableIf:\n\tis false, so Step \
         '{step}' would decline Environment {label}, whose activation is ALWAYS: no Step can \
         leave out an ALWAYS Environment (Template Schemas §3.2 item 3.2)."
    )
}

fn attached_missing(note: &str) -> String {
    format!(
        "JobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every \
         parameter without a default of each Environment its Sessions enter (every attached Environment with activation ALWAYS, each one with \
         activation BY_DEFAULT that the Step does not decline, and each one \
         with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1); missing: 'Packages' of Environment 'Conda' \
         (EnvironmentTemplate[0]){note}."
    )
}

// A disabled entry for a `BY_DEFAULT` attachment declines it (§3.2 item
// 3.2), so these use an `ALWAYS` one: the entry is an error, and the
// attachment is still checked as entered, with the note. Before §3.2 item 3.2
// gave a disabled entry a meaning, they used the `BY_DEFAULT`
// `REQUIRED_ATTACHED`; `a_disabled_entry_declines_a_by_default_attachment`
// covers that case now.

#[test]
fn a_literally_disabled_entry_is_named_in_the_missing_parameters_message() {
    // Job creation stops the Step after §7.4.1 step 4, so only check, which
    // reports every violation it is certain of, gives the note.
    let disabled = always_entry_disabled(0, "S", "'Conda' (EnvironmentTemplate[0])");
    check_err_and_apply_err(
        &entry_with_enable_if("false"),
        &[&required_attached_always()],
        &[&disabled, &attached_missing(DISABLED_NOTE)],
        &[&disabled],
    );
}

#[test]
fn an_entry_disabled_by_an_expression_is_named_at_job_creation() {
    let jt = job_ok(&entry_with_enable_if(r#""Param.Mode == 'on'""#));
    let envs = [env_ok(&required_attached_always())];
    // `check` cannot tell whether the entry is enabled.
    assert_eq!(check(&jt, &envs).unwrap(), Vec::<String>::new());
    apply(&jt, &envs, &[]).expect("enabled, the entry supplies Packages");
    let err = apply(&jt, &envs, &[("Mode", "off")]).unwrap_err();
    assert_errors(
        &err.to_string(),
        "Submission",
        &[&always_entry_disabled(
            0,
            "S",
            "'Conda' (EnvironmentTemplate[0])",
        )],
    );
}

#[test]
fn a_constant_expression_that_disables_an_always_entry_is_named_at_job_creation() {
    // A constant expression is still an expression, unknown before the Job
    // is created (§7.5 rule 5), so `check` accepts the entry and job
    // creation reports the §4 item 4 error (`specs/cli/check.md`). That
    // error, of §7.4.1 step 4, stops the Step, so its constraint 7 is not
    // checked.
    let jt = job_ok(&entry_with_enable_if(r#""false""#));
    let envs = [env_ok(&required_attached_always())];
    assert_eq!(check(&jt, &envs).unwrap(), Vec::<String>::new());
    let err = apply(&jt, &envs, &[]).unwrap_err();
    assert_errors(
        &err.to_string(),
        "Submission",
        &[&always_entry_disabled(
            0,
            "S",
            "'Conda' (EnvironmentTemplate[0])",
        )],
    );
}

#[test]
fn a_disabled_entry_declines_a_by_default_attachment() {
    // §3.2 item 3.2, §3.2.1: the disabled entry's name resolves to the
    // `BY_DEFAULT` attachment, which the Step then declines, so constraint
    // 7 does not apply to it, at check and at job creation.
    let jt = job_ok(&entry_with_enable_if("false"));
    let envs = [env_ok(REQUIRED_ATTACHED)];
    assert_eq!(check(&jt, &envs).unwrap(), Vec::<String>::new());
    apply(&jt, &envs, &[]).expect("declined, Conda requires nothing");
    let jt = job_ok(&entry_with_enable_if(r#""Param.Mode == 'on'""#));
    assert_eq!(check(&jt, &envs).unwrap(), Vec::<String>::new());
    apply(&jt, &envs, &[("Mode", "off")]).expect("declined, Conda requires nothing");
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
        // An anyOf or allOf written as one expression (ENV_PARAMS, §3.3.2)
        // is never literal, so it is left out like the others.
        (
            "",
            "    - {name: attr.custom.e, anyOf: \"{{ [Env.Param.N > 0] }}\"}\n",
        ),
        (
            "",
            "    - {name: attr.custom.f, allOf: \"{{ [Env.Param.N > 0] }}\"}\n",
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
    // The Step's Sessions enter an ALWAYS attachment (the test name
    // predates the rename) whatever the entry's enableIf decides, so its
    // literal requirements conflict either way. A BY_DEFAULT one is
    // declined where the entry is disabled (§3.2 item 3.2), so whether the
    // Step enters it is known only at job creation; this test asserted the
    // conflict at check for a BY_DEFAULT attachment before that rule.
    let jt = gpu_job("    amounts: [{name: amount.worker.gpu, max: 1}]\n")
        .replace(
            "name: J\n",
            "name: J\nparameterDefinitions: [{name: Mode, type: STRING, default: \"on\"}]\n",
        )
        .replace(
            "  hostRequirements:\n",
            "  dependencies:\n  - environment: Gpu\n    enableIf: \"Param.Mode == 'on'\"\n  \
             hostRequirements:\n",
        );
    let always = gpu_attached("2").replace("  name: Gpu\n", "  name: Gpu\n  activation: ALWAYS\n");
    check_err_same_as_apply(&jt, &[&always], &[GPU_CONFLICT]);
    let by_default = gpu_attached("2");
    let jt = job_ok(&jt);
    let envs = [env_ok(&by_default)];
    assert_eq!(check(&jt, &envs).unwrap(), Vec::<String>::new());
    let err = apply(&jt, &envs, &[]).unwrap_err();
    assert_errors(&err.to_string(), "Submission", &[GPU_CONFLICT]);
    // Declined, the attachment adds no requirements (§3.3.3).
    apply(&jt, &envs, &[("Mode", "off")]).expect("declined, Gpu adds no requirements");
}

#[test]
fn an_ambiguous_attachment_without_parameters_still_combines_at_check() {
    // check needs no values for an attachment without parameters, so it
    // combines its requirements despite the ambiguous entry. Job creation
    // stops the Step after the ambiguous entry (§7.4.1 step 3), so it
    // reports the ambiguity alone.
    let jt = gpu_job("    amounts: [{name: amount.worker.gpu, max: 1}]\n").replace(
        "  hostRequirements:\n",
        "  dependencies: [{environment: Gpu}]\n  hostRequirements:\n",
    );
    let att = gpu_attached("2").replace(
        "  envParameterDefinitions: [{name: N, type: INT, default: 2}]\n",
        "",
    );
    let ambiguous = "JobTemplate -> steps[0] -> dependencies[0] -> environment:\n\tnames \
                     Environment 'Gpu', which 2 attached Environment Templates define \
                     (EnvironmentTemplate[0], EnvironmentTemplate[1]); an entry must match \
                     exactly one (Template Schemas §3.2.1 item 3).";
    check_err_and_apply_err(
        &jt,
        &[&att, &att],
        // Both attachments are entered; the first sets the greatest min.
        &[ambiguous, GPU_CONFLICT],
        &[ambiguous],
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
    // An `ALWAYS` attachment, for which the disabled entry is an error and
    // which is still checked as entered (see
    // `a_literally_disabled_entry_is_named_in_the_missing_parameters_message`).
    let src = entry_with_enable_if("false").replace(
        "steps:\n- name: S\n",
        "steps:\n- name: First\n  dependencies: [{environment: Conda, parameters: {Packages: y}}]\n  \
         script: {actions: {onRun: {command: x}}}\n- name: S\n",
    );
    let disabled = always_entry_disabled(1, "S", "'Conda' (EnvironmentTemplate[0])");
    check_err_and_apply_err(
        &src,
        &[&required_attached_always()],
        &[
            &disabled,
            &attached_missing(DISABLED_NOTE).replace("steps[0]", "steps[1]"),
        ],
        &[&disabled],
    );
    // The expression form, decided at job creation, names the second Step,
    // which job creation then stops after §7.4.1 step 4.
    let src = entry_with_enable_if(r#""Param.Mode == 'on'""#).replace(
        "steps:\n- name: S\n",
        "steps:\n- name: First\n  dependencies: [{environment: Conda, parameters: {Packages: y}}]\n  \
         script: {actions: {onRun: {command: x}}}\n- name: S\n",
    );
    let jt = job_ok(&src);
    let envs = [env_ok(&required_attached_always())];
    let err = apply(&jt, &envs, &[("Mode", "off")]).unwrap_err();
    assert_errors(
        &err.to_string(),
        "Submission",
        &[&always_entry_disabled(
            1,
            "S",
            "'Conda' (EnvironmentTemplate[0])",
        )],
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
        create_job_with_environment_templates(&jt, &attach(&envs), &values, &template_ctx(&jt))
            .unwrap();
    // For a Step with no Gpus value the GPU amount's min resolves to None,
    // so the amount is discarded (Template Schemas §3.3).
    let gpu_min = |step: &str| {
        let s = created.job.steps.iter().find(|s| s.name == step).unwrap();
        let hr = s.host_requirements.as_ref().unwrap();
        let attrs = hr.attributes.as_ref().unwrap();
        assert_eq!(attrs[0].name, "attr.worker.os.family", "{step}");
        assert_eq!(attrs[0].any_of, Some(vec!["linux".to_string()]), "{step}");
        let amounts = hr.amounts.as_ref()?;
        assert_eq!(amounts[0].name, "amount.worker.gpu", "{step}");
        amounts[0].min
    };
    assert_eq!(gpu_min("DecodeFrames"), None);
    assert_eq!(gpu_min("SolveSfM"), Some(1.0));
    let decode = created
        .job
        .steps
        .iter()
        .position(|s| s.name == "DecodeFrames")
        .unwrap();
    assert_eq!(
        created.discarded_host_requirements[decode]
            .iter()
            .map(|d| (d.kind, d.name.as_str()))
            .collect::<Vec<_>>(),
        [(
            openjd_model::HostRequirementKind::Amount,
            "amount.worker.gpu"
        )]
    );
}

/// §4.5 item 3 (spec af53020), for an attached Environment Template:
/// `check_environment_templates` and job creation accept a literal
/// `default` a Step takes, a literal `default` no Step takes, and a Step's
/// literal entry value, of any form, relative ones included, as a PATH
/// value and as LIST[PATH] elements, and job creation stores each
/// unchanged.
#[test]
fn an_attachments_paths_of_any_form_pass_check_and_job_creation() {
    let env = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions:
  - {name: Taken, type: PATH, default: 'C:tools'}
  - {name: Unused, type: PATH, default: 'rel\share'}
  - {name: Supplied, type: PATH}
  - {name: List, type: "LIST[PATH]", default: ['C:\x', 'y', '\\srv\s', '\z']}
  - {name: SuppliedList, type: "LIST[PATH]"}
  variables: {D: "{{Env.Param.Taken}}"}
"#;
    for (unused, supplied) in [("/opt/u", r"C:\x"), ("u/v", r"\x"), ("D:u", "./x")] {
        let jt = job_ok(&steps_naming(
            "E",
            true,
            &format!(
                "{{Unused: '{unused}', Supplied: '{supplied}', SuppliedList: ['{supplied}', '{unused}']}}"
            ),
        ));
        let json = |s: &str| serde_json::to_string(s).unwrap();
        assert_eq!(
            check_ok_and_apply_ok(&jt, &[env_ok(env)]),
            [[format!(
                r#"EnvironmentTemplate[0]/E: List=list[path]:["C:\\x","y","\\\\srv\\s","\\z"], Supplied=path:{supplied}, SuppliedList=list[path]:[{},{}], Taken=path:C:tools, Unused=path:{unused}"#,
                json(supplied),
                json(unused)
            )]]
        );
    }
}

/// §4.5 item 3, for a Job Environment when Environment Templates are
/// attached: `create_job_with_environment_templates` keeps its literal
/// `default`s of any form, and both it and `check_environment_templates`
/// accept a Step's relative literal value for it.
/// (`check_environment_templates` does not examine Job Environment
/// `default`s; `specs/cli/check.md`.)
#[test]
fn a_job_environments_literal_paths_of_any_form_pass_with_an_attachment() {
    let jt = job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: J
  envParameterDefinitions:
  - {name: Taken, type: PATH, default: 'tools\bin'}
  - {name: Unused, type: PATH, default: '/opt/u'}
  variables: {V: v}
steps:
- name: S
  dependencies: [{environment: J, parameters: {Unused: 'rel'}}]
  script: {actions: {onRun: {command: x}}}
"#,
    );
    let envs = [env_ok(
        "specificationVersion: environment-2023-09\nenvironment:\n  name: A\n  variables: {V: v}\n",
    )];
    assert_eq!(
        check_ok_and_apply_ok(&jt, &envs),
        [[r"JobTemplate/J: Taken=path:tools\bin, Unused=path:rel"]]
    );
}

/// §4.5 item 3 (spec af53020) at every attached site: an empty value and a
/// URI, as a literal `default`, a format-string `default`, a Step's
/// literal entry value and a Step's format-string entry value, pass
/// `check_environment_templates` and job creation, and are stored
/// unchanged.
#[test]
fn an_attachments_empty_and_uri_values_are_used_as_given_at_every_site() {
    let env = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions:
  - {name: LitEmpty, type: PATH, default: ''}
  - {name: LitUri, type: PATH, default: 's3://b/lit'}
  - {name: FsEmpty, type: PATH, default: "{{ '' }}"}
  - {name: FsUri, type: PATH, default: "{{ 's3://b/fs' }}"}
  - {name: EntryEmpty, type: PATH}
  - {name: EntryUri, type: PATH}
  - {name: EntryFs, type: "LIST[PATH]"}
  variables: {D: "{{Env.Param.LitEmpty}}"}
"#;
    let jt = job_ok(&steps_naming(
        "E",
        true,
        r#"{EntryEmpty: '', EntryUri: 's3://b/entry', EntryFs: "{{ ['', 's3://b/l'] }}"}"#,
    ));
    assert_eq!(
        check_ok_and_apply_ok(&jt, &[env_ok(env)]),
        [[
            r#"EnvironmentTemplate[0]/E: EntryEmpty=path:, EntryFs=list[path]:["","s3://b/l"], EntryUri=path:s3://b/entry, FsEmpty=path:, FsUri=path:s3://b/fs, LitEmpty=path:, LitUri=path:s3://b/lit"#
        ]]
    );
}

/// Rules 1 and 2 at the attached sites. A single expression's string or
/// `list[string]` result is the text as written (a non-POSIX conversion
/// would turn `/` into `\`); literal LIST[PATH] elements of any form are
/// used as given; and the path operations inside a multi-segment value
/// have POSIX semantics. Each holds for an attached `default` and for a
/// Step's entry value, which the submission stage resolves from the
/// frozen Step scope.
#[test]
fn an_attachments_values_are_the_text_as_written_with_posix_path_operations() {
    let env = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions:
  - {name: DefP, type: PATH, default: "{{ '/d/p' }}"}
  - {name: DefL, type: "LIST[PATH]", default: "{{ ['/d/l', 'C:/x'] }}"}
  - {name: DefLit, type: "LIST[PATH]", default: ['', 's3://b/d', 'rel/d']}
  - {name: DefMulti, type: PATH, default: "{{ path('C:/d') / 'e' }}/f"}
  - {name: EntP, type: PATH}
  - {name: EntL, type: "LIST[PATH]"}
  - {name: EntLit, type: "LIST[PATH]"}
  - {name: EntMulti, type: PATH}
  variables: {D: "{{Env.Param.DefP}}"}
"#;
    for dir in ["/r/dir", r"C:\x", "C:/y", r"\\server\share"] {
        let jt = job_ok(&format!(
            r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{{name: Dir, type: STRING, default: '{dir}'}}]
steps:
- name: S
  dependencies:
  - environment: E
    parameters:
      EntP: "{{{{ Param.Dir }}}}"
      EntL: "{{{{ [Param.Dir, 'rel/l'] }}}}"
      EntLit: ['', 's3://b/e', 'rel/e']
      EntMulti: "{{{{ path('C:/a') / 'b' }}}}/x"
  script: {{actions: {{onRun: {{command: x}}}}}}
"#
        ));
        let json = serde_json::to_string(dir).unwrap();
        assert_eq!(
            check_ok_and_apply_ok(&jt, &[env_ok(env)]),
            [[format!(
                r#"EnvironmentTemplate[0]/E: DefL=list[path]:["/d/l","C:/x"], DefLit=list[path]:["","s3://b/d","rel/d"], DefMulti=path:C:/d/e/f, DefP=path:/d/p, EntL=list[path]:[{json},"rel/l"], EntLit=list[path]:["","s3://b/e","rel/e"], EntMulti=path:C:/a/b/x, EntP=path:{dir}"#
            )]],
            "{dir}"
        );
    }
}

/// §4.5 item 3 for the format-string forms at an attached Environment's
/// sites: an empty, a URI and a relative result of a multi-segment or
/// single-expression `default` or entry value are used as given.
#[test]
fn an_attachments_format_string_values_of_any_form_are_used_as_given() {
    let env = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions:
  - {name: ME, type: PATH, default: "{{ '' }}{{ '' }}"}
  - {name: MU, type: PATH, default: "s3://b/{{ 'k' }}"}
  - {name: MR, type: PATH, default: "rel/{{ 'k' }}"}
  - {name: OL, type: "LIST[PATH]", default: "{{ ['', 's3://b/x', 'rel/y'] }}"}
  - {name: EOE, type: PATH}
  - {name: EOU, type: PATH}
  - {name: EME, type: PATH}
  - {name: EMU, type: PATH}
  variables: {D: "{{Env.Param.ME}}"}
"#;
    let jt = job_ok(&steps_naming(
        "E",
        true,
        r#"{EOE: "{{ '' }}", EOU: "{{ 's3://b/e' }}", EME: "{{ '' }}{{ '' }}", EMU: "s3://b/{{ 'm' }}"}"#,
    ));
    assert_eq!(
        check_ok_and_apply_ok(&jt, &[env_ok(env)]),
        [[
            r#"EnvironmentTemplate[0]/E: EME=path:, EMU=path:s3://b/m, EOE=path:, EOU=path:s3://b/e, ME=path:, MR=path:rel/k, MU=path:s3://b/k, OL=list[path]:["","s3://b/x","rel/y"]"#
        ]]
    );
}

/// `check_environment_templates` decides an attached Environment's
/// `enableIf` from the literal values a Step supplies, of any form
/// (§4.5 item 3): with each value read as given the `enableIf` is `false`,
/// so the Environment is disabled and its unsupplied `Q` is no error,
/// as at job creation.
#[test]
fn check_reads_literal_path_values_of_any_form_in_an_enable_if() {
    for value in ["''", "'s3://b/k'", "'rel/x'", "'C:/x'"] {
        let env = format!(
            r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions:
  - {{name: P, type: PATH}}
  - {{name: L, type: "LIST[PATH]"}}
  - {{name: Q, type: STRING}}
  enableIf: "Env.RawParam.P != {value} or Env.RawParam.L != [{value}]"
  variables: {{D: "{{{{Env.Param.Q}}}}"}}
"#
        );
        let jt = job_ok(&steps_naming(
            "E",
            true,
            &format!("{{P: {value}, L: [{value}]}}"),
        ));
        assert_eq!(
            check_ok_and_apply_ok(&jt, &[env_ok(&env)]),
            [Vec::<String>::new()],
            "{value}"
        );
    }
}

/// The converse: when the literal value as given leaves the `enableIf`
/// to a value that stays unknown, `check_environment_templates` reports
/// the missing parameter, as job creation does.
#[test]
fn check_reports_a_missing_parameter_an_enable_if_on_a_literal_path_leaves_unresolved() {
    let env = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions:
  - {name: P, type: PATH}
  - {name: Q, type: STRING}
  enableIf: "Env.RawParam.P != 'C:/x' or Env.Param.Q == 'z'"
  variables: {D: "{{Env.Param.Q}}"}
"#;
    let jt = job_ok(&steps_naming("E", true, "{P: 'C:/x'}"));
    let envs = [env_ok(env)];
    let checked = check(&jt, &envs).expect_err("expected check to reject the submission");
    let applied = apply(&jt, &envs, &[]).expect_err("expected job creation to reject it");
    for err in [checked.to_string(), applied.to_string()] {
        assert!(err.contains("missing: 'Q' of Environment 'E'"), "{err}");
    }
}
