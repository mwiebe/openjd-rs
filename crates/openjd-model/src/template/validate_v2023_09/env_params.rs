// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Pass 11: validate or reject `ENV_PARAMS` features (Environment
//! Parameters RFC, draft).
//!
//! Six schema additions are gated by the `ENV_PARAMS` extension:
//! - `envParameterDefinitions` on an `<Environment>` (§4 item 3, §4.5);
//! - `activation` on an `<Environment>` (§4 item 4);
//! - `hostRequirements` on an `<Environment>` (§4 item 5);
//! - `enableIf` on an `<Environment>` (§4 item 6), on every position,
//!   whose value pass 8 checks;
//! - the `step:` form of a `<StepDependency>`, a synonym of `dependsOn:`
//!   whose Step rules are pass 6's (§3.2 item 1);
//! - the `environment:` form of a `<StepDependency>` (§3.2).
//!
//! A document that lists `ENV_PARAMS` must also list `EXPR` (§1.1 item 3,
//! §1.2 item 3); [`check_expr_prerequisite`] rejects one that does not
//! before any pass runs, so every rule here and in pass 8 takes `EXPR` as
//! listed wherever `ENV_PARAMS` is.
//!
//! Without the extension each use is an error. With it, this pass checks
//! the structural rules that do not involve format strings:
//!
//! - §4 item 3 constraints 1–4: 1–50 definitions, unique names, and not on
//!   a `stepEnvironments` entry; §4 item 4 constraint 2: no `activation` on a
//!   `stepEnvironments` entry; §4 item 5.2: no `hostRequirements` on a
//!   `stepEnvironments` entry;
//! - §4.5 items 1–3 and 5: each type's definition checks (every Job
//!   Parameter type is available, since `EXPR` is listed), which include
//!   those of a
//!   `userInterface`, `objectType`, or `dataFlow` (permitted, and
//!   informational);
//! - §4.5 "checked at two points", point 1: a literal `default` against
//!   the parameter's type and constraints; item 4: a non-literal list
//!   `default` is a single `{{ }}` expression;
//! - §3.2 constraints 2–4 and 8 for every `environment:`
//!   entry, and the forms of a literal value that no parameter type accepts
//!   (an object, `null`, a list nested too deeply);
//! - §3.2 constraints 5 and 6 (against literal values) for entries that
//!   name a `jobEnvironments` entry of the same Job Template, unless their
//!   `enableIf` is `false`; and constraint 7 over each Step's Environment
//!   set, for entries whose `enableIf` is absent or a boolean scalar (the
//!   checked-at paragraph; §7.5 item 1). Constraints 6 and 7 are checked
//!   only for an Environment whose own `enableIf` is absent or `true`, or
//!   certainly unresolved for the Step ([`enable_if_certainly_unresolved`]): they
//!   do not apply to a Step for which it is disabled (§3.2.2), so for one
//!   whose `enableIf` is `false` they never apply, and for one whose
//!   `enableIf` is an expression a violation is not certain here (§7.5
//!   rule 5).
//!
//! §3.2 constraint 1 (exactly one of `dependsOn`, `step`, and
//! `environment`) is checked with the other dependency rules in pass 6. Format-string
//! values, `default`s, and `enableIf` expressions are checked against their
//! scopes in pass 8.

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};

use super::EffectiveLimits;
use crate::error::{path_field, path_index, PathElement, ValidationErrors};
use crate::template::expr_parameters::BoolValue;
use crate::template::{
    Environment, EnvironmentActivation, EnvironmentParameterDefinition, EnvironmentParameterValue,
    EnvironmentTemplate, FlexFloat, FlexInt, JobTemplate, StepDependency,
};
use crate::types::{JobParameterType, ModelExtension, ValidationContext};

/// §4 item 3 constraint 2 and §3.2 constraint 4: at most 50 definitions
/// on an Environment and at most 50 entries in a `parameters` map. Fixed
/// by the spec; not raised by `FEATURE_BUNDLE_1`.
const MAX_ENV_PARAMS: usize = 50;

/// The longest list of declared names a constraint 5 message carries.
const MAX_DECLARED_LIST_CHARS: usize = 512;

/// Where an Environment sits, which decides whether it may declare
/// parameters (§4 item 3 constraint 4).
#[derive(Clone, Copy, PartialEq, Eq)]
enum EnvPlacement {
    /// A `jobEnvironments` entry or an Environment Template's `environment`.
    Declaring,
    /// A `stepEnvironments` entry.
    Step,
}

/// One `jobEnvironments` entry, indexed for the checks of the entries that
/// name it. Built once per template, so each lookup is O(1) and the
/// constraint 5 list of declared names is built at most once per
/// Environment, and only if a message needs it.
struct JobEnvIndex<'a> {
    /// The Environment.
    env: &'a Environment,
    /// The Environment's `activation`, with its default applied.
    activation: EnvironmentActivation,
    /// Whether the Environment is known at template validation not to be
    /// disabled for any Step: its own `enableIf` (§4 item 6) is absent or
    /// `true`. Constraints 6 and 7 do not apply to a Step for which it is
    /// disabled (§3.2.2), so for any other Environment (a literal `false`,
    /// disabled; an expression, undetermined) they are not checked here
    /// (§7.5 rule 5): a violation need not hold whatever the `enableIf`
    /// resolves to, since `false` would remove it. The exception is an
    /// `enableIf` that is certainly unresolved for the Step
    /// ([`enable_if_certainly_unresolved`]), which is checked per entry.
    certainly_enabled: bool,
    /// The definitions by name; for a duplicated name, the first.
    defs: HashMap<&'a str, &'a EnvironmentParameterDefinition>,
    /// The declared names in written order, each once.
    names: Vec<&'a str>,
    /// The constraint 5 message's list of declared names.
    declared: OnceCell<String>,
}

impl<'a> JobEnvIndex<'a> {
    fn new(env: &'a Environment) -> Self {
        let mut defs = HashMap::new();
        let mut names = Vec::new();
        for d in env.env_parameter_definitions.iter().flatten() {
            if !defs.contains_key(d.name()) {
                defs.insert(d.name(), d);
                names.push(d.name());
            }
        }
        Self {
            env,
            activation: env.activation(),
            certainly_enabled: env.literal_enabled() == Some(true),
            defs,
            names,
            declared: OnceCell::new(),
        }
    }

    /// The declared names for a constraint 5 message; see
    /// [`capped_name_list`].
    fn declared(&self) -> &str {
        self.declared
            .get_or_init(|| capped_name_list(self.names.iter().copied(), self.names.len()))
    }
}

