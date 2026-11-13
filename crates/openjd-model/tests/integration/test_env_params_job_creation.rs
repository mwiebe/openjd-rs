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

use crate::env_params_common::{assert_errors, job_ok, job_params, rendered_values, template_ctx};

/// Create the Job with the template's own context.
fn create(jt_src: &str, params: &[(&str, &str)]) -> Result<Job, ModelError> {
    let jt = job_ok(jt_src);
    create_job(&jt, &job_params(&jt, params), &template_ctx(&jt))
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
    // §3.2 item 3: an entry does not delay scheduling.
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

// ── Conversion of values (§3.2 bullets, §4.5 item 1) ───────────────

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
    // Constraint 6 / §4.5 item 2, for a supplied value and a default.
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

// ── When a default is checked (§4.5 item 2; plan A19) ──────────────

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
fn a_relative_literal_path_default_is_used_as_given() {
    // §4.5 item 3 (spec af53020): a PATH value need not be absolute, so a
    // relative literal `default` is no error, even when a Step supplies
    // the parameter, and a Step that takes it gets it unchanged.
    let src = r#"
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
- name: B
  script: {actions: {onRun: {command: x}}}
"#;
    let job = create_ok(src, &[]);
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["JobTemplate/E: P=path:/abs"]
    );
    assert_eq!(
        rendered_values(&job.steps[1]),
        ["JobTemplate/E: P=path:rel/dir"]
    );
}

// ── PATH values (§4.5 item 3 at spec af53020) ──────────────────────

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

/// Every form a PATH value can be written in, as a YAML single-quoted
/// value and the text it stands for: absolute in POSIX, in WINDOWS (drive
/// with either separator, UNC with either separator), relative in either
/// style, drive-relative, drive-less rooted, and a URI.
const FORMS: [(&str, &str); 13] = [
    ("'/mnt/proj'", "/mnt/proj"),
    (r"'C:\proj'", r"C:\proj"),
    ("'C:/proj'", "C:/proj"),
    (r"'\\server\share\proj'", r"\\server\share\proj"),
    ("'//server/share'", "//server/share"),
    ("'rel/dir'", "rel/dir"),
    (r"'relative\dir'", r"relative\dir"),
    ("'C:dir'", "C:dir"),
    (r"'\dir'", r"\dir"),
    ("'./x'", "./x"),
    ("'~/x'", "~/x"),
    ("'s3://bucket/key'", "s3://bucket/key"),
    ("''", ""),
];

/// §4.5 item 3 (spec af53020): a PATH value, and each LIST[PATH] element,
/// is used as given, whatever its form, relative and empty included. It
/// is the same value whether it is written as a literal, a single `{{ }}`
/// expression whose result is a string, or a multi-segment format string.
#[test]
fn every_path_form_is_used_as_given() {
    for (value, text) in FORMS {
        // The text as an expression-language string literal, then each
        // value in a YAML single-quoted scalar, where `'` doubles.
        let expr = format!("'{}'", text.replace('\\', "\\\\"));
        let yaml = |s: &str| format!("'{}'", s.replace('\'', "''"));
        for (p, l) in [
            (value.to_string(), format!("[{value}]")),
            (
                yaml(&format!("{{{{ {expr} }}}}")),
                yaml(&format!("{{{{ [{expr}] }}}}")),
            ),
            (yaml(&format!("{{{{ '' }}}}{text}")), format!("[{value}]")),
        ] {
            let job = create(&paths(&p, &l), &[]).unwrap_or_else(|e| panic!("{p}: {e}"));
            let json = serde_json::to_string(text).unwrap();
            assert_eq!(
                rendered_values(&job.steps[0]),
                [format!(
                    "JobTemplate/E: L=list[path]:[{json}], P=path:{text}"
                )],
                "{p}"
            );
        }
    }
}

