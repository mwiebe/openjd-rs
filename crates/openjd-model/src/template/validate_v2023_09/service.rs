// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Pass 11: `SERVICE` — validate or reject (RFC 0009, Template Schemas §9).
//!
//! Four fields are gated by the `SERVICE` extension:
//! - `services` on the job template root (§1.1 item 8)
//! - `requiresServices` on the job template root (§1.1 item 9)
//! - `services` on the environment template root (§1.2)
//! - `runScope` on `<Environment>` (§4 item 3)
//!
//! (The `onWrapService*` hooks on `<EnvironmentActions>`, gated on both
//! `WRAP_ACTIONS` and `SERVICE`, are handled by pass 10 with the other wrap
//! hooks.)
//!
//! When the extension is not enabled, using any of these fields is a
//! validation error and nothing inside the lists is examined. When it is
//! enabled, this pass additionally enforces:
//!
//! - **EXPR prerequisite.** A template that lists `SERVICE` in
//!   `extensions:` must also list `EXPR` (§9, §9.9 item 7).
//! - **List shape.** Each `services` / `requiresServices` list has 1–10
//!   elements (§1.1 items 8–9, §1.2).
//! - **Name uniqueness.** Service names are unique within a list,
//!   requirement names are unique within theirs, and no requirement bears
//!   the name of an inline Service (§9.9 item 5).
//! - **Step names.** No Step's `name` contains `:` (§3.1 constraint 4,
//!   §9.9 item 12), so that a `dependsOn` beginning `service:` can only
//!   name a Service.
//! - **`dependsOn: service:<name>`** (§3.2 constraint 1, §9.9 item 9): on a
//!   Step or a Service, the name is a Service of the document's `services`
//!   or, in a Job Template, of its `requiresServices`. (Step targets, self
//!   dependency and duplicates are pass 6's for a Step; a Service's
//!   `dependencies` are checked wholly here: at least one element, each
//!   entry a Step of the Job Template or a Service other than itself, no
//!   duplicates, and in an Environment Template only the `service:` form.)
//! - **One acyclic graph.** The `dependencies` of the document's Steps and
//!   Services form one graph with every kind of edge, which must be
//!   acyclic (§3.2 constraint 3, §9.9 item 10); the error names the cycle.
//!   See [`crate::template::service_scope`].
//! - **Scope** (§9.1, §9.9 item 11): every Service of a Job Template has a
//!   non-empty scope — some Step lists it, directly or through other
//!   Services, or some Job Environment references it — else it is unused
//!   and rejected by name. A Service a Job Environment references has every
//!   Step in its scope and so may not list a Step in `dependencies`: that
//!   Step could not run until the Service was READY, and the Service could
//!   not start until the Step completed.
//! - **`<Service>` structure** (§9–§9.7): identifier names that are not
//!   `File`; 1–10 uniquely named ports; literal `port` in 1–65535; the
//!   literal `<ServiceHealthCheck>` numeric fields
//!   (`readinessIntervalSeconds`, `readinessTimeoutSeconds`,
//!   `healthIntervalSeconds`, `failureThreshold`) positive; literal
//!   `maxAttempts` non-negative; `onHealthCheck` defined iff the health
//!   check type is `COMMAND`; every port a `TCP_CONNECT` check names is
//!   declared and has `protocol: TCP`; a Service none of whose ports is TCP
//!   declares a `STDOUT` or `COMMAND` check; a `STDOUT` check gives
//!   `failureThreshold` only together with `healthIntervalSeconds` (§9 item
//!   7, §9.4 items 3 and 6, §9.9 item 4 — a `STDOUT` check's
//!   `readinessIntervalSeconds` is rejected as an unknown field at decode,
//!   as `ports` is on anything but `TCP_CONNECT`); no two ports of the same
//!   `protocol` have the same literal `port` number (§9 item 6.4, §9.9 item
//!   8 — format-string numbers are checked at job creation).
//!   `description`, `variables`, `hostRequirements`, embedded files and
//!   every `<Action>` reuse the pass-6 validators.
//! - **`<ServiceRequirement>` structure** (§9.8): an identifier name that is
//!   not `File`; 1–10 uniquely named ports, each an identifier other than
//!   `File`.
//! - **`runScope`** (§4 item 3, §9.9 item 3): at least one element, only
//!   recognized `<RunScopeName>`s (`TASK`, `SERVICE`), no duplicates.
//!
//! The numeric `@fmtstring` fields are modeled like `<Action>.timeout`: a
//! value without an expression is checked here, and a format string is
//! resolved and range-checked at job creation. Format-string *scopes*
//! (`Service.*`, §9.9 items 1–2) and `let` bindings are validated by the
//! format-string pass, not here.

