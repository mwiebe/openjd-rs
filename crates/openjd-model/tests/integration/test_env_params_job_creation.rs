// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `ENV_PARAMS` at job creation: the effective values `create_job`
//! computes for the Job Template's own `jobEnvironments` (Template Schemas
//! §3.2.2, §4.5), the entries it freezes for external Environments, and
//! the §3.2.2 equality rule.

use openjd_expr::path_mapping::PathFormat;
use openjd_model::job::{Document, Job};
use openjd_model::{create_job, ModelError};

use crate::env_params_common::{assert_errors, job_ok, job_params, posix_ctx, rendered_values};

/// Create the Job with the template's own context (POSIX submission
/// paths).
fn create(jt_src: &str, params: &[(&str, &str)]) -> Result<Job, ModelError> {
    let jt = job_ok(jt_src);
    create_job(&jt, &job_params(&jt, params), &posix_ctx(&jt))
}

fn create_ok(jt_src: &str, params: &[(&str, &str)]) -> Job {
    create(jt_src, params).unwrap_or_else(|e| panic!("create_job failed:\n{e}"))
}

/// `create_job` must fail with exactly `expected` errors for `JobTemplate`.
fn create_err(jt_src: &str, params: &[(&str, &str)], expected: &[&str]) {
    let err = create(jt_src, params).expect_err("expected create_job to fail");
    assert_errors(&err.to_string(), "JobTemplate", expected);
}

const SEATS: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions:
- {name: Seats, type: INT, default: 2}
- {name: Edition, type: STRING, default: pro}
jobEnvironments:
- name: License
  envParameterDefinitions:
  - {name: SeatCount, type: INT, default: "{{Param.Seats}}"}
  - {name: Edition, type: STRING, default: "{{Param.Edition}}-x"}
  - {name: Server, type: STRING, default: lic.example}
  variables: {V: v}
steps:
- name: A
  dependencies: [{environment: License, parameters: {SeatCount: 3}}]
  script: {actions: {onRun: {command: x}}}
- name: B
  script: {actions: {onRun: {command: x}}}
"#;

// ── §3.2.2 effective values ─────────────────────────────────────────

#[test]
fn a_supplied_value_wins_and_others_take_the_resolved_default() {
    let job = create_ok(SEATS, &[]);
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["JobTemplate/License: Edition=string:pro-x, SeatCount=int:3, Server=string:lic.example"]
    );
    assert_eq!(
        rendered_values(&job.steps[1]),
        ["JobTemplate/License: Edition=string:pro-x, SeatCount=int:2, Server=string:lic.example"]
    );
    assert_eq!(
        job.steps[0].environment_parameters.as_ref().unwrap()[0].document,
        Document::JobTemplate
    );
}

#[test]
fn defaults_read_the_merged_job_parameter_values() {
    // §1.2.1: the submitted value of a Job Parameter changes the default.
    let job = create_ok(SEATS, &[("Seats", "7"), ("Edition", "lite")]);
    assert_eq!(
        rendered_values(&job.steps[1]),
        ["JobTemplate/License: Edition=string:lite-x, SeatCount=int:7, Server=string:lic.example"]
    );
}

#[test]
fn environment_parameters_do_not_become_job_parameters() {
    // §1.2.1 last paragraph: never merged with Job Parameters.
    let job = create_ok(SEATS, &[]);
    let mut names: Vec<&str> = job.parameters.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(names, ["Edition", "Seats"]);
}

#[test]
fn every_step_gets_values_for_every_declaring_job_environment() {
    // Plan A8: a Step that names no Environment still gets its defaults;
    // a Job Environment without parameters gets no entry.
    let job = create_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: Plain
  script: {actions: {onEnter: {command: x}}}
- name: E1
  envParameterDefinitions: [{name: X, type: STRING, default: a}]
  variables: {V: v}
- name: E2
  envParameterDefinitions: [{name: Y, type: INT, default: 1}]
  variables: {V: v}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
        &[],
    );
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["JobTemplate/E1: X=string:a", "JobTemplate/E2: Y=int:1"]
    );
}

#[test]
fn a_job_without_environment_parameters_serializes_as_before() {
    let job = create_ok(
        r#"
specificationVersion: jobtemplate-2023-09
name: J
jobEnvironments:
- name: Plain
  script: {actions: {onEnter: {command: x}}}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
        &[],
    );
    assert_eq!(job.steps[0].environment_parameters, None);
    assert_eq!(job.steps[0].environments, None);
    let json = serde_json::to_value(&job).unwrap();
    let step = json["steps"][0].as_object().unwrap();
    assert!(!step.contains_key("environmentParameters"));
    assert!(!step.contains_key("environments"));
}

#[test]
fn environment_entries_are_not_step_dependencies() {
    // §3.2 item 2: an entry does not delay scheduling.
    let job = create_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: X, type: STRING, default: a}]
  variables: {V: v}
steps:
- name: A
  script: {actions: {onRun: {command: x}}}
- name: B
  dependencies: [{environment: E, parameters: {X: b}}, {dependsOn: A}]
  script: {actions: {onRun: {command: x}}}
