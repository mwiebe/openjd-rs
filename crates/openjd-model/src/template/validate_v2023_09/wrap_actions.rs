// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Validate or reject `WRAP_ACTIONS` features (RFC 0008).
//!
//! Three fields are gated by the `WRAP_ACTIONS` extension:
//! - `onWrapEnvEnter` on `<EnvironmentActions>`
//! - `onWrapTaskRun` on `<EnvironmentActions>`
//! - `onWrapEnvExit` on `<EnvironmentActions>`
//!
//! When the extension is not enabled, using any of these fields is a
//! validation error. When it is enabled, we additionally enforce the
//! constraints from RFC 0008:
//!
//! - **All-or-nothing.** An environment that defines any of the three
//!   wrap hooks must define all three.
//! - **Single-layer.** At most one environment in the session stack
//!   (the job environments and each step's step environments) may define
//!   any wrap hook. It is reported with [`report_wrap_layers`], once for
//!   Steps with the same layers.
//! - **EXPR prerequisite.** A template that lists `WRAP_ACTIONS` in
//!   `extensions:` must also list `EXPR`.

use crate::error::{path_field, path_index, PathElement, ValidationErrors};
use crate::job::create_job::{steps_phrase, ByValues};
use crate::template::actions::EnvironmentActions;
use crate::template::{Environment, EnvironmentTemplate, JobTemplate};
use crate::types::{ModelExtension, ValidationContext};

/// Check one environment's `<EnvironmentActions>` for wrap hook usage.
///
/// Reports every offending field individually so users see a complete list
/// rather than having to fix them one at a time. Also enforces the
/// all-or-nothing rule when the extension is enabled.
fn check_environment_actions(
    actions: &EnvironmentActions,
    actions_path: &[PathElement],
    active: bool,
    errors: &mut ValidationErrors,
) {
    let wrap_hooks = actions.wrap_hooks();
    for (name, slot, _) in wrap_hooks {
        if slot.is_some() && !active {
            errors.add(
                &path_field(actions_path, name),
                format!("{name} requires the WRAP_ACTIONS extension."),
            );
        }
    }

    // All-or-nothing rule (RFC 0008): an env that defines any wrap hook
    // must define all three. Only enforced when the extension is active —
    // otherwise the per-hook errors above already cover it.
    if active {
        let defined = wrap_hooks
            .iter()
            .filter(|(_, slot, _)| slot.is_some())
            .count();
        if defined > 0 && defined < wrap_hooks.len() {
            errors.add(
                actions_path,
                "an environment that defines any of onWrapEnvEnter, onWrapTaskRun, or onWrapEnvExit must define all three (RFC 0008).",
            );
        }
    }
}

/// Walk one environment for WRAP_ACTIONS gating.
fn check_env(env: &Environment, path: &[PathElement], active: bool, errors: &mut ValidationErrors) {
    if let Some(script) = &env.script {
        let actions_path = path_field(&path_field(path, "script"), "actions");
        check_environment_actions(&script.actions, &actions_path, active, errors);
    }
}

/// Enforce the EXPR prerequisite: when `WRAP_ACTIONS` is listed in a
/// template's `extensions:`, `EXPR` must also be listed (RFC 0008).
fn check_expr_prerequisite(ctx: &ValidationContext, errors: &mut ValidationErrors) {
    let has_wrap = ctx.profile.has_extension(ModelExtension::WrapActions);
    let has_expr = ctx.profile.has_extension(ModelExtension::Expr);
    if has_wrap && !has_expr {
        errors.add(
            &path_field(&[], "extensions"),
            "WRAP_ACTIONS requires EXPR; both must be listed in the template's `extensions` (RFC 0008).",
        );
    }
}