#[test]
fn an_empty_path_from_a_job_parameter_is_used_as_given() {
    // An optional, empty PATH Job Parameter passes through unchanged.
    let job = create_ok(&paths(r#""{{RawParam.Opt}}""#, "[]"), &[]);
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["JobTemplate/E: L=list[path]:[], P=path:"]
    );
}

/// §4.5 item 3, for a Job Environment's literal `default`s: one a Step
/// takes and one no Step takes, of any form, are used as given.
#[test]
fn a_job_environment_literal_default_of_any_form_is_used_as_given() {
    let job = create_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: Taken, type: PATH, default: 'C:\tools'}
  - {name: Unused, type: PATH, default: 'rel\share'}
  - {name: Rel, type: PATH, default: 'tools/bin'}
  - {name: L, type: "LIST[PATH]", default: ['C:\x', 'y', '\\srv\s', 'C:z']}
  variables: {V: v}
steps:
- name: S
  dependencies: [{environment: E, parameters: {Unused: /u}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[],
    );
    assert_eq!(
        rendered_values(&job.steps[0]),
        [
            r#"JobTemplate/E: L=list[path]:["C:\\x","y","\\\\srv\\s","C:z"], Rel=path:tools/bin, Taken=path:C:\tools, Unused=path:/u"#
        ]
    );
}

/// §4.5 item 3 (spec af53020) for a Job Environment's `default`s: an
/// empty value and a URI, as a literal and as a format string, a Step
/// takes and no Step takes, are used as given.
#[test]
fn a_job_environments_empty_and_uri_defaults_are_used_as_given() {
    let src = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: LitEmpty, type: PATH, default: ''}
  - {name: LitUri, type: PATH, default: 's3://b/lit'}
  - {name: FsEmpty, type: PATH, default: "{{ '' }}"}
  - {name: FsUri, type: PATH, default: "{{ 's3://b/fs' }}"}
  - {name: L, type: "LIST[PATH]", default: ['', 's3://b/l']}
  variables: {V: v}
steps:
- name: Takes
  script: {actions: {onRun: {command: x}}}
- name: Supplies
  dependencies: [{environment: E, parameters: {LitEmpty: /a, LitUri: /b, FsEmpty: /c, FsUri: /d, L: [/e]}}]
  script: {actions: {onRun: {command: x}}}
"#;
    let job = create_ok(src, &[]);
    assert_eq!(
        rendered_values(&job.steps[0]),
        [
            r#"JobTemplate/E: FsEmpty=path:, FsUri=path:s3://b/fs, L=list[path]:["","s3://b/l"], LitEmpty=path:, LitUri=path:s3://b/lit"#
        ]
    );
    assert_eq!(
        rendered_values(&job.steps[1]),
        [
            r#"JobTemplate/E: FsEmpty=path:/c, FsUri=path:/d, L=list[path]:["/e"], LitEmpty=path:/a, LitUri=path:/b"#
        ]
    );
}

/// Expression Language §1.2.1: TEMPLATE-scope fields (§7.3.2, §7.4)
/// evaluate with POSIX semantics, so a Job Environment's format-string `default`
/// joins paths with `/`, a Windows drive path included.
#[test]
fn a_job_environment_format_string_default_evaluates_with_posix_paths() {
    let job = create_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: Win, type: PATH, default: "{{ path('C:/tools') / 'bin' }}"}
  - {name: Posix, type: PATH, default: "{{ '/opt' }}/bin"}
  - {name: L, type: "LIST[PATH]", default: "{{ [path('C:/x') / 'y', path('rel') / 'z'] }}"}
  variables: {V: v}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
        &[],
    );
    assert_eq!(
        rendered_values(&job.steps[0]),
        [
            r#"JobTemplate/E: L=list[path]:["C:/x/y","rel/z"], Posix=path:/opt/bin, Win=path:C:/tools/bin"#
        ]
    );
}

/// §4.5 item 3: a LIST[PATH] value may mix forms.
#[test]
fn a_list_path_value_may_mix_path_forms() {
    let job = create_ok(
        &paths(
            "/p",
            r#"['/mnt/a', 'D:\b', 'E:/c', '\\server\share\d', 'rel', 'C:e', '\f']"#,
        ),
        &[],
    );
    assert_eq!(
        rendered_values(&job.steps[0]),
        [
            r#"JobTemplate/E: L=list[path]:["/mnt/a","D:\\b","E:/c","\\\\server\\share\\d","rel","C:e","\\f"], P=path:/p"#
        ]
    );
}