- name: C
  dependencies: [{environment: E, parameters: {X: c}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[],
    );
    let deps: Vec<_> = job.steps[1]
        .dependencies
        .iter()
        .flatten()
        .map(|d| d.depends_on.as_str())
        .collect();
    assert_eq!(deps, ["A"]);
    assert_eq!(job.steps[2].dependencies, None);
    let graph = openjd_model::StepDependencyGraph::new(&job).unwrap();
    assert_eq!(graph.topo_sorted_names().unwrap(), ["A", "B", "C"]);
}

// ── Conversion of values (§3.2 bullets, §4.5 item 4) ────────────────

const TYPES_EXPR: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions:
- {name: Seats, type: INT, default: 2}
- {name: Dir, type: PATH, default: data/in}
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: N, type: INT, default: "{{ Param.Seats * 10 }}"}
  - {name: F, type: FLOAT, default: 1.5}
  - {name: B, type: BOOL, default: "yes"}
  - {name: L, type: "LIST[INT]", default: "{{ [Param.Seats, 3] }}"}
  - {name: P, type: PATH, default: "{{RawParam.Dir}}"}
  - {name: Mounts, type: "LIST[PATH]", default: "{{ [RawParam.Dir, '/opt'] }}"}
  - {name: Tag, type: STRING, default: "s{{Param.Seats}}"}
  variables: {V: v}
steps:
- name: S
  let: ["tag = Step.Name.lower() + '-' + Job.Name"]
  dependencies: [{environment: E, parameters: {Tag: "{{ tag }}", L: [4, "5"]}}]
  script: {actions: {onRun: {command: x}}}
- name: T
  script: {actions: {onRun: {command: x}}}
"#;

#[test]
fn values_convert_to_the_parameter_type() {
    let job = create_ok(TYPES_EXPR, &[]);
    assert_eq!(
        rendered_values(&job.steps[0]),
        [
            "JobTemplate/E: B=bool:true, F=float:1.5, L=list[int]:[\"4\",\"5\"], \
             Mounts=list[path]:[\"/templates/data/in\",\"/opt\"], N=int:20, P=path:/templates/data/in, \
             Tag=string:s-J"
        ]
    );
    assert_eq!(
        rendered_values(&job.steps[1]),
        [
            "JobTemplate/E: B=bool:true, F=float:1.5, L=list[int]:[\"2\",\"3\"], \
             Mounts=list[path]:[\"/templates/data/in\",\"/opt\"], N=int:20, P=path:/templates/data/in, \
             Tag=string:s2"
        ]
    );
}

#[test]
fn format_string_values_and_defaults_convert_to_the_parameters_type() {
    let job = create_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions:
- {name: Seats, type: INT, default: 4}
- {name: Ratio, type: FLOAT, default: "2.50"}
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: N, type: INT, default: "{{Param.Seats}}"}
  - {name: R, type: FLOAT, default: "{{Param.Ratio}}"}
  - {name: Text, type: STRING, default: "n={{Param.Seats}}"}
  variables: {V: v}
steps:
- name: S
  dependencies: [{environment: E, parameters: {N: "1{{Param.Seats}}"}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[],
    );
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["JobTemplate/E: N=int:14, R=float:2.50, Text=string:n=4"]
    );
}

#[test]
fn a_value_that_does_not_convert_is_an_error_at_the_entry() {
    create_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Word, type: STRING, default: abc}]
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: N, type: INT, default: 1}]
  variables: {V: v}
steps:
- name: S
  dependencies: [{environment: E, parameters: {N: "{{Param.Word}}"}}]
  script: {actions: {onRun: {command: x}}}
- name: T
  dependencies: [{environment: E, parameters: {N: "{{Param.Word}}1"}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[],
        &[
            // A single expression evaluates with the parameter's type as
            // its target type (§3.2).
            "steps[0] -> dependencies[0] -> parameters -> N:\n\tcould not be resolved as a value of type INT: Cannot convert 'abc' to int: invalid digit found in string\n  Param.Word\n  ~~~~~~^~~~",
            // Any other format string resolves to a string, which converts
            // as a submitted Job Parameter value of the type would.
            "steps[1] -> dependencies[0] -> parameters -> N:\n\tcould not be resolved as a value of type INT: Value 'abc1' is not a valid integer or integer string.",
        ],
    );
}

#[test]
fn a_resolved_value_is_checked_against_the_constraints() {
    // Constraint 6 / §4.5 item 5, for a supplied value and a default.
    create_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Seats, type: INT, default: 9}]
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: N, type: INT, maxValue: 4, default: "{{Param.Seats}}"}]
  variables: {V: v}
steps:
- name: A
  dependencies: [{environment: E, parameters: {N: "{{Param.Seats}}"}}]
  script: {actions: {onRun: {command: x}}}
- name: B
  script: {actions: {onRun: {command: x}}}
"#,
        &[],
        &[
            "steps[0] -> dependencies[0] -> parameters -> N:\n\tParameter 'N': value 9 exceeds maximum 4",
            "jobEnvironments[0] -> envParameterDefinitions[0] -> default:\n\tfor Step 'B', which does not supply this parameter: Parameter 'N': value 9 exceeds maximum 4",
        ],
    );
}

// ── When a default is checked (§4.5 points 1–2; plan A19) ───────────

const UNUSED_DEFAULT: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Word, type: STRING, default: abc}]
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: N, type: INT, default: "{{Param.Word}}"}]
  variables: {V: v}
