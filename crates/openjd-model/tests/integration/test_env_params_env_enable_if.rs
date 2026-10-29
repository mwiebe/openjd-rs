// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `ENV_PARAMS`: `enableIf` on an `<Environment>` (Template Schemas §4
//! item 6), an expression field of type `bool` (§7.5).
//!
//! Template validation: gating (constraint 4), placement on every
//! position, the two scopes (constraints 1–3), the §7.5 form rules, and
//! the single-layer rule counting only Environments certainly entered.
//! Job creation (§7.4): resolution once per Step with its effective
//! values, and every effect of a disabled Environment (not entered, no
//! values, no host requirements, `Job.EnvNames` unchanged, no constraint
//! 7 and no value checks, the Environment-set comparison, the single-layer rule over the
//! final set), the `enableIf` evaluated with the values that are known
//! (§3.2.2), and `check_environment_templates` treating only certainly
//! entered Environments as entered.

use openjd_model::job::{Document, EnvironmentRef, Job};
use openjd_model::template::EnvironmentTemplate;
use openjd_model::{
    check_environment_templates, create_job_with_environment_templates,
    AttachedEnvironmentTemplate, CreatedJob, ModelError, StepDisabledEnvironments,
};

use crate::env_params_common::{
    assert_errors, env_err, env_ok, job_err, job_ok, job_params_with, posix_ctx, rendered_values,
};

const NOT_FIT_INT: &str = "the integer scalar 1 does not fit type bool: an expression field of \
                           type bool takes a boolean scalar or a string holding one expression \
                           (Template Schemas §7.5).";

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

/// The names of the Job Environments Step `step`'s Sessions enter.
fn entered(job: &Job, step: usize) -> Vec<String> {
    let docs = vec![Document::JobTemplate; job.job_environments.iter().flatten().count()];
    job.job_environments
        .iter()
        .flatten()
        .zip(&docs)
        .filter(|(e, d)| job.steps[step].enters_environment(d, &e.name))
        .map(|(e, _)| e.name.clone())
        .collect()
}

fn job_env_ref(name: &str) -> EnvironmentRef {
    EnvironmentRef {
        document: Document::JobTemplate,
        environment: name.to_string(),
    }
}

// ── Template validation: gating and placement (constraint 4) ────────

