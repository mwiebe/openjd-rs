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
//!   (each step's Environment set: the job environments its sessions
//!   enter, by `activation` and its `environment:` entries, plus its step
//!   environments, less any whose `enableIf` is `false`) may define any wrap hook. Template validation
//!   counts only the Environments certainly entered; job creation checks
//!   the final set.
//! - **EXPR prerequisite.** A template that lists `WRAP_ACTIONS` in
//!   `extensions:` must also list `EXPR`.

use crate::error::{path_field, path_index, PathElement, ValidationErrors};
use crate::template::actions::EnvironmentActions;
use std::collections::HashSet;

use crate::template::{
    Environment, EnvironmentActivation, EnvironmentTemplate, JobTemplate, KnownEntry,
};
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

/// Walk one environment for WRAP_ACTIONS gating and return whether it
/// defined any wrap hook (used for the single-layer check upstream).
fn check_env(
    env: &Environment,
    path: &[PathElement],
    active: bool,
    errors: &mut ValidationErrors,
) -> bool {
    let Some(script) = &env.script else {
        return false;
    };
    let script_path = path_field(path, "script");
    let actions_path = path_field(&script_path, "actions");
    check_environment_actions(&script.actions, &actions_path, active, errors);
    script.actions.has_any_wrap_hook()
}