steps:
- name: A
  dependencies: [{environment: E, parameters: {N: 1}}]
  script: {actions: {onRun: {command: x}}}
"#;

#[test]
fn a_non_literal_default_no_step_takes_is_never_checked() {
    let job = create_ok(UNUSED_DEFAULT, &[]);
    assert_eq!(rendered_values(&job.steps[0]), ["JobTemplate/E: N=int:1"]);
}

#[test]
fn a_non_literal_default_is_reported_once_naming_the_first_step_that_takes_it() {
    let src = UNUSED_DEFAULT.replace(
        "  script: {actions: {onRun: {command: x}}}\n",
        "  script: {actions: {onRun: {command: x}}}\n- name: B\n  script: {actions: {onRun: {command: x}}}\n- name: C\n  script: {actions: {onRun: {command: x}}}\n",
    );
    create_err(
        &src,
        &[],
        &["jobEnvironments[0] -> envParameterDefinitions[0] -> default:\n\tfor Step 'B', which does not supply this parameter: could not be resolved as a value of type INT: Cannot convert 'abc' to int: invalid digit found in string\n  Param.Word\n  ~~~~~~^~~~"],
    );
}

#[test]
fn a_relative_literal_path_default_is_an_error_even_when_every_step_supplies_it() {
    create_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: P, type: PATH, default: rel/dir}]
  variables: {V: v}
steps:
- name: A
  dependencies: [{environment: E, parameters: {P: /abs}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[],
        &["jobEnvironments[0] -> envParameterDefinitions[0] -> default:\n\tmust be an absolute path in the submission path format (POSIX), since an Environment parameter has no directory to be relative to; got 'rel/dir'."],
    );
}

// ── PATH values (§4.5 PATH paragraph; plan A16) ─────────────────────

const PATHS: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Opt, type: PATH, default: ""}]
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: P, type: PATH}
  - {name: L, type: "LIST[PATH]", default: []}
  variables: {V: v}
steps:
- name: A
  dependencies: [{environment: E, parameters: {P: VALUE_P, L: VALUE_L}}]
  script: {actions: {onRun: {command: x}}}
"#;

fn paths(p: &str, l: &str) -> String {
    PATHS.replace("VALUE_P", p).replace("VALUE_L", l)
}