/// Job Parameter values preprocessed in `format`, with directories of
/// that format, so a value absolute in that format is kept as given.
fn params_in(
    jt: &openjd_model::template::JobTemplate,
    params: &[(&str, &str)],
    format: PathFormat,
) -> openjd_model::JobParameterValues {
    params_in_with(jt, params, &[], format)
}

/// [`params_in`] with list values too.
fn params_in_with(
    jt: &openjd_model::template::JobTemplate,
    params: &[(&str, &str)],
    lists: &[(&str, &[&str])],
    format: PathFormat,
) -> openjd_model::JobParameterValues {
    let input: openjd_model::JobParameterInputValues = params
        .iter()
        .map(|(k, v)| (k.to_string(), openjd_expr::ExprValue::String(v.to_string())))
        .chain(lists.iter().map(|(k, v)| {
            (
                k.to_string(),
                openjd_expr::ExprValue::ListString(v.iter().map(|s| s.to_string()).collect(), 0),
            )
        }))
        .collect();
    let (dir, cwd) = match format {
        PathFormat::Windows => (r"C:\templates", r"C:\work"),
        _ => ("/templates", "/work"),
    };
    let mut opts = openjd_model::PathParameterOptions::new(dir, cwd);
    opts.path_format = format;
    openjd_model::preprocess_job_parameters(jt, &input, &[], &opts)
        .unwrap_or_else(|e| panic!("preprocess_job_parameters failed:\n{e}"))
}

const RAW: &str = r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions:
- {name: Dir, type: PATH}
- {name: Mounts, type: "LIST[PATH]"}
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: P, type: PATH}
  - {name: L, type: "LIST[PATH]"}
  - {name: Lit, type: PATH, default: LITERAL}
  - {name: Multi, type: PATH, default: "{{RawParam.Dir}}{{ '' }}"}
  variables: {V: v}
steps:
- name: A
  dependencies: [{environment: E, parameters: {P: "{{RawParam.Dir}}", L: "{{RawParam.Mounts}}"}}]
  script: {actions: {onRun: {command: x}}}
"#;

/// Rule 1: a PATH or LIST[PATH] value written as a single `{{ }}`
/// expression whose result is a `string` (`RawParam.<PATH>`) or a
/// `list[string]` keeps that text: `{{RawParam.Dir}}` with `/data`,
/// `C:\x` or `\\server\share` is exactly that string, as the literal and
/// a multi-segment format string with the same text are. Job creation has
/// no path format to convert it in.
#[test]
fn a_single_expression_string_result_is_the_value_as_written() {
    for (format, dir, mounts) in [
        (PathFormat::Posix, "/data", &["/a", "/b/c"][..]),
        (PathFormat::Windows, r"C:\x", &[r"C:\a", r"D:\b"][..]),
        (
            PathFormat::Windows,
            r"\\server\share",
            &[r"\\srv\a", r"E:\b"][..],
        ),
    ] {
        let lit = format!("'{dir}'");
        let jt = job_ok(&RAW.replace("LITERAL", &lit));
        let params = params_in_with(&jt, &[("Dir", dir)], &[("Mounts", mounts)], format);
        let job =
            create_job(&jt, &params, &template_ctx(&jt)).unwrap_or_else(|e| panic!("{dir}: {e}"));
        assert_eq!(
            rendered_values(&job.steps[0]),
            [format!(
                "JobTemplate/E: L=list[path]:{}, Lit=path:{dir}, Multi=path:{dir}, P=path:{dir}",
                serde_json::to_string(mounts).unwrap()
            )],
            "{dir}"
        );
    }
}

