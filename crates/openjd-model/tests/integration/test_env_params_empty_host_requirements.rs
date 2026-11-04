// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `ENV_PARAMS` empty and discarded host requirements (Template Schemas
//! §3.3, RFC 0010): an empty `hostRequirements` is valid, a requirement
//! that resolves to nothing at job creation is discarded, the duplicate-name
//! rule applies to the requirements left, and an attribute's `anyOf` or
//! `allOf` may be one expression of type `list[string]?` (§3.3.2). Literal
//! empties stay errors, and without `ENV_PARAMS` the base rules hold.

use openjd_model::job::{
    AmountRequirement, AttributeRequirement, Document, EnvironmentRef, HostRequirements,
};
use openjd_model::template::{AttributeValues, EnvironmentTemplate, JobTemplate};
use openjd_model::{
    check_environment_templates, create_job_with_environment_templates,
    AttachedEnvironmentTemplate, CreatedJob, DiscardedHostRequirement, HostRequirementKind,
    ModelError,
};

use crate::env_params_common::{
    assert_errors, env_ok, job_err, job_err_with, job_ok, job_params_with, posix_ctx,
};

// ── Helpers ─────────────────────────────────────────────────────────

/// A Job Template with `extensions` whose body after `name` is `body`.
fn jt(extensions: &str, body: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09\nextensions: [{extensions}]\nname: J\n{body}"
    )
}

/// A Job Template with `ENV_PARAMS`, `EXPR`, and `FEATURE_BUNDLE_1`, a
/// STRING Job Parameter `G` (default `""`), and one Step `S` with
/// `hostRequirements: {hr}` (YAML flow style).
fn step_hr(hr: &str) -> String {
    jt(
        "ENV_PARAMS, EXPR, FEATURE_BUNDLE_1",
        &format!(
            "parameterDefinitions:\n- {{name: G, type: STRING, default: \"\"}}\n\
             steps:\n- name: S\n  hostRequirements: {hr}\n  {RUN}\n"
        ),
    )
}

/// As [`step_hr`], with the requirements on a Job Environment `E` that
/// declares STRING parameter `P` (default `""`) and every Step enters.
fn env_hr(hr: &str) -> String {
    jt(
        "ENV_PARAMS, EXPR, FEATURE_BUNDLE_1",
        &format!(
            "jobEnvironments:\n- name: E\n  activation: ALWAYS\n  {VARS}\n  \
             envParameterDefinitions:\n  - {{name: P, type: STRING, default: \"\"}}\n  \
             hostRequirements: {hr}\nsteps:\n- name: S\n  {RUN}\n"
        ),
    )
}

const RUN: &str = "script: {actions: {onRun: {command: x}}}";
const VARS: &str = "variables: {V: v}";

fn create(
    jt: &JobTemplate,
    envs: &[EnvironmentTemplate],
    params: &[(&str, &str)],
) -> Result<CreatedJob, ModelError> {
    let values = job_params_with(jt, params, envs);
    let attached: Vec<_> = envs.iter().map(AttachedEnvironmentTemplate::new).collect();
    create_job_with_environment_templates(jt, &attached, &values, &posix_ctx(jt))
}

/// Create the Job from `src` with `params`; it must succeed.
fn created(src: &str, params: &[(&str, &str)]) -> CreatedJob {
    create(&job_ok(src), &[], params).unwrap_or_else(|e| panic!("job creation failed:\n{e}"))
}

/// Create the Job from `src` with `params`; it must fail with exactly the
/// error `expected` (a Step's own requirements fail with one error).
fn create_err_msg(src: &str, params: &[(&str, &str)], expected: &str) {
    let err = create(&job_ok(src), &[], params).expect_err("expected job creation to fail");
    assert_eq!(err.to_string(), expected);
}

/// Create the Job from `src` with `params`; it must fail with `expected`.
fn create_err(src: &str, params: &[(&str, &str)], model: &str, expected: &[&str]) {
    let err = create(&job_ok(src), &[], params).expect_err("expected job creation to fail");
    assert_errors(&err.to_string(), model, expected);
}

fn amount(name: &str, min: Option<f64>, max: Option<f64>) -> AmountRequirement {
    AmountRequirement {
        name: name.to_string(),
        min,
        max,
    }
}

fn attr(name: &str, any_of: Option<&[&str]>, all_of: Option<&[&str]>) -> AttributeRequirement {
    let list = |v: Option<&[&str]>| v.map(|v| v.iter().map(|s| s.to_string()).collect());
    AttributeRequirement {
        name: name.to_string(),
        any_of: list(any_of),
        all_of: list(all_of),
    }
}

fn discarded(
    environment: Option<&str>,
    kind: HostRequirementKind,
    name: &str,
) -> DiscardedHostRequirement {
    DiscardedHostRequirement {
        environment: environment.map(|e| EnvironmentRef {
            document: Document::JobTemplate,
            environment: e.to_string(),
        }),
        kind,
        name: name.to_string(),
    }
}

// ── Template validation: empty objects and lists (§3.3 constraint 1) ──

#[test]
fn an_empty_host_requirements_is_valid_with_env_params() {
    // A Step (JT-173, JT-174), a Job Environment (JT-117), and an
    // Environment Template (ET-46).
    for hr in [
        "{}",
        "{amounts: [], attributes: []}",
        "{amounts: []}",
        "{attributes: []}",
    ] {
        job_ok(&step_hr(hr));
        job_ok(&env_hr(hr));
        env_ok(&format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR]\n\
             environment:\n  name: E\n  {VARS}\n  hostRequirements: {hr}\n"
        ));
    }
}