#[test]
fn relative_and_empty_paths_are_errors() {
    create_err(
        &paths("rel", r#"["/ok", "", "x/y"]"#),
        &[],
        &[
            "steps[0] -> dependencies[0] -> parameters -> L:\n\titem[1] must not be an empty path.",
            "steps[0] -> dependencies[0] -> parameters -> L:\n\titem[2] must be an absolute path in the submission path format (POSIX), since an Environment parameter has no directory to be relative to; got 'x/y'.",
            "steps[0] -> dependencies[0] -> parameters -> P:\n\tmust be an absolute path in the submission path format (POSIX), since an Environment parameter has no directory to be relative to; got 'rel'.",
        ],
    );
}

#[test]
fn an_empty_path_from_a_job_parameter_is_an_error() {
    // Plan A16: an optional, empty PATH Job Parameter does not pass through.
    create_err(
        &paths(r#""{{RawParam.Opt}}""#, "[]"),
        &[],
        &["steps[0] -> dependencies[0] -> parameters -> P:\n\tmust not be an empty path."],
    );
}

#[test]
fn absoluteness_follows_the_submission_path_format() {
    let jt = job_ok(&paths(r#"'C:\data'"#, r#"['D:\x']"#));
    let params = job_params(&jt, &[]);
    let windows = jt
        .default_validation_context()
        .with_submission_path_format(PathFormat::Windows);
    let job = create_job(&jt, &params, &windows).unwrap();
    // The value is stored exactly as written, not rewritten to POSIX.
    assert_eq!(
        rendered_values(&job.steps[0]),
        [r#"JobTemplate/E: L=list[path]:["D:\\x"], P=path:C:\data"#]
    );
    let err = create_job(&jt, &params, &posix_ctx(&jt)).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &[
            r"steps[0] -> dependencies[0] -> parameters -> L:
	item[0] must be an absolute path in the submission path format (POSIX), since an Environment parameter has no directory to be relative to; got 'D:\x'.",
            r"steps[0] -> dependencies[0] -> parameters -> P:
	must be an absolute path in the submission path format (POSIX), since an Environment parameter has no directory to be relative to; got 'C:\data'.",
        ],
    );
}

#[test]
fn a_uri_is_accepted_only_with_the_callers_permission() {
    let jt = job_ok(&paths("s3://bucket/key", "[]"));
    let params = job_params(&jt, &[]);
    let err = create_job(&jt, &params, &posix_ctx(&jt)).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &["steps[0] -> dependencies[0] -> parameters -> P:\n\tis a URI ('s3://bucket/key'), and URI path values are not permitted for this submission."],
    );
    let job = create_job(
        &jt,
        &params,
        &posix_ctx(&jt).with_allow_uri_path_values(true),
    )
    .unwrap();
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["JobTemplate/E: L=list[path]:[], P=path:s3://bucket/key"]
    );
}

// ── External entries kept for submission (plan decision 6) ──────────

#[test]
fn an_external_entry_resolves_in_the_steps_scope_at_job_creation() {
    // The entry's format strings read the Step's `let` binding and a PATH
    // Job Parameter's RawParam, both resolved before the attachment is
    // known; a `required: false` entry with no attachment is ignored.
    let jt = job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions:
- {name: Pkgs, type: STRING, default: numpy}
- {name: Unused, type: STRING, default: u}
- {name: Dir, type: PATH, default: d}
steps:
- name: S
  let: ["extra = Param.Pkgs + ' scipy'"]
  dependencies:
  - {dependsOn: T}
  - {environment: Conda, parameters: {Packages: "{{ extra }}", Channel: conda-forge, Mounts: "{{ [RawParam.Dir] }}"}}
  - {environment: Docker, required: false}
  script: {actions: {onRun: {command: x}}}
- name: T
  script: {actions: {onRun: {command: x}}}
"#,
    );
    let env = crate::env_params_common::env_ok(
        r#"
specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: Conda
  envParameterDefinitions:
  - {name: Packages, type: STRING, default: ""}
  - {name: Channel, type: STRING, default: defaults}
  - {name: Mounts, type: "LIST[PATH]", default: []}
  variables: {V: v}
"#,
    );
    let params = crate::env_params_common::job_params_with(&jt, &[], std::slice::from_ref(&env));
    let attached = [openjd_model::AttachedEnvironmentTemplate::new(&env)];
    let created = openjd_model::create_job_with_environment_templates(
        &jt,
        &attached,
        &params,
        &posix_ctx(&jt),
    )
    .unwrap_or_else(|e| panic!("job creation failed:\n{e}"));
    assert_eq!(
        rendered_values(&created.job.steps[0]),
        [
            r#"EnvironmentTemplate[0]/Conda: Channel=string:conda-forge, Mounts=list[path]:["/templates/d"], Packages=string:numpy scipy"#
        ]
    );
    assert_eq!(
        rendered_values(&created.job.steps[1]),
        ["EnvironmentTemplate[0]/Conda: Channel=string:defaults, Mounts=list[path]:[], Packages=string:"]
    );
    let warnings: Vec<String> = created.warnings.iter().map(ToString::to_string).collect();
    assert_eq!(
        warnings,
        ["JobTemplate -> steps[0] -> dependencies[2] -> environment: names Environment 'Docker', which is not a jobEnvironments entry of the Job Template, and none of the attached Environment Templates (EnvironmentTemplate[0]) defines it. The entry has 'required: false', so it has no effect."]
    );
}

#[test]
fn create_job_rejects_a_required_external_entry() {
    // `create_job` attaches no Environment Templates, so its Job is
    // complete: a required external entry is an error, reported for the
    // submission.
    let err = create(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
- name: S
  dependencies: [{environment: Conda}, {environment: Docker, required: false}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[],
    )
    .unwrap_err();
    assert_errors(
        &err.to_string(),
        "Submission",
        &["JobTemplate -> steps[0] -> dependencies[0] -> environment:\n\tnames Environment 'Conda', which is not a jobEnvironments entry of the Job Template, and no Environment Template is attached. The entry is required; set 'required: false' if the Step can run without the Environment (Template Schemas §3.2.1 item 2)."],
    );
}

// ── Equality (§3.2.2; plan decision 8, A1) ──────────────────────────

const EQUALITY: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: N, type: INT, default: 0}
  - {name: F, type: FLOAT, default: 0}
  - {name: Fs, type: "LIST[FLOAT]", default: []}
  - {name: P, type: PATH, default: /p}
  variables: {V: v}
steps:
- name: A
  dependencies: [{environment: E, parameters: VALUES_A}]
  script: {actions: {onRun: {command: x}}}
- name: B
  dependencies: [{environment: E, parameters: VALUES_B}]
  script: {actions: {onRun: {command: x}}}
"#;

fn pair(a: &str, b: &str) -> Job {
    create_ok(&EQUALITY.replace("VALUES_A", a).replace("VALUES_B", b), &[])
}

#[test]
fn int_1_and_intstring_1_are_equal() {
    let job = pair("{N: 1}", r#"{N: "1"}"#);
    assert!(job.steps[0].environment_values_equal(&job.steps[1]));
    assert_eq!(
        job.steps[0].environment_parameters,
        job.steps[1].environment_parameters
    );
}

#[test]
fn floats_compare_by_value_not_by_their_text() {
    let job = pair(r#"{F: 1.0, Fs: [2.5]}"#, r#"{F: "1.00", Fs: ["2.50"]}"#);
    assert!(job.steps[0].environment_values_equal(&job.steps[1]));
    // The transport form keeps each as written, so `==` differs.
    assert_ne!(
        job.steps[0].environment_parameters,
        job.steps[1].environment_parameters
    );
}

#[test]
fn different_values_and_paths_compare_unequal() {
    let job = pair("{N: 1}", "{N: 2}");
    assert!(!job.steps[0].environment_values_equal(&job.steps[1]));
    let job = pair("{F: 1.5}", "{F: 1.25}");
    assert!(!job.steps[0].environment_values_equal(&job.steps[1]));
    let job = pair("{Fs: [1.5]}", "{Fs: [1.5, 1.5]}");
    assert!(!job.steps[0].environment_values_equal(&job.steps[1]));
    // PATH values compare by unmapped string: `/a/b` is not `/a//b`.
    let job = pair("{P: /a/b}", "{P: /a//b}");
    assert!(!job.steps[0].environment_values_equal(&job.steps[1]));
}

#[test]
fn equality_matches_environments_by_document_and_name() {
    let job = pair("{N: 1}", "{N: 1}");
    let a = &job.steps[0];
    let mut b = job.steps[1].clone();
    assert!(a.environment_values_equal(&b));
    // The same values under another document are another Environment.
    b.environment_parameters.as_mut().unwrap()[0].document =
        Document::environment_template(0, None);
    assert!(!a.environment_values_equal(&b));
    // A Step with no values differs from one with values, both ways.
    b.environment_parameters = None;
    assert!(!a.environment_values_equal(&b));
    assert!(!b.environment_values_equal(a));
    assert!(b.environment_values_equal(&b.clone()));
}

// ── `PartialEq` / `Hash` on the new Step fields ────────────────────

fn hash_of<T: std::hash::Hash>(v: &T) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

#[test]
fn steps_with_equal_values_hash_equal_and_different_values_differ() {
    let job = pair("{N: 1}", r#"{N: "1"}"#);
    let (a, b) = (&job.steps[0], &job.steps[1]);
    assert_eq!(a.environment_parameters, b.environment_parameters);
    assert_eq!(
        hash_of(&a.environment_parameters),
        hash_of(&b.environment_parameters)
    );
    let other = pair("{N: 2}", "{N: 2}");
    assert_ne!(
        a.environment_parameters,
        other.steps[0].environment_parameters
    );
    assert_ne!(
        hash_of(&a.environment_parameters),
        hash_of(&other.steps[0].environment_parameters)
    );
}

// ── Error paths name the right Environment and entry ────────────────

#[test]
fn errors_are_reported_at_the_environment_and_entry_they_concern() {
    create_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E0
  envParameterDefinitions: [{name: X, type: STRING, default: x}]
  variables: {V: v}
- name: E1
  envParameterDefinitions: [{name: P, type: PATH, default: rel}, {name: Q, type: PATH, default: /q}]
  variables: {V: v}
steps:
- name: A
  script: {actions: {onRun: {command: x}}}
- name: B
  dependencies: [{environment: E0, parameters: {X: y}}, {environment: E1, parameters: {Q: also-rel}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[],
        &[
            "jobEnvironments[1] -> envParameterDefinitions[0] -> default:\n\tmust be an absolute path in the submission path format (POSIX), since an Environment parameter has no directory to be relative to; got 'rel'.",
            "steps[1] -> dependencies[1] -> parameters -> Q:\n\tmust be an absolute path in the submission path format (POSIX), since an Environment parameter has no directory to be relative to; got 'also-rel'.",
        ],
    );
}

// ── A JobTemplate built or edited in code (not decoded) ─────────────

#[test]
fn create_job_checks_keys_and_required_parameters_of_an_edited_template() {
    // Template validation rejects both of these for a decoded template;
    // `create_job` still checks them, since the template's fields are
    // public.
    let mut jt = job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: Need, type: STRING}]
  variables: {V: v}
steps:
- name: S
  dependencies: [{environment: E, parameters: {Need: n}}]
  script: {actions: {onRun: {command: x}}}
"#,
    );
    let params = job_params(&jt, &[]);
    let deps = jt.steps[0].dependencies.as_mut().unwrap();
    let map = &mut deps[0].parameters.as_mut().unwrap().0;
    let need = map.remove("Need").unwrap();
    map.insert("Other".to_string(), need);
    let err = create_job(&jt, &params, &posix_ctx(&jt)).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &[
            "steps[0] -> dependencies[0] -> parameters -> Other:\n\tEnvironment 'E' declares no parameter 'Other'. Declared: Need.",
            "steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every jobEnvironments entry with activation ALWAYS, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7); missing: 'Need' of Environment 'E'.",
        ],
    );
}

// ── More equality cases (§3.2.2) ────────────────────────────────────

/// Steps from two Jobs whose Environment `E` has the given definitions,
/// with every value from the defaults.
fn step_with_defs(defs: &str) -> openjd_model::job::Step {
    let job = create_ok(
        &format!(
            r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: {defs}
  variables: {{V: v}}
steps:
- name: S
  script: {{actions: {{onRun: {{command: x}}}}}}
"#
        ),
        &[],
    );
    job.steps[0].clone()
}

#[test]
fn strings_compare_by_text_even_when_they_look_like_floats() {
    let a = step_with_defs(r#"[{name: S, type: STRING, default: "1.0"}]"#);
    let b = step_with_defs(r#"[{name: S, type: STRING, default: "1.00"}]"#);
    assert!(!a.environment_values_equal(&b));
}

#[test]
fn values_differing_in_name_type_or_count_compare_unequal() {
    let base = step_with_defs("[{name: X, type: INT, default: 1}]");
    for other in [
        "[{name: Y, type: INT, default: 1}]",
        "[{name: X, type: FLOAT, default: 1}]",
        "[{name: X, type: INT, default: 1}, {name: Z, type: INT, default: 1}]",
    ] {
        let o = step_with_defs(other);
        assert!(!base.environment_values_equal(&o), "{other}");
        assert!(!o.environment_values_equal(&base), "{other}");
    }
    assert!(base.environment_values_equal(&step_with_defs("[{name: X, type: INT, default: 1}]")));
}

#[test]
fn equality_is_symmetric_when_a_list_repeats_an_environment() {
    let job = pair("{N: 1}", "{N: 1}");
    let mut a = job.steps[0].clone();
    let b = job.steps[1].clone();
    // `a` lists E twice; `b` lists E and another Environment.
    let e = a.environment_parameters.as_ref().unwrap()[0].clone();
    a.environment_parameters.as_mut().unwrap().push(e.clone());
    let mut b2 = b.clone();
    let mut f = e;
    f.environment = "F".to_string();
    b2.environment_parameters.as_mut().unwrap().push(f);
    assert!(!a.environment_values_equal(&b2));
    assert!(!b2.environment_values_equal(&a));
    assert!(!a.environment_values_equal(&a.clone()));
}

#[test]
fn create_job_rejects_keys_for_an_edited_job_environment_without_parameters() {
    let mut jt = job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: N, type: INT, default: 1}]
  variables: {V: v}
steps:
- name: S
  dependencies: [{environment: E, parameters: {N: 2}}]
  script: {actions: {onRun: {command: x}}}
"#,
    );
    let params = job_params(&jt, &[]);
    jt.job_environments.as_mut().unwrap()[0].env_parameter_definitions = None;
    let err = create_job(&jt, &params, &posix_ctx(&jt)).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &["steps[0] -> dependencies[0] -> parameters:\n\tEnvironment 'E' declares no parameters, so this entry must not supply any; it supplies N."],
    );
}

