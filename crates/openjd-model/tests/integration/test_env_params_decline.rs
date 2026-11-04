// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `ENV_PARAMS`: what a disabled `environment:` entry means (Template
//! Schemas §3.2 item 3.2). It supplies no values, and its effect depends on
//! the `activation` of the Environment it names:
//!
//! - `ON_REQUEST`: the Step does not request it;
//! - `BY_DEFAULT`: the Step declines it, so its Sessions do not enter it,
//!   its own `enableIf` is not evaluated for the Step, and constraint 7,
//!   the value checks, and its `hostRequirements` do not apply to the Step;
//! - `ALWAYS`: an error naming the Step and the Environment, at template
//!   validation for a literal `false` on an entry naming a Job
//!   Environment, otherwise when the Job is created.
//!
//! A disabled entry's name is resolved, against `jobEnvironments` and then
//! the attached Environments (§3.2.1), to learn the `activation`; one that
//! matches no attachment has no effect, whatever `required` says.

use openjd_model::job::{Document, Job};
use openjd_model::template::EnvironmentTemplate;
use openjd_model::{
    check_environment_templates, create_job_with_environment_templates,
    AttachedEnvironmentTemplate, CreatedJob, ModelError,
};

use crate::env_params_common::{
    assert_errors, env_ok, job_err, job_ok, job_params_with, posix_ctx, rendered_values,
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
    create_job_with_environment_templates(&jt, &attached, &values, &posix_ctx(&jt))
}

fn create_ok(jt_src: &str, env_srcs: &[&str], params: &[(&str, &str)]) -> CreatedJob {
    create(jt_src, env_srcs, params).unwrap_or_else(|e| panic!("job creation failed:\n{e}"))
}

fn create_err(
    jt_src: &str,
    env_srcs: &[&str],
    params: &[(&str, &str)],
    model: &str,
    expected: &[&str],
) {
    let err = create(jt_src, env_srcs, params).expect_err("expected job creation to fail");
    assert_errors(&err.to_string(), model, expected);
}

/// `check_environment_templates` for `jt_src` with `env_srcs` attached.
fn check(jt_src: &str, env_srcs: &[&str]) -> Result<Vec<String>, ModelError> {
    let jt = job_ok(jt_src);
    let envs: Vec<EnvironmentTemplate> = env_srcs.iter().map(|s| env_ok(s)).collect();
    let attached: Vec<AttachedEnvironmentTemplate<'_>> =
        envs.iter().map(AttachedEnvironmentTemplate::new).collect();
    check_environment_templates(&jt, &attached, &posix_ctx(&jt))
        .map(|w| w.iter().map(ToString::to_string).collect())
}

/// Whether Step `step`'s Sessions enter the Environment `name` of
/// `document`.
fn enters(created: &CreatedJob, step: usize, document: &Document, name: &str) -> bool {
    created.job.steps[step].enters_environment(document, name)
}

/// The capability names of Step `step`'s combined host requirements.
fn requirement_names(job: &Job, step: usize) -> Vec<String> {
    let Some(hr) = &job.steps[step].host_requirements else {
        return Vec::new();
    };
    hr.amounts
        .iter()
        .flatten()
        .map(|a| a.name.clone())
        .chain(hr.attributes.iter().flatten().map(|a| a.name.clone()))
        .collect()
}

/// The §3.2 item 3.2 error for Step `step` (at index `i`), whose entry
/// `dependencies[j]` is disabled and names the `ALWAYS` Environment
/// `label`; `prefix` is the document path (`""` or `"JobTemplate -> "`).
fn always_error(prefix: &str, i: usize, j: usize, step: &str, label: &str) -> String {
    format!(
        "{prefix}steps[{i}] -> dependencies[{j}] -> enableIf:\n\tis false, so Step '{step}' would \
         decline Environment {label}, whose activation is ALWAYS: no Step can leave out an \
         ALWAYS Environment (Template Schemas §3.2 item 3.2)."
    )
}

/// A Job Template with one Job Environment `Lic` of `activation`
/// `ACTIVATION` declaring `Product` (STRING, `allowedValues: [maya, nuke]`,
/// no `default`), and Steps A (whose entry for Lic has `enableIf`
/// `ENABLE`) and B (which supplies `maya`).
const LIC: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Mode, type: STRING, default: "off"}]
jobEnvironments:
- name: Lic
  activation: ACTIVATION
  envParameterDefinitions:
  - {name: Product, type: STRING, allowedValues: [maya, nuke]}
  variables: {P: "{{Env.Param.Product}}"}
steps:
- name: A
  dependencies:
  - environment: Lic
    enableIf: ENABLE
    parameters: {Product: VALUE}
  script: {actions: {onRun: {command: x}}}
