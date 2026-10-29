// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `ENV_PARAMS` value-reference scopes and the template-validation checks
//! of entries that name a Job Environment (prototype plan PR 2).
//!
//! Covers: `Env.Param.*` / `Env.RawParam.*` within the declaring
//! Environment only (§4 `Env.*` scope, §7.3.1, RFC Value references),
//! with their EXPR types (§4.5 last paragraph); the scope of a `default`
//! (§4.5 item 4) and of an `environment:` entry value (§3.2); the EXPR
//! target-type check of single-expression values (§7.4, plan A17);
//! §3.2 constraints 5–7 for `jobEnvironments` entries; the carried-forward
//! checks at job creation; and the spec samples and RFC examples.

use super::env_params_common::*;
use openjd_model::{create_job, JobParameterValues};

/// A Job Template whose one Job Environment `E` declares `I` (INT), `S`
/// (STRING), and `P` (PATH), all with defaults, and whose `variables` hold
/// `vars` (YAML flow-map body).
fn job_env_with_vars(vars: &str) -> String {
    format!(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {{name: I, type: INT, default: 1}}
  - {{name: S, type: STRING, default: s}}
  - {{name: P, type: PATH, default: /p}}
  variables: {{{vars}}}
steps:
- name: S
  script: {{actions: {{onRun: {{command: x}}}}}}
"#
    )
}

// ── Env.Param.* / Env.RawParam.* scope ─────────────────────────────

#[test]
fn env_param_is_available_in_every_field_of_the_declaring_environment() {
    job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR, WRAP_ACTIONS]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: I, type: INT, default: 1}
  - {name: S, type: STRING, default: s}
  - {name: P, type: PATH, default: /p}
  variables:
    V: "{{Env.Param.S}} {{Env.RawParam.P}}"
  script:
    let: ["n = Env.Param.I * 2"]
    actions:
      onEnter: {command: echo, args: ["{{ n }}", "{{Env.Param.P}}"]}
      onWrapEnvEnter: {command: "{{WrappedAction.Command}}", args: ["{{Env.Param.S}}"]}
      onWrapTaskRun: {command: "{{WrappedAction.Command}}", args: ["{{Env.Param.S}}"]}
      onWrapEnvExit: {command: "{{WrappedAction.Command}}", args: ["{{Env.Param.S}}"]}
      onExit: {command: echo, args: ["{{Env.RawParam.I}}"]}
    embeddedFiles:
    - {name: F, type: TEXT, data: "{{Env.Param.S}}"}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
    );
    env_ok(
        r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions: [{name: S, type: STRING, default: s}]
  variables: {V: "{{Env.Param.S}}"}
  script:
    actions:
      onEnter: {command: echo, args: ["{{Env.RawParam.S}}"]}
"#,
    );
}

#[test]
fn env_param_is_rejected_in_steps_and_other_environments() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: A, type: STRING, default: a}]
  variables: {V: "{{Env.Param.A}}"}
- name: Other
  variables: {V: "{{Env.Param.A}}"}
steps:
- name: S
  stepEnvironments: [{name: SE, variables: {V: "{{Env.RawParam.A}}"}}]
  script:
    actions: {onRun: {command: "{{Env.Param.A}}"}}
    embeddedFiles: [{name: F, type: TEXT, data: "{{Env.Param.A}}"}]
"#,
        &[
            "jobEnvironments[1] -> variables -> V:\n\tFailed to parse interpolation expression at [0, 15]. Undefined variable: 'Env.Param.A'.",
            "steps[0] -> script -> actions -> onRun -> command:\n\tFailed to parse interpolation expression at [0, 15]. Undefined variable: 'Env.Param.A'.",
            "steps[0] -> script -> embeddedFiles[0] -> data:\n\tFailed to parse interpolation expression at [0, 15]. Undefined variable: 'Env.Param.A'.",
            "steps[0] -> stepEnvironments[0] -> variables -> V:\n\tFailed to parse interpolation expression at [0, 18]. Undefined variable: 'Env.RawParam.A'.",
        ],
    );
}