#[test]
fn an_empty_host_requirements_keeps_the_base_rule_without_env_params() {
    job_err_with(
        &jt(
            "EXPR",
            &format!("steps:\n- name: S\n  hostRequirements: {{}}\n  {RUN}\n"),
        ),
        &["EXPR"],
        &["steps[0] -> hostRequirements:\n\tmust have at least one of amounts or attributes."],
    );
}

#[test]
fn an_empty_host_requirements_means_no_requirements() {
    for hr in ["{}", "{amounts: [], attributes: []}"] {
        let c = created(&step_hr(hr), &[]);
        assert_eq!(c.job.steps[0].host_requirements, None, "{hr}");
        assert_eq!(c.discarded_host_requirements, [Vec::new()]);
        // An Environment's adds nothing to the Step's own.
        let c = created(&env_hr(hr), &[]);
        assert_eq!(c.job.steps[0].host_requirements, None, "{hr}");
    }
}

#[test]
fn literal_empties_stay_errors() {
    // JT-175 and JT-176: no value can change them.
    job_err(
        &step_hr(
            "{amounts: [{name: amount.custom}], attributes: [{name: custom:attr.a, anyOf: []}, \
             {name: custom:attr.b, allOf: []}]}",
        ),
        &[
            "steps[0] -> hostRequirements -> amounts[0]:\n\tmust have at least one of min or max.",
            "steps[0] -> hostRequirements -> attributes[0] -> anyOf:\n\tmust not be empty.",
            "steps[0] -> hostRequirements -> attributes[1] -> allOf:\n\tmust not be empty.",
        ],
    );
}

// ── Template validation: `anyOf` / `allOf` as one expression (§3.3.2) ──

#[test]
fn any_of_and_all_of_may_be_one_expression() {
    // JT-177.
    let t = job_ok(&env_hr(
        "{attributes: [{name: custom:attr.m, anyOf: \"{{ [Env.Param.P] if Env.Param.P != '' \
         else None }}\", allOf: \"{{ ['a', 'b'] }}\"}]}",
    ));
    let a = &t.job_environments.as_ref().unwrap()[0]
        .host_requirements
        .as_ref()
        .unwrap()
        .attributes
        .as_ref()
        .unwrap()[0];
    assert!(matches!(a.any_of, Some(AttributeValues::Expression(_))));
    assert!(a.may_be_discarded());
}

#[test]
fn a_string_any_of_needs_env_params() {
    job_err_with(
        &jt(
            "EXPR",
            &format!(
                "steps:\n- name: S\n  hostRequirements:\n    attributes: [{{name: custom:attr.a, \
                 anyOf: \"{{{{ ['a'] }}}}\"}}]\n  {RUN}\n"
            ),
        ),
        &["EXPR"],
        &[
            "steps[0] -> hostRequirements -> attributes[0] -> anyOf:\n\tmust be a list of values, \
           not a string. A string holding one {{ }} expression is accepted in place of the list \
           only with the ENV_PARAMS extension (Template Schemas §3.3.2).",
        ],
    );
}

#[test]
fn a_rejected_string_any_of_gets_no_expression_checks() {
    // Structure rejects the string; pass 8, which runs after structure
    // errors too, must not then check it as an ENV_PARAMS expression
    // (an empty list, a list of numbers, the duplicate rule).
    let needs = "must be a list of values, not a string. A string holding one {{ }} expression \
                 is accepted in place of the list only with the ENV_PARAMS extension (Template \
                 Schemas §3.3.2).";
    job_err_with(
        &jt(
            "EXPR",
            &format!(
                "parameterDefinitions:\n- {{name: LI, type: \"LIST[INT]\", default: [1]}}\n\
                 steps:\n- name: S\n  hostRequirements:\n    attributes:\n    \
                 - {{name: custom:attr.a, anyOf: \"{{{{ Param.LI }}}}\"}}\n    \
                 - {{name: custom:attr.b, allOf: \"{{{{ [] }}}}\"}}\n  {RUN}\n"
            ),
        ),
        &["EXPR"],
        &[
            &format!("steps[0] -> hostRequirements -> attributes[0] -> anyOf:\n\t{needs}"),
            &format!("steps[0] -> hostRequirements -> attributes[1] -> allOf:\n\t{needs}"),
        ],
    );
    let not_one = "must be a list of values, or a string that is exactly one {{ }} expression \
                   with no other text. A list has no string form, so the expression cannot be \
                   joined with other text (Template Schemas §3.3.2).";
    job_err(
        &step_hr(
            "{attributes: [{name: custom:attr.a, anyOf: \"{{ [1] }} {{ [2] }}\"}, {name: \
             custom:attr.b, allOf: \" {{ [] }}\"}]}",
        ),
        &[
            &format!("steps[0] -> hostRequirements -> attributes[0] -> anyOf:\n\t{not_one}"),
            &format!("steps[0] -> hostRequirements -> attributes[1] -> allOf:\n\t{not_one}"),
        ],
    );
}

