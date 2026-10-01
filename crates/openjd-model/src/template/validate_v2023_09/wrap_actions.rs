// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Validate or reject `WRAP_ACTIONS` features (RFC 0008), including the
//! RFC 0009 Service hooks.
//!
//! Seven fields on `<EnvironmentActions>` are gated by the `WRAP_ACTIONS`
//! extension:
//! - `onWrapEnvEnter`, `onWrapTaskRun`, `onWrapEnvExit` (RFC 0008)
//! - `onWrapServiceEnter`, `onWrapServiceRun`, `onWrapServiceReadinessCheck`,
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
//! - **Hooks follow `runScope`** (constraint 6; with `SERVICE`, replacing
//!   the all-or-nothing rule). A wrapping environment — one that defines
//!   any wrap hook — must define `onWrapEnvEnter` and `onWrapEnvExit`; must
//!   define `onWrapTaskRun` iff its `runScope` includes `TASK`; and must
//!   define all four `onWrapService*` hooks iff its `runScope` includes
//!   `SERVICE`. A hook the `runScope` does not call for is rejected, as is
//!   a missing one it does.
//! - **Single-layer** (constraint 2). At most one environment in the
//!   session stack (job environments + each step's step environments) may
//!   define any wrap hook. With `SERVICE`, the same holds for every Service
//!   Session's stack: the scope's environments whose `runScope` includes
//!   `SERVICE` plus the Service's own `serviceEnvironments` (RFC 0009).
//! - **EXPR prerequisite** (constraint 3). A template that lists
//!   `WRAP_ACTIONS` in `extensions:` must also list `EXPR`.
//!
//! A Service's `serviceEnvironments` (RFC 0009, Template Schemas §9 item 5)
//! carry no `runScope` and have an effective `runScope` of `[SERVICE]`, so a
//! wrapping Service Environment must define `onWrapEnvEnter`, `onWrapEnvExit`,
//! and the four `onWrapService*` hooks, and must not define `onWrapTaskRun`.

