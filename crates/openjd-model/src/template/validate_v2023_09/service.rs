// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Pass 11: `SERVICE` — validate or reject (RFC 0009, Template Schemas §9).
//!
//! Four fields are gated by the `SERVICE` extension:
//! - `jobServices` on the job template root (§1.1)
//! - `stepServices` on `<StepTemplate>` (§3)
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
//!   `extensions:` must also list `EXPR` (§9, §9.7 item 7).
//! - **List shape.** Each Service list has 1–10 elements (§1.1 item 8, §1.2
//!   item 6, §3 item 6).
//! - **Name uniqueness.** Service names are unique within a list, and a Step
//!   Service must not share a name with a Job Service. Different Steps may
//!   reuse a Step Service name (§9.7 item 5).
//! - **`<Service>` structure** (§9–§9.6): identifier names that are not
//!   `File`; 1–10 uniquely named ports; literal `port` in 1–65535; literal
//!   `timeoutSeconds`/`intervalSeconds` positive; literal `maxAttempts`
//!   non-negative; `onReadinessCheck` defined iff the readiness type is
//!   `COMMAND`; every port a `TCP_CONNECT` check names is declared and has
//!   `protocol: TCP`; a Service none of whose ports is TCP declares a
//!   `STDOUT` or `COMMAND` check (§9 item 6, §9.7 item 4); no two ports of
//!   the same `protocol` have the same literal `port` number (§9 item 5.4,
//!   §9.7 item 8 — format-string numbers are checked at job creation).
//!   `description`, `variables`, `hostRequirements`, embedded files and
//!   every `<Action>` reuse the pass-6 validators.
//! - **`runScope`** (§4 item 3, §9.7 item 3): at least one element, only
//!   recognized `<RunScopeName>`s (`TASK`, `SERVICE`), no duplicates.
//!
//! The numeric `@fmtstring` fields are modeled like `<Action>.timeout`: a
//! value without an expression is checked here, and a format string is
//! resolved and range-checked at job creation. Format-string *scopes*
//! (`Service.*`, §9.7 items 1–2) and `let` bindings are validated by the
//! format-string pass, not here.

use std::collections::HashSet;

use super::structure::{
    validate_action, validate_description, validate_embedded_files,
    validate_host_requirements_in_context, validate_variables,
};
use super::{EffectiveLimits, EffectiveRules};
use crate::error::{path_field, path_index, PathElement, ValidationErrors};
use crate::template::*;
use crate::types::{ModelExtension, ValidationContext};

/// §1.1 item 8 / §1.2 item 6 / §3 item 6 / §9 item 5: maximum elements in a
/// Service list and in a Service's `ports` list.
const MAX_SERVICES: usize = 10;
const MAX_PORTS: usize = 10;

/// §9.1 / §9.2: the name reserved for `Service.File.*` references.
const RESERVED_FILE_NAME: &str = "File";

/// Validate RFC 0009 constraints for a job template.
///
/// Runs regardless of whether `SERVICE` is enabled: when disabled, it
/// rejects templates that use `jobServices`, `stepServices`, or `runScope`;
/// when enabled, it enforces the EXPR prerequisite, validates every Service,
/// and validates every Environment's `runScope`.
pub fn validate_services_job_template(
    jt: &JobTemplate,
    limits: &EffectiveLimits,
    rules: &EffectiveRules,
    ctx: &ValidationContext,
    errors: &mut ValidationErrors,
) {
    let active = ctx.profile.has_extension(ModelExtension::Service);
    check_expr_prerequisite(ctx, errors);

    let mut job_service_names: HashSet<&str> = HashSet::new();
    if let Some(services) = &jt.job_services {
        let list_path = path_field(&[], "jobServices");
        if !active {
            errors.add(&list_path, "jobServices requires the SERVICE extension.");
        } else {
            validate_service_list(
                services,
                &list_path,
                &HashSet::new(),
                limits,
                rules,
                ctx,
                errors,
            );
            job_service_names.extend(services.iter().map(|s| s.name.as_str()));
        }
    }

    for (i, step) in jt.steps.iter().enumerate() {
        let Some(services) = &step.step_services else {
            continue;
        };
        let list_path = path_field(
            &[PathElement::Field("steps".into()), PathElement::Index(i)],
            "stepServices",
        );
        if !active {
            errors.add(&list_path, "stepServices requires the SERVICE extension.");
        } else {
            // §3 item 6.4: a Step Service must not share a name with a Job
            // Service. Different Steps may reuse a name (item 6, note), so
            // only the Job Services are carried into each Step's check.
            validate_service_list(
                services,
                &list_path,
                &job_service_names,
                limits,
                rules,
                ctx,
                errors,
            );
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

/// Validate RFC 0009 constraints for an environment template: the EXPR
/// prerequisite, the `services` list (§1.2 item 6, gated and validated
/// exactly like `jobServices`), and the Environment's `runScope`.
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
            validate_service_list(
                services,
                &list_path,
                &HashSet::new(),
                limits,
                rules,
                ctx,
                errors,
            );
        }
    }

    if let Some(env) = &et.environment {
        validate_run_scope(env, &path_field(&[], "environment"), active, errors);
    }
}