#[test]
fn a_string_any_of_must_be_exactly_one_expression() {
    // JT-178: a list has no string form.
    let not_one = "must be a list of values, or a string that is exactly one {{ }} expression \
                   with no other text. A list has no string form, so the expression cannot be \
                   joined with other text (Template Schemas §3.3.2).";
    job_err(
        &step_hr(
            "{attributes: [{name: custom:attr.a, anyOf: \"x{{Param.G}}\"}, {name: custom:attr.b, \
             allOf: \"linux\"}, {name: custom:attr.c, anyOf: \"{{ ['a'] }} \"}, {name: \
             custom:attr.d, anyOf: \"\"}]}",
        ),
        &[
            &format!("steps[0] -> hostRequirements -> attributes[0] -> anyOf:\n\t{not_one}"),
            &format!("steps[0] -> hostRequirements -> attributes[1] -> allOf:\n\t{not_one}"),
            &format!("steps[0] -> hostRequirements -> attributes[2] -> anyOf:\n\t{not_one}"),
            &format!("steps[0] -> hostRequirements -> attributes[3] -> anyOf:\n\t{not_one}"),
        ],
    );
}

#[test]
fn an_expressions_static_type_must_fit_a_list_of_strings() {
    // JT-179: an INT does not coerce to `list[string]?`.
    job_err(
        &jt(
            "ENV_PARAMS, EXPR",
            &format!(
                "jobEnvironments:\n- name: E\n  {VARS}\n  envParameterDefinitions:\n  \
                 - {{name: N, type: INT, default: 1}}\n  hostRequirements:\n    attributes: \
                 [{{name: custom:attr.a, anyOf: \"{{{{ Env.Param.N }}}}\"}}]\nsteps:\n- name: S\n  {RUN}\n"
            ),
        ),
        &["jobEnvironments[0] -> hostRequirements -> attributes[0] -> anyOf:\n\tFailed to parse \
           interpolation expression at [0, 17]. Cannot coerce int to list[string]?"],
    );
}

#[test]
fn a_list_of_numbers_is_rejected_at_template_validation() {
    // An openjd-rs choice (specs/model/validation.md): no `int` or `float`
    // string form is an `<AttributeCapabilityValue>`, and an empty list is
    // an error too, so such an expression can never resolve to a valid
    // value. One that may resolve to `null` can, so it is accepted, as is a
    // list of `bool`, whose string forms are `true` and `false`.
    let numbers = |t: &str| {
        format!(
            "resolves to a {t}, whose elements can never be attribute values: a value must start \
             with a letter or an underscore, and the string form of a number starts with a digit \
             or a sign (Template Schemas §3.3.2.2)."
        )
    };
    job_err(
        &jt(
            "ENV_PARAMS, EXPR",
            &format!(
                "parameterDefinitions:\n- {{name: L, type: \"LIST[INT]\", default: [1]}}\n\
                 - {{name: F, type: FLOAT, default: 1.5}}\n\
                 steps:\n- name: S\n  hostRequirements:\n    attributes:\n    \
                 - {{name: custom:attr.a, anyOf: \"{{{{ Param.L }}}}\"}}\n    \
                 - {{name: custom:attr.b, allOf: \"{{{{ [Param.F] }}}}\"}}\n    \
                 - {{name: custom:attr.c, anyOf: \"{{{{ [Param.F] if Param.F > 2 else None }}}}\"}}\n    \
                 - {{name: custom:attr.d, anyOf: \"{{{{ [Param.F > 2] }}}}\"}}\n  {RUN}\n"
            ),
        ),
        &[
            &format!(
                "steps[0] -> hostRequirements -> attributes[0] -> anyOf:\n\t{}",
                numbers("list[int]")
            ),
            &format!(
                "steps[0] -> hostRequirements -> attributes[1] -> allOf:\n\t{}",
                numbers("list[float]")
            ),
        ],
    );
    // A static list gets that one error, not one per element as well; a
    // choice of lists of numbers is a list of numbers too, and so is one
    // that may be empty, since `[]` is an error as well.
    job_err(
        &jt(
            "ENV_PARAMS, EXPR",
            &format!(
                "parameterDefinitions:\n- {{name: G, type: STRING, default: \"\"}}\n\
                 - {{name: LI, type: \"LIST[INT]\", default: [1]}}\n\
                 steps:\n- name: S\n  hostRequirements:\n    attributes:\n    \
                 - {{name: custom:attr.a, anyOf: \"{{{{ [1, 2.5] }}}}\"}}\n    \
                 - {{name: custom:attr.b, anyOf: \"{{{{ [1] if Param.G != '' else [2.5] }}}}\"}}\n    \
                 - {{name: custom:attr.c, anyOf: \"{{{{ Param.LI if Param.G != '' else [] }}}}\"}}\n    \
                 - {{name: custom:attr.d, anyOf: \"{{{{ [x * 2 for x in Param.LI] }}}}\"}}\n    \
                 - {{name: custom:attr.e, anyOf: \"{{{{ Param.LI if Param.G != '' else ['a'] }}}}\"}}\n  {RUN}\n"
            ),
        ),
        &[
            &format!(
                "steps[0] -> hostRequirements -> attributes[0] -> anyOf:\n\t{}",
                numbers("list[float]")
            ),
            &format!(
                "steps[0] -> hostRequirements -> attributes[1] -> anyOf:\n\t{}",
                numbers("list[float] | list[int]")
            ),
            &format!(
                "steps[0] -> hostRequirements -> attributes[2] -> anyOf:\n\t{}",
                numbers("list[int] | list[nulltype]")
            ),
            &format!(
                "steps[0] -> hostRequirements -> attributes[3] -> anyOf:\n\t{}",
                numbers("list[int?]")
            ),
        ],
    );
}

