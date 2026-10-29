// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `ENV_PARAMS` Environment `hostRequirements` (Template Schemas §4 item 5)
//! and a Step's combined host requirements (§3.3.3): where the field is
//! permitted, the per-source §3.3 checks, the format-string scopes of
//! §4 item 5.1, resolution per entering Step at job creation, and the
//! §3.3.3 normalization and conflicts.

use openjd_model::job::{AmountRequirement, AttributeRequirement, HostRequirements, Job};
use openjd_model::template::{EnvironmentTemplate, JobTemplate};
use openjd_model::{
    check_environment_templates, create_job_with_environment_templates,
    AttachedEnvironmentTemplate, CreatedJob, ModelError,
};

use crate::env_params_common::{
    assert_errors, env_err, env_ok, job_err, job_err_with, job_ok, job_params_with, posix_ctx,
};

// ── Helpers ─────────────────────────────────────────────────────────

struct Submission {
    jt: JobTemplate,
    envs: Vec<EnvironmentTemplate>,
    params: openjd_model::JobParameterValues,
}

fn submission(jt_src: &str, env_srcs: &[&str], params: &[(&str, &str)]) -> Submission {
    let jt = job_ok(jt_src);
    let envs: Vec<EnvironmentTemplate> = env_srcs.iter().map(|s| env_ok(s)).collect();
    let params = job_params_with(&jt, params, &envs);
    Submission { jt, envs, params }
}

impl Submission {
    /// Create the Job, attachment `i` labelled `labels[i]` when given.
    fn create_labelled(&self, labels: &[&str]) -> Result<CreatedJob, ModelError> {
        let attached: Vec<AttachedEnvironmentTemplate<'_>> = self
            .envs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let a = AttachedEnvironmentTemplate::new(t);
                match labels.get(i) {
                    Some(l) => a.with_label(l),
                    None => a,
                }
            })
            .collect();
        create_job_with_environment_templates(
            &self.jt,
            &attached,
            &self.params,
            &posix_ctx(&self.jt),
        )
    }

    /// Create the Job, and check that `check_environment_templates`, which
    /// combines the literal requirements with the same `combine`, accepts
    /// what job creation accepts.
    fn job(&self) -> Job {
        let job = self
            .create_labelled(&[])
            .unwrap_or_else(|e| panic!("job creation failed:\n{e}"))
            .job;
        let attached: Vec<AttachedEnvironmentTemplate<'_>> = self
            .envs
            .iter()
            .map(AttachedEnvironmentTemplate::new)
            .collect();
        check_environment_templates(&self.jt, &attached, &posix_ctx(&self.jt))
            .unwrap_or_else(|e| panic!("check_environment_templates failed:\n{e}"));
        job
    }

    fn err(&self, expected: &[&str]) {
        self.err_labelled(&[], expected);
    }

    fn err_labelled(&self, labels: &[&str], expected: &[&str]) {
        let err = self
            .create_labelled(labels)
            .expect_err("expected the submission to be rejected");
        assert_errors(&err.to_string(), "Submission", expected);
    }
}

fn job(jt_src: &str) -> Job {
    submission(jt_src, &[], &[]).job()
}

fn amount(name: &str, min: Option<f64>, max: Option<f64>) -> AmountRequirement {
    AmountRequirement {
        name: name.to_string(),
        min,
        max,
    }
}

fn any_of(name: &str, values: &[&str]) -> AttributeRequirement {
    AttributeRequirement {
        name: name.to_string(),
        any_of: Some(values.iter().map(|v| v.to_string()).collect()),
        all_of: None,
    }
}

fn all_of(name: &str, values: &[&str]) -> AttributeRequirement {
    AttributeRequirement {
        name: name.to_string(),
        any_of: None,
        all_of: Some(values.iter().map(|v| v.to_string()).collect()),
    }
}

fn reqs(
    amounts: Vec<AmountRequirement>,
    attributes: Vec<AttributeRequirement>,
) -> Option<HostRequirements> {
    Some(HostRequirements {
        amounts: (!amounts.is_empty()).then_some(amounts),
        attributes: (!attributes.is_empty()).then_some(attributes),
    })
}

/// A Job Template with `ENV_PARAMS`, `EXPR`, and `extra` extensions whose
/// `jobEnvironments` and `steps` are `body` (YAML, at column 0).
fn jt(extra: &str, body: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [ENV_PARAMS, EXPR{extra}]\nname: J\n{body}"
    )
}

const RUN: &str = "script: {actions: {onRun: {command: x}}}";
const VARS: &str = "variables: {V: v}";

// ── §4 item 5: where the field is permitted ──────────────────────────

#[test]
fn host_requirements_on_an_environment_require_env_params() {
    job_err_with(
        &format!(
            "specificationVersion: jobtemplate-2023-09\nname: J\n\
             jobEnvironments:\n- name: E\n  {VARS}\n  hostRequirements:\n    \
             attributes: [{{name: \"attr.{{{{Param.Nope}}}}\", anyOf: []}}]\n\
             steps:\n- name: A\n  {RUN}\n"
        ),
        &["ENV_PARAMS"],
        &["jobEnvironments[0] -> hostRequirements:\n\thostRequirements on an Environment requires the ENV_PARAMS extension."],
    );
    env_err(
        &format!(
            "specificationVersion: environment-2023-09\nenvironment:\n  name: E\n  {VARS}\n  \
             hostRequirements:\n    attributes: [{{name: \"attr.{{{{Param.Nope}}}}\", anyOf: []}}]\n"
        ),
        &["environment -> hostRequirements:\n\thostRequirements on an Environment requires the ENV_PARAMS extension."],
    );
}

#[test]
fn host_requirements_are_rejected_on_a_step_environment() {
    job_err(
        &jt(
            "",
            &format!(
                "steps:\n- name: A\n  {RUN}\n  stepEnvironments:\n  - name: S\n    {VARS}\n    \
                 hostRequirements:\n      amounts: [{{name: amount.worker.vcpu, min: 1}}]\n"
            ),
        ),
        &["steps[0] -> stepEnvironments[0] -> hostRequirements:\n\tis not permitted on a stepEnvironments entry, whose Step has hostRequirements of its own."],
    );
}

#[test]
fn host_requirements_are_accepted_on_a_job_environment_and_an_environment_template() {
    job_ok(&jt(
        "",
        &format!(
            "jobEnvironments:\n- name: E\n  {VARS}\n  hostRequirements:\n    \
             amounts: [{{name: amount.worker.vcpu, min: 1}}]\nsteps:\n- name: A\n  {RUN}\n"
        ),
    ));
    env_ok(&format!(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\nenvironment:\n  \
         name: E\n  {VARS}\n  hostRequirements:\n    attributes: [{{name: attr.worker.os.family, anyOf: [linux]}}]\n"
    ));
}

// ── §3.3 per-source checks at template validation ───────────────────

#[test]
fn an_environments_requirements_get_the_steps_structural_checks() {
    job_err(
        &jt(
            "",
            &format!(
                "jobEnvironments:\n\
                 - name: Empty\n  {VARS}\n  hostRequirements: {{}}\n\
                 - name: Dup\n  {VARS}\n  hostRequirements:\n    amounts:\n    \
                 - {{name: amount.custom, min: 1}}\n    - {{name: AMOUNT.Custom, min: 2}}\n\
                 - name: Bounds\n  {VARS}\n  hostRequirements:\n    amounts:\n    \
                 - {{name: amount.a, min: -1}}\n    - {{name: amount.b, min: 3, max: 2}}\n\
                 - name: Name\n  {VARS}\n  hostRequirements:\n    attributes: [{{name: bad, anyOf: [x]}}]\n\
                 steps:\n- name: A\n  {RUN}\n"
            ),
        ),
        &[
            "jobEnvironments[0] -> hostRequirements:\n\tmust have at least one of amounts or attributes.",
            "jobEnvironments[1] -> hostRequirements -> amounts[1]:\n\tduplicate amount name 'AMOUNT.Custom'.",
            "jobEnvironments[2] -> hostRequirements -> amounts[0] -> min:\n\tmust be non-negative.",
            "jobEnvironments[2] -> hostRequirements -> amounts[1]:\n\tmin (3) > max (2).",
            "jobEnvironments[3] -> hostRequirements -> attributes[0]:\n\tname 'bad' does not match capability name pattern.",
        ],
    );
}

#[test]
fn an_environment_templates_requirements_get_the_structural_checks() {
    env_err(
        &format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\nenvironment:\n  \
             name: E\n  {VARS}\n  hostRequirements: {{}}\n"
        ),
        &["environment -> hostRequirements:\n\tmust have at least one of amounts or attributes."],
    );
}

#[test]
fn a_zero_min_is_accepted() {
    // N8: `<nonnegativefloatstring>` admits 0.
    job_ok(&jt(
        ", FEATURE_BUNDLE_1",
        &format!(
            "jobEnvironments:\n- name: E\n  {VARS}\n  hostRequirements:\n    \
             amounts: [{{name: amount.custom, min: \"0\"}}]\nsteps:\n- name: A\n  {RUN}\n"
        ),
    ));
}

// ── §4 item 5.1: format-string scopes ───────────────────────────────

