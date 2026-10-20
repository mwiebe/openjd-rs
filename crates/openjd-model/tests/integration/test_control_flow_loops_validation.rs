// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `CONTROL_FLOW` multi-Step loops (RFC 0013): the `<StepLoop>` schema and
//! every rule of RFC 0013 §Validation, "At template validation".
//!
//! Failure tests assert the full error output (count, path, and message), as
//! `AGENTS.md` § Test Quality Standard requires; expression failures include
//! the expression and the caret. They are grouped by the RFC 0013
//! §Validation item they check.

use openjd_model::template::{BoolOrFormatString, IntOrFormatString, StepEntry};
use openjd_model::types::OutputCollectMode;
use openjd_model::{
    create_job, decode_job_template, preprocess_job_parameters, CallerLimits, JobParameterValues,
    PathParameterOptions,
};

const SUPPORTED: &[&str] = &["EXPR", "CONTROL_FLOW", "TASK_CHUNKING", "FEATURE_BUNDLE_1"];

fn decode_with(
    template: &str,
    limits: &CallerLimits,
) -> Result<openjd_model::template::JobTemplate, openjd_model::ModelError> {
    decode_job_template(
        serde_saphyr::from_str(template).unwrap(),
        Some(SUPPORTED),
        limits,
    )
}

fn decode(template: &str) -> Result<openjd_model::template::JobTemplate, openjd_model::ModelError> {
    decode_with(template, &CallerLimits::default())
}

fn decode_ok(template: &str) -> openjd_model::template::JobTemplate {
    match decode(template) {
        Ok(jt) => jt,
        Err(e) => panic!("expected the template to decode, got:\n{e}\n\n{template}"),
    }
}

fn expected(errors: &[(&str, &str)]) -> String {
    let n = errors.len();
    let word = if n == 1 { "error" } else { "errors" };
    let mut out = format!("Model validation error: {n} validation {word} for JobTemplate");
    for (path, msg) in errors {
        out.push_str(&format!("\n{path}:\n\t{msg}"));
    }
    out
}

/// Assert the complete validation error output.
#[track_caller]
fn assert_errs(template: &str, errors: &[(&str, &str)]) {
    let err = decode(template).expect_err("expected a validation error");
    assert_eq!(err.to_string(), expected(errors), "\n{template}");
}

/// Assert a decode (serde) failure's complete message.
#[track_caller]
fn assert_decode_err(template: &str, message: &str) {
    let err = decode(template).expect_err("expected a decode error");
    assert_eq!(
        err.to_string(),
        format!("Validation error: 'jobtemplate-2023-09' failed checks: {message}"),
        "\n{template}"
    );
}

const SCRIPT: &str = "script: {actions: {onRun: {command: x}}}";

/// A Job Template with `extensions` and parameters `Run: BOOL`, `Num: INT`,
/// `Name: STRING`, and `Root: PATH`, whose `steps` are `steps` (block YAML
/// at the top level, so each entry starts with `- `).
fn template_with(extensions: &str, job_fields: &str, steps: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09
extensions: {extensions}
name: J
parameterDefinitions:
  - {{name: Run, type: BOOL, default: true}}
  - {{name: Num, type: INT, default: 3}}
  - {{name: Name, type: STRING, default: a}}
  - {{name: Root, type: PATH, default: root}}
{job_fields}steps:
{steps}"
    )
}

fn template(steps: &str) -> String {
    template_with("[EXPR, CONTROL_FLOW]", "", steps)
}

/// A template whose only entry is loop `L` with `loop_fields` (indented four
/// spaces under `loop:`), `fields` (other `<StepLoop>` fields, indented two
/// spaces), and a body of one Step `W` that reports output `X: INT`.
fn single_loop(loop_fields: &str, fields: &str) -> String {
    template(&format!(
        "- name: L
{fields}  loop:
{loop_fields}    steps:
      - name: W
        outputVariables: [{{name: X, type: INT}}]
        {SCRIPT}
"
    ))
}

const UNTIL: &str = "    until: \"{{Loop.Iteration >= 1}}\"\n    maxIterations: 5\n";

// ── Schema ────────────────────────────────────────────────────────