/// A comma-separated list of `names` for a message: at most 50 names and
/// [`MAX_DECLARED_LIST_CHARS`] characters (the first name is always
/// listed), then "and N more" for the rest of `total`, so an over-long
/// list (already an error) or long names cannot make a message
/// arbitrarily long. Consumes `names` only as far as it lists.
pub(crate) fn capped_name_list<'n>(names: impl Iterator<Item = &'n str>, total: usize) -> String {
    let mut text = String::new();
    let mut shown = 0usize;
    for name in names {
        if shown == MAX_ENV_PARAMS
            || (shown > 0 && text.len() + 2 + name.len() > MAX_DECLARED_LIST_CHARS)
        {
            break;
        }
        if shown > 0 {
            text.push_str(", ");
        }
        text.push_str(name);
        shown += 1;
    }
    if total > shown {
        text.push_str(&format!(", and {} more", total - shown));
    }
    text
}

/// The §3.2 constraint 7 message for one Step: `missing` yields
/// `(environment label, parameter name)` pairs grouped by label, in
/// order, and `total` is how many there are. `why` says which Environments
/// the Step's Sessions enter, and cites the rule. A label in `disabled`
/// names an Environment the Step has an `environment:` entry for whose
/// `enableIf` disabled it (§3.2 item 5), which its group says, since a
/// reader would otherwise take the Step's entry as enabled. A label in
/// `unresolved` names an Environment whose own `enableIf` (§4 item 6) was
/// unresolved for the Step, which its group also says: the `enableIf`
/// depends on a value that is missing or does not convert, so it does not
/// disable the Environment (§3.2.2), and a reader who expected it to be
/// `false` would otherwise not see why the parameters are required.
///
/// One error per Step, so the report grows with the number of Steps and
/// not with Steps times parameters. The list holds at most 50 names and
/// [`MAX_DECLARED_LIST_CHARS`] characters (the first name is always
/// listed, however long), then "and N more"; `missing` is consumed only
/// as far as the list goes, so a Step costs O(50) here.
pub(crate) fn missing_parameters_message<'n>(
    missing: impl Iterator<Item = (&'n str, &'n str)>,
    total: usize,
    why: &str,
    disabled: &HashSet<&str>,
    unresolved: &HashSet<&str>,
    failed: &HashSet<&str>,
) -> String {
    let note = |label: &str| {
        let mut note = String::new();
        if disabled.contains(label) {
            note.push_str(DISABLED_NOTE);
        }
        if unresolved.contains(label) {
            note.push_str(UNRESOLVED_NOTE);
        }
        if failed.contains(label) {
            note.push_str(FAILED_NOTE);
        }
        note
    };
    let mut groups: Vec<String> = Vec::new();
    let mut listed = 0usize;
    // The text of the completed groups, with their "; " separators.
    let mut list_len = 0usize;
    // The open group: its label, its names, and its text so far
    // (the names plus the " of Environment <label>" suffix).
    let mut current: Option<(&str, Vec<String>, usize)> = None;
    let close = |groups: &mut Vec<String>, list_len: &mut usize, g: (&str, Vec<String>, usize)| {
        if !g.1.is_empty() {
            *list_len += if groups.is_empty() { 0 } else { 2 } + g.2;
            groups.push(format!(
                "{} of Environment {}{}",
                g.1.join(", "),
                g.0,
                note(g.0)
            ));
        }
    };
    for (label, name) in missing {
        if current.as_ref().is_some_and(|g| g.0 != label) {
            close(
                &mut groups,
                &mut list_len,
                current.take().unwrap_or_default(),
            );
        }
        let group = current.get_or_insert_with(|| {
            (
                label,
                Vec::new(),
                " of Environment ".len() + label.len() + note(label).len(),
            )
        });
        let add = name.len() + 2 + if group.1.is_empty() { 0 } else { 2 };
        let sep = if groups.is_empty() { 0 } else { 2 };
        if listed == MAX_ENV_PARAMS
            || (listed > 0 && list_len + sep + group.2 + add > MAX_DECLARED_LIST_CHARS)
        {
            break;
        }
        listed += 1;
        group.2 += add;
        group.1.push(format!("'{name}'"));
    }
    if let Some(g) = current {
        close(&mut groups, &mut list_len, g);
    }
    let mut list = groups.join("; ");
    if total > listed {
        list.push_str(&format!("; and {} more", total - listed));
    }
    format!(
        "must supply, in an 'environment:' dependency, every parameter without a default of each \
         Environment its Sessions enter ({why}); missing: {list}."
    )
}

/// The note [`missing_parameters_message`] adds to an Environment's group
/// when the Step's entry for it is disabled by its `enableIf`.
const DISABLED_NOTE: &str = " (the Step's 'environment:' dependency for it is disabled by its \
     enableIf, so it supplies nothing; Template Schemas §3.2 item 5)";

/// The note [`missing_parameters_message`] adds to an Environment's group
/// when the Environment's own `enableIf` (§4 item 6) was unresolved for
/// the Step.
const UNRESOLVED_NOTE: &str = " (its own enableIf depends on a value that is missing or does \
     not convert, so it does not disable the Environment; Template Schemas §3.2.2)";

/// The note for an Environment whose own `enableIf` failed to evaluate for
/// the Step: the failure is reported at the `enableIf`, and the Environment
/// is checked as if it is not disabled so the Step's other mistakes are
/// reported in the same run.
const FAILED_NOTE: &str = " (its own enableIf could not be evaluated for this Step, so the \
     Environment is checked as if it is not disabled)";

/// The constraint 7 reason for `jobEnvironments` entries, which completes
/// [`missing_parameters_message`].
pub(crate) const EVERY_JOB_ENVIRONMENT: &str =
    "every jobEnvironments entry with activation ALWAYS, \
     and each one with activation ON_REQUEST that the Step requests; Template Schemas §3.2 \
     constraint 7";

/// The prerequisite of `ENV_PARAMS`: a document that lists it must also
/// list `EXPR` (Template Schemas §1.1 item 3; §1.2 item 3 gives an
/// Environment Template the same prerequisites). `section` is the item
/// the message cites. Reports at `extensions` and returns `false` when
/// the document breaks the rule; the caller then runs no other pass, so
/// every other check here, in pass 8, and at job creation may take `EXPR`
/// as listed wherever `ENV_PARAMS` is.
pub(super) fn check_expr_prerequisite(
    ctx: &ValidationContext,
    section: &str,
    errors: &mut ValidationErrors,
) -> bool {
    let ok = !ctx.profile.has_extension(ModelExtension::EnvParams)
        || ctx.profile.has_extension(ModelExtension::Expr);
    if !ok {
        errors.add(
            &path_field(&[], "extensions"),
            format!(
                "ENV_PARAMS requires EXPR; both must be listed in the template's `extensions` \
                 (Template Schemas {section})."
            ),
        );
    }
    ok
}