#[test]
fn a_default_cannot_read_job_name_at_job_creation() {
    // Plan A21: template validation rejects `Job.Name` in a default, and
    // job creation resolves defaults without it too, for a template edited
    // after decoding.
    let mut jt = job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: T, type: STRING, default: t}]
  variables: {V: v}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
    );
    let params = job_params(&jt, &[]);
    jt.job_environments.as_mut().unwrap()[0]
        .env_parameter_definitions
        .as_mut()
        .unwrap()[0]
        .default = Some(
        openjd_model::template::EnvironmentParameterValue::from_json(serde_json::json!(
            "{{ Job.Name }}"
        ))
        .unwrap(),
    );
    let err = create_job(&jt, &params, &posix_ctx(&jt)).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &["jobEnvironments[0] -> envParameterDefinitions[0] -> default:\n\tfor Step 'S', which does not supply this parameter: could not be resolved as a value of type STRING: Undefined variable: 'Job.Name'."],
    );
}

#[test]
fn an_edited_template_with_a_duplicated_job_environment_uses_the_first() {
    let mut jt = job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: X, type: STRING, default: first}]
  variables: {V: v}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
    );
    let params = job_params(&jt, &[]);
    let envs = jt.job_environments.as_mut().unwrap();
    let mut second = envs[0].clone();
    second.env_parameter_definitions.as_mut().unwrap()[0].default = Some(
        openjd_model::template::EnvironmentParameterValue::from_json(serde_json::json!("second"))
            .unwrap(),
    );
    envs.push(second);
    let job = create_job(&jt, &params, &posix_ctx(&jt)).unwrap();
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["JobTemplate/E: X=string:first"]
    );
}

