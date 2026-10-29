// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `ENV_PARAMS` Step-side selection at job creation (Template Schemas §7.4):
//! each Step's Environment set from `activation` and its enabled entries
//! (§4 item 4, §3.2.2), `enableIf` (§3.2 item 5), `Job.EnvNames` (§7.3.1),
//! constraint 7 over the set (§3.2 constraint 7, §3.2.1), and the
//! `WRAP_ACTIONS` single-layer rule over each Step's set (plan R8).

use openjd_model::job::{Document, EnvironmentRef, Job};
use openjd_model::template::EnvironmentTemplate;
use openjd_model::{
    check_environment_templates, create_job, create_job_with_environment_templates,
    AttachedEnvironmentTemplate, CreatedJob, ModelError,
};

use crate::env_params_common::{
    assert_errors, env_ok, job_err, job_ok, job_params, job_params_with, posix_ctx, rendered_values,
};

/// Create the Job from `jt_src` with `env_srcs` attached, in order.
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

fn create_ok(jt_src: &str, env_srcs: &[&str], params: &[(&str, &str)]) -> Job {
    create(jt_src, env_srcs, params)
        .unwrap_or_else(|e| panic!("job creation failed:\n{e}"))
        .job
}

fn create_err(jt_src: &str, env_srcs: &[&str], model: &str, expected: &[&str]) {
    let err = create(jt_src, env_srcs, &[]).expect_err("expected job creation to fail");
    assert_errors(&err.to_string(), model, expected);
}

/// The `(document, name)` of each Job Environment a Step's Sessions enter.
fn entered(job: &Job, step: usize) -> Vec<String> {
    let step = &job.steps[step];
    job.job_environments
        .iter()
        .flatten()
        .filter(|e| {
            let docs: Vec<Document> = step
                .environments
                .iter()
                .flatten()
                .filter(|r| r.environment == e.name)
                .map(|r| r.document.clone())
                .collect();
            step.environments.is_none() || !docs.is_empty()
        })
        .map(|e| e.name.clone())
        .collect()
}

fn job_ref(name: &str) -> EnvironmentRef {
    EnvironmentRef {
        document: Document::JobTemplate,
        environment: name.to_string(),
    }
}

/// An attached Environment Template with `activation` `scope` and a body.
fn attached_env(name: &str, scope: &str, body: &str) -> String {
    format!(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
         environment:\n  name: {name}\n  activation: {scope}\n{body}"
    )
}

const VARS: &str = "  variables: {V: v}\n";

// ── Each Step's Environment set (§4 item 4) ─────────────────────────

const SCOPES: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- {name: All, variables: {V: v}}
- {name: Only, activation: ON_REQUEST, variables: {V: v}}
- {name: Last, activation: ALWAYS, variables: {V: v}}
steps:
- name: Requests
  dependencies: [{environment: Only}]
  script: {actions: {onRun: {command: x}}}
- name: Plain
  script: {actions: {onRun: {command: x}}}
- name: AlsoRequests
  dependencies: [{environment: Only}]
  script: {actions: {onRun: {command: x}}}
"#;

#[test]
fn a_step_enters_every_all_environment_and_the_on_request_ones_it_requests() {
    let job = create_ok(SCOPES, &[], &[]);
    // `None`: every Job Environment.
    assert_eq!(job.steps[0].environments, None);
    assert_eq!(entered(&job, 0), ["All", "Only", "Last"]);
    // The others keep their Job order.
    assert_eq!(
        job.steps[1].environments,
        Some(vec![job_ref("All"), job_ref("Last")])
    );
    assert!(job.steps[1].enters_environment(&Document::JobTemplate, "All"));
    assert!(!job.steps[1].enters_environment(&Document::JobTemplate, "Only"));
    assert!(job.steps[0].enters_environment(&Document::JobTemplate, "Only"));
    // An attached Environment of the same name is another Environment.
    assert!(!job.steps[1].enters_environment(&Document::environment_template(0, None), "All"));
    // The sharing rule's first condition (§3.2.2 ¶4).
    assert!(job.steps[0].same_environments(&job.steps[2]));
    assert!(!job.steps[0].same_environments(&job.steps[1]));
}