#[test]
fn env_param_naming_no_parameter_is_an_error() {
    job_err(
        &job_env_with_vars(r#"V: "{{Env.Param.Nope}}""#),
        &["jobEnvironments[0] -> variables -> V:\n\tFailed to parse interpolation expression at [0, 18]. Undefined variable: 'Env.Param.Nope'. Did you mean one of: Env.Param.I, Env.Param.P, Env.Param.S"],
    );
}

#[test]
fn env_param_is_not_available_in_job_creation_fields() {
    // An action `timeout` is resolved at job creation, before any Session
    // enters the Environment.
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR, FEATURE_BUNDLE_1]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: T, type: INT, default: 5}]
  script:
    actions:
      onEnter: {command: echo, timeout: "{{Env.Param.T}}"}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
        &["jobEnvironments[0] -> script -> actions -> onEnter -> timeout:\n\tFailed to parse interpolation expression at [0, 15]. Undefined variable: 'Env.Param.T'."],
    );
}

#[test]
fn env_param_has_the_parameter_type() {
    job_ok(&job_env_with_vars(
        r#"A: "{{ Env.Param.I + 1 }}", B: "{{ Env.Param.P.name }}", C: "{{ Env.RawParam.P.upper() }}""#,
    ));
    job_err(
        &job_env_with_vars(
            r#"A: "{{ Env.Param.S + 1 }}", B: "{{ Env.RawParam.P.name }}""#,
        ),
        &[
            "jobEnvironments[0] -> variables -> A:\n\tFailed to parse interpolation expression at [0, 21]. Cannot use '+' operator with string and int",
            "jobEnvironments[0] -> variables -> B:\n\tFailed to parse interpolation expression at [0, 25]. 'name' property is not available for string. Available for: path",
        ],
    );
}

#[test]
fn env_raw_param_of_list_path_is_list_of_string() {
    let doc = |expr: &str| {
        format!(
            r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{{name: LP, type: "LIST[PATH]", default: []}}]
  variables: {{V: "{{{{ {expr} }}}}"}}
steps:
- name: S
  script: {{actions: {{onRun: {{command: x}}}}}}
"#
        )
    };
    job_ok(&doc("Env.RawParam.LP[0].upper()"));
    job_ok(&doc("Env.Param.LP[0].name"));
    job_err(
        &doc("Env.Param.LP[0].upper()"),
        &["jobEnvironments[0] -> variables -> V:\n\tFailed to parse interpolation expression at [0, 29]. upper() is not available for path. Available for: string"],
    );
}

#[test]
fn wrap_hooks_see_the_wrapping_environments_parameters_only() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR, WRAP_ACTIONS]
name: J
jobEnvironments:
- name: Inner
  envParameterDefinitions: [{name: X, type: STRING, default: x}]
  variables: {V: "{{Env.Param.X}}"}
- name: Wrapper
  envParameterDefinitions: [{name: W, type: STRING, default: w}]
  script:
    actions:
      onWrapEnvEnter: {command: "{{WrappedAction.Command}}", args: ["{{Env.Param.W}}"]}
      onWrapTaskRun: {command: "{{WrappedAction.Command}}", args: ["{{Env.Param.X}}"]}
      onWrapEnvExit: {command: "{{WrappedAction.Command}}"}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
        &["jobEnvironments[1] -> script -> actions -> onWrapTaskRun -> args[0]:\n\tFailed to parse interpolation expression at [0, 15]. Undefined variable: 'Env.Param.X'. Did you mean: Env.Param.W"],
    );
}

// ── Scope of a `default` (§4.5 item 4) ─────────────────────────────

#[test]
fn default_scope_is_the_documents_raw_and_non_path_params() {
    job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions:
- {name: Seats, type: INT, default: 2}
- {name: Dir, type: PATH, default: d}
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: A, type: INT, default: "{{Param.Seats}}"}
  - {name: B, type: PATH, default: "{{RawParam.Dir}}"}
  - {name: C, type: STRING, default: "seats={{RawParam.Seats}}"}
  variables: {V: x}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
    );
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Dir, type: PATH, default: d}]
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: A, type: PATH, default: "{{Param.Dir}}"}
  - {name: B, type: STRING, default: "{{Job.Name}}"}
  - {name: C, type: STRING, default: "{{Env.Param.B}}"}
  - {name: D, type: STRING, default: "{{Session.WorkingDirectory}}"}
  variables: {V: x}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "jobEnvironments[0] -> envParameterDefinitions[0] -> default:\n\tFailed to parse interpolation expression at [0, 13]. Undefined variable: 'Param.Dir'. Did you mean: RawParam.Dir",
            "jobEnvironments[0] -> envParameterDefinitions[1] -> default:\n\tFailed to parse interpolation expression at [0, 12]. Undefined variable: 'Job.Name'.",
            "jobEnvironments[0] -> envParameterDefinitions[2] -> default:\n\tFailed to parse interpolation expression at [0, 15]. Undefined variable: 'Env.Param.B'.",
            "jobEnvironments[0] -> envParameterDefinitions[3] -> default:\n\tFailed to parse interpolation expression at [0, 28]. Undefined variable: 'Session.WorkingDirectory'.",
        ],
    );
}

