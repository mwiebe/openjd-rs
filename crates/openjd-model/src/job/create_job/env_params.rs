// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Effective Environment parameter values (`ENV_PARAMS`, Template Schemas
//! §3.2.2), shared by job creation and submission.
//!
//! For each Step and each Environment its Sessions would enter (§4 item 4
//! `activation`) that declares `envParameterDefinitions`, every parameter's
//! effective value is the value the Step's enabled `environment:` entry for
//! that Environment supplies, else the parameter's resolved `default`
//! (§3.2.2 items 1–2). [`create_job`] does this for the Job Template's own
//! `jobEnvironments` ([`resolve_step_entries`], which also decides each
//! entry's `enableIf` and each Step Environment's own `enableIf`, §4 item
//! 6); its submission stage,
//! [`apply_environment_templates`], does it for the attached Environment
//! Templates' Environments. Both go through
//! [`DeclaringEnvironment::values_for_step`], then decide the
//! Environment's own `enableIf` (§4 item 6) with the values that are known
//! ([`DeclaringEnvironment::decide`]), then report the value checks and
//! constraint 7 only where it does not disable the Environment
//! ([`DeclaringEnvironment::commit`]; §3.2.2, §7.4), so the two stages
//! convert and check values the same way:
//!
//! - a literal is decoded as the Job Parameter `default` of its type is
//!   (the template-validation decoder, so the messages match);
//! - a format string resolves, in the submission path format, in the
//!   scope its position allows (§3.2, §4.5 item 4). A single `{{ }}`
//!   expression evaluates with the parameter's type as its target type;
//!   any other format string resolves to a string, which converts as
//!   a submitted Job Parameter value of that type would, with no directory
//!   join;
//! - a PATH value, and each element of a LIST[PATH] value, must be a
//!   non-empty path that is absolute in
//!   [`ValidationContext::submission_path_format`], or, with
//!   [`ValidationContext::allow_uri_path_values`], a URI (§4.5; prototype
//!   plan A12, whose caller-policy half still holds, and A16);
//! - the definition's constraints are checked against the value
//!   (constraint 6, §4.5 item 5);
//! - a RANGE_EXPR value is stored as its `range_expr`, whose transport form
//!   is its canonical form, so two values are equal when they denote the
//!   same integers (§3.2.2): `1-3` and `1,2,3` are equal. Its length
//!   constraints check both the value as written and its canonical form
//!   (§4.5 item 5, §2.10).
//!
//! A literal `default` is checked when the Environment is set up, even if
//! no Step takes it (§4.5 point 1). Any other `default` is resolved the
//! first time a Step takes it, and a failure is reported once, naming the
//! first Step that takes it for which the Environment is not disabled
//! (§4.5 point 2, §3.2.2; plan A19): its value cannot depend on the Step,
//! since its scope is the document's Job Parameters.
//!
//! [`create_job`]: super::create_job
//! [`apply_environment_templates`]: super::external::apply_environment_templates
//! [`ValidationContext::submission_path_format`]: crate::types::ValidationContext::submission_path_format
//! [`ValidationContext::allow_uri_path_values`]: crate::types::ValidationContext::allow_uri_path_values

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};

use openjd_expr::path_mapping::PathFormat;
use openjd_expr::{ExprType, ExprValue, SymbolTable};

use crate::error::{path_field, path_index, PathElement, ValidationErrors};
use crate::job;
use crate::template::expr_field::{self, ExprField, ExprFieldEval};
use crate::template::validate_v2023_09::env_params as tv;
use crate::template::{self, EnvironmentParameterDefinition, EnvironmentParameterValue};
use crate::types::{JobParameterType, ValidationContext};

use super::parameters::coerce_from_str;
use super::EvalBudgets;

/// The submission policy every value conversion reads.
#[derive(Clone, Copy)]
pub(crate) struct ResolvePolicy<'a> {
    pub(crate) ctx: &'a ValidationContext,
    pub(crate) budgets: EvalBudgets,
}

impl<'a> ResolvePolicy<'a> {
    pub(crate) fn new(ctx: &'a ValidationContext) -> Self {
        Self {
            ctx,
            budgets: EvalBudgets::from_ctx(ctx),
        }
    }
}

/// The values one `environment:` entry supplies to one Environment.
pub(crate) struct Supplied<'s> {
    /// The entry's `parameters`, by name; empty for an entry without them.
    pub(crate) params: &'s BTreeMap<String, EnvironmentParameterValue>,
    /// The scope its format strings resolve in (§3.2 bullet 2).
    pub(crate) symtab: &'s SymbolTable,
    /// The entry's full path, `... -> steps[i] -> dependencies[j]`.
    pub(crate) entry_path: Vec<PathElement>,
}

/// One Environment that may declare parameters, set up for computing each
/// Step's effective values and deciding its own `enableIf` for each Step.
pub(crate) struct DeclaringEnvironment<'a> {
    /// The document that declares the Environment.
    pub(crate) document: job::Document,
    /// The Environment.
    pub(crate) env: &'a template::Environment,
    /// The Environment's `name`.
    pub(crate) name: &'a str,
    /// The Environment's `activation`, with its default applied.
    pub(crate) activation: template::EnvironmentActivation,
    /// Whether the Environment's own `enableIf` (§4 item 6) is absent or
    /// `true`, so it is known before job creation not to be disabled for
    /// any Step.
    pub(crate) certainly_enabled: bool,
    /// How messages name the Environment: `'Name'`, plus the document for
    /// an attached one.
    pub(crate) label: String,
    defs: &'a [EnvironmentParameterDefinition],
    /// The Environment's own `enableIf`.
    enable_if: Option<&'a ExprField>,
    /// The full path of the Environment.
    env_path: Vec<PathElement>,
    /// The full path of `envParameterDefinitions`.
    defs_path: Vec<PathElement>,
    /// Each definition's resolved `default`, resolved on first use.
    defaults: Vec<OnceCell<DefaultOutcome>>,
    /// The declared names, for lookups by key.
    declared: HashSet<&'a str>,
    /// The constraint 5 message's capped list of declared names, built on
    /// first use.
    declared_list: OnceCell<String>,
    /// The `enableIf` results so far, one per distinct set of known values
    /// (§3.2.2), with the Steps they were resolved for, in Step order.
    decisions: RefCell<EnableIfDecisions>,
}

/// A resolved `default` (§4.5 item 4): the value converted to the
/// parameter's type, if it converts; whether it met every check; and the
/// errors of the checks it failed, reported once, by the first Step for
/// which the Environment is not disabled that takes it (or, for a literal,
/// when the Environment is set up, §4.5 point 1).
struct DefaultOutcome {
    value: Option<Effective>,
    ok: bool,
    errors: Vec<crate::error::ValidationError>,
    reported: Cell<bool>,
}

/// One effective value as [`resolve_value`] returns it, in Job Parameter
/// form.
#[derive(Clone)]
struct Effective {
    value: ExprValue,
}

