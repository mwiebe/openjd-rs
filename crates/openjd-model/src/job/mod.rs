// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Instantiated job types — the result of `create_job()`.
//!
//! These types represent a fully resolved job where all format strings
//! have been evaluated and template variables substituted. Contrast with
//! `crate::template` which holds the unresolved template types.
//!
//! # Equality and hashing
//!
//! All types in this module implement `PartialEq` and `Hash` with the
//! invariant `a == b ⇒ hash(a) == hash(b)`. Equality is structural on
//! the *created job*, not the source template: derived state such as
//! `resolved_symtab` participates, and since its transport format
//! preserves original float literals, jobs created from `1.0` vs `1.00`
//! parameter values compare unequal. Map-typed fields (`IndexMap`,
//! `HashMap`) compare order-insensitively, so their `Hash` impls are
//! written by hand to hash entries sorted by key. `f64` fields hash via
//! `to_bits()` after normalizing `-0.0` to `0.0`, consistent with
//! `-0.0 == 0.0`. Types with `f64` fields implement `PartialEq` but not
//! `Eq`.

pub mod create_job;
pub mod service_symbols;
pub mod step_dependency_graph;
pub mod step_param_space;

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use indexmap::IndexMap;
use openjd_expr::format_string::FormatString;
use openjd_expr::symbol_table::SerializedSymbolTable;
use openjd_expr::value::Float64;
use openjd_expr::ExprValue;
use openjd_expr::RangeExpr;
use serde::{Deserialize, Serialize};

use crate::types::{EndOfLine, FileType};

use crate::template::RangeConstraint;
pub use crate::template::{CompletedTasksPolicy, RunScope};
use crate::types::JobParameterType;

/// Hash the entries of a string-keyed map sorted by key, so that maps
/// that compare equal (order-insensitively) hash identically regardless
/// of insertion order.
fn hash_map_entries<K: AsRef<str>, V: Hash, H: Hasher>(
    entries: impl Iterator<Item = (K, V)>,
    state: &mut H,
) {
    let mut entries: Vec<_> = entries.collect();
    entries.sort_unstable_by(|(a, _), (b, _)| a.as_ref().cmp(b.as_ref()));
    entries.len().hash(state);
    for (k, v) in entries {
        k.as_ref().hash(state);
        v.hash(state);
    }
}

/// Hash an `f64` via its bit pattern, normalizing `-0.0` to `0.0` so the
/// hash is consistent with `-0.0 == 0.0` under `PartialEq`.
fn hash_f64<H: Hasher>(v: f64, state: &mut H) {
    let v = if v == 0.0 { 0.0 } else { v };
    v.to_bits().hash(state);
}

/// A fully instantiated job — all format strings resolved, parameters bound.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub name: String,
    pub description: Option<String>,
    pub extensions: Option<Vec<crate::types::ModelExtension>>,
    pub parameters: IndexMap<String, JobParameter>,
    pub steps: Vec<Step>,
    pub job_environments: Option<Vec<Environment>>,
    /// The Job's Services (RFC 0009 `jobServices`), in start order. Each is
    /// started before any Task of the Job is scheduled and stopped once no
    /// Task remains. `None` when the template declares none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_services: Option<Vec<Service>>,
}

/// Manual because `IndexMap` has no `Hash`; parameters hash as
/// key-sorted entries to match `IndexMap`'s order-insensitive equality.
impl Hash for Job {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.description.hash(state);
        self.extensions.hash(state);
        hash_map_entries(self.parameters.iter(), state);
        self.steps.hash(state);
        self.job_environments.hash(state);
        self.job_services.hash(state);
    }
}

/// A resolved job parameter (name + type + bound value).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobParameter {
    pub name: String,
    pub param_type: JobParameterType,
    pub value: ExprValue,
}