#[test]
fn environment_template_default_reads_its_own_documents_parameters() {
    env_ok(
        r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
parameterDefinitions: [{name: DockerImage, type: STRING, default: ubuntu}]
environment:
  name: Docker
  envParameterDefinitions: [{name: Image, type: STRING, default: "{{Param.DockerImage}}"}]
  variables: {V: "{{Env.Param.Image}}"}
"#,
    );
    env_err(
        r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: Docker
  envParameterDefinitions: [{name: Image, type: STRING, default: "{{Param.JobImage}}"}]
  variables: {V: x}
"#,
        &["environment -> envParameterDefinitions[0] -> default:\n\tFailed to parse interpolation expression at [0, 18]. Undefined variable: 'Param.JobImage'."],
    );
}

#[test]
fn list_typed_format_string_default_must_be_one_expression() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Dir, type: PATH, default: d}]
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: A, type: "LIST[PATH]", default: "{{ [RawParam.Dir] }}"}
  - {name: B, type: "LIST[PATH]", default: "{{RawParam.Dir}} more"}
  variables: {V: x}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
        &["jobEnvironments[0] -> envParameterDefinitions[1] -> default:\n\ta LIST[PATH] parameter has no string form, so a format-string value for it must be a single '{{ }}' expression."],
    );
}

#[test]
fn single_expression_default_is_type_checked_against_the_parameter_type() {
    // Plan A17: a static type that can never convert is an error even
    // when every Step supplies the parameter. A string that might parse
    // as an INT is left to job creation.
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Name, type: STRING, default: n}]
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: A, type: INT, default: "{{ [1] }}"}
  - {name: B, type: INT, default: "{{ Param.Name }}"}
  - {name: C, type: "LIST[STRING]", default: "{{ Param.Name }}"}
  variables: {V: x}
steps:
- name: S
  dependencies: [{environment: E, parameters: {A: 1}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "jobEnvironments[0] -> envParameterDefinitions[0] -> default:\n\tFailed to parse interpolation expression at [0, 9]. Cannot coerce list[int] to int",
            "jobEnvironments[0] -> envParameterDefinitions[2] -> default:\n\tFailed to parse interpolation expression at [0, 16]. Cannot coerce string to list[string]",
        ],
    );
}

// ── Scope of an environment: entry value (§3.2) ────────────────────

#[test]
fn entry_value_scope_includes_step_name_and_let_with_expr() {
    job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions:
- {name: Pkgs, type: STRING, default: ffmpeg}
- {name: Dir, type: PATH, default: d}
steps:
- name: S
  let: ["pk = Param.Pkgs + ' extra'"]
  dependencies:
  - environment: Conda
    required: false
    parameters:
      Packages: "{{ pk }}"
      Label: "{{Step.Name}}-{{Job.Name}}"
      Mounts: "{{ [RawParam.Dir] }}"
  script: {actions: {onRun: {command: x}}}
"#,
    );
}

#[test]
fn entry_value_scope_excludes_task_session_env_and_path_params() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Dir, type: PATH, default: d}]
steps:
- name: S
  parameterSpace:
    taskParameterDefinitions: [{name: F, type: INT, range: "1-2"}]
  dependencies:
  - environment: Ext
    parameters:
      A: "{{Task.Param.F}}"
      B: "{{Session.WorkingDirectory}}"
      C: "{{Env.Param.X}}"
      D: "{{Param.Dir}}"
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "steps[0] -> dependencies[0] -> parameters -> A:\n\tFailed to parse interpolation expression at [0, 16]. Undefined variable: 'Task.Param.F'.",
            "steps[0] -> dependencies[0] -> parameters -> B:\n\tFailed to parse interpolation expression at [0, 28]. Undefined variable: 'Session.WorkingDirectory'.",
            "steps[0] -> dependencies[0] -> parameters -> C:\n\tFailed to parse interpolation expression at [0, 15]. Undefined variable: 'Env.Param.X'.",
            "steps[0] -> dependencies[0] -> parameters -> D:\n\tFailed to parse interpolation expression at [0, 13]. Undefined variable: 'Param.Dir'. Did you mean: RawParam.Dir",
        ],
    );
}

