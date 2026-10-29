// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `ENV_PARAMS` schema, extension gating, and structural checks
//! (prototype plan PR 1).
//!
//! Covers: the `ENV_PARAMS` extension name, its gate, and its `EXPR`
//! prerequisite (§1.1 item 3, §1.2 item 3, RFC Backward compatibility);
//! `$schema` on an Environment Template (§1.2 item 2);
//! `<Environment>.envParameterDefinitions` (§4 item 3 constraints 1–4) and
//! `<EnvironmentParameterDefinition>` (§4.5 items 1–5, literal `default`s per
//! "checked at two points" point 1); the `environment:` form of
//! `<StepDependency>` and its constraints 1–4 and 8 (§3.2); the literal-value
//! forms no parameter type accepts (§3.2 last paragraph; plan A4); and that
//! `environment:` entries never reach the job's Step dependency graph (§3.2
//! item 2).

use super::env_params_common::*;
use openjd_model::template::{
    parse, DependencyTarget, EnvironmentParameterValue, StepDependency, StepKey,
};
use openjd_model::{
    create_job, JobParameterValues, ModelExtension, StepDependencyGraph, ValidationContext,
};
use std::str::FromStr;

// ── Extension name and gating ──────────────────────────────────────

#[test]
fn extension_name_round_trips_and_is_in_all() {
    let ext = ModelExtension::from_str("ENV_PARAMS").unwrap();
    assert_eq!(ext, ModelExtension::EnvParams);
    assert_eq!(ext.as_str(), "ENV_PARAMS");
    // In ALL now that the CLI supports it, so the default allowlists
    // (CLI, openjd-for-js) accept ENV_PARAMS templates.
    assert!(ModelExtension::ALL.contains(&ModelExtension::EnvParams));
}

#[test]
fn default_allowlist_accepts_env_params() {
    let all: Vec<&str> = ModelExtension::ALL.iter().map(|e| e.as_str()).collect();
    let value = parse::document_string_to_object(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
        parse::DocumentType::Yaml,
        &openjd_model::CallerLimits::default(),
    )
    .unwrap();
    parse::decode_job_template(value, Some(&all), &openjd_model::CallerLimits::default())
        .expect("the default allowlist includes ENV_PARAMS");
}

#[test]
fn environment_parameter_definitions_require_the_extension() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: A, type: STRING, default: a}]
  variables: {V: "{{Env.Param.A}}"}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
        &["jobEnvironments[0] -> envParameterDefinitions:\n\tenvParameterDefinitions on an Environment requires the ENV_PARAMS extension."],
    );
    env_err(
        r#"
specificationVersion: environment-2023-09
environment:
  name: E
  envParameterDefinitions: [{name: A, type: STRING, default: a}]
  variables: {V: x}
"#,
        &["environment -> envParameterDefinitions:\n\tenvParameterDefinitions on an Environment requires the ENV_PARAMS extension."],
    );
}

#[test]
fn environment_dependency_requires_the_extension() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
name: J
steps:
- name: S
  dependencies:
  - environment: Conda
    required: false
    parameters: {Packages: x}
  script: {actions: {onRun: {command: x}}}
"#,
        &["steps[0] -> dependencies[0]:\n\tthe 'environment' form of a dependency requires the ENV_PARAMS extension."],
    );
}

#[test]
fn without_the_extension_a_dependency_needs_depends_on() {
    // Without ENV_PARAMS the only form is `dependsOn`, and the message
    // says so instead of naming a form the template cannot use.
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
name: J
steps:
- name: S
  dependencies: [{required: true}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "steps[0] -> dependencies[0]:\n\tmust have 'dependsOn'.",
            "steps[0] -> dependencies[0] -> required:\n\t'required' is permitted only with 'environment', which requires the ENV_PARAMS extension.",
        ],
    );
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
name: J
steps:
- name: A
  script: {actions: {onRun: {command: x}}}
- name: S
  dependencies: [{dependsOn: A, parameters: {X: 1}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &["steps[1] -> dependencies[0] -> parameters:\n\t'parameters' is permitted only with 'environment', which requires the ENV_PARAMS extension."],
    );
}

/// A Job Template that lists `exts` and uses only STRING, INT, FLOAT,
/// and PATH Environment parameters, with `extra` appended at the root.
fn base_types_job(exts: &str, extra: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [{exts}]\nname: J\n\
         jobEnvironments:\n- name: E\n  envParameterDefinitions:\n  \
         - {{name: S, type: STRING, default: a}}\n  - {{name: I, type: INT, default: 1}}\n  \
         - {{name: F, type: FLOAT, default: 1.5}}\n  - {{name: P, type: PATH, default: /x}}\n  \
         variables: {{V: \"{{{{Env.Param.S}}}} {{{{Env.Param.I}}}} {{{{Env.Param.F}}}} {{{{Env.RawParam.P}}}}\"}}\n\
         steps:\n- name: S\n  script: {{actions: {{onRun: {{command: x}}}}}}\n{extra}"
    )
}

const JT_NEEDS_EXPR: &str = "extensions:\n\tENV_PARAMS requires EXPR; both must be listed in the \
                             template's `extensions` (Template Schemas §1.1 item 3).";
const ET_NEEDS_EXPR: &str = "extensions:\n\tENV_PARAMS requires EXPR; both must be listed in the \
                             template's `extensions` (Template Schemas §1.2 item 3).";

#[test]
fn env_params_requires_expr() {
    // §1.1 item 3: a document that lists ENV_PARAMS must also list EXPR,
    // even when it uses only the types available without EXPR.
    job_ok(&base_types_job("ENV_PARAMS, EXPR", ""));
    job_err(&base_types_job("ENV_PARAMS", ""), &[JT_NEEDS_EXPR]);
    job_err(
        &base_types_job("ENV_PARAMS, FEATURE_BUNDLE_1", ""),
        &[JT_NEEDS_EXPR],
    );
    // The rule is on listing the extension: it holds with no ENV_PARAMS
    // field in use (conformance JT-161).
    job_err(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS]\nname: J\n\
         steps:\n- name: S\n  script: {actions: {onRun: {command: x}}}\n",
        &[JT_NEEDS_EXPR],
    );
    // §1.2 item 3: an Environment Template has the same prerequisites.
    let et = |exts: &str| {
        format!(
            "specificationVersion: environment-2023-09\nextensions: [{exts}]\n\
             environment:\n  name: E\n  envParameterDefinitions: [{{name: S, type: STRING, \
             default: a}}]\n  variables: {{V: \"{{{{Env.Param.S}}}}\"}}\n"
        )
    };
    env_ok(&et("ENV_PARAMS, EXPR"));
    env_err(&et("ENV_PARAMS"), &[ET_NEEDS_EXPR]);
}