#[test]
fn needs_env_params_in_every_position() {
    let jt = |exts: &str| {
        format!(
            "specificationVersion: jobtemplate-2023-09\n{exts}name: J\n\
             jobEnvironments:\n- {{name: E, enableIf: true, variables: {{V: x}}}}\n\
             steps:\n- name: S\n  stepEnvironments:\n  - {{name: L, enableIf: false, variables: {{V: x}}}}\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    let gate = |path: &str| {
        format!(
            "{path} -> enableIf:\n\tenableIf on an Environment requires the ENV_PARAMS extension."
        )
    };
    let (je, se) = ("jobEnvironments[0]", "steps[0] -> stepEnvironments[0]");
    job_err(&jt("extensions: [EXPR]\n"), &[&gate(je), &gate(se)]);
    job_err(&jt(""), &[&gate(je), &gate(se)]);
    // With ENV_PARAMS (and so EXPR) it is permitted on a Job and a Step
    // Environment.
    job_ok(&jt("extensions: [ENV_PARAMS, EXPR]\n"));

    let et = |exts: &str| {
        format!(
            "specificationVersion: environment-2023-09\n{exts}\
             environment: {{name: E, enableIf: true, variables: {{V: x}}}}\n"
        )
    };
    env_err(&et("extensions: [EXPR]\n"), &[&gate("environment")]);
    env_err(&et(""), &[&gate("environment")]);
    env_ok(&et("extensions: [ENV_PARAMS, EXPR]\n"));
}

// ── Template validation: §7.5 forms ─────────────────────────────────

#[test]
fn expression_field_rules_apply_in_every_position() {
    job_err(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         parameterDefinitions: [{name: Mode, type: STRING, default: 'on'}]\n\
         jobEnvironments:\n\
         - {name: A, enableIf: \"{{ true }}\", variables: {V: x}}\n\
         - {name: B, enableIf: Param.Mode, variables: {V: x}}\n\
         - {name: C, enableIf: null, variables: {V: x}}\n\
         - {name: D, enableIf: 1, variables: {V: x}}\n\
         - {name: F, enableIf: \"None\", variables: {V: x}}\n\
         steps:\n- name: S\n  stepEnvironments:\n  - {name: L, enableIf: \"'x'\", variables: {V: x}}\n  \
         script: {actions: {onRun: {command: x}}}\n",
        &[
            "jobEnvironments[0] -> enableIf:\n\tInvalid expression: Set literals are not supported. \
             An expression field takes an expression without '{{ }}' (Template Schemas §7.5).\n  \
             {{ true }}\n  ^~~~~~~~~~",
            "jobEnvironments[1] -> enableIf:\n\thas type string, which does not fit type bool; an \
             expression field converts no type except int to float (Template Schemas §7.5).",
            "jobEnvironments[2] -> enableIf:\n\tmust not be null: an expression field of type bool \
             takes a boolean scalar or a string holding one expression (Template Schemas §7.5).",
            &format!("jobEnvironments[3] -> enableIf:\n\t{NOT_FIT_INT}"),
            "jobEnvironments[4] -> enableIf:\n\thas type nulltype, which does not fit type bool \
             (Template Schemas §7.5).",
            "steps[0] -> stepEnvironments[0] -> enableIf:\n\thas type string, which does not fit \
             type bool; an expression field converts no type except int to float (Template \
             Schemas §7.5).",
        ],
    );
    env_err(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
         environment: {name: E, enableIf: 1, variables: {V: x}}\n",
        &[&format!("environment -> enableIf:\n\t{NOT_FIT_INT}")],
    );
}

#[test]
fn a_disabled_environment_is_still_validated_in_full() {
    // `enableIf: false` does not exempt the Environment itself from any
    // check: it is validated in full as written. Only the checks of the
    // Steps' values for it, and constraint 7, are skipped (§3.2.2).
    job_err(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         jobEnvironments:\n- {name: E, enableIf: false, variables: {V: \"{{ Param.Nope }}\"}}\n\
         steps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n",
        &["jobEnvironments[0] -> variables -> V:\n\tFailed to parse interpolation expression at [0, \
           16]. Undefined variable: 'Param.Nope'."],
    );
}

// ── Template validation: scopes (constraints 1–3) ───────────────────

const JOB_SCOPE_HEAD: &str = "specificationVersion: jobtemplate-2023-09\n\
     extensions: [ENV_PARAMS, EXPR]\nname: J\n\
     parameterDefinitions:\n\
     - {name: N, type: INT, default: 1}\n\
     - {name: Dir, type: PATH, default: /d}\n\
     jobEnvironments:\n\
     - name: Other\n  envParameterDefinitions: [{name: O, type: STRING, default: o}]\n  variables: {V: x}\n\
     - name: E\n  envParameterDefinitions:\n  - {name: S, type: STRING, default: s}\n  \
     - {name: P, type: PATH, default: /p}\n  \
     script:\n    actions: {onEnter: {command: x}}\n    embeddedFiles: [{name: F, type: TEXT, data: d}]\n  \
     enableIf: ";
const JOB_SCOPE_TAIL: &str = "\nsteps:\n- name: S\n  let: [l = 1]\n  \
     parameterSpace: {taskParameterDefinitions: [{name: T, type: INT, range: [1]}]}\n  \
     script: {actions: {onRun: {command: x}}}\n";

fn job_env_enable_if(expr: &str) -> String {
    format!("{JOB_SCOPE_HEAD}\"{expr}\"{JOB_SCOPE_TAIL}")
}

#[test]
fn a_job_environment_reads_exactly_its_host_requirements_scope() {
    // Constraint 1: the Job Parameters' RawParam.* and non-PATH Param.*,
    // Job.Name, Job.EnvNames, and its own Env.RawParam.* of any type and
    // non-PATH Env.Param.*.
    // Each name on its own: the static check of `a and b` does not look at
    // `b` when `a` is unresolved, so a conjunction would test only `a`.
    for expr in [
        "Param.N == 1",
        "RawParam.Dir != ''",
        "Job.Name != ''",
        "'E' in Job.EnvNames",
        "Env.Param.S == 's'",
        "Env.RawParam.S == 's'",
        "Env.RawParam.P != ''",
    ] {
        job_ok(&job_env_enable_if(expr));
    }
    // Constraint 3: nothing else.
    for (expr, message) in [
        (
            "Param.Dir != ''",
            "Undefined variable: 'Param.Dir'. Did you mean one of: Param.N, RawParam.Dir",
        ),
        (
            "Env.Param.P != ''",
            "Undefined variable: 'Env.Param.P'. Did you mean: Env.Param.S",
        ),
        (
            "Env.Param.O != ''",
            "Undefined variable: 'Env.Param.O'. Did you mean: Env.Param.S",
        ),
        ("Env.File.F != ''", "Undefined variable: 'Env.File.F'."),
        (
            "Step.Name != ''",
            "Undefined variable: 'Step.Name'. Did you mean: Job.Name",
        ),
        ("l == 1", "Undefined variable: 'l'."),
        (
            "Task.Param.T == 1",
            "Undefined variable: 'Task.Param.T'. Did you mean one of: Env.Param.S, RawParam.N",
        ),
        (
            "Session.WorkingDirectory != ''",
            "Undefined variable: 'Session.WorkingDirectory'.",
        ),
    ] {
        job_err(
            &job_env_enable_if(expr),
            &[&format!(
                "jobEnvironments[1] -> enableIf:\n\tInvalid expression: {message}"
            )],
        );
    }
}

#[test]
fn an_environments_enable_if_and_host_requirements_read_env_raw_param_of_a_path() {
    // §4 item 5.1 and item 6 constraint 1: Env.RawParam.<PATH> is the
    // unmapped string, as RawParam.<PATH> of a Job Parameter is, so both
    // fields may read it. A disables E by supplying /none; B takes /data.
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              jobEnvironments:\n- name: E\n  activation: ON_REQUEST\n  \
              envParameterDefinitions: [{name: D, type: PATH, default: /data}]\n  \
              enableIf: \"Env.RawParam.D != '/none'\"\n  hostRequirements:\n    \
              attributes: [{name: attr.custom.dir, anyOf: [\"{{Env.RawParam.D.replace('/', '')}}\"]}]\n  \
              variables: {V: x}\n\
              steps:\n\
              - name: A\n  dependencies: [{environment: E, parameters: {D: /none}}]\n  \
              script: {actions: {onRun: {command: x}}}\n\
              - name: B\n  dependencies: [{environment: E}]\n  \
              script: {actions: {onRun: {command: x}}}\n";
    let job = create_ok(jt, &[], &[]).job;
    assert_eq!(entered(&job, 0), Vec::<String>::new());
    assert_eq!(entered(&job, 1), ["E"]);
    assert_eq!(job.steps[0].host_requirements, None);
    let attrs = job.steps[1]
        .host_requirements
        .as_ref()
        .and_then(|h| h.attributes.clone())
        .unwrap();
    assert_eq!(attrs[0].any_of, Some(vec!["data".to_string()]));
}

#[test]
fn env_raw_param_of_a_path_is_a_string_to_the_enable_if() {
    // Env.RawParam of a PATH is bound as a `string` (and LIST[PATH] as
    // `list[string]`) at template validation and at job creation, so a
    // string-only method such as upper() works on it. `==` would not show
    // the difference, since it accepts a path too.
    let jt = |d: &str, entry: &str| {
        format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             jobEnvironments:\n- name: E\n  envParameterDefinitions: [{{name: D, type: PATH{d}}}, \
             {{name: L, type: 'LIST[PATH]', default: [/a, /b]}}, {{name: Q, type: INT}}]\n  \
             enableIf: \"Env.RawParam.D.upper() != '/NONE' and Env.RawParam.L[1].upper() == '/B'\"\n  \
             variables: {{V: x}}\n\
             steps:\n- name: S\n  dependencies: [{{environment: E{entry}}}]\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    // A known value: `/none` disables E, so Q need not be supplied.
    let job = create_ok(&jt(", default: /none", ""), &[], &[]).job;
    assert_eq!(entered(&job, 0), Vec::<String>::new());
    // A value that does not convert is unknown, typed as a string: only
    // the value's own error, no enableIf type error.
    create_err(
        &jt(", default: /none", ", parameters: {D: \"{{ [1] }}\", Q: 1}"),
        &[],
        &[],
        "JobTemplate",
        &["steps[0] -> dependencies[0] -> parameters -> D:\n\tcould not be resolved as a value of \
           type PATH: Cannot coerce list[int] to path"],
    );
    // A missing value, at template validation: unresolved[string], so the
    // result depends on it and constraint 7 is reported then.
    job_err(
        &jt("", ""),
        &[
            "steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a \
           default of each Environment its Sessions enter (every jobEnvironments entry with \
           activation ALWAYS, and each one with activation ON_REQUEST that the Step requests; \
           Template Schemas §3.2 constraint 7); missing: 'D', 'Q' of Environment 'E' (its own \
           enableIf depends on a value that is missing or does not convert, so it does not \
           disable the Environment; Template Schemas §3.2.2).",
        ],
    );
}

#[test]
fn an_attachments_host_requirements_and_enable_if_read_env_raw_param_of_a_path() {
    // The same scope for an attached Environment Template (§4 item 5.1, item
    // 6 constraint 1): Env.RawParam of a PATH and of a LIST[PATH] element are
    // strings. A default of /none disables it; /data enables it.
    let att = |d: &str| {
        format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
             environment:\n  name: A\n  envParameterDefinitions: [{{name: D, type: PATH, \
             default: {d}}}, {{name: L, type: 'LIST[PATH]', default: [/a, /b]}}]\n  \
             enableIf: \"Env.RawParam.D.upper() != '/NONE'\"\n  hostRequirements:\n    \
             attributes: [{{name: attr.custom.dir, anyOf: [\"{{{{Env.RawParam.D.upper().replace('/', \
             '')}}}}\", \"{{{{Env.RawParam.L[1].upper().replace('/', '')}}}}\"]}}]\n  \
             variables: {{V: x}}\n"
        )
    };
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              steps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n";
    let job = create_ok(jt, &[&att("/data")], &[]).job;
    let attrs = job.steps[0]
        .host_requirements
        .as_ref()
        .and_then(|h| h.attributes.clone())
        .unwrap();
    assert_eq!(
        attrs[0].any_of,
        Some(vec!["DATA".to_string(), "B".to_string()])
    );
    let job = create_ok(jt, &[&att("/none")], &[]).job;
    assert_eq!(job.steps[0].host_requirements, None);
}

fn step_env_enable_if(expr: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         parameterDefinitions:\n- {{name: N, type: INT, default: 1}}\n\
         - {{name: Dir, type: PATH, default: /d}}\n\
         jobEnvironments:\n- name: Job\n  \
         envParameterDefinitions: [{{name: S, type: STRING, default: s}}]\n  variables: {{V: x}}\n\
         steps:\n- name: S\n  let: [l = Param.N + 1]\n  \
         parameterSpace: {{taskParameterDefinitions: [{{name: T, type: INT, range: [1]}}]}}\n  \
         stepEnvironments:\n  - name: L\n    enableIf: \"{expr}\"\n    \
         script:\n      actions: {{onEnter: {{command: x}}}}\n      \
         embeddedFiles: [{{name: F, type: TEXT, data: d}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    )
}

#[test]
fn a_step_environment_reads_the_steps_template_scope() {
    // Constraint 2: RawParam.*, non-PATH Param.*, Job.Name, Job.EnvNames,
    // Step.Name, and the Step's let.
    // Each name on its own (see the Job Environment test).
    for expr in [
        "Param.N == 1",
        "RawParam.Dir != ''",
        "Job.Name != ''",
        "'Job' in Job.EnvNames",
        "Step.Name == 'S'",
        "l == 2",
    ] {
        job_ok(&step_env_enable_if(expr));
    }
    for (expr, message) in [
        (
            "Param.Dir != ''",
            "Undefined variable: 'Param.Dir'. Did you mean one of: Param.N, RawParam.Dir",
        ),
        (
            "Task.Param.T == 1",
            "Undefined variable: 'Task.Param.T'. Did you mean: RawParam.N",
        ),
        (
            "Session.WorkingDirectory != ''",
            "Undefined variable: 'Session.WorkingDirectory'.",
        ),
        ("Env.File.F != ''", "Undefined variable: 'Env.File.F'."),
        (
            "Env.Param.S != ''",
            "Undefined variable: 'Env.Param.S'. Did you mean one of: Param.N, RawParam.N",
        ),
    ] {
        job_err(
            &step_env_enable_if(expr),
            &[&format!(
                "steps[0] -> stepEnvironments[0] -> enableIf:\n\tInvalid expression: {message}"
            )],
        );
    }
}

#[test]
fn an_environment_template_reads_its_own_parameters() {
    let et = |expr: &str| {
        format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
             parameterDefinitions: [{{name: Q, type: STRING, default: q}}]\n\
             environment:\n  name: E\n  \
             envParameterDefinitions: [{{name: S, type: STRING, default: s}}]\n  \
             enableIf: \"{expr}\"\n  variables: {{V: x}}\n"
        )
    };
    for expr in [
        "Param.Q == 'q'",
        "RawParam.Q == 'q'",
        "Env.Param.S == 's'",
        "Env.RawParam.S == 's'",
        "Job.Name != ''",
        "'E' in Job.EnvNames",
    ] {
        env_ok(&et(expr));
    }
    env_err(
        &et("Step.Name != ''"),
        &[
            "environment -> enableIf:\n\tInvalid expression: Undefined variable: 'Step.Name'. \
           Did you mean: Job.Name",
        ],
    );
    // Constraint 3: not the session scope either.
    for (expr, message) in [
        (
            "Session.WorkingDirectory != ''",
            "Undefined variable: 'Session.WorkingDirectory'.",
        ),
        (
            "Session.HasPathMappingRules",
            "Undefined variable: 'Session.HasPathMappingRules'.",
        ),
    ] {
        env_err(
            &et(expr),
            &[&format!(
                "environment -> enableIf:\n\tInvalid expression: {message}"
            )],
        );
    }
}

#[test]
fn a_disabled_environments_wrap_hooks_still_need_the_extension() {
    // The WRAP_ACTIONS gate applies whatever the Environment's enableIf.
    let jt = format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         jobEnvironments:\n- name: W\n  enableIf: false\n  {WRAP_HOOKS}\n\
         steps:\n- name: S\n  stepEnvironments:\n  - name: L\n    enableIf: false\n    {}\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n",
        WRAP_HOOKS.replace('\n', "\n  ")
    );
    let gate = |path: &str, hook: &str| {
        format!(
            "{path} -> script -> actions -> {hook}:\n\t{hook} requires the WRAP_ACTIONS extension."
        )
    };
    let mut expected = Vec::new();
    for path in ["jobEnvironments[0]", "steps[0] -> stepEnvironments[0]"] {
        for hook in ["onWrapEnvEnter", "onWrapTaskRun", "onWrapEnvExit"] {
            expected.push(gate(path, hook));
        }
    }
    job_err(
        &jt,
        &expected.iter().map(String::as_str).collect::<Vec<_>>(),
    );
}

// ── Template validation: constraint 7 and the single-layer rule ─────

#[test]
fn a_disabled_environment_is_not_held_to_constraint_7() {
    // §3.2.2: constraint 7 does not apply to a Step for which the
    // Environment is disabled. With the scalar `false` it is disabled for
    // every Step, which template validation knows; with an expression it is
    // undetermined there (§7.5 rule 5), and job creation decides it.
    let jt = |enable_if: &str| {
        format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             parameterDefinitions: [{{name: Use, type: STRING, default: 'no'}}]\n\
             jobEnvironments:\n- name: Lic\n  envParameterDefinitions: [{{name: Product, type: \
             STRING}}]\n  enableIf: {enable_if}\n  variables: {{V: x}}\n\
             steps:\n- name: S\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    let missing = |note: &str| {
        format!(
            "steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without \
             a default of each Environment its Sessions enter (every jobEnvironments entry with \
             activation ALWAYS, and each one with activation ON_REQUEST that the Step requests; \
             Template Schemas §3.2 constraint 7); missing: 'Product' of Environment 'Lic'{note}."
        )
    };
    // Disabled for every Step: valid, and the Job does not enter it.
    let created = create_ok(&jt("false"), &[], &[]);
    assert_eq!(entered(&created.job, 0), Vec::<String>::new());
    assert_eq!(created.job.steps[0].environment_parameters, None);
    // An expression: no error at template validation. `false` for the
    // Step: disabled, so the Job is created.
    let expr = jt("\"Param.Use == 'yes'\"");
    job_ok(&expr);
    let created = create_ok(&expr, &[], &[]);
    assert_eq!(
        created.disabled_environments[0].environments,
        [job_env_ref("Lic")]
    );
    // `true` for the Step: the parameter is required, with no note.
    create_err(
        &expr,
        &[],
        &[("Use", "yes")],
        "JobTemplate",
        &[&missing("")],
    );
    // The scalar `true`: required, at template validation.
    job_err(&jt("true"), &[&missing("")]);
    // An expression that reads only the missing value is certainly
    // unresolved, so it does not disable the Environment, at template
    // validation as at job creation, and the message says why.
    job_err(
        &jt("\"Env.Param.Product != ''\""),
        &[&missing(" (its own enableIf depends on a value that is missing or does not convert, so it does not disable the Environment; Template Schemas §3.2.2)")],
    );
}

const WRAP_HOOKS: &str = "script:\n    actions:\n      onWrapEnvEnter: {command: x}\n      \
     onWrapTaskRun: {command: x}\n      onWrapEnvExit: {command: x}";

/// Two wrapping Job Environments, `Outer` and `Inner`, with the given
/// `enableIf`s (`None` for absent), and parameter `UseInner`.
fn two_wrappers(outer: Option<&str>, inner: Option<&str>) -> String {
    let field = |e: Option<&str>| e.map(|e| format!("  enableIf: {e}\n")).unwrap_or_default();
    format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, EXPR]\n\
         name: J\nparameterDefinitions: [{{name: UseInner, type: STRING, default: 'yes'}}]\n\
         jobEnvironments:\n- name: Outer\n{}  {WRAP_HOOKS}\n- name: Inner\n{}  {WRAP_HOOKS}\n\
         steps:\n- name: S\n  script: {{actions: {{onRun: {{command: x}}}}}}\n",
        field(outer),
        field(inner)
    )
}

const SINGLE_LAYER_TV: &str = "jobEnvironments:\n\tonly one environment in the session stack may \
     define any of onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit (RFC 0008).";
const SINGLE_LAYER_JC: &str = "JobTemplate -> steps[0]:\n\tits Sessions would enter 2 \
     Environments that define wrap hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): \
     'Outer', 'Inner'. A Session may have at most one Environment defining wrap hooks (RFC \
     0008).";

#[test]
fn single_layer_rule_counts_only_environments_certainly_entered() {
    // Both certainly entered: rejected at template validation.
    job_err(&two_wrappers(None, None), &[SINGLE_LAYER_TV]);
    job_err(&two_wrappers(Some("true"), None), &[SINGLE_LAYER_TV]);
    // A scalar false counts as not entered, so the Job is created and the
    // Step enters only Outer.
    let created = create_ok(&two_wrappers(Some("true"), Some("false")), &[], &[]);
    assert_eq!(entered(&created.job, 0), ["Outer"]);
    // An expression, even a constant one, defers the rule to job creation:
    // accepted at template validation, and rejected or not by its value.
    for inner in ["\"true\"", "\"Param.UseInner == 'yes'\""] {
        let jt = two_wrappers(None, Some(inner));
        job_ok(&jt);
        create_err(&jt, &[], &[], "Submission", &[SINGLE_LAYER_JC]);
    }
    let created = create_ok(
        &two_wrappers(None, Some("\"Param.UseInner == 'yes'\"")),
        &[],
        &[("UseInner", "no")],
    );
    assert_eq!(entered(&created.job, 0), ["Outer"]);
    assert_eq!(
        created.disabled_environments,
        [StepDisabledEnvironments {
            environments: vec![job_env_ref("Inner")],
            step_environments: vec![],
        }]
    );
}

#[test]
fn single_layer_rule_skips_a_disabled_step_environment() {
    let jt = |enable_if: &str| {
        format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, EXPR]\n\
             name: J\njobEnvironments:\n- name: Outer\n  {WRAP_HOOKS}\n\
             steps:\n- name: S\n  stepEnvironments:\n  - name: Inner\n    enableIf: {enable_if}\n    \
             {}\n  script: {{actions: {{onRun: {{command: x}}}}}}\n",
            WRAP_HOOKS.replace('\n', "\n  ")
        )
    };
    job_ok(&jt("false"));
    job_err(
        &jt("true"),
        &[
            "steps[0] -> stepEnvironments:\n\tonly one environment in the session stack may \
           define any of onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit (RFC 0008).",
        ],
    );
    // An expression is decided at job creation.
    job_ok(&jt("\"Step.Name == 'S'\""));
    create_err(
        &jt("\"Step.Name == 'S'\""),
        &[],
        &[],
        "Submission",
        &[
            "JobTemplate -> steps[0]:\n\tits Sessions would enter 2 Environments that define wrap \
           hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'Outer', 'Inner' \
           (stepEnvironments). A Session may have at most one Environment defining wrap hooks \
           (RFC 0008).",
        ],
    );
    let created = create_ok(&jt("\"Step.Name == 'T'\""), &[], &[]);
    assert_eq!(created.job.steps[0].step_environments, None);
}

