// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `ENV_PARAMS` in the Session runtime: entering an Environment with its
//! effective parameter values (`Session::enter_environment_with_parameters`)
//! binds `Env.Param.*` (path-mapped) and `Env.RawParam.*` (unmapped) for that
//! Environment's own `variables`, `let` bindings, actions, embedded files,
//! and wrap hooks, and for nothing else (Template Schemas §3.2.2, §4.5,
//! §7.3.1; How Jobs Are Run, Environment Parameters).
//!
//! Most tests go end to end: decode a Job Template (and any attached
//! Environment Templates), run `create_job_with_environment_templates` to
//! get the combined Job, then enter its Environments with each Step's
//! values and run the Step, collecting every action's stdout.

#![cfg(unix)] // actions run `sh -c "echo ..."`

use openjd_expr::format_string::FormatString;
use openjd_expr::SerializedSymbolTable;
use openjd_model::job::{
    Action, Document, Environment, EnvironmentActions, EnvironmentParameterValues,
    EnvironmentScript, Job, Step, StepActions, StepScript,
};
use openjd_model::template::parse::{document_string_to_object, DocumentType};
use openjd_model::template::{EnvironmentTemplate, JobTemplate};
use openjd_model::{
    create_job_with_environment_templates, decode_environment_template, decode_job_template,
    AttachedEnvironmentTemplate, CallerLimits, JobParameterValues, ModelProfile, ValidationContext,
};
use openjd_sessions::error::SessionError;
use openjd_sessions::path_mapping::{PathFormat, PathMappingRule};
use openjd_sessions::session::{Session, SessionState};
use tempfile::TempDir;

// ────────────────────────────────────────────────────────────────────
// Submission helpers (model side)
// ────────────────────────────────────────────────────────────────────

/// The extensions every decode allows, named explicitly so the tests do
/// not change when `ModelExtension::ALL` grows.
const ALLOW: &[&str] = &["ENV_PARAMS", "EXPR", "WRAP_ACTIONS"];

fn yaml(s: &str) -> serde_json::Value {
    document_string_to_object(s, DocumentType::Yaml, &CallerLimits::default()).unwrap()
}

fn job_template(s: &str) -> JobTemplate {
    decode_job_template(yaml(s), Some(ALLOW), &CallerLimits::default())
        .unwrap_or_else(|e| panic!("expected a valid Job Template, got:\n{e}"))
}

fn environment_template(s: &str) -> EnvironmentTemplate {
    decode_environment_template(yaml(s), Some(ALLOW), &CallerLimits::default())
        .unwrap_or_else(|e| panic!("expected a valid Environment Template, got:\n{e}"))
}

fn ctx(jt: &JobTemplate) -> ValidationContext {
    jt.default_validation_context()
        .with_submission_path_format(openjd_expr::path_mapping::PathFormat::Posix)
}

fn job_parameters(jt: &JobTemplate, envs: &[EnvironmentTemplate]) -> JobParameterValues {
    let mut opts = openjd_model::PathParameterOptions::new("/templates", "/work");
    opts.path_format = openjd_expr::path_mapping::PathFormat::Posix;
    openjd_model::preprocess_job_parameters(jt, &Default::default(), envs, &opts)
        .unwrap_or_else(|e| panic!("preprocess_job_parameters failed:\n{e}"))
}

/// A submitted Job: the combined Job, the document of each of its
/// `job_environments`, and the merged Job Parameter values.
struct Submitted {
    job: Job,
    documents: Vec<Document>,
    params: JobParameterValues,
}

/// Run job creation and submission for `jt_src` with `env_srcs` attached,
/// as a scheduler would before starting any Session.
fn submit(jt_src: &str, env_srcs: &[&str]) -> Result<Submitted, String> {
    let jt = job_template(jt_src);
    let envs: Vec<EnvironmentTemplate> = env_srcs.iter().map(|s| environment_template(s)).collect();
    let params = job_parameters(&jt, &envs);
    let ctx = ctx(&jt);
    let attached: Vec<AttachedEnvironmentTemplate<'_>> =
        envs.iter().map(AttachedEnvironmentTemplate::new).collect();
    let created = create_job_with_environment_templates(&jt, &attached, &params, &ctx)
        .map_err(|e| e.to_string())?;
    Ok(Submitted {
        job: created.job,
        documents: created.environment_documents,
        params,
    })
}

fn submit_ok(jt_src: &str, env_srcs: &[&str]) -> Submitted {
    submit(jt_src, env_srcs).unwrap_or_else(|e| panic!("submission failed:\n{e}"))
}

fn step<'a>(job: &'a Job, name: &str) -> &'a Step {
    job.steps.iter().find(|s| s.name == name).unwrap()
}

/// The Step's values for the Environment declared by `document` and named
/// `environment`, keyed by both as the model's docs require.
fn values_for<'a>(
    step: &'a Step,
    document: &Document,
    environment: &str,
) -> Option<&'a EnvironmentParameterValues> {
    step.environment_parameters
        .as_deref()?
        .iter()
        .find(|v| &v.document == document && v.environment == environment)
}

// ────────────────────────────────────────────────────────────────────
// Session helpers
// ────────────────────────────────────────────────────────────────────

fn session(tmp: &TempDir, params: &JobParameterValues, rules: Vec<PathMappingRule>) -> Session {
    // `new_for_test` does not create its embedded-files directory.
    std::fs::create_dir_all(tmp.path().join("embedded_files")).unwrap();
    let mut s = Session::new_for_test(tmp.path().to_path_buf()).with_profile(
        ModelProfile::new(openjd_model::types::SpecificationRevision::V2023_09)
            .with_extensions(ALLOW.iter().map(|e| e.parse().unwrap()).collect()),
    );
    s.set_job_parameter_values_for_test(params.clone());
    s.with_path_mapping(rules)
}

fn rule(src: &str, dst: &str) -> PathMappingRule {
    PathMappingRule {
        source_path_format: PathFormat::Posix,
        source_path: src.to_string(),
        destination_path: dst.to_string(),
    }
}

