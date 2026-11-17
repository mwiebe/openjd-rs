// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Pass 12: `SERVICE` — validate or reject (RFC 0009, Template Schemas §9).
//!
//! Six fields are gated by the `SERVICE` extension:
//! - `services` on the job template root (§1.1 item 8)
//! - `externalServices` on the job template root (§1.1 item 9)
//! - `services` on the environment template root (§1.2)
//! - `dependencies` on `<Environment>` (§4 item 3)
//! - `service` on `<StepDependency>` (§3.2 item 2)
//!
//! (The `onWrapService*` hooks on `<EnvironmentActions>`, gated on both
//! `WRAP_ACTIONS` and `SERVICE`, are handled by pass 10 with the other wrap
//! hooks.)
//!
//! When the extension is not enabled, using any of these fields is a
//! validation error and nothing inside the lists is examined
//! ([`gate_services_job_template`] / [`gate_services_environment_template`],
//! run ahead of every other pass so these errors lead the list; what follows
//! from the missing extension — a `Service.*` reference to a Service the
//! gated lists name — is left unreported as a consequence). When it is
//! enabled, this pass additionally enforces:
//!
//! - **Prerequisites.** A template that lists `SERVICE` in
//!   `extensions:` must also list `EXPR` and `ENV_PARAMS` (RFC 0009
//!   "Backward compatibility"; [`check_prerequisites`], run before any
//!   pass).
//! - **List shape.** Each `services` / `externalServices` list has 1–10
//!   elements (§1.1 items 8–9, §1.2).
//! - **Name uniqueness.** Service names are unique within a list,
//!   requirement names are unique within theirs, and no requirement bears
//!   the name of an inline Service (§9.2, §1.1 item 9.4).
//! - **`service: <name>`** (§3.2 constraint 1): on a Step or a
//!   Service, the name is a Service of the document's `services` or, in a
//!   Job Template, of its `externalServices`. (Step targets, self
//!   dependency, duplicates and the one-of rule are pass 6's for a Step; a
//!   Service's `dependencies` are checked wholly here: at least one
//!   element, each entry exactly one of `dependsOn` / `step` / `service` /
//!   `environment`, naming a Step of the Job Template or a Service other
//!   than itself, no duplicates, and in an Environment Template only the
//!   `service` key. A Service's `environment:` entries are pass 11's, as a
//!   Step's are.) A
//!   `service` entry that names no Service but a Step of that name gets `;
//!   did you mean 'dependsOn: <name>'?`, and a `dependsOn` that names no
//!   Step but a Service of that name the mirror hint
//!   ([`super::helpers::dependency_kind_hint`]).
//! - **One acyclic graph.** The `dependencies` of the document's Steps,
//!   Services, and Job Environments form one graph with every kind of
//!   edge — each Step and each Service whose Sessions enter a Job
//!   Environment gains an edge to every Service it lists — which must be
//!   acyclic (§3.2 constraint 3). Template validation reports a cycle all
//!   of whose edges are known (every `enableIf` they depend on known by
//!   §7.6 rule 5, and no Environment in an `exclusiveGroup`); job creation
//!   checks the rest. The error names the
//!   cycle and, when entering a Job Environment closed it, the
//!   Environment. See [`crate::template::service_scope`].
//! - **Scope** (§9.1): every Service of a Job Template has a
//!   non-empty scope — some Step lists it, directly or through other
//!   Services, or enters a Job Environment that lists it — else it is
//!   unused and rejected by name; a Service listed only by unused Services
//!   is unused, and the message names them. Where the scope depends on an
//!   `enableIf` not known (§7.6 rule 5), only a Service that no Step can be
//!   in the scope of is rejected here; job creation decides the rest.
//! - **`<Environment>.dependencies`** (§4 item 3, §3.2 constraint 5):
//!   permitted only on a `jobEnvironments` entry and on an
//!   Environment Template's `environment`, never on a `stepEnvironments`
//!   entry; at least one element; every entry with the `service` key (a
//!   `dependsOn` entry is rejected — an Environment is entered by Sessions,
//!   not scheduled) naming a Service of the document's `services` or, in a
//!   Job Template, of its `externalServices`; and no Service listed twice.
//! - **`<Service>` structure** (§9–§9.7): identifier names that are not
//!   `File`; 1–10 uniquely named ports; literal `port` in 1–65535; the
//!   literal `<ServiceHealthCheck>` numeric fields
//!   (`readinessIntervalSeconds`, `readinessTimeoutSeconds`,
//!   `healthIntervalSeconds`, `failureThreshold`) positive; literal
//!   `maxRestarts` non-negative, and a literal `maxRestarts` greater than 0
//!   accompanied by `completedTasks` (§9.5 item 2 — a
//!   format-string `maxRestarts` is checked at job creation);
//!   `onHealthCheck` defined iff the health
//!   check type is `COMMAND`; every port a `TCP_CONNECT` check names is
//!   declared and has `protocol: TCP`; a Service none of whose ports is TCP
//!   declares a `STDOUT` or `COMMAND` check; a `STDOUT` check gives
//!   `failureThreshold` only together with `healthIntervalSeconds` (§9 item
//!   7, §9.4 items 3 and 6 — a `STDOUT` check's
//!   `readinessIntervalSeconds` is rejected as an unknown field at decode,
//!   as `ports` is on anything but `TCP_CONNECT`); no two ports of the same
//!   `protocol` have the same literal `port` number (§9 item 6.4 —
//!   format-string numbers are checked at job creation).
//!   `description`, `variables`, `hostRequirements`, embedded files and
//!   every `<Action>` reuse the pass-6 validators.
//! - **`<ServiceRequirement>` structure** (§9.8): an identifier name that is
//!   not `File`; 1–10 uniquely named ports, each an identifier other than
//!   `File`.
//!
//! The numeric `@fmtstring` fields are modeled like `<Action>.timeout`: a
//! value without an expression is checked here, and a format string is
//! resolved and range-checked at job creation. Format-string *scopes*
//! (`Service.*`, §9 visibility rule, §9 job-creation exclusion) and `let` bindings are validated by the
//! format-string pass, not here.

use std::collections::HashSet;

use super::helpers::dependency_kind_hint;
use super::structure::{
    validate_action, validate_description, validate_embedded_files,
    validate_host_requirements_in_context, validate_variables,
};
use super::{EffectiveLimits, EffectiveRules};
use crate::error::{path_field, path_index, PathElement, ValidationErrors};
use crate::template::service_scope::{environment_template_service_cycle, ComputedServiceScope};
use crate::template::*;
use crate::types::{ModelExtension, ValidationContext};

/// §1.1 items 8–9 / §1.2 / §9 item 6 / §9.8 item 2: maximum elements in a
/// Service list, a requirement list, and a `ports` list.
const MAX_SERVICES: usize = 10;
const MAX_PORTS: usize = 10;

