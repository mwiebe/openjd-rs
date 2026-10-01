// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Action types per spec §5.

use crate::format_string::FormatString;
use serde::Deserialize;

/// §5 Action
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Action {
    pub command: FormatString,
    pub args: Option<Vec<FormatString>>,
    pub cancelation: Option<CancelationMode>,
    pub timeout: Option<FormatString>,
}

/// §5.3 CancelationMethod — discriminated union on `mode`.
///
/// `DeferredMode` carries a format-string `mode` (FEATURE_BUNDLE_1) whose
/// TERMINATE-vs-NOTIFY_THEN_TERMINATE decision is deferred to run time:
/// `mode` is the schema selector, so it normally must be known at parse
/// time, but a forwarded value like `{{WrappedAction.Cancelation.Mode}}`
/// (RFC 0008 round-trip forwarding) only exists at run time. See
/// `specs/model/template-types.md` § CancelationMode for the full design
/// rationale, and openjd-specifications Template Schemas §5.3 / RFC 0008
/// "Cancelation behavior" for the normative rules.
#[derive(Debug, Clone)]
pub enum CancelationMode {
    /// §5.3.1 — immediate termination, no extra fields allowed.
    Terminate,
    /// §5.3.2 — notify then terminate, with optional grace period.
    NotifyThenTerminate {
        notify_period_in_seconds: Option<FormatString>,
    },
    /// §5.3 (FEATURE_BUNDLE_1) — the mode is a format string, resolved at
    /// run time. See the type-level docs for why this exists.
    DeferredMode {
        mode: FormatString,
        notify_period_in_seconds: Option<FormatString>,
    },
}

impl<'de> Deserialize<'de> for CancelationMode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use std::collections::HashMap;
        let map = HashMap::<String, serde_json::Value>::deserialize(deserializer)?;
        let mode_value = map
            .get("mode")
            .ok_or_else(|| serde::de::Error::missing_field("mode"))?;
        let mode = mode_value
            .as_str()
            .ok_or_else(|| serde::de::Error::custom("`mode` must be a string"))?;
        match mode {
            "TERMINATE" => {
                let extra: Vec<_> = map.keys().filter(|k| *k != "mode").collect();
                if !extra.is_empty() {
                    return Err(serde::de::Error::custom(format!(
                        "unknown field `{}`, TERMINATE accepts no additional fields",
                        extra[0]
                    )));
                }
                Ok(CancelationMode::Terminate)
            }
            "NOTIFY_THEN_TERMINATE" => {
                let extra: Vec<_> = map
                    .keys()
                    .filter(|k| *k != "mode" && *k != "notifyPeriodInSeconds")
                    .collect();
                if !extra.is_empty() {
                    return Err(serde::de::Error::custom(format!(
                        "unknown field `{}`, expected `notifyPeriodInSeconds`",
                        extra[0]
                    )));
                }
                // An explicit null is treated as "not provided", matching
                // the Python implementation (pydantic Optional).
                let notify = map
                    .get("notifyPeriodInSeconds")
                    .filter(|v| !v.is_null())
                    .map(|v| FormatString::deserialize(v.clone()))
                    .transpose()
                    .map_err(serde::de::Error::custom)?;
                Ok(CancelationMode::NotifyThenTerminate {
                    notify_period_in_seconds: notify,
                })
            }
            other if other.contains("{{") => {
                // A format-string mode defers the TERMINATE-vs-
                // NOTIFY_THEN_TERMINATE decision to run time (see the
                // type-level docs). Because the shape is not yet known,
                // accept the union of the two shapes' fields; the
                // resolved object is validated against the resolved
                // variant's shape at run time. FEATURE_BUNDLE_1 gating is
                // enforced by template validation, not here.
                let extra: Vec<_> = map
                    .keys()
                    .filter(|k| *k != "mode" && *k != "notifyPeriodInSeconds")
                    .collect();
                if !extra.is_empty() {
                    return Err(serde::de::Error::custom(format!(
                        "unknown field `{}`, expected `notifyPeriodInSeconds`",
                        extra[0]
                    )));
                }
                let mode = FormatString::deserialize(mode_value.clone())
                    .map_err(serde::de::Error::custom)?;
                let notify = map
                    .get("notifyPeriodInSeconds")
                    .filter(|v| !v.is_null())
                    .map(|v| FormatString::deserialize(v.clone()))
                    .transpose()
                    .map_err(serde::de::Error::custom)?;
                Ok(CancelationMode::DeferredMode {
                    mode,
                    notify_period_in_seconds: notify,
                })
            }
            other => Err(serde::de::Error::custom(format!(
                "unknown variant `{other}`, expected `TERMINATE` or `NOTIFY_THEN_TERMINATE`"
            ))),
        }
    }
}