#[test]
fn env_params_without_expr_is_reported_alone() {
    // Every other ENV_PARAMS rule assumes EXPR, so the prerequisite is
    // checked before any validation pass: the document's other mistakes
    // (an undefined parameter and a list value for a STRING parameter) are
    // not reported with it.
    let extra = "- name: T\n  dependencies: [{environment: E, enableIf: true, \
                 parameters: {S: [a]}}]\n  script: {actions: {onRun: {command: \
                 \"{{Param.Nope}}\"}}}\n";
    job_err(&base_types_job("ENV_PARAMS", extra), &[JT_NEEDS_EXPR]);
    // With EXPR the same document reports those mistakes.
    job_err(
        &base_types_job("ENV_PARAMS, EXPR", extra),
        &[
            "steps[1] -> script -> actions -> onRun -> command:\n\tFailed to parse interpolation \
             expression at [0, 14]. Undefined variable: 'Param.Nope'.\n  Param.Nope\n  ~~~~~~^~~~",
            "steps[1] -> dependencies[0] -> parameters -> S:\n\tis not a valid STRING value: \
             invalid type: sequence, expected a string",
        ],
    );
    // The same for an Environment Template: the enableIf that does not fit
    // bool is not reported with it.
    env_err(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS]\n\
         environment: {name: E, enableIf: 1, variables: {V: v}}\n",
        &[ET_NEEDS_EXPR],
    );
    // WRAP_ACTIONS has the same prerequisite, about the same list, so both
    // errors are reported.
    job_err(
        &base_types_job("ENV_PARAMS, WRAP_ACTIONS", ""),
        &[
            JT_NEEDS_EXPR,
            "extensions:\n\tWRAP_ACTIONS requires EXPR; both must be listed in the template's \
             `extensions` (RFC 0008).",
        ],
    );
    env_err(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, WRAP_ACTIONS]\n\
         environment: {name: E, enableIf: 1, variables: {V: v}}\n",
        &[
            ET_NEEDS_EXPR,
            "extensions:\n\tWRAP_ACTIONS requires EXPR; both must be listed in the template's \
             `extensions` (RFC 0008).",
        ],
    );
}

#[test]
fn a_document_without_env_params_is_not_held_to_the_prerequisite() {
    // A Job Template that lists neither extension keeps its meaning; one
    // that lists only EXPR is valid too.
    for exts in ["", "extensions: [EXPR]\n"] {
        job_ok(&format!(
            "specificationVersion: jobtemplate-2023-09\n{exts}name: J\n\
             steps:\n- name: S\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        ));
    }
}

#[test]
fn environment_template_accepts_schema() {
    let et = env_ok(
        r#"
specificationVersion: environment-2023-09
$schema: "https://example.com/schema.json"
environment: {name: E, variables: {V: x}}
"#,
    );
    assert_eq!(
        et.schema.as_deref(),
        Some("https://example.com/schema.json")
    );
}

// ── <Environment>.envParameterDefinitions (§4 item 3) ─────────────────

/// The decode error for an unknown key `field` on an `<Environment>` of a
/// `doc` document.
fn unknown_env_field(doc: &str, field: &str) -> String {
    format!(
        "Validation error: '{doc}' failed checks: unknown field `{field}`, expected one of \
         `name`, `description`, `envParameterDefinitions`, `activation`, `hostRequirements`, \
         `enableIf`, `script`, `variables`"
    )
}

#[test]
fn the_names_before_the_rename_are_unknown_fields_on_an_environment() {
    // §4 items 3 and 4 name the fields `envParameterDefinitions` and
    // `activation`; no alias keeps `parameterDefinitions` or `runScope`.
    for (field, value) in [
        (
            "parameterDefinitions",
            "[{name: A, type: STRING, default: a}]",
        ),
        ("runScope", "ALL"),
    ] {
        let job = format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\n\
             jobEnvironments:\n- {{name: E, {field}: {value}, variables: {{V: x}}}}\n\
             steps:\n- name: S\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        );
        let err = openjd_model::decode_job_template(
            yaml(&job),
            Some(ALLOW),
            &openjd_model::CallerLimits::default(),
        )
        .unwrap_err()
        .to_string();
        assert_eq!(err, unknown_env_field("jobtemplate-2023-09", field));
        let env = format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
             environment: {{name: E, {field}: {value}, variables: {{V: x}}}}\n"
        );
        let err = openjd_model::decode_environment_template(
            yaml(&env),
            Some(ALLOW),
            &openjd_model::CallerLimits::default(),
        )
        .unwrap_err()
        .to_string();
        assert_eq!(err, unknown_env_field("environment-2023-09", field));
    }
    // The Environment Template's own top-level `parameterDefinitions`
    // (§1.2) keeps its name, beside its Environment's renamed field.
    let et = env_ok(
        r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
parameterDefinitions: [{name: J, type: STRING, default: j}]
environment:
  name: E
  envParameterDefinitions: [{name: A, type: STRING, default: "{{Param.J}}"}]
  variables: {V: "{{Env.Param.A}}"}
"#,
    );
    assert_eq!(et.parameter_definitions.as_ref().unwrap()[0].name(), "J");
    assert_eq!(
        et.environment().env_parameter_definitions.as_ref().unwrap()[0].name(),
        "A"
    );
}

#[test]
fn parameter_definitions_must_not_be_empty() {
    env_err(
        r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment: {name: E, envParameterDefinitions: [], variables: {V: x}}
"#,
        &["environment -> envParameterDefinitions:\n\tif provided, must contain at least one element."],
    );
}

#[test]
fn parameter_definitions_at_most_fifty() {
    let defs: Vec<String> = (0..51)
        .map(|i| format!("  - {{name: P{i}, type: STRING, default: a}}"))
        .collect();
    let doc = format!(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\nenvironment:\n  name: E\n  variables: {{V: x}}\n  envParameterDefinitions:\n{}\n",
        defs.join("\n")
    );
    env_err(
        &doc,
        &["environment -> envParameterDefinitions:\n\tmust not contain more than 50 elements."],
    );
    // Fifty is accepted.
    let doc50 = doc.replace("  - {name: P50, type: STRING, default: a}\n", "");
    env_ok(&doc50);
}

#[test]
fn feature_bundle_1_does_not_raise_the_fifty_caps() {
    // FEATURE_BUNDLE_1 raises the Job Template's own parameter limit to
    // 200, but not these two (§4 item 3 constraint 2, §3.2 constraint 4).
    let defs: Vec<String> = (0..51)
        .map(|i| format!("  - {{name: D{i}, type: STRING, default: a}}"))
        .collect();
    let params: Vec<String> = (0..51).map(|i| format!("P{i}: v")).collect();
    job_err(
        &format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR, FEATURE_BUNDLE_1]\nname: J\njobEnvironments:\n- name: E\n  variables: {{V: x}}\n  envParameterDefinitions:\n{}\nsteps:\n- name: S\n  dependencies:\n  - environment: Ext\n    parameters: {{{}}}\n  script: {{actions: {{onRun: {{command: x}}}}}}\n",
            defs.join("\n"),
            params.join(", ")
        ),
        &[
            "jobEnvironments[0] -> envParameterDefinitions:\n\tmust not contain more than 50 elements.",
            "steps[0] -> dependencies[0] -> parameters:\n\tmust not contain more than 50 entries.",
        ],
    );
}

#[test]
fn parameter_names_unique_but_may_match_job_parameters_and_other_environments() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: A, type: STRING, default: a}
  - {name: A, type: INT, default: 1}
  variables: {V: x}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
        &["jobEnvironments[0] -> envParameterDefinitions[1]:\n\tduplicate parameter name: 'A'"],
    );
    job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: A, type: STRING, default: j}]
jobEnvironments:
- name: E1
  envParameterDefinitions: [{name: A, type: STRING, default: "{{Param.A}}"}]
  variables: {V: "{{Env.Param.A}}"}
- name: E2
  envParameterDefinitions: [{name: A, type: INT, default: 1}]
  variables: {V: "{{Env.Param.A}}"}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
    );
}

