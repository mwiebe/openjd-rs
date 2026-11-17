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
pub use crate::template::{
    CompletedTasksPolicy, DependencyTarget, ServicePortProtocol, ServiceScope, StepKey,
};
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
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
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
    /// [`create_job_with_environment_templates`](crate::create_job_with_environment_templates)
    /// has folded them in. `None` when none is declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub services: Option<Vec<Service>>,
    /// The external Services the Job Template requires (RFC 0009
    /// `externalServices`, Template Schemas §9.8), in declaration order.
    /// Matched to attached Services at submission by
    /// [`create_job_with_environment_templates`](crate::create_job_with_environment_templates).
    /// `None` when none is declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_services: Option<Vec<ServiceRequirement>>,
}

impl Job {
    /// True when the Job was created from a template declaring `extension`.
    #[must_use]
    pub fn has_extension(&self, extension: crate::types::ModelExtension) -> bool {
        self.extensions
            .as_ref()
            .is_some_and(|exts| exts.contains(&extension))
    }

    /// True when the Job declares the `SERVICE` extension, under which a
    /// [`StepDependency`] may name a Service with its `service` key and the
    /// Job may have `services` and `externalServices`.
    #[must_use]
    pub fn service_active(&self) -> bool {
        self.has_extension(crate::types::ModelExtension::Service)
    }
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
        self.external_services.hash(state);
    }
}

/// A resolved job parameter (name + type + bound value).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub struct JobParameter {
    pub name: String,
    pub param_type: JobParameterType,
    pub value: ExprValue,
}

/// A fully instantiated step.
#[derive(Debug, Clone, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub struct Step {
    pub name: String,
    pub description: Option<String>,
    pub script: StepScript,
    /// The Step's own Environments, entered after its Job Environments, in
    /// written order. With `ENV_PARAMS`, one whose `enableIf` (Template
    /// Schemas §4 item 6) is `false` for the Step is left out; `None` when
    /// that leaves none.
    pub step_environments: Option<Vec<Environment>>,
    pub parameter_space: Option<StepParameterSpace>,
    /// The Step's resolved `hostRequirements`. When an Environment the
    /// Step's Sessions enter has `hostRequirements` (`ENV_PARAMS` in that
    /// Environment's document), these are its **combined** host
    /// requirements (Template Schemas §3.3.3): its
    /// own and those of every Environment its Sessions enter, each
    /// Environment's resolved with this Step's effective values,
    /// normalized per capability (names compared ignoring case). A
    /// scheduler reads the combined set here; the Environments' own
    /// requirements are not stored. A Step none of whose entered
    /// Environments has requirements keeps its own, unchanged. In a
    /// document that lists `ENV_PARAMS`, a requirement that resolved to
    /// nothing was discarded first (Template Schemas §3.3), so this is
    /// `None` when none are left (listed in
    /// [`CreatedJob::discarded_host_requirements`](crate::CreatedJob::discarded_host_requirements)).
    pub host_requirements: Option<HostRequirements>,
    /// The Steps and, with `SERVICE`, the Services this Step depends on, as
    /// written (Template Schemas §3 item 4): the Step's Tasks are scheduled
    /// only once every listed Step has completed and every listed Service is
    /// READY. Classify an entry with [`StepDependency::target`].
    pub dependencies: Option<Vec<StepDependency>>,
    /// The step-scope symbols that this Step's step-level `let` bindings
    /// and host-side format strings (its script and its Step Environments)
    /// reference, in JSON transport format: the referenced subset of
    /// non-PATH `Param.*`, `RawParam.*`, `Job.Name`, `Job.EnvNames`
    /// (`ENV_PARAMS`), `Step.Name`, and the
    /// step-level `let` bindings, plus `RawParam.X` for each referenced
    /// PATH `Param.X`. The session deserializes this with
    /// PathFormat::host() and layers Session.* and Task.* values on top at
    /// runtime.
    #[serde(rename = "resolvedSymTab", skip_serializing_if = "Option::is_none")]
    pub resolved_symtab: Option<SerializedSymbolTable>,
    /// The effective Environment parameter values of this Step
    /// (`ENV_PARAMS`, Template Schemas §3.2.2): one entry for each
    /// Environment the Step's Sessions enter that declares
    /// `envParameterDefinitions`, in the order the Sessions enter them. An
    /// Environment whose `enableIf` (§4 item 6) is `false` for the Step, or
    /// that an earlier one of its `exclusiveGroup` supersedes (§4 item 7),
    /// is not entered, so it has no entry.
    /// `None` when there is none, so a Job without Environment parameters
    /// serializes exactly as before. Compare two Steps' values with
    /// [`Step::environment_values_equal`], not with `==`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_parameters: Option<Vec<EnvironmentParameterValues>>,
    /// The Job Environments this Step's Sessions enter (`ENV_PARAMS`,
    /// Template Schemas §4 item 4, §3.2.2), in the order of
    /// [`Job::job_environments`]: every one with `activation: ALWAYS` or from
    /// an Environment Template that does not list `ENV_PARAMS`, which behaves
    /// as `ALWAYS` and on which a disabled entry has no effect (§4 item 4),
    /// every one with `activation: BY_DEFAULT` that the Step does not decline
    /// with a disabled `environment:` entry (§3.2 item 3.2), and every one with
    /// `activation: ON_REQUEST` that an enabled entry of the Step requests,
    /// less any whose own
    /// `enableIf` (§4 item 6) is `false` for the Step (for an `ALWAYS` one,
    /// that is a job-creation error, §3.2.2 item 6) and any that an earlier
    /// one of its `exclusiveGroup` supersedes (§4 item 7). Each names an
    /// entry of `Job::job_environments` by `(document, environment)`.
    ///
    /// `None` means every Job Environment, which is every Step of a Job
    /// whose documents use no `activation: ON_REQUEST`, no Environment
    /// `enableIf` or `exclusiveGroup`, and no disabled `environment:` entry,
    /// so such a Job serializes exactly as before. A Job from
    /// [`create_job`](crate::create_job()) never holds `Some` of the full
    /// list, so [`Step::same_environments`] can compare the field directly.
    /// The Step's own `step_environments` are also in its Environment set,
    /// entered after these, and are not listed here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environments: Option<Vec<EnvironmentRef>>,
}