/// Enforce the EXPR prerequisite: when `WRAP_ACTIONS` is listed in a
/// template's `extensions:`, `EXPR` must also be listed (RFC 0008).
pub(super) fn check_expr_prerequisite(ctx: &ValidationContext, errors: &mut ValidationErrors) {
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
    known: &super::format_strings::KnownEnableIfs,
    errors: &mut ValidationErrors,
) {
    let active = ctx.profile.has_extension(ModelExtension::WrapActions);
    check_expr_prerequisite(ctx, errors);

    // Single-wrap-layer enforcement (RFC 0008).
    //
    // A Session's environment stack is its Step's Environment set: every
    // `jobEnvironments` entry with `activation: ALWAYS`, every one with
    // `BY_DEFAULT` that the Step does not decline, every one with
    // `activation: ON_REQUEST` that the Step requests (ENV_PARAMS,
    // Template Schemas §4 item 4, §3.2 item 3.2), and the Step's own
    // `stepEnvironments`. So "only one wrap layer per session" reduces to:
    // for every step, (wrap envs among the ALWAYS and BY_DEFAULT
    // jobEnvironments it enters) + (wrap envs among the
    // ON_REQUEST ones it requests) + (wrap envs in its
    // stepEnvironments) must be <= 1.
    //
    // Whether a Step requests or declines an Environment is known here only
    // for an entry whose `enableIf` is known (absent, a boolean scalar, or
    // an expression whose value is known, §7.6 rule 5); any other entry
    // neither requests an `ON_REQUEST` Environment nor certainly leaves a
    // `BY_DEFAULT` one entered, so neither is counted, and job creation
    // checks the Step's resolved set (§3.2, checked-at paragraph). A
    // `BY_DEFAULT` one an entry known to be disabled declines is not
    // entered. The same holds for an Environment's own `enableIf`
    // (ENV_PARAMS, §4, after item 10): an Environment whose `enableIf` is
    // known to be `true` is counted, one known to be `false` is not
    // entered, and any other is not counted here and is decided at job
    // creation.
    // An `ALWAYS` Environment is counted whatever its `enableIf`: it cannot
    // be disabled, so it is entered or the Job is rejected (§3.2.2 item 6).
    // So only a violation among Environments every relevant Session
    // certainly enters is reported here; it is one whatever the
    // expressions resolve to. This is the rule for every set-dependent
    // check made before job creation: fail only when the failure is
    // certain with what is known at this stage.
    //
    // `check_env` does double duty below: it gates the new fields on the
    // extension and returns true iff the env defines any wrap hook, which
    // is what we sum into the per-scope counts.

    // 1. The wrap-defining envs in jobEnvironments: the ALWAYS and
    //    BY_DEFAULT ones a Step may certainly enter, and the names of the
    //    ON_REQUEST ones only requesting Steps' Sessions may.
    let mut job_wrap_envs: Vec<(usize, &Environment)> = Vec::new();
    let mut requested_wrap_envs: HashSet<&str> = HashSet::new();
    if let Some(envs) = &jt.job_environments {
        let envs_path = path_field(&[], "jobEnvironments");
        for (i, env) in envs.iter().enumerate() {
            // `check_env` gates the hooks whatever the Environment's
            // `enableIf`; only one certainly entered is counted.
            // A grouped one is never certainly entered before the
            // Environment Templates are known: one of its group may come
            // first and supersede it (§4 item 7, and the paragraph after
            // item 10).
            if check_env(env, &path_index(&envs_path, i), active, errors) && env.group().is_none() {
                let enabled = known.job_environment(i);
                if env.activation() == EnvironmentActivation::OnRequest {
                    if env.certainly_entered(enabled, KnownEntry::Enabled) {
                        requested_wrap_envs.insert(env.name.as_str());
                    }
                } else if env.certainly_entered(enabled, KnownEntry::Absent) {
                    job_wrap_envs.push((i, env));
                }
            }
        }
    }
    // What each Step's entry for each of them is known to be: a disabled
    // entry declines a BY_DEFAULT one, and an expression entry may
    // (ENV_PARAMS, Template Schemas §3.2 item 3.2).
    let step_entered: Vec<Vec<bool>> = jt
        .steps
        .iter()
        .enumerate()
        .map(|(i, step)| {
            job_wrap_envs
                .iter()
                .map(|(k, env)| {
                    env.certainly_entered(
                        known.job_environment(*k),
                        known.entry_in_step(i, step, &env.name),
                    )
                })
                .collect()
        })
        .collect();
    // The ones every Step certainly enters.
    let every_step_count = (0..job_wrap_envs.len())
        .filter(|k| step_entered.iter().all(|entered| entered[*k]))
        .count();

    // Job-envs-only violation: multiple ALWAYS or BY_DEFAULT wrap envs in
    // jobEnvironments that every Step certainly enters are reachable from
    // every session, regardless of any step's stepEnvironments, so emit at
    // the jobEnvironments path as soon as the count exceeds one. This is
    // independent of the per-step check below: a template with two
    // job-env wrap layers AND a step that adds its own should report both
    // the jobEnvironments and stepEnvironments violations.
    if active && every_step_count > 1 {
        errors.add(&path_field(&[], "jobEnvironments"), SINGLE_WRAP_LAYER_MSG);
    }

    // 2. For each step, add the ON_REQUEST wrap envs it requests and
    //    its stepEnvironments' wrap envs to the ALWAYS and BY_DEFAULT
    //    ones it certainly enters — that sum is
    //    exactly the set of wrap envs certainly reachable in that step's
    //    session.
    for (i, step) in jt.steps.iter().enumerate() {
        let base = path_index(&path_field(&[], "steps"), i);
        let job_env_wrap_count = step_entered[i].iter().filter(|e| **e).count();
        let requested: HashSet<&str> = step
            .dependencies
            .iter()
            .flatten()
            .enumerate()
            .filter(|(j, _)| known.entry(i, *j) == Some(true))
            .filter_map(|(_, d)| d.environment())
            .filter(|name| requested_wrap_envs.contains(name))
            .collect();
        let Some(envs) = &step.step_environments else {
            // A step without stepEnvironments adds a layer only through
            // the ON_REQUEST envs it requests; report at its
            // dependencies, where the requests are. Its ALWAYS and
            // BY_DEFAULT layers alone are reported at the Step when
            // another Step declines one of them, so the jobEnvironments
            // report above does not cover it.
            if active && job_env_wrap_count + requested.len() > 1 {
                if !requested.is_empty() {
                    errors.add(&path_field(&base, "dependencies"), SINGLE_WRAP_LAYER_MSG);
                } else if every_step_count <= 1 {
                    errors.add(&base, SINGLE_WRAP_LAYER_MSG);
                }
            }
            continue;
        };
        let envs_path = path_field(&base, "stepEnvironments");
        let mut step_env_wrap_count = 0usize;
        for (j, env) in envs.iter().enumerate() {
            if check_env(env, &path_index(&envs_path, j), active, errors)
                && known.step_environment(i, j) == Some(true)
            {
                step_env_wrap_count += 1;
            }
        }
        // Single-wrap-layer rule: a session is built from the step's
        // Environment set, so checking each step's combined total catches
        // every reachable session. This catches two job-env wrap layers,
        // one job-env + one step-env, and two step-env layers within the
        // same step. It is reported at the dependencies when only the
        // requested ON_REQUEST envs add layers on top of the job envs,
        // as for a step without stepEnvironments, and at the
        // stepEnvironments otherwise.
        if active && job_env_wrap_count + requested.len() + step_env_wrap_count > 1 {
            let at = if step_env_wrap_count == 0 && !requested.is_empty() {
                path_field(&base, "dependencies")
            } else {
                envs_path
            };
            errors.add(&at, SINGLE_WRAP_LAYER_MSG);
        }
    }
}

const SINGLE_WRAP_LAYER_MSG: &str =
    "only one environment in the session stack may define any of onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit (RFC 0008).";

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
    //! No direct unit tests here: every helper is pure-data and already
    //! covered through the integration surface.
}