/// Validate `ENV_PARAMS` rules for a Job Template.
pub fn validate_env_params_job_template(
    jt: &JobTemplate,
    ctx: &ValidationContext,
    errors: &mut ValidationErrors,
) {
    let active = ctx.profile.has_extension(ModelExtension::EnvParams);
    let limits = EffectiveLimits::from_context(ctx);

    let root: Vec<PathElement> = vec![];
    if let Some(envs) = &jt.job_environments {
        let envs_path = path_field(&root, "jobEnvironments");
        for (i, env) in envs.iter().enumerate() {
            check_environment(
                env,
                &path_index(&envs_path, i),
                EnvPlacement::Declaring,
                active,
                &limits,
                errors,
            );
        }
    }

    // Job Environments by name, for the checks of entries that name one.
    // Names are unique within `jobEnvironments` (pass 6), so the first
    // match is the only one in a valid template.
    let mut job_envs: HashMap<&str, JobEnvIndex<'_>> = HashMap::new();
    for env in jt.job_environments.iter().flatten() {
        job_envs
            .entry(env.name.as_str())
            .or_insert_with(|| JobEnvIndex::new(env));
    }

    for (i, step) in jt.steps.iter().enumerate() {
        let step_path = path_index(&path_field(&root, "steps"), i);
        if let Some(envs) = &step.step_environments {
            let envs_path = path_field(&step_path, "stepEnvironments");
            for (j, env) in envs.iter().enumerate() {
                check_environment(
                    env,
                    &path_index(&envs_path, j),
                    EnvPlacement::Step,
                    active,
                    &limits,
                    errors,
                );
            }
        }

        let Some(deps) = &step.dependencies else {
            continue;
        };
        let deps_path = path_field(&step_path, "dependencies");
        let step_env_names: HashSet<&str> = step
            .step_environments
            .iter()
            .flatten()
            .map(|e| e.name.as_str())
            .collect();
        let mut seen_envs: HashSet<&str> = HashSet::new();
        for (j, dep) in deps.iter().enumerate() {
            let dep_path = path_index(&deps_path, j);
            // §3.2 item 1: `step` is the ENV_PARAMS synonym of `dependsOn`;
            // its Step rules are pass 6's.
            if dep.step.is_some() && !active {
                errors.add(
                    &dep_path,
                    "the 'step' form of a dependency requires the ENV_PARAMS extension.",
                );
            }
            if dep.environment.is_none() {
                continue;
            }
            if !active {
                errors.add(
                    &dep_path,
                    "the 'environment' form of a dependency requires the ENV_PARAMS extension.",
                );
                continue;
            }
            // §4.1 `<EnvironmentName>`: the name is checked whatever the
            // entry's form, since an invalid name can never resolve.
            if let Some(name) = &dep.environment {
                check_environment_name(
                    name,
                    &path_field(&dep_path, "environment"),
                    &limits,
                    errors,
                );
            }
            // A malformed entry (`environment` with `dependsOn` or `step`)
            // is reported by pass 6; its environment half is not checked.
            let Some(env_name) = dep.environment() else {
                continue;
            };
            check_environment_entry(
                dep,
                env_name,
                &dep_path,
                &step_env_names,
                &mut seen_envs,
                job_envs.get(env_name),
                &limits,
                ctx,
                errors,
            );
        }
    }

    if active {
        check_required_parameters_supplied(jt, &job_envs, ctx, errors);
    }
}

/// Validate `ENV_PARAMS` rules for an Environment Template.
pub fn validate_env_params_environment_template(
    et: &EnvironmentTemplate,
    ctx: &ValidationContext,
    errors: &mut ValidationErrors,
) {
    let active = ctx.profile.has_extension(ModelExtension::EnvParams);
    let limits = EffectiveLimits::from_context(ctx);
    check_environment(
        &et.environment,
        &path_field(&[], "environment"),
        EnvPlacement::Declaring,
        active,
        &limits,
        errors,
    );
}

/// §4 item 6 constraint 4: whether one Environment's `enableIf` is
/// permitted; §4 item 4: its `activation`; §4 item 5: where its
/// `hostRequirements` are permitted; §4 item 3 and §4.5: its
/// `envParameterDefinitions`.
fn check_environment(
    env: &Environment,
    env_path: &[PathElement],
    placement: EnvPlacement,
    active: bool,
    limits: &EffectiveLimits,
    errors: &mut ValidationErrors,
) {
    // §4 item 6 constraint 4: `enableIf` needs ENV_PARAMS in the document,
    // on every position (a `stepEnvironments` entry included). Its value,
    // an expression field of type `bool` (§7.5), is checked against its
    // scope in pass 8.
    if env.enable_if.is_some() && !active {
        errors.add(
            &path_field(env_path, "enableIf"),
            "enableIf on an Environment requires the ENV_PARAMS extension.",
        );
    }
    // §4 item 4 constraint 2. Constraint 1 (an unrecognized value) is a
    // decode error.
    if env.activation.is_some() {
        let path = path_field(env_path, "activation");
        if !active {
            errors.add(
                &path,
                "activation on an Environment requires the ENV_PARAMS extension.",
            );
        } else if placement == EnvPlacement::Step {
            errors.add(
                &path,
                "is not permitted on a stepEnvironments entry, which only its own Step's \
                 Sessions enter.",
            );
        }
    }
    // §4 item 5 and item 5.2. The object's own §3.3 checks are made by
    // pass 6, and its format strings by pass 8, only where it is permitted.
    if env.host_requirements.is_some() {
        let path = path_field(env_path, "hostRequirements");
        if !active {
            errors.add(
                &path,
                "hostRequirements on an Environment requires the ENV_PARAMS extension.",
            );
        } else if placement == EnvPlacement::Step {
            errors.add(
                &path,
                "is not permitted on a stepEnvironments entry, whose Step has hostRequirements \
                 of its own.",
            );
        }
    }
    let Some(defs) = &env.env_parameter_definitions else {
        return;
    };
    let defs_path = path_field(env_path, "envParameterDefinitions");
    if !active {
        errors.add(
            &defs_path,
            "envParameterDefinitions on an Environment requires the ENV_PARAMS extension.",
        );
        return;
    }
    if placement == EnvPlacement::Step {
        errors.add(
            &defs_path,
            "is not permitted on a stepEnvironments entry; only a jobEnvironments entry or an \
             Environment Template's environment may declare parameters.",
        );
        return;
    }
    if defs.is_empty() {
        errors.add(
            &defs_path,
            "if provided, must contain at least one element.",
        );
    }
    if defs.len() > MAX_ENV_PARAMS {
        errors.add(
            &defs_path,
            format!("must not contain more than {MAX_ENV_PARAMS} elements."),
        );
    }
    let mut names = HashSet::new();
    for (k, def) in defs.iter().enumerate() {
        let def_path = path_index(&defs_path, k);
        if !names.insert(def.name()) {
            errors.add(
                &def_path,
                format!("duplicate parameter name: '{}'", def.name()),
            );
        }
        check_definition(def, &def_path, limits, errors);
    }
}