#[test]
fn a_loop_parses_into_its_template_types() {
    let jt = decode_ok(&template(&format!(
        "- name: Setup
  outputVariables: [{{name: Start, type: INT}}]
  {SCRIPT}
- name: L
  description: refine
  enableIf: \"{{{{Param.Run}}}}\"
  dependencies: [{{dependsOn: Setup}}]
  loop:
    variables:
      - {{name: Pass, type: INT, initial: \"{{{{Steps.Setup.Output.Start}}}}\", next: \"{{{{Loop.Pass + 1}}}}\", minValue: 1, description: pass}}
    until: \"{{{{Steps.Check.Output.Score >= 3}}}}\"
    maxIterations: \"{{{{Param.Num}}}}\"
    steps:
      - name: Render
        {SCRIPT}
      - name: Check
        dependencies: [{{dependsOn: Render}}]
        outputVariables: [{{name: Score, type: INT}}]
        {SCRIPT}
  outputVariables:
    - {{name: Final, type: INT, collect: LAST, value: \"{{{{Steps.Check.Output.Score}}}}\", unit: points}}
    - {{name: History, type: \"LIST[INT]\", collect: APPEND, value: \"{{{{Steps.Check.Output.Score}}}}\"}}
- name: Publish
  dependencies: [{{dependsOn: L}}]
  {SCRIPT}
"
    )));
    assert!(jt.has_loops());
    assert_eq!(jt.steps.len(), 3);
    assert!(matches!(jt.steps[0], StepEntry::Step(_)));
    let StepEntry::Loop(l) = &jt.steps[1] else {
        panic!("steps[1] is a loop");
    };
    assert_eq!(jt.steps[1].name(), "L");
    assert_eq!(jt.steps[1].dependencies()[0].depends_on, "Setup");
    assert!(jt.steps[1].enable_if().is_some());
    assert!(jt.steps[1].as_step().is_none());
    assert_eq!(l.description.as_ref().unwrap().0, "refine");
    assert!(matches!(
        l.enable_if,
        Some(BoolOrFormatString::FormatString(_))
    ));
    let def = &l.loop_definition;
    assert!(def.while_condition.is_none());
    assert!(def.until_condition.is_some());
    assert!(matches!(
        def.max_iterations,
        IntOrFormatString::FormatString(_)
    ));

    let var = &l.variables()[0];
    assert_eq!(var.name(), "Pass");
    assert_eq!(var.job_param_type().as_spec_str(), "INT");
    assert_eq!(var.initial().raw(), "{{Steps.Setup.Output.Start}}");
    assert_eq!(var.next().raw(), "{{Loop.Pass + 1}}");
    assert_eq!(var.description(), Some("pass"));
    assert!(!var.constraints().is_empty());
    assert_eq!(var.to_output_variable().name, "Pass");

    let outputs = l.output_variables_list();
    assert_eq!(outputs[0].name(), "Final");
    assert_eq!(outputs[0].collect(), Some(OutputCollectMode::Last));
    assert_eq!(outputs[0].unit(), Some("points"));
    assert_eq!(outputs[0].value().raw(), "{{Steps.Check.Output.Score}}");
    assert_eq!(outputs[1].collect(), Some(OutputCollectMode::Append));
    assert_eq!(outputs[1].job_param_type().as_spec_str(), "LIST[INT]");
    assert_eq!(
        outputs[1].to_output_variable().collect,
        OutputCollectMode::Append
    );

    let body: Vec<&str> = l.body().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(body, ["Render", "Check"]);

    // Every StepTemplate, with its location and path.
    let all: Vec<(String, String, bool)> = jt
        .step_templates()
        .map(|st| {
            let path = st
                .path()
                .iter()
                .map(|e| match e {
                    openjd_model::PathElement::Field(f) => f.clone(),
                    openjd_model::PathElement::Index(i) => format!("[{i}]"),
                })
                .collect::<Vec<_>>()
                .join(".");
            (st.step.name.clone(), path, st.step_loop.is_some())
        })
        .collect();
    assert_eq!(
        all,
        [
            ("Setup".into(), "steps.[0]".into(), false),
            ("Render".into(), "steps.[1].loop.steps.[0]".into(), true),
            ("Check".into(), "steps.[1].loop.steps.[1]".into(), true),
            ("Publish".into(), "steps.[2]".into(), false),
        ]
    );
    let top: Vec<&str> = jt.top_level_steps().map(|s| s.name.as_str()).collect();
    assert_eq!(top, ["Setup", "Publish"]);
    assert_eq!(jt.loops().count(), 1);
}

#[test]
fn a_while_loop_with_a_literal_max_iterations_parses() {
    let jt = decode_ok(&single_loop(
        "    while: true\n    maxIterations: \"7\"\n",
        "",
    ));
    let l = jt.loops().next().unwrap();
    assert!(matches!(
        l.loop_definition.while_condition,
        Some(BoolOrFormatString::Bool(true))
    ));
    assert_eq!(l.loop_definition.max_iterations.as_i64(), Some(7));
    assert!(l.variables().is_empty());
    assert!(l.output_variables_list().is_empty());
}

#[test]
fn a_template_without_loops_has_none() {
    let jt = decode_ok(&template(&format!("- name: S\n  {SCRIPT}\n")));
    assert!(!jt.has_loops());
    assert_eq!(jt.loops().count(), 0);
    assert_eq!(jt.top_level_steps().count(), 1);
}

#[test]
fn a_loop_without_control_flow_is_an_error_at_its_loop_field() {
    assert_errs(
        &template_with(
            "[EXPR]",
            "",
            &format!(
                "- name: L
  loop:
    until: true
    maxIterations: 2
    steps:
      - name: W
        {SCRIPT}
"
            ),
        ),
        &[(
            "steps[0] -> loop",
            "loop requires the CONTROL_FLOW extension (RFC 0013).",
        )],
    );
}

#[test]
fn a_loop_has_no_step_fields() {
    for (field, value) in [
        ("script", "{actions: {onRun: {command: x}}}"),
        ("parameterSpace", "{taskParameterDefinitions: []}"),
        ("hostRequirements", "{}"),
        ("stepEnvironments", "[]"),
        ("let", "[]"),
    ] {
        assert_decode_err(
            &single_loop(UNTIL, &format!("  {field}: {value}\n")),
            &format!("steps entry 'L' has a 'loop' field, so it is a loop (CONTROL_FLOW, RFC 0013), which has no such field: unknown field `{field}`, expected one of `name`, `description`, `enableIf`, `dependencies`, `loop`, `outputVariables`"),
        );
    }
}

#[test]
fn a_loop_definition_has_only_its_own_fields() {
    assert_decode_err(
        &single_loop(&format!("{UNTIL}    script: x\n"), ""),
        "loop 'L': unknown field `script`, expected one of `variables`, `while`, `until`, `maxIterations`, `steps`",
    );
}

#[test]
fn loops_do_not_nest() {
    assert_decode_err(
        &template(&format!(
            "- name: Outer
  loop:
    until: true
    maxIterations: 2
    steps:
      - name: Inner
        loop:
          until: true
          maxIterations: 2
          steps:
            - name: W
              {SCRIPT}
"
        )),
        "loop 'Outer': loop body entry 0 'Inner' has a 'loop' field, but loops do not nest: a loop's body may contain only Steps (RFC 0013).",
    );
}

#[test]
fn max_iterations_is_required() {
    assert_decode_err(
        &single_loop("    until: true\n", ""),
        "loop 'L': missing field `maxIterations`",
    );
}

#[test]
fn a_loop_variable_requires_initial_and_next() {
    assert_decode_err(
        &single_loop(
            &format!("    variables: [{{name: V, type: INT, next: \"1\"}}]\n{UNTIL}"),
            "",
        ),
        "loop 'L': missing 'initial' field in loop variable definition",
    );
    assert_decode_err(
        &single_loop(
            &format!("    variables: [{{name: V, type: INT, initial: \"1\"}}]\n{UNTIL}"),
            "",
        ),
        "loop 'L': missing 'next' field in loop variable definition",
    );
}

#[test]
fn a_loop_output_requires_value() {
    assert_decode_err(
        &single_loop(
            UNTIL,
            "  outputVariables: [{name: O, type: INT, collect: LAST}]\n",
        ),
        "loop 'L': missing 'value' field in loop output definition",
    );
}

#[test]
fn an_unknown_loop_variable_field_lists_the_loop_variable_fields() {
    assert_decode_err(
        &single_loop(
            &format!(
                "    variables: [{{name: V, type: INT, initial: \"1\", next: \"1\", bogus: 1}}]\n{UNTIL}"
            ),
            "",
        ),
        "loop 'L': unknown field `bogus`, expected one of `name`, `description`, `allowedValues`, `minValue`, `maxValue`, `initial`, `next`",
    );
}

// ── Item 1: properties of <StepLoop> and <LoopDefinition> ─────────

#[test]
fn exactly_one_of_while_and_until() {
    assert_errs(
        &single_loop(
            "    while: true\n    until: true\n    maxIterations: 2\n",
            "",
        ),
        &[(
            "steps[0] -> loop",
            "must have exactly one of 'while' and 'until', not both.",
        )],
    );
    assert_errs(
        &single_loop("    maxIterations: 2\n", ""),
        &[(
            "steps[0] -> loop",
            "must have exactly one of 'while' and 'until'.",
        )],
    );
}

// ── Item 2: the body ──────────────────────────────────────────────

#[test]
fn a_body_is_not_empty() {
    assert_errs(
        &template("- name: L\n  loop:\n    until: true\n    maxIterations: 2\n    steps: []\n"),
        &[(
            "steps[0] -> loop -> steps",
            "must contain at least one element.",
        )],
    );
}

#[test]
fn body_steps_get_every_step_check_at_their_own_path() {
    // No script; a SimpleAction without FEATURE_BUNDLE_1 is checked too.
    assert_errs(
        &template_with(
            "[EXPR, CONTROL_FLOW]",
            "",
            "- name: L
  loop:
    until: true
    maxIterations: 2
    steps:
      - name: A
      - name: B
        bash: {script: echo}
",
        ),
        &[
            (
                "steps[0] -> loop -> steps[0]",
                "must have 'script' or a simple action field.",
            ),
            (
                "steps[0] -> loop -> steps[1] -> bash",
                "'bash' requires the FEATURE_BUNDLE_1 extension.",
            ),
        ],
    );
}

#[test]
fn a_body_step_chunk_parameter_needs_task_chunking() {
    assert_errs(
        &single_loop(UNTIL, "").replace(
            "        outputVariables: [{name: X, type: INT}]\n",
            "        parameterSpace:\n          taskParameterDefinitions:\n            - {name: F, type: \"CHUNK[INT]\", range: \"1-3\", chunks: {defaultTaskCount: 1, rangeConstraint: CONTIGUOUS}}\n",
        ),
        &[(
            "steps[0] -> loop -> steps[0] -> parameterSpace -> taskParameterDefinitions[0]",
            "task parameter type 'CHUNK[INT]' is not allowed.",
        ), (
            "steps[0] -> loop -> steps[0] -> parameterSpace -> taskParameterDefinitions[0]",
            "CHUNK[INT] requires the TASK_CHUNKING extension.",
        )],
    );
}

// ── Item 3: names are distinct across the whole template ──────────

#[test]
fn a_body_step_may_not_share_a_top_level_name() {
    assert_errs(
        &template(&format!(
            "- name: W
  {SCRIPT}
- name: L
  loop:
    until: true
    maxIterations: 2
    steps:
      - name: W
        {SCRIPT}
"
        )),
        &[(
            "steps[1] -> loop -> steps[0] -> name",
            "duplicate step name: 'W'",
        )],
    );
}

#[test]
fn body_steps_of_two_loops_may_not_share_a_name() {
    let one = |name: &str| {
        format!(
            "- name: {name}
  loop:
    until: true
    maxIterations: 2
    steps:
      - name: W
        {SCRIPT}
"
        )
    };
    assert_errs(
        &template(&(one("A") + &one("B"))),
        &[(
            "steps[1] -> loop -> steps[0] -> name",
            "duplicate step name: 'W'",
        )],
    );
}

#[test]
fn a_loop_may_not_share_its_name_with_its_body_step() {
    assert_errs(
        &single_loop(UNTIL, "").replace("      - name: W", "      - name: L"),
        &[(
            "steps[0] -> loop -> steps[0] -> name",
            "duplicate step name: 'L'",
        )],
    );
}

#[test]
fn a_loop_name_gets_the_step_name_checks() {
    assert_errs(
        &single_loop(UNTIL, "").replace("- name: L", "- name: \"{{L}}\""),
        &[(
            "steps[0] -> name",
            "must not contain format string expressions.",
        )],
    );
    assert_errs(
        &single_loop(UNTIL, "").replace("- name: L", &format!("- name: {}", "x".repeat(65))),
        &[("steps[0] -> name", "exceeds 64 characters.")],
    );
}

// ── Item 4: dependencies ──────────────────────────────────────────

#[test]
fn a_top_level_step_may_not_depend_on_a_body_step() {
    assert_errs(
        &template(&format!(
            "- name: L
  loop:
    until: true
    maxIterations: 2
    steps:
      - name: W
        {SCRIPT}
- name: C
  dependencies: [{{dependsOn: W}}]
  {SCRIPT}
"
        )),
        &[(
            "steps[1] -> dependencies[0]",
            "dependency 'W' is a body Step of loop 'L'; a top-level Step or loop may depend only on top-level Steps and loops. Depend on loop 'L' and read its outputs instead.",
        )],
    );
}

#[test]
fn a_body_step_may_depend_only_on_its_own_body() {
    let tpl = |dep: &str| {
        template(&format!(
            "- name: Setup
  {SCRIPT}
- name: M
  loop:
    until: true
    maxIterations: 2
    steps:
      - name: Other
        {SCRIPT}
- name: L
  loop:
    until: true
    maxIterations: 2
    steps:
      - name: W
        dependencies: [{{dependsOn: {dep}}}]
        {SCRIPT}
"
        ))
    };
    let path = "steps[2] -> loop -> steps[0] -> dependencies[0]";
    assert_errs(
        &tpl("Setup"),
        &[(
            path,
            "dependency 'Setup' is not a Step of loop 'L's body; a body Step may depend only on Steps of the same body. Add 'dependsOn: Setup' to loop 'L' instead: every body Step reads the Steps its loop depends on.",
        )],
    );
    assert_errs(
        &tpl("L"),
        &[(path, "a body Step cannot depend on its own loop 'L'.")],
    );
    assert_errs(
        &tpl("Other"),
        &[(
            path,
            "dependency 'Other' is a body Step of loop 'M'; a body Step may depend only on Steps of the same body.",
        )],
    );
    assert_errs(&tpl("Nope"), &[(path, "dependency 'Nope' not found.")]);
    assert_errs(
        &tpl("W"),
        &[
            (path, "cannot depend on itself."),
            (
                "steps[2] -> loop -> steps",
                "the loop body's step dependencies contain a cycle.",
            ),
        ],
    );
}

/// [`assert_errs`] with a last error at the template root, which has no
/// path.
#[track_caller]
fn assert_errs_and_root(template: &str, errors: &[(&str, &str)], root: Option<&str>) {
    let err = decode(template).expect_err("expected a validation error");
    let n = errors.len() + usize::from(root.is_some());
    let word = if n == 1 { "error" } else { "errors" };
    let mut want = format!("Model validation error: {n} validation {word} for JobTemplate");
    for (path, msg) in errors {
        want.push_str(&format!("\n{path}:\n\t{msg}"));
    }
    if let Some(root) = root {
        want.push_str(&format!("\nJobTemplate: {root}"));
    }
    assert_eq!(err.to_string(), want, "\n{template}");
}

#[test]
fn a_loop_dependency_is_checked_as_a_steps() {
    assert_errs_and_root(
        &single_loop(
            UNTIL,
            "  dependencies: [{dependsOn: L}, {dependsOn: Nope}]\n",
        ),
        &[
            ("steps[0] -> dependencies[0]", "cannot depend on itself."),
            (
                "steps[0] -> dependencies[1]",
                "dependency 'Nope' not found.",
            ),
        ],
        Some("step dependencies contain a cycle."),
    );
}

#[test]
fn a_body_dependency_cycle_is_reported_at_the_body() {
    assert_errs(
        &template(&format!(
            "- name: L
  loop:
    until: true
    maxIterations: 2
    steps:
      - name: A
        dependencies: [{{dependsOn: B}}]
        {SCRIPT}
      - name: B
        dependencies: [{{dependsOn: A}}]
        {SCRIPT}
"
        )),
        &[(
            "steps[0] -> loop -> steps",
            "the loop body's step dependencies contain a cycle.",
        )],
    );
}

#[test]
fn a_top_level_cycle_through_a_loop_is_reported() {
    assert_errs_and_root(
        &template(&format!(
            "- name: L
  dependencies: [{{dependsOn: C}}]
  loop:
    until: true
    maxIterations: 2
    steps:
      - name: W
        {SCRIPT}
- name: C
  dependencies: [{{dependsOn: L}}]
  {SCRIPT}
"
        )),
        &[],
        Some("step dependencies contain a cycle."),
    );
}

// ── Item 5: variables ─────────────────────────────────────────────

fn var(name: &str) -> String {
    format!("{{name: {name}, type: INT, initial: \"1\", next: \"1\"}}")
}

#[test]
fn variables_have_one_to_thirty_two_unique_names() {
    assert_errs(
        &single_loop(&format!("    variables: []\n{UNTIL}"), ""),
        &[(
            "steps[0] -> loop -> variables",
            "if provided, must contain at least one element.",
        )],
    );
    let many: Vec<String> = (0..33).map(|i| var(&format!("V{i}"))).collect();
    assert_errs(
        &single_loop(
            &format!("    variables: [{}]\n{UNTIL}", many.join(", ")),
            "",
        ),
        &[(
            "steps[0] -> loop -> variables",
            "must not contain more than 32 elements.",
        )],
    );
    assert_errs(
        &single_loop(
            &format!("    variables: [{}, {}]\n{UNTIL}", var("V"), var("V")),
            "",
        ),
        &[(
            "steps[0] -> loop -> variables[1]",
            "duplicate loop variable name: 'V'",
        )],
    );
}

#[test]
fn iteration_is_reserved() {
    assert_errs(
        &single_loop(
            &format!("    variables: [{}]\n{UNTIL}", var("Iteration")),
            "",
        ),
        &[(
            "steps[0] -> loop -> variables[0] -> name",
            "'Iteration' is reserved: Loop.Iteration is the index of the current iteration.",
        )],
    );
}

#[test]
fn a_loop_variable_has_no_collect_unit_default_or_user_interface() {
    assert_errs(
        &single_loop(
            &format!(
                "    variables: [{{name: V, type: INT, initial: \"1\", next: \"1\", collect: LAST, unit: s, default: 1, userInterface: {{control: SPIN_BOX}}}}]\n{UNTIL}"
            ),
            "",
        ),
        &[
            (
                "steps[0] -> loop -> variables[0] -> collect",
                "is not allowed in a loop variable definition.",
            ),
            (
                "steps[0] -> loop -> variables[0] -> unit",
                "is not allowed in a loop variable definition.",
            ),
            (
                "steps[0] -> loop -> variables[0] -> default",
                "is not allowed in a loop variable definition.",
            ),
            (
                "steps[0] -> loop -> variables[0] -> userInterface",
                "is not allowed in a loop variable definition.",
            ),
        ],
    );
}

#[test]
fn loop_variable_constraints_must_be_valid_for_the_type() {
    assert_errs(
        &single_loop(
            &format!(
                "    variables: [{{name: V, type: INT, initial: \"5\", next: \"5\", minValue: 9, maxValue: 1}}]\n{UNTIL}"
            ),
            "",
        ),
        &[(
            "steps[0] -> loop -> variables[0]",
            "Loop variable 'V': minValue (9) > maxValue (1).",
        )],
    );
}

#[test]
fn a_loop_variable_may_be_list_list_int() {
    decode_ok(&single_loop(
        &format!("    variables: [{{name: G, type: \"LIST[LIST[INT]]\", initial: \"{{{{[[1]]}}}}\", next: \"{{{{Loop.G + [[2]]}}}}\"}}]\n{UNTIL}"),
        "",
    ));
}

// ── Item 6: loop outputs ──────────────────────────────────────────

fn output(name: &str) -> String {
    format!("{{name: {name}, type: INT, collect: LAST, value: \"1\"}}")
}

#[test]
fn loop_outputs_have_one_to_thirty_two_unique_names() {
    assert_errs(
        &single_loop(UNTIL, "  outputVariables: []\n"),
        &[(
            "steps[0] -> outputVariables",
            "if provided, must contain at least one element.",
        )],
    );
    let many: Vec<String> = (0..33).map(|i| output(&format!("O{i}"))).collect();
    assert_errs(
        &single_loop(
            UNTIL,
            &format!("  outputVariables: [{}]\n", many.join(", ")),
        ),
        &[(
            "steps[0] -> outputVariables",
            "must not contain more than 32 elements.",
        )],
    );
    assert_errs(
        &single_loop(
            UNTIL,
            &format!("  outputVariables: [{}, {}]\n", output("O"), output("O")),
        ),
        &[(
            "steps[0] -> outputVariables[1]",
            "duplicate loop output name: 'O'",
        )],
    );
}

#[test]
fn a_loop_output_collect_is_last_or_append() {
    let with = |collect: &str, ty: &str| {
        single_loop(
            UNTIL,
            &format!("  outputVariables: [{{name: O, type: \"{ty}\", {collect}value: \"{{{{Loop.Iteration}}}}\"}}]\n"),
        )
    };
    assert_errs(
        &with("", "INT"),
        &[(
            "steps[0] -> outputVariables[0] -> collect",
            "is required in a loop output definition: LAST or APPEND.",
        )],
    );
    assert_errs(
        &with("collect: ONCE, ", "INT"),
        &[(
            "steps[0] -> outputVariables[0] -> collect",
            "ONCE is not allowed for a loop output, whose value is evaluated at every iteration boundary; use LAST or APPEND.",
        )],
    );
    assert_errs(
        &with("collect: APPEND, ", "INT"),
        &[(
            "steps[0] -> outputVariables[0] -> collect",
            "APPEND requires a LIST[...] type; 'INT' is not a list type.",
        )],
    );
    decode_ok(&with("collect: APPEND, ", "LIST[INT]"));
    decode_ok(&with("collect: LAST, ", "INT"));
}

#[test]
fn a_loop_output_unit_and_unavailable_properties_are_checked() {
    assert_errs(
        &single_loop(
            UNTIL,
            "  outputVariables: [{name: O, type: INT, collect: LAST, value: \"1\", unit: \"\", default: 1}]\n",
        ),
        &[
            (
                "steps[0] -> outputVariables[0] -> default",
                "is not allowed in a loop output definition.",
            ),
            (
                "steps[0] -> outputVariables[0] -> unit",
                "must not be empty.",
            ),
        ],
    );
}

#[test]
fn a_loop_output_may_be_list_list_int() {
    decode_ok(&single_loop(
        UNTIL,
        "  outputVariables: [{name: O, type: \"LIST[LIST[INT]]\", collect: APPEND, value: \"{{[Loop.Iteration]}}\"}]\n",
    ));
}

// ── Item 7: maxIterations ─────────────────────────────────────────

#[test]
fn a_literal_max_iterations_is_from_1_to_1000() {
    decode_ok(&single_loop("    until: true\n    maxIterations: 1\n", ""));
    decode_ok(&single_loop(
        "    until: true\n    maxIterations: 1000\n",
        "",
    ));
    for n in [0, 1001, -1] {
        assert_errs(
            &single_loop(&format!("    until: true\n    maxIterations: {n}\n"), ""),
            &[(
                "steps[0] -> loop -> maxIterations",
                &format!("must be from 1 to 1000; got {n}."),
            )],
        );
    }
}

#[test]
fn a_computed_max_iterations_is_an_int_from_the_job_parameters() {
    decode_ok(&single_loop(
        "    until: true\n    maxIterations: \"{{Param.Num * 2}}\"\n",
        "",
    ));
    decode_ok(&single_loop(
        "    until: true\n    maxIterations: \"{{len(Job.Name) + len(RawParam.Root)}}\"\n",
        "",
    ));
    // A static value out of range.
    assert_errs(
        &single_loop("    until: true\n    maxIterations: \"{{2000}}\"\n", ""),
        &[(
            "steps[0] -> loop -> maxIterations",
            "maxIterations must be from 1 to 1000.",
        )],
    );
    // Not an int.
    assert_errs(
        &single_loop("    until: true\n    maxIterations: \"{{[Param.Num]}}\"\n", ""),
        &[(
            "steps[0] -> loop -> maxIterations",
            "Failed to parse interpolation expression at [0, 15]. Cannot coerce list[int] to int\n  [Param.Num]\n  ^~~~~~~~~~~",
        )],
    );
    // A PATH parameter is not available, only RawParam.
    assert_errs(
        &single_loop("    until: true\n    maxIterations: \"{{len(Param.Root)}}\"\n", ""),
        &[(
            "steps[0] -> loop -> maxIterations",
            "Failed to parse interpolation expression at [0, 19]. Undefined variable: 'Param.Root'. Did you mean one of: Param.Run, RawParam.Root\n  len(Param.Root)\n      ~~~~~~^~~~",
        )],
    );
}

#[test]
fn max_iterations_reads_no_steps_or_loop_symbols() {
    assert_errs(
        &template(&format!(
            "- name: Setup
  outputVariables: [{{name: K, type: INT}}]
  {SCRIPT}
- name: L
  dependencies: [{{dependsOn: Setup}}]
  loop:
    until: true
    maxIterations: \"{{{{Steps.Setup.Output.K}}}}\"
    steps:
      - name: W
        {SCRIPT}
"
        )),
        &[(
            "steps[1] -> loop -> maxIterations",
            "'Steps.Setup.Output.K': Steps.* symbols are not available in a loop's maxIterations, which is known at Job creation: it may read only Param.*, RawParam.*, and Job.Name.",
        )],
    );
    assert_errs(
        &single_loop("    until: true\n    maxIterations: \"{{Loop.Iteration}}\"\n", ""),
        &[(
            "steps[0] -> loop -> maxIterations",
            "'Loop.Iteration': Loop.* and RawLoop.* symbols are not available in a loop's maxIterations, which is known at Job creation: it may read only Param.*, RawParam.*, and Job.Name.",
        )],
    );
}

// ── Item 8: the Loop.* scope ──────────────────────────────────────

#[test]
fn loop_symbols_are_available_in_body_steps_and_the_loops_own_conditions() {
    decode_ok(&template_with(
        "[EXPR, CONTROL_FLOW, FEATURE_BUNDLE_1]",
        "",
        "- name: L
  loop:
    variables:
      - {name: V, type: INT, initial: \"0\", next: \"{{Loop.V + Loop.Iteration}}\"}
      - {name: P, type: PATH, initial: \"{{RawParam.Root}}\", next: \"{{RawLoop.P}}\"}
    while: \"{{Loop.V < 10 and Loop.Iteration < 3 and RawLoop.V >= 0}}\"
    maxIterations: 5
    steps:
      - name: W
        enableIf: \"{{Loop.Iteration > 0}}\"
        let: [\"v = Loop.V\", \"p = RawLoop.P\"]
        hostRequirements:
          amounts: [{name: amount.worker.vcpu, min: \"{{Loop.V}}\"}]
        parameterSpace:
          taskParameterDefinitions:
            - {name: I, type: INT, range: \"{{range(Loop.V + 1)}}\"}
        stepEnvironments:
          - name: E
            variables: {A: \"{{Loop.P}}\"}
        script:
          actions:
            onRun: {command: x, args: [\"{{Loop.P}}\", \"{{repr_py(Loop.V)}}\", \"{{RawLoop.P}}\"]}
  outputVariables:
    - {name: O, type: PATH, collect: LAST, value: \"{{RawLoop.P}}\"}
",
    ));
}

#[test]
fn loop_symbols_are_not_available_outside_a_loop() {
    let outside = "Loop.* and RawLoop.* symbols are available only in the body Steps of a loop and in the loop's while, until, next, and output values.";
    assert_errs(
        &template(
            "- name: S
  script: {actions: {onRun: {command: x, args: [\"{{Loop.Iteration}}\"]}}}
",
        ),
        &[(
            "steps[0] -> script -> actions -> onRun -> args[0]",
            &format!("'Loop.Iteration': {outside}"),
        )],
    );
    assert_errs(
        &template_with(
            "[EXPR, CONTROL_FLOW]",
            "jobOutputDefinitions: [{name: O, type: INT, value: \"{{RawLoop.V}}\"}]\n",
            single_loop(UNTIL, "").split_once("steps:\n").unwrap().1,
        ),
        &[(
            "jobOutputDefinitions[0] -> value",
            &format!("'RawLoop.V': {outside}"),
        )],
    );
}

#[test]
fn loop_symbols_are_not_available_before_the_first_iteration() {
    assert_errs(
        &single_loop(
            &format!("    variables: [{{name: V, type: INT, initial: \"{{{{Loop.Iteration}}}}\", next: \"1\"}}]\n{UNTIL}"),
            "  enableIf: \"{{RawLoop.V > 0}}\"\n",
        ),
        &[
            (
                "steps[0] -> enableIf",
                "'RawLoop.V': Loop.* and RawLoop.* symbols are not available in a loop's enableIf, which is evaluated before the loop's first iteration.",
            ),
            (
                "steps[0] -> loop -> variables[0] -> initial",
                "'Loop.Iteration': Loop.* and RawLoop.* symbols are not available in a loop's initial, which is evaluated before the loop's first iteration.",
            ),
        ],
    );
}

#[test]
fn a_loop_symbol_names_a_variable_of_this_loop_or_iteration() {
    let two = template(&format!(
        "- name: A
  loop:
    variables: [{{name: Num, type: INT, initial: \"0\", next: \"1\"}}]
    until: true
    maxIterations: 2
    steps:
      - name: WA
        {SCRIPT}
- name: B
  loop:
    variables: [{{name: M, type: INT, initial: \"0\", next: \"1\"}}]
    until: \"{{{{Loop.Nope > 0}}}}\"
    maxIterations: 2
    steps:
      - name: WB
        script: {{actions: {{onRun: {{command: x, args: [\"{{{{Loop.Num}}}}\", \"{{{{RawLoop.Iteration}}}}\"]}}}}}}
"
    ));
    assert_errs(
        &two,
        &[
            (
                "steps[1] -> loop -> steps[0] -> script -> actions -> onRun -> args[0]",
                "'Loop.Num': loop 'B' has no variable 'Num'. 'Num' is a variable of loop 'A', which is available only in that loop.",
            ),
            (
                "steps[1] -> loop -> steps[0] -> script -> actions -> onRun -> args[1]",
                "'RawLoop.Iteration': Loop.Iteration has no RawLoop form; use Loop.Iteration.",
            ),
            (
                "steps[1] -> loop -> until",
                "'Loop.Nope': loop 'B' has no variable 'Nope'.",
            ),
        ],
    );
}

#[test]
fn a_path_loop_variable_is_read_through_raw_loop_in_template_scope() {
    let tpl = |next: &str, let_expr: &str| {
        single_loop(
            &format!("    variables: [{{name: P, type: PATH, initial: \"{{{{RawParam.Root}}}}\", next: \"{next}\"}}]\n{UNTIL}"),
            "",
        )
        .replace(
            "        outputVariables: [{name: X, type: INT}]\n",
            &format!("        let: [\"d = {let_expr}\"]\n"),
        )
    };
    decode_ok(&tpl("{{RawLoop.P}}", "RawLoop.P"));
    assert_errs(
        &tpl("{{Loop.P}}", "Loop.P"),
        &[
            (
                "steps[0] -> loop -> steps[0] -> let[0]",
                "Invalid expression in let binding 'd': 'Loop.P' has a path type, so it is available only in fields resolved on the worker host. Use RawLoop.P here.",
            ),
            (
                "steps[0] -> loop -> variables[0] -> next",
                "'Loop.P' has a path type, so it is available only in fields resolved on the worker host. Use RawLoop.P here.",
            ),
        ],
    );
}

#[test]
fn an_incomplete_loop_reference_is_explained() {
    assert_errs(
        &single_loop("    until: \"{{len(Loop) > 0}}\"\n    maxIterations: 2\n", ""),
        &[(
            "steps[0] -> loop -> until",
            "'Loop' is not a complete reference; expected Loop.<VarName>, RawLoop.<VarName>, or Loop.Iteration.",
        )],
    );
}

// ── Item 9: Steps.* in and around a loop ──────────────────────────

/// `Setup` (output `K: INT`), `Other` (output `Z: INT`), loop `L`
/// depending on `Setup` with body `A` (output `X: INT`) and `B` (depending
/// on `A`), and `C` after the loop. `l_fields` are the loop's extra fields
/// (two-space indented), `b_args` the args of `B`'s script, `c_args` those
/// of `C`'s.
fn around(loop_fields: &str, l_fields: &str, b_args: &str, c_args: &str) -> String {
    template(&format!(
        "- name: Setup
  outputVariables: [{{name: K, type: INT}}]
  {SCRIPT}
- name: Other
  outputVariables: [{{name: Z, type: INT}}]
  {SCRIPT}
- name: L
  dependencies: [{{dependsOn: Setup}}]
{l_fields}  loop:
{loop_fields}    maxIterations: 3
    steps:
      - name: A
        outputVariables: [{{name: X, type: INT}}]
        {SCRIPT}
      - name: B
        dependencies: [{{dependsOn: A}}]
        script: {{actions: {{onRun: {{command: x, args: [{b_args}]}}}}}}
  outputVariables:
    - {{name: Last, type: INT, collect: LAST, value: \"{{{{Steps.A.Output.X}}}}\"}}
- name: C
  dependencies: [{{dependsOn: L}}]
  script: {{actions: {{onRun: {{command: x, args: [{c_args}]}}}}}}
"
    ))
}

const U: &str = "    until: true\n";
const B_ARGS: &str = "\"{{Steps.A.Output.X + Steps.Setup.Output.K + Loop.Iteration}}\"";
const C_ARGS: &str = "\"{{Steps.L.Output.Last}}\"";

#[test]
fn the_scopes_around_a_loop_accept_what_rfc_0013_allows() {
    decode_ok(&around(
        "    variables: [{name: V, type: INT, initial: \"{{Steps.Setup.Output.K}}\", next: \"{{Steps.A.Output.X + Steps.Setup.Output.K}}\"}]\n    until: \"{{Steps.A.Output.X > Steps.Setup.Output.K}}\"\n",
        "  enableIf: \"{{Steps.Setup.Output.K > 0}}\"\n",
        B_ARGS,
        C_ARGS,
    ));
    decode_ok(&around(
        "    while: \"{{Steps.Setup.Output.K > Loop.Iteration}}\"\n",
        "",
        B_ARGS,
        "\"{{Steps.L.Output.Last or 0}}\"",
    ));
}

#[test]
fn a_body_step_reads_its_body_dependencies_and_its_loops_dependencies() {
    let path = "steps[2] -> loop -> steps[1] -> script -> actions -> onRun -> args[0]";
    assert_errs(
        &around(U, "", "\"{{Steps.Other.Output.Z}}\"", C_ARGS),
        &[(
            path,
            "'Steps.Other.Output.Z': Step 'Other' is not a dependency of loop 'L'. A body Step reads the Steps its loop depends on: add 'dependsOn: Other' to the dependencies of loop 'L' to read Steps.Other.*.",
        )],
    );
    assert_errs(
        &around(U, "", "\"{{Steps.L.Output.Last}}\"", C_ARGS),
        &[(
            path,
            "'Steps.L.Output.Last': Step 'B' is in the body of loop 'L', which cannot read the loop's own outputs: they are available only after the loop ends.",
        )],
    );
    assert_errs(
        &around(U, "", "\"{{Steps.B.Output.X}}\"", C_ARGS),
        &[(
            path,
            "'Steps.B.Output.X': a Step cannot read its own Steps.* symbols.",
        )],
    );
    // A reads nothing in the body: B is not its dependency.
    assert_errs(
        &around(U, "", B_ARGS, C_ARGS).replace(
            "      - name: A\n        outputVariables: [{name: X, type: INT}]\n        script: {actions: {onRun: {command: x}}}",
            "      - name: A\n        outputVariables: [{name: X, type: INT}]\n        script: {actions: {onRun: {command: x, args: [\"{{Steps.B.Output.X}}\"]}}}",
        ),
        &[(
            "steps[2] -> loop -> steps[0] -> script -> actions -> onRun -> args[0]",
            "'Steps.B.Output.X': Step 'B' is not a direct dependency of Step 'A'. Add 'dependsOn: B' to its dependencies to read Steps.B.*.",
        )],
    );
}

#[test]
fn a_loops_conditions_read_what_their_stage_allows() {
    assert_errs(
        &around(
            "    variables: [{name: V, type: INT, initial: \"{{Steps.A.Output.X}}\", next: \"{{Steps.Other.Output.Z}}\"}]\n    while: \"{{Steps.A.Output.X > 0}}\"\n",
            "  enableIf: \"{{Steps.A.Output.X > 0}}\"\n",
            B_ARGS,
            C_ARGS,
        ),
        &[
            (
                "steps[2] -> enableIf",
                "'Steps.A.Output.X': Step 'A' is a body Step of loop 'L', which the loop's enableIf cannot read: it is evaluated before an iteration runs. The loop's until, next, and output values may read its body Steps.",
            ),
            (
                "steps[2] -> loop -> variables[0] -> initial",
                "'Steps.A.Output.X': Step 'A' is a body Step of loop 'L', which the loop's initial cannot read: it is evaluated before an iteration runs. The loop's until, next, and output values may read its body Steps.",
            ),
            (
                "steps[2] -> loop -> variables[0] -> next",
                "'Steps.Other.Output.Z': Step 'Other' is not a direct dependency of loop 'L'. Add 'dependsOn: Other' to its dependencies to read Steps.Other.*.",
            ),
            (
                "steps[2] -> loop -> while",
                "'Steps.A.Output.X': Step 'A' is a body Step of loop 'L', which the loop's while cannot read: it is evaluated before an iteration runs. The loop's until, next, and output values may read its body Steps.",
            ),
        ],
    );
    assert_errs(
        &around("    until: \"{{Steps.L.Output.Last > 0}}\"\n", "", B_ARGS, C_ARGS),
        &[(
            "steps[2] -> loop -> until",
            "'Steps.L.Output.Last': a loop cannot read its own Steps.* symbols; they are available only after the loop ends.",
        )],
    );
}

#[test]
fn a_body_step_is_not_available_outside_its_loop() {
    let msg = "'Steps.A.Output.X': Step 'A' is a body Step of loop 'L', and is available only within that loop. Read a loop output instead, as Steps.L.Output.<Name>.";
    assert_errs(
        &around(U, "", B_ARGS, "\"{{Steps.A.Output.X}}\""),
        &[("steps[3] -> script -> actions -> onRun -> args[0]", msg)],
    );
    let with_output = around(U, "", B_ARGS, C_ARGS).replace(
        "steps:\n- name: Setup",
        "jobOutputDefinitions: [{name: O, type: INT, value: \"{{Steps.A.Output.X}}\"}]\nsteps:\n- name: Setup",
    );
    assert_errs(&with_output, &[("jobOutputDefinitions[0] -> value", msg)]);
}

#[test]
fn a_job_output_reads_a_loop_output() {
    decode_ok(&around(U, "", B_ARGS, C_ARGS).replace(
        "steps:\n- name: Setup",
        "jobOutputDefinitions: [{name: O, type: INT, value: \"{{Steps.L.Output.Last + 1}}\"}]\nsteps:\n- name: Setup",
    ));
}

#[test]
fn a_loop_has_declared_outputs_and_no_task_parameters() {
    assert_errs(
        &around(U, "", B_ARGS, "\"{{Steps.L.TaskParam.X}}\", \"{{Steps.L.Output.Nope}}\""),
        &[
            (
                "steps[3] -> script -> actions -> onRun -> args[0]",
                "'Steps.L.TaskParam.X': 'L' is a loop, which has no Task parameters. Declare a loop output and read Steps.L.Output.<Name> instead.",
            ),
            (
                "steps[3] -> script -> actions -> onRun -> args[1]",
                "'Steps.L.Output.Nope': loop 'L' has no output variable 'Nope'.",
            ),
        ],
    );
}

#[test]
fn a_step_after_a_loop_must_depend_on_it() {
    assert_errs(
        &around(U, "", B_ARGS, C_ARGS).replace("  dependencies: [{dependsOn: L}]\n", ""),
        &[(
            "steps[3] -> script -> actions -> onRun -> args[0]",
            "'Steps.L.Output.Last': Step 'L' is not a direct dependency of Step 'C'. Add 'dependsOn: L' to its dependencies to read Steps.L.*.",
        )],
    );
}

#[test]
fn a_path_loop_output_is_read_through_raw_output_in_template_scope() {
    let tpl = |c_let: &str| {
        template(&format!(
            "- name: L
  loop:
    until: true
    maxIterations: 2
    steps:
      - name: W
        {SCRIPT}
  outputVariables: [{{name: P, type: PATH, collect: LAST, value: \"{{{{RawParam.Root}}}}\"}}]
- name: C
  dependencies: [{{dependsOn: L}}]
  let: [\"p = {c_let}\"]
  {SCRIPT}
"
        ))
    };
    decode_ok(&tpl("Steps.L.RawOutput.P"));
    assert_errs(
        &tpl("Steps.L.Output.P"),
        &[(
            "steps[1] -> let[0]",
            "Invalid expression in let binding 'p': 'Steps.L.Output.P' has a path type, so it is available only in fields resolved on the worker host. Use Steps.L.RawOutput.P here.",
        )],
    );
}

// ── Item 10: types ────────────────────────────────────────────────

#[test]
fn while_and_until_are_bool_not_optional() {
    assert_errs(
        &single_loop(
            &format!("    variables: [{}]\n    until: \"{{{{Loop.V}}}}\"\n    maxIterations: 2\n", var("V")),
            "",
        ),
        &[(
            "steps[0] -> loop -> until",
            "Failed to parse interpolation expression at [0, 10]. Cannot coerce int to bool\n  Loop.V\n  ~~~~~^",
        )],
    );
    // A conditional body Step's output is `bool?`, which is accepted where
    // `bool` is required and checked when concrete (RFC 0011 Design Choice
    // 5); only a non-bool type is an error.
    decode_ok(&template(&format!(
        "- name: L
  loop:
    until: \"{{{{Steps.W.Output.B}}}}\"
    maxIterations: 2
    steps:
      - name: W
        enableIf: \"{{{{Loop.Iteration > 0}}}}\"
        outputVariables: [{{name: B, type: BOOL}}]
        {SCRIPT}
"
    )));
    assert_errs(
        &single_loop("    while: \"yes please\"\n    maxIterations: 2\n", ""),
        &[(
            "steps[0] -> loop -> while",
            "must resolve to a bool: Cannot convert 'yes please' to bool.",
        )],
    );
}

#[test]
fn initial_and_next_have_the_variables_type() {
    assert_errs(
        &single_loop(
            &format!("    variables: [{{name: V, type: INT, initial: \"abc\", next: \"{{{{[Loop.V]}}}}\"}}]\n{UNTIL}"),
            "",
        ),
        &[
            (
                "steps[0] -> loop -> variables[0] -> initial",
                "must convert to int: Cannot convert 'abc' to int: invalid digit found in string.",
            ),
            (
                "steps[0] -> loop -> variables[0] -> next",
                "Failed to parse interpolation expression at [0, 12]. Cannot coerce list[int] to int\n  [Loop.V]\n  ^~~~~~~~",
            ),
        ],
    );
    // `null` is an error, not an omitted value.
    assert_errs(
        &single_loop(
            &format!("    variables: [{{name: V, type: INT, initial: \"{{{{null}}}}\", next: \"1\"}}]\n{UNTIL}"),
            "",
        ),
        &[(
            "steps[0] -> loop -> variables[0] -> initial",
            "Failed to parse interpolation expression at [0, 8]. Cannot coerce nulltype to int\n  null\n  ^~~~",
        )],
    );
}

#[test]
fn a_loop_output_value_has_its_type_or_its_element_type() {
    let tpl = |ty: &str, collect: &str, value: &str| {
        single_loop(
            UNTIL,
            &format!("  outputVariables: [{{name: O, type: \"{ty}\", collect: {collect}, value: \"{value}\"}}]\n"),
        )
    };
    decode_ok(&tpl("LIST[INT]", "APPEND", "{{Steps.W.Output.X}}"));
    decode_ok(&tpl("LIST[INT]", "LAST", "{{[Steps.W.Output.X]}}"));
    assert_errs(
        &tpl("LIST[INT]", "APPEND", "{{[1]}}"),
        &[(
            "steps[0] -> outputVariables[0] -> value",
            "Failed to parse interpolation expression at [0, 7]. Cannot coerce list[int] to int\n  [1]\n  ^~~",
        )],
    );
    assert_errs(
        &tpl("INT", "LAST", "{{[Steps.W.Output.X]}}"),
        &[(
            "steps[0] -> outputVariables[0] -> value",
            "Failed to parse interpolation expression at [0, 22]. Cannot coerce list[int] to int\n  [Steps.W.Output.X]\n  ^~~~~~~~~~~~~~~~~~",
        )],
    );
}

/// A loop output read after the loop type-checks with the loop's types (the
/// exact `ExprType` of each row of RFC 0013's table is checked by the
/// `steps_scope` unit tests).
#[test]
fn loop_outputs_type_check_after_the_loop() {
    decode_ok(&template(&format!(
        "- name: L
  loop:
    until: true
    maxIterations: 2
    steps:
      - name: W
        {SCRIPT}
  outputVariables:
    - {{name: Last, type: INT, collect: LAST, value: \"1\"}}
    - {{name: All, type: \"LIST[INT]\", collect: APPEND, value: \"1\"}}
- name: C
  dependencies: [{{dependsOn: L}}]
  script: {{actions: {{onRun: {{command: x, args: [\"{{{{Steps.L.Output.Last + sum(Steps.L.Output.All)}}}}\"]}}}}}}
"
    )));
}

// ── Round 1 audit additions ───────────────────────────────────────

#[test]
fn a_body_step_error_names_its_loop_and_entry() {
    assert_decode_err(
        &single_loop(UNTIL, "").replace(
            "        outputVariables: [{name: X, type: INT}]\n",
            "        bogus: 1\n",
        ),
        "loop 'L': loop body entry 0 'W': unknown field `bogus`, expected one of `name`, `description`, `enableIf`, `let`, `dependencies`, `stepEnvironments`, `hostRequirements`, `parameterSpace`, `outputVariables`, `script`, `bash`, `python`, `cmd`, `powershell`, `node`",
    );
}

#[test]
fn without_control_flow_a_loop_with_a_script_names_the_loop_field() {
    assert_decode_err(
        &template_with(
            "[EXPR]",
            "",
            &format!(
                "- name: L
  {SCRIPT}
  loop: {{until: true, maxIterations: 2, steps: [{{name: W, {SCRIPT}}}]}}
"
            ),
        ),
        "steps entry 'L' has a 'loop' field, so it is a loop (CONTROL_FLOW, RFC 0013), which has no such field: unknown field `script`, expected one of `name`, `description`, `enableIf`, `dependencies`, `loop`, `outputVariables`",
    );
}

#[test]
fn a_computed_max_iterations_is_checked_at_both_bounds() {
    for n in [1, 1000] {
        decode_ok(&single_loop(
            &format!("    until: true\n    maxIterations: \"{{{{{n}}}}}\"\n"),
            "",
        ));
    }
    for n in [0, 1001] {
        assert_errs(
            &single_loop(
                &format!("    until: true\n    maxIterations: \"{{{{{n}}}}}\"\n"),
                "",
            ),
            &[(
                "steps[0] -> loop -> maxIterations",
                "maxIterations must be from 1 to 1000.",
            )],
        );
    }
}

/// Two loops, `A` (body `WA`, output `X`) and `B` (body `WB`, output `K`),
/// where `B` depends on `A`; `wb_args` are `WB`'s script args and `b_until`
/// is `B`'s `until`.
fn two_loops(wb_args: &str, b_until: &str) -> String {
    template(&format!(
        "- name: A
  loop:
    until: true
    maxIterations: 2
    steps:
      - name: WA
        outputVariables: [{{name: X, type: INT}}]
        {SCRIPT}
  outputVariables: [{{name: X, type: INT, collect: LAST, value: \"{{{{Steps.WA.Output.X}}}}\"}}]
- name: B
  dependencies: [{{dependsOn: A}}]
  loop:
    until: \"{b_until}\"
    maxIterations: 2
    steps:
      - name: WB
        outputVariables: [{{name: K, type: INT}}]
        script: {{actions: {{onRun: {{command: x, args: [{wb_args}]}}}}}}
"
    ))
}

#[test]
fn a_body_step_of_another_loop_is_not_available_in_a_loop() {
    // Through its loop's dependency on A, WB reads A's outputs.
    decode_ok(&two_loops(
        "\"{{Steps.A.Output.X}}\"",
        "{{Steps.WB.Output.K > 0}}",
    ));
    assert_errs(
        &two_loops("\"{{Steps.WA.Output.X}}\"", "{{Steps.WA.Output.X > 0}}"),
        &[
            (
                "steps[1] -> loop -> steps[0] -> script -> actions -> onRun -> args[0]",
                "'Steps.WA.Output.X': Step 'WA' is a body Step of loop 'A', and is available only within that loop. Read a loop output instead, as Steps.A.Output.<Name>.",
            ),
            (
                "steps[1] -> loop -> until",
                "'Steps.WA.Output.X': Step 'WA' is a body Step of loop 'A', and is available only within that loop. Read a loop output instead, as Steps.A.Output.<Name>.",
            ),
        ],
    );
}

#[test]
fn only_a_loops_enable_if_reads_step_name() {
    decode_ok(&single_loop(
        UNTIL,
        "  enableIf: \"{{Step.Name == 'L'}}\"\n",
    ));
    assert_errs(
        &single_loop("    while: \"{{Step.Name == 'L'}}\"\n    maxIterations: 2\n", ""),
        &[(
            "steps[0] -> loop -> while",
            "Failed to parse interpolation expression at [0, 20]. Undefined variable: 'Step.Name'. Did you mean: Job.Name\n  Step.Name == 'L'\n  ~~~~~^~~~",
        )],
    );
}

#[test]
fn two_loops_are_both_listed() {
    let jt = decode_ok(&two_loops("\"x\"", "true"));
    let names: Vec<&str> = jt.loops().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["A", "B"]);
}

/// A loop whose body Step `W` has `w_fields` (eight-space indented) and a
/// script, under `extensions`.
fn body_step(extensions: &str, w_fields: &str) -> String {
    template_with(
        extensions,
        "",
        &format!(
            "- name: L
  loop:
    until: true
    maxIterations: 2
    steps:
      - name: W
{w_fields}        {SCRIPT}
"
        ),
    )
}

#[test]
fn a_body_steps_output_variables_get_the_rfc_0012_checks() {
    assert_errs(
        &body_step(
            "[EXPR, CONTROL_FLOW]",
            "        outputVariables: [{name: K, type: INT}, {name: K, type: INT}]\n",
        ),
        &[(
            "steps[0] -> loop -> steps[0] -> outputVariables[1]",
            "duplicate output variable name: 'K'",
        )],
    );
}

#[test]
fn a_body_steps_host_requirements_and_environments_are_checked() {
    assert_errs(
        &body_step(
            "[EXPR, CONTROL_FLOW, FEATURE_BUNDLE_1]",
            "        hostRequirements:\n          amounts: [{name: amount.worker.vcpu, min: \"{{Param.Nope}}\"}]\n        stepEnvironments: []\n",
        ),
        &[
            (
                "steps[0] -> loop -> steps[0] -> stepEnvironments",
                "must not be empty.",
            ),
            (
                "steps[0] -> loop -> steps[0] -> hostRequirements -> amounts[0] -> min",
                "Failed to parse interpolation expression at [0, 14]. Undefined variable: 'Param.Nope'. Did you mean: Param.Name\n  Param.Nope\n  ~~~~~~^~~~",
            ),
        ],
    );
}

#[test]
fn a_body_steps_environments_get_the_per_environment_checks() {
    // WRAP_ACTIONS gating, the FEATURE_BUNDLE_1 endOfLine gate, and the
    // comprehension check (reported at the root, as for any Step).
    let template = body_step(
        "[EXPR, CONTROL_FLOW]",
        "        stepEnvironments:
          - name: E
            script:
              let: [\"x = 1\"]
              actions:
                onEnter: {command: x, args: [\"{{[x for x in [1]]}}\"]}
                onWrapTaskRun: {command: y}
              embeddedFiles: [{name: F, type: TEXT, data: d, endOfLine: LF}]
",
    );
    let err = decode(&template).expect_err("expected a validation error");
    assert_eq!(
        err.to_string(),
        "Model validation error: 3 validation errors for JobTemplate
steps[0] -> loop -> steps[0] -> stepEnvironments[0] -> script -> embeddedFiles[0] -> endOfLine:
\trequires the FEATURE_BUNDLE_1 extension.
JobTemplate: List comprehension variable 'x' shadows a let binding
steps[0] -> loop -> steps[0] -> stepEnvironments[0] -> script -> actions -> onWrapTaskRun:
\tonWrapTaskRun requires the WRAP_ACTIONS extension."
    );
}

#[test]
fn a_body_steps_environment_gets_the_structural_checks() {
    assert_errs(
        &body_step(
            "[EXPR, CONTROL_FLOW]",
            "        stepEnvironments: [{name: E}]\n",
        ),
        &[(
            "steps[0] -> loop -> steps[0] -> stepEnvironments[0]",
            "must have at least one of 'script' or 'variables'.",
        )],
    );
}

#[test]
fn a_body_step_name_gets_the_length_limit() {
    assert_errs(
        &single_loop(UNTIL, "").replace(
            "      - name: W",
            &format!("      - name: {}", "w".repeat(65)),
        ),
        &[(
            "steps[0] -> loop -> steps[0] -> name",
            "exceeds 64 characters.",
        )],
    );
}

#[test]
fn a_body_step_task_parameter_type_is_case_sensitive_without_expr() {
    assert_errs(
        &body_step(
            "[CONTROL_FLOW]",
            "        parameterSpace:\n          taskParameterDefinitions: [{name: I, type: int, range: [1]}]\n",
        ),
        &[(
            "steps[0] -> loop -> steps[0] -> parameterSpace -> taskParameterDefinitions[0]",
            "task parameter type 'int' is not recognized. Type names are case-sensitive without the EXPR extension; expected 'INT'.",
        )],
    );
}

// ── Limits and job creation ───────────────────────────────────────

#[test]
fn the_step_count_limit_counts_loops_and_body_steps() {
    // One loop with one body Step: two Steps.
    let tpl = single_loop(UNTIL, "");
    let limits = CallerLimits {
        max_step_count: Some(1),
        ..Default::default()
    };
    let err = decode_with(&tpl, &limits).expect_err("over the limit");
    assert_eq!(
        err.to_string(),
        expected(&[("steps", "exceeds caller limit of 1 steps (2 steps).")])
    );
    let limits = CallerLimits {
        max_step_count: Some(2),
        ..Default::default()
    };
    decode_with(&tpl, &limits).expect("at the limit");
}

#[test]
fn create_job_does_not_support_loops_yet() {
    let jt = decode_ok(&template(&format!(
        "- name: S
  {SCRIPT}
{}",
        single_loop(UNTIL, "").split_once("steps:\n").unwrap().1
    )));
    let ctx = jt.default_validation_context();
    let values: JobParameterValues = preprocess_job_parameters(
        &jt,
        &Default::default(),
        &[],
        &PathParameterOptions::new("/t", "/t"),
    )
    .unwrap();
    let err = create_job(&jt, &values, &ctx).expect_err("loops are not instantiated");
    assert_eq!(
        err.to_string(),
        "Compatibility error: create_job does not support loops (RFC 0013) yet: steps[1] is loop 'L'."
    );
}