/// §9.2 / §9.3: the name reserved for `Service.File.*` references.
const RESERVED_FILE_NAME: &str = "File";

/// Validate RFC 0009 constraints for a job template that declares
/// `SERVICE`: every Service and requirement, every `service` dependency,
/// the combined dependency graph and the Services' scopes, and every
/// Environment's `dependencies`. (The prerequisites are
/// [`check_prerequisites`]'s and the gated fields
/// [`gate_services_job_template`]'s, both ahead of every other pass.)
pub fn validate_services_job_template(
    jt: &JobTemplate,
    limits: &EffectiveLimits,
    rules: &EffectiveRules,
    ctx: &ValidationContext,
    known_enable_ifs: &super::format_strings::KnownEnableIfs,
    errors: &mut ValidationErrors,
) {
    if !ctx.profile.has_extension(ModelExtension::Service) {
        // The gated fields were reported by `gate_services_job_template`,
        // ahead of every other pass.
        return;
    }

    let mut service_names: HashSet<&str> = HashSet::new();
    if let Some(services) = &jt.services {
        let list_path = path_field(&[], "services");
        validate_service_list(services, &list_path, limits, rules, ctx, errors);
        check_environment_entry_limit(services, &list_path, ctx, errors);
        service_names.extend(services.iter().map(|s| s.name.as_str()));
    }
    let env_params = ctx.profile.has_extension(ModelExtension::EnvParams);
    validate_job_template_dependencies(jt, env_params, known_enable_ifs, errors);

    if let Some(requirements) = &jt.external_services {
        let list_path = path_field(&[], "externalServices");
        validate_requirement_list(requirements, &list_path, &service_names, limits, errors);
    }

    // §4 item 3: `dependencies` on every Environment.
    let known = EnvironmentDependencyTargets {
        services: &service_names,
        required: &jt
            .external_services()
            .iter()
            .map(|r| r.name.as_str())
            .collect(),
        where_: "no Service of that name in services or externalServices",
    };
    if let Some(envs) = &jt.job_environments {
        let envs_path = path_field(&[], "jobEnvironments");
        for (i, env) in envs.iter().enumerate() {
            let env_path = path_index(&envs_path, i);
            validate_environment_dependencies(
                env,
                &env_path,
                EnvironmentKind::Job,
                &known,
                env_params,
                errors,
            );
        }
    }
    for (i, step) in jt.steps.iter().enumerate() {
        let Some(envs) = &step.step_environments else {
            continue;
        };
        let envs_path = path_field(
            &[PathElement::Field("steps".into()), PathElement::Index(i)],
            "stepEnvironments",
        );
        for (j, env) in envs.iter().enumerate() {
            let env_path = path_index(&envs_path, j);
            validate_environment_dependencies(
                env,
                &env_path,
                EnvironmentKind::Step,
                &known,
                env_params,
                errors,
            );
        }
    }
}

/// Report the `SERVICE`-gated fields of a job template that does not
/// declare the extension: `services`, `externalServices`, the `service` key
/// of every Step's `dependencies` entries, and every Environment's
/// `dependencies`. Nothing inside the lists is examined.
/// Runs ahead of every other pass so that these errors come first: an
/// `Undefined variable: 'Service.X.p.port'` that follows from the missing
/// extension is a consequence, and pass 8 leaves it unreported (see
/// [`super::service_diagnostics`]). Does nothing when the extension is
/// declared.
pub fn gate_services_job_template(
    jt: &JobTemplate,
    ctx: &ValidationContext,
    errors: &mut ValidationErrors,
) {
    if ctx.profile.has_extension(ModelExtension::Service) {
        return;
    }
    if jt.services.is_some() {
        errors.add(
            &path_field(&[], "services"),
            "services requires the SERVICE extension.",
        );
    }
    if jt.external_services.is_some() {
        errors.add(
            &path_field(&[], "externalServices"),
            "externalServices requires the SERVICE extension.",
        );
    }
    if let Some(envs) = &jt.job_environments {
        let envs_path = path_field(&[], "jobEnvironments");
        for (i, env) in envs.iter().enumerate() {
            gate_environment_fields(env, &path_index(&envs_path, i), errors);
        }
    }
    for (i, step) in jt.steps.iter().enumerate() {
        let step_path = [PathElement::Field("steps".into()), PathElement::Index(i)];
        gate_service_dependencies(
            step.dependencies.as_deref(),
            &path_field(&step_path, "dependencies"),
            errors,
        );
        let Some(envs) = &step.step_environments else {
            continue;
        };
        let envs_path = path_field(&step_path, "stepEnvironments");
        for (j, env) in envs.iter().enumerate() {
            gate_environment_fields(env, &path_index(&envs_path, j), errors);
        }
    }
}

/// §3.2 item 2 without `SERVICE`: the `service` key of a `<StepDependency>`
/// is an unknown field, reported on the entry's `service` path.
fn gate_service_dependencies(
    deps: Option<&[StepDependency]>,
    deps_path: &[PathElement],
    errors: &mut ValidationErrors,
) {
    for (j, dep) in deps.into_iter().flatten().enumerate() {
        if dep.service.is_some() {
            errors.add(
                &path_field(&path_index(deps_path, j), "service"),
                "service requires the SERVICE extension.",
            );
        }
    }
}

/// As [`gate_services_job_template`], for an environment template:
/// `services` and the Environment's `dependencies`.
pub fn gate_services_environment_template(
    et: &EnvironmentTemplate,
    ctx: &ValidationContext,
    errors: &mut ValidationErrors,
) {
    if ctx.profile.has_extension(ModelExtension::Service) {
        return;
    }
    if et.services.is_some() {
        errors.add(
            &path_field(&[], "services"),
            "services requires the SERVICE extension.",
        );
    }
    if let Some(env) = &et.environment {
        gate_environment_fields(env, &path_field(&[], "environment"), errors);
    }
}

/// §4 item 3 without `SERVICE`: `dependencies` on an Environment is
/// rejected outright.
fn gate_environment_fields(
    env: &Environment,
    env_path: &[PathElement],
    errors: &mut ValidationErrors,
) {
    if env.dependencies.is_some() {
        errors.add(
            &path_field(env_path, "dependencies"),
            "dependencies requires the SERVICE extension.",
        );
    }
}

/// Where an Environment's `dependencies` entry may point (§4 item 3
/// constraint 2): the document's `services` and, in a Job Template, its
/// `externalServices`; `where_` says which lists were searched.
struct EnvironmentDependencyTargets<'a> {
    services: &'a HashSet<&'a str>,
    required: &'a HashSet<&'a str>,
    where_: &'static str,
}

impl EnvironmentDependencyTargets<'_> {
    /// Every Service name an entry could have meant.
    fn all(&self) -> impl Iterator<Item = &str> {
        self.services.iter().chain(self.required.iter()).copied()
    }
}