#[test]
fn the_steps_environments_serialize_only_when_they_are_not_every_one() {
    let job = create_ok(SCOPES, &[], &[]);
    let json = serde_json::to_value(&job).unwrap();
    assert!(json["steps"][0].get("environments").is_none());
    assert_eq!(
        json["steps"][1]["environments"],
        serde_json::json!([
            {"document": {"kind": "JobTemplate"}, "environment": "All"},
            {"document": {"kind": "JobTemplate"}, "environment": "Last"}
        ])
    );
    let back: openjd_model::job::Step = serde_json::from_value(json["steps"][1].clone()).unwrap();
    assert_eq!(back, job.steps[1]);
}

#[test]
fn an_on_request_environments_values_exist_only_for_requesting_steps() {
    let job = create_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: Lic
  activation: ON_REQUEST
  envParameterDefinitions: [{name: Product, type: STRING}]
  variables: {V: v}
steps:
- name: Render
  dependencies: [{environment: Lic, parameters: {Product: maya}}]
  script: {actions: {onRun: {command: x}}}
- name: Encode
  script: {actions: {onRun: {command: x}}}
"#,
        &[],
        &[],
    );
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["JobTemplate/Lic: Product=string:maya"]
    );
    assert_eq!(job.steps[1].environment_parameters, None);
    assert_eq!(job.steps[1].environments, Some(vec![]));
}

// ── Attached Environments (§3.2.1) ──────────────────────────────────

const REQUESTS_EXT: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- {name: Own, variables: {V: v}}
steps:
- name: Requests
  dependencies: [{environment: Ext, parameters: {Product: maya}}]
  script: {actions: {onRun: {command: x}}}
- name: Plain
  script: {actions: {onRun: {command: x}}}
"#;

fn ext(scope: &str) -> String {
    attached_env(
        "Ext",
        scope,
        "  envParameterDefinitions: [{name: Product, type: STRING}]\n  variables: {V: v}\n",
    )
}

#[test]
fn an_attached_on_request_environment_is_entered_by_requesting_steps() {
    let created = create(REQUESTS_EXT, &[&ext("ON_REQUEST")], &[]).unwrap();
    let job = &created.job;
    assert_eq!(
        created.environment_documents,
        [
            Document::environment_template(0, None),
            Document::JobTemplate
        ]
    );
    assert_eq!(job.steps[0].environments, None);
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["EnvironmentTemplate[0]/Ext: Product=string:maya"]
    );
    // The non-requesting Step neither enters it nor has to supply its
    // parameter without a default (§3.2.1, last paragraph).
    assert_eq!(job.steps[1].environments, Some(vec![job_ref("Own")]));
    assert_eq!(job.steps[1].environment_parameters, None);
}

#[test]
fn an_attached_all_environment_is_entered_by_every_step() {
    create_err(
        REQUESTS_EXT,
        &[&ext("ALWAYS")],
        "Submission",
        &["JobTemplate -> steps[1]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every attached Environment with activation ALWAYS, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1); missing: 'Product' of Environment 'Ext' (EnvironmentTemplate[0])."],
    );
}

#[test]
fn a_job_template_without_env_params_never_enters_an_on_request_attachment() {
    // §1.2 item 3: such a Job Template has no entries, so it requests
    // nothing.
    let job = create_ok(
        r#"
specificationVersion: jobtemplate-2023-09
name: J
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
        &[&ext("ON_REQUEST")],
        &[],
    );
    assert_eq!(job.steps[0].environments, Some(vec![]));
    assert_eq!(job.steps[0].environment_parameters, None);
}