- name: B
  dependencies: [{environment: Lic, parameters: {Product: maya}}]
  script: {actions: {onRun: {command: x}}}
"#;

fn lic(activation: &str, enable_if: &str, value: &str) -> String {
    LIC.replace("ACTIVATION", activation)
        .replace("ENABLE", enable_if)
        .replace("VALUE", value)
}

const MODE_ON: &str = r#""Param.Mode == 'on'""#;

// ── ALWAYS: an error naming the Step and the Environment ────────────

#[test]
fn a_literal_false_entry_for_an_always_job_environment_is_rejected_at_validation() {
    // The entry supplies nothing, so Lic is still checked as entered, with
    // every default: its parameter without a default is missing, and the
    // message says why.
    job_err(
        &lic("ALWAYS", "false", "maya"),
        &[
            &always_error("", 0, 0, "A", "'Lic'"),
            "steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a \
             default of each Environment its Sessions enter (every jobEnvironments entry with activation ALWAYS, each one with \
             activation BY_DEFAULT that the Step does not decline, and each one \
             with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7); missing: 'Product' of \
             Environment 'Lic' (the Step's 'environment:' dependency for it is disabled by its \
             enableIf, so it supplies nothing; Template Schemas §3.2 item 3.2).",
        ],
    );
    // With a default for Product, the entry is the only error.
    job_err(
        &lic("ALWAYS", "false", "maya").replace(
            "allowedValues: [maya, nuke]}",
            "allowedValues: [maya, nuke], default: nuke}",
        ),
        &[&always_error("", 0, 0, "A", "'Lic'")],
    );
}

#[test]
fn an_expression_entry_for_an_always_job_environment_is_rejected_at_job_creation() {
    // Not known to be disabled at template validation (§7.5 rule 5). The
    // error, of §7.4.1 step 4, stops the Step, so its constraint 7 is not
    // checked.
    let src = lic("ALWAYS", MODE_ON, "maya");
    job_ok(&src);
    create_ok(&src, &[], &[("Mode", "on")]);
    create_err(
        &src,
        &[],
        &[("Mode", "off")],
        "JobTemplate",
        &[&always_error("", 0, 0, "A", "'Lic'")],
    );
}

/// An attached Environment Template `Conda` with `activation` `activation`
/// and a parameter `Packages` with a `default`.
fn conda(activation: &str) -> String {
    format!(
        r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: Conda
  activation: {activation}
  envParameterDefinitions: [{{name: Packages, type: STRING, default: base}}]
  variables: {{P: "{{{{Env.Param.Packages}}}}"}}
"#
    )
}

/// A Job Template whose Step A's entry for `Conda`, which it does not
/// define, has `enableIf` `enable_if`.
fn external_entry(enable_if: &str) -> String {
    format!(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{{name: Mode, type: STRING, default: "off"}}]
steps:
- name: A
  dependencies:
  - environment: Conda
    enableIf: {enable_if}
    parameters: {{Packages: colmap}}
  script: {{actions: {{onRun: {{command: x}}}}}}
"#
    )
}

#[test]
fn a_disabled_entry_for_an_attached_always_environment_is_rejected_at_job_creation() {
    let always = conda("ALWAYS");
    let expected = always_error(
        "JobTemplate -> ",
        0,
        0,
        "A",
        "'Conda' (EnvironmentTemplate[0])",
    );
    for enable_if in ["false", MODE_ON] {
        let src = external_entry(enable_if);
        // Template validation cannot tell what the name resolves to.
        job_ok(&src);
        create_err(&src, &[&always], &[], "Submission", &[&expected]);
    }
    // The literal one is known once the attachments are, so `check`, which
    // `openjd check --environment` runs, rejects it too.
    let err = check(&external_entry("false"), &[&always]).unwrap_err();
    assert_errors(&err.to_string(), "Submission", &[&expected]);
    assert_eq!(
        check(&external_entry(MODE_ON), &[&always]).unwrap(),
        Vec::<String>::new()
    );
    // Enabled, the entry supplies its value.
    let created = create_ok(&external_entry(MODE_ON), &[&always], &[("Mode", "on")]);
    assert_eq!(
        rendered_values(&created.job.steps[0]),
        ["EnvironmentTemplate[0]/Conda: Packages=string:colmap"]
    );
}

// ── BY_DEFAULT: the Step declines it ────────────────────────────────