/// Which list an Environment is an entry of, for §4 item 3 constraint 4.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EnvironmentKind {
    /// A `jobEnvironments` entry, or an Environment Template's
    /// `environment`: may give `dependencies`.
    Job,
    /// A `stepEnvironments` entry: follows its Step's `dependencies` and may
    /// not give a list of its own.
    Step,
}

/// §4 item 3 / §3.2 constraint 5: validate one Environment's
/// `dependencies` at `env_path`, with `SERVICE` declared. A Step
/// Environment may not give the list at all; else the list is non-empty,
/// every entry uses the `service` key (a `dependsOn` entry is rejected — an
/// Environment is entered, not scheduled; a malformed entry is rejected as
/// such) and names a Service in `known`, and no Service is listed twice.
fn validate_environment_dependencies(
    env: &Environment,
    env_path: &[PathElement],
    kind: EnvironmentKind,
    known: &EnvironmentDependencyTargets<'_>,
    env_params: bool,
    errors: &mut ValidationErrors,
) {
    let Some(deps) = &env.dependencies else {
        return;
    };
    let deps_path = path_field(env_path, "dependencies");
    if kind == EnvironmentKind::Step {
        errors.add(
            &deps_path,
            "a Step Environment follows its Step's dependencies and must not give a dependencies \
             list of its own (Template Schemas §4 item 3 constraint 4).",
        );
        return;
    }
    if deps.is_empty() {
        errors.add(&deps_path, "must not be empty.");
    }
    let mut seen: HashSet<DependencyTarget<'_>> = HashSet::new();
    for (j, dep) in deps.iter().enumerate() {
        let dep_path = path_index(&deps_path, j);
        check_non_step_list_entry(dep, &dep_path, ListOwner::Environment, env_params, errors);
        match dep.target() {
            None => errors.add(&dep_path, malformed_service_owned_dependency(dep)),
            Some(target @ DependencyTarget::Step(..)) => errors.add(
                &dep_path,
                format!(
                    "dependency '{target}' names a Step, but an Environment is entered \
                     by Sessions, not scheduled; an Environment may depend only on a Service, as \
                     'service: <name>'{}",
                    dependency_kind_hint(target, known.all())
                ),
            ),
            // Reported by `check_non_step_list_entry`.
            Some(DependencyTarget::Environment(_)) => {}
            Some(DependencyTarget::Service(name)) => {
                if !known.services.contains(name) && !known.required.contains(name) {
                    errors.add(
                        &dep_path,
                        unknown_service_dependency(name, known.where_, std::iter::empty()),
                    );
                }
            }
        }
        if dep.target().is_some_and(|t| !seen.insert(dedup_key(t))) {
            errors.add(&dep_path, duplicate_dependency(dep));
        }
    }
}

/// The key a dependency target is compared under for §3.2 constraint 2
/// (no target listed twice): a `step:` entry and a `dependsOn:` entry
/// naming one Step are the same target.
pub(super) fn dedup_key(target: DependencyTarget<'_>) -> DependencyTarget<'_> {
    match target {
        DependencyTarget::Step(name, _) => DependencyTarget::Step(name, StepKey::DependsOn),
        other => other,
    }
}

/// Where a `dependencies` list other than a Step's sits, for the rules on
/// its `environment:` entries (RFC 0009 `<StepDependency>` item 3).
#[derive(Clone, Copy, PartialEq, Eq)]
enum ListOwner {
    /// A `<Service>` of a Job Template's `services`: may request
    /// Environments as a Step does (pass 11 checks the entries).
    JobTemplateService,
    /// A `<Service>` of an Environment Template's `services`, whose author
    /// writes the document's `environment` too: it may have only disabled
    /// `environment:` entries (§1.2 *services* constraint 5), which
    /// [`check_environment_template_service_entry`] checks.
    EnvironmentTemplateService,
    /// An `<Environment>`, which does not request another Environment.
    Environment,
}

/// The rules for an entry of a `dependencies` list other than a Step's: a
/// Service's or an Environment's (`SERVICE` lists). The `step:` synonym of
/// `dependsOn:` is read the same as in a Step's list and needs
/// `ENV_PARAMS`. The `environment:` form and its `required`, `enableIf`,
/// and `parameters` keys are permitted in a Step's list and in a Job
/// Template Service's (§3.2; RFC 0009 `<StepDependency>` item 3), whose
/// entries pass 11 checks, so they are rejected here for an Environment.
/// An Environment Template's Service may have only disabled ones (§1.2
/// *services* constraint 5), which
/// [`check_environment_template_service_entry`] checks.
fn check_non_step_list_entry(
    dep: &StepDependency,
    dep_path: &[PathElement],
    owner: ListOwner,
    env_params: bool,
    errors: &mut ValidationErrors,
) {
    if dep.step.is_some() && !env_params {
        errors.add(
            dep_path,
            "the 'step' form of a dependency requires the ENV_PARAMS extension.",
        );
    }
    let why = match owner {
        ListOwner::JobTemplateService => return,
        ListOwner::EnvironmentTemplateService
            if matches!(dep.target(), Some(DependencyTarget::Environment(_))) =>
        {
            return;
        }
        ListOwner::EnvironmentTemplateService | ListOwner::Environment => {
            "an Environment does not request another Environment (RFC 0009 <StepDependency> \
             item 3)"
        }
    };
    if let Some(DependencyTarget::Environment(name)) = dep.target() {
        errors.add(
            dep_path,
            format!("dependency 'environment: {name}' names an Environment; {why}."),
        );
        // The entry's other keys are part of the same mistake.
        return;
    }
    for (key, present) in [
        ("required", dep.required.is_some()),
        ("enableIf", dep.enable_if.is_some()),
        ("parameters", dep.parameters.is_some()),
    ] {
        if present {
            errors.add(
                &path_field(dep_path, key),
                format!(
                    "'{key}' is permitted only in an 'environment' entry of the dependencies of a \
                     Step or of a Service."
                ),
            );
        }
    }
}

/// §3.2: the message for an entry that gives both keys or neither, in a
/// document without `ENV_PARAMS` (so without `SERVICE` either: an entry
/// that gives a `service` key there is also reported by the SERVICE gate).
pub(super) fn malformed_dependency(dep: &StepDependency) -> String {
    let keys = [&dep.depends_on, &dep.step, &dep.service, &dep.environment]
        .into_iter()
        .filter(|k| k.is_some())
        .count();
    match keys {
        2.. => format!(
            "a dependency names a Step with dependsOn or a Service with service, not both: \
             {{{}}} (Template Schemas §3.2).",
            dep.describe()
        ),
        _ => "a dependency must name a Step with dependsOn or a Service with service; this \
              entry gives neither (Template Schemas §3.2)."
            .to_string(),
    }
}

/// RFC 0009 `<StepDependency>` constraint 4, for an entry of a Service's
/// or an Environment's `dependencies`, which exist only under `SERVICE` and
/// so always with `ENV_PARAMS`: the same message a Step's entry gets
/// (`validate_dependency_form` in `structure`), naming every form.
pub(super) fn malformed_service_owned_dependency(dep: &StepDependency) -> String {
    let present: Vec<&str> = [
        ("dependsOn", dep.depends_on.is_some()),
        ("step", dep.step.is_some()),
        ("service", dep.service.is_some()),
        ("environment", dep.environment.is_some()),
    ]
    .into_iter()
    .filter_map(|(key, is)| is.then_some(key))
    .collect();
    let all_forms = "must have exactly one of 'dependsOn', 'step', 'service', or 'environment'";
    match present.as_slice() {
        [a, b] => format!("{all_forms}, not both '{a}' and '{b}'."),
        [_, _, _] => format!("{all_forms}, not all three."),
        [_, _, _, _] => format!("{all_forms}, not all four."),
        _ => format!("{all_forms}."),
    }
}

/// §3.2 constraint 2: `duplicate dependency 'A'.` for a Step named twice,
/// `duplicate dependency 'service: X'.` for a Service.
pub(super) fn duplicate_dependency(dep: &StepDependency) -> String {
    match dep.target() {
        Some(DependencyTarget::Step(name, _)) => format!("duplicate dependency '{name}'."),
        _ => format!("duplicate dependency '{}'.", dep.describe()),
    }
}

/// `dependency 'service: X' not found: …` — a `service` entry names no
/// Service. `where_` says which lists were searched; `step_names` are the
/// Steps of the template: when one of that name exists the message is
/// instead `dependency 'X' names no Service; did you mean 'dependsOn: X'?`
/// ([`dependency_kind_hint`]).
fn unknown_service_dependency<'a>(
    name: &str,
    where_: &str,
    step_names: impl IntoIterator<Item = &'a str>,
) -> String {
    let hint = dependency_kind_hint(DependencyTarget::Service(name), step_names);
    if hint == "." {
        format!("dependency 'service: {name}' not found: {where_}.")
    } else {
        format!("dependency '{name}' names no Service{hint}")
    }
}