#[test]
fn a_static_list_result_is_checked_at_template_validation() {
    let fifty_one: Vec<String> = (0..51).map(|i| format!("'v{i}'")).collect();
    job_err(
        &step_hr(&format!(
            "{{attributes: [{{name: custom:attr.a, anyOf: \"{{{{ [] }}}}\"}}, {{name: \
             attr.worker.os.family, allOf: \"{{{{ ['linux', 'windows'] }}}}\"}}, {{name: \
             custom:attr.c, anyOf: \"{{{{ ['ok', '9x'] }}}}\"}}, {{name: custom:attr.d, anyOf: \
             \"{{{{ [{}] }}}}\"}}, {{name: attr.worker.cpu.arch, anyOf: \"{{{{ ['sparc'] }}}}\"}}]}}",
            fifty_one.join(", ")
        )),
        &[
            "steps[0] -> hostRequirements -> attributes[0] -> anyOf:\n\tresolves to an empty \
             list. An empty list is not allowed; an expression that resolves to null omits the \
             field instead (Template Schemas §3.3.2).",
            "steps[0] -> hostRequirements -> attributes[1] -> allOf:\n\tsingle-valued attribute \
             cannot have more than 1 element after resolution.",
            "steps[0] -> hostRequirements -> attributes[2] -> anyOf:\n\tlist element 1: value \
             '9x' contains invalid characters.",
            "steps[0] -> hostRequirements -> attributes[3] -> anyOf:\n\tresolves to a list of 51 \
             elements, exceeding the maximum of 50.",
            "steps[0] -> hostRequirements -> attributes[4] -> anyOf:\n\tlist element 0: value \
             'sparc' is not valid for attr.worker.cpu.arch.",
        ],
    );
    // A static `None` omits the field, and one value for the allOf of a
    // single-valued attribute and exactly 50 values are allowed.
    let fifty: Vec<String> = (0..50).map(|i| format!("'v{i}'")).collect();
    job_ok(&step_hr(&format!(
        "{{attributes: [{{name: custom:attr.a, anyOf: \"{{{{ None }}}}\"}}, {{name: \
         attr.worker.os.family, allOf: \"{{{{ ['linux'] }}}}\"}}, {{name: custom:attr.d, anyOf: \
         \"{{{{ [{}] }}}}\"}}]}}",
        fifty.join(", ")
    )));
}

// ── Template validation: duplicates after discarding (§3.3, §7.5 rule 5) ──

#[test]
fn a_duplicate_that_may_be_discarded_is_not_an_error_at_template_validation() {
    // JB-138's template: the second amount's bounds may both resolve to
    // `null`. Likewise an attribute written as one expression.
    job_ok(&env_hr(
        "{amounts: [{name: amount.custom, min: 1}, {name: amount.custom, min: \"{{ 2 if \
         Env.Param.P != '' else None }}\", max: \"{{ 4 if Env.Param.P != '' else None }}\"}], \
         attributes: [{name: custom:attr.a, anyOf: [x]}, {name: custom:attr.a, anyOf: \"{{ \
         [Env.Param.P] if Env.Param.P != '' else None }}\"}]}",
    ));
}

#[test]
fn a_duplicate_that_is_kept_whatever_the_values_is_an_error() {
    job_err(
        &step_hr(
            "{amounts: [{name: amount.custom, min: 1}, {name: amount.custom, max: \"{{ 2 }}\"}, \
             {name: amount.other, min: \"{{ 1 if Param.G != '' else None }}\"}, {name: amount.other, \
             max: 3}, {name: amount.other, max: \"{{ None }}\"}], attributes: [{name: \
             custom:attr.a, anyOf: [x]}, {name: custom:attr.a, allOf: \"{{ ['y'] }}\"}, {name: \
             custom:attr.b, anyOf: [x]}, {name: custom:attr.b, anyOf: [y]}]}",
        ),
        &[
            // A static non-null bound keeps the amount.
            "steps[0] -> hostRequirements -> amounts[1]:\n\tduplicate amount name 'amount.custom'.",
            "steps[0] -> hostRequirements -> attributes[1]:\n\tduplicate attribute name \
             'custom:attr.a'.",
            "steps[0] -> hostRequirements -> attributes[3]:\n\tduplicate attribute name \
             'custom:attr.b'.",
        ],
    );
}

#[test]
fn a_duplicate_whose_bounds_cannot_be_null_is_an_error_at_template_validation() {
    // §7.5 rule 5: a bound or expression whose static type has no `null`
    // keeps its requirement whatever the values are, so the duplicate holds
    // for every value and is reported at once.
    job_err(
        &jt(
            "ENV_PARAMS, EXPR, FEATURE_BUNDLE_1",
            &format!(
                "parameterDefinitions:\n- {{name: F, type: FLOAT, default: 1}}\n\
                 - {{name: L, type: \"LIST[STRING]\", default: [a]}}\n\
                 steps:\n- name: S\n  hostRequirements:\n    amounts:\n    \
                 - {{name: amount.worker.vcpu, min: \"{{{{Param.F}}}}\"}}\n    \
                 - {{name: amount.worker.vcpu, max: \"{{{{Param.F}}}}\"}}\n    attributes:\n    \
                 - {{name: custom:attr.a, anyOf: \"{{{{Param.L}}}}\"}}\n    \
                 - {{name: custom:attr.a, anyOf: \"{{{{Param.L}}}}\"}}\n  {RUN}\n"
            ),
        ),
        &[
            "steps[0] -> hostRequirements -> amounts[1]:\n\tduplicate amount name \
             'amount.worker.vcpu'.",
            "steps[0] -> hostRequirements -> attributes[1]:\n\tduplicate attribute name \
             'custom:attr.a'.",
        ],
    );
}