#[test]
fn step_let_cannot_carry_a_path_param_into_an_entry() {
    // Plan A2: a step-level `let` is template scope, where PATH `Param.*`
    // does not exist, so the existing `let` check already prevents it.
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Dir, type: PATH, default: d}]
steps:
- name: S
  let: ["p = Param.Dir"]
  dependencies: [{environment: Ext, parameters: {M: "{{ p }}"}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &["steps[0] -> let[0]:\n\tInvalid expression in let binding 'p': Undefined variable: 'Param.Dir'. Did you mean: RawParam.Dir"],
    );
}

#[test]
fn job_environment_entry_values_are_type_checked_external_ones_are_not() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: M, type: "LIST[PATH]", default: []}
  - {name: I, type: INT, default: 1}
  variables: {V: x}
steps:
- name: S
  dependencies:
  - environment: E
    parameters: {M: "{{ ['a', 1] }}", I: "{{ [1] }}"}
  - environment: Ext
    parameters: {M: "{{ [1] }}", I: "{{ [1] }}"}
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "steps[0] -> dependencies[0] -> parameters -> I:\n\tFailed to parse interpolation expression at [0, 9]. Cannot coerce list[int] to int",
            "steps[0] -> dependencies[0] -> parameters -> M:\n\tFailed to parse interpolation expression at [0, 14]. Cannot coerce int to path",
        ],
    );
}

#[test]
fn list_typed_job_environment_entry_value_must_be_one_expression() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Dir, type: PATH, default: d}]
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: M, type: "LIST[PATH]", default: []}]
  variables: {V: x}
steps:
- name: S
  dependencies: [{environment: E, parameters: {M: "{{RawParam.Dir}},/b"}}]
  script: {actions: {onRun: {command: x}}}
- name: T
  dependencies: [{environment: E, parameters: {M: ["{{RawParam.Dir}}"]}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "steps[0] -> dependencies[0] -> parameters -> M:\n\ta LIST[PATH] parameter has no string form, so a format-string value for it must be a single '{{ }}' expression.",
            "steps[1] -> dependencies[0] -> parameters -> M[0]:\n\tthe elements of a list literal are literals, not format strings. Write the whole value as one '{{ }}' expression, for example '{{ [RawParam.Dir] }}'.",
        ],
    );
}

// ── Constraints 5–7 for jobEnvironments entries ────────────────────

#[test]
fn constraint_5_keys_must_be_declared() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: A, type: STRING, default: a}
  - {name: B, type: STRING, default: b}
  variables: {V: x}
- name: Plain
  variables: {V: x}
steps:
- name: S
  dependencies:
  - {environment: E, parameters: {A: x, Zed: 1}}
  - {environment: Plain, parameters: {A: x, B: y}}
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "steps[0] -> dependencies[0] -> parameters -> Zed:\n\tEnvironment 'E' declares no parameter 'Zed'. Declared: A, B.",
            "steps[0] -> dependencies[1] -> parameters:\n\tEnvironment 'Plain' declares no parameters, so this entry must not supply any.",
        ],
    );
}

#[test]
fn constraint_6_literal_values_against_the_definition() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: L
  envParameterDefinitions:
  - {name: Product, type: STRING, allowedValues: [arnold, redshift]}
  - {name: Seats, type: INT, default: 1, minValue: 1}
  - {name: Ratio, type: FLOAT, default: 1.0}
  variables: {V: x}
steps:
- name: A
  dependencies: [{environment: L, parameters: {Product: vray, Seats: 0}}]
  script: {actions: {onRun: {command: x}}}
- name: B
  # Plan A5: a Job Parameter STRING default must be a string, so 7 is not
  # a STRING literal; INT and FLOAT accept numeric strings.
  dependencies: [{environment: L, parameters: {Product: 7, Seats: "2", Ratio: "x"}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "steps[0] -> dependencies[0] -> parameters -> Product:\n\tParameter 'Product': value 'vray' is not in allowed values",
            "steps[0] -> dependencies[0] -> parameters -> Seats:\n\tParameter 'Seats': value 0 is less than minimum 1",
            "steps[1] -> dependencies[0] -> parameters -> Product:\n\tis not a valid STRING value: invalid type: integer `7`, expected a string",
            "steps[1] -> dependencies[0] -> parameters -> Ratio:\n\tis not a valid FLOAT value: Cannot parse 'x' as float",
        ],
    );
}