#[test]
fn a_literal_false_entry_declines_a_by_default_job_environment() {
    // Constraint 7 does not apply to A (JT-103), and A's Sessions do not
    // enter Lic; B's do, with its value.
    let src = lic("BY_DEFAULT", "false", "maya");
    job_ok(&src);
    let created = create_ok(&src, &[], &[]);
    assert!(!enters(&created, 0, &Document::JobTemplate, "Lic"));
    assert_eq!(created.job.steps[0].environment_parameters, None);
    assert!(enters(&created, 1, &Document::JobTemplate, "Lic"));
    assert_eq!(
        rendered_values(&created.job.steps[1]),
        ["JobTemplate/Lic: Product=string:maya"]
    );
    // Declined is not "disabled by the Environment's own enableIf".
    assert_eq!(created.disabled_environments[0].environments, []);
}

#[test]
fn a_declined_environments_parameters_are_not_listed_as_missing() {
    // A declines Lic and enters Other, whose parameter it does not supply.
    // The constraint 7 error names only Other's parameter.
    let src = lic("BY_DEFAULT", "false", "maya").replace(
        "steps:\n",
        "- name: Other\n  envParameterDefinitions: [{name: Seat, type: STRING}]\n  \
         variables: {S: \"{{Env.Param.Seat}}\"}\nsteps:\n",
    );
    let src = src.replace(
        "  dependencies: [{environment: Lic, parameters: {Product: maya}}]",
        "  dependencies:\n  - {environment: Lic, parameters: {Product: maya}}\n  \
         - {environment: Other, parameters: {Seat: s}}",
    );
    job_err(
        &src,
        &[
            "steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a \
           default of each Environment its Sessions enter (every jobEnvironments entry with \
           activation ALWAYS, each one with activation BY_DEFAULT that the Step does not \
           decline, and each one with activation ON_REQUEST that the Step requests; Template \
           Schemas §3.2 constraint 7); missing: 'Seat' of Environment 'Other'.",
        ],
    );
}

#[test]
fn an_expression_entry_that_resolves_false_declines_a_by_default_job_environment() {
    // Whether A enters Lic is undetermined at template validation, so
    // constraint 7 is not reported then (JB-35).
    let src = lic("BY_DEFAULT", MODE_ON, "maya");
    job_ok(&src);
    let created = create_ok(&src, &[], &[]);
    assert!(!enters(&created, 0, &Document::JobTemplate, "Lic"));
    assert_eq!(created.job.steps[0].environment_parameters, None);
    let created = create_ok(&src, &[], &[("Mode", "on")]);
    assert!(enters(&created, 0, &Document::JobTemplate, "Lic"));
    assert_eq!(
        rendered_values(&created.job.steps[0]),
        ["JobTemplate/Lic: Product=string:maya"]
    );
}

#[test]
fn a_declined_environments_value_checks_do_not_apply() {
    // A's value breaks `allowedValues`; declined, it is not checked, at
    // template validation or at job creation.
    let src = lic("BY_DEFAULT", "false", "houdini");
    job_ok(&src);
    create_ok(&src, &[], &[]);
    // The same for a format-string value, which is resolved only when the
    // Job is created, under an expression entry that resolves `false`.
    let src = lic("BY_DEFAULT", MODE_ON, "\"{{ 'hou' + 'dini' }}\"");
    job_ok(&src);
    create_ok(&src, &[], &[]);
    // Enabled, the value is checked.
    create_err(
        &src,
        &[],
        &[("Mode", "on")],
        "JobTemplate",
        &[
            "steps[0] -> dependencies[0] -> parameters -> Product:\n\tParameter 'Product': value \
           'houdini' is not in allowed values",
        ],
    );
    // A format-string value of the wrong type is not typed in pass 8 for a
    // literal-false entry either; for an enabled one it is.
    let typed = |enable_if: &str| {
        lic("BY_DEFAULT", enable_if, "\"{{ 'x' }}\"")
            .replace(
                "{name: Product, type: STRING, allowedValues: [maya, nuke]}",
                "{name: Product, type: INT}",
            )
            .replace("{Product: maya}", "{Product: 1}")
    };
    job_ok(&typed("false"));
    create_ok(&typed("false"), &[], &[]);
    job_err(&typed("true"), &[TYPED_ERROR]);
}