#[test]
fn parameter_definitions_not_permitted_on_step_environments() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  stepEnvironments:
  - name: SE
    envParameterDefinitions: [{name: A, type: STRING, default: a}]
    variables: {V: x}
  script: {actions: {onRun: {command: x}}}
"#,
        &["steps[0] -> stepEnvironments[0] -> envParameterDefinitions:\n\tis not permitted on a stepEnvironments entry; only a jobEnvironments entry or an Environment Template's environment may declare parameters."],
    );
}

#[test]
fn parameter_name_length_follows_identifier_limit() {
    let long = "A".repeat(65);
    job_err(
        &format!(
            r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{{name: {long}, type: STRING, default: a}}]
  variables: {{V: x}}
steps:
- name: S
  script: {{actions: {{onRun: {{command: x}}}}}}
"#
        ),
        &["jobEnvironments[0] -> envParameterDefinitions[0]:\n\tname exceeds 64 characters."],
    );
}

// ── <EnvironmentParameterDefinition> (§4.5) ────────────────────────

#[test]
fn every_job_parameter_type_is_allowed() {
    // §4.5 item 1: any type a `<JobParameterDefinition>` has in a document
    // that lists EXPR, RANGE_EXPR included.
    job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: B, type: BOOL, default: "on"}
  - {name: R, type: RANGE_EXPR, default: "1-10"}
  - {name: LS, type: "LIST[STRING]", default: [a, b]}
  - {name: LP, type: "LIST[PATH]", default: []}
  - {name: LI, type: "LIST[INT]", default: [1, "2"]}
  - {name: LF, type: "LIST[FLOAT]", default: [1.5]}
  - {name: LB, type: "LIST[BOOL]", default: [true, 0]}
  - {name: LL, type: "LIST[LIST[INT]]", default: [[1, 2], ["3"]]}
  variables: {V: x}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
    );
    // A literal RANGE_EXPR default is checked against the type, and an
    // entry value too.
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: R, type: RANGE_EXPR, default: "3-1"}]
  variables: {V: x}
steps:
- name: S
  dependencies: [{environment: E, parameters: {R: "1-"}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "jobEnvironments[0] -> envParameterDefinitions[0] -> default:\n\tis not a valid RANGE_EXPR value: Value '3-1' is not a valid range expression: Descending range 3-1 requires a negative step",
            "steps[0] -> dependencies[0] -> parameters -> R:\n\tis not a valid RANGE_EXPR value: Value '1-' is not a valid range expression: Unexpected end of expression: '1-'",
        ],
    );
}

#[test]
fn type_names_are_case_insensitive() {
    // §2: with EXPR, which ENV_PARAMS requires, type names are
    // case-insensitive, for Environment parameters as for Job Parameters.
    job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: A, type: string, default: a}]
  variables: {V: x}
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
  envParameterDefinitions: [{name: A, type: Path, default: /a}]
  variables: {V: x}
"#,
    );
    // Without EXPR the document cannot list ENV_PARAMS, so a lowercase
    // name gets the prerequisite error, not a case error, and without
    // ENV_PARAMS the definitions are rejected outright.
    let doc = |exts: &str| {
        format!(
            "specificationVersion: environment-2023-09\n{exts}environment:\n  name: E\n  \
             envParameterDefinitions: [{{name: A, type: Path, default: /a}}]\n  variables: {{V: x}}\n"
        )
    };
    env_err(&doc("extensions: [ENV_PARAMS]\n"), &[ET_NEEDS_EXPR]);
    env_err(
        &doc(""),
        &[
            "environment -> envParameterDefinitions:\n\tenvParameterDefinitions on an Environment \
           requires the ENV_PARAMS extension.",
        ],
    );
}

#[test]
fn user_interface_object_type_and_data_flow_are_permitted() {
    // §4.5 items 2-3: permitted, and informational.
    let jt = job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: A, type: STRING, default: a, userInterface: {control: LINE_EDIT, label: Name}}
  - {name: P, type: PATH, default: /a, objectType: FILE, dataFlow: IN}
  - {name: LP, type: "LIST[PATH]", default: [], objectType: DIRECTORY, dataFlow: OUT}
  variables: {V: x}
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
  envParameterDefinitions:
  - {name: A, type: STRING, default: a, userInterface: {control: HIDDEN}}
  - {name: P, type: PATH, default: /a, objectType: DIRECTORY, dataFlow: INOUT}
  variables: {V: x}
"#,
    );
    // They change nothing about the Job: it has the same Job Parameters
    // and the same values as without them.
    let job = openjd_model::create_job(
        &jt,
        &crate::env_params_common::job_params(&jt, &[]),
        &crate::env_params_common::posix_ctx(&jt),
    )
    .unwrap();
    assert!(job.parameters.is_empty());
    assert_eq!(
        crate::env_params_common::rendered_values(&job.steps[0]),
        ["JobTemplate/E: A=string:a, LP=list[path]:[], P=path:/a"]
    );
    // They have the shape and constraints of the Job Parameter of the same
    // type: an unknown field, an unknown value, and a control that needs
    // allowedValues are rejected.
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: A, type: STRING, default: a, userInterface: {control: DROPDOWN_LIST}}
  variables: {V: x}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
        &["jobEnvironments[0] -> envParameterDefinitions[0]:\n\tParameter 'A': DROPDOWN_LIST requires allowedValues."],
    );
}

#[test]
fn definition_constraints_are_checked_as_for_job_parameters() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: A, type: STRING, minLength: 5, maxLength: 2}
  - {name: I, type: INT, minValue: 5, maxValue: 2}
  variables: {V: x}
steps:
- name: S
  dependencies: [{environment: E, parameters: {A: abc, I: 3}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "jobEnvironments[0] -> envParameterDefinitions[0]:\n\tParameter 'A': minLength (5) > maxLength (2).",
            "jobEnvironments[0] -> envParameterDefinitions[1]:\n\tParameter 'I': minValue (5) > maxValue (2).",
            "steps[0] -> dependencies[0] -> parameters -> A:\n\tParameter 'A': value length 3 is less than minimum 5",
            "steps[0] -> dependencies[0] -> parameters -> I:\n\tParameter 'I': value 3 is less than minimum 5",
        ],
    );
}

#[test]
fn literal_default_is_checked_against_type_and_constraints() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: S, type: STRING, default: 7}
  - {name: I, type: INT, default: "seven"}
  - {name: Lo, type: INT, default: 0, minValue: 1}
  - {name: Al, type: STRING, default: c, allowedValues: [a, b]}
  - {name: P, type: PATH, default: /toolong, maxLength: 3}
  - {name: L, type: "LIST[INT]", default: [1], minLength: 2}
  - {name: LP, type: "LIST[PATH]", default: [/a/long/path], item: {maxLength: 3}}
  - {name: B, type: BOOL, default: "maybe"}
  variables: {V: x}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "jobEnvironments[0] -> envParameterDefinitions[0] -> default:\n\tis not a valid STRING value: invalid type: integer `7`, expected a string",
            "jobEnvironments[0] -> envParameterDefinitions[1] -> default:\n\tis not a valid INT value: Cannot parse 'seven' as integer",
            "jobEnvironments[0] -> envParameterDefinitions[2] -> default:\n\tParameter 'Lo': value 0 is less than minimum 1",
            "jobEnvironments[0] -> envParameterDefinitions[3] -> default:\n\tParameter 'Al': value 'c' is not in allowed values",
            "jobEnvironments[0] -> envParameterDefinitions[4] -> default:\n\tParameter 'P': value length 8 exceeds maximum 3",
            "jobEnvironments[0] -> envParameterDefinitions[5] -> default:\n\tParameter 'L': list length 1 is less than minimum 2",
            "jobEnvironments[0] -> envParameterDefinitions[6] -> default:\n\tParameter 'LP': item[0] length 12 exceeds maximum 3",
            "jobEnvironments[0] -> envParameterDefinitions[7] -> default:\n\tis not a valid BOOL value: Invalid bool value: 'maybe'",
        ],
    );
}