// ── Job creation: per-Step resolution and its effects ───────────────

const PER_STEP: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: First
  variables: {V: x}
- name: Conda
  envParameterDefinitions:
  - {name: Packages, type: STRING, default: none}
  enableIf: "Env.Param.Packages != 'none'"
  hostRequirements:
    amounts: [{name: amount.worker.gpu, min: 1}]
  variables: {CONDA: "{{Env.Param.Packages}}"}
steps:
- name: A
  dependencies:
  - environment: Conda
    parameters: {Packages: colmap}
  script: {actions: {onRun: {command: x}}}
- name: B
  script: {actions: {onRun: {command: x}}}
- name: C
  script: {actions: {onRun: {command: x}}}
"#;

#[test]
fn a_job_environment_is_decided_per_step_with_its_effective_values() {
    let created = create_ok(PER_STEP, &[], &[]);
    let job = &created.job;
    // A supplies Packages and enters Conda, with its values and its host
    // requirements.
    assert_eq!(entered(job, 0), ["First", "Conda"]);
    assert_eq!(job.steps[0].environments, None);
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["JobTemplate/Conda: Packages=string:colmap"]
    );
    assert!(job.steps[0].host_requirements.is_some());
    // B and C take the default, for which enableIf is false: they enter
    // only First, have no values for Conda, and get none of its host
    // requirements.
    for s in [1, 2] {
        assert_eq!(entered(job, s), ["First"]);
        assert_eq!(job.steps[s].environments, Some(vec![job_env_ref("First")]));
        assert_eq!(job.steps[s].environment_parameters, None);
        assert_eq!(job.steps[s].host_requirements, None);
    }
    assert_eq!(
        created.disabled_environments,
        [
            StepDisabledEnvironments::default(),
            StepDisabledEnvironments {
                environments: vec![job_env_ref("Conda")],
                step_environments: vec![],
            },
            StepDisabledEnvironments {
                environments: vec![job_env_ref("Conda")],
                step_environments: vec![],
            },
        ]
    );
    // `same_environments` compares the final sets: B and C enter the same
    // Job Environments, and A's differ from both.
    assert!(job.steps[1].same_environments(&job.steps[2]));
    assert!(!job.steps[0].same_environments(&job.steps[1]));
    // Job.EnvNames still lists Conda (§7.3.1).
    let names: Vec<&str> = job
        .job_environments
        .iter()
        .flatten()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(names, ["First", "Conda"]);
}