/// §4.5 items 1–5 for one definition, and its literal `default`.
fn check_definition(
    def: &EnvironmentParameterDefinition,
    def_path: &[PathElement],
    limits: &EffectiveLimits,
    errors: &mut ValidationErrors,
) {
    let pt = def.job_param_type();
    if def.name().chars().count() > limits.max_identifier_len {
        errors.add(
            def_path,
            format!("name exceeds {} characters.", limits.max_identifier_len),
        );
    }
    // §4.5 item 1: any type a `<JobParameterDefinition>` has in a document
    // that lists `EXPR`, which `ENV_PARAMS` requires, so every type decodes
    // to one that is allowed. §4.5 items 2-3: `userInterface`,
    // `objectType`, and `dataFlow` are permitted, with the shape and
    // constraints of the Job Parameter of the same type (checked by
    // `validate_definition` below); they are informational and change
    // nothing about job creation or how the Job runs.
    //
    // §4.5 item 5: the definition's own constraints, as for the Job
    // Parameter of the same type.
    if let Err(msgs) = def.definition.validate_definition(limits) {
        for m in msgs {
            errors.add(def_path, m);
        }
    }
    let Some(default) = &def.default else {
        return;
    };
    let default_path = path_field(def_path, "default");
    match default {
        // §4.5 point 1: a literal `default` is checked at template
        // validation against the type and constraints.
        EnvironmentParameterValue::Literal(lit) => {
            let _ = check_literal_value(def, lit, &default_path, errors);
        }
        // §4.5 item 4: a list type has no string form.
        EnvironmentParameterValue::FormatString(fs) => {
            check_list_format_string(pt, fs, &default_path, errors);
        }
    }
}

/// §3.2 constraints 2–6 and 8 for one `environment:` entry. Constraints 5
/// and 6 are skipped for an entry whose `enableIf` is `false`, and
/// constraint 6 for a Job Environment whose own `enableIf` is not absent or
/// `true` (§3.2.2, §7.5 rule 5).
#[allow(clippy::too_many_arguments)]
fn check_environment_entry<'a>(
    dep: &'a StepDependency,
    env_name: &'a str,
    dep_path: &[PathElement],
    step_env_names: &HashSet<&str>,
    seen_envs: &mut HashSet<&'a str>,
    job_env: Option<&JobEnvIndex<'_>>,
    limits: &EffectiveLimits,
    ctx: &ValidationContext,
    errors: &mut ValidationErrors,
) {
    let env_path = path_field(dep_path, "environment");
    // Constraint 2.
    if step_env_names.contains(env_name) {
        errors.add(
            &env_path,
            format!(
                "must not name '{env_name}', a stepEnvironments entry of the same Step. \
                 A Step Environment cannot declare parameters."
            ),
        );
    }
    // Constraint 3.
    if !seen_envs.insert(env_name) {
        errors.add(
            &env_path,
            format!("duplicate environment dependency '{env_name}'."),
        );
    }
    // Constraint 8.
    if job_env.is_some() && dep.required == Some(false) {
        errors.add(
            &path_field(dep_path, "required"),
            format!(
                "must not be false: '{env_name}' is a jobEnvironments entry, which is always \
                 present."
            ),
        );
    }

    let Some(params) = &dep.parameters else {
        return;
    };
    let params_path = path_field(dep_path, "parameters");
    // Constraint 4.
    if params.is_empty() {
        errors.add(
            &params_path,
            "if provided, must contain at least one entry.",
        );
    }
    if params.len() > MAX_ENV_PARAMS {
        errors.add(
            &params_path,
            format!("must not contain more than {MAX_ENV_PARAMS} entries."),
        );
    }

    // An entry whose `enableIf` is `false` has no effect of any kind
    // (§3.2 item 2), so constraints 5 and 6 do not apply to it (plan A28).
    // Constraints 2–4 and 8, above, apply to entries as written.
    let job_env = job_env.filter(|_| dep.literal_enabled() != Some(false));
    // Constraint 6 applies to the values of an Environment that is not
    // disabled for the Step (§3.2.2): one whose `enableIf` is absent or
    // `true`, or certainly unresolved with this entry's values, which needs
    // the entry to be certainly enabled: a disabled one supplies nothing.
    let values_checked = job_env.is_some_and(|e| {
        e.certainly_enabled
            || (dep.literal_enabled() == Some(true)
                && enable_if_certainly_unresolved(e.env, Some(&params.0), ctx))
    });

    // Constraint 5 for a Job Environment that declares no parameters.
    if job_env.is_some_and(|e| e.names.is_empty()) && !params.is_empty() {
        errors.add(
            &params_path,
            format!(
                "Environment '{env_name}' declares no parameters, so this entry must not \
                 supply any."
            ),
        );
        // Every key would repeat the same error.
        return;
    }

    // Only the first undeclared key of an entry lists the declared names,
    // so an entry with many undeclared keys does not repeat the list.
    let mut listed_declared = false;
    for (key, value) in params.iter() {
        let value_path = path_field(&params_path, key);
        if !is_identifier(key) || key.chars().count() > limits.max_identifier_len {
            errors.add(
                &value_path,
                format!(
                    "'{key}' is not a valid parameter name: it must match \
                     [A-Za-z_][A-Za-z0-9_]* and be at most {} characters.",
                    limits.max_identifier_len
                ),
            );
            continue;
        }
        // The forms no parameter type accepts (A4).
        if let EnvironmentParameterValue::Literal(lit) = value {
            if !check_literal_form(lit, &value_path, errors) {
                continue;
            }
        }
        let Some(job_env) = job_env else {
            // External Environment: its definitions are known only when
            // the Job is created.
            continue;
        };
        // Constraint 5.
        let Some(def) = job_env.defs.get(key.as_str()) else {
            let msg = if listed_declared {
                format!("Environment '{env_name}' declares no parameter '{key}'.")
            } else {
                listed_declared = true;
                format!(
                    "Environment '{env_name}' declares no parameter '{key}'. Declared: {}.",
                    job_env.declared()
                )
            };
            errors.add(&value_path, msg);
            continue;
        };
        // Constraint 6, against literal values, and the single-expression
        // rule for a list-typed parameter (a list type has no string form,
        // so another format string cannot convert to it), for an
        // Environment that is certainly not disabled: a Step's values for
        // an Environment disabled for it are not checked against their
        // types and constraints (§3.2.2), so for any other Environment a
        // violation is not certain here (§7.5 rule 5).
        if !values_checked {
            continue;
        }
        match value {
            EnvironmentParameterValue::Literal(lit) => {
                let _ = check_literal_value(def, lit, &value_path, errors);
            }
            EnvironmentParameterValue::FormatString(fs) => {
                check_list_format_string(def.job_param_type(), fs, &value_path, errors);
            }
        }
    }
}