#[test]
fn literal_default_is_checked_even_when_every_step_supplies_the_parameter() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: I, type: INT, default: 0, minValue: 1}]
  variables: {V: x}
steps:
- name: S
  dependencies: [{environment: E, parameters: {I: 2}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &["jobEnvironments[0] -> envParameterDefinitions[0] -> default:\n\tParameter 'I': value 0 is less than minimum 1"],
    );
}

#[test]
fn relative_literal_path_default_is_not_a_template_validation_error() {
    // Absoluteness depends on the submission path format and is checked
    // at job creation (§4.5 point 1, PATH paragraph).
    job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: P, type: PATH, default: relative/dir}]
  variables: {V: "{{Env.Param.P}}"}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
    );
}

#[test]
fn list_literal_elements_are_not_format_strings() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Dir, type: PATH, default: d}]
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: M, type: "LIST[PATH]", default: ["{{RawParam.Dir}}"]}]
  variables: {V: x}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
        &["jobEnvironments[0] -> envParameterDefinitions[0] -> default[0]:\n\tthe elements of a list literal are literals, not format strings. Write the whole value as one '{{ }}' expression, for example '{{ [RawParam.Dir] }}'."],
    );
}

#[test]
fn object_and_null_element_defaults_are_rejected() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: A, type: STRING, default: {x: 1}}
  - {name: C, type: "LIST[STRING]", default: [a, null]}
  variables: {V: x}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "jobEnvironments[0] -> envParameterDefinitions[0] -> default:\n\tmust not be an object: no parameter type has an object value.",
            "jobEnvironments[0] -> envParameterDefinitions[1] -> default[1]:\n\tmust not be null: no parameter type has a null value.",
        ],
    );
}

#[test]
fn unbalanced_braces_in_a_default_fail_decode() {
    // A string containing `{{` is a format string (plan A18), so `a{{b`
    // is a format-string error rather than a STRING literal. Like every
    // serde-stage error in this crate, it carries no field path.
    let err = openjd_model::decode_job_template(
        yaml(
            r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: A, type: STRING, default: "a{{b"}]
  variables: {V: x}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
        ),
        Some(ALLOW),
        &openjd_model::CallerLimits::default(),
    )
    .unwrap_err()
    .to_string();
    assert_eq!(
        err,
        "Validation error: 'jobtemplate-2023-09' failed checks: Failed to parse interpolation \
         expression at [1, 4]. Reason: Braces mismatch.\n  a{{b\n   ^~"
    );
}

#[test]
fn non_object_definition_fails_decode() {
    let err = openjd_model::decode_environment_template(
        yaml(
            r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment: {name: E, envParameterDefinitions: [5], variables: {V: x}}
"#,
        ),
        Some(ALLOW),
        &openjd_model::CallerLimits::default(),
    )
    .unwrap_err()
    .to_string();
    assert_eq!(
        err,
        "Validation error: 'environment-2023-09' failed checks: an Environment parameter \
         definition must be an object"
    );
}

#[test]
fn null_default_means_no_default() {
    // As for a <JobParameterDefinition>, `default: null` is no default, so
    // the parameter is required (constraint 7).
    let jt = job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: A, type: STRING, default: null}]
  variables: {V: x}
steps:
- name: S
  dependencies: [{environment: E, parameters: {A: a}}]
  script: {actions: {onRun: {command: x}}}
"#,
    );
    let defs = jt.job_environments.as_ref().unwrap()[0]
        .env_parameter_definitions
        .as_ref()
        .unwrap();
    assert!(defs[0].default.is_none());
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: A, type: STRING, default: null}]
  variables: {V: x}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
        &["steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every jobEnvironments entry with activation ALWAYS, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7); missing: 'A' of Environment 'E'."],
    );
}

#[test]
fn default_is_decoded_as_written() {
    let jt = job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Seats, type: INT, default: 2}]
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: A, type: INT, default: "{{Param.Seats}}"}
  - {name: B, type: INT, default: "3"}
  - {name: C, type: STRING}
  variables: {V: x}
steps:
- name: S
  dependencies: [{environment: E, parameters: {C: c}}]
  script: {actions: {onRun: {command: x}}}
"#,
    );
    let defs = jt.job_environments.as_ref().unwrap()[0]
        .env_parameter_definitions
        .as_ref()
        .unwrap();
    assert_eq!(
        defs[0]
            .default
            .as_ref()
            .unwrap()
            .as_format_string()
            .unwrap()
            .raw(),
        "{{Param.Seats}}"
    );
    assert_eq!(
        defs[1].default,
        Some(EnvironmentParameterValue::Literal(serde_json::json!("3")))
    );
    assert!(defs[2].default.is_none());
}

// ── <StepDependency> environment: form (§3.2) ──────────────────────

#[test]
fn step_dependency_target_and_required_default() {
    let jt = job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: A
  script: {actions: {onRun: {command: x}}}
- name: B
  dependencies:
  - dependsOn: A
  - environment: Conda
  - environment: Docker
    required: false
    parameters: {Image: ubuntu}
  script: {actions: {onRun: {command: x}}}
"#,
    );
    let deps = jt.steps[1].dependencies.as_ref().unwrap();
    assert_eq!(
        deps[0].target(),
        Some(DependencyTarget::Step("A", StepKey::DependsOn))
    );
    assert_eq!(deps[0], StepDependency::on_step("A"));
    assert_eq!(deps[0].step(), Some("A"));
    assert_eq!(deps[0].environment(), None);
    assert_eq!(deps[1], StepDependency::on_environment("Conda"));
    assert_eq!(deps[1].step(), None);
    assert_eq!(
        deps[1].target(),
        Some(DependencyTarget::Environment("Conda"))
    );
    assert!(deps[1].is_required(), "required defaults to true");
    assert!(deps[1].parameters.is_none());
    assert_eq!(deps[2].environment(), Some("Docker"));
    assert!(!deps[2].is_required());
    assert_eq!(
        deps[2].parameters.as_ref().unwrap().get("Image"),
        Some(&EnvironmentParameterValue::Literal(serde_json::json!(
            "ubuntu"
        )))
    );
}

#[test]
fn constraint_1_exactly_one_form() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: A
  script: {actions: {onRun: {command: x}}}
- name: B
  dependencies:
  - {dependsOn: A, environment: E}
  - {}
  - {dependsOn: A, required: false, parameters: {X: 1}}
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "steps[1] -> dependencies[0]:\n\tmust have exactly one of 'dependsOn', 'step', or 'environment', not both 'dependsOn' and 'environment'.",
            "steps[1] -> dependencies[1]:\n\tmust have exactly one of 'dependsOn', 'step', or 'environment'.",
            "steps[1] -> dependencies[2] -> required:\n\t'required' is permitted only with 'environment'.",
            "steps[1] -> dependencies[2] -> parameters:\n\t'parameters' is permitted only with 'environment'.",
        ],
    );
}