#[test]
fn a_duplicate_is_reported_once() {
    // The middle entry may be discarded by its text, so structure compares
    // only the outer two; pass 8 sees that it is kept and reports it, and
    // must not report the third entry again.
    job_err(
        &step_hr(
            "{amounts: [{name: amount.x, min: 1}, {name: amount.x, min: \"{{ 2 }}\"}, {name: \
             amount.x, min: 3}], attributes: [{name: custom:attr.a, anyOf: [x]}, {name: \
             custom:attr.a, anyOf: \"{{ ['y'] }}\"}, {name: custom:attr.a, anyOf: [z]}]}",
        ),
        &[
            "steps[0] -> hostRequirements -> amounts[1]:\n\tduplicate amount name 'amount.x'.",
            "steps[0] -> hostRequirements -> amounts[2]:\n\tduplicate amount name 'amount.x'.",
            "steps[0] -> hostRequirements -> attributes[1]:\n\tduplicate attribute name \
             'custom:attr.a'.",
            "steps[0] -> hostRequirements -> attributes[2]:\n\tduplicate attribute name \
             'custom:attr.a'.",
        ],
    );
}

#[test]
fn a_steps_expression_that_fails_to_resolve_names_its_field() {
    // The static type `string | list[string]` passes template validation;
    // a string result does not fit `list[string]?` at job creation.
    let src = |field: &str| {
        step_hr(&format!(
            "{{attributes: [{{name: custom:attr.a, {field}: \"{{{{ Param.G if Param.G == 'z' else \
             [Param.G] }}}}\"}}]}}"
        ))
    };
    for field in ["anyOf", "allOf"] {
        create_err_msg(
            &src(field),
            &[("G", "z")],
            &format!(
                "Format string error: steps[0] -> hostRequirements -> attributes[0] -> {field}: \
                 Cannot coerce string to list[string]?\n  Param.G if Param.G == 'z' else \
                 [Param.G]\n  ~~~~~~^"
            ),
        );
    }
    // So does a list-form item that fails to resolve.
    create_err_msg(
        &step_hr(
            "{attributes: [{name: custom:attr.a, anyOf: [\"{{ Param.G if Param.G == 'z' else \
             [[Param.G]] }}\"]}]}",
        ),
        &[("G", "y")],
        "Format string error: steps[0] -> hostRequirements -> attributes[0] -> anyOf: Cannot \
         coerce list[list[string]] to list[string] | string | nulltype\n  Param.G if Param.G == \
         'z' else [[Param.G]]\n                                 ^~~~~~~~~~~",
    );
    // An Environment's names the field through its path.
    create_err(
        &env_hr(
            "{attributes: [{name: custom:attr.a, anyOf: \"{{ Env.Param.P if Env.Param.P == '' \
             else [Env.Param.P] }}\"}]}",
        ),
        &[],
        "JobTemplate",
        &[
            "jobEnvironments[0] -> hostRequirements -> attributes[0] -> anyOf:\n\t\
           Format string error: Cannot coerce string to list[string]?. Resolved for Step 'S'.",
        ],
    );
}

#[test]
fn without_env_params_a_duplicate_amount_is_an_error_whatever_its_bounds() {
    job_err_with(
        &jt(
            "EXPR, FEATURE_BUNDLE_1",
            &format!(
                "steps:\n- name: S\n  hostRequirements:\n    amounts:\n    - {{name: amount.custom, \
                 min: \"{{{{ None }}}}\"}}\n    - {{name: amount.custom, min: \"{{{{ None }}}}\"}}\n  {RUN}\n"
            ),
        ),
        &["EXPR", "FEATURE_BUNDLE_1"],
        &["steps[0] -> hostRequirements -> amounts[1]:\n\tduplicate amount name 'amount.custom'."],
    );
}

// ── Job creation: discarding (§3.3) ─────────────────────────────────

#[test]
fn an_amount_whose_bounds_resolve_to_null_is_discarded() {
    // JB-132: the Step's only amount.
    let src = step_hr(
        "{amounts: [{name: amount.custom, min: \"{{ 1 if Param.G != '' else None }}\"}, {name: \
         amount.kept, max: 2}]}",
    );
    let c = created(&src, &[]);
    assert_eq!(
        c.job.steps[0].host_requirements,
        Some(HostRequirements {
            amounts: Some(vec![amount("amount.kept", None, Some(2.0))]),
            attributes: None,
        })
    );
    assert_eq!(
        c.discarded_host_requirements,
        [vec![discarded(
            None,
            HostRequirementKind::Amount,
            "amount.custom"
        )]]
    );
    // With a value it is kept.
    let c = created(&src, &[("G", "x")]);
    assert_eq!(
        c.job.steps[0].host_requirements.as_ref().unwrap().amounts,
        Some(vec![
            amount("amount.custom", Some(1.0), None),
            amount("amount.kept", None, Some(2.0)),
        ])
    );
    assert_eq!(c.discarded_host_requirements, [Vec::new()]);
}