#[test]
fn an_unreachable_on_request_attachment_is_not_an_error() {
    // Named like a Job Environment, so no entry can request it; no Session
    // enters it, so its parameter without a default needs no value.
    let created = create(
        &REQUESTS_EXT.replace(
            "environment: Ext, parameters: {Product: maya}",
            "environment: Own",
        ),
        &[&attached_env(
            "Own",
            "ON_REQUEST",
            "  envParameterDefinitions: [{name: Product, type: STRING}]\n  variables: {V: v}\n",
        )],
        &[],
    )
    .unwrap();
    let warnings: Vec<String> = created.warnings.iter().map(ToString::to_string).collect();
    assert_eq!(
        warnings,
        ["EnvironmentTemplate[0] -> environment -> name: Environment 'Own' has the same name as a jobEnvironments entry of the Job Template, so no 'environment:' dependency of the Job Template can reach it (Template Schemas §3.2.1 item 4)."]
    );
    assert_eq!(
        created.job.steps[0].environments,
        Some(vec![job_ref("Own")])
    );
}

// ── enableIf (§3.2 item 5) ──────────────────────────────────────────

const ENABLE_IF: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Mode, type: STRING, default: "on"}]
jobEnvironments:
- name: Conda
  activation: SCOPE
  envParameterDefinitions: [{name: Packages, type: STRING, default: numpy}]
  variables: {V: v}
steps:
- name: S
  let: ["on = Param.Mode == 'on'"]
  dependencies:
  - environment: Conda
    enableIf: "on"
    parameters: {Packages: ffmpeg}
  script: {actions: {onRun: {command: x}}}
"#;

#[test]
fn an_enabled_entry_requests_and_supplies() {
    let src = ENABLE_IF.replace("SCOPE", "ON_REQUEST");
    let job = create_ok(&src, &[], &[]);
    assert_eq!(job.steps[0].environments, None);
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["JobTemplate/Conda: Packages=string:ffmpeg"]
    );
}

#[test]
fn a_disabled_entry_is_as_if_absent() {
    // ON_REQUEST: not entered.
    let src = ENABLE_IF.replace("SCOPE", "ON_REQUEST");
    let job = create_ok(&src, &[], &[("Mode", "off")]);
    assert_eq!(job.steps[0].environments, Some(vec![]));
    assert_eq!(job.steps[0].environment_parameters, None);
    // ALWAYS: entered with every default.
    let src = ENABLE_IF.replace("SCOPE", "ALWAYS");
    let job = create_ok(&src, &[], &[("Mode", "off")]);
    assert_eq!(job.steps[0].environments, None);
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["JobTemplate/Conda: Packages=string:numpy"]
    );
}

#[test]
fn a_literal_false_entry_requests_nothing_at_job_creation() {
    let src = ENABLE_IF
        .replace("SCOPE", "ON_REQUEST")
        .replace("enableIf: \"on\"\n", "enableIf: false\n");
    let job = create_ok(&src, &[], &[]);
    assert_eq!(job.steps[0].environments, Some(vec![]));
    assert!(!job.steps[0].enters_environment(&Document::JobTemplate, "Conda"));
    assert_eq!(job.steps[0].environment_parameters, None);
}

#[test]
fn a_windows_submission_keeps_the_template_scope_for_enable_if() {
    // `entry_scope` evaluates again, in the submission path format, only
    // the bindings the enabled entries' values read (`w`). The entry's
    // `enableIf` reads `on`, which it gets from the template scope
    // (Template Schemas §3.2 item 5, spec 5535230), so it still resolves.
    let jt = job_ok(
        &ENABLE_IF
            .replace("SCOPE", "ON_REQUEST")
            .replace(
                "let: [\"on = Param.Mode == 'on'\"]",
                "let: [\"on = Param.Mode == 'on'\", \"w = 'x'\"]",
            )
            .replace("{Packages: ffmpeg}", "{Packages: \"{{ w }}\"}"),
    );
    let ctx =
        posix_ctx(&jt).with_submission_path_format(openjd_expr::path_mapping::PathFormat::Windows);
    let job = create_job(&jt, &job_params(&jt, &[]), &ctx).unwrap();
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["JobTemplate/Conda: Packages=string:x"]
    );
}