/// §3.2 constraint 7, for Job Environments: every Step supplies every
/// parameter without a `default` of every `jobEnvironments` entry its
/// Sessions enter. They enter every entry with `activation: ALWAYS`, and every
/// entry with `activation: ON_REQUEST` that the Step requests with an
/// enabled `environment:` entry.
///
/// Whether an entry is enabled is known here only when its `enableIf` is
/// absent or a boolean scalar (§3.2, checked-at paragraph; §7.5 item 1).
/// An entry whose `enableIf` is an expression, even a constant one such as
/// `"true"`, defers the check for its Environment to job creation; an
/// `enableIf: false` entry is as if absent, so for an `ALWAYS` Environment
/// every parameter takes its `default`.
///
/// Constraint 7 does not apply to a Step for which the Environment is
/// disabled (§3.2.2), so only an Environment whose own `enableIf` (§4 item
/// 6) is absent or `true`, or certainly unresolved for the Step
/// ([`enable_if_certainly_unresolved`], with its entry's values), is
/// checked here, the latter with the unresolved note. One whose `enableIf`
/// is `false` is disabled for every Step; one whose `enableIf` is any other
/// expression is undetermined, and disabled it requires nothing, so a
/// violation need not hold whatever it resolves to (§7.5 rule 5) and the
/// check is left to job creation.
///
/// Linear: a Step costs O(its entries), plus O(50) to list the missing
/// names when there are any. An Environment name or a parameter name that
/// is duplicated (reported elsewhere) is checked once, against the first
/// entry that names the Environment.
fn check_required_parameters_supplied(
    jt: &JobTemplate,
    job_envs: &HashMap<&str, JobEnvIndex<'_>>,
    ctx: &ValidationContext,
    errors: &mut ValidationErrors,
) {
    // Distinct Job Environment names in written order, with their
    // `activation` and required parameters.
    struct Required<'a> {
        index: &'a JobEnvIndex<'a>,
        env: &'a str,
        label: String,
        activation: EnvironmentActivation,
        names: Vec<&'a str>,
        /// The Environment's own `enableIf` is an expression: it counts for
        /// a Step only where it is certainly unresolved.
        conditional: bool,
        /// For a conditional one, whether it counts for a Step without an
        /// enabled entry for it, which supplies nothing.
        counts_without_entry: bool,
    }
    let mut required: Vec<Required<'_>> = Vec::new();
    let mut seen = HashSet::new();
    for env in jt.job_environments.iter().flatten() {
        if !seen.insert(env.name.as_str()) {
            continue;
        }
        let Some(index) = job_envs.get(env.name.as_str()) else {
            continue;
        };
        // Disabled for every Step.
        if env.literal_enabled() == Some(false) {
            continue;
        }
        let names: Vec<&str> = index
            .names
            .iter()
            .copied()
            .filter(|n| index.defs[n].default.is_none())
            .collect();
        if names.is_empty() {
            continue;
        }
        let conditional = !index.certainly_enabled;
        // An `ON_REQUEST` Environment is entered only through an entry.
        let counts_without_entry = !conditional
            || (index.activation == EnvironmentActivation::Always
                && enable_if_certainly_unresolved(index.env, None, ctx));
        required.push(Required {
            index,
            env: env.name.as_str(),
            label: format!("'{}'", env.name),
            activation: index.activation,
            names,
            conditional,
            counts_without_entry,
        });
    }
    if required.is_empty() {
        return;
    }
    let by_name: HashMap<&str, &Required<'_>> = required.iter().map(|r| (r.env, r)).collect();
    let all_count: usize = required
        .iter()
        .filter(|r| r.activation == EnvironmentActivation::Always && r.counts_without_entry)
        .map(|r| r.names.len())
        .sum();
    for (i, step) in jt.steps.iter().enumerate() {
        // The first entry for each Job Environment with required
        // parameters, matched on the `environment` key as written, so an
        // entry that also has `dependsOn` (reported by pass 6) does not add
        // a second, cascading error here.
        let mut entries: HashMap<&str, &StepDependency> = HashMap::new();
        for d in step.dependencies.iter().flatten() {
            if let Some(env) = d.environment.as_deref() {
                if by_name.contains_key(env) {
                    entries.entry(env).or_insert(d);
                }
            }
        }
        let supplied = |d: &StepDependency, r: &Required<'_>| {
            r.names
                .iter()
                .filter(|n| d.parameters.as_ref().is_some_and(|p| p.get(n).is_some()))
                .count()
        };
        // Whether the Environment of an entry enabled by `enableIf: true`
        // (or no `enableIf`) counts for this Step: always, unless its own
        // `enableIf` is an expression, which counts only where it is
        // certainly unresolved with the entry's values. One evaluation per
        // entry.
        let counts_with: HashMap<&str, bool> = entries
            .iter()
            .filter(|(_, d)| d.literal_enabled() == Some(true))
            .map(|(env, d)| {
                let r = by_name[env];
                let counts = !r.conditional
                    || enable_if_certainly_unresolved(
                        r.index.env,
                        d.parameters.as_ref().map(|p| &p.0),
                        ctx,
                    );
                (*env, counts)
            })
            .collect();
        // Count the missing names from the Step's entries alone: every
        // counted `ALWAYS` Environment's names start missing; an enabled
        // entry replaces that with what it leaves missing, if its
        // Environment counts, and adds the names of an `ON_REQUEST` one it
        // requests; an expression entry `enableIf` defers its Environment.
        let mut missing = all_count;
        for (env, d) in &entries {
            let r = by_name[env];
            let base = if r.activation == EnvironmentActivation::Always && r.counts_without_entry {
                r.names.len()
            } else {
                0
            };
            match d.literal_enabled() {
                None => missing -= base,
                Some(false) => {}
                Some(true) => {
                    missing -= base;
                    if counts_with[env] {
                        missing += r.names.len() - supplied(d, r);
                    }
                }
            }
        }
        if missing == 0 {
            continue;
        }
        // Whether `r` counts for this Step, and the entry parameters that
        // supply its names.
        let entered = |r: &Required<'_>| {
            let entry = entries.get(r.env).copied();
            match entry.map(StepDependency::literal_enabled) {
                Some(None) => (false, None),
                Some(Some(true)) => (
                    counts_with[r.env],
                    entry.and_then(|d| d.parameters.as_ref()),
                ),
                None | Some(Some(false)) => (
                    r.activation == EnvironmentActivation::Always && r.counts_without_entry,
                    None,
                ),
            }
        };
        // The `ALWAYS` Environments whose entry a scalar `enableIf: false`
        // disabled: their names are missing although the entry lists them.
        let disabled: HashSet<&str> = required
            .iter()
            .filter(|r| {
                entries
                    .get(r.env)
                    .is_some_and(|d| d.literal_enabled() == Some(false))
            })
            .map(|r| r.label.as_str())
            .collect();
        // The ones that count only because their `enableIf` is certainly
        // unresolved, which the message says.
        let unresolved: HashSet<&str> = required
            .iter()
            .filter(|r| r.conditional && entered(r).0)
            .map(|r| r.label.as_str())
            .collect();
        let missing_pairs = required.iter().flat_map(|r| {
            let (counts, params) = entered(r);
            r.names
                .iter()
                .filter(move |n| counts && !params.is_some_and(|p| p.get(n).is_some()))
                .map(move |n| (r.label.as_str(), *n))
        });
        errors.add(
            &path_index(&path_field(&[], "steps"), i),
            missing_parameters_message(
                missing_pairs,
                missing,
                EVERY_JOB_ENVIRONMENT,
                &disabled,
                &unresolved,
                &HashSet::new(),
            ),
        );
    }
}