#[test]
fn a_disabled_environments_host_requirements_are_not_resolved() {
    // E's amount resolves to a negative min for the default -1, which
    // would be an error; enableIf is false for that value, so the
    // requirements are never resolved for B.
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR, FEATURE_BUNDLE_1]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: N, type: INT, default: -1}]
  enableIf: "Env.Param.N >= 0"
  hostRequirements:
    amounts: [{name: amount.custom, min: "{{Env.Param.N}}"}]
  variables: {V: x}
steps:
- name: A
  dependencies: [{environment: E, parameters: {N: 2}}]
  script: {actions: {onRun: {command: x}}}
- name: B
  script: {actions: {onRun: {command: x}}}
"#;
    let job = create_ok(jt, &[], &[]).job;
    assert_eq!(entered(&job, 1), Vec::<String>::new());
    assert_eq!(job.steps[1].host_requirements, None);
    let a = job.steps[0].host_requirements.as_ref().unwrap();
    assert_eq!(a.amounts.as_ref().unwrap()[0].min, Some(2.0));
    // With enableIf true for every value, B's requirements are resolved
    // and fail.
    create_err(
        &jt.replace("\"Env.Param.N >= 0\"", "true"),
        &[],
        &[],
        "Submission",
        &[
            "JobTemplate -> jobEnvironments[0] -> hostRequirements -> amounts[0] -> min:\n\tmust \
           be non-negative. Resolved for Step 'B'.",
        ],
    );
}

#[test]
fn a_null_result_is_an_error_naming_the_steps() {
    let jt = PER_STEP.replace(
        "\"Env.Param.Packages != 'none'\"",
        "\"None if Env.Param.Packages == 'none' else true\"",
    );
    create_err(
        &jt,
        &[],
        &[],
        "JobTemplate",
        &[
            "jobEnvironments[1] -> enableIf:\n\tresolved to null, which does not fit \
           type bool (Template Schemas §7.5). Resolved for Steps 'B' and 'C'.",
        ],
    );
}