/// An Environment's `enableIf` results, keyed by the known values they
/// were evaluated with.
#[derive(Default)]
struct EnableIfDecisions {
    by_key: HashMap<String, usize>,
    /// The result, and the names of the Steps it was resolved for.
    results: Vec<(Result<EnableIf, String>, Vec<String>)>,
}

/// What an Environment's own `enableIf` (§4 item 6) decides for one Step
/// (§3.2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnableIf {
    /// Absent, or `true`: the Environment is entered.
    Enabled,
    /// `false`: the Environment is disabled for the Step.
    Disabled,
    /// The result depends on a value that is missing or does not convert
    /// (`unresolved[bool]`): the Environment is not disabled.
    Unresolved,
    /// The evaluation failed or did not fit `bool` (reported once, by
    /// [`DeclaringEnvironment::report_enable_if_errors`]). Job creation
    /// fails either way; which other errors to report is an implementation
    /// choice. The Environment is checked as if it is not disabled, so the
    /// Step's other mistakes for it (a value that breaks a constraint, a
    /// missing parameter) are reported in the same run.
    Failed,
}

/// One value check of [`PendingValues`] whose report waits for the
/// Environment's `enableIf`.
enum Deferred {
    /// An error of a supplied value.
    Error(crate::error::ValidationError),
    /// The failed checks of definition `k`'s resolved `default`.
    Default(usize),
}

/// One Step's effective values for one Environment (§3.2.2), computed
/// before the Environment's own `enableIf` decides whether they are
/// checked.
pub(crate) struct PendingValues<'a> {
    /// The values, when every parameter has one and every check passed.
    values: Option<job::EnvironmentParameterValues>,
    /// For each definition, in order: its name, type, and the value known
    /// to the `enableIf`, in transport form: the effective value when it
    /// converts to the type, whether or not it meets the other
    /// constraints, and `None` (unknown) when it is missing or does not
    /// convert.
    known: Vec<(&'a str, JobParameterType, Option<ExprValue>)>,
    /// The value checks' errors, reported only when the Environment is not
    /// disabled for the Step.
    deferred: Vec<Deferred>,
    /// The parameters with neither a supplied value nor a `default`
    /// (constraint 7), reported only when the Environment is not disabled.
    missing: Vec<&'a str>,
}

impl<'a> DeclaringEnvironment<'a> {
    /// Set up `env`, whose path is `env_path`. A duplicated parameter name
    /// is a template-validation error, so names are taken as unique.
    pub(crate) fn new(
        document: job::Document,
        env: &'a template::Environment,
        env_path: &[PathElement],
    ) -> Self {
        let defs = env.env_parameter_definitions.as_deref().unwrap_or(&[]);
        let label = if document.is_job_template() {
            format!("'{}'", env.name)
        } else {
            format!("'{}' ({document})", env.name)
        };
        Self {
            document,
            env,
            name: &env.name,
            activation: env.activation(),
            certainly_enabled: env.literal_enabled() == Some(true),
            label,
            defs,
            enable_if: env.enable_if.as_ref(),
            env_path: env_path.to_vec(),
            defs_path: path_field(env_path, "envParameterDefinitions"),
            defaults: (0..defs.len()).map(|_| OnceCell::new()).collect(),
            declared: defs
                .iter()
                .map(EnvironmentParameterDefinition::name)
                .collect(),
            declared_list: OnceCell::new(),
            decisions: RefCell::new(EnableIfDecisions::default()),
        }
    }

    /// True when the Environment declares parameters, so every Step gets
    /// effective values for it.
    pub(crate) fn declares_parameters(&self) -> bool {
        !self.defs.is_empty()
    }

    /// §4.5 point 1: check every literal `default` now, so a literal that
    /// fails a check is an error even when every Step supplies the
    /// parameter, or the Environment is disabled for every Step that takes
    /// it: the check is of the definition, not of a Step's effective value.
    pub(crate) fn check_literal_defaults(
        &self,
        default_symtab: &SymbolTable,
        policy: ResolvePolicy<'_>,
        errors: &mut ValidationErrors,
    ) {
        for (k, def) in self.defs.iter().enumerate() {
            if matches!(def.default, Some(EnvironmentParameterValue::Literal(_))) {
                let outcome = self.default_outcome(k, default_symtab, policy);
                if !outcome.reported.replace(true) {
                    errors.errors.extend(outcome.errors.iter().cloned());
                }
            }
        }
    }

    /// The resolved `default` of definition `k`, resolving it on first
    /// use. Its errors are not reported here.
    fn default_outcome(
        &self,
        k: usize,
        default_symtab: &SymbolTable,
        policy: ResolvePolicy<'_>,
    ) -> &DefaultOutcome {
        self.defaults[k].get_or_init(|| {
            let def = &self.defs[k];
            let Some(default) = def.default.as_ref() else {
                return DefaultOutcome {
                    value: None,
                    ok: false,
                    errors: Vec::new(),
                    reported: Cell::new(true),
                };
            };
            let path = path_field(&path_index(&self.defs_path, k), "default");
            let mut scratch = ValidationErrors::default();
            let (value, ok) =
                resolve_value(def, default, default_symtab, policy, &path, &mut scratch);
            DefaultOutcome {
                value,
                ok,
                errors: scratch.errors,
                reported: Cell::new(false),
            }
        })
    }

    /// The effective values of this Environment for a Step (§3.2.2), which
    /// supplies `supplied` when it has an `environment:` entry naming this
    /// Environment, before the Environment's `enableIf` decides whether
    /// they are checked.
    ///
    /// Reports into `errors` at once the checks that apply whatever the
    /// `enableIf` decides: every key the Environment does not declare
    /// (constraint 5). The value checks (constraint 6, §4.5 PATH paragraph) and
    /// constraint 7 are kept in the result, for
    /// [`commit`](Self::commit) to report when the Environment is not
    /// disabled for the Step.
    pub(crate) fn values_for_step(
        &self,
        supplied: Option<&Supplied<'_>>,
        default_symtab: &SymbolTable,
        policy: ResolvePolicy<'_>,
        errors: &mut ValidationErrors,
    ) -> PendingValues<'a> {
        let mut pending = PendingValues {
            values: None,
            known: Vec::with_capacity(self.defs.len()),
            deferred: Vec::new(),
            missing: Vec::new(),
        };
        let mut table = SymbolTable::new();
        let mut complete = true;