/// Enter every Job Environment of `sub` with `step_name`'s values, run the
/// Step once, and exit the Environments in reverse. Returns every action's
/// stdout, in order, one trimmed line per action that printed.
async fn run_step(sub: &Submitted, step_name: &str, rules: Vec<PathMappingRule>) -> Vec<String> {
    let tmp = TempDir::new().unwrap();
    let mut session = session(&tmp, &sub.params, rules);
    let step = step(&sub.job, step_name);
    let mut out = Vec::new();
    let mut ids = Vec::new();
    for (env, doc) in sub
        .job
        .job_environments
        .iter()
        .flatten()
        .zip(&sub.documents)
    {
        let values = values_for(step, doc, &env.name);
        let (id, stdout) = session
            .enter_environment_with_parameters(env, None, values, None, None)
            .await
            .unwrap_or_else(|e| panic!("entering '{}' failed: {e}", env.name));
        assert_eq!(session.environment_parameters(&id), values);
        out.push(stdout);
        ids.push(id);
    }
    let result = session
        .run_task(
            &step.name,
            &step.script,
            None,
            step.resolved_symtab.as_ref(),
            None,
        )
        .await
        .unwrap();
    out.push(result.stdout);
    for id in ids.iter().rev() {
        out.push(
            session
                .exit_environment(id, None, false, None)
                .await
                .unwrap(),
        );
        assert_eq!(session.environment_parameters(id), None);
    }
    out.iter()
        .flat_map(|s| s.lines())
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

// ────────────────────────────────────────────────────────────────────
// Defaults, per-Step overrides, and the stored values
// ────────────────────────────────────────────────────────────────────

/// One Job Environment that uses its parameters in every place a
/// declaring Environment may: `variables`, script `let`, `onEnter`,
/// `onExit`, and an embedded file.
const LICENSE: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions:
- {name: Seats, type: INT, default: 4}
jobEnvironments:
- name: License
  envParameterDefinitions:
  - {name: Count, type: INT, default: "{{Param.Seats}}"}
  - {name: Server, type: STRING, default: lic.example.com}
  variables:
    LIC_COUNT: "{{Env.Param.Count}}"
  script:
    let: ["twice = Env.Param.Count * 2"]
    embeddedFiles:
    - name: Cfg
      type: TEXT
      data: "server={{Env.Param.Server}}"
    actions:
      onEnter:
        command: sh
        args: ["-c", "echo enter count={{Env.Param.Count}} raw={{Env.RawParam.Count}} twice={{twice}} var=$LIC_COUNT file=$(cat {{Env.File.Cfg}})"]
      onExit:
        command: sh
        args: ["-c", "echo exit count={{Env.Param.Count}} server={{Env.Param.Server}}"]
steps:
- name: Default
  script: {actions: {onRun: {command: sh, args: ["-c", "echo task var=$LIC_COUNT"]}}}
- name: Override
  dependencies:
  - environment: License
    parameters: {Count: 9, Server: other.example.com}
  script: {actions: {onRun: {command: sh, args: ["-c", "echo task var=$LIC_COUNT"]}}}
"#;

#[tokio::test]
async fn a_step_that_names_no_entry_takes_every_default() {
    let sub = submit_ok(LICENSE, &[]);
    assert_eq!(
        run_step(&sub, "Default", vec![]).await,
        [
            "enter count=4 raw=4 twice=8 var=4 file=server=lic.example.com",
            "task var=4",
            "exit count=4 server=lic.example.com",
        ]
    );
}

#[tokio::test]
async fn a_steps_entry_overrides_the_defaults_for_that_step_only() {
    let sub = submit_ok(LICENSE, &[]);
    assert_eq!(
        run_step(&sub, "Override", vec![]).await,
        [
            "enter count=9 raw=9 twice=18 var=9 file=server=other.example.com",
            "task var=9",
            "exit count=9 server=other.example.com",
        ]
    );
}

/// The onExit binds the values the Environment was entered with, even
/// though `exit_environment` takes no values and the caller passes no
/// table (§3.2.2: the Session keeps the values until it exits).
#[tokio::test]
async fn on_exit_reuses_the_values_stored_at_entry() {
    let sub = submit_ok(LICENSE, &[]);
    let tmp = TempDir::new().unwrap();
    let mut session = session(&tmp, &sub.params, vec![]);
    let env = &sub.job.job_environments.as_ref().unwrap()[0];
    let values = values_for(
        step(&sub.job, "Override"),
        &Document::JobTemplate,
        "License",
    );
    let (id, _) = session
        .enter_environment_with_parameters(env, None, values, Some("lic"), None)
        .await
        .unwrap();
    assert_eq!(id, "lic");
    assert_eq!(session.environment_parameters("lic"), values);
    let out = session
        .exit_environment(&id, None, false, None)
        .await
        .unwrap();
    assert_eq!(out.trim(), "exit count=9 server=other.example.com");
    assert_eq!(session.environment_parameters("lic"), None);
}

// ────────────────────────────────────────────────────────────────────
// PATH and LIST[PATH]: mapped in Env.Param, raw in Env.RawParam
// ────────────────────────────────────────────────────────────────────

/// `maxLength: 13` fits the unmapped `/mnt/src/data` but not the mapped
/// `/mnt/destination/data`; path mapping on the Worker Host does not
/// re-check constraints (§4.5 PATH paragraph).
const PATHS: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: Mounts
  envParameterDefinitions:
  - {name: Dir, type: PATH, maxLength: 13, default: /mnt/src/data}
  - {name: Dirs, type: "LIST[PATH]", default: [/mnt/src/a, /other/b]}
  script:
    actions:
      onEnter:
        command: sh
        args:
        - "-c"
        - "echo dir={{Env.Param.Dir}} raw={{Env.RawParam.Dir}} dirs={{ Env.Param.Dirs[0] }},{{ Env.Param.Dirs[1] }} rawdirs={{ Env.RawParam.Dirs[0] }},{{ Env.RawParam.Dirs[1] }} n={{ len(Env.Param.Dirs) }}"
steps:
- name: S
  script: {actions: {onRun: {command: "true"}}}
"#;

#[tokio::test]
async fn path_values_are_mapped_in_env_param_and_raw_in_env_raw_param() {
    let sub = submit_ok(PATHS, &[]);
    assert_eq!(
        run_step(&sub, "S", vec![rule("/mnt/src", "/mnt/destination")]).await,
        ["dir=/mnt/destination/data raw=/mnt/src/data \
          dirs=/mnt/destination/a,/other/b rawdirs=/mnt/src/a,/other/b n=2"],
    );
}

#[tokio::test]
async fn path_values_are_unchanged_without_path_mapping_rules() {
    let sub = submit_ok(PATHS, &[]);
    assert_eq!(
        run_step(&sub, "S", vec![]).await,
        ["dir=/mnt/src/data raw=/mnt/src/data \
          dirs=/mnt/src/a,/other/b rawdirs=/mnt/src/a,/other/b n=2"],
    );
}

/// `Env.Param` of a PATH is a `path` and `Env.RawParam` a
/// `string`, and for a LIST[PATH] a `list[path]` and a `list[string]`
/// (§4.5 last paragraph). `upper()` exists only for a string, and `/` only
/// for a path, so each type is pinned.
#[tokio::test]
async fn env_param_of_a_path_is_a_path_and_env_raw_param_a_string() {
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: Mounts
  envParameterDefinitions:
  - {name: Dir, type: PATH, default: /mnt/src/data}
  - {name: Dirs, type: "LIST[PATH]", default: [/mnt/src/a]}
  script:
    actions:
      onEnter:
        command: sh
        args: ["-c", "echo {{ Env.Param.Dir / 'x' }} {{ Env.RawParam.Dir.upper() }} {{ Env.RawParam.Dirs[0].upper() }} {{ Env.Param.Dirs[0] / 'z' }}"]
steps:
- name: S
  script: {actions: {onRun: {command: "true"}}}
"#;
    let sub = submit_ok(jt, &[]);
    assert_eq!(
        run_step(&sub, "S", vec![rule("/mnt/src", "/dst")]).await,
        ["/dst/data/x /MNT/SRC/DATA /MNT/SRC/A /dst/a/z"],
    );
}

// ────────────────────────────────────────────────────────────────────
// External Environments: matched by name; absent optional and required
// ────────────────────────────────────────────────────────────────────

const CONDA_ENV: &str = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: Conda
  envParameterDefinitions:
  - {name: Packages, type: STRING, default: "python=3.11"}
  - {name: Channel, type: STRING, default: conda-forge}
  script:
    actions:
      onEnter:
        command: sh
        args: ["-c", "echo conda packages={{Env.Param.Packages}} channel={{Env.Param.Channel}}"]
"#;

const USES_CONDA: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: Solve
  dependencies:
  - environment: Conda
    required: false
    parameters: {Packages: "colmap=3.11"}
  script: {actions: {onRun: {command: sh, args: ["-c", "echo solve"]}}}
- name: Plain
  script: {actions: {onRun: {command: sh, args: ["-c", "echo plain"]}}}
"#;

#[tokio::test]
async fn an_attached_environment_takes_the_value_its_named_entry_supplies() {
    let sub = submit_ok(USES_CONDA, &[CONDA_ENV]);
    assert_eq!(
        sub.documents,
        [Document::environment_template(0, None)],
        "the combined Job holds the attachment"
    );
    assert_eq!(
        run_step(&sub, "Solve", vec![]).await,
        ["conda packages=colmap=3.11 channel=conda-forge", "solve"],
    );
}

#[tokio::test]
async fn an_attached_environment_takes_its_defaults_for_a_step_that_names_no_entry() {
    let sub = submit_ok(USES_CONDA, &[CONDA_ENV]);
    assert_eq!(
        run_step(&sub, "Plain", vec![]).await,
        ["conda packages=python=3.11 channel=conda-forge", "plain"],
    );
}

/// `required: false` with no such Environment attached: the entry is
/// ignored, the combined Job has no Environment to enter, and the Step
/// runs exactly as without the entry (§3.2 item 3).
#[tokio::test]
async fn an_absent_optional_environment_is_silently_satisfied() {
    let sub = submit_ok(USES_CONDA, &[]);
    assert!(sub.job.job_environments.is_none());
    assert_eq!(step(&sub.job, "Solve").environment_parameters, None);
    assert_eq!(run_step(&sub, "Solve", vec![]).await, ["solve"]);
}

/// `required: true` (here by omission) with no such Environment attached
/// is a submission error, so no Session is ever started (§3.2.1 item 2).
#[test]
fn an_absent_required_environment_rejects_the_job_before_any_session() {
    let jt = USES_CONDA.replace("    required: false\n", "");
    let err = submit(&jt, &[])
        .err()
        .expect("expected the submission to fail");
    assert_eq!(
        err,
        "Model validation error: 1 validation error for Submission\n\
         JobTemplate -> steps[0] -> dependencies[0] -> environment:\n\
         \tnames Environment 'Conda', which is not a jobEnvironments entry of the Job \
         Template, and no Environment Template is attached. The entry is required; set \
         'required: false' if the Step can run without the Environment (Template \
         Schemas §3.2.1 item 2)."
    );
}

// ────────────────────────────────────────────────────────────────────
// Scoping: each Environment sees only its own values
// ────────────────────────────────────────────────────────────────────

/// Two Environments that declare a parameter of the same name keep
/// separate values, each bound only in its own scope.
#[tokio::test]
async fn same_named_parameters_of_two_environments_stay_separate() {
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: A
  envParameterDefinitions: [{name: Tag, type: STRING, default: a-default}]
  script: {actions: {onEnter: {command: sh, args: ["-c", "echo A={{Env.Param.Tag}}"]}}}
- name: B
  envParameterDefinitions: [{name: Tag, type: STRING, default: b-default}]
  script: {actions: {onEnter: {command: sh, args: ["-c", "echo B={{Env.Param.Tag}}"]}}}
steps:
- name: S
  dependencies:
  - environment: B
    parameters: {Tag: b-step}
  script: {actions: {onRun: {command: "true"}}}
"#;
    let sub = submit_ok(jt, &[]);
    assert_eq!(
        run_step(&sub, "S", vec![]).await,
        ["A=a-default", "B=b-step"]
    );
}

fn fs(s: &str) -> FormatString {
    FormatString::new(s).unwrap()
}

fn echo(text: &str) -> Action {
    Action {
        command: fs("sh"),
        args: Some(vec![fs("-c"), fs(&format!("echo {text}"))]),
        timeout: None,
        cancelation: None,
    }
}

fn env(name: &str, actions: EnvironmentActions) -> Environment {
    Environment {
        name: name.to_string(),
        description: None,
        script: Some(EnvironmentScript {
            let_bindings: None,
            actions,
            embedded_files: None,
        }),
        variables: None,
        resolved_symtab: None,
    }
}

fn on_enter(action: Action) -> EnvironmentActions {
    EnvironmentActions {
        on_enter: Some(action),
        on_wrap_env_enter: None,
        on_wrap_task_run: None,
        on_wrap_env_exit: None,
        on_exit: None,
    }
}

/// Hand-built values: one STRING parameter `name` with value `value`.
fn string_values(environment: &str, name: &str, value: &str) -> EnvironmentParameterValues {
    EnvironmentParameterValues {
        document: Document::JobTemplate,
        environment: environment.to_string(),
        values: SerializedSymbolTable::from_value(serde_json::json!([
            {"name": name, "type": "string", "value": value}
        ])),
    }
}

fn format_error(err: SessionError) -> String {
    match err {
        SessionError::FormatString { context, reason } => format!("{context}: {reason}"),
        other => panic!("expected a FormatString error, got {other:?}"),
    }
}

/// Template validation rejects these references, so the runtime checks
/// below use hand-built Environments: a Task, and a later Environment,
/// cannot read an entered Environment's `Env.Param.*` (§4 `Env.*` scope).
#[tokio::test]
async fn neither_a_task_nor_another_environment_sees_an_environments_values() {
    let tmp = TempDir::new().unwrap();
    let mut session = Session::new_for_test(tmp.path().to_path_buf());
    let a = env("A", on_enter(echo("A={{Env.Param.Tag}}")));
    let values = string_values("A", "Tag", "a");
    let (_, out) = session
        .enter_environment_with_parameters(&a, None, Some(&values), None, None)
        .await
        .unwrap();
    assert_eq!(out.trim(), "A=a");

    let task = StepScript {
        let_bindings: None,
        actions: StepActions {
            on_run: echo("{{Env.Param.Tag}}"),
        },
        embedded_files: None,
    };
    let err = session
        .run_task("S", &task, None, None, None)
        .await
        .expect_err("a Task must not see Env.Param.*");
    assert_eq!(
        format_error(err),
        "argument: Undefined variable: 'Env.Param.Tag'.\n  Env.Param.Tag\n  ~~~~~~~~~~^~~"
    );
    session.set_state_for_test(SessionState::Ready);

    let b = env("B", on_enter(echo("{{Env.RawParam.Tag}}")));
    let err = session
        .enter_environment_with_parameters(&b, None, None, None, None)
        .await
        .expect_err("another Environment must not see A's Env.RawParam.*");
    assert_eq!(
        format_error(err),
        "argument: Undefined variable: 'Env.RawParam.Tag'.\n  Env.RawParam.Tag\n  ~~~~~~~~~~~~~^~~"
    );
}

// ────────────────────────────────────────────────────────────────────
// RFC 0008 wrap hooks
// ────────────────────────────────────────────────────────────────────

/// A wrapping Environment's hooks bind its own stored values, while the
/// wrapped action itself (`WrappedAction.*`) resolves in the wrapped
/// Environment's scope with that Environment's values.
#[tokio::test]
async fn wrap_hooks_see_the_wrapping_environments_own_values() {
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR, WRAP_ACTIONS]
name: J
jobEnvironments:
- name: Wrap
  envParameterDefinitions: [{name: Mode, type: STRING, default: wrap-default}]
  script:
    actions:
      onEnter: {command: sh, args: ["-c", "echo wrap-enter {{Env.Param.Mode}}"]}
      onWrapEnvEnter:
        command: sh
        args: ["-c", "echo hook-enter {{Env.Param.Mode}}; {{ WrappedAction.Command }} \"$@\"", "sh", "{{ WrappedAction.Args }}"]
      onWrapTaskRun:
        command: sh
        args: ["-c", "echo hook-task {{Env.RawParam.Mode}}; {{ WrappedAction.Command }} \"$@\"", "sh", "{{ WrappedAction.Args }}"]
      onWrapEnvExit:
        command: sh
        args: ["-c", "echo hook-exit {{Env.Param.Mode}}; {{ WrappedAction.Command }} \"$@\"", "sh", "{{ WrappedAction.Args }}"]
- name: Inner
  envParameterDefinitions: [{name: Tag, type: STRING, default: inner-default}]
  script:
    actions:
      onEnter: {command: sh, args: ["-c", "echo inner-enter {{Env.Param.Tag}}"]}
      onExit: {command: sh, args: ["-c", "echo inner-exit {{Env.Param.Tag}}"]}
steps:
- name: S
  dependencies:
  - environment: Wrap
    parameters: {Mode: wrap-step}
  - environment: Inner
    parameters: {Tag: inner-step}
  script: {actions: {onRun: {command: sh, args: ["-c", "echo task"]}}}
"#;
    let sub = submit_ok(jt, &[]);
    assert_eq!(
        run_step(&sub, "S", vec![]).await,
        [
            "wrap-enter wrap-step",
            "hook-enter wrap-step",
            "inner-enter inner-step",
            "hook-task wrap-step",
            "task",
            "hook-exit wrap-step",
            "inner-exit inner-step",
        ]
    );
}