#[test]
fn an_expression_entrys_parameters_are_checked_only_if_it_is_enabled() {
    // The entry may be disabled, and a disabled entry's parameters are not
    // checked (§3.2 item 3.2), so its undeclared key, its literal outside
    // `allowedValues`, and its mistyped format string are not errors at
    // template validation (§7.5 rule 5). Disabled at job creation, they
    // are still not; enabled, each is.
    let src = lic("BY_DEFAULT", MODE_ON, "houdini").replace(
        "parameters: {Product: houdini}",
        "parameters: {Product: houdini, Bogus: x}",
    );
    job_ok(&src);
    create_ok(&src, &[], &[]);
    create_err(
        &src,
        &[],
        &[("Mode", "on")],
        "JobTemplate",
        &[
            "steps[0] -> dependencies[0] -> parameters -> Bogus:\n\tEnvironment 'Lic' declares no \
             parameter 'Bogus'. Declared: Product.",
            "steps[0] -> dependencies[0] -> parameters -> Product:\n\tParameter 'Product': value \
             'houdini' is not in allowed values",
        ],
    );
    let typed = lic("BY_DEFAULT", MODE_ON, "\"{{ 'x' }}\"")
        .replace(
            "{name: Product, type: STRING, allowedValues: [maya, nuke]}",
            "{name: Product, type: INT}",
        )
        .replace("{Product: maya}", "{Product: 1}");
    job_ok(&typed);
    create_ok(&typed, &[], &[]);
    create_err(
        &typed,
        &[],
        &[("Mode", "on")],
        "JobTemplate",
        &[TYPED_AT_CREATION],
    );
}

/// Job creation's error for `"{{ 'x' }}"` supplied to an INT parameter.
const TYPED_AT_CREATION: &str = "steps[0] -> dependencies[0] -> parameters -> Product:\n\tcould \
                                 not be resolved as a value of type INT: Cannot convert 'x' to \
                                 int: invalid digit found in string";

/// Pass 8's error for `"{{ 'x' }}"` supplied to an INT parameter.
const TYPED_ERROR: &str = "steps[0] -> dependencies[0] -> parameters -> Product:\n\tFailed to \
                           parse interpolation expression at [0, 9]. Cannot convert 'x' to int: \
                           invalid digit found in string";

#[test]
fn a_declined_environments_enable_if_is_not_evaluated() {
    // Lic's own enableIf fails for a Step that takes the default Index 5;
    // A declines Lic, so it is not evaluated for A. B supplies Index 0.
    let src = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: Lic
  envParameterDefinitions: [{name: Index, type: INT, default: 5}]
  enableIf: "[true][Env.Param.Index]"
  variables: {V: x}
steps:
- name: A
  dependencies: [{environment: Lic, enableIf: false}]
  script: {actions: {onRun: {command: x}}}
- name: B
  dependencies: [{environment: Lic, parameters: {Index: 0}}]
  script: {actions: {onRun: {command: x}}}
"#;
    let created = create_ok(src, &[], &[]);
    assert!(!enters(&created, 0, &Document::JobTemplate, "Lic"));
    assert!(enters(&created, 1, &Document::JobTemplate, "Lic"));
    // A Step that does not decline it evaluates it, and it fails.
    create_err(
        &src.replace("  dependencies: [{environment: Lic, enableIf: false}]\n", ""),
        &[],
        &[],
        "JobTemplate",
        &["jobEnvironments[0] -> enableIf:\n\tcould not be evaluated: Index 5 out of bounds for list \
           of length 1. Resolved for Step 'A'."],
    );
}

#[test]
fn a_declined_environments_host_requirements_do_not_apply() {
    // JB-142: Gpu's anyOf conflicts with A's own; declined, Gpu adds none
    // of its requirements to A.
    let src = |enable_if: &str| {
        format!(
            r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: Gpu
  hostRequirements:
    attributes: [{{name: attr.custom.flavor, anyOf: [b]}}]
    amounts: [{{name: amount.custom.gpus, min: 2}}]
  variables: {{V: x}}
steps:
- name: A
  dependencies: [{{environment: Gpu, enableIf: {enable_if}}}]
  hostRequirements:
    attributes: [{{name: attr.custom.flavor, anyOf: [a]}}]
    amounts: [{{name: amount.custom.gpus, max: 1}}]
  script: {{actions: {{onRun: {{command: x}}}}}}
"#
        )
    };
    for enable_if in ["false", "\"'Nope' in Job.EnvNames\""] {
        let created = create_ok(&src(enable_if), &[], &[]);
        assert!(!enters(&created, 0, &Document::JobTemplate, "Gpu"));
        assert_eq!(
            requirement_names(&created.job, 0),
            ["amount.custom.gpus", "attr.custom.flavor"],
            "{enable_if}"
        );
    }
    // Enabled, the requirements combine and conflict.
    create_err(
        &src("true"),
        &[],
        &[],
        "JobTemplate",
        &[
            "steps[0]:\n\tStep 'A': no host can satisfy the combined host \
           requirements for amount 'amount.custom.gpus': jobEnvironments[0] -> \
           hostRequirements (Environment 'Gpu') requires min 2, and steps[0] -> \
           hostRequirements requires max 1. Template Schemas §3.3.3.",
        ],
    );
}

