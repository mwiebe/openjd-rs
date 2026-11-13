// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `ENV_PARAMS`: disabled Environments (Template Schemas §3.2.2), and
//! RANGE_EXPR Environment parameters (§4.5).
//!
//! An Environment is disabled for a Step when its own `enableIf` is `false` for
//! the Step, evaluated with the Step's concrete values: a Step's effective value
//! that converts to its type is used whether or not it meets its constraints,
//! and an `enableIf` that reads a missing or unconvertible one is not
//! evaluated, so it does not disable the Environment. A disabled Environment
//! is still held to constraint 7 for every Step it applies to, but the Step's
//! values for it are not checked against their type and constraints, though
//! constraint 5 still applies. Template validation and
//! `check_environment_templates` check constraint 6 only for an Environment
//! known not to be disabled: its `enableIf` is absent or known `true`, or it
//! reads a literal value that does not convert (§3.2.2 item 2, §7.6 rule 5).
//! Two RANGE_EXPR values are equal when they denote the same integers.

use openjd_model::template::EnvironmentTemplate;
use openjd_model::{
    check_environment_templates, create_job_with_environment_templates,
    AttachedEnvironmentTemplate, CreatedJob, ModelError,
};

use crate::env_params_common::{
    assert_errors, env_ok, job_err, job_ok, job_params_with, rendered_values, template_ctx,
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
    create_job_with_environment_templates(&jt, &attached, &values, &template_ctx(&jt))
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
fn check(jt_src: &str, env_srcs: &[&str]) -> Result<(), ModelError> {
    let jt = job_ok(jt_src);
    let envs: Vec<EnvironmentTemplate> = env_srcs.iter().map(|s| env_ok(s)).collect();
    let attached: Vec<AttachedEnvironmentTemplate<'_>> =
        envs.iter().map(AttachedEnvironmentTemplate::new).collect();
    check_environment_templates(&jt, &attached, &template_ctx(&jt)).map(|_| ())
}

// ── Template validation (§3.2 checked-at paragraph, §7.6 rule 5) ────

/// A Job Environment `E` with the given `enableIf` line (empty for none)
/// and parameters `Mode` (STRING, `allowedValues: [a, b]`), `N` (INT), and
/// `L` (LIST[STRING]), and a Step supplying `values`.
fn job_env_with(enable_if: &str, values: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         parameterDefinitions: [{{name: Use, type: STRING, default: 'no'}}]\n\
         jobEnvironments:\n- name: E\n{enable_if}  envParameterDefinitions:\n  \
         - {{name: Mode, type: STRING, allowedValues: [a, b], default: a}}\n  \
         - {{name: N, type: INT, default: 1}}\n  - {{name: L, type: 'LIST[STRING]', default: []}}\n  \
         variables: {{V: x}}\n\
         steps:\n- name: S\n  dependencies: [{{environment: E, parameters: {values}}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    )
}

const NOT_ALLOWED: &str = "steps[0] -> dependencies[0] -> parameters -> Mode:\n\tParameter \
                           'Mode': value 'c' is not in allowed values";

#[test]
fn a_literal_value_is_checked_only_for_an_environment_certainly_not_disabled() {
    // Absent or `true`: checked at template validation (constraint 6).
    for enable_if in ["", "  enableIf: true\n"] {
        job_err(&job_env_with(enable_if, "{Mode: c}"), &[NOT_ALLOWED]);
    }
    // `false`: disabled for every Step, so the value is not checked, at
    // template validation or when the Job is created.
    let disabled = job_env_with("  enableIf: false\n", "{Mode: c}");
    create_ok(&disabled, &[], &[]);
    // An expression: undetermined at template validation, so not reported
    // there; checked at job creation when the Environment is not disabled.
    let expr = job_env_with("  enableIf: \"Param.Use == 'yes'\"\n", "{Mode: c}");
    job_ok(&expr);
    create_ok(&expr, &[], &[]);
    create_err(&expr, &[], &[("Use", "yes")], "JobTemplate", &[NOT_ALLOWED]);
}

#[test]
fn a_format_string_values_type_is_checked_only_for_an_environment_certainly_not_disabled() {
    // The static type of a single expression is checked against the
    // parameter's type (pass 8), and a LIST value must be one expression;
    // both are checks of the value's type, so neither applies to an
    // Environment that is disabled or undetermined.
    let values = "{N: \"{{ 'x' }}\", L: \"a{{Param.Use}}\"}";
    job_err(
        &job_env_with("", values),
        &[
            "steps[0] -> dependencies[0] -> parameters -> N:\n\tFailed to parse interpolation \
             expression at [0, 9]. Cannot convert 'x' to int: invalid digit found in string\n  \
             'x'\n  ^~~",
            "steps[0] -> dependencies[0] -> parameters -> L:\n\ta LIST[STRING] parameter has no \
             string form, so a format-string value for it must be a single '{{ }}' expression.",
        ],
    );
    job_ok(&job_env_with("  enableIf: false\n", values));
    let expr = job_env_with("  enableIf: \"Param.Use == 'yes'\"\n", values);
    job_ok(&expr);
    create_ok(&expr, &[], &[]);
}

#[test]
fn constraint_5_and_the_value_forms_apply_to_a_disabled_environment() {
    // A key the Environment does not declare, and a value no type has, are
    // errors whatever the `enableIf` (§3.2.2 exempts only the checks of a
    // value against its parameter's type and constraints).
    job_err(
        &job_env_with("  enableIf: false\n", "{Other: 1, Mode: null}"),
        &[
            "steps[0] -> dependencies[0] -> parameters -> Mode:\n\tmust not be null: no \
             parameter type has a null value.",
            "steps[0] -> dependencies[0] -> parameters -> Other:\n\tEnvironment 'E' declares no \
             parameter 'Other'. Declared: Mode, N, L.",
        ],
    );
}

// ── Job creation (§3.2.2, §7.4.1) ──────────────────────────────────

#[test]
fn a_failed_resolved_default_is_reported_only_for_a_step_it_is_not_disabled_for() {
    // Seats's default resolves to `many`, which does not convert to INT.
    // Lic's enableIf reads Env.Param.Mode, which B supplies `on` and A takes
    // as `off`: A is disabled, so its default is not checked; B is not, and
    // the error names B.
    let env = "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
               parameterDefinitions: [{name: S, type: STRING, default: many}]\n\
               environment:\n  name: Lic\n  enableIf: \"Env.Param.Mode == 'on'\"\n  \
               envParameterDefinitions:\n  - {name: Mode, type: STRING, default: 'off'}\n  \
               - {name: Seats, type: INT, default: \"{{Param.S}}\"}\n  variables: {V: x}\n";
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              steps:\n- name: A\n  script: {actions: {onRun: {command: x}}}\n\
              - name: B\n  dependencies: [{environment: Lic, parameters: {Mode: 'on'}}]\n  \
              script: {actions: {onRun: {command: x}}}\n";
    let failed = "EnvironmentTemplate[0] -> environment -> envParameterDefinitions[1] -> \
                  default:\n\tfor Step 'B', which does not supply this parameter: could not be \
                  resolved as a value of type INT: Cannot convert 'many' to int: invalid digit \
                  found in string\n  Param.S\n  ~~~~~~^";
    create_err(jt, &[env], &[], "Submission", &[failed]);
    // With a default that converts, A is disabled and B enters Lic.
    let created = create_ok(jt, &[env], &[("S", "3")]);
    assert_eq!(created.job.steps[0].environments, Some(vec![]));
    assert_eq!(created.job.steps[1].environments, None);
}

#[test]
fn a_failed_enable_if_is_reported_once_and_the_environment_is_still_checked() {
    // Both Steps supply the same P, so the enableIf sees the same values and
    // is evaluated once; its error names both Steps. Which further errors
    // to report is an implementation choice; openjd-rs checks the
    // Environment as if it is not disabled, so each Step's own mistake (Q
    // below its minimum) is reported in the same run.
    let jt = |p: &str| {
        format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             parameterDefinitions: [{{name: L, type: 'LIST[INT]', default: [1]}}]\n\
             jobEnvironments:\n- name: E\n  enableIf: \"Param.L[5] == 1 or Env.Param.P != ''\"\n  \
             envParameterDefinitions: [{{name: P, type: STRING}}, {{name: Q, type: INT, \
             minValue: 5, default: 5}}]\n  variables: {{V: x}}\n\
             steps:\n- name: A\n  dependencies: [{{environment: E, parameters: {{Q: 1{p}}}}}]\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n\
             - name: B\n  dependencies: [{{environment: E, parameters: {{Q: 1{p}}}}}]\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    // Without P, template validation reports constraint 7 for both Steps,
    // whatever E's enableIf is (§3.2 constraint 7).
    let missing = |step: usize| {
        format!(
            "steps[{step}]:\n\tmust supply, in an 'environment:' dependency, every parameter \
             without a default of each Environment that applies to it (every jobEnvironments entry with activation ALWAYS, each one with \
             activation BY_DEFAULT that the Step does not decline, and each one \
             with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7), whatever that Environment's own enableIf is; missing: 'P' of Environment 'E'."
        )
    };
    job_err(&jt(""), &[&missing(0), &missing(1)]);
    // With P, template validation reports nothing: the enableIf reads a Job
    // Parameter, so it is undetermined, and the value checks are left to
    // job creation.
    let jt = jt(", P: p");
    job_ok(&jt);
    create_err(
        &jt,
        &[],
        &[],
        "JobTemplate",
        &[
            "jobEnvironments[0] -> enableIf:\n\tcould not be evaluated: Index 5 out of bounds for \
           list of length 1. Resolved for Steps 'A' and 'B'.\n  Param.L[5] == 1 or Env.Param.P \
           != ''\n  ~~~~~~~^~~",
            "steps[0] -> dependencies[0] -> parameters -> Q:\n\tParameter 'Q': value 1 is less \
             than minimum 5",
            "steps[1] -> dependencies[0] -> parameters -> Q:\n\tParameter 'Q': value 1 is less \
             than minimum 5",
        ],
    );
}

#[test]
fn an_environments_enable_if_reads_raw_path_job_parameters() {
    // §7.3.2: `RawParam.*` of any type, PATH included, with
    // the unmapped value.
    let jt = |document: &str| {
        format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             parameterDefinitions: [{{name: Dir, type: PATH}}]\n\
             jobEnvironments:\n- name: E\n  enableIf: \"RawParam.Dir == '{document}'\"\n  \
             hostRequirements:\n    attributes: [{{name: attr.custom.dir, anyOf: \
             [\"{{{{ 'd' + string(len(RawParam.Dir)) }}}}\"]}}]\n  variables: {{V: x}}\n\
             steps:\n- name: S\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    let dir = [("Dir", "/data")];
    let created = create_ok(&jt("/data"), &[], &dir);
    assert_eq!(created.job.steps[0].environments, None);
    let attrs = created.job.steps[0]
        .host_requirements
        .as_ref()
        .and_then(|h| h.attributes.clone())
        .unwrap();
    assert_eq!(attrs[0].any_of, Some(vec!["d5".to_string()]));
    let created = create_ok(&jt("/other"), &[], &dir);
    assert_eq!(created.job.steps[0].environments, Some(vec![]));
    assert_eq!(created.job.steps[0].host_requirements, None);
}

#[test]
fn check_environment_templates_checks_values_only_for_an_attachment_certainly_not_disabled() {
    let env = |enable_if: &str| {
        format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
             environment:\n  name: Lic\n  enableIf: {enable_if}\n  envParameterDefinitions:\n  \
             - {{name: Mode, type: STRING, allowedValues: [a, b], default: a}}\n  \
             - {{name: P, type: STRING}}\n  variables: {{V: x}}\n"
        )
    };
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              steps:\n- name: S\n  dependencies: [{environment: Lic, parameters: {Mode: c, X: 1}}]\n  \
              script: {actions: {onRun: {command: x}}}\n";
    let undeclared =
        "JobTemplate -> steps[0] -> dependencies[0] -> parameters -> X:\n\tEnvironment \
                      'Lic' (EnvironmentTemplate[0]) declares no parameter 'X'. Declared: Mode, P.";
    let not_allowed = "JobTemplate -> steps[0] -> dependencies[0] -> parameters -> Mode:\n\t\
                       Parameter 'Mode': value 'c' is not in allowed values";
    let missing =
        "JobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every \
                   parameter without a default of each Environment that applies to it (every attached Environment with activation ALWAYS, each one with \
                   activation BY_DEFAULT that the Step does not decline, and each one \
                   with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1), whatever that Environment's own enableIf is; missing: 'P' of \
                   Environment 'Lic' (EnvironmentTemplate[0]).";
    // `true`, or the constant expression `"true"`, which is known before
    // job creation (§7.6 rule 5): every check.
    for enable_if in ["true", "\"true\""] {
        let err = check(jt, &[&env(enable_if)]).unwrap_err();
        assert_errors(
            &err.to_string(),
            "Submission",
            &[undeclared, not_allowed, missing],
        );
    }
    // `false`, or an expression over `Job.Name`, unknown before job
    // creation: constraint 5, and constraint 7, which holds whatever Lic's
    // enableIf is (§3.2 constraint 7), but not the value checks.
    for enable_if in ["false", "\"Job.Name == 'J'\""] {
        let err = check(jt, &[&env(enable_if)]).unwrap_err();
        assert_errors(&err.to_string(), "Submission", &[undeclared, missing]);
    }
    // Job creation: `false` disables Lic, so constraints 5 and 7 only; the
    // expression is `true`, so every check.
    create_err(
        jt,
        &[&env("false")],
        &[],
        "Submission",
        &[undeclared, missing],
    );
    create_err(
        jt,
        &[&env("\"Job.Name == 'J'\"")],
        &[],
        "Submission",
        &[undeclared, not_allowed, missing],
    );
}

// ── RANGE_EXPR (§4.5, §3.2.2) ──────────────────────────────────────

const RANGES: &str = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
                      name: J\njobEnvironments:\n- name: Cache\n  envParameterDefinitions: \
                      [{name: Frames, type: RANGE_EXPR, default: '1-10'}]\n  enableIf: \
                      \"len(Env.Param.Frames) > 3\"\n  variables: {F: \"{{Env.Param.Frames}}\"}\n\
                      steps:\n\
                      - name: A\n  dependencies: [{environment: Cache, parameters: {Frames: '1-3'}}]\n  \
                      script: {actions: {onRun: {command: x}}}\n\
                      - name: B\n  dependencies: [{environment: Cache, parameters: {Frames: '1,3,5,7,9'}}]\n  \
                      script: {actions: {onRun: {command: x}}}\n\
                      - name: C\n  dependencies: [{environment: Cache, parameters: {Frames: \"{{ '1-10:2' }}\"}}]\n  \
                      script: {actions: {onRun: {command: x}}}\n\
                      - name: D\n  script: {actions: {onRun: {command: x}}}\n";

#[test]
fn a_range_expr_value_is_its_canonical_form_and_compared_by_value() {
    // §3.2.2: two RANGE_EXPR values are equal when they have the same
    // canonical form (§3.4.1.1.1), however they were written: a literal, a
    // format string's resolved string, or a single expression, with or
    // without white space.
    let jt = RANGES.replace(
        "- name: D\n",
        "- name: E\n  dependencies: [{environment: Cache, parameters: {Frames: '9,7,5,3,1'}}]\n  \
         script: {actions: {onRun: {command: x}}}\n\
         - name: F\n  dependencies: [{environment: Cache, parameters: {Frames: ' 1,3,5,7,9 '}}]\n  \
         script: {actions: {onRun: {command: x}}}\n\
         - name: G\n  dependencies: [{environment: Cache, parameters: {Frames: \"{{ Param.R }}\"}}]\n  \
         script: {actions: {onRun: {command: x}}}\n\
         - name: H\n  dependencies: [{environment: Cache, parameters: {Frames: \"{{ 1 }},3,5,7,9\"}}]\n  \
         script: {actions: {onRun: {command: x}}}\n\
         - name: I\n  dependencies: [{environment: Cache, parameters: {Frames: \"{{ Param.N }},2,3,4\"}}]\n  \
         script: {actions: {onRun: {command: x}}}\n\
         - name: K\n  dependencies: [{environment: Cache, parameters: {Frames: \"{{ Param.S }}\"}}]\n  \
         script: {actions: {onRun: {command: x}}}\n\
         - name: L\n  dependencies: [{environment: Cache, parameters: {Frames: '1-4'}}]\n  \
         script: {actions: {onRun: {command: x}}}\n\
         - name: D\n",
    )
    .replace(
        "name: J\n",
        "name: J\nparameterDefinitions:\n- {name: R, type: RANGE_EXPR, default: '9,7,5,3,1'}\n\
         - {name: N, type: INT, default: 1}\n- {name: S, type: STRING, default: '1,2,3,4'}\n",
    );
    let created = create_ok(&jt, &[], &[]);
    let job = &created.job;
    // A's three frames: the enableIf reads the range_expr and is false.
    assert_eq!(job.steps[0].environments, Some(vec![]));
    let [b, c, e, f, g, h, i, k, l, d] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10].map(|n| &job.steps[n]);
    // B, C, E, F, G, and H all denote 1, 3, 5, 7, 9.
    for step in [b, c, e, f, g, h] {
        assert_eq!(
            rendered_values(step),
            ["JobTemplate/Cache: Frames=range_expr:1-9:2"],
            "{}",
            step.name
        );
        assert!(b.environment_values_equal(step), "{}", step.name);
    }
    // I, K, and L denote 1 to 4.
    for step in [i, k, l] {
        assert_eq!(
            rendered_values(step),
            ["JobTemplate/Cache: Frames=range_expr:1-4"],
            "{}",
            step.name
        );
        assert!(i.environment_values_equal(step), "{}", step.name);
    }
    assert!(!b.environment_values_equal(i));
    // D takes the default.
    assert_eq!(
        rendered_values(d),
        ["JobTemplate/Cache: Frames=range_expr:1-10"]
    );
    assert!(!b.environment_values_equal(d));
    assert!(b.same_environments(c));
}

#[test]
fn an_int_value_compares_as_resolved() {
    // An INT value written `0{{ Param.N }}` resolves to the INT 1 (§3.2.2:
    // values are equal when they resolve the same), so it equals a literal
    // `1`.
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              parameterDefinitions: [{name: N, type: INT, default: 1}]\n\
              jobEnvironments:\n- name: E\n  envParameterDefinitions: [{name: I, type: INT}]\n  \
              variables: {V: \"{{Env.Param.I}}\"}\n\
              steps:\n\
              - name: A\n  dependencies: [{environment: E, parameters: {I: \"0{{ Param.N }}\"}}]\n  \
              script: {actions: {onRun: {command: x}}}\n\
              - name: B\n  dependencies: [{environment: E, parameters: {I: 1}}]\n  \
              script: {actions: {onRun: {command: x}}}\n";
    let created = create_ok(jt, &[], &[]);
    let [a, b] = [0, 1].map(|n| &created.job.steps[n]);
    assert_eq!(rendered_values(a), ["JobTemplate/E: I=int:1"]);
    assert!(a.environment_values_equal(b));
}

#[test]
fn a_range_expr_default_is_its_canonical_form() {
    // A `default`, literal or format string, is stored in canonical form too
    // (§3.2.2), so `1,2,3` equals `1-3` and `07` equals `7`. Other types
    // keep their transport form.
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              parameterDefinitions: [{name: N, type: INT, default: 1}]\n\
              jobEnvironments:\n- name: E\n  envParameterDefinitions:\n  \
              - {name: R, type: RANGE_EXPR, default: '1,2,3'}\n  \
              - {name: S, type: RANGE_EXPR, default: \"{{ Param.N }},2,3,4\"}\n  \
              - {name: I, type: INT, default: 5}\n  \
              variables: {V: \"{{Env.Param.R}}\"}\n\
              steps:\n\
              - name: A\n  script: {actions: {onRun: {command: x}}}\n\
              - name: B\n  dependencies: [{environment: E, parameters: {R: '1-3', S: '1-4'}}]\n  \
              script: {actions: {onRun: {command: x}}}\n\
              - name: C\n  dependencies: [{environment: E, parameters: {R: '7'}}]\n  \
              script: {actions: {onRun: {command: x}}}\n\
              - name: D\n  dependencies: [{environment: E, parameters: {R: '07'}}]\n  \
              script: {actions: {onRun: {command: x}}}\n";
    let created = create_ok(jt, &[], &[]);
    let [a, b, c, d] = [0, 1, 2, 3].map(|n| &created.job.steps[n]);
    for step in [a, b] {
        assert_eq!(
            rendered_values(step),
            ["JobTemplate/E: I=int:5, R=range_expr:1-3, S=range_expr:1-4"],
            "{}",
            step.name
        );
    }
    assert!(a.environment_values_equal(b));
    assert_eq!(
        rendered_values(d),
        ["JobTemplate/E: I=int:5, R=range_expr:7, S=range_expr:1-4"]
    );
    assert!(c.environment_values_equal(d));
}

#[test]
fn range_expr_spellings_share_an_enable_if_and_a_host_requirements_resolution() {
    // `1-3` and `1,2,3` are equal values (§3.2.2), so the two Steps share
    // one `enableIf` evaluation and one `hostRequirements` resolution, and
    // each error names both.
    let jt = |enable_if: &str, min: &str| {
        format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR, FEATURE_BUNDLE_1]\n\
             name: J\njobEnvironments:\n- name: E\n  envParameterDefinitions: [{{name: F, type: RANGE_EXPR}}]\n  \
             enableIf: \"{enable_if}\"\n  hostRequirements:\n    amounts: [{{name: amount.custom, min: \"{min}\"}}]\n  \
             variables: {{V: x}}\n\
             steps:\n\
             - name: A\n  dependencies: [{{environment: E, parameters: {{F: '1-3'}}}}]\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n\
             - name: B\n  dependencies: [{{environment: E, parameters: {{F: '1,2,3'}}}}]\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    create_err(
        &jt("Env.Param.F[10] == 1", "1"),
        &[],
        &[],
        "JobTemplate",
        &["jobEnvironments[0] -> enableIf:\n\tcould not be evaluated: Index 10 out of bounds for \
           range_expr of length 3. Resolved for Steps 'A' and 'B'.\n  Env.Param.F[10] == 1\n  \
           ~~~~~~~~~~~^~~~"],
    );
    create_err(
        &jt("true", "{{ Env.Param.F[10] }}"),
        &[],
        &[],
        "JobTemplate",
        &["jobEnvironments[0] -> hostRequirements -> amounts[0] -> min:\n\tFormat \
          string error: hostRequirements amount min: Index 10 out of bounds for range_expr of length \
          3. Resolved for Steps 'A' and 'B'.\n  Env.Param.F[10]\n  ~~~~~~~~~~~^~~~"],
    );
}

#[test]
fn an_expression_valued_range_expr_is_its_canonical_form() {
    // A single expression whose result is a string converts to its target
    // type, range_expr, and the value every reader sees is that range_expr:
    // the Environment's enableIf and hostRequirements read its canonical
    // form, `13-19`, as for a literal that denotes the same integers.
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              parameterDefinitions: [{name: S, type: STRING, default: '19-18:-5,13-18'}]\n\
              jobEnvironments:\n- name: E\n  envParameterDefinitions: [{name: R, type: RANGE_EXPR}]\n  \
              enableIf: \"string(Env.Param.R) == '13-19'\"\n  hostRequirements:\n    \
              attributes: [{name: attr.custom.r, allOf: [\"r{{ string(Env.Param.R) }}\"]}]\n  \
              variables: {V: x}\n\
              steps:\n\
              - name: X\n  dependencies: [{environment: E, parameters: {R: \"{{ Param.S }}\"}}]\n  \
              script: {actions: {onRun: {command: x}}}\n\
              - name: L\n  dependencies: [{environment: E, parameters: {R: '13-18,19'}}]\n  \
              script: {actions: {onRun: {command: x}}}\n";
    let created = create_ok(jt, &[], &[]);
    let [x, l] = [0, 1].map(|n| &created.job.steps[n]);
    assert_eq!(rendered_values(x), ["JobTemplate/E: R=range_expr:13-19"]);
    assert!(x.environment_values_equal(l));
    for step in [x, l] {
        assert_eq!(step.environments, None, "{}", step.name);
        let attrs = step
            .host_requirements
            .as_ref()
            .and_then(|h| h.attributes.clone())
            .unwrap();
        assert_eq!(
            attrs[0].all_of,
            Some(vec!["r13-19".to_string()]),
            "{}",
            step.name
        );
    }
}

#[test]
fn a_computed_range_expr_value_meets_its_length_constraints_as_written_and_canonically() {
    // §4.5 item 4 and §2.10: a format string's RANGE_EXPR value, or a single
    // expression's string result, is checked as written and in canonical
    // form; a single expression whose result is a range_expr has only its
    // canonical form. A result that does not parse, such as the empty
    // range_expr of a slice, is rejected.
    let jt = |r: &str, constraint: &str| {
        format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             parameterDefinitions:\n- {{name: S, type: STRING, default: '1,3,5,7,9'}}\n\
             - {{name: P, type: RANGE_EXPR, default: '1,3,5,7,9'}}\n\
             - {{name: One, type: RANGE_EXPR, default: '5'}}\n- {{name: N, type: INT, default: 1}}\n\
             jobEnvironments:\n- name: E\n  envParameterDefinitions: [{{name: R, type: RANGE_EXPR, \
             {constraint}}}]\n  variables: {{V: x}}\n\
             steps:\n- name: A\n  dependencies: [{{environment: E, parameters: {{R: \"{r}\"}}}}]\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    let at = "steps[0] -> dependencies[0] -> parameters -> R:\n\t";
    // A literal is checked as written at template validation: `1,2,3` is 5
    // characters, although its canonical form `1-3` fits maxLength 3.
    job_err(
        &jt("1,2,3", "maxLength: 3"),
        &[&format!(
            "{at}Parameter 'R': value '1,2,3' length 5 exceeds maximum 3"
        )],
    );
    job_ok(&jt("1-3", "maxLength: 3"));
    // And in canonical form, as an Environment parameter's always is
    // (§4.5 item 4): `1-3:2` is 5 characters, but its canonical form `1,3`
    // is below minLength 4.
    job_err(
        &jt("1-3:2", "minLength: 4"),
        &[&format!(
            "{at}Parameter 'R': canonical form '1,3' length 3 is less than minimum 4"
        )],
    );
    // A string result, from a single expression or a format string, is
    // checked as written: `1,3,5,7,9` is 9 characters.
    for (r, written) in [
        ("{{ Param.S }}", "1,3,5,7,9"),
        ("{{ Param.S }} ", "1,3,5,7,9 "),
    ] {
        let len = written.len();
        create_err(
            &jt(r, "maxLength: 8"),
            &[],
            &[],
            "JobTemplate",
            &[&format!(
                "{at}Parameter 'R': value '{written}' length {len} exceeds maximum 8"
            )],
        );
    }
    // A string result is checked in canonical form too: `1,3,5,7,9` fits
    // minLength 6 as written, but its canonical form `1-9:2` does not.
    create_err(
        &jt("{{ Param.S }}", "minLength: 6"),
        &[],
        &[],
        "JobTemplate",
        &[&format!(
            "{at}Parameter 'R': canonical form '1-9:2' length 5 is less than minimum 6"
        )],
    );
    // A range_expr result has only its canonical form, `1-9:2`, 5
    // characters: within maxLength 8, below minLength 6.
    create_ok(&jt("{{ Param.P }}", "maxLength: 8"), &[], &[]);
    create_err(
        &jt("{{ Param.P }}", "minLength: 6"),
        &[],
        &[],
        "JobTemplate",
        &[&format!(
            "{at}Parameter 'R': canonical form '1-9:2' length 5 is less than minimum 6"
        )],
    );
    // A format string written `1,2,3` (5 characters) breaks maxLength 3,
    // although its canonical form `1-3` fits.
    create_err(
        &jt("{{ Param.N }},2,3", "maxLength: 3"),
        &[],
        &[],
        "JobTemplate",
        &[&format!(
            "{at}Parameter 'R': value '1,2,3' length 5 exceeds maximum 3"
        )],
    );
    let created = create_ok(&jt("{{ Param.N }},2,3", "maxLength: 5"), &[], &[]);
    assert_eq!(
        rendered_values(&created.job.steps[0]),
        ["JobTemplate/E: R=range_expr:1-3"]
    );
    create_err(
        &jt("{{ Param.One[1:] }}", "minLength: 1"),
        &[],
        &[],
        "JobTemplate",
        &[&format!(
            "{at}could not be resolved as a value of type RANGE_EXPR: Value '' is not a valid \
             range expression: Empty expression"
        )],
    );
}

#[test]
fn an_enable_if_reads_a_range_expr_literal_in_canonical_form() {
    // `19-18:-5,13-18` denotes 13 to 19, canonical form `13-19`. Job
    // creation reads that range_expr, so the enableIf is `false`: E is
    // disabled, and the INT N it does not read, which does not convert, is
    // not checked (§7.4.1 step 6). Template validation leaves the enableIf,
    // which reads R and U, to job creation (§7.6 rule 5), and does not
    // check N either. U is supplied, since E applies to X whatever its
    // enableIf is (§3.2 constraint 7).
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              jobEnvironments:\n- name: E\n  \
              enableIf: \"string(Env.Param.R) != '13-19' and Env.Param.U == 1\"\n  \
              envParameterDefinitions:\n  - {name: R, type: RANGE_EXPR}\n  \
              - {name: U, type: INT}\n  - {name: N, type: INT}\n  variables: {V: x}\n\
              steps:\n- name: X\n  dependencies: [{environment: E, parameters: \
              {R: '19-18:-5,13-18', U: 1, N: 'x'}}]\n  script: {actions: {onRun: {command: x}}}\n";
    job_ok(jt);
    assert_eq!(
        create_ok(jt, &[], &[]).job.steps[0].environments,
        Some(vec![])
    );
}

#[test]
fn a_range_expr_value_that_does_not_parse_is_rejected() {
    let bad = "is not a valid RANGE_EXPR value: Value '1-' is not a valid range expression: \
               Unexpected end of expression: '1-'";
    let at = format!("steps[1] -> dependencies[0] -> parameters -> Frames:\n\t{bad}");
    // Cache's enableIf reads Frames, which does not convert for B, so it is
    // not evaluated for B and Cache is not disabled for B: template
    // validation rejects the literal (§3.2.2 item 2).
    let jt = RANGES.replace("'1,3,5,7,9'", "'1-'");
    job_err(&jt, &[&at]);
    // So it does without the enableIf.
    job_err(
        &jt.replace("  enableIf: \"len(Env.Param.Frames) > 3\"\n", ""),
        &[&at],
    );
    // An enableIf that also reads Job.Name still reads Frames, so Cache is
    // certainly not disabled for B either, and template validation rejects
    // the literal (§3.2.2 item 2).
    let jt = jt.replace(
        "\"len(Env.Param.Frames) > 3\"",
        "\"len(Env.Param.Frames) > 3 and Job.Name != ''\"",
    );
    job_err(&jt, &[&at]);
    // A format string is resolved at job creation.
    create_err(
        &RANGES.replace("'1,3,5,7,9'", "\"{{ '1-' }}\""),
        &[],
        &[],
        "JobTemplate",
        &[
            "steps[1] -> dependencies[0] -> parameters -> Frames:\n\tcould not be resolved as a \
           value of type RANGE_EXPR: Cannot convert '1-' to range_expr: Unexpected end of \
           expression: '1-'\n  '1-'\n  ^~~~",
        ],
    );
}

// ── Audit round 1 ───────────────────────────────────────────────────

#[test]
fn an_unreachable_attachment_needs_its_values_whatever_its_enable_if() {
    // §3.2.1 items 3 and 4: no entry can reach an attachment named like a
    // Job Environment or like another attachment. With `activation:
    // BY_DEFAULT` (here, by omission) and a parameter without a default it
    // rejects the submission whenever it applies to a Step that is not
    // stopped, whatever its own `enableIf` (§3.2 constraint 7). An `ALWAYS`
    // one is covered in test_env_params_activation.
    let att = |enable_if: &str| {
        format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
             environment:\n  name: Gpu\n{enable_if}  envParameterDefinitions: [{{name: X, type: \
             STRING}}]\n  variables: {{V: x}}\n"
        )
    };
    let shadowed = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
                    name: J\njobEnvironments: [{name: Gpu, variables: {V: x}}]\nsteps:\n\
                    - name: S\n  script: {actions: {onRun: {command: x}}}\n";
    let plain = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
                 name: J\nsteps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n";
    let item_4 = |entered: &str| {
        format!(
            "EnvironmentTemplate[0] -> environment -> envParameterDefinitions:\n\tEnvironment \
             'Gpu' has the same name as a jobEnvironments entry of the Job Template, so no \
             'environment:' dependency can reach it (Template Schemas §3.2.1 item 4), and it has \
             parameters without a default that no Step can supply: X. {entered}, and the \
             submission is rejected (§3.2.1)."
        )
    };
    let item_3 = |doc: usize, entered: &str| {
        format!(
            "EnvironmentTemplate[{doc}] -> environment -> envParameterDefinitions:\n\t\
             Environment 'Gpu' is defined by 2 attached Environment Templates \
             (EnvironmentTemplate[0], EnvironmentTemplate[1]), so no 'environment:' dependency \
             can reach it (Template Schemas §3.2.1 item 3), and it has parameters without a \
             default that no Step can supply: X. {entered}, and the submission is rejected \
             (§3.2.1)."
        )
    };
    const EVERY: &str = "Its activation is BY_DEFAULT, so every Session enters it";
    const WHATEVER: &str = "Its activation is BY_DEFAULT, so it applies to every Step whatever \
                            its enableIf and exclusiveGroup (§3.2 constraint 7)";
    // Without an enableIf, and with `false`, an expression that is `false`
    // or `true` for S, or one that reads X, which no Step can supply:
    // rejected, by check and by job creation.
    for (enable_if, entered) in [
        ("", EVERY),
        ("  enableIf: false\n", WHATEVER),
        ("  enableIf: \"Job.Name == 'K'\"\n", WHATEVER),
        ("  enableIf: \"Job.Name == 'J'\"\n", WHATEVER),
        ("  enableIf: \"Env.Param.X != ''\"\n", WHATEVER),
    ] {
        let env = att(enable_if);
        let err = check(shadowed, &[&env]).unwrap_err();
        assert_errors(&err.to_string(), "Submission", &[&item_4(entered)]);
        create_err(shadowed, &[&env], &[], "Submission", &[&item_4(entered)]);
        create_err(
            plain,
            &[&env, &env],
            &[],
            "Submission",
            &[&item_3(0, entered), &item_3(1, entered)],
        );
    }
}

#[test]
fn an_attachments_failed_enable_if_is_reported_once_and_the_attachment_is_still_checked() {
    // The error names the Steps with the same known values. The attachment
    // is checked as if it is not disabled, so the Steps' other mistakes are
    // reported in the same run: M's default does not convert, and A is a
    // second wrapping layer under the single-layer rule.
    let att = "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, \
               EXPR]\nparameterDefinitions: [{name: S, type: STRING, default: many}]\nenvironment:\n  \
               name: A\n  envParameterDefinitions: [{name: N, type: INT, default: -1}, {name: M, \
               type: INT, default: \"{{Param.S}}\"}]\n  enableIf: \"None if Env.Param.N < 0 else \
               true\"\n  \
               script:\n    actions:\n      onWrapEnvEnter: {command: x}\n      onWrapTaskRun: \
               {command: x}\n      onWrapEnvExit: {command: x}\n";
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, EXPR]\n\
              name: J\njobEnvironments:\n- name: Outer\n  script:\n    actions:\n      \
              onWrapEnvEnter: {command: x}\n      onWrapTaskRun: {command: x}\n      onWrapEnvExit: \
              {command: x}\nsteps:\n- name: B\n  script: {actions: {onRun: {command: x}}}\n\
              - name: C\n  script: {actions: {onRun: {command: x}}}\n";
    let layers = |step: usize| {
        format!(
            "JobTemplate -> steps[{step}]:\n\tits Sessions would enter 2 Environments that define \
             wrap hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'A' \
             (EnvironmentTemplate[0]), 'Outer'. A Session may have at most one Environment \
             defining wrap hooks (RFC 0008)."
        )
    };
    create_err(
        jt,
        &[att],
        &[],
        "Submission",
        &[
            "EnvironmentTemplate[0] -> environment -> envParameterDefinitions[1] -> default:\n\t\
             for Step 'B', which does not supply this parameter: could not be resolved as a value \
             of type INT: Cannot convert 'many' to int: invalid digit found in string",
            "EnvironmentTemplate[0] -> environment -> enableIf:\n\tresolved to null, which does not \
           fit type bool (Template Schemas §7.6). Resolved for Steps 'B' and 'C'.",
            &layers(0),
            &layers(1),
        ],
    );
}

#[test]
fn an_attachments_failed_enable_if_still_reports_missing_parameters_and_host_conflicts() {
    // Checked as if not disabled: A's parameter P, which no Step supplies,
    // is missing (with the note); and, where every value is present, A's
    // host requirements conflict with the Step's. Each is reported once,
    // beside the enableIf error.
    let att = |p: &str| {
        format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
             environment:\n  name: A\n  envParameterDefinitions: [{p}{{name: N, type: INT, \
             default: -1}}]\n  enableIf: \"None if Env.Param.N < 0 else true\"\n  \
             hostRequirements:\n    amounts: [{{name: amount.worker.vcpu, min: 8}}]\n  \
             variables: {{V: x}}\n"
        )
    };
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              steps:\n- name: S\n  hostRequirements:\n    amounts: [{name: amount.worker.vcpu, \
              max: 4}]\n  script: {actions: {onRun: {command: x}}}\n";
    let enable_if = "EnvironmentTemplate[0] -> environment -> enableIf:\n\tresolved to null, \
                     which does not fit type bool (Template Schemas §7.6). Resolved for Step 'S'.";
    create_err(
        jt,
        &[&att("{name: P, type: STRING}, ")],
        &[],
        "Submission",
        &[
            "JobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every \
             parameter without a default of each Environment that applies to it (every attached Environment with activation ALWAYS, each one with \
             activation BY_DEFAULT that the Step does not decline, and each one \
             with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1), whatever that Environment's own enableIf is; missing: 'P' of Environment 'A' \
             (EnvironmentTemplate[0]).",
            enable_if,
        ],
    );
    create_err(
        jt,
        &[&att("")],
        &[],
        "Submission",
        &[
            enable_if,
            "JobTemplate -> steps[0]:\n\tStep 'S': no host can satisfy the combined host \
             requirements for amount 'amount.worker.vcpu': EnvironmentTemplate[0] -> environment \
             -> hostRequirements (Environment 'A') requires min 8, and JobTemplate -> steps[0] \
             -> hostRequirements requires max 4. Template Schemas §3.3.3.",
        ],
    );
}

#[test]
fn an_unreachable_attachment_whose_enable_if_fails_is_reported_with_both_errors() {
    // A has a Job Environment's name, so no entry can supply P. It applies
    // to S whatever its enableIf (§3.2 constraint 7), so its unreachable
    // parameter is reported once, and its enableIf, which reads only N,
    // a value that converts, is evaluated for S and fails, which is
    // reported too (§7.4.1 step 6).
    let att = "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
               environment:\n  name: Gpu\n  envParameterDefinitions: [{name: P, type: STRING}, \
               {name: N, type: INT, default: -1}]\n  enableIf: \"None if Env.Param.N < 0 else \
               true\"\n  variables: {V: x}\n";
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              jobEnvironments:\n- {name: Gpu, variables: {V: x}}\n\
              steps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n";
    create_err(
        jt,
        &[att],
        &[],
        "Submission",
        &[
            "EnvironmentTemplate[0] -> environment -> enableIf:\n\tresolved to null, which does \
             not fit type bool (Template Schemas §7.6). Resolved for Step 'S'.",
            "EnvironmentTemplate[0] -> environment -> envParameterDefinitions:\n\tEnvironment \
             'Gpu' has the same name as a jobEnvironments entry of the Job Template, so no \
             'environment:' dependency can reach it (Template Schemas §3.2.1 item 4), and it has \
             parameters without a default that no Step can supply: P. Its activation is BY_DEFAULT, \
             so it applies to every Step whatever its enableIf and exclusiveGroup (§3.2 \
             constraint 7), and the submission is rejected (§3.2.1).",
        ],
    );
}

#[test]
fn an_attachment_whose_enable_if_reads_a_missing_value_is_not_evaluated_and_entered() {
    // A's enableIf reads N, which no Step supplies, so it is not evaluated
    // (§3.2.2 item 2) and A is not disabled. A is held to constraint 7,
    // which it would be whatever its enableIf, and it is a layer for the
    // single-layer rule.
    let att = "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, \
               EXPR]\nenvironment:\n  name: A\n  envParameterDefinitions: [{name: N, type: \
               INT}]\n  enableIf: \"Env.Param.N > 0\"\n  script:\n    actions:\n      \
               onWrapEnvEnter: {command: x}\n      onWrapTaskRun: {command: x}\n      \
               onWrapEnvExit: {command: x}\n";
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS, EXPR]\n\
              name: J\njobEnvironments:\n- name: Outer\n  script:\n    actions:\n      \
              onWrapEnvEnter: {command: x}\n      onWrapTaskRun: {command: x}\n      onWrapEnvExit: \
              {command: x}\nsteps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n";
    create_err(
        jt,
        &[att],
        &[],
        "Submission",
        &[
            "JobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every \
             parameter without a default of each Environment that applies to it (every attached Environment with activation ALWAYS, each one with \
             activation BY_DEFAULT that the Step does not decline, and each one \
             with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1), whatever that Environment's own enableIf is; missing: 'N' of Environment 'A' \
             (EnvironmentTemplate[0]).",
            "JobTemplate -> steps[0]:\n\tits Sessions would enter 2 Environments that define \
             wrap hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'A' \
             (EnvironmentTemplate[0]), 'Outer'. A Session may have at most one Environment \
             defining wrap hooks (RFC 0008).",
        ],
    );
}

#[test]
fn an_enable_if_of_an_optional_type_that_reads_a_missing_value_is_not_evaluated() {
    // `None if Env.Param.N > 0 else true` has static type `bool?`. With N
    // missing, template validation reports the missing N whatever the
    // enableIf (§3.2 constraint 7), for one that also reads Job.Name too.
    // At job creation, which this attaches E to reach, an enableIf that
    // reads a missing value is not evaluated (§7.4.1 step 6), so the
    // missing N is the only error, with no enableIf error.
    let body = |enable_if: &str| {
        format!(
            "- name: E\n  envParameterDefinitions: [{{name: N, type: INT}}]\n  \
             enableIf: \"{enable_if}\"\n  variables: {{V: x}}\n"
        )
    };
    let jt = |enable_if: &str| {
        format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             jobEnvironments:\n{}\
             steps:\n- name: A\n  script: {{actions: {{onRun: {{command: x}}}}}}\n",
            body(enable_if)
        )
    };
    let missing = "steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter \
                   without a default of each Environment that applies to it (every jobEnvironments entry with activation ALWAYS, each one with \
                   activation BY_DEFAULT that the Step does not decline, and each one \
                   with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7), whatever that Environment's own enableIf is; \
                   missing: 'N' of Environment 'E'.";
    let attached_missing = "JobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' \
                            dependency, every parameter without a default of each Environment \
                            that applies to it (every attached Environment with activation \
                            ALWAYS, each one with activation BY_DEFAULT that the Step does not \
                            decline, and each one with activation ON_REQUEST that the Step \
                            requests; Template Schemas §3.2.1), whatever that Environment's own \
                            enableIf is; missing: 'N' of Environment 'E' (EnvironmentTemplate[0]).";
    let plain = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
                 name: J\nsteps:\n- name: A\n  script: {actions: {onRun: {command: x}}}\n";
    for enable_if in [
        "None if Env.Param.N > 0 else true",
        "None if Env.Param.N > 0 and Job.Name != '' else true",
    ] {
        job_err(&jt(enable_if), &[missing]);
        let env = format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
             environment:\n{}",
            body(enable_if).replace("- name: E\n  ", "  name: E\n  ")
        );
        create_err(plain, &[&env], &[], "Submission", &[attached_missing]);
    }
}

#[test]
fn a_literal_that_breaks_a_constraint_is_known_to_the_enable_if() {
    // A literal 0 for an INT with minValue 1 converts, so the enableIf
    // reads it: 0 > 0 is false, and the Environment is disabled, so the
    // value is not checked. Template validation leaves it to job creation,
    // since the enableIf is an expression.
    let jt = |enable_if: &str| {
        format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             jobEnvironments:\n- name: E\n  envParameterDefinitions: [{{name: Count, type: INT, \
             minValue: 1, default: 1}}]\n  enableIf: \"{enable_if}\"\n  variables: {{V: x}}\n\
             steps:\n- name: S\n  dependencies: [{{environment: E, parameters: {{Count: 0}}}}]\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    job_ok(&jt("Env.Param.Count > 0"));
    let created = create_ok(&jt("Env.Param.Count > 0"), &[], &[]);
    assert_eq!(created.job.steps[0].environments, Some(vec![]));
    create_err(
        &jt("Env.Param.Count == 0"),
        &[],
        &[],
        "JobTemplate",
        &[
            "steps[0] -> dependencies[0] -> parameters -> Count:\n\tParameter 'Count': value 0 is \
           less than minimum 1",
        ],
    );
}

#[test]
fn a_path_constraint_is_not_checked_where_the_environment_is_disabled() {
    // §3.2.2: the value checks apply only where the Environment is not
    // disabled for the Step. A PATH value is checked only against its
    // constraints (§4.5 items 2-3), so a relative value is no error in
    // either case and one outside `allowedValues` is one only where the
    // Environment is enabled.
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              parameterDefinitions: [{name: U, type: STRING, default: 'no'}]\n\
              jobEnvironments:\n- name: E\n  envParameterDefinitions: [{name: D, type: PATH, \
              allowedValues: [/ok, relative/dir]}, {name: L, type: 'LIST[PATH]', default: []}]\n  \
              enableIf: \"Param.U == 'yes'\"\n  \
              variables: {V: x}\nsteps:\n- name: S\n  dependencies: [{environment: E, parameters: \
              {D: DVALUE, L: [rel]}}]\n  script: {actions: {onRun: {command: x}}}\n";
    let relative = jt.replace("DVALUE", "relative/dir");
    let outside = jt.replace("DVALUE", "/other");
    create_ok(&outside, &[], &[]);
    let created = create_ok(&relative, &[], &[("U", "yes")]);
    assert_eq!(
        rendered_values(&created.job.steps[0]),
        [r#"JobTemplate/E: D=path:relative/dir, L=list[path]:["rel"]"#]
    );
    create_err(
        &outside,
        &[],
        &[("U", "yes")],
        "JobTemplate",
        &["steps[0] -> dependencies[0] -> parameters -> D:\n\tParameter 'D': value '/other' is not in allowed values"],
    );
}

// ── Audit round 2 ───────────────────────────────────────────────────

#[test]
fn an_ambiguous_attachment_that_is_disabled_is_still_held_to_constraint_7_by_check() {
    // Both attachments named Lic are unreachable (§3.2.1 item 3). The only
    // Step names Lic, so the entry is rejected as ambiguous. Lic is
    // BY_DEFAULT, so it applies to S whatever its enableIf, here `false`
    // (§3.2 constraint 7): check, which reports every violation it is
    // certain of, reports each copy's parameter without a default. Job
    // creation stops S after the ambiguous entry (§7.4.1 step 3), so no Step
    // that is not stopped applies Lic, and it reports the ambiguity alone.
    let env = "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
               environment:\n  name: Lic\n  enableIf: false\n  envParameterDefinitions: [{name: P, \
               type: STRING}]\n  variables: {V: x}\n";
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              steps:\n- name: S\n  dependencies: [{environment: Lic}]\n  script: {actions: \
              {onRun: {command: x}}}\n";
    let ambiguous =
        "JobTemplate -> steps[0] -> dependencies[0] -> environment:\n\tnames Environment \
                     'Lic', which 2 attached Environment Templates define (EnvironmentTemplate[0], \
                     EnvironmentTemplate[1]); an entry must match exactly one (Template Schemas \
                     §3.2.1 item 3).";
    let item_3 = |doc: usize| {
        format!(
            "EnvironmentTemplate[{doc}] -> environment -> envParameterDefinitions:\n\t\
             Environment 'Lic' is defined by 2 attached Environment Templates \
             (EnvironmentTemplate[0], EnvironmentTemplate[1]), so no 'environment:' dependency \
             can reach it (Template Schemas §3.2.1 item 3), and it has parameters without a \
             default that no Step can supply: P. Its activation is BY_DEFAULT, so it applies to \
             every Step whatever its enableIf and exclusiveGroup (§3.2 constraint 7), and the \
             submission is rejected (§3.2.1)."
        )
    };
    create_err(jt, &[env, env], &[], "Submission", &[ambiguous]);
    let err = check(jt, &[env, env]).unwrap_err();
    assert_errors(
        &err.to_string(),
        "Submission",
        &[ambiguous, &item_3(0), &item_3(1)],
    );
}

#[test]
fn an_unknown_value_keeps_its_parameters_type() {
    // At template validation a value not yet known is `unresolved[T]` of
    // its parameter's type, so `Env.Param.N + 1` is an unresolved INT, not
    // an error (§7.6 rule 5). At job creation an enableIf that reads the
    // missing N is not evaluated (§3.2.2 item 2). Either way constraint 7
    // reports N, for a Job Environment and for an attachment alike.
    let missing = |prefix: &str, label: &str, why: &str| {
        format!(
            "{prefix}steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter \
             without a default of each Environment that applies to it ({why}), whatever that Environment's own enableIf is; missing: 'N' of \
             Environment {label}."
        )
    };
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              jobEnvironments:\n- name: E\n  envParameterDefinitions: [{name: N, type: INT}]\n  \
              enableIf: \"Env.Param.N + 1 > 3\"\n  variables: {V: x}\n\
              steps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n";
    job_err(
        jt,
        &[&missing(
            "",
            "'E'",
            "every jobEnvironments entry with activation ALWAYS, each one with \
             activation BY_DEFAULT that the Step does not decline, and each one \
             with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7",
        )],
    );
    let env = "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
               environment:\n  name: A\n  envParameterDefinitions: [{name: N, type: INT}]\n  \
               enableIf: \"Env.Param.N + 1 > 3\"\n  variables: {V: x}\n";
    let plain = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
                 name: J\nsteps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n";
    create_err(
        plain,
        &[env],
        &[],
        "Submission",
        &[&missing(
            "JobTemplate -> ",
            "'A' (EnvironmentTemplate[0])",
            "every attached Environment with activation ALWAYS, each one with \
             activation BY_DEFAULT that the Step does not decline, and each one \
             with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1",
        )],
    );
}

// ── Audit round 3 ───────────────────────────────────────────────────

#[test]
fn an_environments_enable_if_reads_env_raw_param() {
    // §7.3.2: `Env.RawParam.*` of the Environment's own
    // non-PATH parameters, with each Step's value.
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              jobEnvironments:\n- name: E\n  envParameterDefinitions: [{name: Mode, type: STRING, \
              default: 'off'}]\n  enableIf: \"Env.RawParam.Mode == 'on'\"\n  variables: {V: x}\n\
              steps:\n- name: A\n  dependencies: [{environment: E, parameters: {Mode: 'on'}}]\n  \
              script: {actions: {onRun: {command: x}}}\n\
              - name: B\n  script: {actions: {onRun: {command: x}}}\n";
    let created = create_ok(jt, &[], &[]);
    assert_eq!(created.job.steps[0].environments, None);
    assert_eq!(created.job.steps[1].environments, Some(vec![]));
}

#[test]
fn an_attachments_enable_if_reads_job_env_names() {
    // An attachment's enableIf has its hostRequirements scope, with
    // `Job.EnvNames` when its document lists EXPR.
    let env = |names: &str| {
        format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
             environment:\n  name: Lic\n  enableIf: \"'{names}' in Job.EnvNames\"\n  variables: \
             {{V: x}}\n"
        )
    };
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              jobEnvironments: [{name: Own, variables: {V: x}}]\nsteps:\n- name: A\n  script: \
              {actions: {onRun: {command: x}}}\n";
    let created = create_ok(jt, &[&env("Own")], &[]);
    assert_eq!(created.job.steps[0].environments, None);
    let created = create_ok(jt, &[&env("Nope")], &[]);
    assert_eq!(
        created.job.steps[0].environments,
        Some(vec![openjd_model::job::EnvironmentRef {
            document: openjd_model::job::Document::JobTemplate,
            environment: "Own".to_string(),
        }])
    );
}

#[test]
fn an_attachment_reads_job_names_whatever_the_job_template_lists() {
    // An attachment that lists ENV_PARAMS lists EXPR (§1.2 item 3), so its
    // enableIf and hostRequirements read `Job.Name` and `Job.EnvNames`
    // (§7.3.2) even under a Job Template that
    // lists neither extension (RFC Backward compatibility).
    let env = |cond: &str| {
        format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
             environment:\n  name: Lic\n  enableIf: \"{cond}\"\n  hostRequirements:\n    \
             attributes: [{{name: attr.custom, anyOf: [\"{{{{Job.Name}}}}-{{{{len(Job.EnvNames)}}}}\"]}}]\n  \
             variables: {{V: x}}\n"
        )
    };
    let jt = "specificationVersion: jobtemplate-2023-09\nname: TheJob\nsteps:\n- name: A\n  \
              script: {actions: {onRun: {command: x}}}\n";
    let created = create_ok(
        jt,
        &[&env("Job.Name == 'TheJob' and Job.EnvNames == ['Lic']")],
        &[],
    );
    assert_eq!(created.job.steps[0].environments, None);
    let attrs = created.job.steps[0]
        .host_requirements
        .as_ref()
        .and_then(|h| h.attributes.clone())
        .unwrap();
    assert_eq!(attrs.len(), 1);
    assert_eq!(attrs[0].name, "attr.custom");
    assert_eq!(attrs[0].any_of, Some(vec!["TheJob-1".to_string()]));
    // Disabled by `Job.Name`: not entered, and no requirements added.
    let created = create_ok(jt, &[&env("Job.Name == 'Other'")], &[]);
    assert_eq!(created.job.steps[0].environments, Some(vec![]));
    assert_eq!(created.job.steps[0].host_requirements, None);
}

#[test]
fn a_relative_literal_default_is_accepted_when_its_environment_is_disabled_for_every_step() {
    // §4.5 item 2 (plan 4 A42) checks a literal `default` even where the
    // Environment is disabled, but since spec af53020 §4.5 item 3 has no
    // form check, so a relative PATH `default` is no error there, for a Job
    // Environment and for an attachment, at job creation and in `check`.
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              jobEnvironments:\n- name: E\n  enableIf: false\n  envParameterDefinitions: [{name: \
              P, type: PATH, default: rel}]\n  variables: {V: x}\n\
              steps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n";
    let created = create_ok(jt, &[], &[]);
    assert_eq!(created.job.steps[0].environments, Some(vec![]));
    let env = "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
               environment:\n  name: A\n  enableIf: false\n  envParameterDefinitions: [{name: P, \
               type: PATH, default: rel}]\n  variables: {V: x}\n";
    let plain = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
                 name: J\nsteps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n";
    let created = create_ok(plain, &[env], &[]);
    assert_eq!(created.job.steps[0].environments, Some(vec![]));
    check(plain, &[env]).unwrap();
}

#[test]
fn steps_that_differ_in_a_path_value_are_decided_separately() {
    // A PATH parameter is in an enableIf's scope as its Env.RawParam (§7.3.2),
    // so Steps that differ in a PATH value the enableIf reads have different
    // known values and are decided, and reported, separately. A value the
    // enableIf does not read is not bound (§7.4.1 step 6), so Steps that
    // differ only in it are decided, and reported, once.
    let jt = |enable_if: &str| {
        format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             parameterDefinitions: [{{name: L, type: 'LIST[INT]', default: [1]}}]\n\
             jobEnvironments:\n- name: E\n  envParameterDefinitions: [{{name: D, type: PATH}}]\n  \
             enableIf: \"{enable_if}\"\n  variables: {{V: x}}\n\
             steps:\n- name: A\n  dependencies: [{{environment: E, parameters: {{D: /a}}}}]\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n\
             - name: B\n  dependencies: [{{environment: E, parameters: {{D: /b}}}}]\n  \
             script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    create_err(
        &jt("Param.L[5] == 1 or Env.RawParam.D == '/a'"),
        &[],
        &[],
        "JobTemplate",
        &[
            "jobEnvironments[0] -> enableIf:\n\tcould not be evaluated: Index 5 out of bounds for \
           list of length 1. Resolved for Step 'A'.",
            "jobEnvironments[0] -> enableIf:\n\tcould not be evaluated: Index 5 out of bounds for \
           list of length 1. Resolved for Step 'B'.",
        ],
    );
    create_err(
        &jt("Param.L[5] == 1"),
        &[],
        &[],
        "JobTemplate",
        &[
            "jobEnvironments[0] -> enableIf:\n\tcould not be evaluated: Index 5 out of bounds for \
           list of length 1. Resolved for Steps 'A' and 'B'.\n  Param.L[5] == 1\n  ~~~~~~~^~~",
        ],
    );
}

#[test]
fn an_unreachable_attachments_error_follows_the_steps_values() {
    // The error is reported after every Step's values (job-creation.md, the
    // submission stage's error order), since it depends on the attachment's
    // enableIf, which is decided with them.
    let own = "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
               environment:\n  name: Own\n  envParameterDefinitions: [{name: Q, type: STRING}]\n  \
               variables: {V: x}\n";
    let other = "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
                 environment:\n  name: Other\n  envParameterDefinitions: [{name: R, type: STRING}]\n  \
                 variables: {V: x}\n";
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              jobEnvironments: [{name: Own, variables: {V: x}}]\nsteps:\n- name: S\n  script: \
              {actions: {onRun: {command: x}}}\n";
    let err = create(jt, &[own, other], &[]).expect_err("expected job creation to fail");
    let text = err.to_string();
    let values = text
        .find("JobTemplate -> steps[0]:\n\tmust supply")
        .expect("constraint 7 error");
    let unreachable = text
        .find("EnvironmentTemplate[0] -> environment -> envParameterDefinitions:")
        .expect("unreachable attachment error");
    assert!(values < unreachable, "{text}");
}

#[test]
fn an_ambiguous_attachment_with_an_enable_if_is_an_error_saying_it_applies_whatever_it_is() {
    // §3.2.1 item 3, with an enableIf: the attachment applies to every Step
    // whatever its enableIf (§3.2 constraint 7), and the message says so.
    let env = "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
               environment:\n  name: Lic\n  enableIf: \"Job.Name == 'J'\"\n  envParameterDefinitions: \
               [{name: Q, type: STRING}]\n  variables: {V: x}\n";
    let jt = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
              steps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n";
    let item_3 = |doc: usize| {
        format!(
            "EnvironmentTemplate[{doc}] -> environment -> envParameterDefinitions:\n\t\
             Environment 'Lic' is defined by 2 attached Environment Templates \
             (EnvironmentTemplate[0], EnvironmentTemplate[1]), so no 'environment:' dependency \
             can reach it (Template Schemas §3.2.1 item 3), and it has parameters without a \
             default that no Step can supply: Q. Its activation is BY_DEFAULT, so it applies to \
             every Step whatever its enableIf and exclusiveGroup (§3.2 constraint 7), and the \
             submission is rejected (§3.2.1)."
        )
    };
    create_err(
        jt,
        &[env, env],
        &[],
        "Submission",
        &[&item_3(0), &item_3(1)],
    );
}

// ── Audit round 4: what is known before job creation ───────────────

/// A Job Environment `Lic` with the given `enableIf`, a STRING `Mode`
/// (default `off`), a required STRING `Product`, and an INT `Seats`
/// (default 1); its only Step supplies `values`.
fn lic(enable_if: &str, values: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         jobEnvironments:\n- name: Lic\n  enableIf: \"{enable_if}\"\n  envParameterDefinitions:\n  \
         - {{name: Mode, type: STRING, default: 'off'}}\n  - {{name: Product, type: STRING}}\n  \
         - {{name: Seats, type: INT, default: 1}}\n  variables: {{V: x}}\n\
         steps:\n- name: S\n  dependencies: [{{environment: Lic, parameters: {values}}}]\n  \
         script: {{actions: {{onRun: {{command: x}}}}}}\n"
    )
}

const LIC_MISSING: &str = "steps[0]:\n\tmust supply, in an 'environment:' dependency, every \
                           parameter without a default of each Environment that applies to it \
                           (every jobEnvironments entry with activation ALWAYS, each one with \
                           activation BY_DEFAULT that the Step does not decline, and each one \
                           with activation ON_REQUEST that the Step requests; Template Schemas §3.2 \
                           constraint 7), whatever that Environment's own enableIf is; missing: 'Product' of Environment 'Lic'.";

#[test]
fn a_certain_failure_is_reported_at_template_validation() {
    // §7.6 rule 5's preference: a missing value is reported at template
    // validation whatever the enableIf (§3.2 constraint 7, JB-114's shape),
    // and a value is checked there when the Environment is certainly not
    // disabled: its enableIf is known `true`, or reads a literal that does
    // not convert, so it is never evaluated (§3.2.2 item 2).
    job_err(
        &lic("Env.Param.Product != 'none'", "{Mode: on}"),
        &[LIC_MISSING],
    );
    // A literal that does not convert, read by the enableIf.
    job_err(
        &lic("Env.Param.Seats > 0", "{Seats: many, Product: p}"),
        &[
            "steps[0] -> dependencies[0] -> parameters -> Seats:\n\tis not a valid INT value: \
           Cannot parse 'many' as integer",
        ],
    );
    // A format-string value's type, checked by pass 8 where the enableIf is
    // known `true`: the constant expression `true`.
    job_err(
        &lic("true", "{Seats: \"{{ 'x' }}\"}"),
        &[
            LIC_MISSING,
            "steps[0] -> dependencies[0] -> parameters -> Seats:\n\tFailed to parse interpolation \
             expression at [0, 9]. Cannot convert 'x' to int: invalid digit found in string\n  \
             'x'\n  ^~~",
        ],
    );
    // An enableIf that reads only the missing Product may still be decided
    // at job creation for all template validation knows, so the value is
    // not checked; only the missing value is reported.
    job_err(
        &lic("Env.Param.Product != ''", "{Seats: \"{{ 'x' }}\"}"),
        &[LIC_MISSING],
    );
}

#[test]
fn a_constant_enable_if_is_known_and_one_over_env_param_waits_for_job_creation() {
    // §7.6 rule 5: template validation evaluates an enableIf with every
    // value unknown, Env.Param included, and a concrete bool is known. So
    // the constant `true` is known: Lic is not disabled, and Seats's value
    // is checked there. An expression over a value (here a literal Mode) is
    // unknown there, so the value is left to job creation, which decides it.
    let seats = "steps[0] -> dependencies[0] -> parameters -> Seats:\n\tis not a valid INT value: \
                 Cannot parse 'many' as integer";
    job_err(&lic("true", "{Seats: many, Product: p}"), &[seats]);
    // `true` with the literals: not disabled, so job creation checks it.
    let jt = lic("Env.Param.Mode == 'off'", "{Seats: many, Product: p}");
    job_ok(&jt);
    create_err(&jt, &[], &[], "JobTemplate", &[seats]);
    // `false` with the literals: disabled, so the Job is created.
    let jt = lic("Env.Param.Mode == 'on'", "{Seats: many, Product: p}");
    job_ok(&jt);
    create_ok(&jt, &[], &[]);
    // A value the enableIf reads that is a format string is known only at
    // job creation.
    let jt = lic("Env.Param.Product != ''", "{Product: \"{{ Job.Name }}\"}");
    job_ok(&jt);
    create_ok(&jt, &[], &[]);
}

#[test]
fn range_expr_values_compare_by_value() {
    // `values_equal` compares RANGE_EXPR values by the integers they denote
    // (§3.2.2), as stored or as found in a deserialized Job.
    let created = create_ok(RANGES, &[], &[]);
    let stored = created.job.steps[1]
        .environment_parameters
        .as_ref()
        .unwrap()[0]
        .clone();
    let with_value = |value: &str| {
        let mut json = serde_json::to_value(&stored).unwrap();
        json["values"][0]["value"] = serde_json::Value::String(value.to_string());
        serde_json::from_value::<openjd_model::job::EnvironmentParameterValues>(json).unwrap()
    };
    for same in ["1-9:2", "1,3,5,7,9", "9,7,5,3,1", "1-10:2"] {
        assert!(stored.values_equal(&with_value(same)), "{same}");
    }
    for other in ["1-9", "1,3,5,7"] {
        assert!(!stored.values_equal(&with_value(other)), "{other}");
    }
    // A value that does not parse (not one `create_job` makes) compares by
    // its text.
    assert!(with_value("x").values_equal(&with_value("x")));
    assert!(!with_value("x").values_equal(&with_value("y")));
    assert!(!with_value("x").values_equal(&with_value("1-9:2")));
}

// ── Audit round 5 ───────────────────────────────────────────────────

#[test]
fn an_entry_that_may_be_disabled_gives_no_certain_values() {
    // The entry's own enableIf reads a Job Parameter, so whether it
    // requests E and supplies anything is not known at template validation
    // (§7.6 rule 5): E is left to job creation for the Step, its missing N
    // included, and nothing is reported early. Disabled, the entry leaves
    // E (ON_REQUEST) not applying to the Step, and the Job is created.
    let jt = |entry_enable_if: &str| {
        format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             parameterDefinitions: [{{name: Use, type: STRING, default: 'no'}}]\n\
             jobEnvironments:\n- name: E\n  activation: ON_REQUEST\n  enableIf: \"Env.Param.N > \
             0\"\n  envParameterDefinitions:\n  - {{name: N, type: INT}}\n  - {{name: S, type: \
             STRING, allowedValues: [a], default: a}}\n  - {{name: M, type: INT, default: 1}}\n  \
             variables: {{V: x}}\n\
             steps:\n- name: S\n  dependencies:\n  - environment: E\n    enableIf: \
             {entry_enable_if}\n    parameters: {{S: zz, M: \"{{{{ 'x' }}}}\"}}\n  script: \
             {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    let expr = jt("\"Param.Use == 'yes'\"");
    job_ok(&expr);
    create_ok(&expr, &[], &[]);
    // Enabled at job creation: every check applies.
    create_err(
        &expr,
        &[],
        &[("Use", "yes")],
        "JobTemplate",
        &[
            "steps[0] -> dependencies[0] -> parameters -> S:\n\tParameter 'S': value 'zz' is not \
             in allowed values",
            "steps[0] -> dependencies[0] -> parameters -> M:\n\tcould not be resolved as a value \
             of type INT: Cannot convert 'x' to int: invalid digit found in string\n  'x'\n  ^~~",
            "steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a \
             default of each Environment that applies to it (every jobEnvironments entry with activation ALWAYS, each one with \
             activation BY_DEFAULT that the Step does not decline, and each one \
             with activation ON_REQUEST that the Step requests; \
             Template Schemas §3.2 constraint 7), whatever that Environment's own enableIf is; missing: 'N' of Environment 'E'.",
        ],
    );
    // A disabled entry supplies nothing, so pass 8 does not type its values.
    job_ok(&jt("false"));
}

#[test]
fn early_value_checks_follow_env_raw_param_reads_and_range_exprs_as_parsed() {
    // Constraint 7 holds whatever the enableIf: a missing Seats is reported
    // at template validation (§3.2 constraint 7).
    let jt = |enable_if: &str, deps: &str| {
        format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             jobEnvironments:\n- name: Lic\n  enableIf: \"{enable_if}\"\n  \
             envParameterDefinitions:\n  - {{name: R, type: RANGE_EXPR, default: '1,3,5,7,9'}}\n  \
             - {{name: Seats, type: INT}}\n  variables: {{V: x}}\n\
             steps:\n- name: S\n{deps}  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    let missing = "steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter \
                   without a default of each Environment that applies to it (every jobEnvironments entry with activation ALWAYS, each one with \
                   activation BY_DEFAULT that the Step does not decline, and each one \
                   with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7), whatever that Environment's own enableIf is; \
                   missing: 'Seats' of Environment 'Lic'.";
    job_err(&jt("Env.RawParam.Seats > 1", ""), &[missing]);
    // A Seats literal that does not convert: an enableIf that reads Seats,
    // as Env.RawParam too, and whether or not evaluation reaches it, is
    // never evaluated, so Lic is certainly not disabled, and template
    // validation rejects the literal (§3.2.2 item 2).
    let many = "  dependencies: [{environment: Lic, parameters: {Seats: many}}]\n";
    let bad = "steps[0] -> dependencies[0] -> parameters -> Seats:\n\tis not a valid INT value: \
               Cannot parse 'many' as integer";
    for enable_if in [
        "Env.RawParam.Seats > 1",
        "len(Env.Param.R) == 9 and Env.Param.Seats > 1",
    ] {
        job_err(&jt(enable_if, many), &[bad]);
    }
    // One that does not read Seats is left to job creation, which reads R
    // as its range_expr: `len(R)` counts its 5 integers, not the
    // characters of a string. `true`: Lic is entered and the literal
    // rejected; `false`: Lic is disabled and the literal not checked.
    let five = jt("len(Env.Param.R) == 5", many);
    job_ok(&five);
    create_err(&five, &[], &[], "JobTemplate", &[bad]);
    let nine = jt("len(Env.Param.R) == 9", many);
    job_ok(&nine);
    assert_eq!(
        create_ok(&nine, &[], &[]).job.steps[0].environments,
        Some(vec![])
    );
}

#[test]
fn check_reports_an_unreachable_attachment_whatever_its_enable_if() {
    // The attachment applies to S whatever its enableIf, here one that reads
    // only X, which no Step can supply (§3.2 constraint 7): check reports
    // the attachment-level error as job creation does.
    let env = "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
               environment:\n  name: Gpu\n  enableIf: \"Env.Param.X != ''\"\n  \
               envParameterDefinitions: [{name: X, type: STRING}]\n  variables: {V: x}\n";
    let shadowed = "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
                    name: J\njobEnvironments: [{name: Gpu, variables: {V: x}}]\nsteps:\n\
                    - name: S\n  script: {actions: {onRun: {command: x}}}\n";
    let expected = "EnvironmentTemplate[0] -> environment -> envParameterDefinitions:\n\t\
                    Environment 'Gpu' has the same name as a jobEnvironments entry of the Job \
                    Template, so no 'environment:' dependency can reach it (Template Schemas \
                    §3.2.1 item 4), and it has parameters without a default that no Step can \
                    supply: X. Its activation is BY_DEFAULT, so it applies to every Step whatever \
                    its enableIf and exclusiveGroup (§3.2 constraint 7), and the submission is \
                    rejected (§3.2.1).";
    let err = check(shadowed, &[env]).unwrap_err();
    assert_errors(&err.to_string(), "Submission", &[expected]);
    create_err(shadowed, &[env], &[], "Submission", &[expected]);
}

// ── Audit round 6 ───────────────────────────────────────────────────

/// A Job Environment `E` (activation `activation`) with INT parameters
/// `X` and `Y` without defaults and `enableIf: "Env.Param.X == 1"`; its
/// only Step requests it with `values`.
fn x_and_y(activation: &str, values: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         jobEnvironments:\n- name: E\n  activation: {activation}\n  enableIf: \"Env.Param.X == \
         1\"\n  envParameterDefinitions: [{{name: X, type: INT}}, {{name: Y, type: INT}}]\n  \
         variables: {{V: x}}\n\
         steps:\n- name: S\n  dependencies: [{{environment: E, parameters: {values}}}]\n  script: \
         {{actions: {{onRun: {{command: x}}}}}}\n"
    )
}

#[test]
fn constraint_7_holds_whatever_the_entrys_values_make_the_enable_if() {
    // With Y missing, template validation reports constraint 7 whether the
    // entry's X makes E's enableIf `false` (X: 2) or `true` (X: 1): E applies
    // to S, as BY_DEFAULT and as an ON_REQUEST one the entry requests,
    // whatever its enableIf (§3.2 constraint 7). With Y supplied, job
    // creation decides the enableIf from X: disabled for X: 2, entered for
    // X: 1.
    for activation in ["BY_DEFAULT", "ON_REQUEST"] {
        for x in ["2", "1"] {
            job_err(
                &x_and_y(activation, &format!("{{X: {x}}}")),
                &["steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without \
                   a default of each Environment that applies to it (every jobEnvironments entry with activation ALWAYS, each one with \
                   activation BY_DEFAULT that the Step does not decline, and each one \
                   with activation ON_REQUEST that the Step requests; \
                   Template Schemas §3.2 constraint 7), whatever that Environment's own enableIf is; missing: 'Y' of Environment 'E'."],
            );
        }
        let created = create_ok(&x_and_y(activation, "{X: 2, Y: 0}"), &[], &[]);
        assert_eq!(created.job.steps[0].environments, Some(vec![]));
        let created = create_ok(&x_and_y(activation, "{X: 1, Y: 0}"), &[], &[]);
        assert_eq!(created.job.steps[0].environments, None);
    }
}

#[test]
fn a_resolved_default_that_breaks_a_constraint_is_known_to_the_enable_if() {
    // §3.2.2: a value from a `default` that converts is known whether or not
    // it meets its constraints. X's default resolves to 0, below its
    // minimum; the enableIf reads it as 0 and is false, so E is disabled and
    // the default is not checked.
    let jt = |enable_if: &str| {
        format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             parameterDefinitions: [{{name: N, type: INT, default: 0}}]\n\
             jobEnvironments:\n- name: E\n  enableIf: \"{enable_if}\"\n  envParameterDefinitions: \
             [{{name: X, type: INT, minValue: 1, default: \"{{{{Param.N}}}}\"}}]\n  variables: \
             {{V: x}}\nsteps:\n- name: S\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        )
    };
    create_ok(&jt("Env.Param.X != 0"), &[], &[]);
    create_err(
        &jt("Env.Param.X == 0"),
        &[],
        &[],
        "JobTemplate",
        &[
            "jobEnvironments[0] -> envParameterDefinitions[0] -> default:\n\tfor Step 'S', which \
           does not supply this parameter: Parameter 'X': value 0 is less than minimum 1",
        ],
    );
}

#[test]
fn an_enable_if_that_reads_a_missing_path_parameter_is_reported_at_template_validation() {
    // An enableIf may read a PATH parameter's Env.RawParam, its unmapped
    // string (§7.3.2). P has no default and S does not
    // supply it, so the result depends on a value known to be missing, and
    // constraint 7 is reported at template validation (§3.2.2).
    job_err(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
         jobEnvironments:\n- name: E\n  enableIf: \"Env.RawParam.P == '/x'\"\n  \
         envParameterDefinitions: [{name: P, type: PATH}, {name: Q, type: INT}]\n  variables: \
         {V: x}\nsteps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n",
        &[
            "steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without \
             a default of each Environment that applies to it (every jobEnvironments entry with activation ALWAYS, each one with \
             activation BY_DEFAULT that the Step does not decline, and each one \
             with activation ON_REQUEST that the Step requests; \
             Template Schemas §3.2 constraint 7), whatever that Environment's own enableIf is; missing: 'P', 'Q' of Environment 'E'.",
        ],
    );
}