        if let Some(s) = supplied {
            self.check_supplied_keys(s, errors);
        }
        for (k, def) in self.defs.iter().enumerate() {
            let pt = def.job_param_type();
            let given = supplied.and_then(|s| s.params.get(def.name()).map(|v| (s, v)));
            let (value, ok) = match given {
                Some((s, value)) => {
                    let path = path_field(&path_field(&s.entry_path, "parameters"), def.name());
                    let mut scratch = ValidationErrors::default();
                    let r = resolve_value(def, value, s.symtab, policy, &path, &mut scratch);
                    pending
                        .deferred
                        .extend(scratch.errors.into_iter().map(Deferred::Error));
                    r
                }
                None if def.default.is_some() => {
                    let outcome = self.default_outcome(k, default_symtab, policy);
                    if !outcome.ok {
                        pending.deferred.push(Deferred::Default(k));
                    }
                    (outcome.value.clone(), outcome.ok)
                }
                None => {
                    pending.missing.push(def.name());
                    (None, false)
                }
            };
            let value = value.map(|v| to_transport(pt, v.value));
            match (&value, ok) {
                // A definition's name is an `<Identifier>`, so `set`
                // cannot fail on it.
                (Some(v), true) => {
                    if table.set(def.name(), v.clone()).is_err() {
                        complete = false;
                    }
                }
                _ => complete = false,
            }
            pending.known.push((def.name(), pt, value));
        }
        pending.values =
            (complete && self.declares_parameters()).then(|| job::EnvironmentParameterValues {
                document: self.document.clone(),
                environment: self.name.to_string(),
                values: openjd_expr::SerializedSymbolTable::from_symtab(&table),
            });
        pending
    }

    /// Report the value checks and constraint 7 of `pending`, for a Step
    /// named `step_name` for which the Environment is not disabled, and
    /// return its values (`None` when a parameter has no value or a check
    /// failed) and the names of the parameters it lacks (constraint 7), for
    /// the caller's one error per Step. A failed resolved `default` is
    /// reported once, naming the first Step that reports it.
    pub(crate) fn commit(
        &self,
        pending: PendingValues<'a>,
        step_name: &str,
        errors: &mut ValidationErrors,
    ) -> (Option<job::EnvironmentParameterValues>, Vec<&'a str>) {
        for d in pending.deferred {
            match d {
                Deferred::Error(e) => errors.errors.push(e),
                Deferred::Default(k) => {
                    let Some(outcome) = self.defaults[k].get() else {
                        continue;
                    };
                    if outcome.reported.replace(true) {
                        continue;
                    }
                    for e in &outcome.errors {
                        let mut e = e.clone();
                        e.message = format!(
                            "for Step '{step_name}', which does not supply this parameter: {}",
                            e.message
                        );
                        errors.errors.push(e);
                    }
                }
            }
        }
        (pending.values, pending.missing)
    }

    /// Decide this Environment's own `enableIf` (§4 item 6) for the Step
    /// named `step_name`, whose effective values for it are `pending`
    /// (§3.2.2): absent is [`EnableIf::Enabled`], a boolean scalar is used
    /// as written, and an expression is evaluated in its scope (§4 item 6
    /// constraint 1): `base`, which holds the document's Job Parameters,
    /// `Job.Name`, and `Job.EnvNames`, plus this Environment's
    /// `Env.RawParam.*` of any type (a path as its unmapped string) and
    /// `Env.Param.*` of types other than PATH and LIST[PATH], each bound to
    /// its known value or, when the value is missing, fails to resolve, or
    /// does not convert, to `unresolved[T]` of its type. The
    /// result is `true`, `false`, or unresolved (Expression Language,
    /// progressive evaluation).
    ///
    /// Steps with the same known values share one evaluation, whose error,
    /// if any, [`report_enable_if_errors`](Self::report_enable_if_errors)
    /// reports once, naming them all.
    pub(crate) fn decide(
        &self,
        pending: &PendingValues<'_>,
        step_name: &str,
        base: &SymbolTable,
        policy: ResolvePolicy<'_>,
    ) -> EnableIf {
        let Some(field) = self.enable_if else {
            return EnableIf::Enabled;
        };
        if let Some(b) = field.as_bool() {
            return if b {
                EnableIf::Enabled
            } else {
                EnableIf::Disabled
            };
        }
        let is_path = |pt: &JobParameterType| {
            matches!(pt, JobParameterType::Path | JobParameterType::ListPath)
        };
        let mut key = String::new();
        for (name, _, value) in &pending.known {
            match value {
                Some(v) => key.push_str(&format!("{name}={v:?};")),
                None => key.push_str(&format!("{name}?;")),
            }
        }
        let mut decisions = self.decisions.borrow_mut();
        if let Some(&i) = decisions.by_key.get(&key) {
            let (result, steps) = &mut decisions.results[i];
            steps.push(step_name.to_string());
            return result.as_ref().map_or(EnableIf::Failed, |r| *r);
        }
        let mut symtab = base.clone();
        let mut bound = Ok(());
        for (name, pt, value) in &pending.known {
            // A path type's raw value is its unmapped string; its
            // `Env.Param.*` is path-mapped, which needs a host, so it is not
            // in scope.
            if is_path(pt) {
                let raw = value
                    .clone()
                    .map(paths_to_strings)
                    .unwrap_or_else(|| ExprValue::unresolved(pt.raw_expr_type()));
                bound = bound.and_then(|()| symtab.set(&format!("Env.RawParam.{name}"), raw));
                continue;
            }
            let value = value
                .clone()
                .unwrap_or_else(|| ExprValue::unresolved(pt.expr_type()));
            bound = bound
                .and_then(|()| symtab.set(&format!("Env.Param.{name}"), value.clone()))
                .and_then(|()| symtab.set(&format!("Env.RawParam.{name}"), value));
        }
        let result = bound.map_err(|e| e.to_string()).and_then(|()| {
            // The profile and library pass 8 checked the field with: its
            // document's, in template scope, with POSIX paths.
            let profile = policy
                .ctx
                .profile
                .to_expr_profile(openjd_expr::HostContext::None);
            let lib = openjd_expr::FunctionLibrary::for_profile(&profile);
            let ev = ExprFieldEval {
                profile: &profile,
                library: &lib,
                memory_limit: policy.budgets.memory,
                operation_limit: policy.budgets.operations,
            };
            expr_field::resolve_progressive(field, &ExprType::BOOL, &symtab, &ev).map(|v| match v {
                Some(ExprValue::Bool(true)) => EnableIf::Enabled,
                Some(ExprValue::Bool(false)) => EnableIf::Disabled,
                // `resolve_progressive` returns only values that fit
                // `bool`, or `None` for an unresolved result.
                _ => EnableIf::Unresolved,
            })
        });
        let decision = result.as_ref().map_or(EnableIf::Failed, |r| *r);
        let index = decisions.results.len();
        decisions.by_key.insert(key, index);
        decisions
            .results
            .push((result, vec![step_name.to_string()]));
        decision
    }

    /// Report each failed `enableIf` evaluation of [`decide`](Self::decide)
    /// at `<environment> -> enableIf`, once per set of known values, naming
    /// the Steps it was resolved for.
    pub(crate) fn report_enable_if_errors(&self, errors: &mut ValidationErrors) {
        for (result, steps) in &self.decisions.borrow().results {
            let Err(msg) = result else {
                continue;
            };
            let names: Vec<String> = steps.iter().map(|s| format!("'{s}'")).collect();
            let noun = if steps.len() == 1 { "Step" } else { "Steps" };
            // The note goes after the first line, as for host
            // requirements, so an expression error's source and caret
            // lines stay last.
            let note = format!(
                "Resolved for {noun} {}.",
                super::host_requirements::name_list(
                    &names.iter().map(String::as_str).collect::<Vec<_>>()
                )
            );
            errors.add(
                &path_field(&self.env_path, "enableIf"),
                super::host_requirements::with_note(msg, &note),
            );
        }
    }

    /// The checks of [`values_for_step`](Self::values_for_step) that need
    /// no Job Parameter values, for
    /// [`check_environment_templates`](super::check_environment_templates):
    /// constraint 5 (keys); and, when `values_checked` (the Environment is certainly
    /// not disabled, so its own `enableIf` is absent or `true`), for each
    /// supplied value a literal converted and checked as `values_for_step`
    /// does (type, constraints, PATH absoluteness) and a format string
    /// checked only against the single-expression rule for a list-typed
    /// parameter, and constraint 7. `default`s are not resolved here (a
    /// literal one is checked by
    /// [`check_literal_defaults`](Self::check_literal_defaults)). Returns
    /// the names of parameters with neither a supplied value nor a
    /// `default` (constraint 7) for the caller to report; none when
    /// `!values_checked`.
    pub(crate) fn check_step_statically(
        &self,
        supplied: Option<&Supplied<'_>>,
        values_checked: bool,
        policy: ResolvePolicy<'_>,
        errors: &mut ValidationErrors,
    ) -> Vec<&'a str> {
        if let Some(s) = supplied {
            self.check_supplied_keys(s, errors);
        }
        let mut missing = Vec::new();
        for def in self.defs {
            match supplied.and_then(|s| s.params.get(def.name()).map(|v| (s, v))) {
                Some((s, value)) => {
                    let path = path_field(&path_field(&s.entry_path, "parameters"), def.name());
                    if !values_checked {
                        continue;
                    }
                    match value {
                        EnvironmentParameterValue::Literal(_) => {
                            let _ = resolve_value(def, value, s.symtab, policy, &path, errors);
                        }
                        EnvironmentParameterValue::FormatString(fs) => {
                            tv::check_list_format_string(def.job_param_type(), fs, &path, errors);
                        }
                    }
                }
                None if def.default.is_some() || !values_checked => {}
                None => missing.push(def.name()),
            }
        }
        missing
    }

    /// Constraint 5: every supplied key names a declared parameter. An
    /// Environment that declares none rejects any key, with one error. As
    /// at template validation, only an entry's first undeclared key lists
    /// the declared names, and every list is capped
    /// ([`tv::capped_name_list`]), so messages stay short however long the
    /// names are.
    fn check_supplied_keys(&self, s: &Supplied<'_>, errors: &mut ValidationErrors) {
        let params_path = path_field(&s.entry_path, "parameters");
        if self.defs.is_empty() {
            if !s.params.is_empty() {
                errors.add(
                    &params_path,
                    format!(
                        "Environment {} declares no parameters, so this entry must not supply \
                         any; it supplies {}.",
                        self.label,
                        tv::capped_name_list(s.params.keys().map(String::as_str), s.params.len())
                    ),
                );
            }
            return;
        }
        let mut listed = false;
        for key in s.params.keys() {
            if self.declared.contains(key.as_str()) {
                continue;
            }
            let msg = if listed {
                format!("Environment {} declares no parameter '{key}'.", self.label)
            } else {
                listed = true;
                let list = self.declared_list.get_or_init(|| {
                    tv::capped_name_list(self.defs.iter().map(|d| d.name()), self.defs.len())
                });
                format!(
                    "Environment {} declares no parameter '{key}'. Declared: {list}.",
                    self.label
                )
            };
            errors.add(&path_field(&params_path, key), msg);
        }
    }
}