/// Validate RFC 0008 constraints for a job template.
///
/// This runs regardless of whether `WRAP_ACTIONS` is enabled:
/// - When disabled, it rejects templates that attempt to use any of the
///   new fields.
/// - When enabled, it additionally enforces the EXPR prerequisite, the
///   all-or-nothing rule, and the single-wrap-layer rule per session.
pub fn validate_wrap_actions_job_template(
    jt: &JobTemplate,
    ctx: &ValidationContext,
    errors: &mut ValidationErrors,
) {
    let active = ctx.profile.has_extension(ModelExtension::WrapActions);
    check_expr_prerequisite(ctx, errors);

    // Single-wrap-layer enforcement (RFC 0008). A Session enters the Job
    // Environments and one Step's Step Environments, so each Step's layers
    // are the Job Environments and its own Step Environments that define a
    // wrap hook.
    let job_envs = jt.job_environments.as_deref().unwrap_or(&[]);
    for (k, env) in job_envs.iter().enumerate() {
        let path = path_index(&path_field(&[], "jobEnvironments"), k);
        check_env(env, &path, active, errors);
    }
    // Every Step's Sessions enter the Job Environments, so their layers are
    // named once, and each Step adds only its own Step Environments'.
    let job_layers: Vec<String> = job_envs
        .iter()
        .filter(|env| env.defines_wrap_hooks())
        .map(|env| format!("'{}'", env.name))
        .collect();
    let mut steps = Vec::new();
    for (i, step) in jt.steps.iter().enumerate() {
        let base = path_index(&path_field(&[], "steps"), i);
        let envs_path = path_field(&base, "stepEnvironments");
        for (j, env) in step.step_environments.iter().flatten().enumerate() {
            check_env(env, &path_index(&envs_path, j), active, errors);
        }
        if !active {
            continue;
        }
        let own: Vec<String> = step
            .step_environments
            .iter()
            .flatten()
            .filter(|env| env.defines_wrap_hooks())
            .map(|env| format!("'{}' (stepEnvironments)", env.name))
            .collect();
        steps.push((i, step.name.as_str(), own));
    }
    report_wrap_layers(&job_layers, steps, |i, message| {
        errors.add(&path_index(&path_field(&[], "steps"), i), message);
    });
}

/// The `WRAP_ACTIONS` single-layer rule (RFC 0008) over `steps`. Each Step
/// enters the `shared` layers (named once for all of them), then its own:
/// each Step's index, name and own layers. A layer names an Environment
/// that defines a wrap hook, in the order a Session enters them (the Job
/// Environments, then the Step's `stepEnvironments`). Two or more in all is
/// an error. Steps with the same own layers get one, naming them, which
/// goes to `report` with the index of the first. Memory is bounded by the
/// names given, not by the Steps times the shared layers: Steps are grouped
/// by their own layers, and a message lists at most what
/// [`capped_name_list`](super::helpers::capped_name_list) shows.
pub(crate) fn report_wrap_layers<'s>(
    shared: &[String],
    steps: impl IntoIterator<Item = (usize, &'s str, Vec<String>)>,
    mut report: impl FnMut(usize, String),
) {
    let mut groups: ByValues<(usize, Vec<String>), &str> = ByValues::default();
    for (index, name, own) in steps {
        if shared.len() + own.len() > 1 {
            groups.add(format!("{own:?}"), name, || (index, own));
        }
    }
    for ((first, own), names) in groups.groups() {
        let whose = match names.as_slice() {
            [_] => "its Sessions".to_string(),
            _ => format!("the Sessions of {}", steps_phrase(names)),
        };
        let total = shared.len() + own.len();
        let layers = shared.iter().chain(own).map(String::as_str);
        report(
            *first,
            format!(
                "{whose} would enter {total} Environments that define wrap hooks (onWrapEnvEnter, \
                 onWrapTaskRun, onWrapEnvExit): {}. A Session may have at most one Environment \
                 defining wrap hooks (RFC 0008).",
                super::helpers::capped_name_list(layers, total)
            ),
        );
    }
}

/// Validate RFC 0008 constraints for an environment template.
///
/// An environment template defines one environment, so the single-layer
/// rule is trivially satisfied. We gate the new fields on the extension
/// being enabled, enforce the EXPR prerequisite, and enforce the
/// all-or-nothing rule via `check_env`.
pub fn validate_wrap_actions_environment_template(
    et: &EnvironmentTemplate,
    ctx: &ValidationContext,
    errors: &mut ValidationErrors,
) {
    let active = ctx.profile.has_extension(ModelExtension::WrapActions);
    check_expr_prerequisite(ctx, errors);
    let env_path = path_field(&[], "environment");
    check_env(&et.environment, &env_path, active, errors);
}

#[cfg(test)]
mod tests {
    //! Integration tests in `tests/integration/test_wrap_actions.rs`
    //! exercise the full decode + validate pipeline against real templates.
    use super::report_wrap_layers;