/// A fully instantiated step.
#[derive(Debug, Clone, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub name: String,
    pub description: Option<String>,
    pub script: StepScript,
    pub step_environments: Option<Vec<Environment>>,
    pub parameter_space: Option<StepParameterSpace>,
    pub host_requirements: Option<HostRequirements>,
    pub dependencies: Option<Vec<StepDependency>>,
    /// The Step's Services (RFC 0009 `stepServices`), in start order. Each
    /// is started once the Step's dependencies are satisfied and before any
    /// of its Tasks is scheduled, and is available only to this Step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_services: Option<Vec<Service>>,
    /// Complete symbol table at step scope in JSON transport format.
    /// Contains Param.*, RawParam.*, Job.Name, Step.Name, and step-level let bindings.
    /// The session deserializes this with PathFormat::host() and layers
    /// Session.* and Task.* values on top at runtime.
    #[serde(rename = "resolvedSymTab", skip_serializing_if = "Option::is_none")]
    pub resolved_symtab: Option<SerializedSymbolTable>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepScript {
    #[serde(rename = "let", alias = "letBindings")]
    pub let_bindings: Option<Vec<String>>,
    pub actions: StepActions,
    pub embedded_files: Option<Vec<EmbeddedFile>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepActions {
    pub on_run: Action,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Action {
    pub command: FormatString,
    pub args: Option<Vec<FormatString>>,
    pub timeout: Option<FormatString>,
    pub cancelation: Option<CancelationMode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    pub name: String,
    pub description: Option<String>,
    /// RFC 0009 `runScope` (Template Schemas §4 item 3): the kinds of
    /// Session this Environment is entered in. `None` means every kind;
    /// query the effective scope with [`runs_in`](Self::runs_in). Typed
    /// here (unlike the template side) because validation has already
    /// rejected unrecognized names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_scope: Option<Vec<RunScope>>,
    pub script: Option<EnvironmentScript>,
    pub variables: Option<HashMap<String, FormatString>>,
    /// Filtered symbol table containing only symbols referenced by this
    /// environment's format strings (variables, actions, embedded files, let bindings).
    #[serde(rename = "resolvedSymTab", skip_serializing_if = "Option::is_none")]
    pub resolved_symtab: Option<SerializedSymbolTable>,
}

impl Environment {
    /// True iff this Environment is entered in Sessions of kind `kind`
    /// (Template Schemas §4 item 3, RFC 0009): every kind when `runScope` is
    /// absent, else exactly the kinds the list names. The job-side
    /// counterpart of [`crate::template::Environment::runs_in`].
    #[must_use]
    pub fn runs_in(&self, kind: RunScope) -> bool {
        match &self.run_scope {
            None => true,
            Some(kinds) => kinds.contains(&kind),
        }
    }
}

/// Manual because `HashMap` has no `Hash`; `variables` hashes as
/// key-sorted entries to match `HashMap`'s order-insensitive equality.
impl Hash for Environment {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.description.hash(state);
        self.run_scope.hash(state);
        self.script.hash(state);
        match &self.variables {
            None => false.hash(state),
            Some(vars) => {
                true.hash(state);
                hash_map_entries(vars.iter(), state);
            }
        }
        self.resolved_symtab.hash(state);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentScript {
    #[serde(rename = "let", alias = "letBindings")]
    pub let_bindings: Option<Vec<String>>,
    pub actions: EnvironmentActions,
    pub embedded_files: Option<Vec<EmbeddedFile>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentActions {
    pub on_enter: Option<Action>,
    /// RFC 0008 — wraps inner environments' `onEnter` actions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_wrap_env_enter: Option<Action>,
    /// RFC 0008 — wraps tasks' `onRun` actions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_wrap_task_run: Option<Action>,
    /// RFC 0008 — wraps inner environments' `onExit` actions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_wrap_env_exit: Option<Action>,
    /// RFC 0009 — in a Service Session, wraps the Service's `onEnter`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_wrap_service_enter: Option<Action>,
    /// RFC 0009 — in a Service Session, wraps the Service's `onRun`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_wrap_service_run: Option<Action>,
    /// RFC 0009 — in a Service Session, wraps the Service's
    /// `onReadinessCheck`, concurrently with `onWrapServiceRun`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_wrap_service_readiness_check: Option<Action>,
    /// RFC 0009 — in a Service Session, wraps the Service's `onExit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_wrap_service_exit: Option<Action>,
    pub on_exit: Option<Action>,
}

// The job-side struct mirrors the template side's slots exactly, including
// the RFC 0009 `onWrapService*` hooks, so a Session runtime can dispatch a
// Service Session's wrap hooks from the created job.
crate::template::impl_environment_actions_helpers!(
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

impl EnvironmentActions {
    /// The four RFC 0009 `onWrapService*` hooks, each paired with its schema
    /// name, in lifecycle order (the job-side counterpart of
    /// [`crate::template::EnvironmentActions::service_wrap_hooks`]).
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

/// An instantiated Service (RFC 0009 `<Service>`, Template Schemas §9) —
/// the result of job creation for one `jobServices` or `stepServices`
/// entry.
///
/// Job-creation-stage fields are resolved: the `<Service>.let` bindings
/// (into [`resolved_symtab`](Self::resolved_symtab)), the numeric
/// `@fmtstring` fields (`port`, `timeoutSeconds`, `intervalSeconds`,
/// `maxAttempts`, with the §9 defaults applied where the template gave
/// none), and `hostRequirements`. `variables` and `script` are
/// `@fmtstring[host]` and remain `FormatString`s for the Service Session to
/// resolve, exactly like an [`Environment`]'s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Service {
    pub name: String,
    pub description: Option<String>,
    /// Resolved host requirements the service host must satisfy.
    pub host_requirements: Option<HostRequirements>,
    /// The declared ports, in declaration order.
    pub ports: Vec<ServicePort>,
    /// The effective readiness check, with the §9.3 defaults applied.
    pub readiness_check: ServiceReadinessCheck,
    /// The effective restart policy, with the §9.4 defaults applied.
    pub restart_policy: ServiceRestartPolicy,
    /// Environment variables set for every action of the Service's script
    /// (session scope — resolved on the service host). Not propagated to
    /// the entities in the Service's scope.
    pub variables: Option<HashMap<String, FormatString>>,
    pub script: ServiceScript,
    /// Filtered symbol table containing only the symbols referenced by this
    /// Service's host-resolved format strings (variables, actions,
    /// embedded files, `<ServiceScript>.let`), including the `<Service>.let`
    /// bindings they use. The Service Session layers `Session.*`,
    /// `Service.File.*` and the in-scope `Service.*` endpoints on top.
    #[serde(rename = "resolvedSymTab", skip_serializing_if = "Option::is_none")]
    pub resolved_symtab: Option<SerializedSymbolTable>,
}

/// Manual because `HashMap` has no `Hash`; `variables` hashes as
/// key-sorted entries to match `HashMap`'s order-insensitive equality.
impl Hash for Service {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.description.hash(state);
        self.host_requirements.hash(state);
        self.ports.hash(state);
        self.readiness_check.hash(state);
        self.restart_policy.hash(state);
        match &self.variables {
            None => false.hash(state),
            Some(vars) => {
                true.hash(state);
                hash_map_entries(vars.iter(), state);
            }
        }
        self.script.hash(state);
        self.resolved_symtab.hash(state);
    }
}

impl Service {
    /// The names of the declared ports, in declaration order.
    pub fn port_names(&self) -> impl Iterator<Item = &str> {
        self.ports.iter().map(|p| p.name.as_str())
    }
}

/// An instantiated `<ServicePort>` (§9.2).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServicePort {
    /// The second component of `Service.<service>.<port>.*` references.
    pub name: String,
    /// The specific TCP port the Service requires on its host, or `None`
    /// when the runtime allocates one. Resolved from the template's
    /// `@fmtstring`; a whole-field `null` resolution is `None`.
    pub port: Option<u16>,
}

/// An instantiated `<ServiceReadinessCheck>` (§9.3), with the defaults
/// applied: `timeoutSeconds` 300, `intervalSeconds` 5, and a `TCP_CONNECT`
/// check without `ports` probing every declared port.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum ServiceReadinessCheck {
    /// READY once a TCP connection to each of `ports` succeeds.
    #[serde(rename = "TCP_CONNECT")]
    TcpConnect {
        /// The port names to probe — every declared port when the template
        /// named none.
        ports: Vec<String>,
        timeout_seconds: u64,
    },
    /// READY once `onReadinessCheck` exits 0 while `onRun` is running.
    #[serde(rename = "COMMAND")]
    Command {
        interval_seconds: u64,
        timeout_seconds: u64,
    },
    /// READY once `onRun` writes `openjd_service_ready: <message>`.
    #[serde(rename = "STDOUT")]
    Stdout { timeout_seconds: u64 },
}

impl ServiceReadinessCheck {
    /// The schema value of the `type` discriminator.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::TcpConnect { .. } => "TCP_CONNECT",
            Self::Command { .. } => "COMMAND",
            Self::Stdout { .. } => "STDOUT",
        }
    }

    /// The effective `timeoutSeconds`, whichever variant this is.
    pub fn timeout_seconds(&self) -> u64 {
        match self {
            Self::TcpConnect {
                timeout_seconds, ..
            }
            | Self::Command {
                timeout_seconds, ..
            }
            | Self::Stdout { timeout_seconds } => *timeout_seconds,
        }
    }
}