/// A Job Template whose one Job Environment has parameters `P` (INT),
/// `D` (PATH) and `L` (LIST[PATH]), another Environment `O` with parameter
/// `Q`, Job Parameters `N` (STRING) and `Dir` (PATH), and an attribute
/// requirement whose value is `{{<expr>}}`.
fn scope_template(expr: &str) -> String {
    jt(
        "",
        &format!(
            "parameterDefinitions:\n- {{name: N, type: STRING, default: n}}\n- {{name: Dir, type: PATH, default: d}}\n\
             jobEnvironments:\n- name: O\n  {VARS}\n  envParameterDefinitions:\n  - {{name: Q, type: STRING, default: q}}\n\
             - name: E\n  {VARS}\n  envParameterDefinitions:\n  - {{name: P, type: INT, default: 1}}\n  \
             - {{name: D, type: PATH, default: /d}}\n  - {{name: L, type: \"LIST[PATH]\", default: [/l]}}\n  \
             hostRequirements:\n    attributes: [{{name: custom:attr.x, anyOf: [\"v{{{{{expr}}}}}\"]}}]\n\
             steps:\n- name: A\n  {RUN}\n"
        ),
    )
}

#[test]
fn the_scope_has_the_documents_parameters_job_names_and_the_environments_raw_parameters() {
    // §4 item 5.1: Env.RawParam.* of any type (a path as its unmapped
    // string), and Env.Param.* of non-path types.
    for expr in [
        "Param.N",
        "RawParam.N",
        "RawParam.Dir",
        "Env.Param.P",
        "Env.RawParam.P",
        "Env.RawParam.D",
        "Env.RawParam.L[0]",
        "Job.Name",
        "Job.EnvNames[0]",
    ] {
        job_ok(&scope_template(expr));
    }
}

#[test]
fn the_scope_excludes_path_parameters_steps_sessions_and_other_environments() {
    let path = "jobEnvironments[1] -> hostRequirements -> attributes[0] -> anyOf[0]";
    for (expr, message) in [
        ("Param.Dir", "Failed to parse interpolation expression at [1, 14]. Undefined variable: 'Param.Dir'. Did you mean one of: Param.N, RawParam.Dir"),
        ("Env.Param.D", "Failed to parse interpolation expression at [1, 16]. Undefined variable: 'Env.Param.D'. Did you mean: Env.Param.P"),
        ("Env.Param.L[0]", "Failed to parse interpolation expression at [1, 19]. Undefined variable: 'Env.Param.L'. Did you mean: Env.Param.P"),
        ("Env.Param.Q", "Failed to parse interpolation expression at [1, 16]. Undefined variable: 'Env.Param.Q'. Did you mean: Env.Param.P"),
        ("Session.WorkingDirectory", "Failed to parse interpolation expression at [1, 29]. Undefined variable: 'Session.WorkingDirectory'."),
        ("Step.Name", "Failed to parse interpolation expression at [1, 14]. Undefined variable: 'Step.Name'. Did you mean: Job.Name"),
    ] {
        job_err(&scope_template(expr), &[&format!("{path}:\n\t{message}")]);
    }
}