    /// The errors `report_wrap_layers` gives for `steps`, each with the
    /// index of the Step it is reported at.
    fn reported(steps: Vec<(usize, &str, Vec<String>)>) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        report_wrap_layers(&[], steps, |i, m| out.push((i, m)));
        out
    }

    fn layers(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("'E{i}'")).collect()
    }

    #[test]
    fn two_or_more_layers_are_an_error_naming_them() {
        assert_eq!(reported(vec![(0, "A", layers(0)), (1, "B", layers(1))]), []);
        assert_eq!(
            reported(vec![(3, "A", layers(2))]),
            [(
                3,
                "its Sessions would enter 2 Environments that define wrap hooks (onWrapEnvEnter, \
                 onWrapTaskRun, onWrapEnvExit): 'E0', 'E1'. A Session may have at most one \
                 Environment defining wrap hooks (RFC 0008)."
                    .to_string()
            )]
        );
        // The list is capped.
        let many = reported(vec![(0, "A", layers(60))]).remove(0).1;
        assert!(many.starts_with("its Sessions would enter 60 Environments"));
        assert!(many.contains("and 10 more"), "{many}");
    }

    /// Steps with the same layers get one error naming them, at the first;
    /// different layers get their own, in the order of their first Step.
    #[test]
    fn steps_with_the_same_layers_get_one_error() {
        let other = vec!["'E1'".to_string(), "'E0'".to_string()];
        let got = reported(vec![
            (0, "A", layers(2)),
            (1, "B", other.clone()),
            (2, "C", layers(1)),
            (3, "D", layers(2)),
            (4, "E", layers(2)),
        ]);
        let tail = "Environments that define wrap hooks (onWrapEnvEnter, onWrapTaskRun, \
                    onWrapEnvExit)";
        assert_eq!(
            got,
            [
                (
                    0,
                    format!(
                        "the Sessions of Steps 'A', 'D', 'E' would enter 2 {tail}: 'E0', 'E1'. \
                         A Session may have at most one Environment defining wrap hooks \
                         (RFC 0008)."
                    )
                ),
                (
                    1,
                    format!(
                        "its Sessions would enter 2 {tail}: 'E1', 'E0'. A Session may have at \
                         most one Environment defining wrap hooks (RFC 0008)."
                    )
                ),
            ]
        );
        // Two Steps are named with "and"; many are capped.
        let two = reported(vec![(0, "A", layers(2)), (1, "B", layers(2))]);
        assert!(
            two[0]
                .1
                .starts_with("the Sessions of Steps 'A' and 'B' would"),
            "{two:?}"
        );
        let names: Vec<String> = (0..1000).map(|i| format!("S{i}")).collect();
        let many = reported(
            names
                .iter()
                .enumerate()
                .map(|(i, n)| (i, n.as_str(), layers(2)))
                .collect(),
        );
        assert_eq!(many.len(), 1);
        assert!(
            many[0].1.contains("'S49', and 950 more would enter 2"),
            "{}",
            many[0].1
        );
    }

    /// The shared layers come first and count for every Step; a Step's own
    /// layers follow them, and Steps are grouped by their own.
    #[test]
    fn shared_layers_count_for_every_step() {
        let shared = layers(2);
        let mut out = Vec::new();
        report_wrap_layers(
            &shared,
            vec![
                (0, "A", vec![]),
                (1, "B", vec!["'X' (stepEnvironments)".to_string()]),
                (2, "C", vec![]),
            ],
            |i, m| out.push((i, m)),
        );
        let tail = "Environments that define wrap hooks (onWrapEnvEnter, onWrapTaskRun, \
                    onWrapEnvExit)";
        let rule = "A Session may have at most one Environment defining wrap hooks (RFC 0008).";
        assert_eq!(
            out,
            [
                (
                    0,
                    format!("the Sessions of Steps 'A' and 'C' would enter 2 {tail}: 'E0', 'E1'. {rule}")
                ),
                (
                    1,
                    format!(
                        "its Sessions would enter 3 {tail}: 'E0', 'E1', 'X' (stepEnvironments). {rule}"
                    )
                ),
            ]
        );
        // One shared layer and none of a Step's own is no error.
        let mut out = Vec::new();
        report_wrap_layers(&layers(1), vec![(0, "A", vec![])], |i, m| out.push((i, m)));
        assert_eq!(out, []);
    }
}