#[test]
fn without_env_params_an_amount_that_resolves_to_no_bound_is_an_error() {
    let src = jt(
        "EXPR, FEATURE_BUNDLE_1",
        &format!(
            "steps:\n- name: S\n  hostRequirements:\n    amounts: [{{name: amount.custom, min: \
             \"{{{{ None }}}}\"}}]\n  {RUN}\n"
        ),
    );
    let jt = openjd_model::decode_job_template(
        crate::env_params_common::yaml(&src),
        Some(&["EXPR", "FEATURE_BUNDLE_1"]),
        &openjd_model::CallerLimits::default(),
    )
    .unwrap();
    let err = create(&jt, &[], &[]).unwrap_err();
    assert_errors(
        &err.to_string(),
        "JobTemplate",
        &[
            "steps[0] -> hostRequirements -> amounts[0]:\n\tmust have at least one of min or max \
           after resolution.",
        ],
    );
}

#[test]
fn an_attribute_whose_expressions_resolve_to_null_is_discarded() {
    // JB-135, on a Step and on a Job Environment.
    let hr = "{attributes: [{name: custom:attr.a, anyOf: \"{{ [Param.G] if Param.G != '' else \
              None }}\"}, {name: custom:attr.b, anyOf: \"{{ None }}\", allOf: \"{{ ['v'] if \
              Param.G != '' else None }}\"}]}";
    let c = created(&step_hr(hr), &[]);
    assert_eq!(c.job.steps[0].host_requirements, None);
    assert_eq!(
        c.discarded_host_requirements,
        [vec![
            discarded(None, HostRequirementKind::Attribute, "custom:attr.a"),
            discarded(None, HostRequirementKind::Attribute, "custom:attr.b"),
        ]]
    );
    // One field left keeps the attribute, with only that field.
    let c = created(&step_hr(hr), &[("G", "g")]);
    assert_eq!(
        c.job.steps[0]
            .host_requirements
            .as_ref()
            .unwrap()
            .attributes,
        Some(vec![
            attr("custom:attr.a", Some(&["g"]), None),
            attr("custom:attr.b", None, Some(&["v"])),
        ])
    );
    let c = created(&env_hr(&hr.replace("Param.G", "Env.Param.P")), &[]);
    assert_eq!(c.job.steps[0].host_requirements, None);
    assert_eq!(
        c.discarded_host_requirements,
        [vec![
            discarded(Some("E"), HostRequirementKind::Attribute, "custom:attr.a"),
            discarded(Some("E"), HostRequirementKind::Attribute, "custom:attr.b"),
        ]]
    );
}

#[test]
fn a_discarded_requirement_is_not_a_duplicate() {
    // JB-138, and the same for attributes; the duplicate that is left is
    // still an error.
    let src = step_hr(
        "{amounts: [{name: amount.custom, min: 1}, {name: amount.custom, min: \"{{ 2 if \
         Param.G != '' else None }}\"}], attributes: [{name: custom:attr.a, anyOf: [x]}, {name: \
         custom:attr.a, anyOf: \"{{ [Param.G] if Param.G != '' else None }}\"}]}",
    );
    let c = created(&src, &[]);
    assert_eq!(
        c.job.steps[0].host_requirements,
        Some(HostRequirements {
            amounts: Some(vec![amount("amount.custom", Some(1.0), None)]),
            attributes: Some(vec![attr("custom:attr.a", Some(&["x"]), None)]),
        })
    );
    create_err(
        &src,
        &[("G", "g")],
        "JobTemplate",
        &["steps[0] -> hostRequirements -> amounts[1]:\n\tduplicate amount name 'amount.custom'."],
    );
}

#[test]
fn a_list_whose_items_all_resolve_to_null_is_an_error() {
    // JB-133: the attribute is not discarded.
    create_err_msg(
        &step_hr(
            "{attributes: [{name: custom:attr.flavor, anyOf: [\"{{ Param.G if Param.G != '' else \
             None }}\"]}]}",
        ),
        &[],
        "Validation error: steps[0] -> hostRequirements -> attributes[0] -> anyOf: attribute \
         'custom:attr.flavor' of Step 'S' has no anyOf values after resolution. An empty list \
         is not allowed (Template Schemas §3.3.2); to have no requirement, write the field as \
         one expression that resolves to null",
    );
}

#[test]
fn an_expression_that_resolves_to_an_empty_list_is_an_error() {
    // JB-136, on a Step and on an Environment.
    create_err_msg(
        &step_hr(
            "{attributes: [{name: custom:attr.flavor, anyOf: \"{{ [f for f in [Param.G] if f != \
             ''] }}\"}]}",
        ),
        &[],
        "Validation error: steps[0] -> hostRequirements -> attributes[0] -> anyOf: attribute \
         'custom:attr.flavor' of Step 'S' has no anyOf values after resolution. An empty list \
         is not allowed (Template Schemas §3.3.2); an expression that resolves to null omits \
         the field instead",
    );
    create_err(
        &env_hr(
            "{attributes: [{name: custom:attr.flavor, allOf: \"{{ [f for f in [Env.Param.P] if f \
             != ''] }}\"}]}",
        ),
        &[],
        "JobTemplate",
        &[
            "jobEnvironments[0] -> hostRequirements -> attributes[0] -> allOf:\n\t\
           attribute 'custom:attr.flavor' of Environment 'E' has no allOf values after \
           resolution. An empty list is not allowed (Template Schemas §3.3.2); an expression \
           that resolves to null omits the field instead. Resolved for Step 'S'.",
        ],
    );
}