#[test]
fn an_environment_templates_scope_is_its_own_document_and_environment() {
    let et = |expr: &str| {
        format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
             parameterDefinitions:\n- {{name: T, type: STRING, default: t}}\nenvironment:\n  name: E\n  {VARS}\n  \
             envParameterDefinitions:\n  - {{name: P, type: INT, default: 1}}\n  - {{name: D, type: PATH, default: /d}}\n  \
             hostRequirements:\n    attributes: [{{name: custom:attr.x, anyOf: [\"v{{{{{expr}}}}}\"]}}]\n"
        )
    };
    for expr in [
        "Param.T",
        "RawParam.T",
        "Env.Param.P",
        "Env.RawParam.D",
        "Job.Name",
        "Job.EnvNames[0]",
    ] {
        env_ok(&et(expr));
    }
    env_err(
        &et("Env.Param.D"),
        &["environment -> hostRequirements -> attributes[0] -> anyOf[0]:\n\tFailed to parse interpolation expression at [1, 16]. Undefined variable: 'Env.Param.D'. Did you mean: Env.Param.P"],
    );
}

#[test]
fn a_steps_own_requirements_cannot_read_environment_parameters() {
    job_err(
        &jt(
            "",
            &format!(
                "jobEnvironments:\n- name: E\n  {VARS}\n  envParameterDefinitions:\n  - {{name: P, type: STRING, default: a}}\n\
                 steps:\n- name: A\n  {RUN}\n  hostRequirements:\n    \
                 attributes: [{{name: custom:attr.x, anyOf: [\"{{{{Env.Param.P}}}}\"]}}]\n"
            ),
        ),
        &["steps[0] -> hostRequirements -> attributes[0] -> anyOf[0]:\n\tFailed to parse interpolation expression at [0, 15]. Undefined variable: 'Env.Param.P'."],
    );
}

// ── §3.3.3 combination ──────────────────────────────────────────────

#[test]
fn amounts_take_the_greatest_min_and_least_max_and_do_not_add() {
    let job = job(&jt(
        "",
        &format!(
            "jobEnvironments:\n\
             - name: E1\n  {VARS}\n  hostRequirements:\n    amounts:\n    \
             - {{name: amount.worker.GPU, min: 1}}\n    - {{name: amount.custom.a, min: 2, max: 10}}\n    \
             - {{name: amount.custom.b, max: 8}}\n\
             - name: E2\n  {VARS}\n  hostRequirements:\n    amounts:\n    \
             - {{name: AMOUNT.CUSTOM.A, min: 4, max: 6}}\n    - {{name: amount.custom.c, min: 1}}\n\
             steps:\n- name: A\n  {RUN}\n  hostRequirements:\n    amounts:\n    \
             - {{name: amount.worker.gpu, min: 1}}\n    - {{name: amount.custom.b, min: 3}}\n"
        ),
    ));
    assert_eq!(
        job.steps[0].host_requirements,
        reqs(
            vec![
                // One GPU from the Step and one from E1 reserve one.
                amount("amount.worker.gpu", Some(1.0), None),
                amount("amount.custom.b", Some(3.0), Some(8.0)),
                amount("amount.custom.a", Some(4.0), Some(6.0)),
                amount("amount.custom.c", Some(1.0), None),
            ],
            vec![]
        )
    );
}

#[test]
fn attributes_combine_to_one_all_of_and_the_any_ofs_it_does_not_imply() {
    // §3.3.3 item 2.2: B, the union of every allOf, is one allOf; each
    // anyOf of two or more values stays unless it has a value of B. Two
    // anyOfs never intersect, since a host may have values of both.
    let job = job(&jt(
        "",
        &format!(
            "jobEnvironments:\n\
             - name: E1\n  {VARS}\n  hostRequirements:\n    attributes:\n    \
             - {{name: CUSTOM:attr.any, anyOf: [B, c, d]}}\n    - {{name: custom:ATTR.ALL, allOf: [Y, z]}}\n    \
             - {{name: custom:attr.mixed, allOf: [m]}}\n    - {{name: custom:attr.solo, anyOf: [s], allOf: [t]}}\n\
             steps:\n- name: A\n  {RUN}\n  hostRequirements:\n    attributes:\n    \
             - {{name: Custom:Attr.Any, anyOf: [a, b, C]}}\n    - {{name: custom:attr.all, allOf: [x, y]}}\n    \
             - {{name: custom:attr.mixed, anyOf: [m, n]}}\n"
        ),
    ));
    assert_eq!(
        job.steps[0].host_requirements,
        reqs(
            vec![],
            vec![
                // B is empty: both anyOfs stay, as written, with the first
                // source's name spelling.
                any_of("Custom:Attr.Any", &["a", "b", "C"]),
                any_of("Custom:Attr.Any", &["B", "c", "d"]),
                // The union ignoring case, in first-appearance order.
                all_of("custom:attr.all", &["x", "y", "z"]),
                // anyOf [m, n] has m, a value of B, so the allOf implies it.
                all_of("custom:attr.mixed", &["m"]),
                // Named by one source only: kept as written.
                AttributeRequirement {
                    name: "custom:attr.solo".to_string(),
                    any_of: Some(vec!["s".to_string()]),
                    all_of: Some(vec!["t".to_string()]),
                },
            ]
        )
    );
}

#[test]
fn a_one_value_any_of_joins_the_all_of() {
    // §3.3.3 item 2: an anyOf with one value is the allOf of that value, so
    // its value is in B, in first-appearance order (a source's anyOf before
    // its allOf) and ignoring case. An anyOf with more values that has no
    // value of B stays separate, however many other sources there are.
    let job = job(&jt(
        "",
        &format!(
            "jobEnvironments:\n\
             - name: E1\n  {VARS}\n  hostRequirements:\n    attributes:\n    \
             - {{name: custom:attr.one, allOf: [h]}}\n    - {{name: custom:attr.cut, anyOf: [B, c]}}\n    \
             - {{name: custom:attr.case, allOf: [X]}}\n    - {{name: custom:attr.two, allOf: [t]}}\n    \
             - {{name: custom:attr.lower, allOf: [x, B]}}\n    - {{name: custom:attr.SPELL, allOf: [b]}}\n    \
             - {{name: custom:attr.dup, allOf: [p]}}\n    - {{name: custom:attr.both, allOf: [z]}}\n    \
             - {{name: custom:attr.implied, anyOf: [X, y]}}\n\
             - name: E2\n  {VARS}\n  hostRequirements:\n    attributes:\n    \
             - {{name: custom:attr.cut, allOf: [y]}}\n\
             steps:\n- name: A\n  {RUN}\n  hostRequirements:\n    attributes:\n    \
             - {{name: custom:attr.one, anyOf: [g]}}\n    - {{name: custom:attr.cut, anyOf: [a, b]}}\n    \
             - {{name: custom:attr.case, anyOf: [x]}}\n    - {{name: custom:attr.two, anyOf: [t, u]}}\n    \
             - {{name: custom:attr.lower, anyOf: [X]}}\n    - {{name: Custom:Attr.Spell, anyOf: [a]}}\n    \
             - {{name: custom:attr.dup, anyOf: [q, Q]}}\n    \
             - {{name: custom:attr.both, anyOf: [q], allOf: [r]}}\n    \
             - {{name: custom:attr.implied, allOf: [x]}}\n"
        ),
    ));
    assert_eq!(
        job.steps[0].host_requirements,
        reqs(
            vec![],
            vec![
                all_of("custom:attr.one", &["g", "h"]),
                // Plan 4 A44's case: whatever the order, allOf [y] and the
                // two anyOfs, neither of which has y.
                all_of("custom:attr.cut", &["y"]),
                any_of("custom:attr.cut", &["a", "b"]),
                any_of("custom:attr.cut", &["B", "c"]),
                all_of("custom:attr.case", &["x"]),
                // anyOf [t, u] has t, so allOf [t] implies it.
                all_of("custom:attr.two", &["t"]),
                // Values compare ignoring case, and the combined entry
                // keeps the first source's name spelling (the Step's).
                all_of("custom:attr.lower", &["X", "B"]),
                all_of("Custom:Attr.Spell", &["a", "b"]),
                // `[q, Q]` is one value, compared ignoring case (§3.3.2).
                all_of("custom:attr.dup", &["q", "p"]),
                // A source's one-value anyOf comes before its allOf in B.
                all_of("custom:attr.both", &["q", "r", "z"]),
                // anyOf [X, y] has x, ignoring case, so allOf [x] implies it.
                all_of("custom:attr.implied", &["x"]),
            ]
        )
    );
}

#[test]
fn equal_and_superset_any_ofs_are_each_kept() {
    // §3.3.3 item 2.2 keeps "each anyOf" with no value of B (plan 5 A50),
    // including one equal to, or a superset of, another.
    let job = job(&jt(
        "",
        &format!(
            "jobEnvironments:\n\
             - name: E1\n  {VARS}\n  hostRequirements:\n    attributes: [{{name: custom:attr.f, anyOf: [a, b, c]}}]\n\
             - name: E2\n  {VARS}\n  hostRequirements:\n    attributes: [{{name: custom:attr.f, anyOf: [A, B]}}]\n\
             - name: E3\n  {VARS}\n  hostRequirements:\n    attributes: [{{name: custom:attr.f, anyOf: [a, b]}}]\n\
             steps:\n- name: A\n  {RUN}\n  hostRequirements:\n    attributes: [{{name: custom:attr.f, anyOf: [a, b]}}]\n"
        ),
    ));
    assert_eq!(
        job.steps[0].host_requirements,
        reqs(
            vec![],
            vec![
                any_of("custom:attr.f", &["a", "b"]),
                any_of("custom:attr.f", &["a", "b", "c"]),
                any_of("custom:attr.f", &["A", "B"]),
                any_of("custom:attr.f", &["a", "b"]),
            ]
        )
    );
}

#[test]
fn disjoint_any_ofs_on_a_custom_attribute_are_an_all_of_of_both() {
    // §3.3.3 item 2.2 (JB-119): a host may have both values, so anyOf [a]
    // and anyOf [b] are allOf [a, b], not a conflict.
    let job = job(&jt(
        "",
        &format!(
            "jobEnvironments:\n- name: E\n  {VARS}\n  hostRequirements:\n    \
             attributes: [{{name: custom:attr.f, anyOf: [b]}}]\n\
             steps:\n- name: A\n  {RUN}\n  hostRequirements:\n    \
             attributes: [{{name: custom:attr.f, anyOf: [a]}}]\n"
        ),
    ));
    assert_eq!(
        job.steps[0].host_requirements,
        reqs(vec![], vec![all_of("custom:attr.f", &["a", "b"])])
    );
    // Nor are anyOfs of two or more values with no value in common, from a
    // Job Environment and an attachment: each is kept, in source order (the
    // attachment before the Job Environment), at job creation and at check
    // (`Submission::job`).
    let jt_src = jt(
        "",
        &format!(
            "jobEnvironments:\n- name: E\n  {VARS}\n  hostRequirements:\n    \
             attributes: [{{name: custom:attr.f, anyOf: [a, b]}}]\n\
             steps:\n- name: A\n  {RUN}\n  hostRequirements:\n    \
             attributes: [{{name: custom:attr.f, anyOf: [c, d]}}]\n"
        ),
    );
    let env = format!(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\nenvironment:\n  \
         name: W\n  {VARS}\n  hostRequirements:\n    \
         attributes: [{{name: custom:attr.f, anyOf: [e, f]}}]\n"
    );
    let sub = submission(&jt_src, &[&env], &[]);
    assert_eq!(
        sub.job().steps[0].host_requirements,
        reqs(
            vec![],
            vec![
                any_of("custom:attr.f", &["c", "d"]),
                any_of("custom:attr.f", &["e", "f"]),
                any_of("custom:attr.f", &["a", "b"]),
            ]
        )
    );
}

/// A Job Template whose Step and two Job Environments each require one of
/// `lists` (a YAML `anyOf: [...]` or `allOf: [...]` fragment) of attribute
/// `name`, in that order.
fn three_sources(name: &str, lists: [&str; 3]) -> String {
    let [step, e1, e2] = lists;
    jt(
        "",
        &format!(
            "jobEnvironments:\n\
             - name: E1\n  {VARS}\n  hostRequirements:\n    attributes: [{{name: {name}, {e1}}}]\n\
             - name: E2\n  {VARS}\n  hostRequirements:\n    attributes: [{{name: {name}, {e2}}}]\n\
             steps:\n- name: A\n  {RUN}\n  hostRequirements:\n    attributes: [{{name: {name}, {step}}}]\n"
        ),
    )
}

/// Every order of `lists`.
fn orders(lists: [&str; 3]) -> Vec<[&str; 3]> {
    let [a, b, c] = lists;
    vec![
        [a, b, c],
        [a, c, b],
        [b, a, c],
        [b, c, a],
        [c, a, b],
        [c, b, a],
    ]
}

/// `hr`'s attribute requirements as sets of lowercased values, sorted, so
/// two combinations compare as conjunctions whatever their order.
fn as_sets(hr: &Option<HostRequirements>) -> Vec<(bool, Vec<String>)> {
    let mut sets: Vec<(bool, Vec<String>)> = hr
        .iter()
        .flat_map(|h| h.attributes.iter().flatten())
        .flat_map(|a| {
            [(true, &a.any_of), (false, &a.all_of)]
                .into_iter()
                .filter_map(|(is_any, v)| v.as_ref().map(|v| (is_any, v)))
                .map(|(is_any, v)| {
                    let mut v: Vec<String> = v.iter().map(|s| s.to_lowercase()).collect();
                    v.sort();
                    (is_any, v)
                })
                .collect::<Vec<_>>()
        })
        .collect();
    sets.sort();
    sets
}

#[test]
fn the_combination_does_not_depend_on_the_order_of_the_sources() {
    // §3.3.3: "Each form depends only on the set of requirements". JB-120's
    // custom attribute: allOf [a, x] and anyOf [b, c] in every order.
    let expected = vec![
        (false, vec!["a".to_string(), "x".to_string()]),
        (true, vec!["b".to_string(), "c".to_string()]),
    ];
    for order in orders(["anyOf: [a]", "anyOf: [b, c]", "allOf: [x]"]) {
        let job = job(&three_sources("custom:attr.f", order));
        assert_eq!(
            as_sets(&job.steps[0].host_requirements),
            expected,
            "{order:?}"
        );
    }
    // Item 2.1: the values in every list, in every order.
    for order in orders([
        "anyOf: [linux, windows]",
        "anyOf: [windows, macos]",
        "allOf: [windows]",
    ]) {
        let job = job(&three_sources("attr.worker.os.family", order));
        assert_eq!(
            job.steps[0].host_requirements,
            reqs(vec![], vec![any_of("attr.worker.os.family", &["windows"])]),
            "{order:?}"
        );
    }
    // A conflict is a conflict in every order, naming every source.
    for order in orders([
        "anyOf: [linux, windows]",
        "anyOf: [windows, macos]",
        "allOf: [macos]",
    ]) {
        submission(&three_sources("attr.worker.os.family", order), &[], &[]).err(&["JobTemplate -> steps[0]:\n\tStep 'A': no host can satisfy the combined host requirements for attribute 'attr.worker.os.family', of which a host has exactly one value: the anyOf and allOf values of JobTemplate -> steps[0] -> hostRequirements, JobTemplate -> jobEnvironments[0] -> hostRequirements (Environment 'E1'), JobTemplate -> jobEnvironments[1] -> hostRequirements (Environment 'E2') have no value in common. Template Schemas §3.3.3."]);
    }
}

#[test]
fn single_valued_attributes_combine_to_the_values_in_every_list() {
    // A single-valued attribute only one source names is kept as written,
    // both lists in one entry, like any other.
    let lone = job(&jt(
        "",
        &format!(
            "jobEnvironments:\n- name: E\n  {VARS}\n  hostRequirements:\n    \
             amounts: [{{name: amount.custom, min: 1}}]\n\
             steps:\n- name: A\n  {RUN}\n  hostRequirements:\n    \
             attributes: [{{name: attr.worker.os.family, anyOf: [linux, macos], allOf: [linux]}}]\n"
        ),
    ));
    assert_eq!(
        lone.steps[0].host_requirements,
        reqs(
            vec![amount("amount.custom", Some(1.0), None)],
            vec![AttributeRequirement {
                name: "attr.worker.os.family".to_string(),
                any_of: Some(vec!["linux".to_string(), "macos".to_string()]),
                all_of: Some(vec!["linux".to_string()]),
            }]
        )
    );
    // §3.3.3 item 2.1: a host has one os.family and one cpu.arch, so the
    // combination is one anyOf of the values every anyOf and allOf allows,
    // in first-appearance order, even when it is one value.
    let job = job(&jt(
        "",
        &format!(
            "jobEnvironments:\n- name: E\n  {VARS}\n  hostRequirements:\n    attributes:\n    \
             - {{name: attr.worker.os.family, anyOf: [macos, linux, windows]}}\n    \
             - {{name: ATTR.WORKER.CPU.ARCH, anyOf: [arm64, x86_64]}}\n\
             steps:\n- name: A\n  {RUN}\n  hostRequirements:\n    attributes:\n    \
             - {{name: attr.worker.OS.family, anyOf: [Windows, Linux]}}\n    \
             - {{name: attr.worker.cpu.arch, allOf: [x86_64]}}\n\
             - name: B\n  {RUN}\n"
        ),
    ));
    assert_eq!(
        job.steps[0].host_requirements,
        reqs(
            vec![],
            vec![
                // The first appearance's order and spelling, not sorted.
                any_of("attr.worker.OS.family", &["Windows", "Linux"]),
                any_of("attr.worker.cpu.arch", &["x86_64"]),
            ]
        )
    );
    // B has E's only, so they are kept as written.
    assert_eq!(
        job.steps[1].host_requirements,
        reqs(
            vec![],
            vec![
                any_of("attr.worker.os.family", &["macos", "linux", "windows"]),
                any_of("ATTR.WORKER.CPU.ARCH", &["arm64", "x86_64"]),
            ]
        )
    );
}

#[test]
fn a_tie_names_the_first_source_and_a_sources_any_of_comes_first() {
    // An amount conflict names the first source with the least max (and
    // the first with the greatest min), and item 2.1's values keep their
    // first appearance, a source's anyOf before its allOf.
    submission(
        &jt(
            "",
            &format!(
                "jobEnvironments:\n\
                 - name: E1\n  {VARS}\n  hostRequirements:\n    amounts: [{{name: amount.custom, max: 4}}]\n\
                 - name: E2\n  {VARS}\n  hostRequirements:\n    amounts: [{{name: amount.custom, max: 4}}]\n\
                 - name: E3\n  {VARS}\n  hostRequirements:\n    amounts: [{{name: amount.custom, min: 8}}]\n\
                 - name: E4\n  {VARS}\n  hostRequirements:\n    amounts: [{{name: amount.custom, min: 8}}]\n\
                 steps:\n- name: A\n  {RUN}\n"
            ),
        ),
        &[],
        &[],
    )
    .err(&["JobTemplate -> steps[0]:\n\tStep 'A': no host can satisfy the combined host requirements for amount 'amount.custom': JobTemplate -> jobEnvironments[2] -> hostRequirements (Environment 'E3') requires min 8, and JobTemplate -> jobEnvironments[0] -> hostRequirements (Environment 'E1') requires max 4. Template Schemas §3.3.3."]);
    let job = job(&jt(
        "",
        &format!(
            "jobEnvironments:\n- name: E\n  {VARS}\n  hostRequirements:\n    \
             attributes: [{{name: attr.worker.os.family, anyOf: [Linux]}}]\n\
             steps:\n- name: A\n  {RUN}\n  hostRequirements:\n    \
             attributes: [{{name: attr.worker.os.family, anyOf: [linux, windows], allOf: [LINUX]}}]\n"
        ),
    ));
    assert_eq!(
        job.steps[0].host_requirements,
        reqs(vec![], vec![any_of("attr.worker.os.family", &["linux"])])
    );
}

#[test]
fn a_single_valued_all_of_conflicts_with_an_any_of_without_its_value() {
    // §3.3.3 item 2.1 (JB-121): allOf [linux] and anyOf [windows, macos]
    // have no common value. On a custom attribute the same requirements
    // are allOf [linux] and the anyOf, not a conflict (item 2.2).
    let template = |name: &str| {
        jt(
            "",
            &format!(
                "jobEnvironments:\n- name: E\n  {VARS}\n  hostRequirements:\n    \
                 attributes: [{{name: {name}, anyOf: [windows, macos]}}]\n\
                 steps:\n- name: A\n  {RUN}\n  hostRequirements:\n    \
                 attributes: [{{name: {name}, allOf: [linux]}}]\n"
            ),
        )
    };
    submission(&template("attr.worker.os.family"), &[], &[]).err(&["JobTemplate -> steps[0]:\n\tStep 'A': no host can satisfy the combined host requirements for attribute 'attr.worker.os.family', of which a host has exactly one value: the anyOf and allOf values of JobTemplate -> steps[0] -> hostRequirements and JobTemplate -> jobEnvironments[0] -> hostRequirements (Environment 'E') have no value in common. Template Schemas §3.3.3."]);
    assert_eq!(
        job(&template("custom:attr.os")).steps[0].host_requirements,
        reqs(
            vec![],
            vec![
                all_of("custom:attr.os", &["linux"]),
                any_of("custom:attr.os", &["windows", "macos"]),
            ]
        )
    );
}

#[test]
fn a_step_without_requirements_gets_its_environments_and_one_without_contributions_keeps_its_own() {
    let job = job(&jt(
        "",
        &format!(
            "jobEnvironments:\n\
             - name: E\n  activation: ON_REQUEST\n  {VARS}\n  hostRequirements:\n    \
             amounts: [{{name: amount.Custom, min: 2}}]\n\
             steps:\n\
             - name: A\n  {RUN}\n  dependencies: [{{environment: E}}]\n\
             - name: B\n  {RUN}\n  hostRequirements:\n    amounts: [{{name: amount.custom, max: 1}}]\n"
        ),
    ));
    assert_eq!(
        job.steps[0].host_requirements,
        reqs(vec![amount("amount.Custom", Some(2.0), None)], vec![])
    );
    // B does not request E, so E contributes nothing and there is no
    // conflict.
    assert_eq!(
        job.steps[1].host_requirements,
        reqs(vec![amount("amount.custom", None, Some(1.0))], vec![])
    );
}

#[test]
fn a_disabled_entry_contributes_nothing() {
    let job = job(&jt(
        "",
        &format!(
            "jobEnvironments:\n\
             - name: E\n  activation: ON_REQUEST\n  {VARS}\n  hostRequirements:\n    \
             attributes: [{{name: custom:attr.f, anyOf: [b]}}]\n\
             steps:\n- name: A\n  {RUN}\n  dependencies: [{{environment: E, enableIf: \"'X' in Job.EnvNames\"}}]\n  \
             hostRequirements:\n    attributes: [{{name: custom:attr.f, anyOf: [a]}}]\n"
        ),
    ));
    assert_eq!(
        job.steps[0].host_requirements,
        reqs(vec![], vec![any_of("custom:attr.f", &["a"])])
    );
}

#[test]
fn an_unresolved_optional_external_entry_contributes_nothing() {
    let s = submission(
        &jt(
            "",
            &format!(
                "steps:\n- name: A\n  {RUN}\n  dependencies: [{{environment: Missing, required: false}}]\n  \
                 hostRequirements:\n    amounts: [{{name: amount.custom, max: 1}}]\n"
            ),
        ),
        &[],
        &[],
    );
    assert_eq!(
        s.job().steps[0].host_requirements,
        reqs(vec![amount("amount.custom", None, Some(1.0))], vec![])
    );
}

#[test]
fn a_job_without_environment_requirements_keeps_each_steps_own() {
    let src = format!(
        "specificationVersion: jobtemplate-2023-09\nname: J\nsteps:\n- name: A\n  {RUN}\n  \
         hostRequirements:\n    amounts: [{{name: amount.worker.vcpu, min: 2}}]\n"
    );
    let job = job(&src);
    assert_eq!(
        job.steps[0].host_requirements,
        reqs(vec![amount("amount.worker.vcpu", Some(2.0), None)], vec![])
    );
}

// ── §3.3.3 conflicts ────────────────────────────────────────────────

#[test]
fn an_amount_conflict_names_the_step_the_sources_and_the_capability() {
    submission(
        &jt(
            "",
            &format!(
                "jobEnvironments:\n\
                 - name: E1\n  {VARS}\n  hostRequirements:\n    amounts: [{{name: amount.custom, min: 4}}]\n\
                 - name: E2\n  {VARS}\n  hostRequirements:\n    amounts: [{{name: Amount.Custom, max: 2}}]\n\
                 steps:\n- name: A\n  {RUN}\n  hostRequirements:\n    amounts: [{{name: amount.custom, min: 1, max: 3}}]\n"
            ),
        ),
        &[],
        &[],
    )
    .err(&["JobTemplate -> steps[0]:\n\tStep 'A': no host can satisfy the combined host requirements for amount 'amount.custom': JobTemplate -> jobEnvironments[0] -> hostRequirements (Environment 'E1') requires min 4, and JobTemplate -> jobEnvironments[1] -> hostRequirements (Environment 'E2') requires max 2. Template Schemas §3.3.3."]);
}

#[test]
fn a_single_valued_conflict_names_every_source_that_requires_the_attribute() {
    submission(
        &jt(
            "",
            &format!(
                "jobEnvironments:\n\
                 - name: E1\n  {VARS}\n  hostRequirements:\n    attributes: [{{name: attr.worker.os.family, anyOf: [windows, macos]}}]\n\
                 - name: E2\n  {VARS}\n  hostRequirements:\n    attributes: [{{name: attr.worker.os.family, anyOf: [macos]}}]\n\
                 steps:\n- name: A\n  {RUN}\n  hostRequirements:\n    attributes: [{{name: attr.worker.os.family, anyOf: [linux, windows]}}]\n\
                 - name: B\n  {RUN}\n"
            ),
        ),
        &[],
        &[],
    )
    .err(&[
        // A: linux, windows / windows, macos / macos have no common value.
        // B's requirements are E1's and E2's only: macos.
        "JobTemplate -> steps[0]:\n\tStep 'A': no host can satisfy the combined host requirements for attribute 'attr.worker.os.family', of which a host has exactly one value: the anyOf and allOf values of JobTemplate -> steps[0] -> hostRequirements, JobTemplate -> jobEnvironments[0] -> hostRequirements (Environment 'E1'), JobTemplate -> jobEnvironments[1] -> hostRequirements (Environment 'E2') have no value in common. Template Schemas §3.3.3.",
    ]);
}

#[test]
fn a_conflict_with_an_attached_environment_names_its_label() {
    let step = jt(
        "",
        &format!(
            "steps:\n- name: A\n  {RUN}\n  hostRequirements:\n    attributes: [{{name: attr.worker.os.family, anyOf: [linux]}}]\n"
        ),
    );
    let hw = |value: &str| {
        format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\nenvironment:\n  name: Hw\n  {VARS}\n  \
             hostRequirements:\n    attributes: [{{name: attr.worker.OS.family, anyOf: [{value}]}}]\n"
        )
    };
    submission(&step, &[&hw("Windows")], &[]).err_labelled(
        &["hw.yaml"],
        &["JobTemplate -> steps[0]:\n\tStep 'A': no host can satisfy the combined host requirements for attribute 'attr.worker.os.family', of which a host has exactly one value: the anyOf and allOf values of JobTemplate -> steps[0] -> hostRequirements and hw.yaml -> environment -> hostRequirements (Environment 'Hw') have no value in common. Template Schemas §3.3.3."],
    );
    // Two attachments with one label are still two sources, each listed.
    submission(&step, &[&hw("Windows"), &hw("macos")], &[]).err_labelled(
        &["hw.yaml", "hw.yaml"],
        &["JobTemplate -> steps[0]:\n\tStep 'A': no host can satisfy the combined host requirements for attribute 'attr.worker.os.family', of which a host has exactly one value: the anyOf and allOf values of JobTemplate -> steps[0] -> hostRequirements, hw.yaml -> environment -> hostRequirements (Environment 'Hw'), hw.yaml -> environment -> hostRequirements (Environment 'Hw') have no value in common. Template Schemas §3.3.3."],
    );
}

// ── Resolution per entering Step ────────────────────────────────────

/// One `ALWAYS` Job Environment whose `attr.worker.os.family` requirement
/// reads its parameter `F` (default `linux`), and Steps `A` (supplies `F:
/// windows`), `B` and `C` (take the default), each requiring `anyOf:
/// [windows]` itself when `step_any_of` is set.
fn per_step_template(step_any_of: bool) -> String {
    let own = if step_any_of {
        "\n  hostRequirements:\n    attributes: [{name: attr.worker.os.family, anyOf: [windows]}]"
    } else {
        ""
    };
    jt(
        "",
        &format!(
            "jobEnvironments:\n- name: E\n  {VARS}\n  envParameterDefinitions:\n  - {{name: F, type: STRING, default: linux}}\n  \
             hostRequirements:\n    attributes: [{{name: attr.worker.os.family, anyOf: [\"{{{{Env.Param.F}}}}\"]}}]\n\
             steps:\n\
             - name: A\n  {RUN}\n  dependencies: [{{environment: E, parameters: {{F: windows}}}}]{own}\n\
             - name: B\n  {RUN}{own}\n\
             - name: C\n  {RUN}{own}\n"
        ),
    )
}

#[test]
fn an_environment_is_resolved_with_each_steps_effective_values() {
    let job = job(&per_step_template(false));
    assert_eq!(
        job.steps[0].host_requirements,
        reqs(vec![], vec![any_of("attr.worker.os.family", &["windows"])])
    );
    assert_eq!(
        job.steps[1].host_requirements,
        reqs(vec![], vec![any_of("attr.worker.os.family", &["linux"])])
    );
}

#[test]
fn a_conflict_that_only_resolution_reveals_is_reported_for_each_step_it_affects() {
    submission(&per_step_template(true), &[], &[]).err(&[
        "JobTemplate -> steps[1]:\n\tStep 'B': no host can satisfy the combined host requirements for attribute 'attr.worker.os.family', of which a host has exactly one value: the anyOf and allOf values of JobTemplate -> steps[1] -> hostRequirements and JobTemplate -> jobEnvironments[0] -> hostRequirements (Environment 'E') have no value in common. Template Schemas §3.3.3.",
        "JobTemplate -> steps[2]:\n\tStep 'C': no host can satisfy the combined host requirements for attribute 'attr.worker.os.family', of which a host has exactly one value: the anyOf and allOf values of JobTemplate -> steps[2] -> hostRequirements and JobTemplate -> jobEnvironments[0] -> hostRequirements (Environment 'E') have no value in common. Template Schemas §3.3.3.",
    ]);
}

/// [`per_step_template`] with the requirement a `FEATURE_BUNDLE_1`
/// amount `min` read from an INT parameter `N` (default 1) that Step `A`
/// sets to `n`.
fn amount_template(n: &str) -> String {
    jt(
        ", FEATURE_BUNDLE_1",
        &format!(
            "jobEnvironments:\n- name: E\n  {VARS}\n  envParameterDefinitions:\n  - {{name: N, type: INT, default: 1}}\n  \
             hostRequirements:\n    amounts: [{{name: amount.custom, min: \"{{{{Env.Param.N}}}}\"}}]\n\
             steps:\n\
             - name: A\n  {RUN}\n  dependencies: [{{environment: E, parameters: {{N: {n}}}}}]\n\
             - name: B\n  {RUN}\n  dependencies: [{{environment: E, parameters: {{N: {n}}}}}]\n\
             - name: C\n  {RUN}\n"
        ),
    )
}

#[test]
fn a_resolved_value_is_checked_once_per_distinct_values_naming_its_steps() {
    submission(&amount_template("-1"), &[], &[]).err(&[
        "JobTemplate -> jobEnvironments[0] -> hostRequirements -> amounts[0] -> min:\n\tmust be non-negative. Resolved for Steps 'A' and 'B'.",
    ]);
    let job = submission(&amount_template("3"), &[], &[]).job();
    assert_eq!(
        job.steps[1].host_requirements,
        reqs(vec![amount("amount.custom", Some(3.0), None)], vec![])
    );
    assert_eq!(
        job.steps[2].host_requirements,
        reqs(vec![amount("amount.custom", Some(1.0), None)], vec![])
    );
}

#[test]
fn resolved_names_must_be_unique_within_one_environment() {
    submission(
        &jt(
            "",
            &format!(
                "jobEnvironments:\n- name: E\n  {VARS}\n  envParameterDefinitions:\n  - {{name: S, type: STRING, default: x}}\n  \
                 hostRequirements:\n    attributes:\n    - {{name: \"custom:attr.{{{{Env.Param.S}}}}\", anyOf: [a]}}\n    \
                 - {{name: custom:attr.X, anyOf: [b]}}\n\
                 steps:\n- name: A\n  {RUN}\n"
            ),
        ),
        &[],
        &[],
    )
    .err(&["JobTemplate -> jobEnvironments[0] -> hostRequirements -> attributes[1]:\n\tduplicate attribute name 'custom:attr.X'. Resolved for Step 'A'."]);
}

#[test]
fn an_attached_environment_resolves_with_its_documents_job_names() {
    let s = submission(
        &jt("", &format!("steps:\n- name: A\n  {RUN}\n")),
        &[&format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\nenvironment:\n  name: Hw\n  {VARS}\n  \
             hostRequirements:\n    attributes: [{{name: custom:attr.n, anyOf: [\"{{{{Job.Name}}}}\", \"{{{{Job.EnvNames[0]}}}}\"]}}]\n"
        )],
        &[],
    );
    assert_eq!(
        s.job().steps[0].host_requirements,
        reqs(vec![], vec![any_of("custom:attr.n", &["J", "Hw"])])
    );
}

#[test]
fn an_attached_environments_resolution_error_names_its_document() {
    submission(
        &jt(
            "",
            &format!("steps:\n- name: A\n  {RUN}\n  dependencies: [{{environment: Gpu, parameters: {{V: \"-x\"}}}}]\n"),
        ),
        &[&format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\nenvironment:\n  name: Gpu\n  {VARS}\n  \
             envParameterDefinitions:\n  - {{name: V, type: STRING, default: a}}\n  \
             hostRequirements:\n    attributes: [{{name: custom:attr.v, anyOf: [\"{{{{Env.Param.V}}}}\"]}}]\n"
        )],
        &[],
    )
    .err_labelled(
        &["gpu.yaml"],
        &["gpu.yaml -> environment -> hostRequirements -> attributes[0] -> anyOf[0]:\n\tvalue '-x' contains invalid characters. Resolved for Step 'A'."],
    );
}

#[test]
fn job_parameters_and_raw_environment_parameters_resolve_at_job_creation() {
    let s = submission(
        &jt(
            "",
            &format!(
                "parameterDefinitions:\n- {{name: N, type: STRING, default: n}}\n- {{name: Dir, type: PATH, default: d}}\n\
                 jobEnvironments:\n- name: E\n  {VARS}\n  envParameterDefinitions:\n  - {{name: P, type: STRING, default: p}}\n  \
                 hostRequirements:\n    attributes:\n    - name: custom:attr.x\n      anyOf:\n      \
                 - \"{{{{Param.N}}}}\"\n      - \"r{{{{RawParam.N}}}}\"\n      \
                 - \"{{{{ 'dir' if len(RawParam.Dir) > 0 else 'none' }}}}\"\n      - \"{{{{Env.RawParam.P}}}}\"\n\
                 steps:\n- name: A\n  {RUN}\n"
            ),
        ),
        &[&format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
             parameterDefinitions:\n- {{name: T, type: STRING, default: t}}\nenvironment:\n  name: Hw\n  {VARS}\n  \
             hostRequirements:\n    attributes: [{{name: custom:attr.t, anyOf: [\"{{{{Param.T}}}}\"]}}]\n"
        )],
        &[],
    );
    assert_eq!(
        s.job().steps[0].host_requirements,
        reqs(
            vec![],
            vec![
                any_of("custom:attr.t", &["t"]),
                any_of("custom:attr.x", &["n", "rn", "dir", "p"]),
            ]
        )
    );
}

#[test]
fn a_job_environment_after_an_attachment_is_named_by_its_own_index() {
    submission(
        &jt(
            "",
            &format!(
                "jobEnvironments:\n- name: E0\n  {VARS}\n\
                 - name: E1\n  {VARS}\n  hostRequirements:\n    amounts: [{{name: amount.custom, max: 1}}]\n\
                 steps:\n- name: A\n  {RUN}\n"
            ),
        ),
        &[&format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\nenvironment:\n  name: Hw\n  {VARS}\n  \
             hostRequirements:\n    amounts: [{{name: amount.custom, min: 2}}]\n"
        )],
        &[],
    )
    .err(&["JobTemplate -> steps[0]:\n\tStep 'A': no host can satisfy the combined host requirements for amount 'amount.custom': EnvironmentTemplate[0] -> environment -> hostRequirements (Environment 'Hw') requires min 2, and JobTemplate -> jobEnvironments[1] -> hostRequirements (Environment 'E1') requires max 1. Template Schemas §3.3.3."]);
}

#[test]
fn steps_with_equal_values_in_different_text_resolve_separately() {
    // FLOAT 1.0 and 1.00 are equal values (§3.2.2), but an expression can
    // read their text, so each Step's requirements come from its own.
    let job = job(&jt(
        "",
        &format!(
            "jobEnvironments:\n- name: E\n  {VARS}\n  envParameterDefinitions:\n  - {{name: F, type: FLOAT, default: 2}}\n  \
             hostRequirements:\n    attributes: [{{name: custom:attr.f, anyOf: [\"{{{{ 'v' + string(Env.Param.F).replace('.', '_') }}}}\"]}}]\n\
             steps:\n\
             - name: A\n  {RUN}\n  dependencies: [{{environment: E, parameters: {{F: \"1.0\"}}}}]\n\
             - name: B\n  {RUN}\n  dependencies: [{{environment: E, parameters: {{F: \"1.00\"}}}}]\n"
        ),
    ));
    assert_eq!(
        job.steps[0].host_requirements,
        reqs(vec![], vec![any_of("custom:attr.f", &["v1_0"])])
    );
    assert_eq!(
        job.steps[1].host_requirements,
        reqs(vec![], vec![any_of("custom:attr.f", &["v1_00"])])
    );
}

/// An attached Environment `Gpu` whose amount `min` reads its INT
/// parameter `N` (no default when `required`).
fn gpu_template(required: bool) -> String {
    let default = if required { "" } else { ", default: 1" };
    format!(
        "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR, FEATURE_BUNDLE_1]\nenvironment:\n  \
         name: Gpu\n  {VARS}\n  envParameterDefinitions:\n  - {{name: N, type: INT{default}}}\n  \
         hostRequirements:\n    amounts: [{{name: amount.custom, min: \"{{{{Env.Param.N}}}}\"}}]\n"
    )
}

#[test]
fn rejected_values_are_not_also_reported_as_undefined_in_the_requirements() {
    let step = |deps: &str| jt("", &format!("steps:\n- name: A\n  {RUN}{deps}\n"));
    submission(
        &step("\n  dependencies: [{environment: Gpu, parameters: {N: abc}}]"),
        &[&gpu_template(false)],
        &[],
    )
    .err(&["JobTemplate -> steps[0] -> dependencies[0] -> parameters -> N:\n\tis not a valid INT value: Cannot parse 'abc' as integer"]);
    submission(&step(""), &[&gpu_template(true)], &[]).err(&[
        "JobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' dependency, every parameter without a default of each Environment its Sessions enter (every attached Environment with activation ALWAYS, and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1); missing: 'N' of Environment 'Gpu' (EnvironmentTemplate[0]).",
    ]);
}

#[test]
fn the_resolved_for_note_precedes_an_expression_errors_source_lines() {
    let err = submission(
        &jt(
            "",
            &format!(
                "jobEnvironments:\n- name: E\n  {VARS}\n  envParameterDefinitions:\n  - {{name: S, type: STRING, default: x}}\n  \
                 hostRequirements:\n    attributes: [{{name: custom:attr.s, anyOf: [\"{{{{ int(Env.Param.S) }}}}\"]}}]\n\
                 steps:\n- name: A\n  {RUN}\n"
            ),
        ),
        &[],
        &[],
    )
    .create_labelled(&[])
    .expect_err("expected the submission to be rejected")
    .to_string();
    assert_eq!(
        err,
        "Model validation error: 1 validation error for Submission\n\
         JobTemplate -> jobEnvironments[0] -> hostRequirements -> attributes[0] -> anyOf:\n\t\
         Format string error: Cannot convert 'x' to int. Resolved for Step 'A'.\n  \
         int(Env.Param.S)\n  \
         ^~~~~~~~~~~~~~~~"
    );
}

#[test]
fn the_tighter_bound_wins_whichever_source_comes_first_and_a_missing_max_is_unbounded() {
    let job = job(&jt(
        "",
        &format!(
            "jobEnvironments:\n- name: E\n  {VARS}\n  hostRequirements:\n    amounts:\n    \
             - {{name: amount.custom.a, min: 2, max: 10}}\n    - {{name: amount.custom.b, min: 2}}\n\
             steps:\n- name: A\n  {RUN}\n  hostRequirements:\n    amounts:\n    \
             - {{name: amount.custom.a, min: 4, max: 6}}\n    - {{name: amount.custom.b, max: 4}}\n"
        ),
    ));
    assert_eq!(
        job.steps[0].host_requirements,
        reqs(
            vec![
                amount("amount.custom.a", Some(4.0), Some(6.0)),
                amount("amount.custom.b", Some(2.0), Some(4.0)),
            ],
            vec![]
        )
    );
}

#[test]
fn resolved_capability_names_of_an_environment_are_checked() {
    submission(
        &jt(
            "",
            &format!(
                "jobEnvironments:\n- name: E\n  {VARS}\n  envParameterDefinitions:\n  - {{name: N, type: STRING, default: \"bad name!\"}}\n  \
                 hostRequirements:\n    amounts: [{{name: \"amount.custom.{{{{Env.Param.N}}}}\", min: 1}}]\n    \
                 attributes: [{{name: \"attr.custom.{{{{Env.Param.N}}}}\", anyOf: [a]}}]\n\
                 steps:\n- name: A\n  {RUN}\n"
            ),
        ),
        &[],
        &[],
    )
    .err(&["JobTemplate -> jobEnvironments[0] -> hostRequirements -> amounts[0]:\n\tname 'amount.custom.bad name!' does not match capability name pattern. Resolved for Step 'A'."]);
    submission(
        &jt(
            "",
            &format!(
                "jobEnvironments:\n- name: E\n  {VARS}\n  envParameterDefinitions:\n  - {{name: N, type: STRING, default: \"bad name!\"}}\n  \
                 hostRequirements:\n    attributes: [{{name: \"attr.custom.{{{{Env.Param.N}}}}\", anyOf: [a]}}]\n\
                 steps:\n- name: A\n  {RUN}\n"
            ),
        ),
        &[],
        &[],
    )
    .err(&["JobTemplate -> jobEnvironments[0] -> hostRequirements -> attributes[0]:\n\tname 'attr.custom.bad name!' does not match capability name pattern. Resolved for Step 'A'."]);
}

#[test]
fn a_step_without_requirements_and_no_contributing_environment_keeps_none() {
    let plain =
        format!("specificationVersion: jobtemplate-2023-09\nname: J\nsteps:\n- name: A\n  {RUN}\n");
    assert_eq!(job(&plain).steps[0].host_requirements, None);
    let unrequested = job(&jt(
        "",
        &format!(
            "jobEnvironments:\n- name: E\n  activation: ON_REQUEST\n  {VARS}\n  hostRequirements:\n    \
             amounts: [{{name: amount.custom, min: 1}}]\nsteps:\n- name: A\n  {RUN}\n"
        ),
    ));
    assert_eq!(unrequested.steps[0].host_requirements, None);
}

#[test]
fn resolved_amount_names_must_be_unique_within_one_environment() {
    submission(
        &jt(
            "",
            &format!(
                "jobEnvironments:\n- name: E\n  {VARS}\n  envParameterDefinitions:\n  - {{name: S, type: STRING, default: x}}\n  \
                 hostRequirements:\n    amounts:\n    - {{name: \"amount.custom.{{{{Env.Param.S}}}}\", min: 1}}\n    \
                 - {{name: amount.custom.X, min: 2}}\n\
                 steps:\n- name: A\n  {RUN}\n"
            ),
        ),
        &[],
        &[],
    )
    .err(&["JobTemplate -> jobEnvironments[0] -> hostRequirements -> amounts[1]:\n\tduplicate amount name 'amount.custom.X'. Resolved for Step 'A'."]);
}

#[test]
fn every_attached_environment_contributes_its_own_requirements() {
    let attachment = |name: &str, value: &str| {
        format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\nenvironment:\n  name: {name}\n  {VARS}\n  \
             envParameterDefinitions:\n  - {{name: P, type: STRING, default: {value}}}\n  \
             hostRequirements:\n    attributes: [{{name: \"custom:attr.{name}\", anyOf: [\"{{{{Env.Param.P}}}}\"]}}]\n"
        )
    };
    let s = submission(
        &jt("", &format!("steps:\n- name: A\n  {RUN}\n")),
        &[&attachment("One", "a"), &attachment("Two", "b")],
        &[],
    );
    assert_eq!(
        s.job().steps[0].host_requirements,
        reqs(
            vec![],
            vec![
                any_of("custom:attr.One", &["a"]),
                any_of("custom:attr.Two", &["b"])
            ]
        )
    );
}

#[test]
fn a_job_environment_resolves_job_name_and_env_names_at_job_creation() {
    let job = job(&jt(
        "",
        &format!(
            "jobEnvironments:\n- name: E\n  {VARS}\n  hostRequirements:\n    \
             attributes: [{{name: custom:attr.n, anyOf: [\"{{{{Job.Name}}}}\", \"{{{{Job.EnvNames[0]}}}}\"]}}]\n\
             steps:\n- name: A\n  {RUN}\n"
        ),
    ));
    assert_eq!(
        job.steps[0].host_requirements,
        reqs(vec![], vec![any_of("custom:attr.n", &["J", "E"])])
    );
}

#[test]
fn equal_combined_bounds_are_accepted() {
    let job = job(&jt(
        "",
        &format!(
            "jobEnvironments:\n- name: E\n  {VARS}\n  hostRequirements:\n    amounts: [{{name: amount.custom, max: 4}}]\n\
             steps:\n- name: A\n  {RUN}\n  hostRequirements:\n    amounts: [{{name: amount.custom, min: 4}}]\n"
        ),
    ));
    assert_eq!(
        job.steps[0].host_requirements,
        reqs(vec![amount("amount.custom", Some(4.0), Some(4.0))], vec![])
    );
}

#[test]
fn a_single_valued_conflict_is_reported_once_however_many_sources_take_part() {
    submission(
        &jt(
            "",
            &format!(
                "jobEnvironments:\n\
                 - name: E1\n  {VARS}\n  hostRequirements:\n    attributes: [{{name: attr.worker.os.family, anyOf: [windows]}}]\n\
                 - name: E2\n  {VARS}\n  hostRequirements:\n    attributes: [{{name: attr.worker.os.family, anyOf: [macos]}}]\n\
                 steps:\n- name: A\n  {RUN}\n  hostRequirements:\n    attributes: [{{name: attr.worker.os.family, anyOf: [linux]}}]\n"
            ),
        ),
        &[],
        &[],
    )
    .err(&["JobTemplate -> steps[0]:\n\tStep 'A': no host can satisfy the combined host requirements for attribute 'attr.worker.os.family', of which a host has exactly one value: the anyOf and allOf values of JobTemplate -> steps[0] -> hostRequirements, JobTemplate -> jobEnvironments[0] -> hostRequirements (Environment 'E1'), JobTemplate -> jobEnvironments[1] -> hostRequirements (Environment 'E2') have no value in common. Template Schemas §3.3.3."]);
}

#[test]
fn an_environments_resolved_lists_are_checked_at_its_field() {
    let template = |defs: &str, attr: &str| {
        jt(
            "",
            &format!(
                "jobEnvironments:\n- name: E\n  {VARS}\n  envParameterDefinitions:\n  - {defs}\n  \
                 hostRequirements:\n    attributes: [{attr}]\n\
                 steps:\n- name: A\n  {RUN}\n"
            ),
        )
    };
    submission(
        &template(
            "{name: L, type: \"LIST[STRING]\", default: [linux, windows]}",
            "{name: attr.worker.os.family, allOf: [\"{{Env.Param.L}}\"]}",
        ),
        &[],
        &[],
    )
    .err(&["JobTemplate -> jobEnvironments[0] -> hostRequirements -> attributes[0] -> allOf:\n\tsingle-valued attribute cannot have more than 1 element after resolution. Resolved for Step 'A'."]);
    let fifty_one: Vec<String> = (0..51).map(|i| format!("v{i}")).collect();
    submission(
        &template(
            &format!(
                "{{name: L, type: \"LIST[STRING]\", default: [{}]}}",
                fifty_one.join(", ")
            ),
            "{name: custom:attr.e, anyOf: [\"{{Env.Param.L}}\"]}",
        ),
        &[],
        &[],
    )
    .err(&["JobTemplate -> jobEnvironments[0] -> hostRequirements -> attributes[0] -> anyOf:\n\texceeds 50 elements after resolution. Resolved for Step 'A'."]);
    submission(
        &template(
            "{name: L, type: \"LIST[STRING]\", default: [x]}",
            "{name: custom:attr.e, anyOf: [\"{{ Env.Param.L[1:] }}\"]}",
        ),
        &[],
        &[],
    )
    .err(&["JobTemplate -> jobEnvironments[0] -> hostRequirements -> attributes[0] -> anyOf:\n\thas no elements after resolution. Resolved for Step 'A'."]);
}

#[test]
fn an_environment_templates_names_and_values_use_the_standard_capability_tables() {
    let et = |hr: &str| {
        format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\nenvironment:\n  \
             name: E\n  {VARS}\n  hostRequirements:\n    {hr}\n"
        )
    };
    env_ok(&et(
        "amounts: [{name: \"{{ 'amount.worker.vcpu' }}\", min: 1}]",
    ));
    env_ok(&et(
        "attributes: [{name: \"{{ 'attr.worker.os.family' }}\", anyOf: [linux]}]",
    ));
    env_err(
        &et("attributes: [{name: attr.worker.os.family, anyOf: [\"{{ 'beos' }}\"]}]"),
        &["environment -> hostRequirements -> attributes[0] -> anyOf[0]:\n\tvalue 'beos' is not valid for attr.worker.os.family."],
    );
}

#[test]
fn host_requirement_errors_follow_the_steps_value_errors() {
    submission(
        &jt(
            "",
            &format!(
                "steps:\n- name: A\n  {RUN}\n  dependencies: [{{environment: Gpu, parameters: {{N: abc}}}}]\n\
                 - name: B\n  {RUN}\n  hostRequirements:\n    amounts: [{{name: amount.custom, max: 0.5}}]\n"
            ),
        ),
        &[&gpu_template(false)],
        &[],
    )
    .err(&[
        "JobTemplate -> steps[0] -> dependencies[0] -> parameters -> N:\n\tis not a valid INT value: Cannot parse 'abc' as integer",
        "JobTemplate -> steps[1]:\n\tStep 'B': no host can satisfy the combined host requirements for amount 'amount.custom': EnvironmentTemplate[0] -> environment -> hostRequirements (Environment 'Gpu') requires min 1, and JobTemplate -> steps[1] -> hostRequirements requires max 0.5. Template Schemas §3.3.3.",
    ]);
    // `assert_errors` matches errors in any order; check this order.
    let err = submission(
        &jt(
            "",
            &format!(
                "steps:\n- name: A\n  {RUN}\n  dependencies: [{{environment: Gpu, parameters: {{N: abc}}}}]\n\
                 - name: B\n  {RUN}\n  hostRequirements:\n    amounts: [{{name: amount.custom, max: 0.5}}]\n"
            ),
        ),
        &[&gpu_template(false)],
        &[],
    )
    .create_labelled(&[])
    .expect_err("expected the submission to be rejected")
    .to_string();
    let value = err.find("parameters -> N").expect("value error");
    let conflict = err.find("Step 'B': no host").expect("conflict");
    assert!(value < conflict, "value error must come first:\n{err}");
}

#[test]
fn the_steps_a_resolution_error_names_are_capped() {
    let mut steps = String::new();
    for i in 0..60 {
        steps.push_str(&format!(
            "- name: S{i}\n  {RUN}\n  dependencies: [{{environment: E, parameters: {{N: -1}}}}]\n"
        ));
    }
    let src = jt(
        ", FEATURE_BUNDLE_1",
        &format!(
            "jobEnvironments:\n- name: E\n  {VARS}\n  envParameterDefinitions:\n  - {{name: N, type: INT, default: 1}}\n  \
             hostRequirements:\n    amounts: [{{name: amount.custom, min: \"{{{{Env.Param.N}}}}\"}}]\nsteps:\n{steps}"
        ),
    );
    let names: Vec<String> = (0..50).map(|i| format!("'S{i}'")).collect();
    submission(&src, &[], &[]).err(&[&format!(
        "JobTemplate -> jobEnvironments[0] -> hostRequirements -> amounts[0] -> min:\n\tmust be non-negative. Resolved for Steps {}, and 10 more.",
        names.join(", ")
    )]);
}

#[test]
fn each_environment_resolves_with_its_own_values_matched_by_document_and_name() {
    let env = |name: &str, n: &str, cap: &str| {
        format!(
            "- name: {name}\n  {VARS}\n  envParameterDefinitions:\n  - {{name: N, type: INT, default: {n}}}\n  \
             hostRequirements:\n    amounts: [{{name: amount.custom.{cap}, min: \"{{{{Env.Param.N}}}}\"}}]\n"
        )
    };
    // Two Job Environments: each reads its own N.
    let job = job(&jt(
        ", FEATURE_BUNDLE_1",
        &format!(
            "jobEnvironments:\n{}{}steps:\n- name: A\n  {RUN}\n",
            env("E1", "2", "a"),
            env("E2", "7", "b")
        ),
    ));
    assert_eq!(
        job.steps[0].host_requirements,
        reqs(
            vec![
                amount("amount.custom.a", Some(2.0), None),
                amount("amount.custom.b", Some(7.0), None),
            ],
            vec![]
        )
    );
    // A Job Environment and an attached one with the same name: each
    // reads its own document's values.
    let s = submission(
        &jt(
            ", FEATURE_BUNDLE_1",
            &format!("jobEnvironments:\n{}steps:\n- name: A\n  {RUN}\n", env("E", "2", "a")),
        ),
        &[&format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR, FEATURE_BUNDLE_1]\nenvironment:\n  \
             name: E\n  {VARS}\n  envParameterDefinitions:\n  - {{name: N, type: INT, default: 7}}\n  \
             hostRequirements:\n    amounts: [{{name: amount.custom.b, min: \"{{{{Env.Param.N}}}}\"}}]\n"
        )],
        &[],
    );
    assert_eq!(
        s.job().steps[0].host_requirements,
        reqs(
            vec![
                amount("amount.custom.b", Some(7.0), None),
                amount("amount.custom.a", Some(2.0), None),
            ],
            vec![]
        )
    );
}

#[test]
fn an_environment_parameter_has_its_definitions_type_in_the_requirements() {
    // Env.Param and Env.RawParam of an INT are both `int`: `- 1` would be
    // a type error on a string.
    let job = job(&jt(
        ", FEATURE_BUNDLE_1",
        &format!(
            "jobEnvironments:\n- name: E\n  {VARS}\n  envParameterDefinitions:\n  - {{name: N, type: INT, default: 3}}\n  \
             hostRequirements:\n    amounts:\n    - {{name: amount.custom.a, min: \"{{{{ Env.Param.N - 1 }}}}\"}}\n    \
             - {{name: amount.custom.b, min: \"{{{{ Env.RawParam.N - 2 }}}}\"}}\n\
             steps:\n- name: A\n  {RUN}\n"
        ),
    ));
    assert_eq!(
        job.steps[0].host_requirements,
        reqs(
            vec![
                amount("amount.custom.a", Some(2.0), None),
                amount("amount.custom.b", Some(1.0), None),
            ],
            vec![]
        )
    );
}

#[test]
fn list_limits_apply_to_each_source_and_not_to_the_combined_set() {
    // Each source has 20 amounts and 21 attributes (41, within 50); the
    // combined set has 40 and 41. One allOf of 30 values from each source
    // unites to 60.
    let source = |prefix: &str, all_of: &str| {
        let amounts: Vec<String> = (0..20)
            .map(|i| format!("{{name: amount.{prefix}.a{i}, min: 1}}"))
            .collect();
        let mut attributes: Vec<String> = (0..20)
            .map(|i| format!("{{name: custom:attr.{prefix}.a{i}, anyOf: [v]}}"))
            .collect();
        attributes.push(format!("{{name: custom:attr.shared, allOf: [{all_of}]}}"));
        format!(
            "hostRequirements:\n    amounts: [{}]\n    attributes: [{}]",
            amounts.join(", "),
            attributes.join(", ")
        )
    };
    let values = |prefix: &str| (0..30).map(|i| format!("{prefix}{i}")).collect::<Vec<_>>();
    let job = job(&jt(
        "",
        &format!(
            "jobEnvironments:\n- name: E\n  {VARS}\n  {}\nsteps:\n- name: A\n  {RUN}\n  {}\n",
            source("fromenv", &values("e").join(", ")),
            source("fromstep", &values("s").join(", "))
        ),
    ));
    let hr = job.steps[0].host_requirements.as_ref().unwrap();
    assert_eq!(hr.amounts.as_ref().unwrap().len(), 40);
    let attributes = hr.attributes.as_ref().unwrap();
    assert_eq!(attributes.len(), 41);
    let shared = attributes
        .iter()
        .find(|a| a.name == "custom:attr.shared")
        .unwrap();
    let mut expected = values("s");
    expected.extend(values("e"));
    assert_eq!(shared.all_of.as_ref(), Some(&expected));
}

#[test]
fn a_conflict_between_two_attachments_names_each_by_its_own_document() {
    let attachment = |name: &str, value: &str| {
        format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\nenvironment:\n  name: {name}\n  {VARS}\n  \
             hostRequirements:\n    attributes: [{{name: attr.worker.cpu.arch, anyOf: [{value}]}}]\n"
        )
    };
    submission(
        &jt("", &format!("steps:\n- name: A\n  {RUN}\n")),
        &[&attachment("One", "x86_64"), &attachment("Two", "arm64")],
        &[],
    )
    .err(&["JobTemplate -> steps[0]:\n\tStep 'A': no host can satisfy the combined host requirements for attribute 'attr.worker.cpu.arch', of which a host has exactly one value: the anyOf and allOf values of EnvironmentTemplate[0] -> environment -> hostRequirements (Environment 'One') and EnvironmentTemplate[1] -> environment -> hostRequirements (Environment 'Two') have no value in common. Template Schemas §3.3.3."]);
}

#[test]
fn a_resolved_min_above_its_own_max_is_reported_at_the_environment() {
    submission(
        &jt(
            ", FEATURE_BUNDLE_1",
            &format!(
                "jobEnvironments:\n- name: E\n  {VARS}\n  envParameterDefinitions:\n  - {{name: N, type: INT, default: 5}}\n  \
                 hostRequirements:\n    amounts: [{{name: amount.custom, min: \"{{{{Env.Param.N}}}}\", max: 2}}]\n\
                 steps:\n- name: A\n  {RUN}\n"
            ),
        ),
        &[],
        &[],
    )
    .err(&["JobTemplate -> jobEnvironments[0] -> hostRequirements -> amounts[0]:\n\tmin (5) > max (2). Resolved for Step 'A'."]);
}

#[test]
fn a_min_every_source_omits_stays_missing() {
    let job = job(&jt(
        "",
        &format!(
            "jobEnvironments:\n- name: E\n  {VARS}\n  hostRequirements:\n    amounts: [{{name: amount.custom, max: 8}}]\n\
             steps:\n- name: A\n  {RUN}\n  hostRequirements:\n    amounts: [{{name: amount.custom, max: 4}}]\n"
        ),
    ));
    assert_eq!(
        job.steps[0].host_requirements,
        reqs(vec![amount("amount.custom", None, Some(4.0))], vec![])
    );
}

#[test]
fn an_environments_resolved_amount_needs_a_positive_max_and_some_bound() {
    let template = |extra: &str, field: &str| {
        jt(
            extra,
            &format!(
                "jobEnvironments:\n- name: E\n  {VARS}\n  envParameterDefinitions:\n  - {{name: M, type: INT, default: 0}}\n  \
                 hostRequirements:\n    amounts: [{{name: amount.custom, {field}}}]\n\
                 steps:\n- name: A\n  {RUN}\n"
            ),
        )
    };
    submission(
        &template(", FEATURE_BUNDLE_1", "max: \"{{Env.Param.M}}\""),
        &[],
        &[],
    )
    .err(&["JobTemplate -> jobEnvironments[0] -> hostRequirements -> amounts[0] -> max:\n\tmust be positive. Resolved for Step 'A'."]);
    submission(
        &template(
            ", FEATURE_BUNDLE_1",
            "min: \"{{None if Env.Param.M == 0 else Env.Param.M}}\"",
        ),
        &[],
        &[],
    )
    .err(&["JobTemplate -> jobEnvironments[0] -> hostRequirements -> amounts[0]:\n\tmust have at least one of min or max after resolution. Resolved for Step 'A'."]);
}

#[test]
fn the_first_job_environments_requirements_are_scope_checked() {
    job_err(
        &jt(
            "",
            &format!(
                "jobEnvironments:\n- name: E\n  {VARS}\n  hostRequirements:\n    \
                 attributes: [{{name: custom:attr.x, anyOf: [\"{{{{Step.Name}}}}\"]}}]\n\
                 steps:\n- name: A\n  {RUN}\n"
            ),
        ),
        &["jobEnvironments[0] -> hostRequirements -> attributes[0] -> anyOf[0]:\n\tFailed to parse interpolation expression at [0, 13]. Undefined variable: 'Step.Name'. Did you mean: Job.Name"],
    );
}

#[test]
fn an_item_2_1_conflict_caps_its_list_of_sources() {
    // Every source that names the attribute is listed (plan 5 A49), capped
    // as the other job-creation lists are (512 characters, then "and N
    // more"), so the message stays bounded however many sources conflict.
    let envs: String = (0..8)
        .map(|i| {
            let value = if i == 0 { "linux" } else { "windows" };
            format!(
                "- name: E{i}\n  {VARS}\n  hostRequirements:\n    \
                 attributes: [{{name: attr.worker.os.family, anyOf: [{value}]}}]\n"
            )
        })
        .collect();
    let template = jt(
        "",
        &format!("jobEnvironments:\n{envs}steps:\n- name: A\n  {RUN}\n"),
    );
    submission(&template, &[], &[]).err(&["JobTemplate -> steps[0]:\n\tStep 'A': no host can satisfy the combined host requirements for attribute 'attr.worker.os.family', of which a host has exactly one value: the anyOf and allOf values of JobTemplate -> jobEnvironments[0] -> hostRequirements (Environment 'E0'), JobTemplate -> jobEnvironments[1] -> hostRequirements (Environment 'E1'), JobTemplate -> jobEnvironments[2] -> hostRequirements (Environment 'E2'), JobTemplate -> jobEnvironments[3] -> hostRequirements (Environment 'E3'), JobTemplate -> jobEnvironments[4] -> hostRequirements (Environment 'E4'), JobTemplate -> jobEnvironments[5] -> hostRequirements (Environment 'E5'), and 2 more have no value in common. Template Schemas §3.3.3."]);
}