fn wrap_env_enter_hook(action: Action) -> EnvironmentActions {
    EnvironmentActions {
        on_enter: None,
        on_wrap_env_enter: Some(action),
        on_wrap_task_run: None,
        on_wrap_env_exit: None,
        on_exit: None,
    }
}

/// A wrap hook does not see the wrapped Environment's values, even when it
/// names them (template validation rejects that; this pins the runtime).
#[tokio::test]
async fn a_wrap_hook_does_not_see_the_wrapped_environments_values() {
    let tmp = TempDir::new().unwrap();
    let mut session = Session::new_for_test(tmp.path().to_path_buf());
    let wrap = env("Wrap", wrap_env_enter_hook(echo("hook {{Env.Param.Tag}}")));
    session
        .enter_environment_with_parameters(&wrap, None, None, None, None)
        .await
        .unwrap();
    let inner = env("Inner", on_enter(echo("inner {{Env.Param.Tag}}")));
    let values = string_values("Inner", "Tag", "t");
    let err = session
        .enter_environment_with_parameters(&inner, None, Some(&values), None, None)
        .await
        .expect_err("the hook must not see Inner's Env.Param.Tag");
    assert_eq!(
        format_error(err),
        "argument: Undefined variable: 'Env.Param.Tag'.\n  Env.Param.Tag\n  ~~~~~~~~~~^~~"
    );
}

