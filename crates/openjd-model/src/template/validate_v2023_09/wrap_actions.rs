// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Validate or reject `WRAP_ACTIONS` features (RFC 0008), including the
//! RFC 0009 Service hooks.
//!
//! Seven fields on `<EnvironmentActions>` are gated by the `WRAP_ACTIONS`
//! extension:
//! - `onWrapEnvEnter`, `onWrapTaskRun`, `onWrapEnvExit` (RFC 0008)
//! - `onWrapServiceEnter`, `onWrapServiceRun`, `onWrapServiceHealthCheck`,
//!   `onWrapServiceExit` (RFC 0009) — these additionally require `SERVICE`.
//!
//! When the extension is not enabled, using any of these fields is a
//! validation error. When it is enabled, we additionally enforce the
//! constraints from RFC 0008 and RFC 0009 (Template Schemas §4.3,
//! "WRAP_ACTIONS extension constraints"):
//!
//! - **All-or-nothing** (constraint 1; without `SERVICE`). An environment
//!   that defines any of the three RFC 0008 wrap hooks must define all
//!   three.
//! - **Every action inside a wrapper is wrapped** (RFC 0009 wrap hook rule
//!   1; with `SERVICE`, replacing the all-or-nothing rule). A wrapping
//!   environment — one that defines any wrap hook — must define the hook
//!   for every kind of action a kind of Session entering it may run inside
//!   it: `onWrapEnvEnter`, `onWrapTaskRun`, and `onWrapEnvExit` if any Task
//!   Session enters it, and `onWrapEnvEnter`, `onWrapEnvExit`, and the four
//!   `onWrapService*` hooks if any Service Session enters it. A hook for a
//!   kind of action that never runs inside it is permitted. The check is
//!   made here for a kind of Session known to enter the wrapper ([`Entrances`]
//!   for a Job Environment; its Step, for a Step Environment whose
//!   `enableIf` is absent or `true`), and otherwise when the Job is
//!   created; an Environment Template's wrapper is checked when the Job is
//!   created, against the Sessions that enter it in the combined Job.
//! - **Single-layer** (constraint 2). At most one environment in the
//!   session stack (each step's Environment set: the job environments its
//!   sessions enter, by `activation` and its `environment:` entries, plus
//!   its step environments, less any
//!   whose `enableIf` is `false`; each Service's: the job environments its
//!   Sessions enter) may define any wrap hook. Template validation counts
//!   only the Environments certainly entered; job creation checks the final
//!   set.
//! - **EXPR prerequisite** (constraint 3). A template that lists
//!   `WRAP_ACTIONS` in `extensions:` must also list `EXPR`.

use crate::error::{path_field, path_index, PathElement, ValidationErrors};
use crate::template::actions::EnvironmentActions;
use std::collections::HashSet;

use crate::template::{
    Entrance, Entrances, Environment, EnvironmentActivation, EnvironmentTemplate, JobTemplate,
    KnownEntry, WrapHookScope,
};
use crate::types::{ModelExtension, ValidationContext};

/// Which extensions govern the wrap hooks, computed once per template.
#[derive(Debug, Clone, Copy)]
struct WrapGating {
    /// `WRAP_ACTIONS` is declared.
    wrap_active: bool,
    /// `SERVICE` is declared.
    service_active: bool,
}

/// The entities known at template validation to enter a wrapping
/// environment, one of each kind of Session, as messages name them
/// (`Step 'S'`, `Service 'C'`): wrap hook rule 1 is checked here for each
/// kind that has one.
#[derive(Default)]
struct KnownEntrants {
    task: Option<String>,
    service: Option<String>,
}

/// Check one environment's `<EnvironmentActions>` for wrap hook usage.
///
/// Reports every offending field individually so users see a complete list
/// rather than having to fix them one at a time. Also enforces the
/// all-or-nothing rule (RFC 0008) or, when `SERVICE` is declared, wrap hook
/// rule 1 (RFC 0009) that replaces it, for the kinds of Session `entrants`
/// knows to enter the environment.
fn check_environment_actions(
    actions: &EnvironmentActions,
    actions_path: &[PathElement],
    entrants: &KnownEntrants,
    gating: WrapGating,
    errors: &mut ValidationErrors,
) {
    let wrap_hooks = actions.wrap_hooks();
    for (name, slot, scope) in wrap_hooks {
        if slot.is_none() {
            continue;
        }
        let needs_service = scope == WrapHookScope::Service;
        let missing: &str = match (gating.wrap_active, needs_service && !gating.service_active) {
            (true, false) => continue,
            (false, false) => "the WRAP_ACTIONS extension",
            (true, true) => "the SERVICE extension",
            (false, true) => "the WRAP_ACTIONS and SERVICE extensions",
        };
        errors.add(
            &path_field(actions_path, name),
            format!("{name} requires {missing}."),
        );
    }

    // The completeness rules are only enforced when WRAP_ACTIONS is active —
    // otherwise the per-hook errors above already cover it.
    if !gating.wrap_active {
        return;
    }

    if gating.service_active {
        let defined = defined_wrap_hooks(&wrap_hooks);
        if let Some(message) = wrap_hook_rule_1(&defined, entrants.task.as_deref(), true) {
            errors.add(actions_path, message);
        }
        if let Some(message) = wrap_hook_rule_1(&defined, entrants.service.as_deref(), false) {
            errors.add(actions_path, message);
        }
        return;
    }

    // All-or-nothing rule (RFC 0008, without SERVICE): an env that defines
    // any of the three RFC 0008 wrap hooks must define all three. The RFC
    // 0009 Service hooks are not part of this count; each of them has
    // already been rejected above for lacking SERVICE.
    let rfc0008_hooks: Vec<_> = wrap_hooks
        .iter()
        .filter(|(_, _, scope)| *scope != WrapHookScope::Service)
        .collect();
    let defined = rfc0008_hooks
        .iter()
        .filter(|(_, slot, _)| slot.is_some())
        .count();
    if defined > 0 && defined < rfc0008_hooks.len() {
        errors.add(
            actions_path,
            "an environment that defines any of onWrapEnvEnter, onWrapTaskRun, or onWrapEnvExit must define all three (RFC 0008).",
        );
    }
}