#[test]
fn a_disabled_entry_is_not_resolved_against_the_attachments() {
    // A required entry naming no Environment is no error while disabled.
    let src = ENABLE_IF
        .replace("- name: Conda\n  activation: SCOPE\n", "- name: Other\n")
        .replace("environment: Conda", "environment: Missing");
    let job = create_ok(&src, &[], &[("Mode", "off")]);
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["JobTemplate/Other: Packages=string:numpy"]
    );
    create_err(
        &src,
        &[],
        "Submission",
        &["JobTemplate -> steps[0] -> dependencies[0] -> environment:\n\tnames Environment 'Missing', which is not a jobEnvironments entry of the Job Template, and no Environment Template is attached. The entry is required; set 'required: false' if the Step can run without the Environment (Template Schemas §3.2.1 item 2)."],
    );
}

#[test]
fn a_null_enable_if_is_an_error() {
    let src = ENABLE_IF
        .replace("SCOPE", "ALWAYS")
        .replace("enableIf: \"on\"\n", "enableIf: \"null if on else true\"\n");
    create_err(
        &src,
        &[],
        "JobTemplate",
        &["steps[0] -> dependencies[0] -> enableIf:\n\tresolved to null, which does not fit type bool (Template Schemas §7.5)."],
    );
    create_ok(&src, &[], &[("Mode", "off")]);
}

#[test]
fn an_enable_if_that_fails_is_reported_once() {
    // The entry's Environment gets no values, so its parameter without a
    // default is not also reported as missing.
    let src = ENABLE_IF
        .replace("SCOPE", "ALWAYS")
        .replace("default: numpy", "minLength: 1")
        .replace(
            "enableIf: \"on\"\n",
            "enableIf: \"[1][0 if on else 5] == 1\"\n",
        );
    let err = create(&src, &[], &[("Mode", "off")])
        .unwrap_err()
        .to_string();
    assert_errors(
        &err,
        "JobTemplate",
        &["steps[0] -> dependencies[0] -> enableIf:\n\tcould not be evaluated: Index 5 out of bounds for list of length 1\n  [1][0 if on else 5] == 1\n  ~~~^~~~~~~~~~~~~~~~"],
    );
}

#[test]
fn an_enable_if_that_resolves_to_another_type_is_an_error() {
    // Template validation rejects it for a decoded template; job creation
    // still checks it, since the template's fields are public.
    let mut jt = job_ok(&ENABLE_IF.replace("SCOPE", "ALWAYS"));
    jt.steps[0].dependencies.as_mut().unwrap()[0].enable_if =
        Some(openjd_model::template::ExprField::Expression("1".into()));
    let err = create_job(&jt, &job_params(&jt, &[]), &posix_ctx(&jt))
        .unwrap_err()
        .to_string();
    assert_errors(
        &err,
        "JobTemplate",
        &["steps[0] -> dependencies[0] -> enableIf:\n\tresolved to a value of type int, which does not fit type bool (Template Schemas §7.5)."],
    );
}

#[test]
fn constraint_7_is_checked_at_job_creation_for_an_expression_enable_if() {
    let src = ENABLE_IF
        .replace("default: numpy", "minLength: 1")
        .replace("    parameters: {Packages: ffmpeg}\n", "");
    let missing = "steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every jobEnvironments entry with activation ALWAYS, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7); missing: 'Packages' of Environment 'Conda'.";
    // Enabled: the requested Environment needs the value.
    let only = src.replace("SCOPE", "ON_REQUEST");
    create_err(&only, &[], "JobTemplate", &[missing]);
    // Disabled: an ON_REQUEST Environment is not entered...
    create_ok(&only, &[], &[("Mode", "off")]);
    // ...and an ALWAYS one is entered with defaults, so the value is missing.
    // The message says the entry was disabled (B2-3).
    let err = create(&src.replace("SCOPE", "ALWAYS"), &[], &[("Mode", "off")])
        .unwrap_err()
        .to_string();
    let disabled = missing.replace(
        "'Conda'.",
        "'Conda' (the Step's 'environment:' dependency for it is disabled by its enableIf, so it \
         supplies nothing; Template Schemas §3.2 item 5).",
    );
    assert_errors(&err, "JobTemplate", &[&disabled]);
    // Enabled with an ALWAYS Environment: missing, with no note.
    let err = create(&src.replace("SCOPE", "ALWAYS"), &[], &[])
        .unwrap_err()
        .to_string();
    assert_errors(&err, "JobTemplate", &[missing]);
}

