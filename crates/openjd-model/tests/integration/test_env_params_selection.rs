// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `ENV_PARAMS` Step-side selection at template validation: `activation`
//! (Template Schemas §4 item 4), the `environment:` entry's `enableIf`
//! (§3.2 item 5, constraint 9, the checked-at paragraph), constraint 7 over
//! each Step's Environment set, `Job.EnvNames` (§7.3.1), the `WRAP_ACTIONS`
//! single-layer rule over each Step's set (plan R8), and the
//! `<nonnegativefloatstring>` erratum (§3.3.1).
//!
//! Job creation of the same features is in
//! `test_env_params_selection_job.rs`.

use openjd_model::template::{EnvironmentActivation, ExprField};
use openjd_model::{decode_job_template, CallerLimits};

use crate::env_params_common::{env_err, env_ok, job_err, job_err_with, job_ok, yaml, ALLOW};

/// The constraint 7 message for the Job Template's own Environments.
fn must_supply(list: &str) -> String {
    format!(
        "must supply, in an 'environment:' dependency, every parameter without a default of each \
         Environment its Sessions enter (every jobEnvironments entry with activation ALWAYS, and each \
         one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 \
         constraint 7); missing: {list}."
    )
}

const SINGLE_LAYER: &str = "only one environment in the session stack may define any of \
                            onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit (RFC 0008).";

// ── activation (§4 item 4) ────────────────────────────────────────────

#[test]
fn activation_decodes_and_defaults_to_always() {
    let jt = job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- {name: A, variables: {V: v}}
- {name: B, activation: ALWAYS, variables: {V: v}}
- {name: C, activation: ON_REQUEST, variables: {V: v}}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
    );
    let envs = jt.job_environments.as_ref().unwrap();
    assert_eq!(envs[0].activation, None);
    assert_eq!(envs[0].activation(), EnvironmentActivation::Always);
    assert_eq!(envs[1].activation, Some(EnvironmentActivation::Always));
    assert_eq!(envs[2].activation(), EnvironmentActivation::OnRequest);
    assert_eq!(EnvironmentActivation::Always.as_str(), "ALWAYS");
    assert_eq!(EnvironmentActivation::OnRequest.as_str(), "ON_REQUEST");
    assert_eq!(
        EnvironmentActivation::default(),
        EnvironmentActivation::Always
    );
}

#[test]
fn an_unrecognized_activation_is_rejected() {
    // Constraint 1; the enum is case-sensitive, as other 2023-09 enums are
    // (plan A24), and the values before the rename (`ALL`,
    // `ONLY_REQUESTED`) are not recognized.
    for value in ["SOMETIMES", "always", "on_request", "ALL", "ONLY_REQUESTED"] {
        let src = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             jobEnvironments:\n- {{name: A, activation: {value}, variables: {{V: v}}}}\n\
             steps:\n- name: S\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        );
        let err = decode_job_template(yaml(&src), Some(ALLOW), &CallerLimits::default())
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            format!(
                "Validation error: 'jobtemplate-2023-09' failed checks: unknown variant \
                 `{value}`, expected `ALWAYS` or `ON_REQUEST`"
            )
        );
    }
}

#[test]
fn activation_requires_env_params() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
name: J
jobEnvironments:
- {name: A, activation: ON_REQUEST, variables: {V: v}}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
        &["jobEnvironments[0] -> activation:\n\tactivation on an Environment requires the ENV_PARAMS extension."],
    );
    env_err(
        r#"
specificationVersion: environment-2023-09
environment: {name: A, activation: ALWAYS, variables: {V: v}}
"#,
        &["environment -> activation:\n\tactivation on an Environment requires the ENV_PARAMS extension."],
    );
}

#[test]
fn activation_is_not_permitted_on_a_step_environment() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  stepEnvironments:
  - {name: A, activation: ALWAYS, variables: {V: v}}
  script: {actions: {onRun: {command: x}}}
"#,
        &["steps[0] -> stepEnvironments[0] -> activation:\n\tis not permitted on a stepEnvironments entry, which only its own Step's Sessions enter."],
    );
}