/// §3.2 constraints 1–3 / §9 item 4 / §3.2 constraint 3, §9.1 rule 4 for a Job Template
/// with `SERVICE`: every `service` entry of a Step's `dependencies` names a
/// Service of `services` or `externalServices`; each Service's
/// `dependencies` is non-empty, every entry exactly one of `dependsOn` /
/// `service`, names Steps of the template or Services other than itself,
/// and lists no target twice; the combined graph — including the edge from
/// every Step to each Service a Job Environment lists — is acyclic (the
/// error names the cycle); and every Service, and every `externalServices`
/// entry, has a non-empty scope. (An Environment's `dependencies` are
/// [`validate_environment_dependencies`]'s.)
fn validate_job_template_dependencies(
    jt: &JobTemplate,
    env_params: bool,
    known_enable_ifs: &super::format_strings::KnownEnableIfs,
    errors: &mut ValidationErrors,
) {
    let step_names: HashSet<&str> = jt.steps.iter().map(|s| s.name.as_str()).collect();
    let service_names: HashSet<&str> = jt.services().iter().map(|s| s.name.as_str()).collect();
    let required_names: HashSet<&str> = jt
        .external_services()
        .iter()
        .map(|r| r.name.as_str())
        .collect();
    let where_ = "no Service of that name in services or externalServices";
    let known = || service_names.iter().chain(required_names.iter()).copied();
    let steps = || step_names.iter().copied();

    // Steps: `service` entries (pass 6 did the `dependsOn` entries, the
    // one-of rule and the duplicates).
    for (i, step) in jt.steps.iter().enumerate() {
        let deps_path = path_field(
            &[PathElement::Field("steps".into()), PathElement::Index(i)],
            "dependencies",
        );
        for (j, dep) in step.dependencies.iter().flatten().enumerate() {
            if let Some(name) = dep.service() {
                if !service_names.contains(name) && !required_names.contains(name) {
                    errors.add(
                        &path_index(&deps_path, j),
                        unknown_service_dependency(name, where_, steps()),
                    );
                }
            }
        }
    }

    // Services: the whole list.
    let list_path = path_field(&[], "services");
    for (k, svc) in jt.services().iter().enumerate() {
        let Some(deps) = &svc.dependencies else {
            continue;
        };
        let deps_path = path_field(&path_index(&list_path, k), "dependencies");
        if deps.is_empty() {
            errors.add(&deps_path, "must not be empty.");
        }
        let mut seen: HashSet<DependencyTarget<'_>> = HashSet::new();
        for (j, dep) in deps.iter().enumerate() {
            let dep_path = path_index(&deps_path, j);
            check_non_step_list_entry(
                dep,
                &dep_path,
                ListOwner::JobTemplateService,
                env_params,
                errors,
            );
            match dep.target() {
                None => errors.add(&dep_path, malformed_service_owned_dependency(dep)),
                Some(DependencyTarget::Step(name, _)) => {
                    if !step_names.contains(name) {
                        errors.add(&dep_path, unknown_step_dependency(name, known()));
                    }
                }
                // Pass 11 checks the entry (RFC 0009 `<StepDependency>`
                // item 3).
                Some(DependencyTarget::Environment(_)) => {}
                Some(DependencyTarget::Service(name)) => {
                    if name == svc.name {
                        errors.add(&dep_path, "cannot depend on itself.");
                    } else if !service_names.contains(name) && !required_names.contains(name) {
                        errors.add(&dep_path, unknown_service_dependency(name, where_, steps()));
                    }
                }
            }
            // Pass 11 reports a repeated `environment:` entry (§3.2 constraint 3).
            if dep
                .target()
                .filter(|t| !matches!(t, DependencyTarget::Environment(_)))
                .is_some_and(|t| !seen.insert(dedup_key(t)))
            {
                errors.add(&dep_path, duplicate_dependency(dep));
            }
        }
    }

    // The combined graph (Steps, Services, and the edges a Job
    // Environment's entries add) and the scopes.
    let scopes = match crate::template::service_scope::compute_service_scopes_with(
        jt,
        &crate::template::Entrances::from_known(jt, known_enable_ifs),
    ) {
        Ok(scopes) => scopes,
        Err(cycle) => {
            errors.add(&[], cycle.to_string());
            return;
        }
    };
    for (k, svc) in jt.services().iter().enumerate() {
        let Some(computed) = scopes.get(&svc.name) else {
            continue;
        };
        if computed.is_unused() {
            errors.add(&path_index(&list_path, k), unused_service_message(computed));
        }
    }
    // §9.1 rule 4 for `externalServices` entries: one that no Step, Service,
    // or Job Environment lists confers no scope on the Service it is matched
    // to, and nothing can reference it (§9.8).
    let requirements_path = path_field(&[], "externalServices");
    for (k, req) in jt.external_services().iter().enumerate() {
        if !scopes.requirement_possibly_used(&req.name) {
            // The inline Services that list the entry, which are therefore
            // unused themselves (§9.1 rule 4) — named so the message points
            // at the right fix.
            let listers: Vec<&str> = jt
                .services()
                .iter()
                .filter(|svc| lists_service(svc.dependencies.as_deref(), &req.name))
                .map(|svc| svc.name.as_str())
                .collect();
            errors.add(
                &path_index(&requirements_path, k),
                unused_external_service_message(&req.name, &listers),
            );
        }
    }
}