/// Report, at `step_path`, the parameters without a `default` that a Step
/// does not supply (constraint 7): one error per Step, with the list
/// capped as template validation caps it
/// ([`tv::missing_parameters_message`]). `missing` pairs each
/// Environment's label with its missing names; `why` says which
/// Environments the Step's Sessions enter, and cites the rule. `disabled`
/// and `unresolved` are the labels whose groups get a note (see
/// [`tv::missing_parameters_message`]).
pub(crate) fn report_missing(
    step_path: &[PathElement],
    missing: &[(String, Vec<&str>)],
    why: &str,
    disabled: &HashSet<&str>,
    unresolved: &HashSet<&str>,
    failed: &HashSet<&str>,
    errors: &mut ValidationErrors,
) {
    let total: usize = missing.iter().map(|(_, names)| names.len()).sum();
    if total == 0 {
        return;
    }
    let pairs = missing
        .iter()
        .flat_map(|(label, names)| names.iter().map(move |n| (label.as_str(), *n)));
    errors.add(
        step_path,
        tv::missing_parameters_message(pairs, total, why, disabled, unresolved, failed),
    );
}

/// Convert one written value (a `default` or an entry value) to the
/// parameter's type and check it, reporting at `path`. Returns the
/// value in Job Parameter form (a string for PATH, a list of strings for
/// LIST[PATH]) when it converts to the parameter's type, `None` when it
/// does not, and whether every check passed: a value that converts but
/// breaks a constraint is returned with `false`, since it is known to an
/// Environment's `enableIf` (§3.2.2). (A PATH value that is not absolute is
/// returned the same way, though PATH values are outside the `enableIf`'s
/// scope.)
fn resolve_value(
    def: &EnvironmentParameterDefinition,
    value: &EnvironmentParameterValue,
    symtab: &SymbolTable,
    policy: ResolvePolicy<'_>,
    path: &[PathElement],
    errors: &mut ValidationErrors,
) -> (Option<Effective>, bool) {
    let pt = def.job_param_type();
    let is_format_string = value.as_format_string().is_some();
    let is_range_expr = pt == JobParameterType::RangeExpr;
    // A RANGE_EXPR format string's value as written, which its length
    // constraints check besides its canonical form (§4.5 item 5): the
    // resolved string, or a single `{{ }}` expression's string result.
    // `None` for a single expression whose result is a `range_expr`, which
    // has only its canonical form, and for every other type. A literal is
    // checked as written by `convert_literal_value`.
    let mut text = None;
    let value = match value {
        // The template-validation check: the Job Parameter `default`
        // decoder of the type, list elements that are not format strings,
        // and the definition's constraints. It returns the converted value.
        EnvironmentParameterValue::Literal(lit) => {
            match tv::convert_literal_value(def, lit, path, errors) {
                (Some(v), true) => v,
                (v, false) => return (v.map(|value| Effective { value }), false),
                (None, true) => return (None, false),
            }
        }
        EnvironmentParameterValue::FormatString(fs) => {
            let before = errors.len();
            tv::check_list_format_string(pt, fs, path, errors);
            if errors.len() > before {
                return (None, false);
            }
            // Paths are in the submission path format (§4.5), so path
            // operations (`path(RawParam.Dir) / 'sub'`) use its rules.
            let opts = policy
                .budgets
                .fs_options()
                .with_path_format(policy.ctx.submission_path_format);
            let resolved = if fs.segment_count() == 1 {
                let target = pt.expr_type();
                fs.resolve_with(symtab, &opts.clone().with_target_type(&target))
                    .map_err(|e| e.to_string())
                    .and_then(|v| {
                        super::parameters::coerce_to_job_parameter_type(&paths_to_strings(v), pt)
                    })
                    .and_then(|v| match v {
                        // The canonical form of a computed range_expr must be
                        // a valid <IntRangeExpr>; an empty range_expr (a
                        // slice of nothing, for example) is not.
                        ExprValue::RangeExpr(r) => {
                            coerce_from_str(&r.to_string(), pt)?;
                            Ok(ExprValue::RangeExpr(r))
                        }
                        v => Ok(v),
                    })
                    .inspect(|_| {
                        // The target type converts a string result to a
                        // `range_expr`, so the string the expression wrote is
                        // read by evaluating it again with no target type.
                        // Only a string result has a spelling to check
                        // (§4.5 item 5); a `range_expr` result has only its
                        // canonical form.
                        if is_range_expr {
                            if let Ok(ExprValue::String(s)) = fs.resolve_with(symtab, &opts) {
                                text = Some(s);
                            }
                        }
                    })
            } else {
                fs.resolve_string_with(symtab, &opts)
                    .map_err(|e| e.to_string())
                    .and_then(|s| {
                        if is_range_expr {
                            text = Some(s.clone());
                        }
                        coerce_from_str(&s, pt)
                    })
            };
            match resolved {
                Ok(v) => v,
                Err(e) => {
                    errors.add(
                        path,
                        format!(
                            "could not be resolved as a value of type {}: {e}",
                            pt.as_spec_str()
                        ),
                    );
                    return (None, false);
                }
            }
        }
    };
    if !check_paths(pt, &value, policy.ctx, path, errors) {
        return (Some(Effective { value }), false);
    }
    // The constraints see the value that is stored (a literal's were
    // checked above by the template-validation check, as written), so every
    // effective value satisfies its definition (§4.5 item 5). A RANGE_EXPR
    // with a written string is checked as that string, which checks both it
    // and its canonical form.
    if is_format_string {
        let checked = match &text {
            Some(s) => ExprValue::String(s.clone()),
            None => value.clone(),
        };
        if let Err(e) = def.definition.check_constraints(&checked) {
            errors.add(path, e);
            return (Some(Effective { value }), false);
        }
    }
    (Some(Effective { value }), true)
}