#[test]
fn an_expressions_list_result_is_checked_as_the_list_form_is() {
    let src = |field: &str, name: &str| {
        step_hr(&format!(
            "{{attributes: [{{name: {name}, {field}: \"{{{{ Param.G.split(',') if Param.G != '' \
             else None }}}}\"}}]}}"
        ))
    };
    let c = created(&src("allOf", "custom:attr.a"), &[("G", "a,x")]);
    assert_eq!(
        c.job.steps[0]
            .host_requirements
            .as_ref()
            .unwrap()
            .attributes,
        Some(vec![attr("custom:attr.a", None, Some(&["a", "x"]))])
    );
    // Exactly 50 values are allowed.
    let fifty: Vec<String> = (0..50).map(|i| format!("v{i}")).collect();
    let c = created(&src("anyOf", "custom:attr.a"), &[("G", &fifty.join(","))]);
    assert_eq!(
        c.job.steps[0]
            .host_requirements
            .as_ref()
            .unwrap()
            .attributes
            .as_ref()
            .unwrap()[0]
            .any_of
            .as_ref()
            .map(Vec::len),
        Some(50)
    );
    let fifty_one: Vec<String> = (0..51).map(|i| format!("v{i}")).collect();
    for (field, name, value, expected) in [
        (
            // The field has no elements in the template, so the error is
            // at the field, naming the element, as template validation does.
            "anyOf",
            "custom:attr.a",
            "ok,9x",
            "Model validation error: 1 validation error for JobTemplate\n\
             steps[0] -> hostRequirements -> attributes[0] -> anyOf:\n\tlist element 1: value \
             '9x' contains invalid characters.",
        ),
        (
            "anyOf",
            "attr.worker.os.family",
            "linux,beos",
            "Model validation error: 1 validation error for JobTemplate\n\
             steps[0] -> hostRequirements -> attributes[0] -> anyOf:\n\tlist element 1: value \
             'beos' is not valid for attr.worker.os.family.",
        ),
        (
            "allOf",
            "attr.worker.os.family",
            "linux,windows",
            "Validation error: steps[0] -> hostRequirements -> attributes[0] -> allOf: \
             single-valued attribute cannot have more than 1 element after resolution",
        ),
        (
            "anyOf",
            "custom:attr.a",
            &fifty_one.join(","),
            "Validation error: steps[0] -> hostRequirements -> attributes[0] -> anyOf: attribute \
             'custom:attr.a' of Step 'S' has 51 anyOf values after resolution, exceeding the \
             maximum of 50 (Template Schemas §3.3.2)",
        ),
    ] {
        create_err_msg(&src(field, name), &[("G", value)], expected);
    }
}

#[test]
fn an_expression_combines_with_an_environments_requirements() {
    // JB-137.
    let c = created(
        &jt(
            "ENV_PARAMS, EXPR",
            &format!(
                "parameterDefinitions:\n- {{name: F, type: STRING, default: a}}\n\
                 jobEnvironments:\n- name: Hw\n  activation: ALWAYS\n  {VARS}\n  hostRequirements:\n    \
                 attributes: [{{name: custom:attr.flavor, allOf: [b]}}]\n\
                 steps:\n- name: S\n  hostRequirements:\n    attributes: [{{name: \
                 custom:attr.flavor, allOf: \"{{{{ [Param.F, 'x'] }}}}\"}}]\n  {RUN}\n"
            ),
        ),
        &[],
    );
    assert_eq!(
        c.job.steps[0].host_requirements,
        Some(HostRequirements {
            amounts: None,
            attributes: Some(vec![attr(
                "custom:attr.flavor",
                None,
                Some(&["a", "x", "b"])
            )]),
        })
    );
}

#[test]
fn discarded_requirements_take_no_part_in_combining() {
    // The Environment's amount is discarded, so the Step keeps its own
    // max alone; and a Step whose own requirements are all discarded takes
    // the Environment's.
    let c = created(
        &jt(
            "ENV_PARAMS, EXPR, FEATURE_BUNDLE_1",
            &format!(
                "parameterDefinitions:\n- {{name: G, type: STRING, default: \"\"}}\n\
                 jobEnvironments:\n- name: E\n  activation: ALWAYS\n  {VARS}\n  hostRequirements:\n    \
                 amounts: [{{name: amount.worker.gpu, min: \"{{{{ 9 if Param.G != '' else None }}}}\"}}]\n    \
                 attributes: [{{name: attr.worker.os.family, anyOf: [linux]}}]\n\
                 steps:\n- name: S\n  hostRequirements:\n    amounts: [{{name: amount.worker.gpu, max: 4}}, \
                 {{name: amount.custom, max: \"{{{{ 1 if Param.G != '' else None }}}}\"}}]\n  {RUN}\n\
                 - name: T\n  hostRequirements:\n    amounts: [{{name: amount.custom, max: \"{{{{ 1 if \
                 Param.G != '' else None }}}}\"}}]\n  {RUN}\n"
            ),
        ),
        &[],
    );
    assert_eq!(
        c.job.steps[0].host_requirements,
        Some(HostRequirements {
            amounts: Some(vec![amount("amount.worker.gpu", None, Some(4.0))]),
            attributes: Some(vec![attr("attr.worker.os.family", Some(&["linux"]), None)]),
        })
    );
    assert_eq!(
        c.job.steps[1].host_requirements,
        Some(HostRequirements {
            amounts: None,
            attributes: Some(vec![attr("attr.worker.os.family", Some(&["linux"]), None)]),
        })
    );
    // The Step's own first, then the Environment's.
    assert_eq!(
        c.discarded_host_requirements,
        [
            vec![
                discarded(None, HostRequirementKind::Amount, "amount.custom"),
                discarded(Some("E"), HostRequirementKind::Amount, "amount.worker.gpu"),
            ],
            vec![
                discarded(None, HostRequirementKind::Amount, "amount.custom"),
                discarded(Some("E"), HostRequirementKind::Amount, "amount.worker.gpu"),
            ],
        ]
    );
}