// ────────────────────────────────────────────────────────────────────
// Entering without values, and invalid values
// ────────────────────────────────────────────────────────────────────

/// `enter_environment` (no values) and `enter_environment_with_parameters`
/// with `None` behave alike, and neither binds `Env.Param.*`.
#[tokio::test]
async fn entering_without_values_binds_no_env_param_symbols() {
    let tmp = TempDir::new().unwrap();
    let mut session = Session::new_for_test(tmp.path().to_path_buf());
    let plain = env("Plain", on_enter(echo("plain")));
    let id = session
        .enter_environment(&plain, None, None, None)
        .await
        .unwrap();
    assert_eq!(session.environment_parameters(&id), None);
    let (id2, out) = session
        .enter_environment_with_parameters(&plain, None, None, None, None)
        .await
        .unwrap();
    assert_eq!(out.trim(), "plain");
    assert_eq!(session.environment_parameters(&id2), None);

    let uses = env("Uses", on_enter(echo("{{Env.Param.X}}")));
    let err = session
        .enter_environment(&uses, None, None, None)
        .await
        .expect_err("Env.Param.X is unbound without values");
    assert_eq!(
        format_error(err),
        "argument: Undefined variable: 'Env.Param.X'.\n  Env.Param.X\n  ~~~~~~~~~~^"
    );
}

async fn enter_err(values: EnvironmentParameterValues) -> String {
    let tmp = TempDir::new().unwrap();
    let mut session = Session::new_for_test(tmp.path().to_path_buf());
    let e = env("E", on_enter(echo("x")));
    let err = session
        .enter_environment_with_parameters(&e, None, Some(&values), Some("e"), None)
        .await
        .expect_err("expected the values to be rejected");
    // Rejected before any state is recorded.
    assert_eq!(session.state(), SessionState::Ready);
    assert!(session.environments_entered().is_empty());
    assert_eq!(session.environment_parameters("e"), None);
    assert!(
        matches!(err, SessionError::InvalidEnvironmentParameters { .. }),
        "unexpected error {err:?}"
    );
    err.to_string()
}

#[tokio::test]
async fn values_for_another_environment_are_rejected() {
    assert_eq!(
        enter_err(string_values("Other", "Tag", "t")).await,
        "Environment 'E': invalid Environment parameter values: \
         the values are for Environment 'Other'."
    );
}

#[tokio::test]
async fn a_dotted_parameter_name_is_rejected() {
    assert_eq!(
        enter_err(string_values("E", "A.B", "t")).await,
        "Environment 'E': invalid Environment parameter values: \
         'A.B' is not a parameter name; each entry must be keyed by the bare name."
    );
}

#[tokio::test]
async fn a_repeated_parameter_name_is_rejected() {
    let values = EnvironmentParameterValues {
        document: Document::JobTemplate,
        environment: "E".to_string(),
        values: SerializedSymbolTable::from_value(serde_json::json!([
            {"name": "T", "type": "string", "value": "a"},
            {"name": "T", "type": "string", "value": "b"},
        ])),
    };
    assert_eq!(
        enter_err(values).await,
        "Environment 'E': invalid Environment parameter values: \
         parameter 'T' appears more than once."
    );
}