/// A `path` result converted to its Job Parameter form (a string), and a
/// `list[path]` to a list of strings; any other value unchanged.
pub(super) fn paths_to_strings(v: ExprValue) -> ExprValue {
    match v {
        ExprValue::Path { value, .. } => ExprValue::String(value),
        ExprValue::ListPath(items, _, _) => {
            ExprValue::make_list(
                items.into_iter().map(ExprValue::String).collect(),
                ExprType::STRING,
            )
            // A list of strings is always constructible.
            .unwrap_or(ExprValue::Null)
        }
        other => other,
    }
}

/// §4.5 PATH paragraph: each PATH value, and each LIST[PATH] element, is
/// a non-empty path absolute in the submission path format or, with the
/// caller's permission, a URI.
/// Reports every bad element. Returns `false` when it reported an error.
fn check_paths(
    pt: JobParameterType,
    value: &ExprValue,
    ctx: &ValidationContext,
    path: &[PathElement],
    errors: &mut ValidationErrors,
) -> bool {
    let before = errors.len();
    let mut check = |s: &str, item: Option<usize>| {
        let at = item.map(|i| format!("item[{i}] ")).unwrap_or_default();
        let problem = if s.is_empty() {
            Some("must not be an empty path".to_string())
        } else if openjd_expr::uri_path::is_uri(s) {
            (!ctx.allow_uri_path_values).then(|| {
                format!(
                    "is a URI ('{s}'), and URI path values are not permitted for this submission"
                )
            })
        } else if !is_absolute_in(s, ctx.submission_path_format) {
            Some(format!(
                "must be an absolute path in the submission path format ({}), since an \
                 Environment parameter has no directory to be relative to; got '{s}'",
                format_name(ctx.submission_path_format)
            ))
        } else {
            None
        };
        if let Some(problem) = problem {
            errors.add(path, format!("{at}{problem}."));
        }
    };
    match (pt, value) {
        (JobParameterType::Path, ExprValue::String(s)) => check(s, None),
        (JobParameterType::ListPath, ExprValue::ListString(items, _)) => {
            for (i, s) in items.iter().enumerate() {
                check(s, Some(i));
            }
        }
        _ => {}
    }
    errors.len() == before
}

/// Whether `s` is an absolute path in `format`. In POSIX only a leading
/// `/` makes a path absolute: a backslash is an ordinary filename
/// character, so `\\srv\share` is relative there, although
/// `openjd_expr::functions::path::is_absolute` treats a leading `\\` as a
/// UNC path in every format.
fn is_absolute_in(s: &str, format: PathFormat) -> bool {
    match format {
        PathFormat::Posix => s.starts_with('/'),
        _ => openjd_expr::functions::path::is_absolute(s, format),
    }
}

pub(crate) fn format_name(f: PathFormat) -> &'static str {
    match f {
        PathFormat::Posix => "POSIX",
        PathFormat::Windows => "WINDOWS",
        PathFormat::Uri => "URI",
    }
}

/// The transport form of a checked value: PATH as a `path`, LIST[PATH] as
/// a `list[path]`, so the stored type is the parameter's own. The strings
/// are kept (POSIX construction does not rewrite separators). A RANGE_EXPR
/// keeps its `range_expr`, which a symbol table serializes in canonical
/// form.
fn to_transport(pt: JobParameterType, v: ExprValue) -> ExprValue {
    match (pt, v) {
        (JobParameterType::Path, ExprValue::String(s)) => ExprValue::new_path(s, PathFormat::Posix),
        (JobParameterType::ListPath, ExprValue::ListString(items, _)) => ExprValue::make_list(
            items
                .into_iter()
                .map(|s| ExprValue::new_path(s, PathFormat::Posix))
                .collect(),
            ExprType::PATH,
        )
        // A list of paths is always constructible.
        .unwrap_or(ExprValue::Null),
        (_, v) => v,
    }
}

/// The Job Template's `jobEnvironments`, set up once per `create_job`.
pub(crate) struct JobEnvironments<'a> {
    /// Every Job Environment's name.
    pub(crate) names: HashSet<&'a str>,
    /// Every `jobEnvironments` entry's name and `activation`, in written
    /// order, index for index with the Job's `job_environments`.
    pub(crate) all: Vec<(&'a str, template::EnvironmentActivation)>,
    /// Every Job Environment, in written order. One that declares no
    /// parameters gets no values, but an entry that supplies it any is
    /// still rejected (constraint 5).
    pub(crate) environments: Vec<DeclaringEnvironment<'a>>,
    /// For each of `environments`, its index in `all`.
    pub(crate) positions: Vec<usize>,
    /// The table `default`s resolve in: the merged Job Parameters, without
    /// `Job.Name` (§4.5 item 4; plan A21).
    pub(crate) default_symtab: SymbolTable,
    /// The table each Job Environment's own `enableIf` (§4 item 6
    /// constraint 1) resolves in, before its `Env.*` symbols are added:
    /// the Job Parameters, `Job.Name`, and `Job.EnvNames`.
    pub(crate) enable_if_base: SymbolTable,
}