/// Rule 1 for a `list[string]` result: each element is kept as written,
/// whatever its form, a mix of forms included.
#[test]
fn a_single_expression_list_of_strings_is_kept_element_by_element() {
    let job = create_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions:
- {name: Mounts, type: "LIST[STRING]", default: ['/a', 'C:\b', '\\srv\c', 'rel\d', 'C:e']}
jobEnvironments:
- name: E
  envParameterDefinitions: [{name: L, type: "LIST[PATH]"}]
  variables: {V: v}
steps:
- name: A
  dependencies: [{environment: E, parameters: {L: "{{Param.Mounts}}"}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[],
    );
    assert_eq!(
        rendered_values(&job.steps[0]),
        [r#"JobTemplate/E: L=list[path]:["/a","C:\\b","\\\\srv\\c","rel\\d","C:e"]"#]
    );
}

/// Rule 2: a `path` result evaluates with POSIX semantics (Expression
/// Language §1.2.1): `path(RawParam.Dir) / 'x'` joins with `/` and treats
/// `\` as a filename character, whatever format the Job Parameter was
/// given in. Nothing in the context selects another format, so the
/// result is the same on every host (this test has no `cfg`, and CI runs
/// it on Linux, macOS and Windows).
#[test]
fn a_path_expression_evaluates_with_posix_semantics() {
    let src = PATHS
        .replace(
            r#"parameterDefinitions: [{name: Opt, type: PATH, default: ""}]"#,
            "parameterDefinitions: [{name: Dir, type: PATH}]",
        )
        .replace("P: VALUE_P", r#"P: "{{ path(RawParam.Dir) / 'x' }}""#)
        .replace(
            "L: VALUE_L",
            r#"L: "{{ [path(RawParam.Dir) / 'y', path(RawParam.Dir).parent] }}""#,
        );
    let jt = job_ok(&src);
    for (format, dir, expected) in [
        (
            PathFormat::Posix,
            "/data/in",
            r#"JobTemplate/E: L=list[path]:["/data/in/y","/data"], P=path:/data/in/x"#,
        ),
        (
            PathFormat::Windows,
            r"C:\data\in",
            r#"JobTemplate/E: L=list[path]:["C:\\data\\in/y","."], P=path:C:\data\in/x"#,
        ),
        (
            PathFormat::Windows,
            "C:/data/in",
            r#"JobTemplate/E: L=list[path]:["C:/data/in/y","C:/data"], P=path:C:/data/in/x"#,
        ),
    ] {
        let job = create_job(
            &jt,
            &params_in(&jt, &[("Dir", dir)], format),
            &template_ctx(&jt),
        )
        .unwrap_or_else(|e| panic!("{dir}: {e}"));
        assert_eq!(rendered_values(&job.steps[0]), [expected], "{dir}");
    }
}

/// Rule 2 for a multi-segment format string: its result is a string, but
/// the path operations inside it still use POSIX semantics, in an entry
/// value and in a `default` alike.
#[test]
fn path_operations_in_a_multi_segment_value_use_posix_semantics() {
    let job = create_ok(
        &PATHS
            .replace(
                "  - {name: L, type: \"LIST[PATH]\", default: []}",
                "  - {name: L, type: \"LIST[PATH]\", default: []}\n  - {name: D, type: PATH, default: \"{{ path('C:/d') / 'e' }}/f\"}",
            )
            .replace("P: VALUE_P", r#"P: "{{ path('C:/a') / 'b' }}/x""#)
            .replace("L: VALUE_L", "L: []"),
        &[],
    );
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["JobTemplate/E: D=path:C:/d/e/f, L=list[path]:[], P=path:C:/a/b/x"]
    );
}

/// Every job-creation evaluation has POSIX semantics, a Step's
/// `hostRequirements` expressions included: `a\b` is one POSIX path name,
/// so its `parts` has one element in the `anyOf` list form and in a
/// capability name (a Windows reading would give 2).
#[test]
fn host_requirement_expressions_evaluate_with_posix_semantics() {
    let job = create_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
parameterDefinitions: [{name: S, type: STRING, default: 'a\b'}]
steps:
- name: A
  hostRequirements:
    attributes:
    - {name: "attr.custom.n{{ len(path(Param.S).parts) }}", anyOf: "{{ ['n' + string(len(path(Param.S).parts))] }}"}
  script: {actions: {onRun: {command: x}}}
"#,
        &[],
    );
    let attrs = job.steps[0]
        .host_requirements
        .as_ref()
        .and_then(|h| h.attributes.clone())
        .unwrap();
    assert_eq!(attrs.len(), 1);
    assert_eq!(attrs[0].name, "attr.custom.n1");
    assert_eq!(attrs[0].any_of, Some(vec!["n1".to_string()]));
}

/// §4.5 item 3 for the format-string forms at a Job Environment's sites:
/// an empty, a URI and a relative result of a multi-segment `default`, a
/// single-expression LIST[PATH] `default`, and a multi-segment entry value
/// are used as given.
#[test]
fn a_job_environments_format_string_values_of_any_form_are_used_as_given() {
    let job = create_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: ME, type: PATH, default: "{{ '' }}{{ '' }}"}
  - {name: MU, type: PATH, default: "s3://b/{{ 'k' }}"}
  - {name: MR, type: PATH, default: "rel/{{ 'k' }}"}
  - {name: OL, type: "LIST[PATH]", default: "{{ ['', 's3://b/x', 'rel/y'] }}"}
  - {name: X, type: PATH, default: /x}
  variables: {V: v}
steps:
- name: A
  script: {actions: {onRun: {command: x}}}
- name: B
  dependencies: [{environment: E, parameters: {X: "{{ '' }}{{ '' }}"}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[],
    );
    assert_eq!(
        rendered_values(&job.steps[0]),
        [
            r#"JobTemplate/E: ME=path:, MR=path:rel/k, MU=path:s3://b/k, OL=list[path]:["","s3://b/x","rel/y"], X=path:/x"#
        ]
    );
    assert_eq!(
        rendered_values(&job.steps[1]),
        [
            r#"JobTemplate/E: ME=path:, MR=path:rel/k, MU=path:s3://b/k, OL=list[path]:["","s3://b/x","rel/y"], X=path:"#
        ]
    );
}

/// §3.2.2 compares PATH values by their unmapped strings, and since the
/// values are kept as written, `C:\x`, `C:/x` and `c:\X` are three
/// different values: neither separators nor letter case are normalized.
#[test]
fn path_values_written_differently_are_different_values() {
    let job = create_ok(
        r#"
specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
jobEnvironments:
- name: E
  envParameterDefinitions:
  - {name: P, type: PATH, default: /p}
  - {name: L, type: "LIST[PATH]", default: [/a]}
  variables: {V: v}
steps:
- name: Back
  dependencies: [{environment: E, parameters: {P: 'C:\x'}}]
  script: {actions: {onRun: {command: x}}}
- name: Fwd
  dependencies: [{environment: E, parameters: {P: 'C:/x'}}]
  script: {actions: {onRun: {command: x}}}
- name: Case
  dependencies: [{environment: E, parameters: {P: 'c:\X'}}]
  script: {actions: {onRun: {command: x}}}
- name: List
  dependencies: [{environment: E, parameters: {L: ['\a']}}]
  script: {actions: {onRun: {command: x}}}
- name: Same
  dependencies: [{environment: E, parameters: {P: 'C:\x'}}]
  script: {actions: {onRun: {command: x}}}
- name: Default
  script: {actions: {onRun: {command: x}}}
"#,
        &[],
    );
    let s = &job.steps;
    assert!(!s[0].environment_values_equal(&s[1]), "separators");
    assert!(!s[0].environment_values_equal(&s[2]), "letter case");
    assert!(
        !s[3].environment_values_equal(&s[5]),
        "list element separators"
    );
    assert!(s[0].environment_values_equal(&s[4]), "the same text");
}

#[test]
fn a_uri_path_value_is_used_as_given() {
    // A URI is one more form of value: no caller policy applies to it.
    let job = create_ok(&paths("s3://bucket/key", "['s3://b/k', 'rel']"), &[]);
    assert_eq!(
        rendered_values(&job.steps[0]),
        [r#"JobTemplate/E: L=list[path]:["s3://b/k","rel"], P=path:s3://bucket/key"#]
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
        &template_ctx(&jt),
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
        "JobTemplate",
        &["steps[0] -> dependencies[0] -> environment:\n\tnames Environment 'Conda', which is not a jobEnvironments entry of the Job Template, and no Environment Template is attached. The entry is required; set 'required: false' if the Step can run without the Environment (Template Schemas §3.2.1 item 2)."],
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
  - {name: B, type: BOOL, default: false}
  - {name: S, type: STRING, default: s}
  - {name: Ls, type: "LIST[LIST[INT]]", default: []}
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
fn floats_compare_by_their_text_not_by_value() {
    // §3.2.2: a FLOAT keeps the text it was written with, so the same
    // number written differently is a different value, scalar or element.
    for (a, b) in [
        (r#"{F: "3.10"}"#, r#"{F: "3.1"}"#),
        (r#"{F: "1.0"}"#, r#"{F: "1.00"}"#),
        (r#"{Fs: ["1.50"]}"#, r#"{Fs: ["1.5"]}"#),
        (r#"{Fs: ["2.5", "1.50"]}"#, r#"{Fs: ["2.5", "1.5"]}"#),
        (r#"{F: "1e2"}"#, r#"{F: "100.0"}"#),
        // A YAML number has no text of its own: 1.50 is written 1.5.
        (r#"{F: 1.50}"#, r#"{F: "1.50"}"#),
        (r#"{Fs: [1.50]}"#, r#"{Fs: ["1.50"]}"#),
    ] {
        let job = pair(a, b);
        assert!(
            !job.steps[0].environment_values_equal(&job.steps[1]),
            "{a} {b}"
        );
        assert!(
            !job.steps[1].environment_values_equal(&job.steps[0]),
            "{a} {b}"
        );
    }
}

#[test]
fn floats_written_alike_are_equal() {
    // White space around a FLOAT is not part of its text (§7.5), and an
    // expression that copies a FLOAT keeps its text (Expression Language
    // §1.3.4), so these are equal, and equal values have one transport form.
    for (a, b) in [
        (r#"{F: "3.10"}"#, r#"{F: " 3.10 "}"#),
        (r#"{F: "3.10"}"#, r#"{F: "{{ 3.10 }}"}"#),
        (r#"{Fs: ["1.50", "2"]}"#, r#"{Fs: [" 1.50 ", "2"]}"#),
        (r#"{F: 1.50}"#, r#"{F: "1.5"}"#),
    ] {
        let job = pair(a, b);
        assert!(
            job.steps[0].environment_values_equal(&job.steps[1]),
            "{a} {b}"
        );
        assert_eq!(
            job.steps[0].environment_parameters, job.steps[1].environment_parameters,
            "{a} {b}"
        );
    }
}

#[test]
fn ints_and_bools_have_one_string_form() {
    // §3.2.2: an INT or BOOL resolves to one value whatever its spelling.
    for (a, b) in [
        (r#"{N: "007"}"#, "{N: 7}"),
        (r#"{N: " 7 "}"#, "{N: 7}"),
        (r#"{B: "yes"}"#, "{B: true}"),
        (r#"{Ls: [["01", 2]]}"#, "{Ls: [[1, 2]]}"),
    ] {
        let job = pair(a, b);
        assert!(
            job.steps[0].environment_values_equal(&job.steps[1]),
            "{a} {b}"
        );
    }
    let job = pair("{Ls: [[1, 2]]}", "{Ls: [[1], [2]]}");
    assert!(!job.steps[0].environment_values_equal(&job.steps[1]));
}

#[test]
fn strings_compare_by_code_points_without_normalization() {
    // §3.2.2: U+00E9 and "e" with U+0301 render alike but are not equal.
    let job = pair(r#"{S: "caf\u00e9"}"#, r#"{S: "cafe\u0301"}"#);
    assert!(!job.steps[0].environment_values_equal(&job.steps[1]));
    let job = pair(r#"{S: "caf\u00e9"}"#, r#"{S: "café"}"#);
    assert!(job.steps[0].environment_values_equal(&job.steps[1]));
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
  envParameterDefinitions:
  - {name: P, type: PATH, allowedValues: [/p], default: "{{ 'rel' }}"}
  - {name: Q, type: PATH, allowedValues: [/q], default: /q}
  variables: {V: v}
steps:
- name: A
  script: {actions: {onRun: {command: x}}}
- name: B
  dependencies: [{environment: E0, parameters: {X: y}}, {environment: E1, parameters: {Q: "{{ 'also-rel' }}"}}]
  script: {actions: {onRun: {command: x}}}
"#,
        &[],
        &[
            "jobEnvironments[1] -> envParameterDefinitions[0] -> default:\n\tfor Step 'A', which \
             does not supply this parameter: Parameter 'P': value 'rel' is not in allowed values",
            "steps[1] -> dependencies[1] -> parameters -> Q:\n\tParameter 'Q': value 'also-rel' \
             is not in allowed values",
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
    let err = create_job(&jt, &params, &template_ctx(&jt)).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &[
            "steps[0] -> dependencies[0] -> parameters -> Other:\n\tEnvironment 'E' declares no parameter 'Other'. Declared: Need.",
            "steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every jobEnvironments entry with activation ALWAYS, each one with activation BY_DEFAULT that the Step does not decline, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 constraint 7); missing: 'Need' of Environment 'E'.",
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
    let err = create_job(&jt, &params, &template_ctx(&jt)).unwrap_err();
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
    let err = create_job(&jt, &params, &template_ctx(&jt)).unwrap_err();
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
    let job = create_job(&jt, &params, &template_ctx(&jt)).unwrap();
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["JobTemplate/E: X=string:first"]
    );
}

#[test]
fn a_job_environment_default_uses_posix_for_path_operations() {
    // Expression Language §1.2.1: TEMPLATE scope has POSIX semantics, so a
    // Windows-looking string is one POSIX path name: `/` joins it, and its
    // `name` is the whole string.
    let job = create_ok(
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
        &[],
    );
    assert_eq!(
        rendered_values(&job.steps[0]),
        [r"JobTemplate/E: Name=string:C:\data\proj, Sub=path:C:\data\proj/sub"]
    );
}

// ── Step `let` bindings, evaluated once with POSIX semantics ─────────

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
fn a_path_let_binding_evaluates_with_posix_semantics() {
    // §7.3.2: an entry's values read the Step's `let` bindings, which job
    // creation evaluates once, with POSIX semantics (Expression Language
    // §1.2.1), as for every other field of the Step.
    let job = create_ok(PATH_LET, &[]);
    assert_eq!(
        rendered_values(&job.steps[0]),
        [r"JobTemplate/E: A=path:C:\data\proj/sub, B=path:C:\data\proj/x, N=string:C:\data\proj"]
    );
}

#[test]
fn a_let_binding_that_fails_with_posix_semantics_is_reported_once() {
    // The Step's `let` bindings are evaluated once, with POSIX semantics, so
    // a failing one is the Step's ordinary `let` error, reported once.
    let src = PATH_LET
        .replace(
            "parameterDefinitions: [{name: Root, type: STRING, default: 'C:\\data\\proj'}]",
            "parameterDefinitions: [{name: Root, type: STRING}, {name: Base, type: STRING}]",
        )
        .replace(
            r#"let: ["p = path(Param.Root) / 'sub'", "q = p.parent"]"#,
            r#"let: ["r = path(Param.Root).relative_to(Param.Base)"]"#,
        )
        .replace(
            r#"{A: "{{ p }}", B: "{{ q / 'x' }}", N: "{{ q.name }}"}"#,
            r#"{N: "{{ r }}"}"#,
        )
        .replace("  - {name: A, type: PATH, default: 'C:\\a'}\n  - {name: B, type: PATH, default: 'C:\\b'}\n", "");
    let job = create_ok(&src, &[("Root", "/a/b"), ("Base", "/a")]);
    assert_eq!(
        rendered_values(&job.steps[0]),
        ["JobTemplate/E: N=string:b"]
    );
    create_err(
        &src,
        &[("Root", "/a/b"), ("Base", "/c")],
        &[
            "steps[0] -> let[0]:\n\tInvalid expression in let binding 'r': relative_to failed: \
           '/a/b' is not relative to '/c'\n  r = path(Param.Root).relative_to(Param.Base)\n      \
           ~~~~~~~~~~~~~~~~~^~~~~~~~~~~~~~~~~~~~~~~",
        ],
    );
}

#[test]
fn a_path_value_is_the_same_however_it_is_written() {
    // Plan A22: §3.2.2 compares PATH values by their unmapped strings, and
    // the prototype does not rewrite them. A single expression's string
    // result, a multi-segment format string and a literal with the same
    // text give the same value, a mix of separators included, so the two
    // Steps' values are equal.
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
  dependencies: [{environment: E, parameters: {P: "{{Param.Root}}{{ '' }}", L: ['C:/data\proj']}}]
  script: {actions: {onRun: {command: x}}}
"#;
    let job = create_ok(src, &[]);
    for step in &job.steps {
        assert_eq!(
            rendered_values(step),
            [r#"JobTemplate/E: L=list[path]:["C:/data\\proj"], P=path:C:/data\proj"#]
        );
    }
    assert!(job.steps[0].environment_values_equal(&job.steps[1]));
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
fn the_script_and_the_entries_read_the_same_let_values() {
    // The Step's `let` bindings are evaluated once, with POSIX semantics,
    // and the script and the entries read the same values: `D:\out` is one
    // POSIX path name, so `k` evaluates, and `m.parent / 'x'` joins with
    // `/`.
    let job = create_ok(
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
        &[],
    );
    assert_eq!(
        rendered_values(&job.steps[0]),
        [r"JobTemplate/E: P=path:D:\out/x"]
    );
}

#[test]
fn allowed_values_see_the_value_that_is_stored() {
    // §4.5 item 2: every effective value satisfies its constraints. The
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
    let ctx = template_ctx(&jt);
    let err = create_job(&jt, &job_params(&jt, &[]), &ctx).unwrap_err();
    // `{{Param.D}} ` is the string 'C:/a/b ', which is not allowed.
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &["steps[1] -> dependencies[0] -> parameters -> P:\n\tParameter 'P': value 'C:/a/b ' is not in allowed values"],
    );
    // The single expression `{{ Param.D }}` keeps the string as written,
    // 'C:/a/b', as the literal does.
    let jt = job_ok(&src.replace("{P: '{{Param.D}} '}", "{P: '{{Param.D}}'}"));
    let job = create_job(&jt, &job_params(&jt, &[]), &ctx).unwrap();
    let values: Vec<_> = job.steps.iter().map(rendered_values).collect();
    assert_eq!(
        values,
        [
            ["JobTemplate/E: P=path:C:/a/b"],
            ["JobTemplate/E: P=path:C:/a/b"],
            ["JobTemplate/E: P=path:C:/a/b"]
        ]
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
fn a_relative_path_value_is_checked_only_against_its_constraints() {
    // §4.5 item 3 (spec af53020): no form check, so only `allowedValues`
    // rejects it, against the unmapped value.
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
        &["steps[0] -> dependencies[0] -> parameters -> P:\n\tParameter 'P': value 'rel' is not in allowed values"],
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
    // The duplicates are §7.4.1 step 2 errors; the first Ext entry is still
    // resolved, in step 3, and names no Environment.
    let err = create_job(&jt, &params, &template_ctx(&jt)).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Model validation error: 3 validation errors for JobTemplate\n\
         steps[0] -> dependencies[2] -> environment:\n\tduplicate environment dependency 'E'.\n\
         steps[0] -> dependencies[3] -> environment:\n\tduplicate environment dependency 'Ext'.\n\
         steps[0] -> dependencies[1] -> environment:\n\tnames Environment 'Ext', which is not a \
         jobEnvironments entry of the Job Template, and no Environment Template is attached. The \
         entry is required; set 'required: false' if the Step can run without the Environment \
         (Template Schemas §3.2.1 item 2)."
    );
}