#[test]
fn the_values_are_not_checked_where_the_environment_is_disabled() {
    // §3.2.2: a Step's effective values for an Environment disabled for it
    // are not checked against their type and constraints. A value that
    // converts is known to the `enableIf` whether or not it meets them;
    // one that does not convert is unknown, so an `enableIf` that reads it
    // is unresolved and does not disable the Environment.
    let jt = |enable_if: &str, count: &str| {
        format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             parameterDefinitions: [{{name: N, type: STRING, default: '0'}}]\n\
             jobEnvironments:\n- name: E\n  envParameterDefinitions: [{{name: Count, type: INT, \
             minValue: 1, default: 1}}]\n  enableIf: {enable_if}\n  variables: {{V: x}}\n\
             steps:\n- name: S\n  dependencies:\n  - environment: E\n    parameters: {{Count: \
             \"{count}\"}}\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    let below = "steps[0] -> dependencies[0] -> parameters -> Count:\n\tParameter 'Count': value \
                 0 is less than minimum 1";
    // Disabled: the out-of-range value is not checked.
    let created = create_ok(&jt("\"false\"", "{{Param.N}}"), &[], &[]);
    assert_eq!(entered(&created.job, 0), Vec::<String>::new());
    // The enableIf reads the out-of-range value, which converts, so it is
    // known: 0 > 0 is false, and the Environment is disabled.
    create_ok(&jt("\"Env.Param.Count > 0\"", "{{Param.N}}"), &[], &[]);
    // Not disabled: the value is checked.
    create_err(
        &jt("\"true\"", "{{Param.N}}"),
        &[],
        &[],
        "JobTemplate",
        &[below],
    );
    create_err(
        &jt("\"Env.Param.Count == 0\"", "{{Param.N}}"),
        &[],
        &[],
        "JobTemplate",
        &[below],
    );
    // A value that does not convert is not checked where the enableIf does
    // not read it and is false.
    let many = [("N", "many")];
    create_ok(&jt("\"Param.N == '0'\"", "{{Param.N}}"), &[], &many);
    // Where the enableIf reads it, the result is unresolved, and the
    // value is rejected.
    create_err(
        &jt("\"Env.Param.Count > 0\"", "{{Param.N}}"),
        &[],
        &many,
        "JobTemplate",
        &[
            "steps[0] -> dependencies[0] -> parameters -> Count:\n\tcould not be resolved as a \
           value of type INT: Cannot convert 'many' to int: invalid digit found in string\n  \
           Param.N\n  ~~~~~~^",
        ],
    );
}
#[test]
fn a_step_environment_is_decided_with_the_steps_let() {
    let jt = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Use, type: STRING, default: 'no'}]
steps:
- name: S
  let: [wanted = Param.Use == 'yes']
  stepEnvironments:
  - {name: Scratch, enableIf: "wanted", variables: {V: x}}
  - {name: Always, variables: {V: x}}
  script: {actions: {onRun: {command: x}}}
"#;
    let created = create_ok(jt, &[], &[]);
    let envs: Vec<&str> = created.job.steps[0]
        .step_environments
        .iter()
        .flatten()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(envs, ["Always"]);
    assert_eq!(
        created.disabled_environments[0].step_environments,
        ["Scratch".to_string()]
    );
    let created = create_ok(jt, &[], &[("Use", "yes")]);
    assert_eq!(
        created.job.steps[0]
            .step_environments
            .as_ref()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        created.disabled_environments,
        [StepDisabledEnvironments::default()]
    );
    // Every Step Environment disabled: the Step has none.
    let only = jt.replace("  - {name: Always, variables: {V: x}}\n", "");
    let created = create_ok(&only, &[], &[]);
    assert_eq!(created.job.steps[0].step_environments, None);
    // A null result is an error for the Job Template, at the field.
    create_err(
        &jt.replace("\"wanted\"", "\"None if not wanted else true\""),
        &[],
        &[],
        "JobTemplate",
        &[
            "steps[0] -> stepEnvironments[0] -> enableIf:\n\tresolved to null, which does not fit \
           type bool (Template Schemas §7.5).",
        ],
    );
}

// ── Attached Environments ───────────────────────────────────────────

const ATTACHED_CONDA: &str = r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: Conda
  envParameterDefinitions:
  - {name: Packages, type: STRING, default: none}
  enableIf: "Env.Param.Packages != 'none'"
  variables: {V: x}
"#;

const STEPS_FOR_ATTACHED: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: A
  dependencies:
  - environment: Conda
    parameters: {Packages: ffmpeg}
  script: {actions: {onRun: {command: x}}}
- name: B
  script: {actions: {onRun: {command: x}}}
"#;

#[test]
fn an_attached_environment_is_decided_per_step() {
    let created = create_ok(STEPS_FOR_ATTACHED, &[ATTACHED_CONDA], &[]);
    let conda = EnvironmentRef {
        document: Document::environment_template(0, None),
        environment: "Conda".to_string(),
    };
    assert_eq!(created.job.steps[0].environments, None);
    assert_eq!(created.job.steps[1].environments, Some(vec![]));
    assert_eq!(created.job.steps[1].environment_parameters, None);
    assert_eq!(
        created.disabled_environments[1],
        StepDisabledEnvironments {
            environments: vec![conda],
            step_environments: vec![],
        }
    );
}

#[test]
fn constraint_7_at_job_creation_notes_an_unresolved_enable_if() {
    // The Step's entry for Lic has an expression enableIf, so constraint 7
    // is decided at job creation; the entry is disabled, and Lic's own
    // enableIf reads the parameter the Step therefore does not supply, so
    // it is unresolved and does not disable Lic. The message says both.
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              jobEnvironments:\n- name: Lic\n  envParameterDefinitions: [{name: Product, type: STRING}]\n  \
              enableIf: \"Env.Param.Product != ''\"\n  variables: {V: x}\n\
              steps:\n- name: S\n  dependencies: [{environment: Lic, enableIf: \"Job.Name == 'K'\", \
              parameters: {Product: p}}]\n  script: {actions: {onRun: {command: x}}}\n";
    create_err(
        jt,
        &[],
        &[],
        "JobTemplate",
        &[
            "steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a \
           default of each Environment its Sessions enter (every jobEnvironments entry with \
           activation ALWAYS, and each one with activation ON_REQUEST that the Step requests; \
           Template Schemas §3.2 constraint 7); missing: 'Product' of Environment 'Lic' (the \
           Step's 'environment:' dependency for it is disabled by its enableIf, so it supplies \
           nothing; Template Schemas §3.2 item 5) (its own enableIf depends on a value that is \
           missing or does not convert, so it does not disable the Environment; Template Schemas \
           §3.2.2).",
        ],
    );
}
#[test]
fn constraint_7_and_an_attached_environments_enable_if() {
    // Without a default, B does not supply Packages, so Conda's enableIf,
    // which reads it, is unresolved for B: Conda is not disabled, and
    // constraint 7 rejects B (§3.2.2).
    let env = ATTACHED_CONDA.replace(", default: none", "");
    create_err(
        STEPS_FOR_ATTACHED,
        &[&env],
        &[],
        "Submission",
        &[
            "JobTemplate -> steps[1]:\n\tmust supply, in an 'environment:' dependency, every \
           parameter without a default of each Environment its Sessions enter (every attached \
           Environment with activation ALWAYS, and each one with activation ON_REQUEST that the \
           Step requests; Template Schemas §3.2.1); missing: 'Packages' of Environment 'Conda' \
           (EnvironmentTemplate[0]) (its own enableIf depends on a value that is missing or does \
           not convert, so it does not disable the Environment; Template Schemas §3.2.2).",
        ],
    );
    // An enableIf over the attachment's own Job Parameter that is false
    // for B disables Conda for it, so B need not supply Packages.
    let env = env
        .replace(
            "environment:\n",
            "parameterDefinitions: [{name: UseConda, type: STRING, default: 'no'}]\nenvironment:\n",
        )
        .replace(
            "\"Env.Param.Packages != 'none'\"",
            "\"Param.UseConda == 'yes'\"",
        );
    let created = create_ok(STEPS_FOR_ATTACHED, &[&env], &[]);
    assert_eq!(created.job.steps[1].environments, Some(vec![]));
    assert_eq!(created.job.steps[1].environment_parameters, None);
}
#[test]
fn check_environment_templates_counts_only_certainly_entered_layers() {
    // A wrapping attachment and a wrapping Job Environment: a scalar
    // false or an expression on either leaves the Step to job creation.
    let attached = |enable_if: &str| {
        format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, EXPR]\n\
             environment:\n  name: Att\n  enableIf: {enable_if}\n  {WRAP_HOOKS}\n"
        )
    };
    let jt = job_ok(&format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, EXPR]\n\
         name: J\njobEnvironments:\n- name: Own\n  {WRAP_HOOKS}\n\
         steps:\n- name: S\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    ));
    let check = |enable_if: &str| {
        let env = env_ok(&attached(enable_if));
        check_environment_templates(
            &jt,
            &[AttachedEnvironmentTemplate::new(&env)],
            &posix_ctx(&jt),
        )
        .map_err(|e| e.to_string())
    };
    assert!(check("false").is_ok());
    assert!(check("\"true\"").is_ok());
    let err = check("true").unwrap_err();
    assert_errors(
        &err,
        "Submission",
        &[
            "JobTemplate -> steps[0]:\n\tits Sessions would enter 2 Environments that define wrap \
           hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'Att' (EnvironmentTemplate[0]), \
           'Own'. A Session may have at most one Environment defining wrap hooks (RFC 0008).",
        ],
    );
}

#[test]
fn check_environment_templates_skips_a_disabled_environments_literal_requirements() {
    let env = |enable_if: &str| {
        format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
             environment:\n  name: Gpu\n  enableIf: {enable_if}\n  \
             hostRequirements: {{amounts: [{{name: amount.worker.gpu, min: 4}}]}}\n  \
             variables: {{V: x}}\n"
        )
    };
    let jt = job_ok(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         steps:\n- name: S\n  hostRequirements: {amounts: [{name: amount.worker.gpu, max: 1}]}\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    for enable_if in ["false", "\"Job.Name == 'K'\""] {
        let e = env_ok(&env(enable_if));
        assert!(
            check_environment_templates(
                &jt,
                &[AttachedEnvironmentTemplate::new(&e)],
                &posix_ctx(&jt)
            )
            .is_ok(),
            "{enable_if}"
        );
    }
    let e = env_ok(&env("true"));
    let err = check_environment_templates(
        &jt,
        &[AttachedEnvironmentTemplate::new(&e)],
        &posix_ctx(&jt),
    )
    .unwrap_err();
    assert_errors(
        &err.to_string(),
        "Submission",
        &[&gpu_conflict(
            "EnvironmentTemplate[0] -> environment",
            "Gpu",
        )],
    );
    // Job creation applies the expression: false for this Job's name.
    let values = job_params_with(&jt, &[], &[]);
    let e = env_ok(&env("\"Job.Name == 'K'\""));
    let created = create_job_with_environment_templates(
        &jt,
        &[AttachedEnvironmentTemplate::new(&e)],
        &values,
        &posix_ctx(&jt),
    )
    .unwrap();
    assert_eq!(created.job.steps[0].environments, Some(vec![]));
}

/// The conflict of a Step's `max: 1` GPU with an Environment's `min: 4`, as
/// `check_environment_templates` and job creation report it, for
/// `environment` at `path`.
fn gpu_conflict(path: &str, environment: &str) -> String {
    format!(
        "JobTemplate -> steps[0]:\n\tStep 'S': no host can satisfy the combined host requirements \
         for amount 'amount.worker.gpu': {path} -> hostRequirements (Environment '{environment}') \
         requires min 4, and JobTemplate -> steps[0] -> hostRequirements requires max 1. \
         Template Schemas §3.3.3."
    )
}

// ── Round 1 audit: further coverage ─────────────────────────────────

#[test]
fn check_environment_templates_skips_disabled_job_and_step_environment_layers() {
    // A wrapping attachment, and a wrapping Job or Step Environment whose
    // own enableIf is the scalar false (not entered) or true.
    let att = env_ok(&format!(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, EXPR]\n\
         environment:\n  name: Att\n  {WRAP_HOOKS}\n"
    ));
    let job_env = |enable_if: &str| {
        job_ok(&format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, EXPR]\n\
             name: J\njobEnvironments:\n- name: Own\n  enableIf: {enable_if}\n  {WRAP_HOOKS}\n\
             steps:\n- name: S\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        ))
    };
    let step_env = |enable_if: &str| {
        job_ok(&format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, EXPR]\n\
             name: J\nsteps:\n- name: S\n  stepEnvironments:\n  - name: Mine\n    enableIf: {enable_if}\n    \
             {}\n  script: {{actions: {{onRun: {{command: x}}}}}}\n",
            WRAP_HOOKS.replace('\n', "\n  ")
        ))
    };
    let check = |jt: &openjd_model::template::JobTemplate| {
        check_environment_templates(
            jt,
            &[AttachedEnvironmentTemplate::new(&att)],
            &posix_ctx(jt),
        )
        .map_err(|e| e.to_string())
    };
    let layers = |second: &str| {
        format!(
            "JobTemplate -> steps[0]:\n\tits Sessions would enter 2 Environments that define wrap \
             hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'Att' (EnvironmentTemplate[0]), \
             {second}. A Session may have at most one Environment defining wrap hooks (RFC 0008)."
        )
    };
    assert_eq!(check(&job_env("false")), Ok(vec![]));
    assert_eq!(check(&job_env("\"true\"")), Ok(vec![]));
    assert_errors(
        &check(&job_env("true")).unwrap_err(),
        "Submission",
        &[&layers("'Own'")],
    );
    assert_eq!(check(&step_env("false")), Ok(vec![]));
    assert_eq!(check(&step_env("\"true\"")), Ok(vec![]));
    assert_errors(
        &check(&step_env("true")).unwrap_err(),
        "Submission",
        &[&layers("'Mine' (stepEnvironments)")],
    );
}