#[test]
fn a_disabled_entry_declines_an_attached_by_default_environment() {
    // JB-140: the disabled entry's name resolves to the attached Conda,
    // which A then declines.
    let by_default = conda("BY_DEFAULT");
    let att = Document::environment_template(0, None);
    for enable_if in ["false", MODE_ON] {
        let src = external_entry(enable_if);
        assert_eq!(check(&src, &[&by_default]).unwrap(), Vec::<String>::new());
        let created = create_ok(&src, &[&by_default], &[]);
        assert!(!enters(&created, 0, &att, "Conda"), "{enable_if}");
        assert_eq!(created.job.steps[0].environment_parameters, None);
    }
    // Enabled, A enters it with its value.
    let created = create_ok(&external_entry(MODE_ON), &[&by_default], &[("Mode", "on")]);
    assert!(enters(&created, 0, &att, "Conda"));
    // A Step with no entry enters it with its default.
    let created = create_ok(
        &external_entry("false").replace(
            "  dependencies:\n  - environment: Conda\n    enableIf: false\n    parameters: \
             {Packages: colmap}\n",
            "",
        ),
        &[&by_default],
        &[],
    );
    assert_eq!(
        rendered_values(&created.job.steps[0]),
        ["EnvironmentTemplate[0]/Conda: Packages=string:base"]
    );
}

#[test]
fn a_declined_attachments_host_requirements_do_not_apply_at_check_or_job_creation() {
    let gpu = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: Gpu
  hostRequirements:
    amounts: [{name: amount.custom.gpus, min: 2}]
  variables: {V: x}
"#;
    let src = |enable_if: &str| {
        format!(
            r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: A
  dependencies: [{{environment: Gpu, enableIf: {enable_if}}}]
  hostRequirements:
    amounts: [{{name: amount.custom.gpus, max: 1}}]
  script: {{actions: {{onRun: {{command: x}}}}}}
"#
        )
    };
    assert_eq!(check(&src("false"), &[gpu]).unwrap(), Vec::<String>::new());
    let created = create_ok(&src("false"), &[gpu], &[]);
    assert_eq!(requirement_names(&created.job, 0), ["amount.custom.gpus"]);
    // Enabled, both reject it.
    let conflict = "JobTemplate -> steps[0]:\n\tStep 'A': no host can satisfy the combined host \
                    requirements for amount 'amount.custom.gpus': EnvironmentTemplate[0] -> \
                    environment -> hostRequirements (Environment 'Gpu') requires min 2, and \
                    JobTemplate -> steps[0] -> hostRequirements requires max 1. Template Schemas \
                    §3.3.3.";
    let err = check(&src("true"), &[gpu]).unwrap_err();
    assert_errors(&err.to_string(), "Submission", &[conflict]);
    create_err(&src("true"), &[gpu], &[], "Submission", &[conflict]);
}

// ── ON_REQUEST: unchanged ───────────────────────────────────────────

#[test]
fn a_disabled_entry_requests_no_on_request_environment() {
    let src = lic("ON_REQUEST", "false", "houdini");
    job_ok(&src);
    let created = create_ok(&src, &[], &[]);
    assert!(!enters(&created, 0, &Document::JobTemplate, "Lic"));
    assert!(enters(&created, 1, &Document::JobTemplate, "Lic"));
    let on_request = conda("ON_REQUEST");
    let att = Document::environment_template(0, None);
    for enable_if in ["false", MODE_ON] {
        let src = external_entry(enable_if);
        assert_eq!(check(&src, &[&on_request]).unwrap(), Vec::<String>::new());
        let created = create_ok(&src, &[&on_request], &[]);
        assert!(!enters(&created, 0, &att, "Conda"), "{enable_if}");
    }
}

// ── Resolving a disabled entry's name (§3.2.1) ──────────────────────

#[test]
fn a_disabled_required_entry_naming_no_attachment_has_no_effect() {
    // `required: true` is the default; item 2 does not apply to a
    // disabled entry, with or without other attachments, at check or at
    // job creation, and nothing is warned.
    let other = conda("BY_DEFAULT").replace("name: Conda", "name: Other");
    for enable_if in ["false", MODE_ON] {
        let src = external_entry(enable_if).replace("environment: Conda", "environment: Missing");
        for envs in [&[][..], &[other.as_str()][..]] {
            assert_eq!(check(&src, envs).unwrap(), Vec::<String>::new());
            let created = create_ok(&src, envs, &[]);
            assert_eq!(created.warnings, []);
        }
    }
}