/// Whether `env`'s own `enableIf` (§4 item 6) is certainly unresolved for
/// a Step that supplies `supplied` (its enabled `environment:` entry's
/// `parameters`, if any), before job creation (§3.2.2, §7.5 rule 5).
///
/// That holds when the expression reads only `Env.Param.*` and
/// `Env.RawParam.*` of the Environment, every value it reads is a literal
/// (supplied, or the `default`) or has no value at all, and progressive
/// evaluation with the literals that convert and `unresolved[T]` for the
/// rest gives an unresolved result. Job creation evaluates it with exactly
/// those values, so the result is the same there: whatever the Job
/// Parameters are, the Environment is not disabled for the Step, and a
/// check that applies only to an Environment that is not disabled fails at
/// job creation if it fails here. A known result (`true` or `false`), as of
/// a constant expression, stays unknown before job creation (§7.5 rule 5),
/// so this returns `false` for it, and for anything else that cannot be
/// decided here (a format-string value or `default`, another symbol, a
/// parse or evaluation error).
pub(crate) fn enable_if_certainly_unresolved(
    env: &Environment,
    supplied: Option<&std::collections::BTreeMap<String, EnvironmentParameterValue>>,
    ctx: &ValidationContext,
) -> bool {
    use crate::template::expr_field::{self, ExprFieldEval};
    let Some(field) = &env.enable_if else {
        return false;
    };
    let profile = ctx.profile.to_expr_profile(openjd_expr::HostContext::None);
    let library = openjd_expr::FunctionLibrary::for_profile(&profile);
    let ev = ExprFieldEval {
        profile: &profile,
        library: &library,
        memory_limit: ctx.caller_limits.max_eval_memory_bytes,
        operation_limit: ctx.caller_limits.max_eval_operations,
    };
    let Some(symbols) = expr_field::accessed_symbols(field, &ev) else {
        return false;
    };
    let defs = env.env_parameter_definitions.as_deref().unwrap_or(&[]);
    let mut symtab = openjd_expr::SymbolTable::new();
    for symbol in &symbols {
        let Some(rest) = symbol
            .strip_prefix("Env.Param.")
            .or_else(|| symbol.strip_prefix("Env.RawParam."))
        else {
            return false;
        };
        let name = rest.split('.').next().unwrap_or(rest);
        let Some(def) = defs.iter().find(|d| d.name() == name) else {
            return false;
        };
        let pt = def.job_param_type();
        // A path type is in scope only as `Env.RawParam.*`, its unmapped
        // string (the Job Parameter form `convert_literal_value` returns).
        let is_path = matches!(pt, JobParameterType::Path | JobParameterType::ListPath);
        if is_path && symbol.starts_with("Env.Param.") {
            return false;
        }
        let written = supplied.and_then(|p| p.get(name)).or(def.default.as_ref());
        let value = match written {
            Some(EnvironmentParameterValue::FormatString(_)) => return false,
            Some(EnvironmentParameterValue::Literal(lit)) => {
                let mut scratch = ValidationErrors::default();
                convert_literal_value(def, lit, &[], &mut scratch).0
            }
            None => None,
        };
        // A RANGE_EXPR literal is the `range_expr` its text parses to, as
        // job creation and the Session have it.
        let value = value.unwrap_or_else(|| {
            openjd_expr::ExprValue::unresolved(if is_path {
                pt.raw_expr_type()
            } else {
                pt.expr_type()
            })
        });
        let forms: &[&str] = if is_path {
            &["Env.RawParam"]
        } else {
            &["Env.Param", "Env.RawParam"]
        };
        for form in forms {
            if symtab
                .set(&format!("{form}.{name}"), value.clone())
                .is_err()
            {
                return false;
            }
        }
    }
    matches!(
        expr_field::resolve_progressive(field, &openjd_expr::ExprType::BOOL, &symtab, &ev),
        Ok(None)
    )
}

/// §4.1 `<EnvironmentName>` for the `environment` key of a dependency
/// entry: 1 to `max_env_name_len` characters, no Cc characters. The
/// messages match the ones for an `<Environment>`'s own `name`.
fn check_environment_name(
    name: &str,
    path: &[PathElement],
    limits: &EffectiveLimits,
    errors: &mut ValidationErrors,
) {
    if name.is_empty() {
        errors.add(path, "must not be empty.");
    }
    if name.chars().count() > limits.max_env_name_len {
        errors.add(
            path,
            format!("exceeds {} characters.", limits.max_env_name_len),
        );
    }
    if name.chars().any(char::is_control) {
        errors.add(path, "contains control characters.");
    }
}

/// §3.2 `<EnvironmentParameterValue>`, last sentence, and §4.5 item 4: a
/// list-typed parameter has no string form, so a format-string value for
/// it must be a single `{{ }}` expression.
pub(crate) fn check_list_format_string(
    pt: JobParameterType,
    fs: &crate::format_string::FormatString,
    path: &[PathElement],
    errors: &mut ValidationErrors,
) {
    if crate::job::create_job::parameters::list_element_type(pt).is_some()
        && fs.segment_count() != 1
    {
        errors.add(
            path,
            format!(
                "a {} parameter has no string form, so a format-string value for it must be a \
                 single '{{{{ }}}}' expression.",
                pt.as_spec_str()
            ),
        );
    }
}

/// The literal forms of an entry value that no parameter type accepts:
/// an object or `null` anywhere, and a list nested more than one level,
/// since `LIST[LIST[INT]]` is the only nested type. Returns `false` when
/// it reported an error.
fn check_literal_form(
    lit: &serde_json::Value,
    path: &[PathElement],
    errors: &mut ValidationErrors,
) -> bool {
    let before = errors.len();
    check_no_object_or_null(lit, path, errors);
    if errors.len() > before {
        return false;
    }
    if let serde_json::Value::Array(items) = lit {
        // A list either holds lists (LIST[LIST[INT]], the only nested
        // type) or holds none: no type accepts a mix.
        let nested = items.iter().filter(|i| i.is_array()).count();
        if nested != 0 && nested != items.len() {
            errors.add(
                path,
                "must not mix lists and other values: LIST[LIST[INT]] is the only nested type.",
            );
            return false;
        }
        for (k, item) in items.iter().enumerate() {
            if let serde_json::Value::Array(inner) = item {
                for (m, v) in inner.iter().enumerate() {
                    // An inner element is an INT literal, decoded as the
                    // LIST[LIST[INT]] `default` decodes it.
                    if serde_json::from_value::<FlexInt>(v.clone()).is_err() {
                        errors.add(
                            &path_index(&path_index(path, k), m),
                            "a nested list may hold only integers or integer strings \
                             (LIST[LIST[INT]] is the only nested type).",
                        );
                    }
                }
            }
        }
    }
    errors.len() == before
}