#[test]
fn a_job_environment_default_uses_the_submission_path_format_for_path_operations() {
    let jt = job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Root, type: STRING, default: 'C:\data\proj'}]
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: Sub, type: PATH, default: "{{ path(Param.Root) / 'sub' }}"}
  - {name: Name, type: STRING, default: "{{ path(Param.Root).name }}"}
  variables: {V: v}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
    );
    let ctx = jt
        .default_validation_context()
        .with_submission_path_format(PathFormat::Windows);
    let job = create_job(&jt, &job_params(&jt, &[]), &ctx).unwrap();
    assert_eq!(
        rendered_values(&job.steps[0]),
        [r"JobTemplate/E: Name=string:proj, Sub=path:C:\data\proj\sub"]
    );
}

// ── Step `let` bindings in the submission path format ───────────────

const PATH_LET: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Root, type: STRING, default: 'C:\data\proj'}]
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: A, type: PATH, default: 'C:\a'}
  - {name: B, type: PATH, default: 'C:\b'}
  - {name: N, type: STRING, default: n}
  variables: {V: v}
steps:
- name: S
  let: ["p = path(Param.Root) / 'sub'", "q = p.parent"]
  dependencies: [{environment: E, parameters: {A: "{{ p }}", B: "{{ q / 'x' }}", N: "{{ q.name }}"}}]
  script: {actions: {onRun: {command: x}}}
"#;

#[test]
fn a_path_let_binding_follows_the_submission_path_format() {
    let jt = job_ok(PATH_LET);
    let ctx = jt
        .default_validation_context()
        .with_submission_path_format(PathFormat::Windows);
    let job = create_job(&jt, &job_params(&jt, &[]), &ctx)
        .unwrap_or_else(|e| panic!("create_job failed:\n{e}"));
    assert_eq!(
        rendered_values(&job.steps[0]),
        [r"JobTemplate/E: A=path:C:\data\proj\sub, B=path:C:\data\proj\x, N=string:proj"]
    );
}