use crate::error::{path_field, path_index, PathElement, ValidationErrors};
use crate::template::actions::EnvironmentActions;
use crate::template::{
    Environment, EnvironmentTemplate, JobTemplate, RunScope, Service, WrapHookScope,
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

/// The kinds of Session an environment is entered in, as the
/// hooks-follow-`runScope` rule sees them, with the text that names the
/// scope in its messages.
struct EffectiveRunScope {
    task: bool,
    service: bool,
    text: String,
}

impl EffectiveRunScope {
    /// A Job, Step, or environment-template Environment: its `runScope` as
    /// written, every kind when absent (Template Schemas §4 item 3).
    fn of_environment(env: &Environment) -> Self {
        Self {
            task: env.runs_in(RunScope::Task),
            service: env.runs_in(RunScope::Service),
            text: describe_run_scope(env),
        }
    }

    /// A Service's `serviceEnvironments` entry: entered in that Service's
    /// Session only, so `[SERVICE]` whatever it declares (§9 item 5.2; pass
    /// 11 rejects a `runScope` written on one).
    fn service_environment() -> Self {
        Self {
            task: false,
            service: true,
            text: "effective runScope: [SERVICE], a Service Environment".to_string(),
        }
    }

    fn includes(&self, kind: RunScope) -> bool {
        match kind {
            RunScope::Task => self.task,
            RunScope::Service => self.service,
        }
    }
}

/// Check one environment's `<EnvironmentActions>` for wrap hook usage.
///
/// Reports every offending field individually so users see a complete list
/// rather than having to fix them one at a time. Also enforces the
/// all-or-nothing rule (RFC 0008) or, when `SERVICE` is declared, the
/// hooks-follow-`runScope` rule (RFC 0009) that replaces it, evaluated over
/// `scope` — the environment's own `runScope`, or the fixed `[SERVICE]` of a
/// Service Environment.
fn check_environment_actions(
    actions: &EnvironmentActions,
    actions_path: &[PathElement],
    scope: &EffectiveRunScope,
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
        check_hooks_follow_run_scope(scope, actions, actions_path, errors);
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

/// The hooks-follow-`runScope` rule (RFC 0009; Template Schemas §4.3
/// WRAP_ACTIONS constraint 6, §9.7 item 6), applied to a wrapping
/// environment when `SERVICE` is declared.
///
/// Inner environments are entered in every kind of Session, so every
/// wrapping environment must define `onWrapEnvEnter` and `onWrapEnvExit`.
/// `onWrapTaskRun` is required iff the `runScope` includes `TASK`, and the
/// four `onWrapService*` hooks are required iff it includes `SERVICE`. Each
/// group with missing hooks is reported once at the `actions` path naming
/// the missing hooks and the `runScope` that calls for them; each hook the
/// `runScope` does not call for is reported on its own path.
fn check_hooks_follow_run_scope(
    scope: &EffectiveRunScope,
    actions: &EnvironmentActions,
    actions_path: &[PathElement],
    errors: &mut ValidationErrors,
) {
    if !actions.has_any_wrap_hook() {
        return;
    }
    let run_scope_text = scope.text.as_str();

    // Group 1: the environment hooks, required in every runScope.
    let env_hooks = [
        ("onWrapEnvEnter", &actions.on_wrap_env_enter),
        ("onWrapEnvExit", &actions.on_wrap_env_exit),
    ];
    let missing = missing_names(&env_hooks);
    if !missing.is_empty() {
        errors.add(
            actions_path,
            format!(
                "a wrapping environment must define onWrapEnvEnter and onWrapEnvExit whatever its runScope; missing: {} (RFC 0009).",
                missing.join(", ")
            ),
        );
    }

    // Group 2: onWrapTaskRun iff TASK.
    let task_hooks = [("onWrapTaskRun", &actions.on_wrap_task_run)];
    check_scope_group(
        RunScope::Task,
        scope.includes(RunScope::Task),
        &task_hooks,
        run_scope_text,
        actions_path,
        errors,
    );

    // Group 3: the four onWrapService* hooks iff SERVICE.
    let service_hooks = actions.service_wrap_hooks();
    check_scope_group(
        RunScope::Service,
        scope.includes(RunScope::Service),
        &service_hooks,
        run_scope_text,
        actions_path,
        errors,
    );
}

/// Enforce "define exactly `hooks` iff the `runScope` includes `kind`" for
/// one group of hooks.
fn check_scope_group<A>(
    kind: RunScope,
    in_scope: bool,
    hooks: &[(&'static str, &Option<A>)],
    run_scope_text: &str,
    actions_path: &[PathElement],
    errors: &mut ValidationErrors,
) {
    if in_scope {
        let missing = missing_names(hooks);
        if !missing.is_empty() {
            let required: Vec<&str> = hooks.iter().map(|(name, _)| *name).collect();
            errors.add(
                actions_path,
                format!(
                    "a wrapping environment whose runScope includes {kind} ({run_scope_text}) must define {}; missing: {} (RFC 0009).",
                    join_names(&required),
                    missing.join(", ")
                ),
            );
        }
    } else {
        for (name, slot) in hooks {
            if slot.is_some() {
                errors.add(
                    &path_field(actions_path, name),
                    format!(
                        "{name} must not be defined: this environment's runScope ({run_scope_text}) excludes {kind} (RFC 0009)."
                    ),
                );
            }
        }
    }
}

/// The names in `hooks` whose slot is empty, in order.
fn missing_names<A>(hooks: &[(&'static str, &Option<A>)]) -> Vec<&'static str> {
    hooks
        .iter()
        .filter(|(_, slot)| slot.is_none())
        .map(|(name, _)| *name)
        .collect()
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

/// `runScope: [TASK]` as written, or `default: every kind of Session` when
/// the field is absent, for error messages.
fn describe_run_scope(env: &Environment) -> String {
    match &env.run_scope {
        Some(names) => format!("runScope: [{}]", names.join(", ")),
        None => "default runScope: every kind of Session".to_string(),
    }
}

/// Walk one environment for WRAP_ACTIONS gating and return whether it
/// defined any wrap hook (used for the single-layer check upstream).
fn check_env(
    env: &Environment,
    path: &[PathElement],
    gating: WrapGating,
    errors: &mut ValidationErrors,
) -> bool {
    check_env_in_scope(
        env,
        path,
        &EffectiveRunScope::of_environment(env),
        gating,
        errors,
    )
}

/// [`check_env`] with an explicit effective `runScope`.
fn check_env_in_scope(
    env: &Environment,
    path: &[PathElement],
    scope: &EffectiveRunScope,
    gating: WrapGating,
    errors: &mut ValidationErrors,
) -> bool {
    let Some(script) = &env.script else {
        return false;
    };
    let script_path = path_field(path, "script");
    let actions_path = path_field(&script_path, "actions");
    check_environment_actions(&script.actions, &actions_path, scope, gating, errors);
    script.actions.has_any_wrap_hook()
}

/// True iff `env` is entered in Service Sessions and defines a wrap hook —
/// a wrap layer every Service Session in `env`'s scope sees.
fn wraps_service_sessions(env: &Environment) -> bool {
    env.runs_in(RunScope::Service)
        && env
            .script
            .as_ref()
            .is_some_and(|s| s.actions.has_any_wrap_hook())
}

/// Walk one Service's `serviceEnvironments` (RFC 0009, §9 item 5) at
/// `service_path`, each with the effective `runScope: [SERVICE]`, and apply
/// the single-wrap-layer rule to that Service's Session: `outer_wrap_count`
/// is the number of wrap layers the scope's environments entered in Service
/// Sessions already contribute (job environments with `SERVICE` in their
/// `runScope`, plus the Step's for a Step Service). Reported at the
/// `serviceEnvironments` path only when the Service's own list contributes
/// a layer, so two layers in `jobEnvironments` alone are reported there
/// once and not again under every Service.
fn check_service_environments(
    svc: &Service,
    service_path: &[PathElement],
    outer_wrap_count: usize,
    gating: WrapGating,
    errors: &mut ValidationErrors,
) {
    let Some(envs) = &svc.service_environments else {
        return;
    };
    let envs_path = path_field(service_path, "serviceEnvironments");
    let scope = EffectiveRunScope::service_environment();
    let mut own_wrap_count = 0usize;
    for (j, env) in envs.iter().enumerate() {
        if check_env_in_scope(env, &path_index(&envs_path, j), &scope, gating, errors) {
            own_wrap_count += 1;
        }
    }
    if gating.wrap_active && own_wrap_count > 0 && outer_wrap_count + own_wrap_count > 1 {
        errors.add(&envs_path, SERVICE_SESSION_SINGLE_WRAP_LAYER_MSG);
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
///   all-or-nothing rule (or, with `SERVICE`, the hooks-follow-`runScope`
///   rule), and the single-wrap-layer rule per session.
pub fn validate_wrap_actions_job_template(
    jt: &JobTemplate,
    ctx: &ValidationContext,
    errors: &mut ValidationErrors,
) {
    let gating = gating_for(ctx);
    let active = gating.wrap_active;
    check_expr_prerequisite(ctx, errors);

    // Single-wrap-layer enforcement (RFC 0008).
    //
    // The session model is: a session's environment stack is the job's
    // `jobEnvironments` plus exactly ONE step's `stepEnvironments`.
    // Different steps never share a session. So "only one wrap layer per
    // session" reduces to: for every step, (wrap envs in jobEnvironments)
    // + (wrap envs in that step's stepEnvironments) must be <= 1.
    //
    // `check_env` does double duty below: it gates the new fields on the
    // extension and returns true iff the env defines any wrap hook, which
    // is what we sum into the per-scope counts.

    // 1. Count wrap-defining envs in jobEnvironments (shared by every session).
    let mut job_env_wrap_count = 0usize;
    if let Some(envs) = &jt.job_environments {
        let envs_path = path_field(&[], "jobEnvironments");
        for (i, env) in envs.iter().enumerate() {
            if check_env(env, &path_index(&envs_path, i), gating, errors) {
                job_env_wrap_count += 1;
            }
        }
    }

    // Job-envs-only violation: multiple wrap envs in jobEnvironments are
    // reachable from every session, regardless of any step's
    // stepEnvironments, so emit at the jobEnvironments path as soon as the
    // job-env count exceeds one. This is independent of the per-step check
    // below: a template with two job-env wrap layers AND a step that adds
    // its own should report both the jobEnvironments and stepEnvironments
    // violations.
    if active && job_env_wrap_count > 1 {
        errors.add(&path_field(&[], "jobEnvironments"), SINGLE_WRAP_LAYER_MSG);
    }

    // RFC 0009: a Job Service's Session stack is the job environments
    // entered in Service Sessions plus its own serviceEnvironments. The
    // Service lists are walked only with SERVICE (pass 11 rejects them
    // otherwise, and `serviceEnvironments` exists only inside a Service).
    let job_env_service_wrap_count = jt
        .job_environments
        .iter()
        .flatten()
        .filter(|env| wraps_service_sessions(env))
        .count();
    if gating.service_active {
        let list_path = path_field(&[], "jobServices");
        for (k, svc) in jt.job_services.iter().flatten().enumerate() {
            check_service_environments(
                svc,
                &path_index(&list_path, k),
                job_env_service_wrap_count,
                gating,
                errors,
            );
        }
    }

    // 2. For each step, count its stepEnvironments' wrap envs and add the
    //    job-env total — that sum is exactly the set of wrap envs reachable
    //    in that step's session.
    for (i, step) in jt.steps.iter().enumerate() {
        let Some(envs) = &step.step_environments else {
            continue;
        };
        let base = path_index(&path_field(&[], "steps"), i);
        let envs_path = path_field(&base, "stepEnvironments");
        let mut step_env_wrap_count = 0usize;
        for (j, env) in envs.iter().enumerate() {
            if check_env(env, &path_index(&envs_path, j), gating, errors) {
                step_env_wrap_count += 1;
            }
        }
        // Single-wrap-layer rule: a session is built from the job's
        // jobEnvironments plus one step's stepEnvironments, so checking
        // each step's combined total catches every reachable session.
        // This catches two job-env wrap layers, one job-env + one step-env,
        // and two step-env layers within the same step.
        if active && job_env_wrap_count + step_env_wrap_count > 1 {
            errors.add(&envs_path, SINGLE_WRAP_LAYER_MSG);
        }
    }

    // RFC 0009: a Step Service's Session stack is the job environments and
    // the Step's environments entered in Service Sessions, plus its own
    // serviceEnvironments. (Separate from the loop above, which `continue`s
    // past a Step without stepEnvironments.)
    if gating.service_active {
        for (i, step) in jt.steps.iter().enumerate() {
            let Some(services) = &step.step_services else {
                continue;
            };
            let outer = job_env_service_wrap_count
                + step
                    .step_environments
                    .iter()
                    .flatten()
                    .filter(|env| wraps_service_sessions(env))
                    .count();
            let base = path_index(&path_field(&[], "steps"), i);
            let list_path = path_field(&base, "stepServices");
            for (k, svc) in services.iter().enumerate() {
                check_service_environments(svc, &path_index(&list_path, k), outer, gating, errors);
            }
        }
    }
}

const SINGLE_WRAP_LAYER_MSG: &str =
    "only one environment in the session stack may define any of onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit (RFC 0008).";

const SERVICE_SESSION_SINGLE_WRAP_LAYER_MSG: &str =
    "only one environment in a Service Session's stack (the scope's environments whose runScope includes SERVICE, then this Service's serviceEnvironments) may define any wrap hook (RFC 0008, RFC 0009).";

/// Validate RFC 0008 (and RFC 0009 Service hook) constraints for an
/// environment template.
///
/// An environment template defines at most one environment, so the
/// single-layer rule is trivially satisfied. We gate the new fields on the
/// extensions being enabled, enforce the EXPR prerequisite, and enforce the
/// all-or-nothing / hooks-follow-`runScope` rule via `check_env`.
pub fn validate_wrap_actions_environment_template(
    et: &EnvironmentTemplate,
    ctx: &ValidationContext,
    errors: &mut ValidationErrors,
) {
    let gating = gating_for(ctx);
    check_expr_prerequisite(ctx, errors);
    if let Some(env) = &et.environment {
        let env_path = path_field(&[], "environment");
        check_env(env, &env_path, gating, errors);
    }
    // RFC 0009 §1.2.2: the document's Services' serviceEnvironments. The
    // document's own environment, when entered in Service Sessions, is the
    // one outer wrap layer a Service Session of this document's Services
    // can see from this document alone.
    if gating.service_active {
        let outer = et.environment.as_ref().is_some_and(wraps_service_sessions) as usize;
        let list_path = path_field(&[], "services");
        for (k, svc) in et.services.iter().flatten().enumerate() {
            check_service_environments(svc, &path_index(&list_path, k), outer, gating, errors);
        }
    }
}

#[cfg(test)]
mod tests {
    //! Integration tests in `tests/integration/test_wrap_actions.rs` and
    //! `tests/integration/test_service_environments.rs` exercise the full
    //! decode + validate pipeline against real templates. The only direct
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