/// Report an object or `null`, at any depth of `lit`.
fn check_no_object_or_null(
    lit: &serde_json::Value,
    path: &[PathElement],
    errors: &mut ValidationErrors,
) {
    match lit {
        serde_json::Value::Null => errors.add(
            path,
            "must not be null: no parameter type has a null value.",
        ),
        serde_json::Value::Object(_) => errors.add(
            path,
            "must not be an object: no parameter type has an object value.",
        ),
        serde_json::Value::Array(items) => {
            for (k, item) in items.iter().enumerate() {
                check_no_object_or_null(item, &path_index(path, k), errors);
            }
        }
        _ => {}
    }
}

/// Check a literal `default` or entry value against its definition: the
/// form a `<JobParameterDefinition>` `default` of the same type accepts,
/// and then the definition's constraints (§4.5 point 1; §3.2 constraint 6).
///
/// The form check decodes the literal with exactly the decoder the Job
/// Parameter type's `default` field uses, so a STRING value must be a
/// string, an INT value an integer or `<intstring>`, and so on. A list
/// literal's elements are literals: an element containing `{{` is an
/// error (prototype plan A14). Returns the value in Job Parameter form
/// when every check passes, and `None` after reporting an error.
pub(crate) fn check_literal_value(
    def: &EnvironmentParameterDefinition,
    lit: &serde_json::Value,
    path: &[PathElement],
    errors: &mut ValidationErrors,
) -> Option<openjd_expr::ExprValue> {
    match convert_literal_value(def, lit, path, errors) {
        (Some(value), true) => Some(value),
        _ => None,
    }
}

/// [`check_literal_value`], returning the converted value separately from
/// whether it met the definition's constraints: `(Some(value), false)` for
/// a literal of the parameter's type that breaks a constraint (reported),
/// and `(None, false)` for one that does not convert (reported). Job
/// creation needs the converted value either way, since it is known to an
/// Environment's `enableIf` whether or not it meets its constraints
/// (§3.2.2).
pub(crate) fn convert_literal_value(
    def: &EnvironmentParameterDefinition,
    lit: &serde_json::Value,
    path: &[PathElement],
    errors: &mut ValidationErrors,
) -> (Option<openjd_expr::ExprValue>, bool) {
    let pt = def.job_param_type();
    let before = errors.len();
    check_no_object_or_null(lit, path, errors);
    if errors.len() > before {
        return (None, false);
    }
    if let serde_json::Value::Array(items) = lit {
        for (k, item) in items.iter().enumerate() {
            if item.as_str().is_some_and(|s| s.contains("{{")) {
                errors.add(
                    &path_index(path, k),
                    "the elements of a list literal are literals, not format strings. Write the \
                     whole value as one '{{ }}' expression, for example \
                     '{{ [RawParam.Dir] }}'.",
                );
            }
        }
        if errors.len() > before {
            return (None, false);
        }
    }
    // Decode with the Job Parameter `default` decoder, then coerce its
    // canonical form, so a literal the Job Parameter `default` accepts
    // (for example the `<intstring>` " 3 ", which FlexInt trims) is
    // accepted here too.
    let canonical = match decode_literal_form(pt, lit) {
        Ok(v) => v,
        Err(e) => {
            errors.add(
                path,
                format!("is not a valid {} value: {e}", pt.as_spec_str()),
            );
            return (None, false);
        }
    };
    let value =
        match crate::job::create_job::parameters::coerce_json_to_job_parameter_type(&canonical, pt)
        {
            Ok(v) => v,
            Err(e) => {
                errors.add(
                    path,
                    format!("is not a valid {} value: {e}", pt.as_spec_str()),
                );
                return (None, false);
            }
        };
    // A RANGE_EXPR literal is checked as written, which checks both its
    // string and its canonical form (§4.5 item 5, §2.10).
    let written = match (pt, lit.as_str()) {
        (JobParameterType::RangeExpr, Some(s)) => {
            Some(openjd_expr::ExprValue::String(s.to_string()))
        }
        _ => None,
    };
    if let Err(e) = def
        .definition
        .check_constraints(written.as_ref().unwrap_or(&value))
    {
        errors.add(path, e);
        return (Some(value), false);
    }
    (Some(value), true)
}

/// Decode `lit` with the decoder of the Job Parameter `default` field of
/// type `pt`, and return the decoded value in canonical JSON form: an
/// integer for INT, a number or the trimmed `<floatstring>` for FLOAT, a
/// bool for BOOL, and lists of those.
pub(crate) fn decode_literal_form(
    pt: JobParameterType,
    lit: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    use serde_json::Value;
    fn decode<T: serde::de::DeserializeOwned>(lit: &Value) -> Result<T, String> {
        serde_json::from_value::<T>(lit.clone()).map_err(|e| e.to_string())
    }
    fn int(v: &FlexInt) -> Value {
        Value::from(v.0)
    }
    fn float(v: &FlexFloat) -> Value {
        match &v.1 {
            Some(text) => Value::String(text.clone()),
            None => serde_json::Number::from_f64(v.0).map_or(Value::Null, Value::Number),
        }
    }
    let list = |items: Vec<Value>| Value::Array(items);
    Ok(match pt {
        JobParameterType::String | JobParameterType::Path | JobParameterType::RangeExpr => {
            Value::String(decode::<String>(lit)?)
        }
        JobParameterType::Int => int(&decode::<FlexInt>(lit)?),
        JobParameterType::Float => float(&decode::<FlexFloat>(lit)?),
        JobParameterType::Bool => Value::Bool(decode::<BoolValue>(lit)?.0),
        JobParameterType::ListString | JobParameterType::ListPath => list(
            decode::<Vec<String>>(lit)?
                .into_iter()
                .map(Value::String)
                .collect(),
        ),
        JobParameterType::ListInt => list(decode::<Vec<FlexInt>>(lit)?.iter().map(int).collect()),
        JobParameterType::ListFloat => {
            list(decode::<Vec<FlexFloat>>(lit)?.iter().map(float).collect())
        }
        JobParameterType::ListBool => list(
            decode::<Vec<BoolValue>>(lit)?
                .into_iter()
                .map(|b| Value::Bool(b.0))
                .collect(),
        ),
        JobParameterType::ListListInt => list(
            decode::<Vec<Vec<FlexInt>>>(lit)?
                .iter()
                .map(|inner| list(inner.iter().map(int).collect()))
                .collect(),
        ),
    })
}