#[test]
fn check_environment_templates_skips_a_disabled_job_environments_literal_requirements() {
    let jt = |enable_if: &str| {
        job_ok(&format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             jobEnvironments:\n- name: Gpu\n  enableIf: {enable_if}\n  \
             hostRequirements: {{amounts: [{{name: amount.worker.gpu, min: 4}}]}}\n  variables: {{V: x}}\n\
             steps:\n- name: S\n  hostRequirements: {{amounts: [{{name: amount.worker.gpu, max: 1}}]}}\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n"
        ))
    };
    let att = env_ok(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
         environment: {name: Other, variables: {V: x}}\n",
    );
    let check = |jt: &openjd_model::template::JobTemplate| {
        check_environment_templates(
            jt,
            &[AttachedEnvironmentTemplate::new(&att)],
            &posix_ctx(jt),
        )
        .map_err(|e| e.to_string())
    };
    assert_eq!(check(&jt("false")), Ok(vec![]));
    assert_eq!(check(&jt("\"true\"")), Ok(vec![]));
    assert_errors(
        &check(&jt("true")).unwrap_err(),
        "Submission",
        &[&gpu_conflict("JobTemplate -> jobEnvironments[0]", "Gpu")],
    );
}

#[test]
fn a_failed_job_environment_enable_if_stops_before_the_single_layer_rule() {
    // Outer's enableIf fails for S. Outer is checked as if it is not
    // disabled, but a Job Environment's failure is reported when the Job
    // Template's Steps are decided, which stops job creation before the
    // submission stage, where the single-layer rule runs; so Outer and Inner
    // are not also reported. One Step, so the note is singular.
    let jt = format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, EXPR]\n\
         name: J\njobEnvironments:\n- name: Outer\n  enableIf: \"None if Job.Name == 'J' else true\"\n  \
         {WRAP_HOOKS}\nsteps:\n- name: S\n  stepEnvironments:\n  - name: Inner\n    {}\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n",
        WRAP_HOOKS.replace('\n', "\n  ")
    );
    create_err(
        &jt,
        &[],
        &[],
        "JobTemplate",
        &[
            "jobEnvironments[0] -> enableIf:\n\tresolved to null, which does not fit \
           type bool (Template Schemas §7.5). Resolved for Step 'S'.",
        ],
    );
}