#[test]
fn a_constant_expression_enable_if_defers_constraint_7_to_job_creation() {
    // §3.2 checked-at paragraph and §7.5 item 1: only an absent enableIf or
    // a boolean scalar is known at template validation. "true" is the
    // expression `true`, so template validation accepts the missing value
    // and job creation, which evaluates it, rejects it; "false" disables
    // the entry when the Job is created.
    let src = ENABLE_IF
        .replace("SCOPE", "ON_REQUEST")
        .replace("default: numpy", "minLength: 1")
        .replace("    parameters: {Packages: ffmpeg}\n", "");
    let missing = "steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every jobEnvironments entry with activation ALWAYS, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7); missing: 'Packages' of Environment 'Conda'.";
    let constant_true = src.replace("enableIf: \"on\"\n", "enableIf: \"true\"\n");
    job_ok(&constant_true);
    create_err(&constant_true, &[], "JobTemplate", &[missing]);
    // The boolean scalar is decided at template validation.
    job_err(
        &src.replace("enableIf: \"on\"\n", "enableIf: true\n"),
        &[missing],
    );
    let constant_false = src.replace("enableIf: \"on\"\n", "enableIf: \"false\"\n");
    let job = create_ok(&constant_false, &[], &[]);
    assert_eq!(job.steps[0].environments, Some(vec![]));
}

const ENABLE_IF_CONTEXT: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: P, type: STRING, default: 'C:\a\b.txt'}]
jobEnvironments:
- {name: E, activation: ON_REQUEST, variables: {V: v}}
steps:
- name: S
  dependencies: [{environment: E, enableIf: ENABLE}]
  script: {actions: {onRun: {command: x}}}
"#;