impl<'a> JobEnvironments<'a> {
    pub(crate) fn new(
        jt: &'a template::JobTemplate,
        default_symtab: SymbolTable,
        enable_if_base: SymbolTable,
        policy: ResolvePolicy<'_>,
        errors: &mut ValidationErrors,
    ) -> Self {
        let mut names = HashSet::new();
        let mut environments = Vec::new();
        let mut positions = Vec::new();
        let all = jt
            .job_environments
            .iter()
            .flatten()
            .map(|e| (e.name.as_str(), e.activation()))
            .collect();
        for (k, env) in jt.job_environments.iter().flatten().enumerate() {
            // A duplicated name (a template-validation error) keeps the
            // first, as template validation resolves it.
            if !names.insert(env.name.as_str()) {
                continue;
            }
            let env_path = path_index(&path_field(&[], "jobEnvironments"), k);
            positions.push(k);
            environments.push(DeclaringEnvironment::new(
                job::Document::JobTemplate,
                env,
                &env_path,
            ));
        }
        for de in &environments {
            de.check_literal_defaults(&default_symtab, policy, errors);
        }
        Self {
            names,
            all,
            environments,
            positions,
            default_symtab,
            enable_if_base,
        }
    }

    /// Report each Job Environment's failed `enableIf` evaluations, once
    /// every Step has been decided, naming the Steps each was resolved for.
    pub(crate) fn report_enable_if_errors(&self, errors: &mut ValidationErrors) {
        for de in &self.environments {
            de.report_enable_if_errors(errors);
        }
    }
}

/// An enabled `environment:` entry of a Step that names no
/// `jobEnvironments` entry of the Job Template (`ENV_PARAMS`), kept by job
/// creation for the submission stage, which resolves it against the
/// attached Environment Templates (Template Schemas §3.2.1).
///
/// The entry's values cannot be converted to their types until the
/// external Environment's definitions are known, so they are kept as
/// written, with `symtab`: the Step-scope symbols (non-PATH `Param.*`,
/// `RawParam.*`, `Job.Name`, `Job.EnvNames`, `Step.Name`, and the Step's
/// `let` bindings, `path` values in the submission path
/// format) that the values' format strings reference.
pub(crate) struct FrozenEntry {
    /// The Environment name the entry gives.
    pub(crate) environment: String,
    /// The entry's 0-based position in the Step's `dependencies` list, for
    /// error paths.
    pub(crate) dependency_index: usize,
    /// `required`, with its default (`true`) applied.
    pub(crate) required: bool,
    /// The entry's `parameters`, as written; empty without them.
    pub(crate) parameters: BTreeMap<String, EnvironmentParameterValue>,
    /// The Step-scope symbols the format-string values reference.
    pub(crate) symtab: SymbolTable,
}

/// What job creation decides about one Step's Environments, for the
/// submission stage.
pub(crate) struct StepSelection {
    /// The Step's enabled entries that name no Job Environment.
    pub(crate) frozen: Vec<FrozenEntry>,
    /// For each `jobEnvironments` entry, index for index, whether the
    /// Step's Sessions enter it: its `activation` is `ALWAYS`, or it is
    /// `ON_REQUEST` and an enabled entry of the Step requests it, and its
    /// own `enableIf` (§4 item 6) did not disable it for the Step. (One
    /// whose `enableIf` failed is left in: the failure fails job creation,
    /// so the list is not read.)
    pub(crate) job_environments_entered: Vec<bool>,
    /// The `jobEnvironments` entries, by index, that the Step's Sessions
    /// would otherwise enter and that their own `enableIf` disabled for
    /// the Step, in written order.
    pub(crate) disabled_job_environments: Vec<usize>,
    /// The names of the Step's entries that name no Job Environment and
    /// whose `enableIf` disabled them, for the constraint 7 message.
    pub(crate) disabled: Vec<String>,
    /// The names of the Step's `stepEnvironments` entries whose own
    /// `enableIf` (§4 item 6) is `false`, in written order.
    pub(crate) disabled_step_environments: Vec<String>,
}

/// What a Step's `environment:` entries resolve to at job creation.
pub(crate) struct StepEntries {
    pub(crate) environment_parameters: Option<Vec<job::EnvironmentParameterValues>>,
    pub(crate) selection: StepSelection,
    /// For each `stepEnvironments` entry, index for index, whether its
    /// `enableIf` (§4 item 6) lets the Step's Sessions enter it: `true`
    /// when it is absent, resolved to `true`, or failed (reported).
    pub(crate) step_environments_enabled: Vec<bool>,
}

/// Whether `dep` is enabled (§3.2 item 5): `true` when its `enableIf` is
/// absent, and otherwise the value of the expression field (§7.5) of type
/// `bool`, resolved in `symtab`. `None` after reporting, at `<entry> ->
/// enableIf`, a value that fails to evaluate or does not resolve to `true`
/// or `false` (`null` included).
fn entry_enabled(
    dep: &template::StepDependency,
    symtab: &SymbolTable,
    policy: ResolvePolicy<'_>,
    entry_path: &[PathElement],
    errors: &mut ValidationErrors,
) -> Option<bool> {
    resolve_enable_if(dep.enable_if.as_ref(), symtab, policy, entry_path, errors)
}

/// The value of an `enableIf` (an expression field of type `bool`, §7.5)
/// at `owner_path -> enableIf`, resolved in `symtab` with the profile and
/// library of the Step's template scope: `true` when it is absent. `None`
/// after reporting a value that fails to evaluate or does not resolve to
/// `true` or `false` (`null` included).
fn resolve_enable_if(
    field: Option<&ExprField>,
    symtab: &SymbolTable,
    policy: ResolvePolicy<'_>,
    owner_path: &[PathElement],
    errors: &mut ValidationErrors,
) -> Option<bool> {
    let Some(field) = field else {
        return Some(true);
    };
    // The profile and library pass 8 checked the field with: the Step's
    // template scope, with POSIX paths (not host context).
    let profile = policy
        .ctx
        .profile
        .to_expr_profile(openjd_expr::HostContext::None);
    let lib = openjd_expr::FunctionLibrary::for_profile(&profile);
    let ev = ExprFieldEval {
        profile: &profile,
        library: &lib,
        memory_limit: policy.budgets.memory,
        operation_limit: policy.budgets.operations,
    };
    match expr_field::resolve(field, &ExprType::BOOL, symtab, &ev) {
        Ok(ExprValue::Bool(b)) => Some(b),
        // `resolve` returns only values that fit `bool`.
        Ok(other) => unreachable!("an expression field of type bool resolved to {other:?}"),
        Err(msg) => {
            errors.add(&path_field(owner_path, "enableIf"), msg);
            None
        }
    }
}