/// §3.5.1 StepActions
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepActions {
    pub on_run: Action,
}

/// §4.1 EnvironmentActions
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentActions {
    pub on_enter: Option<Action>,
    /// RFC 0008 — wraps inner environments' `onEnter` actions. Requires the
    /// `WRAP_ACTIONS` extension.
    pub on_wrap_env_enter: Option<Action>,
    /// RFC 0008 — wraps tasks' `onRun` actions. Requires the
    /// `WRAP_ACTIONS` extension.
    pub on_wrap_task_run: Option<Action>,
    /// RFC 0008 — wraps inner environments' `onExit` actions. Requires the
    /// `WRAP_ACTIONS` extension.
    pub on_wrap_env_exit: Option<Action>,
    /// RFC 0009 — in a Service Session, runs instead of the wrapped
    /// Service's `onEnter`. Requires both the `WRAP_ACTIONS` and `SERVICE`
    /// extensions.
    pub on_wrap_service_enter: Option<Action>,
    /// RFC 0009 — in a Service Session, runs instead of the wrapped
    /// Service's `onRun`. Requires both the `WRAP_ACTIONS` and `SERVICE`
    /// extensions.
    pub on_wrap_service_run: Option<Action>,
    /// RFC 0009 — in a Service Session, runs instead of the wrapped
    /// Service's `onReadinessCheck`, concurrently with `onWrapServiceRun`.
    /// Requires both the `WRAP_ACTIONS` and `SERVICE` extensions.
    pub on_wrap_service_readiness_check: Option<Action>,
    /// RFC 0009 — in a Service Session, runs instead of the wrapped
    /// Service's `onExit`. Requires both the `WRAP_ACTIONS` and `SERVICE`
    /// extensions.
    pub on_wrap_service_exit: Option<Action>,
    pub on_exit: Option<Action>,
}

impl EnvironmentActions {
    /// Template Schemas §5 default `timeout` for `onExit`, in seconds (five
    /// minutes), shared by `onWrapEnvExit` and `onWrapServiceExit`.
    pub const ON_EXIT_DEFAULT_TIMEOUT_SECONDS: u64 = 300;
    /// Default `timeout` for `onWrapServiceReadinessCheck`, in seconds: the
    /// wrapped `<ServiceActions>.onReadinessCheck` default (RFC 0009), as
    /// `onWrapEnvExit` takes `onExit`'s.
    pub const ON_WRAP_SERVICE_READINESS_CHECK_DEFAULT_TIMEOUT_SECONDS: u64 = 30;

    /// The default `timeout` of the named `<EnvironmentActions>` slot when
    /// the template gives none, per the Template Schemas §5 timeout table:
    /// `onExit` and `onWrapEnvExit` 300 seconds; the RFC 0009 Service hooks
    /// take the wrapped `<ServiceActions>` default — `onWrapServiceExit` 300
    /// seconds, `onWrapServiceReadinessCheck` 30 seconds; every other slot
    /// has no default (`None`). Also `None` for a name that is not an
    /// `<EnvironmentActions>` slot.
    pub fn default_timeout_seconds(action_name: &str) -> Option<u64> {
        match action_name {
            "onExit" | "onWrapEnvExit" | "onWrapServiceExit" => {
                Some(Self::ON_EXIT_DEFAULT_TIMEOUT_SECONDS)
            }
            "onWrapServiceReadinessCheck" => {
                Some(Self::ON_WRAP_SERVICE_READINESS_CHECK_DEFAULT_TIMEOUT_SECONDS)
            }
            _ => None,
        }
    }

    /// The four RFC 0009 `onWrapService*` hooks, each paired with its schema
    /// name, in lifecycle order. A wrapping Environment whose `runScope`
    /// includes `SERVICE` must define all of them (Template Schemas §4.3
    /// WRAP_ACTIONS constraint 6).
    pub fn service_wrap_hooks(&self) -> [(&'static str, &Option<Action>); 4] {
        [
            ("onWrapServiceEnter", &self.on_wrap_service_enter),
            ("onWrapServiceRun", &self.on_wrap_service_run),
            (
                "onWrapServiceReadinessCheck",
                &self.on_wrap_service_readiness_check,
            ),
            ("onWrapServiceExit", &self.on_wrap_service_exit),
        ]
    }

    /// True iff any of the four RFC 0009 `onWrapService*` hooks is defined.
    pub fn has_any_service_wrap_hook(&self) -> bool {
        self.service_wrap_hooks()
            .iter()
            .any(|(_, slot)| slot.is_some())
    }
}

/// The per-hook companion template variables a wrap hook exposes in
/// addition to `WrappedAction.*` (RFC 0008, RFC 0009).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrapHookScope {
    /// `WrappedEnv.Name` — available in `onWrapEnvEnter` and `onWrapEnvExit`.
    EnvName,
    /// `WrappedStep.Name` — available in `onWrapTaskRun`.
    StepName,
    /// `WrappedService.*` — available in the four RFC 0009 `onWrapService*`
    /// hooks (`SERVICE` extension).
    Service,
}