/// §9.1 rule 4 applied to an `externalServices` entry: the message for an
/// entry in whose scope no Step falls, naming the inline Services that list
/// it when that is why (they are unused too).
fn unused_external_service_message(name: &str, listers: &[&str]) -> String {
    match listers {
        [] => format!(
            "external Service '{name}' is unused: no Step, Service, or Job Environment lists it."
        ),
        [only] => format!(
            "external Service '{name}' is unused: no Step is in its scope (Service '{only}' lists \
             it, but no Step is in {only}'s scope either)."
        ),
        many => {
            let quoted: Vec<String> = many.iter().map(|s| format!("'{s}'")).collect();
            let (init, last) = quoted.split_at(quoted.len() - 1);
            format!(
                "external Service '{name}' is unused: no Step is in its scope (Services {} and {} \
                 list it, but no Step is in their scope either).",
                init.join(", "),
                last[0]
            )
        }
    }
}

/// `dependency 'dependsOn: X' not found…` — a `dependsOn` entry names no
/// Step. `service_names` are the Services the author could have meant,
/// for the `did you mean 'service: X'?` hint when one of that name exists.
pub(super) fn unknown_step_dependency<'a>(
    name: &str,
    service_names: impl IntoIterator<Item = &'a str>,
) -> String {
    let hint = dependency_kind_hint(
        DependencyTarget::Step(name, StepKey::DependsOn),
        service_names,
    );
    if hint == "." {
        format!("dependency '{name}' not found.")
    } else {
        format!("dependency '{name}' names no Step{hint}")
    }
}

/// §9.1 rule 4: the message for a Service in whose scope no
/// Step falls. When only other Services list it, they are named, since
/// they are unused too (a Service listed only by unused Services is
/// unused); otherwise nothing lists it at all.
fn unused_service_message(computed: &ComputedServiceScope) -> String {
    let name = &computed.name;
    // Listed by Job Environments that no Step's Sessions enter (rule 3: an
    // `ON_REQUEST` Environment no Step requests, or one every Step's
    // `enableIf` excludes).
    if !computed.listing_environments.is_empty() {
        let envs: Vec<String> = computed
            .listing_environments
            .iter()
            .map(|e| format!("'{e}'"))
            .collect();
        let (noun, verb) = if envs.len() == 1 {
            ("Job Environment", "lists")
        } else {
            ("Job Environments", "list")
        };
        let mut why = format!(
            "{noun} {} {verb} it, but no Step's Sessions enter {}",
            join_quoted(&envs),
            if envs.len() == 1 { "it" } else { "them" }
        );
        if !computed.dependent_services.is_empty() {
            let svcs: Vec<String> = computed
                .dependent_services
                .iter()
                .map(|s| format!("'{s}'"))
                .collect();
            why.push_str(&format!(
                ", and no Step is in the scope of Service{} {}",
                if svcs.len() == 1 { "" } else { "s" },
                join_quoted(&svcs)
            ));
        }
        return format!("Service '{name}' is unused: no Step is in its scope ({why}).");
    }
    match computed.dependent_services.as_slice() {
        [] => format!("Service '{name}' is unused: no Step, Service, or Job Environment lists it."),
        [only] => format!(
            "Service '{name}' is unused: no Step is in its scope (Service '{only}' lists it, but \
             no Step is in {only}'s scope either)."
        ),
        many => {
            let quoted: Vec<String> = many.iter().map(|s| format!("'{s}'")).collect();
            let (init, last) = quoted.split_at(quoted.len() - 1);
            format!(
                "Service '{name}' is unused: no Step is in its scope (Services {} and {} list it, \
                 but no Step is in their scope either).",
                init.join(", "),
                last[0]
            )
        }
    }
}

/// `'a'`, `'a' and 'b'`, or `'a', 'b', and 'c'`, for already-quoted names.
fn join_quoted(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [only] => only.clone(),
        [a, b] => format!("{a} and {b}"),
        [init @ .., last] => format!("{}, and {last}", init.join(", ")),
    }
}

/// Validate RFC 0009 constraints for an environment template: the
/// `services` list (§1.2, gated and validated exactly like
/// a Job Template's, except that a Service's `dependencies` may use only the
/// `service` key and name a Service of this list, since the document has
/// no Steps, and `externalServices` has no place here), and the
/// Environment's `dependencies` (which name Services of this document's
/// `services`).
pub fn validate_services_environment_template(
    et: &EnvironmentTemplate,
    limits: &EffectiveLimits,
    rules: &EffectiveRules,
    ctx: &ValidationContext,
    errors: &mut ValidationErrors,
) {
    if !ctx.profile.has_extension(ModelExtension::Service) {
        // The gated fields were reported by
        // `gate_services_environment_template`, ahead of every other pass.
        return;
    }

    let env_params = ctx.profile.has_extension(ModelExtension::EnvParams);
    if let Some(services) = &et.services {
        let list_path = path_field(&[], "services");
        validate_service_list(services, &list_path, limits, rules, ctx, errors);
        check_environment_entry_limit(services, &list_path, ctx, errors);
        validate_environment_template_dependencies(
            services,
            et.environment.as_ref(),
            &list_path,
            env_params,
            limits,
            errors,
        );
    }

    if let Some(env) = &et.environment {
        let env_path = path_field(&[], "environment");
        let known = EnvironmentDependencyTargets {
            services: &et.services().iter().map(|s| s.name.as_str()).collect(),
            required: &HashSet::new(),
            where_: "no Service of that name in this document's services",
        };
        validate_environment_dependencies(
            env,
            &env_path,
            EnvironmentKind::Job,
            &known,
            env_params,
            errors,
        );
    }
    // §1.2 *services* constraint 4: the graph of the Services' own
    // dependencies, with an edge from each Service whose Sessions enter the
    // document's Environment to each Service it lists.
    if let Some(cycle) = environment_template_service_cycle(et) {
        errors.add(&path_field(&[], "services"), cycle.to_string());
    }
}