#[tokio::test]
async fn a_value_that_does_not_decode_is_rejected() {
    let values = EnvironmentParameterValues {
        document: Document::JobTemplate,
        environment: "E".to_string(),
        values: SerializedSymbolTable::from_value(serde_json::json!([
            {"name": "N", "type": "int", "value": "x"},
        ])),
    };
    assert_eq!(
        enter_err(values).await,
        "Environment 'E': invalid Environment parameter values: \
         parameter 'N': Cannot convert 'x' to int: invalid digit found in string."
    );
}

/// A PATH value is stored in the submission path format (plan A22), so a
/// WINDOWS-submitted value reaches a POSIX worker as written: mapped by a
/// WINDOWS-source rule in `Env.Param`, unchanged in `Env.RawParam`.
#[tokio::test]
async fn a_windows_submitted_path_is_mapped_by_a_windows_rule() {
    let tmp = TempDir::new().unwrap();
    let mut session =
        Session::new_for_test(tmp.path().to_path_buf()).with_path_mapping(vec![PathMappingRule {
            source_path_format: PathFormat::Windows,
            source_path: r"C:\src".to_string(),
            destination_path: "/mnt/src".to_string(),
        }]);
    let e = env(
        "E",
        on_enter(echo("'{{Env.Param.Dir}}' '{{Env.RawParam.Dir}}'")),
    );
    let values = EnvironmentParameterValues {
        document: Document::JobTemplate,
        environment: "E".to_string(),
        values: SerializedSymbolTable::from_value(serde_json::json!([
            {"name": "Dir", "type": "path", "value": r"C:\src\data"},
        ])),
    };
    let (_, out) = session
        .enter_environment_with_parameters(&e, None, Some(&values), None, None)
        .await
        .unwrap();
    assert_eq!(out.trim(), r"/mnt/src/data C:\src\data");
}

#[tokio::test]
async fn a_path_value_that_is_not_a_string_is_rejected() {
    let values = EnvironmentParameterValues {
        document: Document::JobTemplate,
        environment: "E".to_string(),
        values: SerializedSymbolTable::from_value(serde_json::json!([
            {"name": "D", "type": "path", "value": 7},
        ])),
    };
    assert_eq!(
        enter_err(values).await,
        "Environment 'E': invalid Environment parameter values: \
         parameter 'D': a path value must be a string."
    );
}

#[tokio::test]
async fn a_list_path_value_that_is_not_a_list_of_strings_is_rejected() {
    let values = EnvironmentParameterValues {
        document: Document::JobTemplate,
        environment: "E".to_string(),
        values: SerializedSymbolTable::from_value(serde_json::json!([
            {"name": "D", "type": "list[path]", "value": ["/a", 7]},
        ])),
    };
    assert_eq!(
        enter_err(values).await,
        "Environment 'E': invalid Environment parameter values: \
         parameter 'D': a list[path] value must be a list of strings."
    );
}

#[tokio::test]
async fn an_unknown_type_is_rejected() {
    let values = EnvironmentParameterValues {
        document: Document::JobTemplate,
        environment: "E".to_string(),
        values: SerializedSymbolTable::from_value(serde_json::json!([
            {"name": "D", "type": "PATH", "value": "/a"},
        ])),
    };
    assert_eq!(
        enter_err(values).await,
        "Environment 'E': invalid Environment parameter values: \
         parameter 'D': type 'PATH' is not an expression type (Unknown type string: PATH)."
    );
}

#[tokio::test]
async fn an_entry_without_a_name_is_rejected() {
    let values = EnvironmentParameterValues {
        document: Document::JobTemplate,
        environment: "E".to_string(),
        values: SerializedSymbolTable::from_value(serde_json::json!([
            {"type": "string", "value": "a"},
        ])),
    };
    assert_eq!(
        enter_err(values).await,
        "Environment 'E': invalid Environment parameter values: \
         entry 0 has no string 'name'."
    );
    let values = EnvironmentParameterValues {
        document: Document::JobTemplate,
        environment: "E".to_string(),
        values: SerializedSymbolTable::from_value(serde_json::json!([
            {"name": "A", "type": "string", "value": "a"},
            {"name": 5, "type": "string", "value": "a"},
        ])),
    };
    assert_eq!(
        enter_err(values).await,
        "Environment 'E': invalid Environment parameter values: \
         entry 1 has no string 'name'."
    );
}

#[tokio::test]
async fn a_table_that_is_not_an_array_is_rejected() {
    let values = EnvironmentParameterValues {
        document: Document::JobTemplate,
        environment: "E".to_string(),
        values: SerializedSymbolTable::from_value(serde_json::json!({"D": "a"})),
    };
    assert_eq!(
        enter_err(values).await,
        "Environment 'E': invalid Environment parameter values: \
         the values table is not a JSON array of entries."
    );
}

fn wrap_hooks(task: Option<Action>, exit: Option<Action>) -> EnvironmentActions {
    EnvironmentActions {
        on_enter: None,
        on_wrap_env_enter: None,
        on_wrap_task_run: task,
        on_wrap_env_exit: exit,
        on_exit: None,
    }
}

/// The onWrapTaskRun and onWrapEnvExit hooks, like onWrapEnvEnter, do not
/// see the wrapped Environment's values.
#[tokio::test]
async fn the_task_and_exit_wrap_hooks_do_not_see_the_wrapped_environments_values() {
    let tmp = TempDir::new().unwrap();
    let mut session = Session::new_for_test(tmp.path().to_path_buf());
    let wrap = env(
        "Wrap",
        wrap_hooks(
            Some(echo("task-hook {{Env.Param.Tag}}")),
            Some(echo("exit-hook {{Env.Param.Tag}}")),
        ),
    );
    session
        .enter_environment_with_parameters(&wrap, None, None, None, None)
        .await
        .unwrap();
    let mut inner = env("Inner", on_enter(echo("inner {{Env.Param.Tag}}")));
    inner.script.as_mut().unwrap().actions.on_exit = Some(echo("inner-exit {{Env.Param.Tag}}"));
    let values = string_values("Inner", "Tag", "t");
    let (inner_id, out) = session
        .enter_environment_with_parameters(&inner, None, Some(&values), None, None)
        .await
        .unwrap();
    assert_eq!(out.trim(), "inner t");

    let task = StepScript {
        let_bindings: None,
        actions: StepActions {
            on_run: echo("task"),
        },
        embedded_files: None,
    };
    let err = session
        .run_task("S", &task, None, None, None)
        .await
        .expect_err("onWrapTaskRun must not see Inner's Env.Param.Tag");
    assert_eq!(
        format_error(err),
        "argument: Undefined variable: 'Env.Param.Tag'.\n  Env.Param.Tag\n  ~~~~~~~~~~^~~"
    );
    session.set_state_for_test(SessionState::Ready);

    let err = session
        .exit_environment(&inner_id, None, true, None)
        .await
        .expect_err("onWrapEnvExit must not see Inner's Env.Param.Tag");
    assert_eq!(
        format_error(err),
        "argument: Undefined variable: 'Env.Param.Tag'.\n  Env.Param.Tag\n  ~~~~~~~~~~^~~"
    );
}