impl Step {
    /// Whether this Step's Sessions enter the Job Environment `name`
    /// declared by `document` (Template Schemas §4 item 4): true for every
    /// Job Environment when [`environments`](Self::environments) is `None`,
    /// else when it lists `(document, name)`.
    #[must_use]
    pub fn enters_environment(&self, document: &Document, name: &str) -> bool {
        self.environments.as_ref().is_none_or(|refs| {
            refs.iter()
                .any(|r| &r.document == document && r.environment == name)
        })
    }

    /// Whether this Step's Sessions enter the same Job Environments as
    /// `other`'s (How Jobs Are Run, Environment Parameters): one Session
    /// may run Tasks of both Steps without exiting and entering Job
    /// Environments between them only when this holds and
    /// [`environment_values_equal`](Self::environment_values_equal) does.
    /// (Each Step's own `step_environments` are entered and exited around
    /// its Tasks.) Compares
    /// [`environments`](Self::environments), whose `None` is the full list.
    #[must_use]
    pub fn same_environments(&self, other: &Step) -> bool {
        self.environments == other.environments
    }

    /// Whether this Step's effective Environment parameter values equal
    /// `other`'s (Template Schemas §3.2.2): for every Environment either
    /// Step has values for, both have values for it, and each parameter has
    /// the same type and the same string form, the text
    /// `{{Env.RawParam.<name>}}` gives it.
    ///
    /// This is the comparison a scheduler uses to decide whether one Session
    /// may run Tasks of both Steps without exiting and entering Environments
    /// between them. A value compares by its transport form, which is its
    /// string form, or for a list the string forms of its elements: a FLOAT
    /// (or an element of a `LIST[FLOAT]`) keeps the text it was written with,
    /// so `3.10` and `3.1` are not equal although they are the same number;
    /// a STRING compares by code points, with no normalization; a PATH by
    /// its unmapped string; an INT by its one string form, so `1` and `"1"`
    /// are equal. For the Steps of a Job from
    /// [`create_job`](crate::create_job()), it differs from `==` on the
    /// [`environment_parameters`](Self::environment_parameters) field only
    /// for a RANGE_EXPR, which compares by the integers it denotes (its
    /// canonical form), so `1-3` and `1,2,3` are equal even in a
    /// deserialized Job that holds another spelling; and in matching
    /// Environments by `(document, environment)`, so the order of the list
    /// does not matter.
    ///
    /// A FLOAT given as a YAML or JSON number has no text it was written
    /// with, so `create_job` writes it as a computed float is written
    /// (Template Schemas §3.2.2): the number `1.50` is `1.5`, equal to
    /// `"1.5"` and not to `"1.50"`, and the number `2` is `2.0`.
    ///
    /// The comparison assumes values in the form `create_job` writes them. A
    /// deserialized Job whose INT or BOOL values are spelled another way
    /// (`"007"` for an INT, `"yes"` for a BOOL) compares them by that text,
    /// so values a Session would render alike can compare unequal. (A
    /// FLOAT's stored text is the text a Session renders, so for a FLOAT the
    /// comparison is exact.) That can cost an exit and enter
    /// that was not needed, but never shares an Environment between values
    /// it can tell apart.
    #[must_use]
    pub fn environment_values_equal(&self, other: &Step) -> bool {
        fn index(
            values: &[EnvironmentParameterValues],
        ) -> Option<HashMap<(&Document, &str), &EnvironmentParameterValues>> {
            let index: HashMap<_, _> = values
                .iter()
                .map(|v| ((&v.document, v.environment.as_str()), v))
                .collect();
            // A list with a repeated key is not one `create_job` produces;
            // it is unequal to anything, including itself, so a repeat
            // cannot hide a difference, whichever side it is on.
            (index.len() == values.len()).then_some(index)
        }
        let empty: &[EnvironmentParameterValues] = &[];
        let a = self.environment_parameters.as_deref().unwrap_or(empty);
        let b = other.environment_parameters.as_deref().unwrap_or(empty);
        let (Some(ia), Some(ib)) = (index(a), index(b)) else {
            return false;
        };
        ia.len() == ib.len()
            && ia
                .iter()
                .all(|(key, va)| ib.get(key).is_some_and(|vb| va.values_equal(vb)))
    }
}