#[test]
fn a_let_binding_that_fails_only_in_the_submission_path_format_is_reported() {
    let src = PATH_LET
        .replace(
            r#"let: ["p = path(Param.Root) / 'sub'", "q = p.parent"]"#,
            r#"let: ["r = path('/a/b').relative_to('/a')"]"#,
        )
        .replace(
            r#"{A: "{{ p }}", B: "{{ q / 'x' }}", N: "{{ q.name }}"}"#,
            r#"{N: "{{ r }}"}"#,
        )
        .replace("  - {name: A, type: PATH, default: 'C:\\a'}\n  - {name: B, type: PATH, default: 'C:\\b'}\n", "");
    let jt = job_ok(&src);
    let params = job_params(&jt, &[]);
    // POSIX: '/a/b' is relative to '/a'.
    let job = create_job(&jt, &params, &posix_ctx(&jt)).unwrap();
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["JobTemplate/E: N=string:b"]
    );
    let ctx = jt
        .default_validation_context()
        .with_submission_path_format(PathFormat::Windows);
    let err = create_job(&jt, &params, &ctx).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &["steps[0] -> let[0]:\n\tin the submission path format (WINDOWS), which this Step's Environment parameter values resolve in: Expression error: Error evaluating let binding 'r': relative_to failed: '\\a\\b' is not relative to '/a'\n  r = path('/a/b').relative_to('/a')\n      ~~~~~~~~~~~~~^~~~~~~~~~~~~~~~~"],
    );
}

#[test]
fn a_path_value_is_stored_as_resolved() {
    // Plan A22: §3.2.2 compares PATH values by their unmapped strings, and
    // the prototype does not rewrite them. A single expression evaluates
    // to a `path` in the submission format, whose `/` the WINDOWS rules
    // turn into `\`; a literal or a multi-segment format string is a string
    // and is kept as written. So in a WINDOWS submission `C:/data\proj`
    // written those ways gives different strings, which are unequal.
    let src = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Root, type: STRING, default: 'C:/data\proj'}]
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: P, type: PATH, default: "{{ Param.Root }}"}
  - {name: L, type: "LIST[PATH]", default: "{{ [Param.Root] }}"}
  variables: {V: v}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
- name: T
  dependencies: [{environment: E, parameters: {P: "{{Param.Root}}\\x", L: ['C:/data\proj']}}]
  script: {actions: {onRun: {command: x}}}
"#;
    let jt = job_ok(src);
    let ctx = jt
        .default_validation_context()
        .with_submission_path_format(PathFormat::Windows);
    let job = create_job(&jt, &job_params(&jt, &[]), &ctx).unwrap();
    assert_eq!(
        rendered_values(&job.steps[0]),
        [r#"JobTemplate/E: L=list[path]:["C:\\data\\proj"], P=path:C:\data\proj"#]
    );
    assert_eq!(
        rendered_values(&job.steps[1]),
        [r#"JobTemplate/E: L=list[path]:["C:/data\\proj"], P=path:C:/data\proj\x"#]
    );
    assert!(!job.steps[0].environment_values_equal(&job.steps[1]));
}

#[test]
fn an_empty_list_expression_for_a_list_path_parameter_is_accepted() {
    let job = create_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions:
- {name: LP, type: "LIST[PATH]", default: []}
- {name: N, type: INT, default: 1}
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: A, type: "LIST[PATH]", default: "{{ [] }}"}
  - {name: B, type: "LIST[PATH]", default: "{{ [x for x in RawParam.LP] }}"}
  - {name: C, type: "LIST[PATH]", default: "{{ RawParam.LP if Param.N > 5 else [] }}"}
  variables: {V: v}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
        &[],
    );
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["JobTemplate/E: A=list[path]:[], B=list[path]:[], C=list[path]:[]"]
    );
}

#[test]
fn a_let_binding_no_entry_reads_is_not_evaluated_again() {
    // `k` fails in the Windows format (`D:\out` has 2 parts there, 1 in
    // POSIX), but only the script reads it, so a Windows submission is not
    // rejected. `m`, which the entry reads, and `base`, which `m` reads,
    // are evaluated again.
    let jt = job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: Root, type: STRING, default: 'D:\out'}]
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: P, type: PATH, default: 'C:\p'}]
  variables: {V: v}
steps:
- name: S
  let: ["k = [1][len(path(Param.Root).parts) - 1]", "base = path(Param.Root)", "m = base / 'run'"]
  dependencies: [{environment: E, parameters: {P: "{{ m.parent / 'x' }}"}}]
  script: {actions: {onRun: {command: "{{ k }}"}}}
"#,
    );
    let ctx = jt
        .default_validation_context()
        .with_submission_path_format(PathFormat::Windows);
    let job = create_job(&jt, &job_params(&jt, &[]), &ctx)
        .unwrap_or_else(|e| panic!("create_job failed:\n{e}"));
    assert_eq!(
        rendered_values(&job.steps[0]),
        [r"JobTemplate/E: P=path:D:\out\x"]
    );
}