/// Decide a Step's Environments at job creation (§7.4): evaluate each
/// `environment:` entry's `enableIf`, compute the Step's effective values
/// for every Job Environment its Sessions would enter that declares
/// parameters, and keep each enabled entry that names no Job Environment
/// for the submission stage. Each Step Environment's own `enableIf` (§4
/// item 6) is evaluated here too, in the same scope
/// ([`StepEntries::step_environments_enabled`]). Each Job Environment's
/// own `enableIf` is decided with the Step's effective values for it, with
/// the values that are known (§3.2.2, [`DeclaringEnvironment::decide`]),
/// and the value checks and constraint 7 are reported only for the Job
/// Environments it does not disable.
///
/// A disabled entry is as if absent (§3.2 item 2): it requests nothing and
/// supplies nothing, so an `ON_REQUEST` Environment it names is not
/// entered, an `ALWAYS` one takes every `default`, and an external name is not
/// resolved. Constraint 3 applies to entries as written.
///
/// `step_symtab` is the Step's full template scope (`Param.*` other than
/// PATH, `RawParam.*`, `Job.Name`, `Job.EnvNames`, `Step.Name`, and the
/// Step's `let` bindings), the scope of an entry's
/// format strings and `enableIf` (§3.2 items 4–5). An `enableIf` (an
/// entry's or a Step Environment's) resolves in it as it is, with POSIX
/// paths: it is a TEMPLATE-scope expression field, not host context.
/// `let_base` is that scope before the `let` bindings, from which
/// [`entry_scope`] evaluates the bindings the entries' parameter values
/// read in the submission path format; `None` when there is nothing to
/// evaluate again (a POSIX submission, or a Step without `let` bindings).
pub(crate) fn resolve_step_entries(
    st: &template::StepTemplate,
    step_index: usize,
    step_symtab: &SymbolTable,
    let_base: Option<&SymbolTable>,
    job_envs: &JobEnvironments<'_>,
    policy: ResolvePolicy<'_>,
    errors: &mut ValidationErrors,
) -> StepEntries {
    let step_path = path_index(&path_field(&[], "steps"), step_index);
    let deps_path = path_field(&step_path, "dependencies");
    let empty = BTreeMap::new();
    let entered_by_scope = || {
        job_envs
            .all
            .iter()
            .map(|(_, scope)| *scope == template::EnvironmentActivation::Always)
            .collect()
    };
    // `enableIf` is a TEMPLATE-scope expression field, not host context:
    // it resolves in the Step's template scope as job creation built it,
    // with POSIX paths, whatever the submission path format. Only the
    // entries' parameter values use the scope `entry_scope` builds.
    let template_symtab = step_symtab;

    // §4 item 6: each Step Environment's own `enableIf`, in the Step's
    // template scope, the scope of an entry's `enableIf` (constraint 2).
    // It reads no effective values, so it is decided here, with the
    // entries (plan 3 Q7).
    let mut step_environments_enabled = Vec::new();
    let mut disabled_step_environments = Vec::new();
    for (j, env) in st.step_environments.iter().flatten().enumerate() {
        let env_path = path_index(&path_field(&step_path, "stepEnvironments"), j);
        let enabled = resolve_enable_if(
            env.enable_if.as_ref(),
            template_symtab,
            policy,
            &env_path,
            errors,
        );
        if enabled == Some(false) {
            disabled_step_environments.push(env.name.clone());
        }
        step_environments_enabled.push(enabled != Some(false));
    }

    // Decide every `environment:` entry first (§7.4: `enableIf` before the
    // entry's name is resolved), in the template scope, so that
    // `entry_scope` re-evaluates only the bindings that enabled entries'
    // values read: a disabled entry has no effect of any kind (§3.2
    // item 2). `None` for a dependency that is not a decided entry.
    let mut seen: HashSet<&str> = HashSet::new();
    // Names of entries their `enableIf` disabled, for the constraint 7
    // message, and of those whose `enableIf` failed (reported): their
    // Environments' values are not computed, so the one mistake is
    // reported once.
    let mut disabled: HashSet<&str> = HashSet::new();
    let mut failed: HashSet<&str> = HashSet::new();
    let decisions: Vec<Option<bool>> = st
        .dependencies
        .iter()
        .flatten()
        .enumerate()
        .map(|(j, dep)| {
            let env_name = dep.environment()?;
            let entry_path = path_index(&deps_path, j);
            // §3.2 constraint 3, which template validation checks; checked
            // again for a template edited after decoding, so a second
            // entry is not silently ignored.
            if !seen.insert(env_name) {
                errors.add(
                    &path_field(&entry_path, "environment"),
                    format!("duplicate environment dependency '{env_name}'."),
                );
                return None;
            }
            match entry_enabled(dep, template_symtab, policy, &entry_path, errors) {
                Some(true) => Some(true),
                Some(false) => {
                    disabled.insert(env_name);
                    Some(false)
                }
                None => {
                    failed.insert(env_name);
                    None
                }
            }
        })
        .collect();

    let Some(step_symtab) = entry_scope(
        st,
        &decisions,
        step_symtab,
        let_base,
        policy,
        &step_path,
        errors,
    ) else {
        return StepEntries {
            environment_parameters: None,
            selection: StepSelection {
                frozen: Vec::new(),
                job_environments_entered: entered_by_scope(),
                disabled_job_environments: Vec::new(),
                disabled: Vec::new(),
                disabled_step_environments,
            },
            step_environments_enabled,
        };
    };
    let step_symtab = &*step_symtab;

    let mut supplied: HashMap<&str, Supplied<'_>> = HashMap::new();
    // Job Environments an enabled entry requests.
    let mut requested: HashSet<&str> = HashSet::new();
    let mut frozen = Vec::new();
    for ((j, dep), decision) in st.dependencies.iter().flatten().enumerate().zip(&decisions) {
        let (Some(env_name), Some(true)) = (dep.environment(), decision) else {
            continue;
        };
        let entry_path = path_index(&deps_path, j);
        let params = dep.parameters.as_ref().map_or(&empty, |p| &p.0);
        if job_envs.names.contains(env_name) {
            requested.insert(env_name);
            supplied.insert(
                env_name,
                Supplied {
                    params,
                    symtab: step_symtab,
                    entry_path,
                },
            );
            continue;
        }
        // An external Environment (§3.2 item 2): keep the values as
        // written with the symbols their format strings reference.
        let mut symtab = SymbolTable::new();
        for value in params.values() {
            if let EnvironmentParameterValue::FormatString(fs) = value {
                fs.copy_used_symtab_values(step_symtab, &mut symtab);
            }
        }
        frozen.push(FrozenEntry {
            environment: env_name.to_string(),
            dependency_index: j,
            required: dep.is_required(),
            parameters: params.clone(),
            symtab,
        });
    }

    // §4 item 4: the Step's Sessions enter every `ALWAYS` Job Environment and
    // every `ON_REQUEST` one it requests.
    let enters = |name: &str, scope: template::EnvironmentActivation| {
        scope == template::EnvironmentActivation::Always || requested.contains(name)
    };
    // §3.2.2: the Step's effective values for each Job Environment its
    // Sessions would enter, then the Environment's own `enableIf` (§4 item
    // 6) with the values that are known, then, for an Environment it does
    // not disable, the value checks and constraint 7 (§7.4).
    let mut values = Vec::new();
    let mut missing = Vec::new();
    // By index in `jobEnvironments`: the ones disabled, in order, and
    // whether each is left out (disabled; one whose `enableIf` failed is
    // checked as if it is not disabled).
    let mut env_disabled: Vec<usize> = Vec::new();
    let mut left_out = vec![false; job_envs.all.len()];
    let mut unresolved_labels: HashSet<&str> = HashSet::new();
    let mut failed_labels: HashSet<&str> = HashSet::new();
    for (de, &position) in job_envs.environments.iter().zip(&job_envs.positions) {
        if !enters(de.name, de.activation) || failed.contains(de.name) {
            continue;
        }
        let pending = de.values_for_step(
            supplied.get(de.name),
            &job_envs.default_symtab,
            policy,
            errors,
        );
        match de.decide(&pending, &st.name, &job_envs.enable_if_base, policy) {
            EnableIf::Disabled => {
                env_disabled.push(position);
                left_out[position] = true;
            }
            // A failure is a job-creation error (reported once every Step
            // is decided), so the submission stage does not run. The
            // Environment is checked as if it is not disabled, so the
            // Step's other mistakes for it are reported too.
            decision @ (EnableIf::Enabled | EnableIf::Unresolved | EnableIf::Failed) => {
                let (v, m) = de.commit(pending, &st.name, errors);
                values.extend(v);
                if !m.is_empty() {
                    match decision {
                        EnableIf::Unresolved => {
                            unresolved_labels.insert(de.label.as_str());
                        }
                        EnableIf::Failed => {
                            failed_labels.insert(de.label.as_str());
                        }
                        _ => {}
                    }
                    missing.push((de.label.clone(), m));
                }
            }
        }
    }
    let disabled_labels: HashSet<&str> = job_envs
        .environments
        .iter()
        .filter(|de| disabled.contains(de.name))
        .map(|de| de.label.as_str())
        .collect();
    report_missing(
        &step_path,
        &missing,
        tv::EVERY_JOB_ENVIRONMENT,
        &disabled_labels,
        &unresolved_labels,
        &failed_labels,
        errors,
    );

    StepEntries {
        environment_parameters: (!values.is_empty()).then_some(values),
        selection: StepSelection {
            frozen,
            disabled: disabled
                .iter()
                .filter(|n| !job_envs.names.contains(**n))
                .map(|n| n.to_string())
                .collect(),
            job_environments_entered: job_envs
                .all
                .iter()
                .enumerate()
                .map(|(k, (name, scope))| enters(name, *scope) && !left_out[k])
                .collect(),
            disabled_job_environments: env_disabled,
            disabled_step_environments,
        },
        step_environments_enabled,
    }
}