/// §1.2 item 6 constraints 4–5 / §3.2 constraint 4 / §9 item 4 for an Environment Template's `services`: each `dependencies` list
/// is non-empty, every entry uses the `service` key and names a Service of
/// this list other than the one listing it, and no target is listed twice.
/// The graph is checked by the caller, with the edges the document's
/// Environment adds. (Scope is not checked: an external Service's scope is
/// computed over the combined Job, and one nothing lists is never started.)
fn validate_environment_template_dependencies(
    services: &[Service],
    own_environment: Option<&crate::template::Environment>,
    list_path: &[PathElement],
    env_params: bool,
    limits: &EffectiveLimits,
    errors: &mut ValidationErrors,
) {
    let service_names: HashSet<&str> = services.iter().map(|s| s.name.as_str()).collect();
    for (k, svc) in services.iter().enumerate() {
        let Some(deps) = &svc.dependencies else {
            continue;
        };
        let deps_path = path_field(&path_index(list_path, k), "dependencies");
        if deps.is_empty() {
            errors.add(&deps_path, "must not be empty.");
        }
        let mut seen_envs: HashSet<&str> = HashSet::new();
        let mut seen: HashSet<DependencyTarget<'_>> = HashSet::new();
        for (j, dep) in deps.iter().enumerate() {
            let dep_path = path_index(&deps_path, j);
            check_non_step_list_entry(
                dep,
                &dep_path,
                ListOwner::EnvironmentTemplateService,
                env_params,
                errors,
            );
            match dep.target() {
                None => errors.add(&dep_path, malformed_service_owned_dependency(dep)),
                Some(target @ DependencyTarget::Step(..)) => errors.add(
                    &dep_path,
                    format!(
                        "dependency '{target}' names a Step, but an Environment \
                         Template has no Steps; a Service here may depend only on a Service of \
                         the same document, as 'service: <name>'{}",
                        dependency_kind_hint(target, service_names.iter().copied())
                    ),
                ),
                Some(DependencyTarget::Environment(name)) => {
                    check_environment_template_service_entry(
                        dep,
                        name,
                        &svc.name,
                        own_environment,
                        &dep_path,
                        limits,
                        errors,
                    );
                }
                Some(DependencyTarget::Service(name)) => {
                    if name == svc.name {
                        errors.add(&dep_path, "cannot depend on itself.");
                    } else if !service_names.contains(name) {
                        errors.add(
                            &dep_path,
                            unknown_service_dependency(
                                name,
                                "no Service of that name in this document's services",
                                std::iter::empty(),
                            ),
                        );
                    }
                }
            }
            // A repeated `environment:` entry is RFC 0010's constraint 3,
            // with its message, as pass 11 reports it for a Step.
            match dep.target() {
                Some(DependencyTarget::Environment(name)) if !seen_envs.insert(name) => errors.add(
                    &path_field(&dep_path, "environment"),
                    format!("duplicate environment dependency '{name}'."),
                ),
                Some(DependencyTarget::Environment(_)) => {}
                Some(t) if !seen.insert(dedup_key(t)) => {
                    errors.add(&dep_path, duplicate_dependency(dep));
                }
                _ => {}
            }
        }
    }
}

/// The caller limit on `environment:` entries
/// ([`CallerLimits::max_step_environment_entries`](crate::types::CallerLimits::max_step_environment_entries)),
/// applied to each Service of `services` at `list_path` as to a Step: a
/// Service's entries work exactly as a Step's (RFC 0009 `<StepDependency>`
/// item 3). Every entry with a non-null `environment` counts as written,
/// and the error is at the Service's `dependencies`, with the Step's
/// message.
fn check_environment_entry_limit(
    services: &[Service],
    list_path: &[PathElement],
    ctx: &ValidationContext,
    errors: &mut ValidationErrors,
) {
    let Some(max) = ctx.caller_limits.max_step_environment_entries else {
        return;
    };
    for (k, svc) in services.iter().enumerate() {
        let count = svc
            .dependencies
            .iter()
            .flatten()
            .filter(|d| d.environment.is_some())
            .count();
        if count > max {
            errors.add(
                &path_field(&path_index(list_path, k), "dependencies"),
                format!("exceeds caller limit of {max} 'environment' entries ({count} entries)."),
            );
        }
    }
}

/// §1.2 *services* constraint 5 for one `environment:` entry of an
/// Environment Template's Service named `service`: it must be disabled,
/// with an `enableIf` that is the literal `false` and no `parameters`, so
/// that it only declines an Environment. Its name is checked as every
/// Environment name is (§4.1). One that names the document's own
/// `environment` resolves to it, so it is checked here: declining an
/// `activation: ALWAYS` one is an error, as for a Step (RFC 0010 §3.2 item
/// 3.2). Any other name resolves among the Environment Templates attached
/// before this one, which only job creation knows. A repeated name is
/// RFC 0010's constraint 3, reported with the other duplicates.
fn check_environment_template_service_entry(
    dep: &StepDependency,
    name: &str,
    service: &str,
    own_environment: Option<&crate::template::Environment>,
    dep_path: &[PathElement],
    limits: &EffectiveLimits,
    errors: &mut ValidationErrors,
) {
    super::env_params::check_environment_name(
        name,
        &path_field(dep_path, "environment"),
        limits,
        errors,
    );
    let disabled = dep.literal_enabled() == Some(false);
    if !disabled {
        errors.add(
            dep_path,
            format!(
                "dependency 'environment: {name}' must be disabled: a Service in an Environment \
                 Template may have only 'environment:' entries whose enableIf is the literal \
                 false and that have no parameters, which decline the Environment. The \
                 document's author writes its environment too, and can give it the values \
                 directly (Template Schemas §1.2 services constraint 5)."
            ),
        );
        return;
    }
    if dep.parameters.is_some() {
        errors.add(
            &path_field(dep_path, "parameters"),
            "is not permitted on an 'environment:' entry of a Service in an Environment \
             Template, which may only decline the Environment (Template Schemas §1.2 services \
             constraint 5).",
        );
    }
    if let Some(env) = own_environment.filter(|env| env.name == name) {
        if env.activation() == crate::template::EnvironmentActivation::Always {
            errors.add(
                &path_field(dep_path, "enableIf"),
                super::env_params::always_entry_disabled_message(
                    super::env_params::RequesterKind::Service,
                    service,
                    &format!("'{name}'"),
                ),
            );
        }
    }
}

/// The prerequisites of `SERVICE`: a document that lists it must also list
/// `EXPR` and `ENV_PARAMS` (RFC 0009 "Backward compatibility"; §1.2 item 3
/// gives an Environment Template the same prerequisites). Reports at
/// `extensions`, naming the missing ones, and returns `false` when the
/// document breaks the rule; the caller then runs no other pass, as for
/// `ENV_PARAMS` without `EXPR`, so every `SERVICE` rule may take both as
/// listed.
pub(super) fn check_prerequisites(ctx: &ValidationContext, errors: &mut ValidationErrors) -> bool {
    if !ctx.profile.has_extension(ModelExtension::Service) {
        return true;
    }
    let missing: Vec<&str> = [ModelExtension::Expr, ModelExtension::EnvParams]
        .into_iter()
        .filter(|e| !ctx.profile.has_extension(*e))
        .map(|e| e.as_str())
        .collect();
    if missing.is_empty() {
        return true;
    }
    errors.add(
        &path_field(&[], "extensions"),
        format!(
            "SERVICE requires EXPR and ENV_PARAMS; {} must also be listed in the template's \
             `extensions` (RFC 0009).",
            missing.join(" and ")
        ),
    );
    false
}