#[test]
fn a_disabled_entry_naming_an_ambiguous_attachment_is_an_error() {
    // §3.2.1 item 3 applies to a disabled entry: it must match exactly one
    // attachment for its activation to be known.
    let by_default = conda("BY_DEFAULT");
    let expected = "JobTemplate -> steps[0] -> dependencies[0] -> environment:\n\tnames \
                    Environment 'Conda', which 2 attached Environment Templates define \
                    (EnvironmentTemplate[0], EnvironmentTemplate[1]); an entry must match \
                    exactly one (Template Schemas §3.2.1 item 3).";
    let err = check(&external_entry("false"), &[&by_default, &by_default]).unwrap_err();
    assert_errors(&err.to_string(), "Submission", &[expected]);
    create_err(
        &external_entry("false"),
        &[&by_default, &by_default],
        &[],
        "Submission",
        &[expected],
    );
}

// ── WRAP_ACTIONS single-layer rule (§4, after item 10) ──────────────

fn wrap_env(name: &str, activation: &str) -> String {
    format!(
        "- name: {name}\n  activation: {activation}\n  script:\n    actions:\n      \
         onWrapEnvEnter: {{command: x}}\n      onWrapTaskRun: {{command: x}}\n      \
         onWrapEnvExit: {{command: x}}\n"
    )
}

fn wrap_job(envs: &[String], steps: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR, WRAP_ACTIONS]\n\
         name: J\njobEnvironments:\n{}steps:\n{steps}",
        envs.concat()
    )
}

const SINGLE_LAYER: &str = "only one environment in the session stack may define any of \
                            onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit (RFC 0008).";