/// Wrap hook rule 1 (RFC 0009) for one kind of Session: the hooks a
/// wrapping environment with the hooks `defined` (each name and whether it
/// is defined) must define for the actions that kind of Session runs inside
/// it — `onWrapEnvEnter`, `onWrapTaskRun`, and `onWrapEnvExit` for a Task
/// Session (`task`), and `onWrapEnvEnter`, `onWrapEnvExit`, and the four
/// `onWrapService*` hooks for a Service Session — and those of them it
/// lacks. `None` when nothing is missing or the environment defines no wrap
/// hook.
pub(crate) fn missing_wrap_hooks(
    defined: &[(&'static str, bool)],
    task: bool,
) -> Option<(Vec<&'static str>, Vec<&'static str>)> {
    if !defined.iter().any(|(_, d)| *d) {
        return None;
    }
    let is_defined = |name: &str| defined.iter().any(|(n, d)| *n == name && *d);
    let mut required: Vec<&'static str> = vec!["onWrapEnvEnter"];
    if task {
        required.push("onWrapTaskRun");
    }
    required.push("onWrapEnvExit");
    if !task {
        required.extend([
            "onWrapServiceEnter",
            "onWrapServiceRun",
            "onWrapServiceHealthCheck",
            "onWrapServiceExit",
        ]);
    }
    let missing: Vec<&'static str> = required
        .iter()
        .copied()
        .filter(|n| !is_defined(n))
        .collect();
    (!missing.is_empty()).then_some((required, missing))
}

/// `a`, `a and b`, or `a, b, and c`, for hook names in messages.
pub(crate) fn join_hook_names(names: &[&str]) -> String {
    join_names(names)
}

/// Wrap hook rule 1 at template validation: the message for a wrapping
/// environment that the Sessions of `entrant` (`Step 'S'`, `Service 'C'`)
/// are known to enter, when it lacks hooks for them; see
/// [`missing_wrap_hooks`].
fn wrap_hook_rule_1(
    defined: &[(&'static str, bool)],
    entrant: Option<&str>,
    task: bool,
) -> Option<String> {
    let entrant = entrant?;
    let (required, missing) = missing_wrap_hooks(defined, task)?;
    Some(format!(
        "the Sessions of {entrant} enter this wrapping environment, which therefore must define \
         {}; missing: {} (RFC 0009 wrap hook rule 1).",
        join_names(&required),
        missing.join(", ")
    ))
}

/// Each wrap hook's name and whether it is defined, from an
/// `<EnvironmentActions>`' `wrap_hooks()`, template or job side.
pub(crate) fn defined_wrap_hooks<A>(
    hooks: &[(&'static str, &Option<A>, WrapHookScope)],
) -> Vec<(&'static str, bool)> {
    hooks.iter().map(|(n, s, _)| (*n, s.is_some())).collect()
}

/// `a`, `a and b`, or `a, b, and c`.
fn join_names(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [only] => (*only).to_string(),
        [a, b] => format!("{a} and {b}"),
        [init @ .., last] => format!("{}, and {last}", init.join(", ")),
    }
}

/// Walk one environment for WRAP_ACTIONS gating and return whether it
/// defined any wrap hook (used for the single-layer check upstream).
/// `entrants` are the entities known to enter it, for wrap hook rule 1.
fn check_env(
    env: &Environment,
    path: &[PathElement],
    entrants: KnownEntrants,
    gating: WrapGating,
    errors: &mut ValidationErrors,
) -> bool {
    let Some(script) = &env.script else {
        return false;
    };
    let script_path = path_field(path, "script");
    let actions_path = path_field(&script_path, "actions");
    check_environment_actions(&script.actions, &actions_path, &entrants, gating, errors);
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

fn gating_for(ctx: &ValidationContext) -> WrapGating {
    WrapGating {
        wrap_active: ctx.profile.has_extension(ModelExtension::WrapActions),
        service_active: ctx.profile.has_extension(ModelExtension::Service),
    }
}

/// Validate RFC 0008 (and RFC 0009 Service hook) constraints for a job
/// template.
///
/// This runs regardless of whether `WRAP_ACTIONS` is enabled:
/// - When disabled, it rejects templates that attempt to use any of the
///   new fields.
/// - When enabled, it additionally enforces the EXPR prerequisite, the
///   all-or-nothing rule (or, with `SERVICE`, wrap hook rule 1 for the
///   kinds of Session known to enter each wrapper), and the
///   single-wrap-layer rule per session.
pub fn validate_wrap_actions_job_template(
    jt: &JobTemplate,
    ctx: &ValidationContext,
    known: &super::format_strings::KnownEnableIfs,
    errors: &mut ValidationErrors,
) {
    let gating = gating_for(ctx);
    let active = gating.wrap_active;
    check_expr_prerequisite(ctx, errors);
    // Which entities are known to enter each Job Environment (RFC 0009
    // "Services run inside Environments"), for wrap hook rule 1 and the
    // Service Sessions' single-layer rule.
    let entrances = Entrances::from_known(jt, known);
    let first_known =
        |of: &[Vec<Entrance>], e: usize| of.iter().position(|row| row[e] == Entrance::Enters);

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
            let entrants = KnownEntrants {
                task: first_known(&entrances.steps, i)
                    .map(|s| format!("Step '{}'", jt.steps[s].name)),
                service: first_known(&entrances.services, i)
                    .map(|k| format!("Service '{}'", jt.services()[k].name)),
            };
            // A grouped one is never certainly entered before the
            // Environment Templates are known: one of its group may come
            // first and supersede it (§4 item 7, and the paragraph after
            // item 10).
            if check_env(env, &path_index(&envs_path, i), entrants, gating, errors)
                && env.group().is_none()
            {
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
            // A Step Environment is entered only by its Step's Task
            // Sessions; they are known to enter it when its `enableIf` is
            // known to be `true` (§7.6 rule 5).
            let enabled = known.step_environment(i, j) == Some(true);
            let entrants = KnownEntrants {
                task: enabled.then(|| format!("Step '{}'", step.name)),
                service: None,
            };
            if check_env(env, &path_index(&envs_path, j), entrants, gating, errors) && enabled {
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

    // 3. A Service Session's stack is the Job Environments its Sessions
    //    enter (RFC 0009 "Services run inside Environments"); count those
    //    known to be entered.
    if active {
        let envs = jt.job_environments.as_deref().unwrap_or(&[]);
        for (k, row) in entrances.services.iter().enumerate() {
            let layers = envs
                .iter()
                .zip(row)
                .filter(|(env, entrance)| {
                    **entrance == Entrance::Enters
                        && env
                            .script
                            .as_ref()
                            .is_some_and(|s| s.actions.has_any_wrap_hook())
                })
                .count();
            if layers > 1 {
                errors.add(
                    &path_index(&path_field(&[], "services"), k),
                    SINGLE_WRAP_LAYER_MSG,
                );
            }
        }
    }
}

pub(crate) const SINGLE_WRAP_LAYER_MSG: &str =
    "only one environment in the session stack may define any of onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit (RFC 0008).";

/// Validate RFC 0008 (and RFC 0009 Service hook) constraints for an
/// environment template.
///
/// An environment template defines at most one environment, so the
/// single-layer rule is trivially satisfied. We gate the new fields on the
/// extensions being enabled, enforce the EXPR prerequisite, and, without
/// `SERVICE`, the all-or-nothing rule via `check_env`. With `SERVICE`, no
/// kind of Session is known to enter the environment before the Job is
/// created, so wrap hook rule 1 is checked then.
pub fn validate_wrap_actions_environment_template(
    et: &EnvironmentTemplate,
    ctx: &ValidationContext,
    errors: &mut ValidationErrors,
) {
    let gating = gating_for(ctx);
    check_expr_prerequisite(ctx, errors);
    if let Some(env) = &et.environment {
        let env_path = path_field(&[], "environment");
        check_env(env, &env_path, KnownEntrants::default(), gating, errors);
    }
}

#[cfg(test)]
mod tests {
    //! Integration tests in `tests/integration/test_wrap_actions.rs` and
    //! `tests/integration/test_service.rs` exercise the full decode +
    //! validate pipeline against real templates. The only direct
    //! unit test here is for the message-formatting helper.

    use super::join_names;

    #[test]
    fn join_names_english_list() {
        assert_eq!(join_names(&[]), "");
        assert_eq!(join_names(&["a"]), "a");
        assert_eq!(join_names(&["a", "b"]), "a and b");
        assert_eq!(join_names(&["a", "b", "c"]), "a, b, and c");
    }
}