/// `<Identifier>` (§7.1): `[A-Za-z_][A-Za-z0-9_]*`.
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(n: usize, len: usize) -> Vec<String> {
        (0..n).map(|i| format!("{i:0>len$}")).collect()
    }

    #[test]
    fn capped_name_list_stops_at_512_characters_or_50_names() {
        // 255 + ", " + 255 = 512 characters: both fit.
        let a = "a".repeat(255);
        let b = "b".repeat(255);
        assert_eq!(
            capped_name_list([a.as_str(), b.as_str()].into_iter(), 2),
            format!("{a}, {b}")
        );
        // One more character does not fit.
        let b = "b".repeat(256);
        assert_eq!(
            capped_name_list([a.as_str(), b.as_str()].into_iter(), 2),
            format!("{a}, and 1 more")
        );
        // The first name is listed however long it is.
        let long = "x".repeat(600);
        assert_eq!(
            capped_name_list([long.as_str()].into_iter(), 3),
            format!("{long}, and 2 more")
        );
        // At most 50 names.
        let short = names(51, 2);
        let list = capped_name_list(short.iter().map(String::as_str), 51);
        assert_eq!(list, format!("{}, and 1 more", short[..50].join(", ")));
        assert_eq!(
            capped_name_list(short[..50].iter().map(String::as_str), 50),
            short[..50].join(", ")
        );
    }

    #[test]
    fn missing_parameters_list_counts_every_character_up_to_512() {
        let msg = |pairs: &[(&str, &str)], total: usize| {
            missing_parameters_message(
                pairs.iter().copied(),
                total,
                "X",
                &HashSet::new(),
                &HashSet::new(),
                &HashSet::new(),
            )
        };
        let prefix = "must supply, in an 'environment:' dependency, every parameter without a \
                      default of each Environment its Sessions enter (X); missing: ";
        // One group: "'n1', 'n2' of Environment 'E'" is 243 + 244 + 25 = 512
        // characters, so both names fit; one more character does not.
        let (n1, n2) = ("p".repeat(243), "q".repeat(244));
        let list = format!("'{n1}', '{n2}' of Environment 'E'");
        assert_eq!(list.len(), 512);
        assert_eq!(
            msg(&[("'E'", &n1), ("'E'", &n2)], 2),
            format!("{prefix}{list}.")
        );
        let n2 = "q".repeat(245);
        assert_eq!(
            msg(&[("'E'", &n1), ("'E'", &n2)], 2),
            format!("{prefix}'{n1}' of Environment 'E'; and 1 more.")
        );
        // Two groups: the "; " between them counts too (234 + 234 + 44 = 512).
        let (x, y) = ("x".repeat(234), "y".repeat(234));
        let list = format!("'{x}' of Environment 'A'; '{y}' of Environment 'B'");
        assert_eq!(list.len(), 512);
        assert_eq!(
            msg(&[("'A'", &x), ("'B'", &y)], 2),
            format!("{prefix}{list}.")
        );
        let y = "y".repeat(235);
        assert_eq!(
            msg(&[("'A'", &x), ("'B'", &y)], 2),
            format!("{prefix}'{x}' of Environment 'A'; and 1 more.")
        );
        // A disabled entry's note counts too: "'n1', 'n2' of Environment
        // 'E'<note>" is 512 bytes, so both names fit; one more does not.
        let disabled: HashSet<&str> = ["'E'"].into_iter().collect();
        let noted = |pairs: &[(&str, &str)], total: usize| {
            missing_parameters_message(
                pairs.iter().copied(),
                total,
                "X",
                &disabled,
                &HashSet::new(),
                &HashSet::new(),
            )
        };
        let room = 512 - DISABLED_NOTE.len() - "'', '' of Environment 'E'".len();
        let (n1, n2) = ("p".repeat(room / 2), "q".repeat(room - room / 2));
        let list = format!("'{n1}', '{n2}' of Environment 'E'{DISABLED_NOTE}");
        assert_eq!(list.len(), 512);
        assert_eq!(
            noted(&[("'E'", &n1), ("'E'", &n2)], 2),
            format!("{prefix}{list}.")
        );
        let n2 = format!("{n2}q");
        assert_eq!(
            noted(&[("'E'", &n1), ("'E'", &n2)], 2),
            format!("{prefix}'{n1}' of Environment 'E'{DISABLED_NOTE}; and 1 more.")
        );
        // Only the disabled Environment's group gets the note.
        assert_eq!(
            noted(&[("'A'", "a"), ("'E'", "e")], 2),
            format!("{prefix}'a' of Environment 'A'; 'e' of Environment 'E'{DISABLED_NOTE}.")
        );
        // The unresolved note counts too, alone and after the disabled
        // note: both names fit in exactly 512 bytes, one more does not.
        let unresolved: HashSet<&str> = ["'E'"].into_iter().collect();
        for (disabled, notes) in [
            (HashSet::new(), UNRESOLVED_NOTE.to_string()),
            (
                disabled.clone(),
                format!("{DISABLED_NOTE}{UNRESOLVED_NOTE}"),
            ),
        ] {
            let with = |pairs: &[(&str, &str)], total: usize| {
                missing_parameters_message(
                    pairs.iter().copied(),
                    total,
                    "X",
                    &disabled,
                    &unresolved,
                    &HashSet::new(),
                )
            };
            let room = 512 - notes.len() - "'', '' of Environment 'E'".len();
            let (n1, n2) = ("p".repeat(room / 2), "q".repeat(room - room / 2));
            let list = format!("'{n1}', '{n2}' of Environment 'E'{notes}");
            assert_eq!(list.len(), 512);
            assert_eq!(
                with(&[("'E'", &n1), ("'E'", &n2)], 2),
                format!("{prefix}{list}.")
            );
            let n2 = format!("{n2}q");
            assert_eq!(
                with(&[("'E'", &n1), ("'E'", &n2)], 2),
                format!("{prefix}'{n1}' of Environment 'E'{notes}; and 1 more.")
            );
        }
        // The first name is listed however long it is.
        let long = "z".repeat(600);
        assert_eq!(
            msg(&[("'E'", &long)], 1),
            format!("{prefix}'{long}' of Environment 'E'.")
        );
        // At most 50 names, across groups.
        let many = names(60, 2);
        let pairs: Vec<(&str, &str)> = many
            .iter()
            .enumerate()
            .map(|(i, n)| (if i < 30 { "'A'" } else { "'B'" }, n.as_str()))
            .collect();
        let quoted = |r: std::ops::Range<usize>| {
            many[r]
                .iter()
                .map(|n| format!("'{n}'"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        assert_eq!(
            msg(&pairs, 60),
            format!(
                "{prefix}{} of Environment 'A'; {} of Environment 'B'; and 10 more.",
                quoted(0..30),
                quoted(30..50)
            )
        );
    }

    #[test]
    fn identifier_pattern() {
        assert!(is_identifier("Packages"));
        assert!(is_identifier("_x9"));
        assert!(!is_identifier(""));
        assert!(!is_identifier("9x"));
        assert!(!is_identifier("a-b"));
        assert!(!is_identifier("ü"));
    }
}