#[test]
fn enable_if_is_evaluated_with_posix_paths_whatever_the_submission_path_format() {
    // An `enableIf` is a TEMPLATE-scope expression field, not host
    // context: path operations use POSIX paths, so `.name` never splits on
    // `\`, even for a Windows submission (unlike an entry's parameter
    // values, §4.5).
    let jt = job_ok(&ENABLE_IF_CONTEXT.replace("ENABLE", r#""path(Param.P).name == 'b.txt'""#));
    let values = job_params(&jt, &[]);
    let posix = create_job(&jt, &values, &posix_ctx(&jt)).unwrap();
    assert_eq!(posix.steps[0].environments, Some(vec![]));
    let windows =
        posix_ctx(&jt).with_submission_path_format(openjd_expr::path_mapping::PathFormat::Windows);
    let windows = create_job(&jt, &values, &windows).unwrap();
    assert_eq!(windows.steps[0].environments, Some(vec![]));
}

#[test]
fn enable_if_is_evaluated_within_the_caller_budgets() {
    // CallerLimits bound an enableIf's evaluation at template validation
    // and at job creation, as for every other expression.
    let src = ENABLE_IF_CONTEXT.replace("ENABLE", r#""sum([1] * 1000) > 0""#);
    let limits = openjd_model::CallerLimits {
        max_eval_operations: Some(50),
        ..Default::default()
    };
    let err = openjd_model::decode_job_template(
        crate::env_params_common::yaml(&src),
        Some(crate::env_params_common::ALLOW),
        &limits,
    )
    .unwrap_err()
    .to_string();
    assert_errors(
        &err,
        "JobTemplate",
        &["steps[0] -> dependencies[0] -> enableIf:\n\tInvalid expression: Expression operation count (1003) exceeded limit (50)\n  sum([1] * 1000) > 0\n      ~~~~^~~~~~"],
    );
    let jt = job_ok(&src);
    let ctx = posix_ctx(&jt).with_caller_limits(limits);
    let err = create_job(&jt, &job_params(&jt, &[]), &ctx)
        .unwrap_err()
        .to_string();
    assert_errors(
        &err,
        "JobTemplate",
        &["steps[0] -> dependencies[0] -> enableIf:\n\tcould not be evaluated: Expression operation count (1003) exceeded limit (50)\n  sum([1] * 1000) > 0\n      ~~~~^~~~~~"],
    );
}

#[test]
fn enable_if_is_evaluated_within_the_caller_memory_budget() {
    let src = ENABLE_IF_CONTEXT.replace("ENABLE", r#""len('x' * 100000) > 0""#);
    let limits = openjd_model::CallerLimits {
        max_eval_memory_bytes: Some(1000),
        ..Default::default()
    };
    let err = openjd_model::decode_job_template(
        crate::env_params_common::yaml(&src),
        Some(crate::env_params_common::ALLOW),
        &limits,
    )
    .unwrap_err()
    .to_string();
    assert_errors(
        &err,
        "JobTemplate",
        &["steps[0] -> dependencies[0] -> enableIf:\n\tInvalid expression: Expression memory usage (100136 bytes) exceeded limit (1000 bytes)\n  len('x' * 100000) > 0\n      ~~~~^~~~~~~~"],
    );
    let jt = job_ok(&src);
    let ctx = posix_ctx(&jt).with_caller_limits(limits);
    let err = create_job(&jt, &job_params(&jt, &[]), &ctx)
        .unwrap_err()
        .to_string();
    assert_errors(
        &err,
        "JobTemplate",
        &["steps[0] -> dependencies[0] -> enableIf:\n\tcould not be evaluated: Expression memory usage (100136 bytes) exceeded limit (1000 bytes)\n  len('x' * 100000) > 0\n      ~~~~^~~~~~~~"],
    );
}

// ── Job.EnvNames (§7.3.1, §7.4) ─────────────────────────────────────

fn env_names_of(job: &Job, step: usize, binding: &str) -> serde_json::Value {
    let table = job.steps[step].resolved_symtab.as_ref().unwrap().as_value();
    table
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == binding)
        .map(|e| e["value"].clone())
        .unwrap_or(serde_json::Value::Null)
}

const ENV_NAMES: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- {name: J1, variables: {V: v}}
- {name: J2, activation: ON_REQUEST, variables: {V: v}}
steps:
- name: S
  let: ["names = Job.EnvNames.join(',')"]
  stepEnvironments:
  - {name: StepEnv, variables: {V: v}}
  script: {actions: {onRun: {command: x, args: ["{{ names }}"]}}}
"#;

#[test]
fn job_env_names_lists_the_attachments_then_the_job_environments() {
    // Attachments first, in attachment order (plan P2); a shared name
    // repeats; an unrequested ON_REQUEST Environment is listed; Step
    // Environments are not.
    let a = attached_env("A", "ALWAYS", VARS);
    let b = attached_env("B", "ON_REQUEST", VARS);
    let job = create_ok(ENV_NAMES, &[&a, &b, &a], &[]);
    assert_eq!(env_names_of(&job, 0, "names"), "A,B,A,J1,J2");
    let job = create_ok(ENV_NAMES, &[], &[]);
    assert_eq!(env_names_of(&job, 0, "names"), "J1,J2");
}

#[test]
fn job_env_names_is_concrete_before_step_let_and_enable_if() {
    // A `let` that reads it feeds an `enableIf` and an external entry's
    // value (§7.4).
    let src = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  let: ["docker = 'Docker' in Job.EnvNames"]
  dependencies:
  - {environment: Conda, enableIf: "not docker", parameters: {Note: "{{ Job.EnvNames.join('+') }}"}}
  - {environment: Docker, enableIf: "docker"}
  script: {actions: {onRun: {command: x}}}
"#;
    let conda = attached_env(
        "Conda",
        "ON_REQUEST",
        "  envParameterDefinitions: [{name: Note, type: STRING, default: none}]\n  variables: {V: v}\n",
    );
    let docker = attached_env("Docker", "ON_REQUEST", VARS);
    let job = create_ok(src, &[&conda], &[]);
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["EnvironmentTemplate[0]/Conda: Note=string:Conda"]
    );
    assert_eq!(job.steps[0].environments, None);
    let job = create_ok(src, &[&conda, &docker], &[]);
    assert_eq!(job.steps[0].environment_parameters, None);
    assert_eq!(
        job.steps[0].environments,
        Some(vec![EnvironmentRef {
            document: Document::environment_template(1, None),
            environment: "Docker".to_string(),
        }])
    );
}

#[test]
fn job_env_names_is_frozen_for_environment_scripts() {
    let env_symtab = |job: &Job, i: usize| -> Vec<String> {
        job.job_environments.as_ref().unwrap()[i]
            .resolved_symtab
            .as_ref()
            .unwrap()
            .as_value()
            .as_array()
            .unwrap()
            .iter()
            .map(|e| format!("{}={}", e["name"].as_str().unwrap(), e["value"]))
            .collect()
    };
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: Own
  script: {actions: {onEnter: {command: x, args: ["{{ Job.EnvNames }}"]}}}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#;
    let att = attached_env(
        "A",
        "ALWAYS",
        "  script: {actions: {onEnter: {command: x, args: [\"{{ Job.EnvNames }}\"]}}}\n",
    );
    let job = create_ok(jt, &[&att], &[]);
    assert_eq!(env_symtab(&job, 0), [r#"Job.EnvNames=["A","Own"]"#]);
    assert_eq!(env_symtab(&job, 1), [r#"Job.EnvNames=["A","Own"]"#]);
}

// ── Wrap layers over each Step's set (plan R8) ──────────────────────

fn wrap(name: &str) -> String {
    format!(
        "- name: {name}\n  activation: ON_REQUEST\n  script:\n    actions:\n      \
         onWrapEnvEnter: {{command: x}}\n      onWrapTaskRun: {{command: x}}\n      \
         onWrapEnvExit: {{command: x}}\n"
    )
}

#[test]
fn wrap_layers_are_checked_over_the_resolved_set() {
    // Template validation does not count entries with an expression
    // `enableIf`; job creation checks the Step's resolved set.
    let src = format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR, WRAP_ACTIONS]\n\
         name: J\nparameterDefinitions: [{{name: Both, type: STRING, default: 'no'}}]\n\
         jobEnvironments:\n{}{}steps:\n- name: S\n  dependencies:\n  - {{environment: W1}}\n  \
         - {{environment: W2, enableIf: \"Param.Both == 'yes'\"}}\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n",
        wrap("W1"),
        wrap("W2")
    );
    let jt = job_ok(&src);
    let ctx = posix_ctx(&jt);
    create_job(&jt, &job_params(&jt, &[]), &ctx).unwrap();
    let err = create_job(&jt, &job_params(&jt, &[("Both", "yes")]), &ctx)
        .unwrap_err()
        .to_string();
    assert_errors(
        &err,
        "Submission",
        &["JobTemplate -> steps[0]:\n\tits Sessions would enter 2 Environments that define wrap hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'W1', 'W2'. A Session may have at most one Environment defining wrap hooks (RFC 0008)."],
    );
}

#[test]
fn wrap_layers_include_the_attached_environments() {
    let jt = format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR, WRAP_ACTIONS]\n\
         name: J\njobEnvironments:\n{}steps:\n- name: A\n  dependencies: [{{environment: W1}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n- name: B\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n",
        wrap("W1")
    );
    let att = |scope: &str| {
        attached_env(
            "Att",
            scope,
            "  script:\n    actions:\n      onWrapEnvEnter: {command: x}\n      \
             onWrapTaskRun: {command: x}\n      onWrapEnvExit: {command: x}\n",
        )
        .replace("[ENV_PARAMS, EXPR]", "[ENV_PARAMS, EXPR, WRAP_ACTIONS]")
    };
    // ON_REQUEST and unrequested: no Session has both.
    create_ok(&jt, &[&att("ON_REQUEST")], &[]);
    // ALWAYS: Step A's Sessions have both; Step B's only the attachment.
    create_err(
        &jt,
        &[&att("ALWAYS")],
        "Submission",
        &["JobTemplate -> steps[0]:\n\tits Sessions would enter 2 Environments that define wrap hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'Att' (EnvironmentTemplate[0]), 'W1'. A Session may have at most one Environment defining wrap hooks (RFC 0008)."],
    );
}

#[test]
fn wrap_layers_include_an_attachment_and_a_step_environment() {
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR, WRAP_ACTIONS]
name: J
steps:
- name: S
  stepEnvironments:
  - name: SW
    script:
      actions:
        onWrapEnvEnter: {command: x}
        onWrapTaskRun: {command: x}
        onWrapEnvExit: {command: x}
  script: {actions: {onRun: {command: x}}}
"#;
    let att = attached_env(
        "Att",
        "ALWAYS",
        "  script:\n    actions:\n      onWrapEnvEnter: {command: x}\n      \
         onWrapTaskRun: {command: x}\n      onWrapEnvExit: {command: x}\n",
    )
    .replace("[ENV_PARAMS, EXPR]", "[ENV_PARAMS, EXPR, WRAP_ACTIONS]");
    create_err(
        jt,
        &[&att],
        "Submission",
        &["JobTemplate -> steps[0]:\n\tits Sessions would enter 2 Environments that define wrap hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'Att' (EnvironmentTemplate[0]), 'SW' (stepEnvironments). A Session may have at most one Environment defining wrap hooks (RFC 0008)."],
    );
}

#[test]
fn ambiguous_on_request_attachments_no_entry_names_are_not_an_error() {
    // No entry can request them, so no Session enters them.
    let jt_src = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#;
    let dup = attached_env(
        "Dup",
        "ON_REQUEST",
        "  envParameterDefinitions: [{name: P, type: STRING}]\n  variables: {V: v}\n",
    );
    let job = create_ok(jt_src, &[&dup, &dup], &[]);
    assert_eq!(job.steps[0].environments, Some(vec![]));
    let jt = job_ok(jt_src);
    let envs = [env_ok(&dup), env_ok(&dup)];
    let attached: Vec<_> = envs.iter().map(AttachedEnvironmentTemplate::new).collect();
    assert_eq!(
        check_environment_templates(&jt, &attached, &posix_ctx(&jt)).unwrap(),
        []
    );
}

// ── check_environment_templates (§3.2, last paragraph) ──────────────

#[test]
fn the_early_check_holds_a_requesting_step_to_an_on_request_attachment() {
    let jt = job_ok(&REQUESTS_EXT.replace(", parameters: {Product: maya}", ""));
    let only = env_ok(&ext("ON_REQUEST"));
    let err = check_environment_templates(
        &jt,
        &[AttachedEnvironmentTemplate::new(&only)],
        &posix_ctx(&jt),
    )
    .unwrap_err()
    .to_string();
    assert_errors(
        &err,
        "Submission",
        &["JobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every attached Environment with activation ALWAYS, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1); missing: 'Product' of Environment 'Ext' (EnvironmentTemplate[0])."],
    );
}

#[test]
fn the_early_check_follows_activation_and_literal_enable_if() {
    let jt = job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: Requests
  dependencies: [{environment: Ext, parameters: {Product: maya}}]
  script: {actions: {onRun: {command: x}}}
- name: Plain
  script: {actions: {onRun: {command: x}}}
- name: Disabled
  dependencies: [{environment: Missing, enableIf: false}]
  script: {actions: {onRun: {command: x}}}
- name: Deferred
  dependencies: [{environment: Ext, enableIf: "Job.Name == 'K'"}, {environment: Gone, enableIf: "false"}]
  script: {actions: {onRun: {command: x}}}
"#,
    );
    let only = env_ok(&ext("ON_REQUEST"));
    let ctx = posix_ctx(&jt);
    // Plain does not enter Ext; Disabled's entry is as if absent; Deferred's
    // entries are checked at job creation.
    assert_eq!(
        check_environment_templates(&jt, &[AttachedEnvironmentTemplate::new(&only)], &ctx).unwrap(),
        []
    );
    let all = env_ok(&ext("ALWAYS"));
    let err = check_environment_templates(&jt, &[AttachedEnvironmentTemplate::new(&all)], &ctx)
        .unwrap_err()
        .to_string();
    let missing = |i: usize| {
        format!("JobTemplate -> steps[{i}]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every attached Environment with activation ALWAYS, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1); missing: 'Product' of Environment 'Ext' (EnvironmentTemplate[0]).")
    };
    assert_errors(&err, "Submission", &[&missing(1), &missing(2)]);
}