#[test]
fn allowed_values_see_the_value_that_is_stored() {
    // §4.5 item 5: every effective value satisfies its constraints. The
    // value checked is the value stored, so a Step's value is accepted
    // exactly when it is one of `allowedValues` as written there.
    let src = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: D, type: STRING, default: 'C:/a/b'}]
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: P, type: PATH, allowedValues: ['C:/a/b', 'C:\a\b'], default: 'C:/a/b'}]
  variables: {V: v}
steps:
- name: Literal
  dependencies: [{environment: E, parameters: {P: 'C:/a/b'}}]
  script: {actions: {onRun: {command: x}}}
- name: Text
  dependencies: [{environment: E, parameters: {P: '{{Param.D}} '}}]
  script: {actions: {onRun: {command: x}}}
- name: Expr
  dependencies: [{environment: E, parameters: {P: '{{ Param.D }}'}}]
  script: {actions: {onRun: {command: x}}}
"#;
    let jt = job_ok(src);
    let ctx = jt
        .default_validation_context()
        .with_submission_path_format(PathFormat::Windows);
    let err = create_job(&jt, &job_params(&jt, &[]), &ctx).unwrap_err();
    // `{{Param.D}} ` is the string 'C:/a/b ', which is not allowed; the
    // single expression is the path C:\a\b, which is.
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &["steps[1] -> dependencies[0] -> parameters -> P:\n\tParameter 'P': value 'C:/a/b ' is not in allowed values"],
    );
    // `{{Param.D}}` alone is a single expression too.
    let jt = job_ok(&src.replace("{P: '{{Param.D}} '}", "{P: '{{Param.D}}'}"));
    let job = create_job(&jt, &job_params(&jt, &[]), &ctx).unwrap();
    let values: Vec<_> = job.steps.iter().map(rendered_values).collect();
    assert_eq!(
        values,
        [
            ["JobTemplate/E: P=path:C:/a/b"],
            [r"JobTemplate/E: P=path:C:\a\b"],
            [r"JobTemplate/E: P=path:C:\a\b"]
        ]
    );
}

#[test]
fn a_leading_double_backslash_is_relative_in_posix() {
    // In POSIX a backslash is an ordinary filename character, so
    // `\\srv\share` is relative there; in WINDOWS it is a UNC path.
    let src = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: P, type: PATH, default: '\\srv\share'}]
  variables: {V: v}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#;
    let jt = job_ok(src);
    let params = job_params(&jt, &[]);
    let err = create_job(&jt, &params, &posix_ctx(&jt)).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &[
            r"jobEnvironments[0] -> envParameterDefinitions[0] -> default:
	must be an absolute path in the submission path format (POSIX), since an Environment parameter has no directory to be relative to; got '\\srv\share'.",
        ],
    );
    let windows = jt
        .default_validation_context()
        .with_submission_path_format(PathFormat::Windows);
    let job = create_job(&jt, &params, &windows).unwrap();
    assert_eq!(
        rendered_values(&job.steps[0]),
        [r"JobTemplate/E: P=path:\\srv\share"]
    );
}

#[test]
fn equality_matches_several_environments_of_one_document_in_any_order() {
    let job = create_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: N, type: INT, default: 0}]
  variables: {V: v}
- name: F
  envParameterDefinitions: [{name: N, type: INT, default: 0}]
  variables: {V: v}
steps:
- name: A
  script: {actions: {onRun: {command: x}}}
- name: B
  script: {actions: {onRun: {command: x}}}
- name: C
  dependencies: [{environment: F, parameters: {N: 1}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[],
    );
    assert!(job.steps[0].environment_values_equal(&job.steps[1]));
    let mut b = job.steps[1].clone();
    b.environment_parameters.as_mut().unwrap().swap(0, 1);
    assert!(job.steps[0].environment_values_equal(&b));
    assert!(!job.steps[0].environment_values_equal(&job.steps[2]));
}

#[test]
fn a_path_value_that_fails_its_path_check_is_not_also_checked_against_constraints() {
    create_err(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: X, type: STRING, default: rel}]
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: P, type: PATH, allowedValues: [/a], default: /a}]
  variables: {V: v}
steps:
- name: S
  dependencies: [{environment: E, parameters: {P: "{{Param.X}}"}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[],
        &["steps[0] -> dependencies[0] -> parameters -> P:\n\tmust be an absolute path in the submission path format (POSIX), since an Environment parameter has no directory to be relative to; got 'rel'."],
    );
}

#[test]
fn create_job_rejects_a_second_entry_for_an_environment_in_an_edited_template() {
    let mut jt = job_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: N, type: INT, default: 0}]
  variables: {V: v}
steps:
- name: S
  dependencies: [{environment: E, parameters: {N: 1}}, {environment: Ext, parameters: {I: 1}}]
  script: {actions: {onRun: {command: x}}}
"#,
    );
    let params = job_params(&jt, &[]);
    let deps = jt.steps[0].dependencies.as_mut().unwrap();
    let (e, x) = (deps[0].clone(), deps[1].clone());
    deps.push(e);
    deps.push(x);
    let err = create_job(&jt, &params, &posix_ctx(&jt)).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &[
            "steps[0] -> dependencies[2] -> environment:\n\tduplicate environment dependency 'E'.",
            "steps[0] -> dependencies[3] -> environment:\n\tduplicate environment dependency 'Ext'.",
        ],
    );
}