/// Validate one `services` list: its size, the uniqueness of its names,
/// and each `<Service>`.
fn validate_service_list(
    services: &[Service],
    list_path: &[PathElement],
    limits: &EffectiveLimits,
    rules: &EffectiveRules,
    ctx: &ValidationContext,
    errors: &mut ValidationErrors,
) {
    if services.is_empty() {
        errors.add(list_path, "must not be empty.");
    }
    if services.len() > MAX_SERVICES {
        errors.add(
            list_path,
            format!("must not contain more than {MAX_SERVICES} elements."),
        );
    }
    let mut names: HashSet<&str> = HashSet::new();
    for (i, service) in services.iter().enumerate() {
        let service_path = path_index(list_path, i);
        if !names.insert(service.name.as_str()) {
            errors.add(
                &service_path,
                format!("duplicate service name: '{}'", service.name),
            );
        }
        validate_service(service, &service_path, limits, rules, ctx, errors);
    }
}

/// Validate an `externalServices` list (§1.1 item 9, §9.8): its size, the
/// uniqueness of its names, that no name is also an inline Service's
/// (`service_names`), and each `<ServiceRequirement>`'s ports.
fn validate_requirement_list(
    requirements: &[ServiceRequirement],
    list_path: &[PathElement],
    service_names: &HashSet<&str>,
    limits: &EffectiveLimits,
    errors: &mut ValidationErrors,
) {
    if requirements.is_empty() {
        errors.add(list_path, "must not be empty.");
    }
    if requirements.len() > MAX_SERVICES {
        errors.add(
            list_path,
            format!("must not contain more than {MAX_SERVICES} elements."),
        );
    }
    let mut names: HashSet<&str> = HashSet::new();
    for (i, req) in requirements.iter().enumerate() {
        let req_path = path_index(list_path, i);
        if !names.insert(req.name.as_str()) {
            errors.add(
                &req_path,
                format!("duplicate service requirement name: '{}'", req.name),
            );
        }
        let name_path = path_field(&req_path, "name");
        validate_service_identifier(&req.name, &name_path, limits, errors);
        if service_names.contains(req.name.as_str()) {
            errors.add(
                &name_path,
                format!(
                    "'{}' is also declared in services; a Service is either declared or \
                     required, not both.",
                    req.name
                ),
            );
        }
        let ports_path = path_field(&req_path, "ports");
        if req.ports.is_empty() {
            errors.add(&ports_path, "must not be empty.");
        }
        if req.ports.len() > MAX_PORTS {
            errors.add(
                &ports_path,
                format!("must not contain more than {MAX_PORTS} elements."),
            );
        }
        let mut port_names: HashSet<&str> = HashSet::new();
        for (j, port) in req.ports.iter().enumerate() {
            let port_path = path_index(&ports_path, j);
            if !port_names.insert(port.name.as_str()) {
                errors.add(&port_path, format!("duplicate port name '{}'.", port.name));
            }
            validate_service_identifier(
                &port.name,
                &path_field(&port_path, "name"),
                limits,
                errors,
            );
        }
    }
}

