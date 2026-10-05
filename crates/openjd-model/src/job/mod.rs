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
pub use crate::template::{CompletedTasksPolicy, RunScope, ServicePortProtocol, ServiceScope};
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
    /// The Job's Services (RFC 0009 `services`), in declaration order, each
    /// carrying its computed [`scope`](Service::scope). A Service is started
    /// before any Task of a Step in its scope is scheduled and stopped once
    /// no such Task remains; attached external Services (Template Schemas
    /// §1.2.2) precede the Job Template's own once
    /// [`apply_environment_templates`](crate::apply_environment_templates)
    /// has folded them in. `None` when none is declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub services: Option<Vec<Service>>,
    /// The external Services the Job Template requires (RFC 0009
    /// `requiresServices`, Template Schemas §9.8), in declaration order.
    /// Matched to attached Services at submission by
    /// [`apply_environment_templates`](crate::apply_environment_templates).
    /// `None` when none is declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_services: Option<Vec<ServiceRequirement>>,
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
        self.services.hash(state);
        self.requires_services.hash(state);
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
    /// query the effective scope with [`runs_in`](Self::runs_in). Job
    /// creation materializes the default here: an Environment without
    /// `runScope` that references `Service.*` is converted with `[TASK]`.
    /// Typed here (unlike the template side) because validation has already
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
    /// `onHealthCheck`, concurrently with `onWrapServiceRun`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_wrap_service_health_check: Option<Action>,
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
        ("onWrapServiceHealthCheck", on_wrap_service_health_check),
        ("onWrapServiceExit", on_wrap_service_exit),
        ("onExit", on_exit),
    ],
    wrap_hooks: [
        ("onWrapEnvEnter", on_wrap_env_enter, EnvName),
        ("onWrapTaskRun", on_wrap_task_run, StepName),
        ("onWrapEnvExit", on_wrap_env_exit, EnvName),
        ("onWrapServiceEnter", on_wrap_service_enter, Service),
        ("onWrapServiceRun", on_wrap_service_run, Service),
        ("onWrapServiceHealthCheck", on_wrap_service_health_check, Service),
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
                "onWrapServiceHealthCheck",
                &self.on_wrap_service_health_check,
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

/// The document of a submission that declares an entity: the Job Template,
/// or one of the Environment Templates the scheduler attached to it
/// (Template Schemas §1.2.2 "Services from Environment Templates", RFC 0009
/// "Inline Services shadow external ones").
///
/// Service names are unique within the list that declares them, and
/// nothing more: an external Service from an attached Environment Template
/// may share its `name` with a Service in the Job Template or in another
/// attachment (an error only when a `requiresServices` entry names it), and
/// a scheduler must keep same-named Services from different documents
/// distinct. A [`Service`] therefore carries the document that declares it
/// ([`Service::document`]), so a consumer keys Services on `(document,
/// name)`. A `Service.*` reference resolves within its own document, except
/// that a Job Template's reference to a required external Service resolves
/// to the attached Service the requirement was bound to
/// ([`crate::AppliedEnvironmentTemplates::requirement_bindings`]).
///
/// `Display` names the document the way the submission-time error paths
/// do: `JobTemplate`, the attachment's label when it has one (typically
/// the file path the template was read from), else `EnvironmentTemplate[i]`
/// with its 0-based attachment index.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum Document {
    /// The Job Template of the submission.
    #[default]
    JobTemplate,
    /// The Environment Template attached at 0-based position `index` in
    /// the scheduler's order.
    EnvironmentTemplate {
        /// Attachment index, the same indexing error paths use.
        index: usize,
        /// The label the caller gave the attachment, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
}

impl Document {
    /// The attached Environment Template at `index`, named `label` when
    /// given.
    #[must_use]
    pub fn environment_template(index: usize, label: Option<&str>) -> Self {
        Self::EnvironmentTemplate {
            index,
            label: label.map(str::to_string),
        }
    }

    /// True for the Job Template — the default, and the case omitted from
    /// a serialized [`Service`].
    #[must_use]
    pub fn is_job_template(&self) -> bool {
        matches!(self, Self::JobTemplate)
    }
}

impl std::fmt::Display for Document {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::JobTemplate => f.write_str("JobTemplate"),
            Self::EnvironmentTemplate {
                label: Some(label), ..
            } => f.write_str(label),
            Self::EnvironmentTemplate { index, label: None } => {
                write!(f, "EnvironmentTemplate[{index}]")
            }
        }
    }
}

