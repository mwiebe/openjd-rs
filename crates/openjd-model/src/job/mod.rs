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
    /// Environments has requirements keeps its own, unchanged.
    pub host_requirements: Option<HostRequirements>,
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
    /// Environment whose `enableIf` (§4 item 6) is `false` for the Step is
    /// not entered, so it has no entry.
    /// `None` when there is none, so a Job without Environment parameters
    /// serializes exactly as before. Compare two Steps' values with
    /// [`Step::environment_values_equal`], not with `==`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_parameters: Option<Vec<EnvironmentParameterValues>>,
    /// The Job Environments this Step's Sessions enter (`ENV_PARAMS`,
    /// Template Schemas §4 item 4, §3.2.2), in the order of
    /// [`Job::job_environments`]: every one with `activation: ALWAYS`, and every
    /// one with `activation: ON_REQUEST` that an enabled `environment:`
    /// entry of the Step requests, less any whose own `enableIf` (§4 item
    /// 6) is `false` for the Step. Each names an entry of
    /// `Job::job_environments` by `(document, environment)`.
    ///
    /// `None` means every Job Environment, which is every Step of a Job
    /// whose documents use neither `activation: ON_REQUEST` nor an
    /// Environment `enableIf`, so such a Job serializes exactly as before. A Job from
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
    /// `other`'s (Template Schemas §3.2.2, fourth paragraph): one Session
    /// may run Tasks of both Steps only when this holds and
    /// [`environment_values_equal`](Self::environment_values_equal) does,
    /// and both have equal `step_environments`. Compares
    /// [`environments`](Self::environments), whose `None` is the full list.
    #[must_use]
    pub fn same_environments(&self, other: &Step) -> bool {
        self.environments == other.environments
    }

    /// Whether this Step's effective Environment parameter values equal
    /// `other`'s (Template Schemas §3.2.2): for every Environment either
    /// Step has values for, both have values for it, and each parameter has
    /// the same type and the same value as resolved at job creation.
    ///
    /// This is the comparison a scheduler uses to decide whether one Session
    /// may run Tasks of both Steps. It differs from `==` on the
    /// [`environment_parameters`](Self::environment_parameters) field in two
    /// ways: a FLOAT (or an element of a `LIST[FLOAT]`) compares by numeric
    /// value, so `1.0` and `1.00` are equal here although their transport
    /// form keeps each as written, and a RANGE_EXPR compares by the integers
    /// it denotes, so `1-3` and `1,2,3` are equal. Every other value
    /// compares by its transport form, which for PATH is the unmapped
    /// string.
    /// Environments are matched by `(document, environment)`, so the order
    /// of the list does not matter.
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

    /// True for the Job Template.
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
/// which values to path-map. PATH values are unmapped strings in the
/// submission path format. A RANGE_EXPR value is its canonical form, so two
/// values that denote the same integers have one transport form (§3.2.2);
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
/// names in the same (canonical) order, the same types, and equal values,
/// where a value of a type built from `float` compares numerically and a
/// `range_expr` by the integers it denotes.
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
                        let ty = ty.and_then(serde_json::Value::as_str);
                        if ty == Some("range_expr") {
                            range_expr_transport_equal(va, vb)
                        } else {
                            let is_float = ty.is_some_and(|t| t.contains("float"));
                            transport_values_equal(va, vb, is_float)
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

/// Compare two transport values; with `numeric_floats`, string leaves
/// compare as numbers when both parse as `f64`.
fn transport_values_equal(
    a: &serde_json::Value,
    b: &serde_json::Value,
    numeric_floats: bool,
) -> bool {
    use serde_json::Value;
    match (a, b) {
        (Value::Array(xs), Value::Array(ys)) => {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(ys)
                    .all(|(x, y)| transport_values_equal(x, y, numeric_floats))
        }
        (Value::String(x), Value::String(y)) if numeric_floats => {
            match (x.parse::<f64>(), y.parse::<f64>()) {
                (Ok(fx), Ok(fy)) => fx == fy,
                _ => x == y,
            }
        }
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
    pub script: Option<EnvironmentScript>,
    pub variables: Option<HashMap<String, FormatString>>,
    /// Filtered symbol table containing only symbols referenced by this
    /// environment's format strings (variables, actions, embedded files, let bindings).
    #[serde(rename = "resolvedSymTab", skip_serializing_if = "Option::is_none")]
    pub resolved_symtab: Option<SerializedSymbolTable>,
}

/// Manual because `HashMap` has no `Hash`; `variables` hashes as
/// key-sorted entries to match `HashMap`'s order-insensitive equality.
impl Hash for Environment {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.description.hash(state);
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
    pub on_exit: Option<Action>,
}

crate::template::impl_environment_actions_helpers!(EnvironmentActions, Action);

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
    pub default_task_count: usize,
    pub target_runtime_seconds: Option<usize>,
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

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub struct StepDependency {
    pub depends_on: String,
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