/// Validate one `<Service>` (§9) at `path`.
fn validate_service(
    service: &Service,
    path: &[PathElement],
    limits: &EffectiveLimits,
    rules: &EffectiveRules,
    ctx: &ValidationContext,
    errors: &mut ValidationErrors,
) {
    // §9.2 <ServiceName>
    validate_service_identifier(&service.name, &path_field(path, "name"), limits, errors);

    if let Some(desc) = &service.description {
        validate_description(desc, &path_field(path, "description"), limits, errors);
    }

    if let Some(hr) = &service.host_requirements {
        validate_host_requirements_in_context(
            hr,
            &path_field(path, "hostRequirements"),
            rules,
            ctx,
            errors,
        );
    }

    // §9 item 6, §9.3 <ServicePort>
    let ports_path = path_field(path, "ports");
    if service.ports.is_empty() {
        errors.add(&ports_path, "must not be empty.");
    }
    if service.ports.len() > MAX_PORTS {
        errors.add(
            &ports_path,
            format!("must not contain more than {MAX_PORTS} elements."),
        );
    }
    let mut port_names: HashSet<&str> = HashSet::new();
    for (i, port) in service.ports.iter().enumerate() {
        let port_path = path_index(&ports_path, i);
        if !port_names.insert(port.name.as_str()) {
            errors.add(&port_path, format!("duplicate port name '{}'.", port.name));
        }
        validate_service_identifier(&port.name, &path_field(&port_path, "name"), limits, errors);
        if let Some(number) = &port.port {
            let number_path = path_field(&port_path, "port");
            check_literal_int(
                number,
                &number_path,
                1..=65535,
                "must be between 1 and 65535.",
                errors,
            );
            // §9 item 6.4: the same literal number twice in
            // one protocol's space. Format-string numbers are compared at
            // job creation, once resolved.
            if let Some(n) = literal_int(number) {
                if let Some(earlier) = service.ports[..i].iter().find(|p| {
                    p.protocol == port.protocol && p.port.as_ref().and_then(literal_int) == Some(n)
                }) {
                    errors.add(
                        &number_path,
                        format!(
                            "{} port {n} is also used by port '{}'; two ports with the same \
                             protocol must not have the same port number.",
                            port.protocol, earlier.name
                        ),
                    );
                }
            }
        }
    }
    let protocol_of = |name: &str| {
        service
            .ports
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.protocol)
    };
    let has_tcp_port = service.tcp_port_names().next().is_some();

    // §9.4 <ServiceHealthCheck>
    let health = service.health_check();
    // §9 item 7: TCP_CONNECT, given or defaulted, needs a TCP
    // port to probe. (An empty `ports` list is already reported above.)
    if !service.ports.is_empty()
        && !has_tcp_port
        && matches!(health, ServiceHealthCheck::TcpConnect { .. })
    {
        let which = if service.health_check.is_some() {
            "a TCP_CONNECT health check"
        } else {
            "the default TCP_CONNECT health check"
        };
        errors.add(
            &path_field(path, "healthCheck"),
            format!(
                "{which} has no TCP port to probe: none of the Service's ports has protocol \
                 TCP, so a healthCheck of type STDOUT or COMMAND is required."
            ),
        );
    }
    if let Some(declared) = &service.health_check {
        let hc_path = path_field(path, "healthCheck");
        // §9.4 items 3–6: every numeric field is a <posinteger>.
        for (name, value) in declared.numeric_fields() {
            if let Some(value) = value {
                check_literal_int(
                    value,
                    &path_field(&hc_path, name),
                    1..=i64::MAX,
                    "must be > 0.",
                    errors,
                );
            }
        }
        match declared {
            ServiceHealthCheck::TcpConnect {
                ports: Some(probed),
                ..
            } => {
                let probed_path = path_field(&hc_path, "ports");
                if probed.is_empty() {
                    errors.add(&probed_path, "if provided, must not be empty.");
                }
                // §9 item 7, §9.4 items 2, 3, and 6, §9.7 item 3: every port a TCP_CONNECT check names is
                // declared and has protocol TCP (§9.4 item 2).
                for (i, name) in probed.iter().enumerate() {
                    match protocol_of(name) {
                        None => errors.add(
                            &path_index(&probed_path, i),
                            format!(
                                "references undeclared port '{name}'; declared ports: {}.",
                                service
                                    .ports
                                    .iter()
                                    .map(|p| p.name.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        ),
                        Some(ServicePortProtocol::Tcp) => {}
                        Some(protocol) => errors.add(
                            &path_index(&probed_path, i),
                            format!(
                                "port '{name}' has protocol {protocol} and cannot be probed by a \
                                 TCP_CONNECT health check; only TCP ports may be named."
                            ),
                        ),
                    }
                }
            }
            // §9.4 item 6: a STDOUT check gives
            // failureThreshold only together with healthIntervalSeconds —
            // without a heartbeat there is no probe for it to count.
            ServiceHealthCheck::Stdout {
                health_interval_seconds: None,
                failure_threshold: Some(_),
                ..
            } => {
                errors.add(
                    &path_field(&hc_path, "failureThreshold"),
                    "a STDOUT health check gives failureThreshold only together with \
                     healthIntervalSeconds; without a heartbeat interval there is no probe for \
                     it to count.",
                );
            }
            _ => {}
        }
    }

    // §9.5 <ServiceRestartPolicy>
    if let Some(policy) = &service.restart_policy {
        let policy_path = path_field(path, "restartPolicy");
        if let Some(attempts) = &policy.max_restarts {
            check_literal_int(
                attempts,
                &path_field(&policy_path, "maxRestarts"),
                0..=i64::MAX,
                "must be >= 0.",
                errors,
            );
            // §9.5 item 2: a literal maxRestarts > 0 needs
            // completedTasks. A format string is checked at job creation.
            if policy.completed_tasks.is_none() && literal_int(attempts).is_some_and(|n| n > 0) {
                errors.add(
                    &policy_path,
                    completed_tasks_required_message(literal_int(attempts).expect("literal")),
                );
            }
        }
    }

    if let Some(vars) = &service.variables {
        validate_variables(vars, &path_field(path, "variables"), limits, errors);
    }

    // §9.6 <ServiceScript>, §9.7 <ServiceActions>
    let script_path = path_field(path, "script");
    let actions_path = path_field(&script_path, "actions");
    for (name, action) in service.script.actions.iter_named() {
        validate_action(
            action,
            &path_field(&actions_path, name),
            limits,
            rules,
            errors,
        );
    }
    // §9.7 item 3: onHealthCheck iff type is COMMAND.
    let is_command = matches!(health, ServiceHealthCheck::Command { .. });
    match (&service.script.actions.on_health_check, is_command) {
        (None, true) => errors.add(
            &actions_path,
            "onHealthCheck must be defined when healthCheck.type is COMMAND.",
        ),
        (Some(_), false) => errors.add(
            &path_field(&actions_path, "onHealthCheck"),
            format!(
                "onHealthCheck must not be defined when healthCheck.type is {}.",
                health.type_name()
            ),
        ),
        _ => {}
    }
    if let Some(files) = &service.script.embedded_files {
        let files_path = path_field(&script_path, "embeddedFiles");
        if files.is_empty() {
            errors.add(&files_path, "must not be empty.");
        }
        validate_embedded_files(files, &files_path, errors);
        for (i, f) in files.iter().enumerate() {
            if f.name.chars().count() > limits.max_identifier_len {
                errors.add(
                    &path_field(&path_index(&files_path, i), "name"),
                    format!("exceeds {} characters.", limits.max_identifier_len),
                );
            }
            if let Some(filename) = &f.filename {
                if filename.chars().count() > limits.max_filename_len {
                    errors.add(
                        &path_field(&path_index(&files_path, i), "filename"),
                        format!("exceeds {} characters.", limits.max_filename_len),
                    );
                }
            }
        }
    }
}

/// §9.5 item 2: the message for a `restartPolicy` whose
/// `maxRestarts` (`n`, greater than 0) is given without `completedTasks`.
pub fn completed_tasks_required_message(n: i64) -> String {
    format!(
        "completedTasks must be provided when maxRestarts is greater than 0 (maxRestarts is \
         {n}): a template that allows relaunch must say what a relaunch means for completed \
         Tasks, KEEP or RERUN (Template Schemas §9.5 item 2)."
    )
}

/// §9.2 / §9.3 item 1 / §9.8: a Service, requirement, or port name is an `<Identifier>` (§7.1)
/// within the effective identifier length limit, and is not `File`.
fn validate_service_identifier(
    name: &str,
    path: &[PathElement],
    limits: &EffectiveLimits,
    errors: &mut ValidationErrors,
) {
    let is_identifier = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !is_identifier {
        errors.add(path, format!("'{name}' is not a valid identifier."));
    }
    if name.chars().count() > limits.max_identifier_len {
        errors.add(
            path,
            format!("exceeds {} characters.", limits.max_identifier_len),
        );
    }
    if name == RESERVED_FILE_NAME {
        errors.add(
            path,
            format!(
                "must not be '{RESERVED_FILE_NAME}'; it is reserved for Service.File.* references."
            ),
        );
    }
}

/// The integer a numeric `@fmtstring` field holds when it carries no
/// expression, or `None` for a format string or non-integer text.
fn literal_int(value: &openjd_expr::FormatString) -> Option<i64> {
    let raw = value.raw().trim();
    if value.has_complex_expressions() || raw.contains("{{") {
        return None;
    }
    raw.parse::<i64>().ok()
}

/// Check a numeric `@fmtstring` field whose value carries no expression,
/// in the manner of `<Action>.timeout`: it must parse as an integer within
/// `range`, else `range_msg` (or "must be an integer." when it does not
/// parse). A value containing an expression is resolved and range-checked
/// at job creation instead.
fn check_literal_int(
    value: &openjd_expr::FormatString,
    path: &[PathElement],
    range: std::ops::RangeInclusive<i64>,
    range_msg: &str,
    errors: &mut ValidationErrors,
) {
    let raw = value.raw().trim();
    if value.has_complex_expressions() || raw.contains("{{") {
        return;
    }
    match raw.parse::<i64>() {
        Ok(v) if range.contains(&v) => {}
        Ok(_) => errors.add(path, range_msg),
        Err(_) => errors.add(path, "must be an integer."),
    }
}

#[cfg(test)]
mod tests {
    //! Integration tests in `tests/integration/test_service.rs` exercise
    //! the full decode + validate pipeline against real templates and
    //! assert every error path and message. No direct unit tests here:
    //! every helper is pure-data and already covered through the
    //! integration surface.
}