#[test]
fn an_evaluation_error_keeps_its_expression_and_caret_lines_last() {
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              parameterDefinitions: [{name: L, type: 'LIST[INT]', default: [1]}]\n\
              jobEnvironments:\n- {name: E, enableIf: \"Param.L[5] == 1\", variables: {V: x}}\n\
              steps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n";
    let err = create(jt, &[], &[]).expect_err("expected job creation to fail");
    assert_eq!(
        err.to_string(),
        "Model validation error: 1 validation error for JobTemplate\n\
         jobEnvironments[0] -> enableIf:\n\tcould not be evaluated: Index 5 out of \
         bounds for list of length 1. Resolved for Step 'S'.\n  Param.L[5] == 1\n  ~~~~~~~^~~"
    );
}

#[test]
fn disabled_environments_name_each_environments_document() {
    let att = "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
               environment: {name: Att, enableIf: \"false\", variables: {V: x}}\n";
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              jobEnvironments:\n- {name: Off, enableIf: false, variables: {V: x}}\n\
              steps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n";
    let created = create_ok(jt, &[att], &[]);
    assert_eq!(
        created.disabled_environments,
        [StepDisabledEnvironments {
            environments: vec![
                EnvironmentRef {
                    document: Document::environment_template(0, None),
                    environment: "Att".to_string(),
                },
                job_env_ref("Off"),
            ],
            step_environments: vec![],
        }]
    );
    assert_eq!(created.job.steps[0].environments, Some(vec![]));
}

#[test]
fn a_step_environment_reads_its_let_with_posix_paths() {
    // An enableIf is not host context: the `let` binding it reads keeps
    // the value job creation computed with POSIX paths, whatever the
    // submission path format.
    let jt = job_ok(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         steps:\n- name: S\n  let: [\"p = string(path('/a') / 'b')\"]\n  \
         stepEnvironments:\n  - {name: Posix, enableIf: \"p == '/a/b'\", variables: {V: x}}\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    let values = job_params_with(&jt, &[], &[]);
    for format in [
        openjd_expr::path_mapping::PathFormat::Posix,
        openjd_expr::path_mapping::PathFormat::Windows,
    ] {
        let ctx = jt
            .default_validation_context()
            .with_submission_path_format(format);
        let names: Vec<String> = create_job_with_environment_templates(&jt, &[], &values, &ctx)
            .unwrap()
            .job
            .steps[0]
            .step_environments
            .iter()
            .flatten()
            .map(|e| e.name.clone())
            .collect();
        assert_eq!(names, ["Posix"], "{format:?}");
    }
}

// ── Round 3 audit: the constraint 7 note ────────────────────────────

/// The constraint 7 message for attachment `Att`'s parameter `P`, with
/// the unresolved note when `noted`.
fn att_missing(noted: bool) -> String {
    let note = if noted {
        " (its own enableIf depends on a value that is missing or does not convert, so it does \
         not disable the Environment; Template Schemas §3.2.2)"
    } else {
        ""
    };
    format!(
        "JobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every \
         parameter without a default of each Environment its Sessions enter (every attached \
         Environment with activation ALWAYS, and each one with activation ON_REQUEST that the \
         Step requests; Template Schemas §3.2.1); missing: 'P' of Environment 'Att' \
         (EnvironmentTemplate[0]){note}."
    )
}

fn att_with_enable_if(enable_if: &str) -> String {
    format!(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
         environment:\n  name: Att\n  envParameterDefinitions: [{{name: P, type: STRING}}]\n  \
         enableIf: {enable_if}\n  variables: {{V: x}}\n"
    )
}

const ONE_STEP: &str =
    "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
     name: J\nsteps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n";

#[test]
fn constraint_7_for_an_attachment_follows_its_enable_if() {
    // Job creation: `true` and an expression that is `true` require P with
    // no note; the scalar `false` disables Att, so nothing is required;
    // an expression that reads P is unresolved, with the note.
    for (enable_if, outcome) in [
        ("true", Some(false)),
        ("false", None),
        ("\"Job.Name == 'J'\"", Some(false)),
        ("\"Job.Name == 'K'\"", None),
        ("\"Env.Param.P != ''\"", Some(true)),
    ] {
        let env = att_with_enable_if(enable_if);
        match outcome {
            Some(noted) => create_err(ONE_STEP, &[&env], &[], "Submission", &[&att_missing(noted)]),
            None => {
                create_ok(ONE_STEP, &[&env], &[]);
            }
        }
        // check_environment_templates reports constraint 7 only where Att
        // is certainly not disabled: its enableIf is `true`, or certainly
        // unresolved (it reads only P, which no Step supplies).
        let jt = job_ok(ONE_STEP);
        let e = env_ok(&env);
        let checked = check_environment_templates(
            &jt,
            &[AttachedEnvironmentTemplate::new(&e)],
            &posix_ctx(&jt),
        );
        match enable_if {
            "true" | "\"Env.Param.P != ''\"" => {
                let err = checked.expect_err("expected check to reject the submission");
                assert_errors(
                    &err.to_string(),
                    "Submission",
                    &[&att_missing(enable_if != "true")],
                );
            }
            _ => assert_eq!(checked.unwrap(), vec![], "enableIf: {enable_if}"),
        }
    }
}
#[test]
fn literal_enabled_is_known_only_for_an_absent_field_or_a_boolean_scalar() {
    let env = |enable_if: &str| {
        env_ok(&format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
             environment:\n  name: E\n{enable_if}  variables: {{V: x}}\n"
        ))
        .environment
        .literal_enabled()
    };
    assert_eq!(env(""), Some(true));
    assert_eq!(env("  enableIf: true\n"), Some(true));
    assert_eq!(env("  enableIf: false\n"), Some(false));
    assert_eq!(env("  enableIf: \"true\"\n"), None);
    assert_eq!(env("  enableIf: \"false\"\n"), None);
}

#[test]
fn a_job_environments_enable_if_evaluates_paths_in_posix_form() {
    // A Job or attached Environment's enableIf has its hostRequirements
    // scope and resolves as they do, with POSIX paths, whatever the
    // submission path format.
    let jt = job_ok(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         jobEnvironments:\n- {name: E, enableIf: \"string(path('/a') / 'b') == '/a/b'\", \
         variables: {V: x}}\nsteps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n",
    );
    let values = job_params_with(&jt, &[], &[]);
    for format in [
        openjd_expr::path_mapping::PathFormat::Posix,
        openjd_expr::path_mapping::PathFormat::Windows,
    ] {
        let ctx = jt
            .default_validation_context()
            .with_submission_path_format(format);
        let created = create_job_with_environment_templates(&jt, &[], &values, &ctx).unwrap();
        assert_eq!(created.job.steps[0].environments, None, "{format:?}");
        assert_eq!(
            created.disabled_environments,
            [StepDisabledEnvironments::default()]
        );
    }
}

// ── Round 5 audit ───────────────────────────────────────────────────

#[test]
fn a_job_environments_enable_if_reads_job_env_names_at_job_creation() {
    let created = create_ok(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         jobEnvironments:\n- {name: E, enableIf: \"'E' in Job.EnvNames\", variables: {V: x}}\n\
         - {name: F, enableIf: \"'G' in Job.EnvNames\", variables: {V: x}}\n\
         steps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n",
        &[],
        &[],
    );
    assert_eq!(
        created.job.steps[0].environments,
        Some(vec![job_env_ref("E")])
    );
    assert_eq!(
        created.disabled_environments[0].environments,
        [job_env_ref("F")]
    );
}

#[test]
fn a_job_environments_enable_if_runs_under_the_caller_budgets() {
    let jt = job_ok(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         parameterDefinitions: [{name: N, type: INT, default: 1000}]\n\
         jobEnvironments:\n- {name: E, enableIf: \"sum([1] * Param.N) > 0\", variables: {V: x}}\n\
         steps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n",
    );
    let values = job_params_with(&jt, &[], &[]);
    let ctx = posix_ctx(&jt).with_caller_limits(openjd_model::CallerLimits {
        max_eval_operations: Some(50),
        ..Default::default()
    });
    let err = create_job_with_environment_templates(&jt, &[], &values, &ctx).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &[
            "jobEnvironments[0] -> enableIf:\n\tcould not be evaluated: Expression \
           operation count (1004) exceeded limit (50). Resolved for Step 'S'.",
        ],
    );
    // With the default budgets it evaluates.
    let created =
        create_job_with_environment_templates(&jt, &[], &values, &posix_ctx(&jt)).unwrap();
    assert_eq!(created.job.steps[0].environments, None);
}

#[test]
fn a_let_read_only_by_an_enable_if_is_not_evaluated_in_the_submission_path_format() {
    // `r` fails in WINDOWS form, but only an enableIf reads it, so it is
    // not evaluated again in the submission path format, and its POSIX
    // value enables the Step Environment. An entry's parameter value that
    // read it would be reported (test_env_params_job_creation).
    let jt = job_ok(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         steps:\n- name: S\n  let: [\"r = string(path('/a/b').relative_to('/a'))\"]\n  \
         stepEnvironments:\n  - {name: SE, enableIf: \"r == 'b'\", variables: {V: x}}\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    let values = job_params_with(&jt, &[], &[]);
    let ctx = jt
        .default_validation_context()
        .with_submission_path_format(openjd_expr::path_mapping::PathFormat::Windows);
    let created = create_job_with_environment_templates(&jt, &[], &values, &ctx).unwrap();
    assert_eq!(
        created.job.steps[0]
            .step_environments
            .as_ref()
            .map(Vec::len),
        Some(1)
    );
}

// ── A set-dependent check fails early only when the failure is certain ──

#[test]
fn a_certain_single_layer_violation_is_reported_before_job_creation() {
    // Outer and Inner are certainly entered; Maybe's enableIf is an
    // expression. Whatever Maybe resolves to, every Session has two
    // wrapping Environments, so template validation rejects the template.
    let jt = format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, EXPR]\n\
         name: J\njobEnvironments:\n- name: Outer\n  {WRAP_HOOKS}\n- name: Inner\n  enableIf: true\n  \
         {WRAP_HOOKS}\n- name: Maybe\n  enableIf: \"Job.Name == 'K'\"\n  {WRAP_HOOKS}\n\
         steps:\n- name: S\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    );
    job_err(&jt, &[SINGLE_LAYER_TV]);
    // check_environment_templates does the same with an attachment: Att
    // and Own are certainly entered, Maybe is not counted.
    let att = env_ok(&format!(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, EXPR]\n\
         environment:\n  name: Att\n  {WRAP_HOOKS}\n"
    ));
    let maybe = env_ok(&format!(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, EXPR]\n\
         environment:\n  name: Maybe\n  enableIf: \"Job.Name == 'K'\"\n  {WRAP_HOOKS}\n"
    ));
    let own = job_ok(&format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, EXPR]\n\
         name: J\njobEnvironments:\n- name: Own\n  {WRAP_HOOKS}\n\
         steps:\n- name: S\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    ));
    let err = check_environment_templates(
        &own,
        &[
            AttachedEnvironmentTemplate::new(&att),
            AttachedEnvironmentTemplate::new(&maybe),
        ],
        &posix_ctx(&own),
    )
    .unwrap_err();
    assert_errors(
        &err.to_string(),
        "Submission",
        &[
            "JobTemplate -> steps[0]:\n\tits Sessions would enter 2 Environments that define wrap \
           hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'Att' (EnvironmentTemplate[0]), \
           'Own'. A Session may have at most one Environment defining wrap hooks (RFC 0008).",
        ],
    );
    // With only one certainly entered wrapper, the expression could leave
    // the set fine, so neither stage before job creation rejects it.
    let one = env_ok(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
         environment: {name: Plain, variables: {V: x}}\n",
    );
    assert_eq!(
        check_environment_templates(
            &own,
            &[
                AttachedEnvironmentTemplate::new(&one),
                AttachedEnvironmentTemplate::new(&maybe),
            ],
            &posix_ctx(&own),
        )
        .map_err(|e| e.to_string()),
        Ok(vec![])
    );
}

// ── Round 6 audit ───────────────────────────────────────────────────

/// A Step with a `let` binding `r` that fails only in WINDOWS form, and an
/// entry for ON_REQUEST `R` whose value reads it, with `enable_if`.
fn windows_let_entry(enable_if: &str) -> openjd_model::template::JobTemplate {
    job_ok(&format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         jobEnvironments:\n- {{name: R, activation: ON_REQUEST, \
         envParameterDefinitions: [{{name: P, type: STRING, default: d}}], variables: {{V: x}}}}\n\
         steps:\n- name: S\n  let: [\"r = string(path('/a/b').relative_to('/a'))\"]\n  \
         dependencies: [{{environment: R, enableIf: {enable_if}, parameters: {{P: \"{{{{r}}}}\"}}}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    ))
}

fn windows_ctx(jt: &openjd_model::template::JobTemplate) -> openjd_model::ValidationContext {
    jt.default_validation_context()
        .with_submission_path_format(openjd_expr::path_mapping::PathFormat::Windows)
}

const WINDOWS_LET_ERROR: &str = "steps[0] -> let[0]:\n\tin the submission path format (WINDOWS), \
     which this Step's Environment parameter values resolve in: Expression error: Error \
     evaluating let binding 'r': relative_to failed: '\\a\\b' is not relative to '/a'\n  \
     r = string(path('/a/b').relative_to('/a'))\n             ~~~~~~~~~~~~~^~~~~~~~~~~~~~~~~";

#[test]
fn a_disabled_entrys_values_do_not_reevaluate_let_bindings() {
    // A disabled entry has no effect of any kind (§3.2 item 2), so the
    // binding only its value reads is not evaluated again in WINDOWS form,
    // whether its enableIf is the scalar false or an expression.
    for enable_if in ["false", "\"Step.Name == 'Q'\""] {
        let jt = windows_let_entry(enable_if);
        let values = job_params_with(&jt, &[], &[]);
        let job = create_job_with_environment_templates(&jt, &[], &values, &windows_ctx(&jt))
            .unwrap_or_else(|e| panic!("{enable_if}: {e}"))
            .job;
        assert_eq!(job.steps[0].environments, Some(vec![]), "{enable_if}");
    }
    // Enabled, the value reads it, and it fails in WINDOWS form.
    for enable_if in ["true", "\"Step.Name == 'S'\""] {
        let jt = windows_let_entry(enable_if);
        let values = job_params_with(&jt, &[], &[]);
        let err = create_job_with_environment_templates(&jt, &[], &values, &windows_ctx(&jt))
            .unwrap_err();
        assert_errors(&err.to_string(), "JobTemplate", &[WINDOWS_LET_ERROR]);
    }
}

#[test]
fn enable_ifs_use_the_template_scope_when_an_entry_value_reads_the_same_let() {
    // `p` is read by an entry value (evaluated again in WINDOWS form) and
    // by a Step Environment's and the entry's enableIfs (template scope,
    // POSIX), so each gets its own value.
    let jt = job_ok(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         jobEnvironments:\n- {name: E, activation: ON_REQUEST, \
         envParameterDefinitions: [{name: N, type: STRING}], variables: {V: x}}\n\
         steps:\n- name: S\n  let: [\"p = string(path('/a') / 'b')\"]\n  \
         dependencies: [{environment: E, enableIf: \"p == '/a/b'\", parameters: {N: \"{{ p }}\"}}]\n  \
         stepEnvironments:\n  - {name: Posix, enableIf: \"p == '/a/b'\", variables: {V: x}}\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    let values = job_params_with(&jt, &[], &[]);
    let job = create_job_with_environment_templates(&jt, &[], &values, &windows_ctx(&jt))
        .unwrap()
        .job;
    let names: Vec<&str> = job.steps[0]
        .step_environments
        .iter()
        .flatten()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(names, ["Posix"]);
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["JobTemplate/E: N=string:\\a\\b"]
    );
}

#[test]
fn an_entrys_enable_if_names_are_not_evaluated_again() {
    // `r` fails in WINDOWS form, but only the entry's enableIf reads it
    // (its value is a literal), so the Job is created.
    let jt = job_ok(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         jobEnvironments:\n- {name: E, activation: ON_REQUEST, \
         envParameterDefinitions: [{name: N, type: STRING}], variables: {V: x}}\n\
         steps:\n- name: S\n  let: [\"r = string(path('/a/b').relative_to('/a'))\"]\n  \
         dependencies: [{environment: E, enableIf: \"r == 'b'\", parameters: {N: lit}}]\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    let values = job_params_with(&jt, &[], &[]);
    let job = create_job_with_environment_templates(&jt, &[], &values, &windows_ctx(&jt))
        .unwrap()
        .job;
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["JobTemplate/E: N=string:lit"]
    );
}

#[test]
fn check_environment_templates_ignores_an_on_request_environment_a_disabled_entry_names() {
    // The entry's enableIf is the scalar false, so Own is not requested:
    // neither its wrap hooks nor its literal host requirements count.
    let att = env_ok(&format!(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, EXPR]\n\
         environment:\n  name: Att\n  {WRAP_HOOKS}\n"
    ));
    let wrap = job_ok(&format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, EXPR]\n\
         name: J\njobEnvironments:\n- name: Own\n  activation: ON_REQUEST\n  {WRAP_HOOKS}\n\
         steps:\n- name: S\n  dependencies: [{{environment: Own, enableIf: false}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    ));
    assert_eq!(
        check_environment_templates(
            &wrap,
            &[AttachedEnvironmentTemplate::new(&att)],
            &posix_ctx(&wrap)
        )
        .map_err(|e| e.to_string()),
        Ok(vec![])
    );
    let other = env_ok(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
         environment: {name: Other, variables: {V: x}}\n",
    );
    let gpu = job_ok(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         jobEnvironments:\n- name: Own\n  activation: ON_REQUEST\n  \
         hostRequirements: {amounts: [{name: amount.worker.gpu, min: 4}]}\n  variables: {V: x}\n\
         steps:\n- name: S\n  hostRequirements: {amounts: [{name: amount.worker.gpu, max: 1}]}\n  \
         dependencies: [{environment: Own, enableIf: false}]\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    assert_eq!(
        check_environment_templates(
            &gpu,
            &[AttachedEnvironmentTemplate::new(&other)],
            &posix_ctx(&gpu)
        )
        .map_err(|e| e.to_string()),
        Ok(vec![])
    );
}

#[test]
fn a_job_environments_enable_if_runs_under_the_caller_memory_budget() {
    let jt = job_ok(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         parameterDefinitions: [{name: N, type: INT, default: 1000}]\n\
         jobEnvironments:\n- {name: E, enableIf: \"len([1] * Param.N) > 0\", variables: {V: x}}\n\
         steps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n",
    );
    let values = job_params_with(&jt, &[], &[]);
    let ctx = posix_ctx(&jt).with_caller_limits(openjd_model::CallerLimits {
        max_eval_memory_bytes: Some(2000),
        ..Default::default()
    });
    let err = create_job_with_environment_templates(&jt, &[], &values, &ctx).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &[
            "jobEnvironments[0] -> enableIf:\n\tcould not be evaluated: Expression \
           memory usage (64384 bytes) exceeded limit (2000 bytes). Resolved for Step 'S'.",
        ],
    );
}

#[test]
fn a_step_environments_enable_if_error_is_reported_when_entry_scope_fails() {
    // The entry value's binding fails in WINDOWS form, and the Step
    // Environment's enableIf fails too: both are reported.
    let jt = job_ok(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         parameterDefinitions: [{name: S, type: STRING, default: x}]\n\
         jobEnvironments:\n- {name: R, activation: ON_REQUEST, \
         envParameterDefinitions: [{name: P, type: STRING}], variables: {V: x}}\n\
         steps:\n- name: S\n  let: [\"r = string(path('/a/b').relative_to('/a'))\"]\n  \
         dependencies: [{environment: R, parameters: {P: \"{{r}}\"}}]\n  \
         stepEnvironments:\n  - {name: SE, enableIf: \"int(Param.S) > 0\", variables: {V: x}}\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    let values = job_params_with(&jt, &[], &[]);
    let err =
        create_job_with_environment_templates(&jt, &[], &values, &windows_ctx(&jt)).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &[
            "steps[0] -> stepEnvironments[0] -> enableIf:\n\tcould not be evaluated: Cannot \
             convert 'x' to int\n  int(Param.S) > 0\n  ^~~~~~~~~~~~",
            WINDOWS_LET_ERROR,
        ],
    );
}

// ── Round 7 audit ───────────────────────────────────────────────────

#[test]
fn an_entry_whose_enable_if_fails_does_not_reevaluate_let_bindings() {
    // The entry's enableIf fails, so its value is not computed and the
    // binding only that value reads is not evaluated again in WINDOWS
    // form: the one mistake is reported once, with no `let[0]` error.
    let jt = job_ok(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         parameterDefinitions: [{name: I, type: INT, default: 5}]\n\
         jobEnvironments:\n- {name: R, activation: ON_REQUEST, \
         envParameterDefinitions: [{name: P, type: STRING, default: d}], variables: {V: x}}\n\
         steps:\n- name: S\n  let: [\"r = string(path('/a/b').relative_to('/a'))\"]\n  \
         dependencies: [{environment: R, enableIf: \"[1][Param.I] == 1\", parameters: {P: \"{{r}}\"}}]\n  \
         script: {actions: {onRun: {command: x}}}\n",
    );
    let values = job_params_with(&jt, &[], &[]);
    let err =
        create_job_with_environment_templates(&jt, &[], &values, &windows_ctx(&jt)).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &[
            "steps[0] -> dependencies[0] -> enableIf:\n\tcould not be evaluated: Index 5 out of \
           bounds for list of length 1\n  [1][Param.I] == 1\n  ~~~^~~~~~~~~",
        ],
    );
}

#[test]
fn a_duplicate_entrys_values_are_ignored() {
    // In a template edited after decoding, a second entry for E is
    // reported as a duplicate (constraint 3), and its value, which would
    // fail to convert, is not resolved.
    let mut jt = job_ok(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         parameterDefinitions: [{name: X, type: STRING, default: abc}]\n\
         jobEnvironments:\n- {name: E, envParameterDefinitions: [{name: N, type: INT, default: 0}], \
         variables: {V: x}}\n- {name: F, variables: {V: x}}\n\
         steps:\n- name: S\n  dependencies: [{environment: E, parameters: {N: 1}}, \
         {environment: F}]\n  script: {actions: {onRun: {command: x}}}\n",
    );
    let values = job_params_with(&jt, &[], &[]);
    let deps = jt.steps[0].dependencies.as_mut().unwrap();
    let mut bad = serde_json::from_value::<openjd_model::template::StepDependency>(
        serde_json::json!({"environment": "E", "parameters": {"N": "{{Param.X}}"}}),
    )
    .unwrap();
    std::mem::swap(&mut deps[1], &mut bad);
    let err =
        create_job_with_environment_templates(&jt, &[], &values, &posix_ctx(&jt)).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &["steps[0] -> dependencies[1] -> environment:\n\tduplicate environment dependency 'E'."],
    );
}