#[test]
fn constraint_2_must_not_name_own_step_environment() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  stepEnvironments: [{name: SE, variables: {V: x}}]
  dependencies: [{environment: SE}]
  script: {actions: {onRun: {command: x}}}
- name: T
  # Another Step may name it: there it resolves to an external Environment.
  dependencies: [{environment: SE}]
  script: {actions: {onRun: {command: x}}}
"#,
        &["steps[0] -> dependencies[0] -> environment:\n\tmust not name 'SE', a stepEnvironments entry of the same Step. A Step Environment cannot declare parameters."],
    );
}

#[test]
fn constraint_3_no_duplicate_environment_entries() {
    // Duplicate `dependsOn` entries keep their own rule and message.
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: A
  script: {actions: {onRun: {command: x}}}
- name: B
  dependencies:
  - {environment: Conda}
  - {dependsOn: A}
  - {environment: Conda, required: false}
  - {dependsOn: A}
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "steps[1] -> dependencies[2] -> environment:\n\tduplicate environment dependency 'Conda'.",
            "steps[1] -> dependencies[3]:\n\tduplicate dependency 'A'.",
        ],
    );
}

#[test]
fn constraint_4_parameters_count_and_keys() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  dependencies:
  - {environment: E1, parameters: {}}
  - {environment: E2, parameters: {"not-an-id": a, "9x": b}}
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "steps[0] -> dependencies[0] -> parameters:\n\tif provided, must contain at least one entry.",
            "steps[0] -> dependencies[1] -> parameters -> 9x:\n\t'9x' is not a valid parameter name: it must match [A-Za-z_][A-Za-z0-9_]* and be at most 64 characters.",
            "steps[0] -> dependencies[1] -> parameters -> not-an-id:\n\t'not-an-id' is not a valid parameter name: it must match [A-Za-z_][A-Za-z0-9_]* and be at most 64 characters.",
        ],
    );
    let params: Vec<String> = (0..51).map(|i| format!("P{i}: v")).collect();
    job_err(
        &format!(
            "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\nsteps:\n- name: S\n  dependencies:\n  - environment: Ext\n    parameters: {{{}}}\n  script: {{actions: {{onRun: {{command: x}}}}}}\n",
            params.join(", ")
        ),
        &["steps[0] -> dependencies[0] -> parameters:\n\tmust not contain more than 50 entries."],
    );
}

#[test]
fn constraint_8_required_false_on_a_job_environment() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments: [{name: E, variables: {V: x}}]
steps:
- name: S
  dependencies: [{environment: E, required: false}]
  script: {actions: {onRun: {command: x}}}
- name: T
  # `required: true` is allowed, and `required: false` on an external
  # Environment is the common case.
  dependencies: [{environment: E, required: true}, {environment: Ext, required: false}]
  script: {actions: {onRun: {command: x}}}
"#,
        &["steps[0] -> dependencies[0] -> required:\n\tmust not be false: 'E' is a jobEnvironments entry, which is always present."],
    );
}

#[test]
fn external_entries_are_accepted_without_knowing_the_environment() {
    job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Pkgs, type: STRING, default: ffmpeg}]
steps:
- name: S
  dependencies:
  - environment: Conda
    required: false
    parameters: {Packages: "{{Param.Pkgs}}", Channels: conda-forge, Count: 3, Ratio: 1.5}
  - environment: Docker
  script: {actions: {onRun: {command: x}}}
"#,
    );
}

#[test]
fn external_entry_literal_forms_no_type_accepts() {
    // No parameter type has a null or an object value.
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  dependencies:
  - environment: Ext
    parameters: {O: {a: 1}, N: null}
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "steps[0] -> dependencies[0] -> parameters -> N:\n\tmust not be null: no parameter type has a null value.",
            "steps[0] -> dependencies[0] -> parameters -> O:\n\tmust not be an object: no parameter type has an object value.",
        ],
    );
    // Bools and lists are fine; objects, and lists nested beyond
    // LIST[LIST[INT]], are not.
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  dependencies:
  - environment: Ext
    parameters:
      B: true
      L: [a, b]
      LL: [[1, "2"]]
      Deep: [[1, [2]]]
      Mixed: [[a]]
      ObjEl: [{a: 1}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "steps[0] -> dependencies[0] -> parameters -> Deep[0][1]:\n\ta nested list may hold only integers or integer strings (LIST[LIST[INT]] is the only nested type).",
            "steps[0] -> dependencies[0] -> parameters -> Mixed[0][0]:\n\ta nested list may hold only integers or integer strings (LIST[LIST[INT]] is the only nested type).",
            "steps[0] -> dependencies[0] -> parameters -> ObjEl[0]:\n\tmust not be an object: no parameter type has an object value.",
        ],
    );
}

// ── Effect on the job ──────────────────────────────────────────────

#[test]
fn environment_entries_are_not_step_dependencies_in_the_job() {
    let jt = job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: A, type: STRING, default: a}]
  variables: {V: "{{Env.Param.A}}"}
steps:
- name: First
  dependencies: [{environment: E, parameters: {A: b}}]
  script: {actions: {onRun: {command: x}}}
- name: Second
  dependencies: [{environment: Conda, required: false}, {dependsOn: First}]
  script: {actions: {onRun: {command: x}}}
"#,
    );
    let ctx: ValidationContext = jt.default_validation_context();
    let job = create_job(&jt, &JobParameterValues::new(), &ctx).unwrap();
    // A list left empty by dropping `environment:` entries becomes None.
    assert!(job.steps[0].dependencies.is_none());
    let deps = job.steps[1].dependencies.as_ref().unwrap();
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].depends_on, "First");
    let graph = StepDependencyGraph::new(&job).unwrap();
    assert_eq!(graph.step_node("Second").unwrap().in_edges.len(), 1);
}

#[test]
fn dependency_target_helpers() {
    let step = StepDependency::on_step("A");
    let env = StepDependency::on_environment("E");
    assert!(step.is_well_formed() && env.is_well_formed());
    assert_eq!(step.target().unwrap().to_string(), "dependsOn: A");
    assert_eq!(env.target().unwrap().to_string(), "environment: E");
    assert_eq!(
        DependencyTarget::Step("A", StepKey::DependsOn).environment(),
        None
    );
    assert_eq!(DependencyTarget::Environment("E").step(), None);
    let both = StepDependency {
        environment: Some("E".into()),
        ..StepDependency::on_step("A")
    };
    assert!(!both.is_well_formed());
    assert_eq!(both.target(), None);
    assert_eq!(both.step(), None);
    assert_eq!(both.environment(), None);
}

#[test]
fn environment_entry_name_is_an_environment_name() {
    let long = "x".repeat(65);
    job_err(
        &format!(
            r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  dependencies:
  - {{environment: "", required: false}}
  - {{environment: {long}, required: false}}
  - {{environment: "a\nb", required: false}}
  - {{environment: "{}", required: false}}
  script: {{actions: {{onRun: {{command: x}}}}}}
"#,
            "y".repeat(64)
        ),
        &[
            "steps[0] -> dependencies[0] -> environment:\n\tmust not be empty.",
            "steps[0] -> dependencies[1] -> environment:\n\texceeds 64 characters.",
            "steps[0] -> dependencies[2] -> environment:\n\tcontains control characters.",
        ],
    );
    // FEATURE_BUNDLE_1 raises the limit to 512, as for an Environment name.
    job_ok(&format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR, FEATURE_BUNDLE_1]\nname: J\nsteps:\n- name: S\n  dependencies: [{{environment: {long}}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    ));
}