/// Path mapping is applied at each use with the Session's current rules,
/// as for `Param.*`: rules added after entry map the onExit's value, and
/// `Env.RawParam` is unchanged.
#[tokio::test]
async fn path_mapping_uses_the_rules_current_at_each_use() {
    let tmp = TempDir::new().unwrap();
    let mut session = Session::new_for_test(tmp.path().to_path_buf());
    let mut e = env(
        "E",
        on_enter(echo("enter {{Env.Param.D}} {{Env.RawParam.D}}")),
    );
    e.script.as_mut().unwrap().actions.on_exit =
        Some(echo("exit {{Env.Param.D}} {{Env.RawParam.D}}"));
    let values = EnvironmentParameterValues {
        document: Document::JobTemplate,
        environment: "E".to_string(),
        values: SerializedSymbolTable::from_value(serde_json::json!([
            {"name": "D", "type": "path", "value": "/src/x"},
        ])),
    };
    let (id, out) = session
        .enter_environment_with_parameters(&e, None, Some(&values), None, None)
        .await
        .unwrap();
    assert_eq!(out.trim(), "enter /src/x /src/x");
    session.extend_path_mapping_rules(vec![rule("/src", "/dst")]);
    let out = session
        .exit_environment(&id, None, false, None)
        .await
        .unwrap();
    assert_eq!(out.trim(), "exit /dst/x /src/x");
}

/// A mapped PATH reaches a wrap hook and the `variables` of its own
/// Environment mapped, and a FLOAT renders as written (plan A1).
#[tokio::test]
async fn mapped_paths_reach_wrap_hooks_and_variables_and_floats_keep_their_text() {
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR, WRAP_ACTIONS]
name: J
jobEnvironments:
- name: Wrap
  envParameterDefinitions:
  - {name: Root, type: PATH, default: /mnt/src/root}
  - {name: Scale, type: FLOAT, default: "1.50"}
  variables:
    WRAP_ROOT: "{{Env.Param.Root}}|{{Env.RawParam.Root}}"
  script:
    actions:
      onEnter: {command: sh, args: ["-c", "echo enter $WRAP_ROOT {{Env.Param.Scale}}"]}
      onWrapEnvEnter:
        command: sh
        args: ["-c", "{{ WrappedAction.Command }} \"$@\"", "sh", "{{ WrappedAction.Args }}"]
      onWrapTaskRun:
        command: sh
        args: ["-c", "echo hook {{Env.Param.Root}} {{Env.RawParam.Root}} {{Env.Param.Scale}}; {{ WrappedAction.Command }} \"$@\"", "sh", "{{ WrappedAction.Args }}"]
      onWrapEnvExit:
        command: sh
        args: ["-c", "{{ WrappedAction.Command }} \"$@\"", "sh", "{{ WrappedAction.Args }}"]
steps:
- name: S
  script: {actions: {onRun: {command: sh, args: ["-c", "echo task"]}}}
"#;
    let sub = submit_ok(jt, &[]);
    assert_eq!(
        run_step(&sub, "S", vec![rule("/mnt/src", "/dst")]).await,
        [
            "enter /dst/root|/mnt/src/root 1.50",
            "hook /dst/root /mnt/src/root 1.50",
            "task",
        ]
    );
}

// ────────────────────────────────────────────────────────────────────
// Invalid values (continued), and wrapper scoping
// ────────────────────────────────────────────────────────────────────

fn values(environment: &str, table: serde_json::Value) -> EnvironmentParameterValues {
    EnvironmentParameterValues {
        document: Document::JobTemplate,
        environment: environment.to_string(),
        values: SerializedSymbolTable::from_value(table),
    }
}

const INVALID: &str = "Environment 'E': invalid Environment parameter values: ";

#[tokio::test]
async fn a_name_that_is_not_an_identifier_is_rejected() {
    // ASCII only, as the model's `<Identifier>` rule (Template Schemas §7.1).
    for name in ["1A", "a-b", "ab-c", "a_b-", "", "aé", "éa"] {
        assert_eq!(
            enter_err(string_values("E", name, "t")).await,
            format!(
                "{INVALID}'{name}' is not a parameter name; a name is a letter or '_' followed by \
                 letters, digits, or '_'."
            )
        );
    }
}

