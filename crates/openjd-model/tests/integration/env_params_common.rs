// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Helpers shared by the `ENV_PARAMS` integration tests.
//!
//! Every failure assertion checks the error count and each error's full
//! `path:\n\tmessage` text, per AGENTS.md "Test Quality Standard": an
//! expectation must equal one whole error, from the start of its path to
//! the end of its message, and each error is matched once.

use openjd_model::template::parse::{document_string_to_object, DocumentType};
use openjd_model::template::{EnvironmentTemplate, JobTemplate};
use openjd_model::{decode_environment_template, decode_job_template, CallerLimits};

/// The extension allowlist the tests decode with, named explicitly so
/// the tests do not change when `ModelExtension::ALL` grows.
pub const ALLOW: &[&str] = &[
    "ENV_PARAMS",
    "EXPR",
    "WRAP_ACTIONS",
    "FEATURE_BUNDLE_1",
    "TASK_CHUNKING",
];

/// Parse YAML the way `openjd check` does (strict booleans, so a key such
/// as `N` stays a string).
pub fn yaml(s: &str) -> serde_json::Value {
    document_string_to_object(s, DocumentType::Yaml, &CallerLimits::default()).unwrap()
}

pub fn job_ok(s: &str) -> JobTemplate {
    decode_job_template(yaml(s), Some(ALLOW), &CallerLimits::default())
        .unwrap_or_else(|e| panic!("expected a valid Job Template, got:\n{e}"))
}

pub fn env_ok(s: &str) -> EnvironmentTemplate {
    decode_environment_template(yaml(s), Some(ALLOW), &CallerLimits::default())
        .unwrap_or_else(|e| panic!("expected a valid Environment Template, got:\n{e}"))
}

pub fn assert_errors(msg: &str, model: &str, expected: &[&str]) {
    let n = expected.len();
    // Anchored on the ": " that precedes the count, so that an expected 2
    // does not match "12 validation errors".
    let header = format!(
        ": {n} validation error{} for {model}",
        if n == 1 { "" } else { "s" }
    );
    let Some((first, rest)) = msg.split_once('\n') else {
        panic!("expected {header:?} and {n} errors in error output:\n{msg}");
    };
    assert!(
        first.ends_with(&header),
        "expected {header:?} in error output:\n{msg}"
    );
    // Split the errors apart: each starts on a line that does not begin
    // with whitespace (`path:` or `Model: message`); the `\t` message line
    // and an interpolation error's indented expression and caret lines
    // continue it.
    let mut blocks: Vec<String> = Vec::new();
    for line in rest.split('\n') {
        match blocks.last_mut() {
            Some(b) if line.starts_with(char::is_whitespace) => {
                b.push('\n');
                b.push_str(line);
            }
            _ => blocks.push(line.to_string()),
        }
    }
    // Each expectation must equal one whole error, used once, so a
    // truncated path or message, or a repeated expectation, fails. An
    // interpolation error's indented expression and caret lines may be
    // left out of the expectation.
    let mut unmatched: Vec<Option<String>> = blocks.into_iter().map(Some).collect();
    for line in expected {
        let found = unmatched.iter_mut().find(|b| {
            b.as_deref().is_some_and(|b| {
                b == *line
                    || b.strip_prefix(*line)
                        .is_some_and(|tail| tail.starts_with("\n  "))
            })
        });
        match found {
            Some(slot) => *slot = None,
            None => panic!("missing {line:?} in error output:\n{msg}"),
        }
    }
    let extra: Vec<String> = unmatched.into_iter().flatten().collect();
    assert!(
        extra.is_empty(),
        "unexpected errors {extra:?} in error output:\n{msg}"
    );
}

/// Decode a Job Template that must fail with exactly `expected` errors.
pub fn job_err(s: &str, expected: &[&str]) {
    job_err_with(s, ALLOW, expected);
}

/// [`job_err`] with an explicit extension allowlist.
pub fn job_err_with(s: &str, allow: &[&str], expected: &[&str]) {
    let err = decode_job_template(yaml(s), Some(allow), &CallerLimits::default())
        .expect_err("expected a validation error");
    assert_errors(&err.to_string(), "JobTemplate", expected);
}

/// Decode an Environment Template that must fail with exactly `expected`
/// errors.
pub fn env_err(s: &str, expected: &[&str]) {
    let err = decode_environment_template(yaml(s), Some(ALLOW), &CallerLimits::default())
        .expect_err("expected a validation error");
    assert_errors(&err.to_string(), "EnvironmentTemplate", expected);
}

/// Merged Job Parameter values for `jt` from `params`, as `openjd run`
/// would compute them with no attached Environment Templates.
pub fn job_params(jt: &JobTemplate, params: &[(&str, &str)]) -> openjd_model::JobParameterValues {
    job_params_with(jt, params, &[])
}