#[test]
fn cycle_detection_ignores_environment_entries() {
    // `environment: B` names an (external) Environment, not Step B, so
    // there is no cycle.
    let jt = job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: A
  dependencies: [{environment: B, required: false}]
  script: {actions: {onRun: {command: x}}}
- name: B
  dependencies: [{dependsOn: A}]
  script: {actions: {onRun: {command: x}}}
"#,
    );
    let job = create_job(
        &jt,
        &JobParameterValues::new(),
        &jt.default_validation_context(),
    )
    .unwrap();
    assert!(job.steps[0].dependencies.is_none());
}

#[test]
fn parameter_name_of_64_characters_is_accepted() {
    let name = "A".repeat(64);
    job_ok(&format!(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{{name: {name}, type: STRING, default: a}}]
  variables: {{V: x}}
steps:
- name: S
  dependencies: [{{environment: E, parameters: {{{name}: b}}}}]
  script: {{actions: {{onRun: {{command: x}}}}}}
"#
    ));
}

#[test]
fn parameters_key_length_limit() {
    let ok = "K".repeat(64);
    let long = "K".repeat(65);
    job_err(
        &format!(
            r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  dependencies: [{{environment: Ext, parameters: {{{ok}: a, {long}: b}}}}]
  script: {{actions: {{onRun: {{command: x}}}}}}
"#
        ),
        &[&format!(
            "steps[0] -> dependencies[0] -> parameters -> {long}:\n\t'{long}' is not a valid parameter name: it must match [A-Za-z_][A-Za-z0-9_]* and be at most 64 characters."
        )],
    );
}

#[test]
fn fifty_parameters_entries_are_accepted() {
    let params: Vec<String> = (0..50).map(|i| format!("P{i}: v")).collect();
    job_ok(&format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR]\nname: J\nsteps:\n- name: S\n  dependencies:\n  - environment: Ext\n    parameters: {{{}}}\n  script: {{actions: {{onRun: {{command: x}}}}}}\n",
        params.join(", ")
    ));
}

#[test]
fn user_interface_is_permitted_for_every_type() {
    let cases = [
        ("STRING", "a", "{control: LINE_EDIT}"),
        ("INT", "1", "{control: SPIN_BOX}"),
        ("FLOAT", "1.5", "{control: SPIN_BOX}"),
        ("PATH", "/a", "{control: CHOOSE_DIRECTORY}"),
        ("BOOL", "true", "{control: CHECK_BOX}"),
        ("RANGE_EXPR", "\"1-3\"", "{control: LINE_EDIT}"),
        ("\"LIST[STRING]\"", "[]", "{control: HIDDEN}"),
        ("\"LIST[PATH]\"", "[]", "{control: HIDDEN}"),
        ("\"LIST[INT]\"", "[]", "{control: HIDDEN}"),
        ("\"LIST[FLOAT]\"", "[]", "{control: HIDDEN}"),
        ("\"LIST[BOOL]\"", "[]", "{control: HIDDEN}"),
        ("\"LIST[LIST[INT]]\"", "[]", "{control: HIDDEN}"),
    ];
    for (ty, default, ui) in cases {
        job_ok(&format!(
            r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{{name: A, type: {ty}, default: {default}, userInterface: {ui}}}]
  variables: {{V: x}}
steps:
- name: S
  script: {{actions: {{onRun: {{command: x}}}}}}
"#
        ));
    }
}

#[test]
fn list_literals_are_decoded_as_the_job_parameter_default_is() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: LS, type: "LIST[STRING]", default: [5]}
  - {name: LP, type: "LIST[PATH]", default: [true]}
  - {name: LI, type: "LIST[INT]", default: [1.5]}
  - {name: LF, type: "LIST[FLOAT]", default: ["x"]}
  - {name: LB, type: "LIST[BOOL]", default: ["maybe"]}
  - {name: LL, type: "LIST[LIST[INT]]", default: [[1.5]]}
  - {name: NotList, type: "LIST[STRING]", default: 5}
  variables: {V: x}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "jobEnvironments[0] -> envParameterDefinitions[0] -> default:\n\tis not a valid LIST[STRING] value: invalid type: integer `5`, expected a string",
            "jobEnvironments[0] -> envParameterDefinitions[1] -> default:\n\tis not a valid LIST[PATH] value: invalid type: boolean `true`, expected a string",
            "jobEnvironments[0] -> envParameterDefinitions[2] -> default:\n\tis not a valid LIST[INT] value: Expected integer, got float: 1.5",
            "jobEnvironments[0] -> envParameterDefinitions[3] -> default:\n\tis not a valid LIST[FLOAT] value: Cannot parse 'x' as float",
            "jobEnvironments[0] -> envParameterDefinitions[4] -> default:\n\tis not a valid LIST[BOOL] value: Invalid bool value: 'maybe'",
            "jobEnvironments[0] -> envParameterDefinitions[5] -> default:\n\tis not a valid LIST[LIST[INT]] value: Expected integer, got float: 1.5",
            "jobEnvironments[0] -> envParameterDefinitions[6] -> default:\n\tis not a valid LIST[STRING] value: invalid type: integer `5`, expected a sequence",
        ],
    );
}

#[test]
fn nested_list_floats_are_rejected_for_external_entries() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  dependencies: [{environment: Ext, parameters: {P: [[1.5, 2]]}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &["steps[0] -> dependencies[0] -> parameters -> P[0][0]:\n\ta nested list may hold only integers or integer strings (LIST[LIST[INT]] is the only nested type)."],
    );
}

#[test]
fn a_rejected_value_is_reported_once() {
    // A null value for a Job Environment parameter fails the form check,
    // which stops the definition check from reporting it again; a
    // RANGE_EXPR definition's literal default is checked as a Job
    // Parameter's is, once.
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: A, type: STRING, default: a}
  - {name: R, type: RANGE_EXPR, default: "bad"}
  variables: {V: x}
steps:
- name: S
  dependencies: [{environment: E, parameters: {A: null}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "jobEnvironments[0] -> envParameterDefinitions[1] -> default:\n\tis not a valid RANGE_EXPR value: Value 'bad' is not a valid range expression: Expected integer in 'bad'",
            "steps[0] -> dependencies[0] -> parameters -> A:\n\tmust not be null: no parameter type has a null value.",
        ],
    );
}

#[test]
fn a_malformed_entry_does_not_also_fail_constraint_7() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: R, type: STRING}]
  variables: {V: x}
steps:
- name: A
  dependencies: [{environment: E, parameters: {R: r}}]
  script: {actions: {onRun: {command: x}}}
- name: B
  dependencies: [{dependsOn: A, environment: E, parameters: {R: r}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &["steps[1] -> dependencies[0]:\n\tmust have exactly one of 'dependsOn', 'step', or 'environment', not both 'dependsOn' and 'environment'."],
    );
}

#[test]
fn a_malformed_entrys_environment_name_is_still_checked() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: A
  script: {actions: {onRun: {command: x}}}
- name: B
  dependencies: [{dependsOn: A, environment: ""}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "steps[1] -> dependencies[0]:\n\tmust have exactly one of 'dependsOn', 'step', or 'environment', not both 'dependsOn' and 'environment'.",
            "steps[1] -> dependencies[0] -> environment:\n\tmust not be empty.",
        ],
    );
}