/// The document of a submission that declares something (Template Schemas
/// §1.2): the Job Template, or one of the Environment Templates the
/// scheduler attaches.
///
/// Environment names are unique within one document but not across
/// documents: an attached Environment may share its `name` with a Job
/// Environment (§3.2.1 item 4) or with another attachment's. A consumer
/// therefore keys an Environment's parameter values on `(document, name)`.
///
/// Service names (RFC 0009, Template Schemas §1.2.2 "Services from
/// Environment Templates") are likewise unique within the list that
/// declares them, and nothing more: an external Service from an attached
/// Environment Template may share its `name` with a Service in the Job
/// Template or in another attachment (an error only when an
/// `externalServices` entry names it), and a scheduler must keep same-named
/// Services from different documents distinct. A [`Service`] therefore
/// carries the document that declares it ([`Service::document`]), so a
/// consumer keys Services on `(document, name)`. A `Service.*` reference
/// resolves within its own document, except that a Job Template's
/// reference to a required external Service resolves to the attached
/// Service the requirement was bound to
/// ([`crate::CreatedJob::requirement_bindings`]).
///
/// `Display` names the document the way the submission-time error paths
/// do: `JobTemplate`, the attachment's label when it has one (typically the
/// file path the template was read from), else `EnvironmentTemplate[i]`
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

/// The effective parameter values of one Environment for one Step
/// (`ENV_PARAMS`, Template Schemas §3.2.2).
///
/// `values` is a symbol table in transport form with one entry per
/// parameter of the Environment, keyed by the bare parameter name. Each
/// entry's `type` is the parameter's own expression type (`path` for PATH,
/// `list[path]` for `LIST[PATH]`, `int` for INT, ...), so a Session can tell
/// which values to path-map. PATH values are unmapped strings of any form,
/// used as given (§4.5 item 3). A FLOAT value keeps the text it was
/// written with, which is the text a Session renders, so `3.10` and `3.1`
/// are different values (§3.2.2). A RANGE_EXPR value is its
/// canonical form, so two values that denote the same integers have one
/// transport form (§3.2.2);
/// a Session parses it. A Session binds them as `Env.Param.<name>` (path-mapped) and
/// `Env.RawParam.<name>` (unmapped).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentParameterValues {
    /// The document that declares the Environment.
    pub document: Document,
    /// The Environment's `name`.
    pub environment: String,
    /// One entry per parameter, keyed by parameter name.
    pub values: SerializedSymbolTable,
}

impl EnvironmentParameterValues {
    /// Whether `self` and `other` hold equal values (Template Schemas
    /// §3.2.2), ignoring `document` and `environment`. See
    /// [`Step::environment_values_equal`] for the rule.
    #[must_use]
    pub fn values_equal(&self, other: &EnvironmentParameterValues) -> bool {
        transport_tables_equal(self.values.as_value(), other.values.as_value())
    }
}