#[test]
fn constraint_7_every_step_supplies_parameters_without_default() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: L
  envParameterDefinitions:
  - {name: Product, type: STRING}
  - {name: Seats, type: INT, default: 1}
  variables: {V: "{{Env.Param.Product}}"}
steps:
- name: Supplies
  dependencies: [{environment: L, parameters: {Product: arnold}}]
  script: {actions: {onRun: {command: x}}}
- name: NoEntry
  script: {actions: {onRun: {command: x}}}
- name: EntryWithoutIt
  dependencies: [{environment: L, parameters: {Seats: 2}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "steps[1]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every jobEnvironments entry with activation ALWAYS, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7); missing: 'Product' of Environment 'L'.",
            "steps[2]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every jobEnvironments entry with activation ALWAYS, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7); missing: 'Product' of Environment 'L'.",
        ],
    );
}

#[test]
fn constraint_7_is_not_checked_for_external_environments() {
    // Whether an external Environment has parameters without a default is
    // known only at job creation.
    job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: A
  dependencies: [{environment: Conda, parameters: {Packages: x}}]
  script: {actions: {onRun: {command: x}}}
- name: B
  script: {actions: {onRun: {command: x}}}
"#,
    );
}

// ── Job creation carried-forward checks ────────────────────────────

#[test]
fn create_job_accepts_env_param_references_in_job_environments() {
    let jt = job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Seats, type: INT, default: 2}]
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: N, type: INT, default: "{{Param.Seats}}"}
  - {name: P, type: PATH, default: /p}
  variables: {V: "{{Env.Param.N}}"}
  script:
    let: ["twice = Env.Param.N * 2"]
    actions:
      onEnter: {command: echo, args: ["{{ twice }}", "{{Env.Param.P}}", "{{Env.RawParam.P}}"]}
      onExit: {command: echo, args: ["{{Env.Param.N}}"]}
    embeddedFiles: [{name: F, type: TEXT, data: "{{Env.Param.N}}"}]
steps:
- name: S
  dependencies: [{environment: E, parameters: {N: 3}}]
  script: {actions: {onRun: {command: x}}}
"#,
    );
    let job = create_job(
        &jt,
        &JobParameterValues::new(),
        &jt.default_validation_context(),
    )
    .unwrap_or_else(|e| panic!("create_job failed:\n{e}"));
    assert_eq!(job.job_environments.as_ref().unwrap()[0].name, "E");
}

#[test]
fn create_job_rejects_a_context_without_env_params() {
    let jt = job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
    );
    let mut exts = openjd_model::Extensions::new();
    exts.insert(openjd_model::ModelExtension::Expr);
    let ctx = openjd_model::ValidationContext::with_extensions(
        openjd_model::SpecificationRevision::V2023_09,
        exts,
    );
    let err = create_job(&jt, &JobParameterValues::new(), &ctx)
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "Compatibility error: create_job requires a context enabling every extension the \
         template declares: missing ENV_PARAMS. An application that does not support an \
         extension should reject the template at decode via its supported-extensions list."
    );
}

// ── Spec samples and RFC examples ──────────────────────────────────

const CONDA: &str = include_str!("../fixtures/env_params/conda-env-params.yaml");
const DOCKER: &str = include_str!("../fixtures/env_params/docker-env-params.yaml");
const VIDEO: &str = include_str!("../fixtures/env_params/video-sfm-composite.yaml");

#[test]
fn spec_samples_are_valid() {
    env_ok(CONDA);
    env_ok(DOCKER);
    job_ok(VIDEO);
}