use std::collections::HashSet;

use super::structure::{
    validate_action, validate_description, validate_embedded_files,
    validate_host_requirements_in_context, validate_variables,
};
use super::{EffectiveLimits, EffectiveRules};
use crate::error::{path_field, path_index, PathElement, ValidationErrors};
use crate::template::service_scope::{compute_service_scopes, service_dependency_cycle};
use crate::template::*;
use crate::types::{ModelExtension, ValidationContext};

/// §1.1 items 8–9 / §1.2 / §9 item 6 / §9.8 item 2: maximum elements in a
/// Service list, a requirement list, and a `ports` list.
const MAX_SERVICES: usize = 10;
const MAX_PORTS: usize = 10;

/// §9.2 / §9.3: the name reserved for `Service.File.*` references.
const RESERVED_FILE_NAME: &str = "File";

/// Validate RFC 0009 constraints for a job template.
///
/// Runs regardless of whether `SERVICE` is enabled: when disabled, it
/// rejects templates that use `services`, `requiresServices`, or `runScope`;
/// when enabled, it enforces the EXPR prerequisite, validates every Service
/// and requirement, every `service:` dependency, the combined dependency
/// graph and the Services' scopes, the Step-name colon rule, and every
/// Environment's `runScope`.
pub fn validate_services_job_template(
    jt: &JobTemplate,
    limits: &EffectiveLimits,
    rules: &EffectiveRules,
    ctx: &ValidationContext,
    errors: &mut ValidationErrors,
) {
    let active = ctx.profile.has_extension(ModelExtension::Service);
    check_expr_prerequisite(ctx, errors);

    let mut service_names: HashSet<&str> = HashSet::new();
    if let Some(services) = &jt.services {
        let list_path = path_field(&[], "services");
        if !active {
            errors.add(&list_path, "services requires the SERVICE extension.");
        } else {
            validate_service_list(services, &list_path, limits, rules, ctx, errors);
            service_names.extend(services.iter().map(|s| s.name.as_str()));
        }
    }
    if active {
        validate_step_names(jt, errors);
        validate_job_template_dependencies(jt, errors);
    }

    if let Some(requirements) = &jt.requires_services {
        let list_path = path_field(&[], "requiresServices");
        if !active {
            errors.add(
                &list_path,
                "requiresServices requires the SERVICE extension.",
            );
        } else {
            validate_requirement_list(requirements, &list_path, &service_names, limits, errors);
        }
    }

    // §4 item 3: `runScope` on every Environment.
    if let Some(envs) = &jt.job_environments {
        let envs_path = path_field(&[], "jobEnvironments");
        for (i, env) in envs.iter().enumerate() {
            validate_run_scope(env, &path_index(&envs_path, i), active, errors);
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
            validate_run_scope(env, &path_index(&envs_path, j), active, errors);
        }
    }
}

/// §3.1 constraint 4 / §9.9 item 12: with `SERVICE`, no Step's `name`
/// contains `:`.
fn validate_step_names(jt: &JobTemplate, errors: &mut ValidationErrors) {
    for (i, step) in jt.steps.iter().enumerate() {
        if step.name.contains(':') {
            errors.add(
                &path_field(
                    &[PathElement::Field("steps".into()), PathElement::Index(i)],
                    "name",
                ),
                format!(
                    "must not contain ':' when the SERVICE extension is used, so that a \
                     dependsOn value beginning '{SERVICE_DEPENDENCY_PREFIX}' can only name a \
                     Service (Template Schemas §3.1 constraint 4)."
                ),
            );
        }
    }
}

/// `dependency 'service:X' not found; …` — the `service:` target names no
/// Service. `where` says which lists were searched.
fn unknown_service_dependency(name: &str, where_: &str) -> String {
    format!("dependency '{SERVICE_DEPENDENCY_PREFIX}{name}' not found: {where_}.")
}