/// §4 item 3 / §9.7 item 3: validate one Environment's `runScope` at
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

/// Validate one `jobServices` or `stepServices` list: its size, the
/// uniqueness of its names (also against `outer_names`, the Job Service
/// names when validating a Step's list), and each `<Service>`.
fn validate_service_list(
    services: &[Service],
    list_path: &[PathElement],
    outer_names: &HashSet<&str>,
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
        if outer_names.contains(service.name.as_str()) || !names.insert(service.name.as_str()) {
            errors.add(
                &service_path,
                format!("duplicate service name: '{}'", service.name),
            );
        }
        validate_service(service, &service_path, limits, rules, ctx, errors);
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
    // §9.1 <ServiceName>
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

    // §9 item 5, §9.2 <ServicePort>
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
            // §9 item 5.4 / §9.7 item 8: the same literal number twice in
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

    // §9.3 <ServiceReadinessCheck>
    let readiness = service.readiness_check();
    // §9 item 6 / §9.7 item 4: TCP_CONNECT, given or defaulted, needs a TCP
    // port to probe. (An empty `ports` list is already reported above.)
    if !service.ports.is_empty()
        && !has_tcp_port
        && matches!(readiness, ServiceReadinessCheck::TcpConnect { .. })
    {
        let which = if service.readiness_check.is_some() {
            "a TCP_CONNECT readiness check"
        } else {
            "the default TCP_CONNECT readiness check"
        };
        errors.add(
            &path_field(path, "readinessCheck"),
            format!(
                "{which} has no TCP port to probe: none of the Service's ports has protocol \
                 TCP, so a readinessCheck of type STDOUT or COMMAND is required."
            ),
        );
    }
    if let Some(declared) = &service.readiness_check {
        let rc_path = path_field(path, "readinessCheck");
        if let Some(timeout) = declared.timeout_seconds() {
            check_literal_int(
                timeout,
                &path_field(&rc_path, "timeoutSeconds"),
                1..=i64::MAX,
                "must be > 0.",
                errors,
            );
        }
        match declared {
            ServiceReadinessCheck::TcpConnect {
                ports: Some(probed),
                ..
            } => {
                let probed_path = path_field(&rc_path, "ports");
                if probed.is_empty() {
                    errors.add(&probed_path, "if provided, must not be empty.");
                }
                // §9.7 item 4: every port a TCP_CONNECT check names is
                // declared and has protocol TCP (§9.3 item 2).
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
                                 TCP_CONNECT readiness check; only TCP ports may be named."
                            ),
                        ),
                    }
                }
            }
            ServiceReadinessCheck::Command {
                interval_seconds: Some(interval),
                ..
            } => {
                check_literal_int(
                    interval,
                    &path_field(&rc_path, "intervalSeconds"),
                    1..=i64::MAX,
                    "must be > 0.",
                    errors,
                );
            }
            _ => {}
        }
    }

    // §9.4 <ServiceRestartPolicy>
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

    // §9.5 <ServiceScript>, §9.6 <ServiceActions>
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
    // §9.6 item 3 / §9.7 item 4: onReadinessCheck iff type is COMMAND.
    let is_command = matches!(readiness, ServiceReadinessCheck::Command { .. });
    match (&service.script.actions.on_readiness_check, is_command) {
        (None, true) => errors.add(
            &actions_path,
            "onReadinessCheck must be defined when readinessCheck.type is COMMAND.",
        ),
        (Some(_), false) => errors.add(
            &path_field(&actions_path, "onReadinessCheck"),
            format!(
                "onReadinessCheck must not be defined when readinessCheck.type is {}.",
                readiness.type_name()
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

/// §9.1 / §9.2 item 1: a Service or port name is an `<Identifier>` (§7.1)
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