#[test]
fn spec_samples_without_the_extension_are_rejected() {
    // Remove `- ENV_PARAMS` from each sample's extensions list.
    let strip = |s: &str| s.replace("\n- ENV_PARAMS\n", "\n");
    let gate = "environment -> envParameterDefinitions:\n\tenvParameterDefinitions on an Environment requires the ENV_PARAMS extension.";
    // Only the gate fires: the Environment's own `Env.Param.*` references
    // still resolve, so they do not cascade into further errors.
    // The Conda sample's enableIf is gated too (§4 item 6 constraint 4).
    env_err(
        &strip(CONDA),
        &[
            "environment -> enableIf:\n\tenableIf on an Environment requires the ENV_PARAMS extension.",
            gate,
        ],
    );
    // The Docker sample also has activation and hostRequirements, each gated.
    env_err(
        &strip(DOCKER),
        &[
            "environment -> activation:\n\tactivation on an Environment requires the ENV_PARAMS extension.",
            "environment -> hostRequirements:\n\thostRequirements on an Environment requires the ENV_PARAMS extension.",
            gate,
        ],
    );
    let gate_dep = "the 'environment' form of a dependency requires the ENV_PARAMS extension.";
    // The sample's Step-to-Step entries are written `step:`, which is gated
    // too (§3.2 item 1).
    let gate_step = "the 'step' form of a dependency requires the ENV_PARAMS extension.";
    let mut expected = Vec::new();
    for (step, dep, gate) in [
        (0, 0, gate_dep),
        (0, 1, gate_dep),
        (1, 0, gate_step),
        (1, 1, gate_dep),
        (1, 2, gate_dep),
        (2, 0, gate_step),
        (2, 1, gate_dep),
        (2, 2, gate_dep),
        (3, 0, gate_step),
        (3, 1, gate_dep),
        (3, 2, gate_dep),
    ] {
        expected.push(format!("steps[{step}] -> dependencies[{dep}]:\n\t{gate}"));
    }
    let expected: Vec<&str> = expected.iter().map(String::as_str).collect();
    job_err(&strip(VIDEO), &expected);
}

const RENDER_LICENSE: &str = r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [ENV_PARAMS, EXPR]
name: "Plugin-Selected Renders"
jobEnvironments:
  - name: RenderLicense
    envParameterDefinitions:
      - name: Product
        type: STRING
        allowedValues: ["arnold", "redshift", "vray"]
      - name: Seats
        type: INT
        default: 1
        minValue: 1
    script:
      actions:
        onEnter:
          command: license-checkout
          args: ["--product", "{{Env.Param.Product}}", "--seats", "{{Env.Param.Seats}}"]
        onExit:
          command: license-release
          args: ["--product", "{{Env.Param.Product}}"]
steps:
  - name: BeautyPass
    dependencies:
      - environment: RenderLicense
        parameters: { Product: "arnold" }
    script: {actions: {onRun: {command: render}}}
  - name: VolumePass
    dependencies:
      - environment: RenderLicense
        parameters: { Product: "redshift", Seats: 2 }
    script: {actions: {onRun: {command: render}}}
"#;

#[test]
fn rfc_render_license_example() {
    job_ok(RENDER_LICENSE);
    // "a Step without a RenderLicense entry is a template validation error"
    let without = RENDER_LICENSE.replace(
        "    dependencies:\n      - environment: RenderLicense\n        parameters: { Product: \"arnold\" }\n",
        "",
    );
    job_err(
        &without,
        &["steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every jobEnvironments entry with activation ALWAYS, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7); missing: 'Product' of Environment 'RenderLicense'."],
    );
}

#[test]
fn constraint_7_counts_only_entries_naming_that_environment() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: A
  envParameterDefinitions: [{name: P, type: STRING}]
  variables: {V: x}
- name: B
  envParameterDefinitions: [{name: P, type: STRING, default: b}]
  variables: {V: x}
steps:
- name: S
  dependencies: [{environment: B, parameters: {P: x}}, {environment: Ext, parameters: {P: y}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &["steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every jobEnvironments entry with activation ALWAYS, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7); missing: 'P' of Environment 'A'."],
    );
}

#[test]
fn duplicate_definitions_are_reported_once_downstream() {
    // The duplicate is reported by its own check; constraint 5's list of
    // declared names and constraint 7 name each parameter once.
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: P, type: STRING}, {name: P, type: STRING}]
  variables: {V: x}
steps:
- name: S
  dependencies: [{environment: E, parameters: {Q: x}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "jobEnvironments[0] -> envParameterDefinitions[1]:\n\tduplicate parameter name: 'P'",
            "steps[0] -> dependencies[0] -> parameters -> Q:\n\tEnvironment 'E' declares no parameter 'Q'. Declared: P.",
            "steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every jobEnvironments entry with activation ALWAYS, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7); missing: 'P' of Environment 'E'.",
        ],
    );
}