#[test]
fn a_declined_wrapper_is_not_a_layer() {
    let envs = [wrap_env("W1", "BY_DEFAULT"), wrap_env("W2", "BY_DEFAULT")];
    let declines = |step: &str, env: &str| {
        format!(
            "- name: {step}\n  dependencies: [{{environment: {env}, enableIf: false}}]\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    let plain =
        |step: &str| format!("- name: {step}\n  script: {{actions: {{onRun: {{command: x}}}}}}\n");
    // Every Step declines one of them: one layer each.
    let src = wrap_job(
        &envs,
        &format!("{}{}", declines("A", "W1"), declines("B", "W2")),
    );
    job_ok(&src);
    let created = create_ok(&src, &[], &[]);
    assert!(!enters(&created, 0, &Document::JobTemplate, "W1"));
    assert!(enters(&created, 0, &Document::JobTemplate, "W2"));
    // No Step declines: every Session has two layers, reported once.
    job_err(
        &wrap_job(&envs, &plain("A")),
        &[&format!("jobEnvironments:\n\t{SINGLE_LAYER}")],
    );
    // Only one Step declines: the other is reported, at the Step.
    job_err(
        &wrap_job(&envs, &format!("{}{}", declines("A", "W1"), plain("B"))),
        &[&format!("steps[1]:\n\t{SINGLE_LAYER}")],
    );
    // An expression entry may decline it, so the rule is left to job
    // creation, which checks the final set.
    let src = wrap_job(
        &envs,
        "- name: A\n  dependencies: [{environment: W1, enableIf: \"Job.Name == 'J'\"}]\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    job_ok(&src);
    create_err(
        &src,
        &[],
        &[],
        "JobTemplate",
        &[
            "steps[0]:\n\tits Sessions would enter 2 Environments that define wrap \
           hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'W1', 'W2'. A Session may have \
           at most one Environment defining wrap hooks (RFC 0008).",
        ],
    );
    create_ok(&src.replace("name: J\n", "name: K\n"), &[], &[]);
}

#[test]
fn a_declined_attached_wrapper_is_not_a_layer_at_check_or_job_creation() {
    let attached = "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR, \
                    WRAP_ACTIONS]\nenvironment:\n  name: Docker\n  script:\n    actions:\n      \
                    onWrapEnvEnter: {command: x}\n      onWrapTaskRun: {command: x}\n      \
                    onWrapEnvExit: {command: x}\n";
    let src = |enable_if: &str| {
        wrap_job(
            &[wrap_env("W1", "BY_DEFAULT")],
            &format!(
                "- name: A\n  dependencies: [{{environment: Docker, enableIf: {enable_if}}}]\n  \
                 script: {{actions: {{onRun: {{command: x}}}}}}\n"
            ),
        )
    };
    assert_eq!(
        check(&src("false"), &[attached]).unwrap(),
        Vec::<String>::new()
    );
    create_ok(&src("false"), &[attached], &[]);
    let two = "JobTemplate -> steps[0]:\n\tits Sessions would enter 2 Environments that define \
               wrap hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'Docker' \
               (EnvironmentTemplate[0]), 'W1'. A Session may have at most one Environment \
               defining wrap hooks (RFC 0008).";
    let err = check(&src("true"), &[attached]).unwrap_err();
    assert_errors(&err.to_string(), "Submission", &[two]);
    create_err(&src("true"), &[attached], &[], "Submission", &[two]);
    // An expression entry may decline it, so check leaves it to job
    // creation.
    assert_eq!(
        check(&src("\"Job.Name == 'J'\""), &[attached]).unwrap(),
        Vec::<String>::new()
    );
    create_err(
        &src("\"Job.Name == 'J'\""),
        &[attached],
        &[],
        "Submission",
        &[two],
    );
}

// ── Audit round 2: side effects on other code paths ─────────────────

/// An attached Environment Template `name` with `activation` BY_DEFAULT, a
/// parameter `Packages` with a `default`, and a requirement of at least 4
/// `amount.worker.gpu`.
fn gpu_attachment(name: &str) -> String {
    format!(
        r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: {name}
  envParameterDefinitions: [{{name: Packages, type: STRING, default: base}}]
  hostRequirements:
    amounts: [{{name: amount.worker.gpu, min: 4}}]
  variables: {{P: "{{{{Env.Param.Packages}}}}"}}
"#
    )
}

#[test]
fn an_ambiguous_disabled_entry_reports_only_the_ambiguity() {
    // Neither attached Conda is applied to A, so their hostRequirements do
    // not combine with A's own: the §3.2.1 item 3 error is the only one,
    // at check and at job creation.
    let src = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: A
  dependencies: [{environment: Conda, enableIf: false}]
  hostRequirements:
    amounts: [{name: amount.worker.gpu, max: 1}]
  script: {actions: {onRun: {command: x}}}
"#;
    let conda = gpu_attachment("Conda");
    let expected = "JobTemplate -> steps[0] -> dependencies[0] -> environment:\n\tnames \
                    Environment 'Conda', which 2 attached Environment Templates define \
                    (EnvironmentTemplate[0], EnvironmentTemplate[1]); an entry must match \
                    exactly one (Template Schemas §3.2.1 item 3).";
    let err = check(src, &[&conda, &conda]).unwrap_err();
    assert_errors(&err.to_string(), "Submission", &[expected]);
    create_err(src, &[&conda, &conda], &[], "Submission", &[expected]);
}

#[test]
fn an_ambiguous_entry_with_an_expression_enable_if_is_reported_at_check() {
    // A disabled entry's name is resolved as an enabled one's is, so the
    // §3.2.1 item 3 error holds whatever the expression resolves to, and
    // check reports it (§7.5 rule 5), as job creation does.
    let conda = conda("BY_DEFAULT");
    let expected = "JobTemplate -> steps[0] -> dependencies[0] -> environment:\n\tnames \
                    Environment 'Conda', which 2 attached Environment Templates define \
                    (EnvironmentTemplate[0], EnvironmentTemplate[1]); an entry must match \
                    exactly one (Template Schemas §3.2.1 item 3).";
    for enable_if in [r#""false""#, MODE_ON] {
        let src = external_entry(enable_if);
        let err = check(&src, &[&conda, &conda]).unwrap_err();
        assert_errors(&err.to_string(), "Submission", &[expected]);
        create_err(&src, &[&conda, &conda], &[], "Submission", &[expected]);
        create_err(
            &src,
            &[&conda, &conda],
            &[("Mode", "on")],
            "Submission",
            &[expected],
        );
    }
}

#[test]
fn an_ambiguous_expression_entry_reports_only_the_ambiguity_at_check() {
    // Neither ALWAYS Conda is applied to A, so their host requirements do
    // not combine with A's own at check, as at job creation.
    let src = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Mode, type: STRING, default: "off"}]
steps:
- name: A
  dependencies: [{environment: Conda, enableIf: "Param.Mode == 'on'"}]
  hostRequirements:
    amounts: [{name: amount.worker.gpu, max: 1}]
  script: {actions: {onRun: {command: x}}}
"#;
    let conda = gpu_attachment("Conda").replace(
        "  envParameterDefinitions",
        "  activation: ALWAYS\n  envParameterDefinitions",
    );
    let expected = "JobTemplate -> steps[0] -> dependencies[0] -> environment:\n\tnames \
                    Environment 'Conda', which 2 attached Environment Templates define \
                    (EnvironmentTemplate[0], EnvironmentTemplate[1]); an entry must match \
                    exactly one (Template Schemas §3.2.1 item 3).";
    let err = check(src, &[&conda, &conda]).unwrap_err();
    assert_errors(&err.to_string(), "Submission", &[expected]);
    for mode in ["on", "off"] {
        create_err(
            src,
            &[&conda, &conda],
            &[("Mode", mode)],
            "Submission",
            &[expected],
        );
    }
}

#[test]
fn a_disabled_entry_after_another_entry_is_reported_at_its_own_index() {
    // The disabled entry is dependencies[1]; job creation hands it to the
    // submission stage, whose error names that index.
    let src = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Mode, type: STRING, default: "off"}]
jobEnvironments:
- name: Lic
  variables: {V: x}