/// §3.2 constraints 1–3 / §9 item 4 / §9.9 items 9–11 for a Job Template
/// with `SERVICE`: every `service:` entry of a Step's `dependencies` names a
/// Service of `services` or `requiresServices`; each Service's
/// `dependencies` is non-empty, names Steps of the template or Services
/// other than itself, and lists no target twice; the combined graph is
/// acyclic (the error names the cycle); every Service has a non-empty
/// scope; and a Service with every Step in its scope lists no Step.
fn validate_job_template_dependencies(jt: &JobTemplate, errors: &mut ValidationErrors) {
    let step_names: HashSet<&str> = jt.steps.iter().map(|s| s.name.as_str()).collect();
    let service_names: HashSet<&str> = jt.services().iter().map(|s| s.name.as_str()).collect();
    let required_names: HashSet<&str> = jt
        .requires_services()
        .iter()
        .map(|r| r.name.as_str())
        .collect();
    let where_ = "no Service of that name in services or requiresServices";

    // Steps: `service:` targets (pass 6 did the Step targets).
    for (i, step) in jt.steps.iter().enumerate() {
        let deps_path = path_field(
            &[PathElement::Field("steps".into()), PathElement::Index(i)],
            "dependencies",
        );
        for (j, dep) in step.dependencies.iter().flatten().enumerate() {
            if let Some(name) = dep.target(true).service() {
                if !service_names.contains(name) && !required_names.contains(name) {
                    errors.add(
                        &path_index(&deps_path, j),
                        unknown_service_dependency(name, where_),
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
        let mut seen: HashSet<&str> = HashSet::new();
        for (j, dep) in deps.iter().enumerate() {
            let dep_path = path_index(&deps_path, j);
            match dep.target(true) {
                DependencyTarget::Step(name) => {
                    if !step_names.contains(name) {
                        errors.add(&dep_path, format!("dependency '{name}' not found."));
                    }
                }
                DependencyTarget::Service(name) => {
                    if name == svc.name {
                        errors.add(&dep_path, "cannot depend on itself.");
                    } else if !service_names.contains(name) && !required_names.contains(name) {
                        errors.add(&dep_path, unknown_service_dependency(name, where_));
                    }
                }
            }
            if !seen.insert(dep.depends_on.as_str()) {
                errors.add(
                    &dep_path,
                    format!("duplicate dependency '{}'.", dep.depends_on),
                );
            }
        }
    }

    // The combined graph and the scopes.
    let scopes = match compute_service_scopes(jt) {
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
        let svc_path = path_index(&list_path, k);
        if computed.is_unused() {
            errors.add(
                &svc_path,
                format!(
                    "Service '{}' is unused: no Step or Service lists \
                     '{SERVICE_DEPENDENCY_PREFIX}{}' in its dependencies and no Job Environment \
                     references it, so no Step is in its scope.",
                    svc.name, svc.name
                ),
            );
        }
        if computed.referenced_by_job_environment {
            let deps_path = path_field(&svc_path, "dependencies");
            for (j, dep) in svc.dependencies.iter().flatten().enumerate() {
                if let Some(step) = dep.target(true).step() {
                    if step_names.contains(step) {
                        errors.add(
                            &path_index(&deps_path, j),
                            format!(
                                "Step '{step}' is in the scope of Service '{}' (a Job \
                                 Environment references the Service, so every Step is in its \
                                 scope); a Service cannot depend on a Step in its own scope, \
                                 which could not run until the Service was READY.",
                                svc.name
                            ),
                        );
                    }
                }
            }
        }
    }
}

/// Validate RFC 0009 constraints for an environment template: the EXPR
/// prerequisite, the `services` list (§1.2, gated and validated exactly like
/// a Job Template's, except that a Service's `dependencies` may use only the
/// `service:` form and name a Service of this list, since the document has
/// no Steps, and `requiresServices` has no place here), and the
/// Environment's `runScope`.
pub fn validate_services_environment_template(
    et: &EnvironmentTemplate,
    limits: &EffectiveLimits,
    rules: &EffectiveRules,
    ctx: &ValidationContext,
    errors: &mut ValidationErrors,
) {
    let active = ctx.profile.has_extension(ModelExtension::Service);
    check_expr_prerequisite(ctx, errors);

    if let Some(services) = &et.services {
        let list_path = path_field(&[], "services");
        if !active {
            errors.add(&list_path, "services requires the SERVICE extension.");
        } else {
            validate_service_list(services, &list_path, limits, rules, ctx, errors);
            validate_environment_template_dependencies(services, &list_path, errors);
        }
    }

    if let Some(env) = &et.environment {
        validate_run_scope(env, &path_field(&[], "environment"), active, errors);
    }
}

/// §1.2 item 6 constraints 4–5 / §3.2 constraint 4 / §9 item 4 / §9.9 items
/// 9–10 for an Environment Template's `services`: each `dependencies` list
/// is non-empty, every entry uses the `service:` form and names a Service of
/// this list other than the one listing it, no target is listed twice, and
/// the graph is acyclic. (Scope is not checked: every Step of every Job is
/// in an external Service's scope.)
fn validate_environment_template_dependencies(
    services: &[Service],
    list_path: &[PathElement],
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
        let mut seen: HashSet<&str> = HashSet::new();
        for (j, dep) in deps.iter().enumerate() {
            let dep_path = path_index(&deps_path, j);
            match dep.target(true) {
                DependencyTarget::Step(name) => errors.add(
                    &dep_path,
                    format!(
                        "dependency '{name}' names a Step, but an Environment Template has no \
                         Steps; a Service here may depend only on a Service of the same \
                         document, as '{SERVICE_DEPENDENCY_PREFIX}<name>'."
                    ),
                ),
                DependencyTarget::Service(name) => {
                    if name == svc.name {
                        errors.add(&dep_path, "cannot depend on itself.");
                    } else if !service_names.contains(name) {
                        errors.add(
                            &dep_path,
                            unknown_service_dependency(
                                name,
                                "no Service of that name in this document's services",
                            ),
                        );
                    }
                }
            }
            if !seen.insert(dep.depends_on.as_str()) {
                errors.add(
                    &dep_path,
                    format!("duplicate dependency '{}'.", dep.depends_on),
                );
            }
        }
    }
    if let Some(cycle) = service_dependency_cycle(services) {
        errors.add(list_path, cycle.to_string());
    }
}

/// §4 item 3 / §9.9 item 3: validate one Environment's `runScope` at
/// `env_path`. Without `SERVICE` the field is rejected outright; with it,
/// the list must be non-empty, name only recognized `<RunScopeName>`s, and
/// name each at most once. Each offending element is reported on its own
/// index so the user sees the complete list.
fn validate_run_scope(
    env: &Environment,
    env_path: &[PathElement],
    active: bool,
    errors: &mut ValidationErrors,
) {
    let Some(names) = &env.run_scope else {
        return;
    };
    let path = path_field(env_path, "runScope");
    if !active {
        errors.add(&path, "runScope requires the SERVICE extension.");
        return;
    }
    if names.is_empty() {
        errors.add(&path, "must not be empty.");
    }
    let mut seen: HashSet<&str> = HashSet::new();
    for (i, name) in names.iter().enumerate() {
        let elem_path = path_index(&path, i);
        if name.parse::<RunScope>().is_err() {
            errors.add(
                &elem_path,
                format!(
                    "unknown run scope name '{name}'; expected one of {}.",
                    known_run_scopes()
                ),
            );
        } else if !seen.insert(name.as_str()) {
            errors.add(&elem_path, format!("duplicate run scope name '{name}'."));
        }
    }
}

/// The recognized `<RunScopeName>`s as `TASK, SERVICE`, for error messages.
fn known_run_scopes() -> String {
    RunScope::ALL
        .iter()
        .map(|k| k.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Enforce the EXPR prerequisite: when `SERVICE` is listed in a template's
/// `extensions:`, `EXPR` must also be listed (§9, RFC 0009).
fn check_expr_prerequisite(ctx: &ValidationContext, errors: &mut ValidationErrors) {
    let has_service = ctx.profile.has_extension(ModelExtension::Service);
    let has_expr = ctx.profile.has_extension(ModelExtension::Expr);
    if has_service && !has_expr {
        errors.add(
            &path_field(&[], "extensions"),
            "SERVICE requires EXPR; both must be listed in the template's `extensions` (RFC 0009).",
        );
    }
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

/// Validate a `requiresServices` list (§1.1 item 9, §9.8): its size, the
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
            // §9 item 6.4 / §9.9 item 8: the same literal number twice in
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
    // §9 item 7 / §9.9 item 4: TCP_CONNECT, given or defaulted, needs a TCP
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
                // §9.9 item 4: every port a TCP_CONNECT check names is
                // declared and has protocol TCP (§9.4 item 2).
                for (i, name) in probed.iter().enumerate() {
                    match protocol_of(name) {
                        None => errors.add(
                            &path_index(&probed_path, i),
                            format!("references undeclared port '{name}'."),
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
            // §9.4 item 6 / §9.9 item 4: a STDOUT check gives
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
        if let Some(attempts) = &policy.max_attempts {
            check_literal_int(
                attempts,
                &path_field(&path_field(path, "restartPolicy"), "maxAttempts"),
                0..=i64::MAX,
                "must be >= 0.",
                errors,
            );
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
    // §9.7 item 3 / §9.9 item 4: onHealthCheck iff type is COMMAND.
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