#[test]
fn an_attached_environments_requirements_are_discarded_and_checked() {
    // JB-134, and `check_environment_templates` with the expression form
    // and an empty object.
    let jt_src = jt(
        "ENV_PARAMS, EXPR",
        &format!("steps:\n- name: S\n  dependencies:\n  - environment: Gpu\n  {RUN}\n"),
    );
    let et = |hr: &str| {
        env_ok(&format!(
            "specificationVersion: environment-2023-09\nextensions: [ENV_PARAMS, EXPR, \
             FEATURE_BUNDLE_1]\nenvironment:\n  name: Gpu\n  activation: ON_REQUEST\n  {VARS}\n  \
             envParameterDefinitions:\n  - {{name: Gpus, type: STRING, default: \"\"}}\n  \
             hostRequirements: {hr}\n"
        ))
    };
    let t = job_ok(&jt_src);
    for hr in [
        "{amounts: [{name: amount.worker.gpu, min: \"{{ 1 if Env.Param.Gpus != '' else None }}\"}]}",
        "{attributes: [{name: custom:attr.gpu, anyOf: \"{{ [Env.Param.Gpus] if Env.Param.Gpus \
         != '' else None }}\"}]}",
        "{}",
    ] {
        let envs = [et(hr)];
        let c = create(&t, &envs, &[]).unwrap_or_else(|e| panic!("{hr}:\n{e}"));
        assert_eq!(c.job.steps[0].host_requirements, None, "{hr}");
        let attached: Vec<_> = envs.iter().map(AttachedEnvironmentTemplate::new).collect();
        check_environment_templates(&t, &attached, &posix_ctx(&t))
            .unwrap_or_else(|e| panic!("{hr}:\n{e}"));
    }
    let envs = [et(
        "{amounts: [{name: amount.worker.gpu, min: \"{{ 1 if Env.Param.Gpus != '' else None }}\"}]}",
    )];
    let c = create(&t, &envs, &[]).unwrap();
    assert_eq!(
        c.discarded_host_requirements,
        [vec![DiscardedHostRequirement {
            environment: Some(EnvironmentRef {
                document: Document::EnvironmentTemplate {
                    index: 0,
                    label: None,
                },
                environment: "Gpu".to_string(),
            }),
            kind: HostRequirementKind::Amount,
            name: "amount.worker.gpu".to_string(),
        }]]
    );
}

// ── The template types' helpers and decode errors ─────────────────────

#[test]
fn may_be_discarded_follows_the_written_form() {
    let t = job_ok(&step_hr(
        "{amounts: [{name: amount.a, min: 1}, {name: amount.b, min: \"{{ None }}\"}, {name: \
         amount.c, min: 1, max: \"{{ None }}\"}, {name: amount.d, min: \"{{ None }}\", max: \
         \"{{ None }}\"}], attributes: [{name: custom:attr.a, anyOf: [x]}, {name: custom:attr.b, \
         anyOf: \"{{ None }}\"}, {name: custom:attr.c, anyOf: \"{{ None }}\", allOf: [y]}, {name: \
         custom:attr.d, anyOf: \"{{ None }}\", allOf: \"{{ None }}\"}]}",
    ));
    let hr = t.steps[0].host_requirements.as_ref().unwrap();
    let amounts: Vec<bool> = hr
        .amounts
        .iter()
        .flatten()
        .map(|a| a.may_be_discarded())
        .collect();
    assert_eq!(amounts, [false, true, false, true]);
    let attrs: Vec<bool> = hr
        .attributes
        .iter()
        .flatten()
        .map(|a| a.may_be_discarded())
        .collect();
    assert_eq!(attrs, [false, true, false, true]);
    let a = &hr.attributes.as_ref().unwrap()[0];
    assert!(a.any_of.as_ref().unwrap().is_literal());
    assert_eq!(
        a.any_of.as_ref().unwrap().as_list().map(<[_]>::len),
        Some(1)
    );
    let b = &hr.attributes.as_ref().unwrap()[1];
    assert!(!b.any_of.as_ref().unwrap().is_literal());
    assert!(b.any_of.as_ref().unwrap().as_list().is_none());
}

#[test]
fn an_any_of_that_is_neither_a_list_nor_a_string_is_a_decode_error() {
    for (value, kind) in [
        ("5", "integer `5`"),
        ("1.5", "floating point `1.5`"),
        ("true", "boolean `true`"),
        ("{a: 1}", "map"),
    ] {
        let err = openjd_model::decode_job_template(
            crate::env_params_common::yaml(&step_hr(&format!(
                "{{attributes: [{{name: custom:attr.a, anyOf: {value}}}]}}"
            ))),
            Some(crate::env_params_common::ALLOW),
            &openjd_model::CallerLimits::default(),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "Validation error: 'jobtemplate-2023-09' failed checks: invalid type: {kind}, \
                 expected a list of values, or (with ENV_PARAMS) a string holding one {{{{ }}}} \
                 expression"
            )
        );
    }
}