#[tokio::test]
async fn names_with_underscores_and_digits_are_accepted() {
    for name in ["_x", "a_b", "A1"] {
        let tmp = TempDir::new().unwrap();
        let mut session = Session::new_for_test(tmp.path().to_path_buf());
        let e = env("E", on_enter(echo(&format!("{{{{Env.Param.{name}}}}}"))));
        let (_, out) = session
            .enter_environment_with_parameters(
                &e,
                None,
                Some(&string_values("E", name, "u")),
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(out.trim(), "u", "name {name}");
    }
}

#[tokio::test]
async fn an_entry_without_a_string_type_or_a_value_is_rejected() {
    assert_eq!(
        enter_err(values(
            "E",
            serde_json::json!([{"name": "T", "value": "a"}])
        ))
        .await,
        format!("{INVALID}parameter 'T' has no string 'type'.")
    );
    assert_eq!(
        enter_err(values(
            "E",
            serde_json::json!([{"name": "T", "type": 7, "value": "a"}])
        ))
        .await,
        format!("{INVALID}parameter 'T' has no string 'type'.")
    );
    assert_eq!(
        enter_err(values(
            "E",
            serde_json::json!([{"name": "T", "type": "string"}])
        ))
        .await,
        format!("{INVALID}parameter 'T' has no 'value'.")
    );
}

#[tokio::test]
async fn a_table_over_the_entry_cap_is_rejected() {
    let max = openjd_expr::MAX_SYMBOL_TABLE_ENTRIES;
    let table = |n: usize| {
        serde_json::Value::Array(
            (0..n)
                .map(|i| serde_json::json!({"name": format!("p{i}"), "type": "string", "value": "v"}))
                .collect(),
        )
    };
    assert_eq!(
        enter_err(values("E", table(max + 1))).await,
        format!(
            "{INVALID}the values table has {} entries; the maximum is {max}.",
            max + 1
        )
    );
    // Exactly at the cap is accepted.
    let tmp = TempDir::new().unwrap();
    let mut session = Session::new_for_test(tmp.path().to_path_buf());
    let e = env("E", on_enter(echo("{{Env.Param.p0}}")));
    let (_, out) = session
        .enter_environment_with_parameters(&e, None, Some(&values("E", table(max))), None, None)
        .await
        .unwrap();
    assert_eq!(out.trim(), "v");
}

/// A wrapping Environment's script `let` bindings read its own values
/// when they are evaluated for its hooks.
#[tokio::test]
async fn a_wrap_environments_let_bindings_read_its_values_in_its_hooks() {
    let tmp = TempDir::new().unwrap();
    let mut session = Session::new_for_test(tmp.path().to_path_buf());
    let mut wrap = env("W", wrap_env_enter_hook(echo("hook {{m}}")));
    wrap.script.as_mut().unwrap().let_bindings = Some(vec!["m = Env.Param.Mode".to_string()]);
    session
        .enter_environment_with_parameters(
            &wrap,
            None,
            Some(&string_values("W", "Mode", "w")),
            None,
            None,
        )
        .await
        .unwrap();
    let inner = env("Inner", on_enter(echo("inner")));
    let (_, out) = session
        .enter_environment_with_parameters(&inner, None, None, None, None)
        .await
        .unwrap();
    assert_eq!(out.trim(), "hook w");
}

fn actions(
    on_enter: Option<Action>,
    on_wrap_env_enter: Option<Action>,
    on_wrap_task_run: Option<Action>,
    on_wrap_env_exit: Option<Action>,
    on_exit: Option<Action>,
) -> EnvironmentActions {
    EnvironmentActions {
        on_enter,
        on_wrap_env_enter,
        on_wrap_task_run,
        on_wrap_env_exit,
        on_exit,
    }
}

/// Enter a wrapping Environment `W` with `Mode=w` and the given hooks.
async fn enter_wrapper(session: &mut Session, hooks: EnvironmentActions) {
    session
        .enter_environment_with_parameters(
            &env("W", hooks),
            None,
            Some(&string_values("W", "Mode", "w")),
            None,
            None,
        )
        .await
        .unwrap();
}

fn task(text: &str) -> StepScript {
    StepScript {
        let_bindings: None,
        actions: StepActions { on_run: echo(text) },
        embedded_files: None,
    }
}

const MODE_UNDEFINED: &str =
    "Undefined variable: 'Env.Param.Mode'.\n  Env.Param.Mode\n  ~~~~~~~~~~^~~~";
const MODE_UNDEFINED_TAG: &str = "Undefined variable: 'Env.Param.Mode'. Did you mean: \
                                  Env.Param.Tag\n  Env.Param.Mode\n  ~~~~~~~~~~^~~~";

/// While a wrapping Environment with values is entered, a Task's onRun,
/// whether wrapped or not, does not see the wrapper's values.
#[tokio::test]
async fn a_task_does_not_see_an_active_wrappers_values() {
    for (hooks, prefix) in [
        (
            actions(
                None,
                None,
                Some(echo("hook {{Env.Param.Mode}}")),
                None,
                None,
            ),
            "Failed to resolve wrapped task command: Failed to resolve argument: ",
        ),
        (
            actions(None, Some(echo("hook")), None, None, None),
            "Failed to resolve argument: ",
        ),
    ] {
        let tmp = TempDir::new().unwrap();
        let mut session = Session::new_for_test(tmp.path().to_path_buf());
        enter_wrapper(&mut session, hooks).await;
        let err = session
            .run_task("S", &task("{{Env.Param.Mode}}"), None, None, None)
            .await
            .expect_err("a Task must not see W's Env.Param.Mode");
        assert_eq!(err.to_string(), format!("{prefix}{MODE_UNDEFINED}"));
    }
}

/// While a wrapping Environment with values is entered, an inner
/// Environment's own onEnter, onExit, and `variables`, wrapped or not, do
/// not see the wrapper's values.
#[tokio::test]
async fn an_inner_environment_does_not_see_an_active_wrappers_values() {
    let inner_values = string_values("Inner", "Tag", "t");
    let reads_mode = echo("{{Env.Param.Mode}}");

    // onEnter, wrapped by onWrapEnvEnter and unwrapped.
    for (hooks, prefix) in [
        (
            actions(
                None,
                Some(echo("hook {{Env.Param.Mode}}")),
                None,
                None,
                None,
            ),
            "Failed to resolve wrapped onEnter command: Failed to resolve argument: ",
        ),
        (
            actions(None, None, Some(echo("hook")), None, None),
            "Failed to resolve argument: ",
        ),
    ] {
        let tmp = TempDir::new().unwrap();
        let mut session = Session::new_for_test(tmp.path().to_path_buf());
        enter_wrapper(&mut session, hooks).await;
        let inner = env("Inner", on_enter(reads_mode.clone()));
        let err = session
            .enter_environment_with_parameters(&inner, None, Some(&inner_values), None, None)
            .await
            .expect_err("an inner onEnter must not see W's values");
        assert_eq!(err.to_string(), format!("{prefix}{MODE_UNDEFINED_TAG}"));
    }

    // onExit, wrapped by onWrapEnvExit and unwrapped.
    for (hooks, prefix) in [
        (
            actions(
                None,
                None,
                None,
                Some(echo("hook {{Env.Param.Mode}}")),
                None,
            ),
            "Failed to resolve wrapped onExit command: Failed to resolve argument: ",
        ),
        (
            actions(None, None, Some(echo("hook")), None, None),
            "Failed to resolve argument: ",
        ),
    ] {
        let tmp = TempDir::new().unwrap();
        let mut session = Session::new_for_test(tmp.path().to_path_buf());
        enter_wrapper(&mut session, hooks).await;
        let inner = env(
            "Inner",
            actions(Some(echo("in")), None, None, None, Some(reads_mode.clone())),
        );
        let (id, _) = session
            .enter_environment_with_parameters(&inner, None, Some(&inner_values), None, None)
            .await
            .unwrap();
        let err = session
            .exit_environment(&id, None, false, None)
            .await
            .expect_err("an inner onExit must not see W's values");
        assert_eq!(err.to_string(), format!("{prefix}{MODE_UNDEFINED_TAG}"));
    }

    // variables.
    let tmp = TempDir::new().unwrap();
    let mut session = Session::new_for_test(tmp.path().to_path_buf());
    enter_wrapper(
        &mut session,
        actions(None, None, Some(echo("hook")), None, None),
    )
    .await;
    let mut inner = env("Inner", on_enter(echo("in")));
    inner.variables = Some(std::collections::HashMap::from([(
        "X".to_string(),
        fs("{{Env.Param.Mode}}"),
    )]));
    let err = session
        .enter_environment_with_parameters(&inner, None, Some(&inner_values), None, None)
        .await
        .expect_err("inner variables must not see W's values");
    assert_eq!(
        err.to_string(),
        format!("Failed to resolve env var 'X': {MODE_UNDEFINED_TAG}")
    );
}

/// A failed onEnter leaves the Environment entered (as before ENV_PARAMS),
/// with its values stored, and its onExit binds them.
#[tokio::test]
async fn after_a_failed_on_enter_the_on_exit_still_binds_the_values() {
    let tmp = TempDir::new().unwrap();
    let mut session = Session::new_for_test(tmp.path().to_path_buf());
    let e = env(
        "E",
        actions(
            Some(Action {
                command: fs("sh"),
                args: Some(vec![fs("-c"), fs("exit 3")]),
                timeout: None,
                cancelation: None,
            }),
            None,
            None,
            None,
            Some(echo("exit {{Env.Param.Tag}}")),
        ),
    );
    let values = string_values("E", "Tag", "t");
    let err = session
        .enter_environment_with_parameters(&e, None, Some(&values), Some("e"), None)
        .await
        .expect_err("onEnter fails");
    assert!(
        matches!(err, SessionError::EnvironmentScriptFailed { .. }),
        "unexpected error {err:?}"
    );
    assert_eq!(session.environment_parameters("e"), Some(&values));
    let out = session
        .exit_environment(&"e".to_string(), None, false, None)
        .await
        .unwrap();
    assert_eq!(out.trim(), "exit t");
    assert_eq!(session.environment_parameters("e"), None);
}

/// The wrap hooks bind the wrapping Environment's values even when another
/// Environment with values sits between it and the one being entered or
/// exited.
#[tokio::test]
async fn wrap_hooks_bind_the_wrappers_values_past_an_intermediate_environment() {
    let tmp = TempDir::new().unwrap();
    let mut session = Session::new_for_test(tmp.path().to_path_buf());
    let wrap = env(
        "W",
        actions(
            None,
            Some(echo("enter-hook {{Env.Param.Tag}}")),
            None,
            Some(echo("exit-hook {{Env.Param.Tag}}")),
            None,
        ),
    );
    session
        .enter_environment_with_parameters(
            &wrap,
            None,
            Some(&string_values("W", "Tag", "w")),
            None,
            None,
        )
        .await
        .unwrap();
    let a = env("A", on_enter(echo("a")));
    let (_, out) = session
        .enter_environment_with_parameters(
            &a,
            None,
            Some(&string_values("A", "Tag", "a")),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(out.trim(), "enter-hook w");
    let b = env(
        "B",
        actions(Some(echo("b")), None, None, None, Some(echo("b-exit"))),
    );
    let (b_id, out) = session
        .enter_environment_with_parameters(&b, None, None, None, None)
        .await
        .unwrap();
    assert_eq!(out.trim(), "enter-hook w");
    let out = session
        .exit_environment(&b_id, None, true, None)
        .await
        .unwrap();
    assert_eq!(out.trim(), "exit-hook w");
}

/// Stored values are dropped when the Environment is exited, even when its
/// onExit fails; a failure resolving `variables` after the Environment is
/// recorded leaves it entered, with its values, for the caller to exit.
#[tokio::test]
async fn stored_values_follow_the_environment_through_failures() {
    let tmp = TempDir::new().unwrap();
    let mut session = Session::new_for_test(tmp.path().to_path_buf());
    let fail = Action {
        command: fs("sh"),
        args: Some(vec![fs("-c"), fs("exit 2")]),
        timeout: None,
        cancelation: None,
    };
    let e = env("E", actions(Some(echo("in")), None, None, None, Some(fail)));
    let values = string_values("E", "Tag", "t");
    session
        .enter_environment_with_parameters(&e, None, Some(&values), Some("e"), None)
        .await
        .unwrap();
    session
        .exit_environment(&"e".to_string(), None, true, None)
        .await
        .expect_err("onExit fails");
    assert_eq!(session.environment_parameters("e"), None);
    assert!(session.environments_entered().is_empty());

    let tmp = TempDir::new().unwrap();
    let mut session = Session::new_for_test(tmp.path().to_path_buf());
    let mut v = env("V", on_enter(echo("in")));
    v.variables = Some(std::collections::HashMap::from([(
        "X".to_string(),
        fs("{{Env.Param.Missing}}"),
    )]));
    let values = string_values("V", "Tag", "t");
    let err = session
        .enter_environment_with_parameters(&v, None, Some(&values), Some("v"), None)
        .await
        .expect_err("variables fail");
    assert_eq!(
        err.to_string(),
        "Failed to resolve env var 'X': Undefined variable: 'Env.Param.Missing'.\n  \
         Env.Param.Missing\n  ~~~~~~~~~~^~~~~~~"
    );
    assert_eq!(session.state(), SessionState::Ready);
    assert_eq!(session.environments_entered(), ["v".to_string()]);
    assert_eq!(session.environment_parameters("v"), Some(&values));
}

/// Every element of a LIST[PATH] value is mapped in `Env.Param`, and every
/// element is left as given in `Env.RawParam`.
#[tokio::test]
async fn every_list_path_element_is_mapped() {
    let tmp = TempDir::new().unwrap();
    let mut session = Session::new_for_test(tmp.path().to_path_buf())
        .with_path_mapping(vec![rule("/mnt/src", "/dst")]);
    let e = env(
        "E",
        on_enter(echo(
            "{{Env.Param.Dirs[0]}} {{Env.Param.Dirs[1]}} {{Env.Param.Dirs[2]}} \
             {{Env.RawParam.Dirs[1]}} {{Env.RawParam.Dirs[2]}}",
        )),
    );
    let v = values(
        "E",
        serde_json::json!([{
            "name": "Dirs",
            "type": "list[path]",
            "value": ["/mnt/src/a", "/mnt/src/b", "/mnt/src/c"]
        }]),
    );
    let (_, out) = session
        .enter_environment_with_parameters(&e, None, Some(&v), None, None)
        .await
        .unwrap();
    assert_eq!(out.trim(), "/dst/a /dst/b /dst/c /mnt/src/b /mnt/src/c");
}

/// When an outer wrap hook replaces an Environment's onEnter or onExit,
/// that Environment's own `let` bindings and embedded files, which the
/// wrapped action uses, still read its own values.
#[tokio::test]
async fn a_wrapped_environments_let_and_embedded_files_read_its_values() {
    let tmp = TempDir::new().unwrap();
    let mut session = Session::new_for_test(tmp.path().to_path_buf());
    std::fs::create_dir_all(session.files_directory()).unwrap();
    let pass_through = Action {
        command: fs("{{ WrappedAction.Command }}"),
        args: Some(vec![fs("{{ WrappedAction.Args }}")]),
        timeout: None,
        cancelation: None,
    };
    let wrap = env(
        "W",
        actions(
            None,
            Some(pass_through.clone()),
            None,
            Some(pass_through),
            None,
        ),
    );
    session
        .enter_environment_with_parameters(&wrap, None, None, None, None)
        .await
        .unwrap();
    let mut inner = env(
        "Inner",
        actions(
            Some(echo("enter file=$(cat {{Env.File.F}}) let={{twice}}")),
            None,
            None,
            None,
            Some(echo("exit file=$(cat {{Env.File.F}}) let={{twice}}")),
        ),
    );
    let script = inner.script.as_mut().unwrap();
    script.let_bindings = Some(vec!["twice = Env.Param.N * 2".to_string()]);
    script.embedded_files = Some(vec![openjd_model::job::EmbeddedFile {
        name: "F".to_string(),
        file_type: openjd_model::types::FileType::Text,
        filename: None,
        runnable: None,
        data: Some(fs("{{Env.Param.N}}")),
        end_of_line: None,
    }]);
    let n = values(
        "Inner",
        serde_json::json!([{"name": "N", "type": "int", "value": "21"}]),
    );
    let (id, out) = session
        .enter_environment_with_parameters(&inner, None, Some(&n), None, None)
        .await
        .unwrap();
    assert_eq!(out.trim(), "enter file=21 let=42");
    let out = session
        .exit_environment(&id, None, true, None)
        .await
        .unwrap();
    assert_eq!(out.trim(), "exit file=21 let=42");
}