#[test]
fn one_mistake_gives_one_error() {
    // Each case is one mistake; the checks that would only restate it are
    // skipped, so exactly one error is reported.
    let cases: &[(&str, &str, &str)] = &[
        // Gated `envParameterDefinitions`: its contents are not checked too.
        (
            "",
            "jobEnvironments: [{name: E, envParameterDefinitions: [], variables: {V: x}}]\n",
            "jobEnvironments[0] -> envParameterDefinitions:\n\tenvParameterDefinitions on an Environment requires the ENV_PARAMS extension.",
        ),
        // Gated `environment:` entry: its name is not checked too.
        (
            "",
            "",
            "steps[0] -> dependencies[0]:\n\tthe 'environment' form of a dependency requires the ENV_PARAMS extension.",
        ),
    ];
    for (exts, envs, expected) in cases {
        let ext_line = if exts.is_empty() {
            String::new()
        } else {
            format!("extensions: [{exts}]\n")
        };
        let step_deps = if envs.is_empty() {
            "  dependencies: [{environment: \"\"}]\n"
        } else {
            ""
        };
        job_err(
            &format!(
                "specificationVersion: jobtemplate-2023-09\n{ext_line}name: J\n{envs}steps:\n- name: S\n{step_deps}  script: {{actions: {{onRun: {{command: x}}}}}}\n"
            ),
            &[expected],
        );
    }
    let one = |doc: &str, expected: &str| job_err(doc, &[expected]);
    // A `stepEnvironments` entry's definitions are not checked too.
    one(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  stepEnvironments: [{name: SE, envParameterDefinitions: [], variables: {V: x}}]
  script: {actions: {onRun: {command: x}}}
"#,
        "steps[0] -> stepEnvironments[0] -> envParameterDefinitions:\n\tis not permitted on a stepEnvironments entry; only a jobEnvironments entry or an Environment Template's environment may declare parameters.",
    );
    // An invalid key's value is not checked too.
    one(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  dependencies: [{environment: Ext, parameters: {"9x": null}}]
  script: {actions: {onRun: {command: x}}}
"#,
        "steps[0] -> dependencies[0] -> parameters -> 9x:\n\t'9x' is not a valid parameter name: it must match [A-Za-z_][A-Za-z0-9_]* and be at most 64 characters.",
    );
    // A null inside a nested list is not also a nested-list type error.
    one(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  dependencies: [{environment: Ext, parameters: {A: [[null]]}}]
  script: {actions: {onRun: {command: x}}}
"#,
        "steps[0] -> dependencies[0] -> parameters -> A[0][0]:\n\tmust not be null: no parameter type has a null value.",
    );
    // A format string inside a list literal is not also a type error.
    one(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: L, type: "LIST[INT]", default: ["{{x}}"]}]
  variables: {V: x}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
        "jobEnvironments[0] -> envParameterDefinitions[0] -> default[0]:\n\tthe elements of a list literal are literals, not format strings. Write the whole value as one '{{ }}' expression, for example '{{ [RawParam.Dir] }}'.",
    );
    // An empty map for an Environment that declares nothing is only a
    // constraint 4 error.
    one(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments: [{name: E, variables: {V: x}}]
steps:
- name: S
  dependencies: [{environment: E, parameters: {}}]
  script: {actions: {onRun: {command: x}}}
"#,
        "steps[0] -> dependencies[0] -> parameters:\n\tif provided, must contain at least one entry.",
    );
    // A malformed entry's values are not checked by pass 8 too.
    one(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: A
  script: {actions: {onRun: {command: x}}}
- name: B
  dependencies: [{dependsOn: A, environment: X, parameters: {P: "{{Param.Nope}}"}}]
  script: {actions: {onRun: {command: x}}}
"#,
        "steps[1] -> dependencies[0]:\n\tmust have exactly one of 'dependsOn', 'step', or 'environment', not both 'dependsOn' and 'environment'.",
    );
    // A duplicated Job Environment name: constraint 7 reports once.
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- {name: E, envParameterDefinitions: [{name: X, type: STRING}], variables: {V: x}}
- {name: E, envParameterDefinitions: [{name: X, type: STRING}], variables: {V: x}}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "jobEnvironments[1]:\n\tduplicate environment name: 'E'",
            "steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every jobEnvironments entry with activation ALWAYS, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7); missing: 'X' of Environment 'E'.",
        ],
    );
}

#[test]
fn nested_list_elements_follow_the_list_list_int_decoder() {
    // The LIST[LIST[INT]] `default` decoder (FlexInt) accepts an integer,
    // an integer-valued float, and an integer string with surrounding
    // whitespace; template validation accepts the same for an external
    // entry.
    job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  dependencies: [{environment: Ext, parameters: {P: [[1, 2.0, " 5", "7"]]}}]
  script: {actions: {onRun: {command: x}}}
"#,
    );
}

#[test]
fn a_list_mixing_lists_and_other_values_is_rejected() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  dependencies: [{environment: Ext, parameters: {A: [1, [2]], B: [[1], "a"]}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[
            "steps[0] -> dependencies[0] -> parameters -> A:\n\tmust not mix lists and other values: LIST[LIST[INT]] is the only nested type.",
            "steps[0] -> dependencies[0] -> parameters -> B:\n\tmust not mix lists and other values: LIST[LIST[INT]] is the only nested type.",
        ],
    );
}

#[test]
fn a_malformed_entrys_environment_half_is_not_checked() {
    // Only constraint 1 is reported; `required: false` (constraint 8) and
    // the undeclared key (constraint 5) are not checked on such an entry.
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: R, type: STRING, default: r}]
  variables: {V: x}
steps:
- name: A
  script: {actions: {onRun: {command: x}}}
- name: B
  dependencies: [{dependsOn: A, environment: E, required: false, parameters: {Q: x}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &["steps[1] -> dependencies[0]:\n\tmust have exactly one of 'dependsOn', 'step', or 'environment', not both 'dependsOn' and 'environment'."],
    );
}

#[test]
fn padded_numeric_strings_are_accepted_as_for_job_parameter_defaults() {
    // FlexInt and FlexFloat trim, so a Job Parameter `default` of " 3 "
    // is valid; the same literal is valid for an Environment parameter.
    job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions:
- {name: N, type: INT, default: " 3 "}
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: N, type: INT, default: " 3 ", minValue: 3}
  - {name: F, type: FLOAT, default: " 1.5 ", maxValue: 1.5}
  - {name: L, type: "LIST[INT]", default: [" 3 "]}
  - {name: LL, type: "LIST[LIST[INT]]", default: [[" 3 "]]}
  - {name: M, type: INT, default: 0}
  variables: {V: x}
steps:
- name: S
  dependencies: [{environment: E, parameters: {M: "3 "}}]
  script: {actions: {onRun: {command: x}}}
"#,
    );
}

#[test]
fn step_environment_parameter_definitions_without_the_extension_get_the_gate_error() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
name: J
steps:
- name: S
  stepEnvironments:
  - {name: SE, envParameterDefinitions: [{name: A, type: STRING, default: a}], variables: {V: x}}
  script: {actions: {onRun: {command: x}}}
"#,
        &["steps[0] -> stepEnvironments[0] -> envParameterDefinitions:\n\tenvParameterDefinitions on an Environment requires the ENV_PARAMS extension."],
    );
}