#[test]
fn constraint_5_list_of_declared_names_is_capped() {
    // 51 definitions is itself an error; the constraint 5 message lists
    // the first 50 names only.
    let defs: Vec<String> = (0..51)
        .map(|i| format!("  - {{name: D{i}, type: STRING, default: a}}"))
        .collect();
    let first_50: Vec<String> = (0..50).map(|i| format!("D{i}")).collect();
    job_err(
        &format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\njobEnvironments:\n- name: E\n  variables: {{V: x}}\n  envParameterDefinitions:\n{}\nsteps:\n- name: S\n  dependencies: [{{environment: E, parameters: {{Q: x}}}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n",
            defs.join("\n")
        ),
        &[
            "jobEnvironments[0] -> envParameterDefinitions:\n\tmust not contain more than 50 elements.",
            &format!(
                "steps[0] -> dependencies[0] -> parameters -> Q:\n\tEnvironment 'E' declares no parameter 'Q'. Declared: {}, and 1 more.",
                first_50.join(", ")
            ),
        ],
    );
}

#[test]
fn a_duplicated_parameter_name_types_entries_by_its_first_definition() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: A, type: INT, default: 1}, {name: A, type: STRING, default: s}]
  variables: {V: x}
steps:
- name: S
  dependencies: [{environment: E, parameters: {A: "{{ [1] }}"}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "jobEnvironments[0] -> envParameterDefinitions[1]:\n\tduplicate parameter name: 'A'",
            "steps[0] -> dependencies[0] -> parameters -> A:\n\tFailed to parse interpolation expression at [0, 9]. Cannot coerce list[int] to int",
        ],
    );
}

#[test]
fn a_duplicated_job_environment_name_resolves_to_the_first() {
    // Only the duplicate is reported: the entry is checked against the
    // first `E`, which declares `P`.
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- {name: E, envParameterDefinitions: [{name: P, type: STRING}], variables: {V: x}}
- {name: E, envParameterDefinitions: [{name: Q, type: STRING}], variables: {V: x}}
steps:
- name: S
  dependencies: [{environment: E, parameters: {P: x}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &["jobEnvironments[1]:\n\tduplicate environment name: 'E'"],
    );
}

#[test]
fn constraint_7_reports_once_per_step_with_a_capped_list() {
    // Two Environments with required parameters; the Step supplies one.
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: L
  envParameterDefinitions: [{name: A, type: STRING}, {name: B, type: STRING}]
  variables: {V: x}
- name: M
  envParameterDefinitions: [{name: C, type: STRING}]
  variables: {V: x}
steps:
- name: S
  dependencies: [{environment: L, parameters: {B: b}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &["steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every jobEnvironments entry with activation ALWAYS, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7); missing: 'A' of Environment 'L'; 'C' of Environment 'M'."],
    );
    // Many missing parameters: one error per Step, listing 50.
    let envs: Vec<String> = (0..3)
        .map(|e| {
            let defs: Vec<String> = (0..20)
                .map(|d| format!("{{name: P{d}, type: STRING}}"))
                .collect();
            format!(
                "- {{name: E{e}, envParameterDefinitions: [{}], variables: {{V: x}}}}",
                defs.join(", ")
            )
        })
        .collect();
    let list = |e: usize, n: usize| {
        (0..n)
            .map(|d| format!("'P{d}'"))
            .collect::<Vec<_>>()
            .join(", ")
            + &format!(" of Environment 'E{e}'")
    };
    let expected = format!(
        "must supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every jobEnvironments entry with activation ALWAYS, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7); missing: {}; {}; {}; and 10 more.",
        list(0, 20),
        list(1, 20),
        list(2, 10)
    );
    let doc = format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\njobEnvironments:\n{}\nsteps:\n- name: S\n  script: {{actions: {{onRun: {{command: x}}}}}}\n- name: T\n  script: {{actions: {{onRun: {{command: x}}}}}}\n",
        envs.join("\n")
    );
    job_err(
        &doc,
        &[
            &format!("steps[0]:\n\t{expected}"),
            &format!("steps[1]:\n\t{expected}"),
        ],
    );
}

#[test]
fn environment_template_default_scope_excludes_session_and_env() {
    env_err(
        r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
parameterDefinitions: [{name: Img, type: STRING, default: u}]
environment:
  name: E
  envParameterDefinitions:
  - {name: A, type: STRING, default: "{{Session.WorkingDirectory}}"}
  - {name: B, type: STRING, default: "{{Env.Param.A}}"}
  - {name: C, type: STRING, default: "{{Job.Name}}"}
  - {name: D, type: STRING, default: "{{Param.Img}}"}
  variables: {V: x}
"#,
        &[
            "environment -> envParameterDefinitions[0] -> default:\n\tFailed to parse interpolation expression at [0, 28]. Undefined variable: 'Session.WorkingDirectory'.",
            "environment -> envParameterDefinitions[1] -> default:\n\tFailed to parse interpolation expression at [0, 15]. Undefined variable: 'Env.Param.A'.",
            "environment -> envParameterDefinitions[2] -> default:\n\tFailed to parse interpolation expression at [0, 12]. Undefined variable: 'Job.Name'.",
        ],
    );
}

#[test]
fn constraint_5_lists_the_declared_names_once_per_entry() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: A, type: STRING, default: a}, {name: B, type: STRING, default: b}]
  variables: {V: x}