#[test]
fn an_environment_template_may_set_activation() {
    let et = env_ok(
        r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment: {name: A, activation: ON_REQUEST, variables: {V: v}}
"#,
    );
    assert_eq!(
        et.environment().activation(),
        EnvironmentActivation::OnRequest
    );
}

// ── Constraint 7 over each Step's Environment set ───────────────────

const ON_REQUEST_LICENSE: &str = r#"
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
"#;

#[test]
fn only_requesting_steps_supply_an_on_request_environments_parameters() {
    job_ok(ON_REQUEST_LICENSE);
    // A requesting Step that does not supply it is still rejected.
    job_err(
        &ON_REQUEST_LICENSE.replace(", parameters: {Product: maya}", ""),
        &[&format!(
            "steps[0]:\n\t{}",
            must_supply("'Product' of Environment 'Lic'")
        )],
    );
}

#[test]
fn an_all_environments_parameters_are_supplied_by_every_step() {
    let src = ON_REQUEST_LICENSE.replace("activation: ON_REQUEST", "activation: ALWAYS");
    job_err(
        &src,
        &[&format!(
            "steps[1]:\n\t{}",
            must_supply("'Product' of Environment 'Lic'")
        )],
    );
}

#[test]
fn constraint_7_lists_all_and_requested_environments_together() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- {name: A, envParameterDefinitions: [{name: X, type: STRING}], variables: {V: v}}
- {name: O, activation: ON_REQUEST, envParameterDefinitions: [{name: Y, type: STRING}], variables: {V: v}}
- {name: P, activation: ON_REQUEST, envParameterDefinitions: [{name: Z, type: STRING}], variables: {V: v}}
steps:
- name: S
  dependencies: [{environment: O}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[&format!(
            "steps[0]:\n\t{}",
            must_supply("'X' of Environment 'A'; 'Y' of Environment 'O'")
        )],
    );
}

// ── enableIf (§3.2 item 5, constraint 9) ────────────────────────────

const ENABLE_IF: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions:
- {name: Mode, type: STRING, default: "on"}
- {name: Count, type: INT, default: 1}
- {name: Dir, type: PATH, default: d}
jobEnvironments:
- name: Conda
  envParameterDefinitions: [{name: Packages, type: STRING, default: numpy}]
  variables: {V: v}
steps:
- name: S
  let: ["wanted = Param.Mode == 'on' and 'Docker' not in Job.EnvNames"]
  parameterSpace:
    taskParameterDefinitions: [{name: F, type: INT, range: "1-2"}]
  dependencies:
  - environment: Conda
    enableIf: ENABLE
  script: {actions: {onRun: {command: x}}}
"#;

fn enable_if(value: &str) -> String {
    ENABLE_IF.replace("ENABLE", value)
}