#[test]
fn a_mixed_list_is_reported_once() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  dependencies: [{environment: Ext, parameters: {L: [[1.5], 2]}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &["steps[0] -> dependencies[0] -> parameters -> L:\n\tmust not mix lists and other values: LIST[LIST[INT]] is the only nested type."],
    );
}

#[test]
fn a_malformed_entry_adds_no_cycle_edge() {
    job_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S1
  dependencies: [{dependsOn: S2, environment: E}]
  script: {actions: {onRun: {command: x}}}
- name: S2
  dependencies: [{dependsOn: S1}]
  script: {actions: {onRun: {command: x}}}
"#,
        &["steps[0] -> dependencies[0]:\n\tmust have exactly one of 'dependsOn', 'step', or 'environment', not both 'dependsOn' and 'environment'."],
    );
}

// ── <StepDependency> step: form (§3.2 item 1) ──────────────────────

/// A Job Template with Steps `A` and `B`, `B` having `deps` as its
/// `dependencies`, listing `exts`.
fn two_steps(exts: &str, deps: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09\n{exts}name: J\nsteps:\n\
         - name: A\n  script: {{actions: {{onRun: {{command: x}}}}}}\n\
         - name: B\n  dependencies: {deps}\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
    )
}

#[test]
fn a_step_entry_names_a_step_as_depends_on_does() {
    let jt = job_ok(&two_steps(
        "extensions: [ENV_PARAMS, EXPR]\n",
        "[{step: A}]",
    ));
    let dep = &jt.steps[1].dependencies.as_ref().unwrap()[0];
    assert_eq!(dep.step.as_deref(), Some("A"));
    assert_eq!(dep.depends_on, None);
    assert_eq!(
        dep.target(),
        Some(DependencyTarget::Step("A", StepKey::Step))
    );
    assert_eq!(dep.step(), Some("A"));
    assert_eq!(dep.environment(), None);
    assert!(dep.is_well_formed());
    assert_eq!(dep.target().unwrap().to_string(), "step: A");
    assert_eq!(StepKey::Step.as_str(), "step");
    assert_eq!(StepKey::DependsOn.as_str(), "dependsOn");
    // The Job has one dependency graph, and the entry is a `dependsOn`
    // there (plan 3 Q2): the job type has no `step` key.
    let ctx: ValidationContext = jt.default_validation_context();
    let job = create_job(&jt, &JobParameterValues::new(), &ctx).unwrap();
    let deps = job.steps[1].dependencies.as_ref().unwrap();
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].depends_on, "A");
    let graph = StepDependencyGraph::new(&job).unwrap();
    assert_eq!(graph.step_node("B").unwrap().in_edges.len(), 1);
    assert_eq!(
        serde_json::to_value(&job.steps[1].dependencies).unwrap(),
        serde_json::json!([{"dependsOn": "A"}])
    );
}

#[test]
fn the_step_form_requires_env_params() {
    job_err(
        &two_steps("", "[{step: A}]"),
        &["steps[1] -> dependencies[0]:\n\tthe 'step' form of a dependency requires the ENV_PARAMS extension."],
    );
    // The gate does not replace the Step checks of pass 6, which run on a
    // `step` entry as on a `dependsOn` one.
    job_err(
        &two_steps("", "[{step: Z}, {step: B}]"),
        &[
            "steps[1] -> dependencies[0]:\n\tdependency 'Z' not found.",
            "steps[1] -> dependencies[1]:\n\tcannot depend on itself.",
            "JobTemplate: step dependencies contain a cycle.",
            "steps[1] -> dependencies[0]:\n\tthe 'step' form of a dependency requires the ENV_PARAMS extension.",
            "steps[1] -> dependencies[1]:\n\tthe 'step' form of a dependency requires the ENV_PARAMS extension.",
        ],
    );
    // With another form in the entry, constraint 1 names only the keys the
    // template has, as the `environment` form's does without the extension.
    job_err(
        &two_steps("", "[{step: A, dependsOn: A}]"),
        &[
            "steps[1] -> dependencies[0]:\n\tmust have exactly one of 'dependsOn' or 'step', not both.",
            "steps[1] -> dependencies[0]:\n\tthe 'step' form of a dependency requires the ENV_PARAMS extension.",
        ],
    );
    // All three keys: the all-forms message, and each form's gate.
    job_err(
        &two_steps("", "[{step: A, dependsOn: A, environment: E}]"),
        &[
            "steps[1] -> dependencies[0]:\n\tmust have exactly one of 'dependsOn', 'step', or 'environment', not all three.",
            "steps[1] -> dependencies[0]:\n\tthe 'step' form of a dependency requires the ENV_PARAMS extension.",
            "steps[1] -> dependencies[0]:\n\tthe 'environment' form of a dependency requires the ENV_PARAMS extension.",
        ],
    );
}

#[test]
fn constraint_1_counts_the_step_form() {
    job_err(
        &two_steps(
            "extensions: [ENV_PARAMS, EXPR]\n",
            "\n  - {step: A, dependsOn: A}\n  - {step: A, environment: E}\n  - {step: A, dependsOn: A, environment: E}\n  - {step: A, required: false, enableIf: true, parameters: {X: 1}}",
        ),
        &[
            "steps[1] -> dependencies[0]:\n\tmust have exactly one of 'dependsOn', 'step', or 'environment', not both 'dependsOn' and 'step'.",
            "steps[1] -> dependencies[1]:\n\tmust have exactly one of 'dependsOn', 'step', or 'environment', not both 'step' and 'environment'.",
            "steps[1] -> dependencies[2]:\n\tmust have exactly one of 'dependsOn', 'step', or 'environment', not all three.",
            "steps[1] -> dependencies[3] -> required:\n\t'required' is permitted only with 'environment'.",
            "steps[1] -> dependencies[3] -> enableIf:\n\t'enableIf' is permitted only with 'environment'.",
            "steps[1] -> dependencies[3] -> parameters:\n\t'parameters' is permitted only with 'environment'.",
        ],
    );
}

#[test]
fn every_depends_on_rule_applies_to_a_step_entry() {
    let ext = "extensions: [ENV_PARAMS, EXPR]\n";
    // Self (which, as for `dependsOn`, is also a cycle), unknown, and a
    // duplicate across the two forms.
    job_err(
        &two_steps(ext, "[{step: B}, {step: Z}, {dependsOn: A}, {step: A}]"),
        &[
            "steps[1] -> dependencies[0]:\n\tcannot depend on itself.",
            "steps[1] -> dependencies[1]:\n\tdependency 'Z' not found.",
            "steps[1] -> dependencies[3]:\n\tduplicate dependency 'A'.",
            "JobTemplate: step dependencies contain a cycle.",
        ],
    );
    // Two `step` entries naming one Step are duplicates too.
    job_err(
        &two_steps(ext, "[{step: A}, {step: A}]"),
        &["steps[1] -> dependencies[1]:\n\tduplicate dependency 'A'."],
    );
    // One dependency graph: a cycle through both forms.
    job_err(
        &format!(
            "specificationVersion: jobtemplate-2023-09\n{ext}name: J\nsteps:\n\
             - name: A\n  dependencies: [{{step: B}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n\
             - name: B\n  dependencies: [{{dependsOn: A}}]\n  script: {{actions: {{onRun: {{command: x}}}}}}\n"
        ),
        &["JobTemplate: step dependencies contain a cycle."],
    );
}

// ── enableIf on an Environment (§4 item 6) ──────────────────────────