steps:
- name: S
  dependencies: [{environment: E, parameters: {X: 1, Y: 2, Z: 3}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "steps[0] -> dependencies[0] -> parameters -> X:\n\tEnvironment 'E' declares no parameter 'X'. Declared: A, B.",
            "steps[0] -> dependencies[0] -> parameters -> Y:\n\tEnvironment 'E' declares no parameter 'Y'.",
            "steps[0] -> dependencies[0] -> parameters -> Z:\n\tEnvironment 'E' declares no parameter 'Z'.",
        ],
    );
}

#[test]
fn constraint_5_list_of_declared_names_is_capped_in_characters() {
    // 20 names of 64 characters exceed the 512-character list cap.
    let names: Vec<String> = (0..20)
        .map(|i| format!("{}{i:02}", "N".repeat(62)))
        .collect();
    let defs: Vec<String> = names
        .iter()
        .map(|n| format!("{{name: {n}, type: STRING, default: a}}"))
        .collect();
    // Seven names and their separators fit in 512 characters; an eighth
    // would not.
    let shown = names[..7].join(", ");
    job_err(
        &format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\njobEnvironments:\n- name: E\n  envParameterDefinitions: [{}]\n  variables: {{V: x}}\nsteps:\n- name: S\n  dependencies: [{{environment: E, parameters: {{Q: x}}}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n",
            defs.join(", ")
        ),
        &[&format!(
            "steps[0] -> dependencies[0] -> parameters -> Q:\n\tEnvironment 'E' declares no parameter 'Q'. Declared: {shown}, and 13 more."
        )],
    );
}

#[test]
fn constraint_7_list_is_capped_in_characters() {
    // 20 Environments, each with one 62-character parameter name and no
    // default. A group "'<name>' of Environment 'Ek'" is 84 characters,
    // so five groups and their separators fit in 512 and a sixth would not.
    let name = |e: usize| format!("{}{e:02}", "P".repeat(60));
    let envs: Vec<String> = (0..20)
        .map(|e| {
            format!(
                "- {{name: E{e:02}, envParameterDefinitions: [{{name: {}, type: STRING}}], variables: {{V: x}}}}",
                name(e)
            )
        })
        .collect();
    let shown: Vec<String> = (0..5)
        .map(|e| format!("'{}' of Environment 'E{e:02}'", name(e)))
        .collect();
    let doc = format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\njobEnvironments:\n{}\nsteps:\n- name: S\n  script: {{actions: {{onRun: {{command: x}}}}}}\n",
        envs.join("\n")
    );
    job_err(
        &doc,
        &[&format!(
            "steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every jobEnvironments entry with activation ALWAYS, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7); missing: {}; and 15 more.",
            shown.join("; ")
        )],
    );
}

#[test]
fn a_duplicated_parameter_name_types_env_param_by_its_first_definition() {
    // `Env.Param.X` is an INT, as for the entry, so `+ 1` is valid and the
    // only error is the duplicate.
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: X, type: INT, default: 1}, {name: X, type: STRING, default: a}]
  variables: {V: "{{ Env.Param.X + 1 }}"}
steps:
- name: S
  dependencies: [{environment: E, parameters: {X: 2}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &["jobEnvironments[0] -> envParameterDefinitions[1]:\n\tduplicate parameter name: 'X'"],
    );
}