/// An instantiated Service (RFC 0009 `<Service>`, Template Schemas §9) —
/// the result of job creation for one `services` entry of the Job Template,
/// or for a `services` entry of an attached Environment Template (an
/// external Service).
///
/// Job-creation-stage fields are resolved: the `<Service>.let` bindings
/// (into [`resolved_symtab`](Self::resolved_symtab)), the numeric
/// `@fmtstring` fields (`port`, the health-check seconds and threshold,
/// `maxAttempts`, with the §9 defaults applied where the template gave
/// none), and `hostRequirements`. The Service's [`scope`](Self::scope) and
/// the Services it [`references`](Self::references) are computed from the
/// template's `Service.*` references (§9.1), so a scheduler need not
/// re-derive them. `variables` and `script` are `@fmtstring[host]` and
/// remain `FormatString`s for the Service Session to resolve, exactly like
/// an [`Environment`]'s.
///
/// Two Services of a combined Job are the same Service iff their
/// [`document`](Self::document) and `name` agree: names are unique within
/// one document's list only (Template Schemas §1.2.2 item 2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Service {
    pub name: String,
    pub description: Option<String>,
    /// The document that declares this Service: [`Document::JobTemplate`]
    /// for the Job Template's own `services` (the default, omitted from
    /// JSON), or the attached Environment Template whose `services` list it
    /// came from. Set by `apply_environment_templates` for external
    /// Services. The Service's `Service.*` references to other inline
    /// Services resolve within this document.
    #[serde(default, skip_serializing_if = "Document::is_job_template")]
    pub document: Document,
    /// The Steps whose Tasks depend on this Service (Template Schemas §9.1),
    /// computed from the template's `Service.*` references:
    /// [`ServiceScope::AllSteps`] for a Service a Job Environment references,
    /// one nothing references, one a Job-wide Service references, or an
    /// external Service. A scheduler starts the Service before the first Task
    /// of any Step in the scope and stops it once none has a Task left.
    #[serde(default = "ServiceScope::all_steps_default")]
    pub scope: ServiceScope,
    /// The names of the other Services **of the same document** this Service
    /// references through `Service.<name>.*` (§9.1 rule 3): it starts only
    /// after each is READY and is stopped before any of them. Sorted; never
    /// contains the Service's own name or a required external Service's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<String>,
    /// §9 item 4: the Steps that must complete before this Service is
    /// started, in addition to its other start conditions. Never set on an
    /// external Service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<Vec<StepDependency>>,
    /// Resolved host requirements the service host must satisfy.
    pub host_requirements: Option<HostRequirements>,
    /// The declared ports, in declaration order.
    pub ports: Vec<ServicePort>,
    /// The effective health check, with the §9.3 defaults applied.
    pub health_check: ServiceHealthCheck,
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
        self.document.hash(state);
        self.scope.hash(state);
        self.references.hash(state);
        self.dependencies.hash(state);
        self.host_requirements.hash(state);
        self.ports.hash(state);
        self.health_check.hash(state);
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

    /// The declared port named `name`, if any.
    #[must_use]
    pub fn port(&self, name: &str) -> Option<&ServicePort> {
        self.ports.iter().find(|p| p.name == name)
    }
}

/// An instantiated `<ServiceRequirement>` (Template Schemas §9.8): the Job
/// Template reads the endpoint of an external Service named `name` on the
/// ports listed. [`apply_environment_templates`](crate::apply_environment_templates)
/// matches it to exactly one attached Service with that `name` declaring
/// every listed port with the same protocol.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceRequirement {
    /// The required Service's `name`; distinct from every inline Service's.
    pub name: String,
    /// The ports the Job Template uses, in declaration order.
    pub ports: Vec<ServiceRequirementPort>,
}

impl ServiceRequirement {
    /// The names of the declared ports, in declaration order.
    pub fn port_names(&self) -> impl Iterator<Item = &str> {
        self.ports.iter().map(|p| p.name.as_str())
    }
}

/// An instantiated `<ServiceRequirementPort>` (§9.8.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceRequirementPort {
    /// The `name` of a port the external Service must declare.
    pub name: String,
    /// The protocol the port must carry; `TCP` (the default, omitted from
    /// JSON) or `UDP`.
    #[serde(default, skip_serializing_if = "ServicePortProtocol::is_default")]
    pub protocol: ServicePortProtocol,
}

/// An instantiated `<ServicePort>` (§9.2).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServicePort {
    /// The second component of `Service.<service>.<port>.*` references.
    pub name: String,
    /// The specific port number, in the space of [`protocol`](Self::protocol),
    /// the Service requires on its host, or `None` when the runtime
    /// allocates one. Resolved from the template's `@fmtstring`; a
    /// whole-field `null` resolution is `None`.
    pub port: Option<u16>,
    /// §9.2 item 3: the transport protocol of the port, `TCP` (the
    /// default, omitted from JSON) or `UDP`. The runtime requests or
    /// allocates the number in this protocol's space and publishes or
    /// forwards the port as it.
    #[serde(default, skip_serializing_if = "ServicePortProtocol::is_default")]
    pub protocol: ServicePortProtocol,
}