steps:
- name: A
  dependencies:
  - {environment: Lic}
  - {environment: Conda, enableIf: "Param.Mode == 'on'"}
  script: {actions: {onRun: {command: x}}}
"#;
    let always = conda("ALWAYS");
    create_err(
        src,
        &[&always],
        &[],
        "Submission",
        &[&always_error(
            "JobTemplate -> ",
            0,
            1,
            "A",
            "'Conda' (EnvironmentTemplate[0])",
        )],
    );
    let conda = conda("BY_DEFAULT");
    create_err(
        src,
        &[&conda, &conda],
        &[],
        "Submission",
        &[
            "JobTemplate -> steps[0] -> dependencies[1] -> environment:\n\tnames Environment \
           'Conda', which 2 attached Environment Templates define (EnvironmentTemplate[0], \
           EnvironmentTemplate[1]); an entry must match exactly one (Template Schemas §3.2.1 \
           item 3).",
        ],
    );
}

#[test]
fn a_declined_job_environments_host_requirements_do_not_apply_at_check() {
    // With an unrelated attachment, check combines each Step's host
    // requirements with those of the Environments it enters; A declines
    // Gpu, so Gpu's are not among them.
    let src = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: Gpu
  hostRequirements:
    amounts: [{name: amount.worker.gpu, min: 4}]
  variables: {V: x}
steps:
- name: A
  dependencies: [{environment: Gpu, enableIf: false}]
  hostRequirements:
    amounts: [{name: amount.worker.gpu, max: 1}]
  script: {actions: {onRun: {command: x}}}
"#;
    let other = conda("BY_DEFAULT").replace("name: Conda", "name: Other");
    assert_eq!(check(src, &[&other]).unwrap(), Vec::<String>::new());
    let created = create_ok(src, &[&other], &[]);
    assert!(!enters(&created, 0, &Document::JobTemplate, "Gpu"));
    assert_eq!(requirement_names(&created.job, 0), ["amount.worker.gpu"]);
}

#[test]
fn a_declined_job_environment_wrapper_is_not_a_layer_with_an_attached_wrapper() {
    // A declines the Job Environment W1, so the attached W2 is its only
    // wrap layer, at check and at job creation.
    let attached = "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR, \
                    WRAP_ACTIONS]\nenvironment:\n  name: W2\n  script:\n    actions:\n      \
                    onWrapEnvEnter: {command: x}\n      onWrapTaskRun: {command: x}\n      \
                    onWrapEnvExit: {command: x}\n";
    let src = wrap_job(
        &[wrap_env("W1", "BY_DEFAULT")],
        "- name: A\n  dependencies: [{environment: W1, enableIf: false}]\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    assert_eq!(check(&src, &[attached]).unwrap(), Vec::<String>::new());
    let created = create_ok(&src, &[attached], &[]);
    assert!(!enters(&created, 0, &Document::JobTemplate, "W1"));
    assert!(enters(
        &created,
        0,
        &Document::environment_template(0, None),
        "W2"
    ));
}

#[test]
fn a_declined_attachments_enable_if_is_not_evaluated() {
    // Lic's own enableIf fails for a Step that takes the default Index 5;
    // A declines Lic, so it is not evaluated for A. B supplies Index 0.
    let lic = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: Lic
  envParameterDefinitions: [{name: Index, type: INT, default: 5}]
  enableIf: "[true][Env.Param.Index]"
  variables: {V: x}
"#;
    let src = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: A
  dependencies: [{environment: Lic, enableIf: false}]
  script: {actions: {onRun: {command: x}}}
- name: B
  dependencies: [{environment: Lic, parameters: {Index: 0}}]
  script: {actions: {onRun: {command: x}}}
"#;
    let att = Document::environment_template(0, None);
    let created = create_ok(src, &[lic], &[]);
    assert!(!enters(&created, 0, &att, "Lic"));
    assert!(enters(&created, 1, &att, "Lic"));
}

#[test]
fn a_disabled_entry_for_a_job_environment_is_not_resolved_against_a_shadowed_attachment() {
    // The Job Environment Conda shadows the attached ALWAYS Conda
    // (§3.2.1 item 4), so the entry declines the Job Environment; it is
    // not the §3.2 item 3.2 error against the attachment.
    let src = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: Conda
  envParameterDefinitions: [{name: Packages, type: STRING, default: base}]
  variables: {P: "{{Env.Param.Packages}}"}
steps:
- name: A
  dependencies: [{environment: Conda, enableIf: false}]
  script: {actions: {onRun: {command: x}}}
"#;
    let created = create_ok(src, &[&conda("ALWAYS")], &[]);
    assert!(!enters(&created, 0, &Document::JobTemplate, "Conda"));
}