/// Generate the shared accessor/iteration helpers for an
/// `EnvironmentActions` struct from its list of action slots and the subset
/// of them that are wrap hooks.
///
/// The template-side (`template::actions`) and job-side (`job`) structs
/// share field names but have distinct `Action` types and derives — and the
/// template side additionally carries the RFC 0009 `onWrapService*` hooks —
/// so each struct's slots are enumerated exactly once, at its invocation.
/// Every consumer that needs to "walk the actions" goes through these
/// methods instead of re-listing the fields, which is what keeps a field
/// rename from rippling across the codebase.
///
/// `slots` lists every action slot as `(schema name, field)` in
/// declaration order; `wrap_hooks` lists the wrap-hook subset as
/// `(schema name, field, WrapHookScope variant)`.
macro_rules! impl_environment_actions_helpers {
    (
        $ty:ty, $action:ty,
        slots: [ $( ($slot_name:literal, $slot_field:ident) ),* $(,)? ],
        wrap_hooks: [ $( ($hook_name:literal, $hook_field:ident, $hook_scope:ident) ),* $(,)? ]
    ) => {
        impl $ty {
            /// Every action slot paired with its camelCase schema name, in
            /// declaration order.
            pub fn named_slots(
                &self,
            ) -> [(&'static str, &Option<$action>); { <[()]>::len(&[$( { let _ = $slot_name; } ),*]) }]
            {
                [$( ($slot_name, &self.$slot_field) ),*]
            }

            /// The defined actions, each paired with its schema name, in
            /// declaration order.
            pub fn iter_named(&self) -> impl Iterator<Item = (&'static str, &$action)> {
                self.named_slots()
                    .into_iter()
                    .filter_map(|(name, slot)| slot.as_ref().map(|a| (name, a)))
            }

            /// The defined actions, in declaration order, without names.
            pub fn iter_actions(&self) -> impl Iterator<Item = &$action> {
                self.iter_named().map(|(_, action)| action)
            }

            /// The wrap hooks, each paired with its schema name and the
            /// companion template variables it exposes, in declaration
            /// order.
            pub fn wrap_hooks(
                &self,
            ) -> [(
                &'static str,
                &Option<$action>,
                $crate::template::WrapHookScope,
            ); { <[()]>::len(&[$( { let _ = $hook_name; } ),*]) }] {
                [$( (
                    $hook_name,
                    &self.$hook_field,
                    $crate::template::WrapHookScope::$hook_scope,
                ) ),*]
            }

            /// True iff at least one action slot is defined.
            pub fn has_any_action(&self) -> bool {
                self.named_slots().iter().any(|(_, slot)| slot.is_some())
            }

            /// True iff any wrap hook is defined — the definition of a
            /// *wrapping* Environment (RFC 0008, RFC 0009).
            pub fn has_any_wrap_hook(&self) -> bool {
                self.wrap_hooks().iter().any(|(_, slot, _)| slot.is_some())
            }
        }
    };
}
pub(crate) use impl_environment_actions_helpers;

impl_environment_actions_helpers!(
    EnvironmentActions, Action,
    slots: [
        ("onEnter", on_enter),
        ("onWrapEnvEnter", on_wrap_env_enter),
        ("onWrapTaskRun", on_wrap_task_run),
        ("onWrapEnvExit", on_wrap_env_exit),
        ("onWrapServiceEnter", on_wrap_service_enter),
        ("onWrapServiceRun", on_wrap_service_run),
        ("onWrapServiceReadinessCheck", on_wrap_service_readiness_check),
        ("onWrapServiceExit", on_wrap_service_exit),
        ("onExit", on_exit),
    ],
    wrap_hooks: [
        ("onWrapEnvEnter", on_wrap_env_enter, EnvName),
        ("onWrapTaskRun", on_wrap_task_run, StepName),
        ("onWrapEnvExit", on_wrap_env_exit, EnvName),
        ("onWrapServiceEnter", on_wrap_service_enter, Service),
        ("onWrapServiceRun", on_wrap_service_run, Service),
        ("onWrapServiceReadinessCheck", on_wrap_service_readiness_check, Service),
        ("onWrapServiceExit", on_wrap_service_exit, Service),
    ]
);