#[test]
fn enable_if_decodes_as_an_expression_field() {
    let dep_of = |jt: &openjd_model::template::JobTemplate| {
        jt.steps[0].dependencies.as_ref().unwrap()[0].clone()
    };
    let dep = dep_of(&job_ok(&enable_if("false")));
    assert_eq!(dep.enable_if, Some(ExprField::Bool(false)));
    assert_eq!(dep.literal_enabled(), Some(false));
    let dep = dep_of(&job_ok(&enable_if("true")));
    assert_eq!(dep.literal_enabled(), Some(true));
    let dep = dep_of(&job_ok(&enable_if(r#""wanted and Step.Name == 'S'""#)));
    assert_eq!(
        dep.enable_if,
        Some(ExprField::Expression("wanted and Step.Name == 'S'".into()))
    );
    assert_eq!(dep.literal_enabled(), None);
    // A string is never read as a scalar (§7.5 item 1): "true" is the
    // expression `true`, whose value is known only at job creation.
    let dep = dep_of(&job_ok(&enable_if(r#""true""#)));
    assert_eq!(dep.enable_if, Some(ExprField::Expression("true".into())));
    assert_eq!(dep.enable_if.as_ref().unwrap().as_bool(), None);
    assert_eq!(dep.literal_enabled(), None);
    let dep = dep_of(&job_ok(&ENABLE_IF.replace("    enableIf: ENABLE\n", "")));
    assert_eq!(dep.enable_if, None);
    assert_eq!(dep.literal_enabled(), Some(true));
}

#[test]
fn an_enable_if_that_is_not_a_scalar_or_a_string_is_a_decode_error() {
    for value in ["[true]", "{a: 1}"] {
        let err = decode_job_template(
            yaml(&enable_if(value)),
            Some(ALLOW),
            &CallerLimits::default(),
        )
        .unwrap_err()
        .to_string();
        assert_eq!(
            err,
            "Validation error: 'jobtemplate-2023-09' failed checks: enableIf must be a boolean, \
             integer, float, or string (an expression field, Template Schemas §7.5)"
        );
    }
}

#[test]
fn an_enable_if_scalar_must_be_a_boolean() {
    for (value, kind) in [("1", "integer scalar 1"), ("1.0", "float scalar 1.0")] {
        job_err(
            &enable_if(value),
            &[&format!(
                "steps[0] -> dependencies[0] -> enableIf:\n\tthe {kind} does not fit type bool: \
                 an expression field of type bool takes a boolean scalar or a string holding \
                 one expression (Template Schemas §7.5)."
            )],
        );
    }
}

#[test]
fn a_null_enable_if_is_an_error_not_an_omitted_field() {
    for value in ["null", "~", ""] {
        let src = enable_if(value);
        let dep_src = src.replace("enableIf: \n", "enableIf:\n");
        job_err(
            &dep_src,
            &["steps[0] -> dependencies[0] -> enableIf:\n\tmust not be null: an expression field \
               of type bool takes a boolean scalar or a string holding one expression (Template \
               Schemas §7.5)."],
        );
    }
}

#[test]
fn an_enable_if_written_as_a_format_string_is_a_parse_error() {
    // §7.5 rule 1, with the hint that an expression field takes no `{{ }}`.
    job_err(
        &enable_if(r#""{{ true }}""#),
        &[
            "steps[0] -> dependencies[0] -> enableIf:\n\tInvalid expression: Set literals are not \
           supported. An expression field takes an expression without '{{ }}' (Template \
           Schemas §7.5).\n  {{ true }}\n  ^~~~~~~~~~",
        ],
    );
    job_err(
        &enable_if(r#""x{{ true }}""#),
        &["steps[0] -> dependencies[0] -> enableIf:\n\tInvalid expression: Syntax error: invalid \
           syntax. An expression field takes an expression without '{{ }}' (Template Schemas \
           §7.5).\n  x{{ true }}\n   ^"],
    );
    // A `{{` inside a string literal is text, and the hint is not added to
    // an unrelated parse error.
    job_ok(&enable_if(r#""Param.Mode != '{{x}}'""#));
    job_err(
        &enable_if(r#""Param.Mode =""#),
        &["steps[0] -> dependencies[0] -> enableIf:\n\tInvalid expression: Syntax error: invalid syntax\n  Param.Mode =\n             ^"],
    );
}

#[test]
fn an_enable_if_value_is_reported_at_its_own_entry() {
    // Constraint 9: an expression field of type bool. The second entry's
    // path shows each entry is reported at its own index.
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- {name: A, variables: {V: v}}
- {name: B, variables: {V: v}}
steps:
- name: S
  dependencies: [{environment: A}, {environment: B, enableIf: "1"}]
  script: {actions: {onRun: {command: x}}}
"#,
        &["steps[0] -> dependencies[1] -> enableIf:\n\thas type int, which does not fit type bool; an expression field converts no type except int to float (Template Schemas §7.5)."],
    );
}

#[test]
fn enable_if_is_permitted_only_with_environment() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: A
  script: {actions: {onRun: {command: x}}}
- name: S
  dependencies: [{dependsOn: A, enableIf: true}]
  script: {actions: {onRun: {command: x}}}
"#,
        &["steps[1] -> dependencies[0] -> enableIf:\n\t'enableIf' is permitted only with 'environment'."],
    );
}

#[test]
fn an_enable_if_on_a_depends_on_entry_is_reported_once() {
    // Its value is not checked as well, so an int expression adds no
    // second error.
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: A
  script: {actions: {onRun: {command: x}}}
- name: S
  dependencies: [{dependsOn: A, enableIf: "1"}]
  script: {actions: {onRun: {command: x}}}
"#,
        &["steps[1] -> dependencies[0] -> enableIf:\n\t'enableIf' is permitted only with 'environment'."],
    );
}

#[test]
fn enable_if_must_fit_type_bool() {
    // §7.5 rule 2: no conversion to `bool` (JT-97, JT-149).
    job_err(
        &enable_if(r#""Param.Count""#),
        &[
            "steps[0] -> dependencies[0] -> enableIf:\n\thas type int, which does not fit type \
           bool; an expression field converts no type except int to float (Template Schemas \
           §7.5).",
        ],
    );
    job_err(
        &enable_if(r#""Param.Mode""#),
        &[
            "steps[0] -> dependencies[0] -> enableIf:\n\thas type string, which does not fit type \
           bool; an expression field converts no type except int to float (Template Schemas \
           §7.5).",
        ],
    );
    job_err(
        &enable_if(r#""Param.Mode if Param.Count > 0 else true""#),
        &[
            "steps[0] -> dependencies[0] -> enableIf:\n\thas type bool | string, which does not \
           fit type bool; an expression field converts no type except int to float (Template \
           Schemas §7.5).",
        ],
    );
    // An expression of type nulltype is an error...
    job_err(
        &enable_if(r#""null""#),
        &[
            "steps[0] -> dependencies[0] -> enableIf:\n\thas type nulltype, which does not fit \
           type bool (Template Schemas §7.5).",
        ],
    );
    // ...while `bool?` passes template validation; job creation rejects a
    // `null` result (§7.5 rule 2).
    job_ok(&enable_if(r#""null if Param.Count > 5 else true""#));
    // A constant expression of type bool is accepted.
    job_ok(&enable_if(r#""false""#));
}

#[test]
fn enable_if_has_the_scope_of_a_parameters_value() {
    job_err(
        &enable_if(r#""Task.Param.F > 1""#),
        &[
            "steps[0] -> dependencies[0] -> enableIf:\n\tInvalid expression: Undefined variable: \
           'Task.Param.F'.\n  Task.Param.F > 1\n  ~~~~~~~~~~~^",
        ],
    );
    job_err(
        &enable_if(r#""len(Param.Dir) > 0""#),
        &[
            "steps[0] -> dependencies[0] -> enableIf:\n\tInvalid expression: Undefined variable: \
           'Param.Dir'. Did you mean: RawParam.Dir\n  len(Param.Dir) > 0\n      ~~~~~~^~~",
        ],
    );
    job_ok(&enable_if(
        r#""wanted and Step.Name == 'S' and Job.Name == 'J' and len(RawParam.Dir) > 0""#,
    ));
}

#[test]
fn enable_if_is_checked_with_the_template_scope_library_and_posix_paths() {
    // A host-only function is not in the template-scope library (§3.2
    // item 5: the scope of a parameters value)...
    job_err(
        &enable_if(r#""apply_path_mapping('/a') == '/a'""#),
        &["steps[0] -> dependencies[0] -> enableIf:\n\tInvalid expression: Unknown function: 'apply_path_mapping'\n  apply_path_mapping('/a') == '/a'\n  ^~~~~~~~~~~~~~~~~~~~~~~~"],
    );
    // ...and template validation evaluates under POSIX path rules
    // whatever the host, where a backslash is an ordinary name character.
    job_ok(&enable_if(
        r#""path('a/b').with_name('x\\y') == path('q')""#,
    ));
}

#[test]
fn constraint_3_applies_to_entries_as_written() {
    job_err(
        &ENABLE_IF.replace(
            "    enableIf: ENABLE\n",
            "    enableIf: \"wanted\"\n  - environment: Conda\n    enableIf: \"not wanted\"\n",
        ),
        &["steps[0] -> dependencies[1] -> environment:\n\tduplicate environment dependency 'Conda'."],
    );
}

#[test]
fn a_literal_false_entry_is_as_if_absent() {
    // Constraint 5 does not apply to it (plan A28)...
    job_ok(&ENABLE_IF.replace(
        "    enableIf: ENABLE\n",
        "    enableIf: false\n    parameters: {Nope: x}\n",
    ));
    // ...and for an ALWAYS Environment it supplies nothing, so a parameter
    // without a default must be supplied by an enabled entry.
    let src = ENABLE_IF
        .replace(
            "{name: Packages, type: STRING, default: numpy}",
            "{name: Packages, type: STRING}",
        )
        .replace(
            "    enableIf: ENABLE\n",
            "    enableIf: false\n    parameters: {Packages: x}\n",
        );
    job_err(
        &src,
        &[&format!(
            "steps[0]:\n\t{}",
            must_supply(
                "'Packages' of Environment 'Conda' (the Step's 'environment:' dependency for \
                 it is disabled by its enableIf, so it supplies nothing; Template Schemas §3.2 \
                 item 5)"
            )
        )],
    );
    // For an ON_REQUEST Environment it requests nothing.
    job_ok(&src.replace(
        "- name: Conda\n",
        "- name: Conda\n  activation: ON_REQUEST\n",
    ));
    // Constraint 4 still applies to it as written.
    job_err(
        &ENABLE_IF.replace("    enableIf: ENABLE\n", "    enableIf: false\n    parameters: {}\n"),
        &["steps[0] -> dependencies[0] -> parameters:\n\tif provided, must contain at least one entry."],
    );
}

#[test]
fn a_deferred_environment_is_not_listed_as_missing() {
    // The Step's entry for A has an expression `enableIf`, so only B's
    // parameter is reported.
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- {name: A, envParameterDefinitions: [{name: a, type: STRING}], variables: {V: v}}
- {name: B, envParameterDefinitions: [{name: b, type: STRING}], variables: {V: v}}
steps:
- name: S
  dependencies: [{environment: A, enableIf: "true"}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[&format!(
            "steps[0]:\n\t{}",
            must_supply("'b' of Environment 'B'")
        )],
    );
}

#[test]
fn constraint_7_counts_the_first_of_duplicate_entries() {
    // The duplicate is reported (constraint 3); the first entry supplies the
    // parameter, so constraint 7 adds nothing.
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- {name: A, envParameterDefinitions: [{name: a, type: STRING}], variables: {V: v}}
steps:
- name: S
  dependencies: [{environment: A, parameters: {a: x}}, {environment: A}]
  script: {actions: {onRun: {command: x}}}
"#,
        &["steps[0] -> dependencies[1] -> environment:\n\tduplicate environment dependency 'A'."],
    );
}

#[test]
fn constraint_7_is_deferred_for_an_expression_enable_if() {
    // Whether the entry is enabled is known only when the Job is created.
    let src = ENABLE_IF
        .replace(
            "{name: Packages, type: STRING, default: numpy}",
            "{name: Packages, type: STRING}",
        )
        .replace("ENABLE", r#""wanted""#);
    job_ok(&src);
    job_ok(&src.replace(
        "- name: Conda\n",
        "- name: Conda\n  activation: ON_REQUEST\n",
    ));
}

// ── Job.EnvNames (§7.3.1) ───────────────────────────────────────────

fn env_names_in(field_yaml: &str, exts: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [{exts}]\nname: J\n\
         jobEnvironments:\n- {{name: E, variables: {{V: v}}}}\nsteps:\n- name: S\n{field_yaml}\n  \
         script: {{actions: {{onRun: {{command: x, args: [\"{{{{ Job.EnvNames }}}}\"]}}}}}}\n"
    )
}

#[test]
fn job_env_names_is_a_list_of_strings() {
    job_ok(&env_names_in(
        "  let: [\"n = len(Job.EnvNames)\", \"s = Job.EnvNames.join(',')\"]",
        "ENV_PARAMS, EXPR",
    ));
    job_err(
        &env_names_in("  let: [\"u = Job.EnvNames.upper()\"]", "ENV_PARAMS, EXPR"),
        &["steps[0] -> let[0]:\n\tInvalid expression in let binding 'u': upper() is not available for list[string]. Available for: string"],
    );
}

#[test]
fn job_env_names_requires_env_params() {
    job_err(
        &env_names_in("  let: [\"n = len(Job.EnvNames)\"]", "EXPR"),
        &[
            "steps[0] -> let[0]:\n\tInvalid expression in let binding 'n': Undefined variable: 'Job.EnvNames'. Did you mean: Job.Name",
            "steps[0] -> script -> actions -> onRun -> args[0]:\n\tFailed to parse interpolation expression at [0, 18]. Undefined variable: 'Job.EnvNames'. Did you mean: Job.Name",
        ],
    );
}

#[test]
fn job_env_names_is_not_in_a_default_or_the_job_name() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: "J{{ len(Job.EnvNames) }}"
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: N, type: "LIST[STRING]", default: "{{ Job.EnvNames }}"}]
  variables: {V: v}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "name:\n\tFailed to parse interpolation expression at [1, 24]. Undefined variable: 'Job.EnvNames'.",
            "jobEnvironments[0] -> envParameterDefinitions[0] -> default:\n\tFailed to parse interpolation expression at [0, 18]. Undefined variable: 'Job.EnvNames'.",
        ],
    );
}

#[test]
fn job_env_names_is_in_every_other_job_creation_and_session_scope() {
    job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR, FEATURE_BUNDLE_1]
name: J
jobEnvironments:
- name: E
  script:
    let: ["n = Job.EnvNames"]
    actions:
      onEnter: {command: x, args: ["{{ Job.EnvNames }}"], timeout: "{{ len(Job.EnvNames) + 1 }}"}
  variables: {V: "{{ Job.EnvNames.join(',') }}"}
steps:
- name: S
  let: ["k = len(Job.EnvNames)"]
  hostRequirements:
    amounts: [{name: amount.worker.vcpu, min: "{{ len(Job.EnvNames) }}"}]
  parameterSpace:
    taskParameterDefinitions: [{name: F, type: INT, range: "{{ [len(Job.EnvNames)] }}"}]
  dependencies:
  - {environment: E, enableIf: "'E' in Job.EnvNames"}
  stepEnvironments:
  - name: SE
    variables: {V: "{{ Job.EnvNames.join(',') }}"}
  script: {actions: {onRun: {command: x, args: ["{{ Job.EnvNames }}"]}}}
"#,
    );
    let err = |src: &str, hint: &str| {
        env_err(
            src,
            &[&format!("environment -> script -> actions -> onEnter -> args[0]:\n\tFailed to parse interpolation expression at [0, 18]. Undefined variable: 'Job.EnvNames'.{hint}")],
        )
    };
    let et = |exts: &str| {
        format!(
            "specificationVersion: environment-2023-09\nextensions: [{exts}]\nenvironment:\n  \
             name: A\n  script:\n    actions:\n      onEnter: {{command: x, args: [\"{{{{ Job.EnvNames }}}}\"]}}\n"
        )
    };
    env_ok(&et("ENV_PARAMS, EXPR"));
    // Also in an Environment Template's job-creation fields.
    env_ok(
        r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR, FEATURE_BUNDLE_1]
environment:
  name: A
  script:
    actions:
      onEnter: {command: x, timeout: "{{ len(Job.EnvNames) + 1 }}"}
"#,
    );
    err(&et("EXPR"), " Did you mean: Job.Name");
}

// ── WRAP_ACTIONS single-layer rule over each Step's set (plan R8) ───

fn wrap_env(name: &str, scope: &str) -> String {
    format!(
        "- name: {name}\n  {scope}\n  script:\n    actions:\n      onWrapEnvEnter: {{command: x}}\n      \
         onWrapTaskRun: {{command: x}}\n      onWrapEnvExit: {{command: x}}\n"
    )
}

fn wrap_job(envs: &[String], steps: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR, WRAP_ACTIONS]\n\
         name: J\njobEnvironments:\n{}steps:\n{steps}",
        envs.concat()
    )
}

const RUN: &str = "  script: {actions: {onRun: {command: x}}}\n";

#[test]
fn on_request_wrappers_requested_by_different_steps_are_one_layer_each() {
    let envs = [
        wrap_env("W1", "activation: ON_REQUEST"),
        wrap_env("W2", "activation: ON_REQUEST"),
    ];
    job_ok(&wrap_job(
        &envs,
        &format!(
            "- name: A\n  dependencies: [{{environment: W1}}]\n{RUN}\
             - name: B\n  dependencies: [{{environment: W2}}]\n{RUN}- name: C\n{RUN}"
        ),
    ));
    // The same Step requesting both has two layers.
    job_err(
        &wrap_job(
            &envs,
            &format!(
                "- name: A\n  dependencies: [{{environment: W1}}, {{environment: W2}}]\n{RUN}"
            ),
        ),
        &[&format!("steps[0] -> dependencies:\n\t{SINGLE_LAYER}")],
    );
}

#[test]
fn a_requested_wrapper_adds_to_the_all_and_step_wrappers() {
    let envs = [
        wrap_env("All", "activation: ALWAYS"),
        wrap_env("Only", "activation: ON_REQUEST"),
    ];
    // ALWAYS + requested ON_REQUEST.
    job_err(
        &wrap_job(
            &envs,
            &format!("- name: A\n  dependencies: [{{environment: Only}}]\n{RUN}- name: B\n{RUN}"),
        ),
        &[&format!("steps[0] -> dependencies:\n\t{SINGLE_LAYER}")],
    );
    // The location does not depend on Step Environments that add no layer.
    job_err(
        &wrap_job(
            &envs,
            &format!(
                "- name: A\n  dependencies: [{{environment: Only}}]\n  stepEnvironments:\n  \
                 - {{name: Plain, variables: {{V: v}}}}\n{RUN}"
            ),
        ),
        &[&format!("steps[0] -> dependencies:\n\t{SINGLE_LAYER}")],
    );
    // Requested ON_REQUEST + a Step Environment wrapper.
    let step_wrap = wrap_env("SW", "description: step").replace('\n', "\n  ");
    job_err(
        &wrap_job(
            &envs[1..],
            &format!(
                "- name: A\n  dependencies: [{{environment: Only}}]\n  stepEnvironments:\n  {}\n{RUN}",
                step_wrap.trim_end()
            ),
        ),
        &[&format!("steps[0] -> stepEnvironments:\n\t{SINGLE_LAYER}")],
    );
}

#[test]
fn a_wrapper_an_entry_does_not_definitely_request_is_not_counted() {
    let envs = [
        wrap_env("W1", "activation: ON_REQUEST"),
        wrap_env("W2", "activation: ON_REQUEST"),
    ];
    // A literal `false` entry requests nothing; an expression one is
    // checked at job creation.
    job_ok(&wrap_job(
        &envs,
        &format!(
            "- name: A\n  dependencies: [{{environment: W1}}, {{environment: W2, enableIf: false}}]\n{RUN}\
             - name: B\n  dependencies: [{{environment: W1}}, {{environment: W2, enableIf: \"true\"}}]\n{RUN}"
        ),
    ));
}

#[test]
fn two_all_wrappers_are_also_reported_at_step_environments_that_add_none() {
    // As before ENV_PARAMS: the Step's stack has two layers.
    job_err_with(
        &wrap_job(
            &[
                wrap_env("W1", "activation: ALWAYS"),
                wrap_env("W2", "activation: ALWAYS"),
            ],
            &format!(
                "- name: A\n  stepEnvironments:\n  - {{name: Plain, variables: {{V: v}}}}\n{RUN}"
            ),
        ),
        ALLOW,
        &[
            &format!("jobEnvironments:\n\t{SINGLE_LAYER}"),
            &format!("steps[0] -> stepEnvironments:\n\t{SINGLE_LAYER}"),
        ],
    );
}

#[test]
fn two_all_wrappers_are_still_reported_at_job_environments() {
    job_err_with(
        &wrap_job(
            &[
                wrap_env("W1", "activation: ALWAYS"),
                wrap_env("W2", "variables: {V: v}"),
            ],
            &format!("- name: A\n{RUN}"),
        ),
        ALLOW,
        &[&format!("jobEnvironments:\n\t{SINGLE_LAYER}")],
    );
}

// ── <nonnegativefloatstring> erratum (§3.3.1; N8) ───────────────────

#[test]
fn an_amount_min_of_zero_is_valid() {
    job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [FEATURE_BUNDLE_1]
name: J
steps:
- name: S
  hostRequirements:
    amounts: [{name: amount.worker.gpu, min: "0"}, {name: amount.worker.vcpu, min: 0}]
  script: {actions: {onRun: {command: x}}}
"#,
    );
}