/// The scope a Step's entry parameter values resolve in.
///
/// Job creation evaluates the Step's `let` bindings once, in POSIX, for
/// every field of the Step. When the submission path format is another,
/// the bindings an enabled entry's value reads (`decisions[j]` is
/// `Some(true)` for an enabled entry `j`), directly or through other bindings,
/// are evaluated again from `let_base` (the scope before them) in that
/// format, so a `path` binding (`path(RawParam.Dir) / 'x'`,
/// `.parent`) follows the rules the value itself resolves with. Bindings
/// nothing there reads are not evaluated again (and not in the scope), so
/// one that would fail only in that format does not reject the Job. A
/// binding that is read and fails is reported at `steps[i] -> let[j]`, and
/// the Step's entry values are not computed (`None`); job creation fails
/// on the error. The entries' `enableIf`s and duplicate checks, and the
/// Step Environments' `enableIf`s, were made before this, so their errors
/// are reported with it.
#[allow(clippy::too_many_arguments)]
fn entry_scope<'s>(
    st: &template::StepTemplate,
    decisions: &[Option<bool>],
    step_symtab: &'s SymbolTable,
    let_base: Option<&SymbolTable>,
    policy: ResolvePolicy<'_>,
    step_path: &[PathElement],
    errors: &mut ValidationErrors,
) -> Option<std::borrow::Cow<'s, SymbolTable>> {
    use std::borrow::Cow;
    let format = policy.ctx.submission_path_format;
    let (true, Some(bindings), Some(let_base)) =
        (format != PathFormat::Posix, &st.let_bindings, let_base)
    else {
        return Some(Cow::Borrowed(step_symtab));
    };
    let profile = policy
        .ctx
        .profile
        .to_expr_profile(openjd_expr::HostContext::None);
    // The names the enabled entries' values read (`decisions`, index for
    // index with the Step's dependencies). A disabled entry has no effect
    // (§3.2 item 2), and an `enableIf` resolves in template scope (§3.2
    // item 5), so neither adds names here.
    let mut needed: HashSet<String> = st
        .dependencies
        .iter()
        .flatten()
        .zip(decisions)
        .filter(|(_, decision)| **decision == Some(true))
        .flat_map(|(d, _)| {
            d.parameters
                .iter()
                .flat_map(|p| p.iter())
                .filter_map(|(_, v)| v.as_format_string())
                .flat_map(|fs| fs.accessed_symbols())
        })
        .collect();
    // Each binding as (name, expression source), and whether an entry
    // value reads it, directly or through a later binding that is read.
    let parsed: Vec<Option<(&str, &str)>> = bindings
        .iter()
        .map(|b| {
            let (name, expr) = b.split_once('=')?;
            Some((name.trim(), expr.trim()))
        })
        .collect();
    let mut read = vec![false; bindings.len()];
    for (j, entry) in parsed.iter().enumerate().rev() {
        let Some((name, expr)) = entry else { continue };
        let reads_name = needed
            .iter()
            .any(|s| s == name || s.starts_with(&format!("{name}.")));
        if reads_name {
            read[j] = true;
            if let Ok(p) = openjd_expr::ParsedExpression::new(expr) {
                needed.extend(p.accessed_symbols().iter().cloned());
            }
        }
    }
    if !read.contains(&true) {
        return Some(Cow::Borrowed(step_symtab));
    }
    let lib = openjd_expr::FunctionLibrary::for_profile(&profile);
    // Only the read bindings are evaluated: a read binding refers only to
    // names the reverse pass marked read (or to the scope before the
    // bindings), and the entries read only those names.
    let mut scope = let_base.clone();
    for (j, binding) in bindings.iter().enumerate() {
        if !read[j] {
            continue;
        }
        match super::evaluate_let_bindings(
            std::slice::from_ref(binding),
            &scope,
            Some(&lib),
            format,
            policy.budgets.memory,
            policy.budgets.operations,
        ) {
            Ok(t) => scope = t,
            Err(e) => {
                errors.add(
                    &path_index(&path_field(step_path, "let"), j),
                    format!(
                        "in the submission path format ({}), which this Step's Environment \
                         parameter values resolve in: {e}",
                        format_name(format)
                    ),
                );
                return None;
            }
        }
    }
    Some(Cow::Owned(scope))
}