/// [`job_params`] with attached Environment Templates merged in.
pub fn job_params_with(
    jt: &JobTemplate,
    params: &[(&str, &str)],
    envs: &[EnvironmentTemplate],
) -> openjd_model::JobParameterValues {
    let input: openjd_model::JobParameterInputValues = params
        .iter()
        .map(|(k, v)| (k.to_string(), openjd_expr::ExprValue::String(v.to_string())))
        .collect();
    let mut opts = openjd_model::PathParameterOptions::new("/templates", "/work");
    opts.path_format = openjd_expr::path_mapping::PathFormat::Posix;
    openjd_model::preprocess_job_parameters(jt, &input, envs, &opts)
        .unwrap_or_else(|e| panic!("preprocess_job_parameters failed:\n{e}"))
}

/// The context the job-creation tests use: the template's own profile,
/// with a POSIX submission path format so results do not depend on the
/// host.
pub fn posix_ctx(jt: &JobTemplate) -> openjd_model::ValidationContext {
    jt.default_validation_context()
        .with_submission_path_format(openjd_expr::path_mapping::PathFormat::Posix)
}

/// A Step's effective Environment parameter values, one line per
/// Environment: `document/environment: name=type:value, ...`, where a
/// list value is its transport JSON.
pub fn rendered_values(step: &openjd_model::job::Step) -> Vec<String> {
    step.environment_parameters
        .iter()
        .flatten()
        .map(|v| {
            let entries: Vec<String> = v
                .values
                .as_value()
                .as_array()
                .unwrap()
                .iter()
                .map(|e| {
                    let value = match &e["value"] {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    format!(
                        "{}={}:{}",
                        e["name"].as_str().unwrap(),
                        e["type"].as_str().unwrap(),
                        value
                    )
                })
                .collect();
            format!("{}/{}: {}", v.document, v.environment, entries.join(", "))
        })
        .collect()
}

#[cfg(test)]
mod assert_errors_self_tests {
    use super::assert_errors;

    const MSG: &str = "Model validation error: 2 validation errors for JobTemplate\n\
                       steps[0] -> dependencies[0] -> parameters -> N:\n\tbad value.\n\
                       steps[1]:\n\tFailed to parse.\n  Param.X\n  ~~~~~~^";

    #[test]
    fn whole_errors_match() {
        assert_errors(
            MSG,
            "JobTemplate",
            &[
                "steps[1]:\n\tFailed to parse.\n  Param.X\n  ~~~~~~^",
                "steps[0] -> dependencies[0] -> parameters -> N:\n\tbad value.",
            ],
        );
        // The expression and caret lines may be left out.
        assert_errors(
            MSG,
            "JobTemplate",
            &[
                "steps[0] -> dependencies[0] -> parameters -> N:\n\tbad value.",
                "steps[1]:\n\tFailed to parse.",
            ],
        );
    }

    #[test]
    #[should_panic(expected = "expected \": 1 validation error for JobTemplate\"")]
    fn the_count_is_anchored() {
        assert_errors(MSG.replace(": 2 ", ": 12 ").as_str(), "JobTemplate", &["x"]);
    }

    #[test]
    #[should_panic(expected = "expected \": 2 validation errors for JobTemplate\"")]
    fn the_count_is_anchored_at_the_colon() {
        // "12 validation errors" ends with "2 validation errors".
        assert_errors(
            MSG.replace(": 2 ", ": 12 ").as_str(),
            "JobTemplate",
            &[
                "steps[1]:\n\tFailed to parse.",
                "steps[0] -> dependencies[0] -> parameters -> N:\n\tbad value.",
            ],
        );
    }

    #[test]
    #[should_panic(expected = "missing")]
    fn a_path_without_its_message_fails() {
        assert_errors(
            MSG,
            "JobTemplate",
            &[
                "steps[0] -> dependencies[0] -> parameters -> N:",
                "steps[1]:",
            ],
        );
    }

    #[test]
    #[should_panic(expected = "missing")]
    fn a_truncated_path_fails() {
        assert_errors(
            MSG,
            "JobTemplate",
            &[
                "parameters -> N:\n\tbad value.",
                "steps[1]:\n\tFailed to parse.",
            ],
        );
    }

    #[test]
    #[should_panic(expected = "missing")]
    fn a_truncated_message_fails() {
        assert_errors(
            MSG,
            "JobTemplate",
            &[
                "steps[0] -> dependencies[0] -> parameters -> N:\n\tbad",
                "steps[1]:\n\tFailed to parse.",
            ],
        );
    }

    #[test]
    #[should_panic(expected = "missing")]
    fn trailing_text_fails() {
        assert_errors(
            &MSG.replace("bad value.", "bad value. More."),
            "JobTemplate",
            &[
                "steps[0] -> dependencies[0] -> parameters -> N:\n\tbad value.",
                "steps[1]:\n\tFailed to parse.",
            ],
        );
    }

    #[test]
    #[should_panic(expected = "unexpected errors")]
    fn an_error_beyond_the_expected_ones_fails() {
        // The header says 1, but the output holds two errors.
        assert_errors(
            &MSG.replace(": 2 validation errors", ": 1 validation error"),
            "JobTemplate",
            &["steps[1]:\n\tFailed to parse."],
        );
    }

    #[test]
    #[should_panic(expected = "missing")]
    fn a_repeated_expectation_cannot_hide_an_error() {
        assert_errors(
            MSG,
            "JobTemplate",
            &[
                "steps[1]:\n\tFailed to parse.",
                "steps[1]:\n\tFailed to parse.",
            ],
        );
    }
}