/// An instantiated `<ServiceRestartPolicy>` (§9.4), with the defaults
/// applied: `maxAttempts` 0, `completedTasks` `RERUN`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceRestartPolicy {
    /// How many times the scheduler relaunches the Service after a failure;
    /// the initial launch is not counted.
    pub max_attempts: u64,
    pub completed_tasks: CompletedTasksPolicy,
}

/// An instantiated `<ServiceScript>` (§9.5). `let_bindings` are the
/// `<ServiceScript>.let` bindings, evaluated on the service host.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceScript {
    #[serde(rename = "let", alias = "letBindings")]
    pub let_bindings: Option<Vec<String>>,
    pub actions: ServiceActions,
    pub embedded_files: Option<Vec<EmbeddedFile>>,
}

/// An instantiated `<ServiceActions>` (§9.6).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceActions {
    pub on_enter: Option<Action>,
    pub on_run: Action,
    pub on_readiness_check: Option<Action>,
    pub on_exit: Option<Action>,
}

impl ServiceActions {
    /// All four action slots paired with their camelCase schema name, in
    /// lifecycle order.
    pub fn named_slots(&self) -> [(&'static str, Option<&Action>); 4] {
        [
            ("onEnter", self.on_enter.as_ref()),
            ("onRun", Some(&self.on_run)),
            ("onReadinessCheck", self.on_readiness_check.as_ref()),
            ("onExit", self.on_exit.as_ref()),
        ]
    }

    /// The defined actions, each paired with its schema name, in lifecycle
    /// order.
    pub fn iter_named(&self) -> impl Iterator<Item = (&'static str, &Action)> {
        self.named_slots()
            .into_iter()
            .filter_map(|(name, slot)| slot.map(|a| (name, a)))
    }

    /// The defined actions, in lifecycle order, without names.
    pub fn iter_actions(&self) -> impl Iterator<Item = &Action> {
        self.iter_named().map(|(_, action)| action)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddedFile {
    pub name: String,
    #[serde(alias = "type")]
    pub file_type: FileType,
    pub filename: Option<String>,
    pub data: Option<FormatString>,
    pub runnable: Option<bool>,
    pub end_of_line: Option<EndOfLine>,
}

/// §5.3 CancelationMethod — discriminated union on `mode`.
///
/// `DeferredMode` carries a format-string `mode` (FEATURE_BUNDLE_1) whose
/// TERMINATE-vs-NOTIFY_THEN_TERMINATE decision is made at run time, right
/// before the action launches (in short: `mode` is the schema selector,
/// so it normally must be known at parse time, but a forwarded value like
/// `{{WrappedAction.Cancelation.Mode}}` only exists at run time — see
/// `specs/model/template-types.md` § CancelationMode for the full design
/// rationale). A `null` resolution (whole-field expressions only) means
/// the whole cancelation object is treated as never declared.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CancelationMode {
    Terminate,
    NotifyThenTerminate {
        notify_period_in_seconds: Option<FormatString>,
    },
    DeferredMode {
        mode: FormatString,
        notify_period_in_seconds: Option<FormatString>,
    },
}

// Manual serde impls: the wire shape is `{"mode": <string>, ...}` where a
// DeferredMode's `mode` is the raw format string. A serde `tag = "mode"`
// representation cannot express that (the tag would collide with the
// variant's own `mode` field), so both directions are hand-written.
impl Serialize for CancelationMode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        match self {
            CancelationMode::Terminate => {
                map.serialize_entry("mode", "TERMINATE")?;
            }
            CancelationMode::NotifyThenTerminate {
                notify_period_in_seconds,
            } => {
                map.serialize_entry("mode", "NOTIFY_THEN_TERMINATE")?;
                if let Some(n) = notify_period_in_seconds {
                    map.serialize_entry("notifyPeriodInSeconds", n)?;
                }
            }
            CancelationMode::DeferredMode {
                mode,
                notify_period_in_seconds,
            } => {
                map.serialize_entry("mode", mode)?;
                if let Some(n) = notify_period_in_seconds {
                    map.serialize_entry("notifyPeriodInSeconds", n)?;
                }
            }
        }
        map.end()
    }
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
        let deny_extra = |allowed: &[&str]| -> Result<(), D::Error> {
            if let Some(extra) = map.keys().find(|k| !allowed.contains(&k.as_str())) {
                return Err(serde::de::Error::custom(format!("unknown field `{extra}`")));
            }
            Ok(())
        };
        // An explicit null is treated as "not provided": the previous
        // derived impl serialized an unset period as
        // `"notifyPeriodInSeconds": null`, so documents written by released
        // versions must read back as None rather than failing.
        let notify = || -> Result<Option<FormatString>, D::Error> {
            map.get("notifyPeriodInSeconds")
                .filter(|v| !v.is_null())
                .map(|v| FormatString::deserialize(v.clone()))
                .transpose()
                .map_err(serde::de::Error::custom)
        };
        match mode {
            "TERMINATE" => {
                deny_extra(&["mode"])?;
                Ok(CancelationMode::Terminate)
            }
            "NOTIFY_THEN_TERMINATE" => {
                deny_extra(&["mode", "notifyPeriodInSeconds"])?;
                Ok(CancelationMode::NotifyThenTerminate {
                    notify_period_in_seconds: notify()?,
                })
            }
            other if other.contains("{{") => {
                deny_extra(&["mode", "notifyPeriodInSeconds"])?;
                let mode = FormatString::deserialize(mode_value.clone())
                    .map_err(serde::de::Error::custom)?;
                Ok(CancelationMode::DeferredMode {
                    mode,
                    notify_period_in_seconds: notify()?,
                })
            }
            other => Err(serde::de::Error::custom(format!(
                "unknown variant `{other}`, expected `TERMINATE` or `NOTIFY_THEN_TERMINATE`"
            ))),
        }
    }
}