/// §3.2.2 equality over two symbol tables in transport form: the same
/// names in the same (canonical) order, the same types, and the same
/// string form for each value, which is its transport form (for a list,
/// the transport forms of its elements), except that a `range_expr`
/// compares by the integers it denotes, its canonical form.
fn transport_tables_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    let (Some(a), Some(b)) = (a.as_array(), b.as_array()) else {
        return a == b;
    };
    a.len() == b.len()
        && a.iter().zip(b).all(|(ea, eb)| {
            let ty = ea.get("type");
            ea.get("name") == eb.get("name")
                && ty == eb.get("type")
                && match (ea.get("value"), eb.get("value")) {
                    (Some(va), Some(vb)) => {
                        if ty.and_then(serde_json::Value::as_str) == Some("range_expr") {
                            range_expr_transport_equal(va, vb)
                        } else {
                            va == vb
                        }
                    }
                    (va, vb) => va == vb,
                }
        })
}

/// Compare two `range_expr` transport values by the integers they denote,
/// that is, by canonical form. `create_job` stores the canonical form, but a
/// deserialized Job may hold another spelling. A value that does not parse
/// (not one `create_job` makes) compares by its text.
fn range_expr_transport_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    let canonical = |v: &serde_json::Value| {
        v.as_str()
            .and_then(|s| s.parse::<openjd_expr::RangeExpr>().ok())
            .map(|r| r.to_string())
    };
    match (canonical(a), canonical(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// One Job Environment of a Job, named by the document that declares it
/// and its `name` (`ENV_PARAMS`): the key of [`Step::environments`], as of
/// [`EnvironmentParameterValues`]. Names are unique within one document
/// but not across documents (Template Schemas §3.2.1 item 4).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentRef {
    /// The document that declares the Environment.
    pub document: Document,
    /// The Environment's `name`.
    pub environment: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub struct StepScript {
    #[serde(rename = "let", alias = "letBindings")]
    pub let_bindings: Option<Vec<String>>,
    pub actions: StepActions,
    pub embedded_files: Option<Vec<EmbeddedFile>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub struct StepActions {
    pub on_run: Action,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub struct Action {
    pub command: FormatString,
    pub args: Option<Vec<FormatString>>,
    pub timeout: Option<FormatString>,
    pub cancelation: Option<CancelationMode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub struct Environment {
    pub name: String,
    pub description: Option<String>,
    /// RFC 0009 `dependencies` (Template Schemas §4 item 3): the Services
    /// this Environment lists with the `service` key, as written. Carried for a
    /// `jobEnvironments` entry and an attached Environment Template's
    /// `environment` (a Step Environment never has one); validation has
    /// rejected every entry that is not a Service of the Environment's own
    /// document or, in the Job Template, a required one. The Services
    /// listed are exactly those whose `Service.<name>.<port>.*` the
    /// Environment's format strings may reference, and a runtime seeds them
    /// from this list ([`depends_on_services`](Self::depends_on_services)).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<Vec<StepDependency>>,
    pub script: Option<EnvironmentScript>,
    pub variables: Option<HashMap<String, FormatString>>,
    /// Filtered symbol table containing only symbols referenced by this
    /// environment's format strings (variables, actions, embedded files, let bindings).
    #[serde(rename = "resolvedSymTab", skip_serializing_if = "Option::is_none")]
    pub resolved_symtab: Option<SerializedSymbolTable>,
}

impl Environment {
    /// The Services this Environment lists in its `dependencies` with the
    /// `service` key (Template Schemas §4 item 3), in list order — the
    /// Services whose `port` / `connectAddress` its format strings may
    /// reference. Empty for a Step Environment, which follows its Step's
    /// list.
    pub fn depends_on_services(&self) -> impl Iterator<Item = &str> {
        self.dependencies
            .iter()
            .flatten()
            .filter_map(|d| d.service())
    }

    /// True when this Environment lists `service: <name>` in its
    /// `dependencies`.
    #[must_use]
    pub fn depends_on_service(&self, name: &str) -> bool {
        self.depends_on_services().any(|n| n == name)
    }
}

/// Manual because `HashMap` has no `Hash`; `variables` hashes as
/// key-sorted entries to match `HashMap`'s order-insensitive equality.
impl Hash for Environment {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.description.hash(state);
        self.dependencies.hash(state);
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
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub struct EnvironmentScript {
    #[serde(rename = "let", alias = "letBindings")]
    pub let_bindings: Option<Vec<String>>,
    pub actions: EnvironmentActions,
    pub embedded_files: Option<Vec<EmbeddedFile>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
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

/// An instantiated Service (RFC 0009 `<Service>`, Template Schemas §9) —
/// the result of job creation for one `services` entry of the Job Template,
/// or for a `services` entry of an attached Environment Template (an
/// external Service).
///
/// Job-creation-stage fields are resolved: the `<Service>.let` bindings
/// (into [`resolved_symtab`](Self::resolved_symtab)), the numeric
/// `@fmtstring` fields (`port`, the health-check seconds and threshold,
/// `maxRestarts`, with the §9 defaults applied where the template gave
/// none), and `hostRequirements`. The Service's [`scope`](Self::scope) is
/// computed from the template's `dependencies` lists (§9.1), so a scheduler
/// need not re-derive it; the Steps and Services this Service itself depends
/// on are its [`dependencies`](Self::dependencies), split by kind with
/// [`depends_on_steps`](Self::depends_on_steps) and
/// [`depends_on_services`](Self::depends_on_services). `variables` and
/// `script` are `@fmtstring[host]` and remain `FormatString`s for the
/// Service Session to resolve, exactly like an [`Environment`]'s.
///
/// Two Services of a combined Job are the same Service iff their
/// [`document`](Self::document) and `name` agree: names are unique within
/// one document's list only (Template Schemas §1.2.2 item 3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Service {
    /// The Service's `name`, the first component of `Service.*` references.
    pub name: String,
    /// The resolved `description`, if any.
    pub description: Option<String>,
    /// The document that declares this Service: [`Document::JobTemplate`]
    /// for the Job Template's own `services` (the default, omitted from
    /// JSON), or the attached Environment Template whose `services` list it
    /// came from. Set by the submission stage of `create_job_with_environment_templates` for external
    /// Services. A `service: <name>` dependency of this Service names a
    /// Service of this document (or, in the Job Template, a required one).
    #[serde(default, skip_serializing_if = "Document::is_job_template")]
    pub document: Document,
    /// The Steps whose Tasks depend on this Service (Template Schemas §9.1),
    /// computed from the template's `dependencies` lists: the Steps that list
    /// it with the `service` key, those in the scope of a Service that lists it,
    /// and, for a Service a Job Environment lists, the Steps whose Sessions
    /// enter that Environment ([`ServiceScope::AllSteps`] when every Step
    /// does, as for an `ALWAYS` one, or a `BY_DEFAULT` one no Step declines
    /// or disables); the same holds for one a Job-wide Service depends on. An external Service's scope is
    /// computed by the same rules over the combined Job at submission
    /// ([`create_job_with_environment_templates`](crate::create_job_with_environment_templates));
    /// it is empty when nothing in the Job lists it. A scheduler
    /// starts the Service before the first Task of any Step in the scope and
    /// stops it once none has a Task left.
    #[serde(default = "ServiceScope::all_steps_default")]
    pub scope: ServiceScope,
    /// §9 item 4: the Steps and Services this Service depends on, as written
    /// — `dependsOn: <StepName>`, or `service: <name>` for a Service of the
    /// same document or a required one. The Service is started only after every listed
    /// Step has completed and every listed Service is READY, and stopped
    /// before any Service it lists. See [`depends_on_steps`](Self::depends_on_steps)
    /// and [`depends_on_services`](Self::depends_on_services).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<Vec<StepDependency>>,
    /// The Service's resolved `hostRequirements`. When an Environment its
    /// Sessions enter has `hostRequirements`, these are its **combined**
    /// host requirements (RFC 0009 `<Service>` item 5, Template Schemas
    /// §3.3.3): its own and those of every Environment its Sessions enter,
    /// each Environment's resolved with this Service's effective values,
    /// normalized per capability, as for a [`Step`]. A scheduler places the
    /// Service only on a host that satisfies them.
    pub host_requirements: Option<HostRequirements>,
    /// The effective Environment parameter values of this Service
    /// (`ENV_PARAMS`, Template Schemas §3.2.2 applied to a Service, RFC
    /// 0009 `<Service>` item 4): one entry for each Environment its
    /// Sessions enter that declares `envParameterDefinitions`, in the order
    /// they are entered. `None` when there is none. A Service Session
    /// enters each Environment with these values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_parameters: Option<Vec<EnvironmentParameterValues>>,
    /// The Job Environments this Service's Sessions enter (RFC 0009
    /// "Services run inside Environments"), in the order of
    /// [`Job::job_environments`], chosen exactly as a Step's are: every one
    /// with `activation: ALWAYS` and every one with `BY_DEFAULT` that the
    /// Service does not decline with a disabled `environment:` entry, and
    /// every one with `activation: ON_REQUEST` that an enabled
    /// `environment:` entry of the Service requests, less any whose own
    /// `enableIf` is `false` for the Service and any that an earlier one of
    /// its `exclusiveGroup` supersedes. A Service of the Job Template chooses
    /// among every Job Environment, and one of an Environment Template only
    /// among those of its own document and of the Environment Templates
    /// attached before it. `None` means every Job Environment, as for
    /// [`Step::environments`]; query it with
    /// [`enters_environment`](Self::enters_environment).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environments: Option<Vec<EnvironmentRef>>,
    /// The declared ports, in declaration order.
    pub ports: Vec<ServicePort>,
    /// The effective health check, with the §9.4 defaults applied.
    pub health_check: ServiceHealthCheck,
    /// The effective restart policy, with the §9.5 default `maxRestarts`
    /// applied and `completedTasks` resolved (required when `maxRestarts`
    /// is greater than 0; see [`ServiceRestartPolicy`]).
    pub restart_policy: ServiceRestartPolicy,
    /// Environment variables set for every action of the Service's script
    /// (session scope — resolved on the service host). Not propagated to
    /// the entities in the Service's scope.
    pub variables: Option<HashMap<String, FormatString>>,
    /// The Service's actions and embedded files (§9.6).
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
        self.dependencies.hash(state);
        self.host_requirements.hash(state);
        self.environment_parameters.hash(state);
        self.environments.hash(state);
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
    /// Whether this Service's Sessions enter the Job Environment `name`
    /// declared by `document` (RFC 0009 "Services run inside
    /// Environments"): true for every Job Environment when
    /// [`environments`](Self::environments) is `None`, else when it lists
    /// `(document, name)`.
    #[must_use]
    pub fn enters_environment(&self, document: &Document, name: &str) -> bool {
        self.environments.as_ref().is_none_or(|refs| {
            refs.iter()
                .any(|r| &r.document == document && r.environment == name)
        })
    }

    /// The names of the declared ports, in declaration order.
    pub fn port_names(&self) -> impl Iterator<Item = &str> {
        self.ports.iter().map(|p| p.name.as_str())
    }

    /// The Steps this Service lists in its `dependencies` (§9 item 4), in
    /// list order: it starts only after each has completed.
    pub fn depends_on_steps(&self) -> impl Iterator<Item = &str> {
        self.dependencies.iter().flatten().filter_map(|d| d.step())
    }

    /// The Services this Service lists in its `dependencies` with the
    /// `service` key (§9 item 4), in list order: it starts only after each
    /// is READY and is stopped before any of them. A name is a Service of
    /// this Service's [`document`](Self::document) or, in the Job Template,
    /// a required external Service.
    pub fn depends_on_services(&self) -> impl Iterator<Item = &str> {
        self.dependencies
            .iter()
            .flatten()
            .filter_map(|d| d.service())
    }

    /// True when this Service lists `service: <name>` in its `dependencies`.
    #[must_use]
    pub fn depends_on_service(&self, name: &str) -> bool {
        self.depends_on_services().any(|n| n == name)
    }

    /// The declared port named `name`, if any.
    #[must_use]
    pub fn port(&self, name: &str) -> Option<&ServicePort> {
        self.ports.iter().find(|p| p.name == name)
    }
}

/// An instantiated `<ServiceRequirement>` (Template Schemas §9.8): the Job
/// Template reads the endpoint of an external Service named `name` on the
/// ports listed. [`create_job_with_environment_templates`](crate::create_job_with_environment_templates)
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

/// An instantiated `<ServicePort>` (§9.3).
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
    /// §9.3 item 3: the transport protocol of the port, `TCP` (the
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
        /// when the template named none (§9 item 7, §9.4 item 2).
        ports: Vec<String>,
        /// Seconds between probes before READY (§9.4 item 3).
        readiness_interval_seconds: u64,
        /// Seconds from `onRun`'s launch until an instance that is not yet
        /// READY has failed (§9.4 item 4).
        readiness_timeout_seconds: u64,
        /// Seconds between probes after READY (§9.4 item 5).
        health_interval_seconds: u64,
        /// Consecutive failed probes after READY that make the instance
        /// UNHEALTHY (§9.4 item 6).
        failure_threshold: u64,
    },
    /// A probe is one `onHealthCheck` invocation; exit 0 while `onRun` is
    /// running is a success.
    #[serde(rename = "COMMAND")]
    Command {
        /// Seconds between probes before READY (§9.4 item 3).
        readiness_interval_seconds: u64,
        /// Seconds from `onRun`'s launch until an instance that is not yet
        /// READY has failed (§9.4 item 4).
        readiness_timeout_seconds: u64,
        /// Seconds between probes after READY (§9.4 item 5).
        health_interval_seconds: u64,
        /// Consecutive failed probes after READY that make the instance
        /// UNHEALTHY (§9.4 item 6).
        failure_threshold: u64,
    },
    /// A probe is an `openjd_service_ready: <message>` line on `onRun`'s
    /// stdout: the first makes the instance READY; afterwards one is
    /// expected every `health_interval_seconds` when that is `Some`.
    #[serde(rename = "STDOUT")]
    Stdout {
        /// Seconds from `onRun`'s launch until an instance that is not yet
        /// READY has failed (§9.4 item 4).
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

/// An instantiated `<ServiceRestartPolicy>` (§9.5), with `maxRestarts`
/// resolved (default 0) and `completedTasks` as the template gave it.
///
/// `completedTasks` has no default. Job creation requires it when the
/// resolved `maxRestarts` is greater than 0 (§9.5 item 2), so
/// a Service that can be relaunched always says what a relaunch means for
/// its completed Tasks; a Service with `maxRestarts` 0 may omit it, and
/// then the field matters only when the Service is stopped and started
/// again because a Service it lists began a new Service Session — where an
/// omitted value is read as `RERUN`
/// ([`completed_tasks_on_dependent_restart`](Self::completed_tasks_on_dependent_restart)).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceRestartPolicy {
    /// How many times the scheduler relaunches the Service after a failure;
    /// the initial launch is not counted.
    pub max_restarts: u64,
    /// What a new instance means for the Tasks of the scope that completed
    /// against the previous one. `Some` whenever `max_restarts` is greater
    /// than 0; may be `None` only when it is 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_tasks: Option<CompletedTasksPolicy>,
}

impl ServiceRestartPolicy {
    /// The `completedTasks` that applies when the Service is relaunched
    /// after an instance or start failure. `None` only when `max_restarts`
    /// is 0, in which case no relaunch happens and the failure fails the
    /// scope.
    #[must_use]
    pub fn completed_tasks_on_relaunch(&self) -> Option<CompletedTasksPolicy> {
        self.completed_tasks
    }

    /// The `completedTasks` that applies when the Service is stopped and
    /// started again because a Service it lists began a new Service
    /// Session (RFC 0009 "Dependents of a relaunched Service"): the value
    /// given, or `RERUN` when none was — the Service has not said that its
    /// state survives a new instance.
    #[must_use]
    pub fn completed_tasks_on_dependent_restart(&self) -> CompletedTasksPolicy {
        self.completed_tasks.unwrap_or(CompletedTasksPolicy::Rerun)
    }

    /// True when the Service may be suspended while idle (RFC 0009
    /// constraint 9): only a Service whose `completedTasks` is `KEEP`.
    #[must_use]
    pub fn may_be_suspended(&self) -> bool {
        self.completed_tasks == Some(CompletedTasksPolicy::Keep)
    }
}

/// An instantiated `<ServiceScript>` (§9.5). `let_bindings` are the
/// `<ServiceScript>.let` bindings, evaluated on the service host.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceScript {
    /// The `<ServiceScript>` `let` bindings, as written; evaluated on the
    /// service host when the Service is started.
    #[serde(rename = "let", alias = "letBindings")]
    pub let_bindings: Option<Vec<String>>,
    /// The Service's actions.
    pub actions: ServiceActions,
    /// Files materialized to the Service Session's working directory before
    /// each action runs, reachable as `Service.File.<name>`.
    pub embedded_files: Option<Vec<EmbeddedFile>>,
}

/// An instantiated `<ServiceActions>` (§9.7).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceActions {
    /// One-time setup, run once per Service Session before the first `onRun`.
    pub on_enter: Option<Action>,
    /// The long-lived action whose process is the service.
    pub on_run: Action,
    /// The probe of a `COMMAND` health check; present iff the type is `COMMAND`.
    pub on_health_check: Option<Action>,
    /// Cleanup, run once when the Service Session ends.
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
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
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
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
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
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
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
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
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
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub enum TaskParamRange<T: Serialize> {
    List(Vec<T>),
    RangeExpr(RangeExpr),
}

/// Chunks config with all format strings resolved.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub struct ResolvedChunks {
    /// `u64`, not `usize`, so a value above 2^32 is kept on 32-bit targets
    /// (wasm32) instead of being truncated.
    pub default_task_count: u64,
    /// `u64` for the same reason as `default_task_count`.
    pub target_runtime_seconds: Option<u64>,
    pub range_constraint: RangeConstraint,
}

/// Resolved host requirements — no FormatStrings.
///
/// Within one source names are unique. In a Step's combined set
/// (`ENV_PARAMS`, §3.3.3 item 2.2) an attribute name other than
/// `attr.worker.os.family` and `attr.worker.cpu.arch` may appear more than
/// once, when more than one source names it: an `allOf` entry when any
/// source requires a value outright, then any number of `anyOf` entries of
/// two or more values each; a host must satisfy every one.
#[derive(Debug, Clone, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub struct HostRequirements {
    pub amounts: Option<Vec<AmountRequirement>>,
    pub attributes: Option<Vec<AttributeRequirement>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
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
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub struct AttributeRequirement {
    pub name: String,
    pub any_of: Option<Vec<String>>,
    pub all_of: Option<Vec<String>>,
}

/// One entry of a Step's, a Service's, or a Job Environment's
/// `dependencies` (Template Schemas §3.2): `dependsOn: <StepName>` (also
/// written, with the `ENV_PARAMS` extension, as its synonym `step:`) or,
/// with the `SERVICE` extension, `service: <ServiceName>`. Template
/// validation has rejected an entry giving more than one key or none, so
/// exactly one is `Some` in a Job. An `ENV_PARAMS` `environment:` entry is
/// not a scheduling dependency and has no counterpart here: job creation
/// resolves it into the Step's Environments instead.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub struct StepDependency {
    /// The Step this entry names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<String>,
    /// The Service this entry names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
}

impl StepDependency {
    /// An entry naming the Step `name`.
    #[must_use]
    pub fn on_step(name: impl Into<String>) -> Self {
        Self {
            depends_on: Some(name.into()),
            service: None,
        }
    }

    /// An entry naming the Service `name`.
    #[must_use]
    pub fn on_service(name: impl Into<String>) -> Self {
        Self {
            depends_on: None,
            service: Some(name.into()),
        }
    }

    /// What this entry names; `None` for an entry that gives both keys or
    /// neither (which template validation rejects).
    #[must_use]
    pub fn target(&self) -> Option<DependencyTarget<'_>> {
        match (&self.depends_on, &self.service) {
            // A Job holds a `step:` entry in its `dependsOn` form.
            (Some(step), None) => Some(DependencyTarget::Step(step, StepKey::DependsOn)),
            (None, Some(service)) => Some(DependencyTarget::Service(service)),
            _ => None,
        }
    }

    /// The Step this entry names, if it is a `dependsOn` entry.
    #[must_use]
    pub fn step(&self) -> Option<&str> {
        self.target().and_then(DependencyTarget::step)
    }

    /// The Service this entry names, if it is a `service` entry.
    #[must_use]
    pub fn service(&self) -> Option<&str> {
        self.target().and_then(DependencyTarget::service)
    }
}

/// The Job form of a well-formed template entry: a `dependsOn` or `step`
/// entry becomes `dependsOn`, a `service` entry `service`. An
/// `environment` entry (or a malformed one) gives neither key; callers
/// filter those out first.
impl From<&crate::template::StepDependency> for StepDependency {
    fn from(dep: &crate::template::StepDependency) -> Self {
        Self {
            depends_on: dep.step().map(str::to_string),
            service: dep.service().map(str::to_string),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::transport_tables_equal;
    use serde_json::json;

    /// A table `create_job` never makes (not a list, or an entry without
    /// a `value`) compares by its JSON, so it equals only an identical one.
    #[test]
    fn malformed_transport_tables_compare_by_their_json() {
        assert!(transport_tables_equal(&json!("x"), &json!("x")));
        assert!(!transport_tables_equal(&json!("x"), &json!("y")));
        assert!(!transport_tables_equal(&json!("x"), &json!([])));
        let no_value = json!([{"name": "N", "type": "int"}]);
        let with_value = json!([{"name": "N", "type": "int", "value": "1"}]);
        assert!(transport_tables_equal(&no_value, &no_value));
        assert!(!transport_tables_equal(&no_value, &with_value));
        assert!(!transport_tables_equal(&with_value, &no_value));
    }

    /// A float string that does not parse (not one `create_job` makes)
    /// compares by its text.
    #[test]
    fn unparsed_float_strings_compare_by_text() {
        let t = |v: &str| json!([{"name": "F", "type": "float", "value": v}]);
        assert!(transport_tables_equal(&t("x"), &t("x")));
        assert!(!transport_tables_equal(&t("x"), &t("y")));
        assert!(!transport_tables_equal(&t("x"), &t("1.0")));
    }
}