/// An instantiated `<ServiceHealthCheck>` (§9.3), with the defaults
/// applied: `readinessIntervalSeconds` 1 (`TCP_CONNECT`) or 5 (`COMMAND`),
/// `readinessTimeoutSeconds` 300, `healthIntervalSeconds` 30 (no default for
/// `STDOUT`, where it is the opt-in heartbeat), `failureThreshold` 3, and a
/// `TCP_CONNECT` check without `ports` probing every declared TCP port.
///
/// One probe mechanism in two phases: before READY a probe runs on launch
/// and every `readiness_interval_seconds`, bounded by
/// `readiness_timeout_seconds`; after READY one runs every
/// `health_interval_seconds`, and `failure_threshold` consecutive failures
/// make the instance UNHEALTHY. Intervals are measured from the end of the
/// previous probe.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum ServiceHealthCheck {
    /// A probe succeeds when a TCP connection to each of `ports` succeeds.
    #[serde(rename = "TCP_CONNECT")]
    TcpConnect {
        /// The names of the TCP ports to probe — every declared TCP port
        /// when the template named none (§9 item 6, §9.3 item 2).
        ports: Vec<String>,
        readiness_interval_seconds: u64,
        readiness_timeout_seconds: u64,
        health_interval_seconds: u64,
        failure_threshold: u64,
    },
    /// A probe is one `onHealthCheck` invocation; exit 0 while `onRun` is
    /// running is a success.
    #[serde(rename = "COMMAND")]
    Command {
        readiness_interval_seconds: u64,
        readiness_timeout_seconds: u64,
        health_interval_seconds: u64,
        failure_threshold: u64,
    },
    /// A probe is an `openjd_service_ready: <message>` line on `onRun`'s
    /// stdout: the first makes the instance READY; afterwards one is
    /// expected every `health_interval_seconds` when that is `Some`.
    #[serde(rename = "STDOUT")]
    Stdout {
        readiness_timeout_seconds: u64,
        /// The heartbeat interval, or `None` when no heartbeat is expected
        /// and the instance's health is that `onRun` is still running.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        health_interval_seconds: Option<u64>,
        /// Meaningful only when `health_interval_seconds` is `Some`.
        failure_threshold: u64,
    },
}

impl ServiceHealthCheck {
    /// The schema value of the `type` discriminator.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::TcpConnect { .. } => "TCP_CONNECT",
            Self::Command { .. } => "COMMAND",
            Self::Stdout { .. } => "STDOUT",
        }
    }

    /// The effective `readinessIntervalSeconds`; `None` for `STDOUT`,
    /// which has no readiness probe to space.
    pub fn readiness_interval_seconds(&self) -> Option<u64> {
        match self {
            Self::TcpConnect {
                readiness_interval_seconds,
                ..
            }
            | Self::Command {
                readiness_interval_seconds,
                ..
            } => Some(*readiness_interval_seconds),
            Self::Stdout { .. } => None,
        }
    }

    /// The effective `readinessTimeoutSeconds`, whichever variant this is.
    pub fn readiness_timeout_seconds(&self) -> u64 {
        match self {
            Self::TcpConnect {
                readiness_timeout_seconds,
                ..
            }
            | Self::Command {
                readiness_timeout_seconds,
                ..
            }
            | Self::Stdout {
                readiness_timeout_seconds,
                ..
            } => *readiness_timeout_seconds,
        }
    }

    /// The effective `healthIntervalSeconds`: always `Some` for
    /// `TCP_CONNECT` and `COMMAND`; for `STDOUT`, `Some` only when the
    /// template opted into the heartbeat.
    pub fn health_interval_seconds(&self) -> Option<u64> {
        match self {
            Self::TcpConnect {
                health_interval_seconds,
                ..
            }
            | Self::Command {
                health_interval_seconds,
                ..
            } => Some(*health_interval_seconds),
            Self::Stdout {
                health_interval_seconds,
                ..
            } => *health_interval_seconds,
        }
    }

    /// The effective `failureThreshold`, whichever variant this is.
    pub fn failure_threshold(&self) -> u64 {
        match self {
            Self::TcpConnect {
                failure_threshold, ..
            }
            | Self::Command {
                failure_threshold, ..
            }
            | Self::Stdout {
                failure_threshold, ..
            } => *failure_threshold,
        }
    }

    /// Whether the instance is monitored after READY: `true` unless this
    /// is a `STDOUT` check without `healthIntervalSeconds`.
    #[must_use]
    pub fn monitors_health(&self) -> bool {
        self.health_interval_seconds().is_some()
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
    pub on_health_check: Option<Action>,
    pub on_exit: Option<Action>,
}

impl ServiceActions {
    /// All four action slots paired with their camelCase schema name, in
    /// lifecycle order.
    pub fn named_slots(&self) -> [(&'static str, Option<&Action>); 4] {
        [
            ("onEnter", self.on_enter.as_ref()),
            ("onRun", Some(&self.on_run)),
            ("onHealthCheck", self.on_health_check.as_ref()),
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