/// Resolved parameter space with concrete ranges.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepParameterSpace {
    pub task_parameter_definitions: IndexMap<String, TaskParameter>,
    pub combination: Option<String>,
}

/// Manual because `IndexMap` has no `Hash`; definitions hash as
/// key-sorted entries to match `IndexMap`'s order-insensitive equality.
impl Hash for StepParameterSpace {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_map_entries(self.task_parameter_definitions.iter(), state);
        self.combination.hash(state);
    }
}

/// A resolved task parameter with concrete range values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskParameter {
    Int {
        range: TaskParamRange<i64>,
        chunks: Option<ResolvedChunks>,
    },
    Float {
        range: Vec<Float64>,
    },
    String {
        range: Vec<String>,
    },
    Path {
        range: Vec<String>,
    },
    ChunkInt {
        range: TaskParamRange<i64>,
        chunks: ResolvedChunks,
    },
}

/// Manual because `f64` has no `Hash`; those hash via `hash_f64`. A float range
/// element hashes its rendering too: same number, different spelling means a
/// different command line, so not the same job.
impl Hash for TaskParameter {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Int { range, chunks } => {
                range.hash(state);
                chunks.hash(state);
            }
            Self::Float { range } => {
                range.len().hash(state);
                for elem in range {
                    // Hash the rendering as well as the value, because `Float64`'s
                    // own `Hash` takes only the value and `PartialEq` compares both.
                    hash_f64(elem.value(), state);
                    elem.to_display_string().hash(state);
                }
            }
            Self::String { range } | Self::Path { range } => range.hash(state),
            Self::ChunkInt { range, chunks } => {
                range.hash(state);
                chunks.hash(state);
            }
        }
    }
}

/// A resolved range — either a concrete list or a RangeExpr.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    bound(deserialize = "T: serde::de::DeserializeOwned")
)]
pub enum TaskParamRange<T: Serialize> {
    List(Vec<T>),
    RangeExpr(RangeExpr),
}

/// Chunks config with all format strings resolved.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedChunks {
    pub default_task_count: usize,
    pub target_runtime_seconds: Option<usize>,
    pub range_constraint: RangeConstraint,
}

/// Resolved host requirements — no FormatStrings.
#[derive(Debug, Clone, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostRequirements {
    pub amounts: Option<Vec<AmountRequirement>>,
    pub attributes: Option<Vec<AttributeRequirement>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AmountRequirement {
    pub name: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// Manual because `f64` has no `Hash`; `min`/`max` hash via `hash_f64`.
impl Hash for AmountRequirement {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        for bound in [self.min, self.max] {
            match bound {
                None => false.hash(state),
                Some(v) => {
                    true.hash(state);
                    hash_f64(v, state);
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributeRequirement {
    pub name: String,
    pub any_of: Option<Vec<String>>,
    pub all_of: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepDependency {
    pub depends_on: String,
}
