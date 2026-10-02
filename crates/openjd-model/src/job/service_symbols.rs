// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! The `Service.*` and `WrappedService.*` symbol scopes (RFC 0009, Template
//! Schemas §7.3.1 and §4.3.1).
//!
//! A Service's endpoint — for each declared port, the TCP port number and
//! the addresses to bind and connect to — is unknown until the scheduler
//! places the Service, so the `Service.<name>.<port>.*` values are
//! `@fmtstring[host]`: template validation and job creation type-check them
//! as `unresolved[int]` / `unresolved[string]`, and the runtime binds the
//! concrete values when it starts a Session. This module is the single
//! source of the key spellings and types for both stages:
//!
//! - the `pub` functions build the concrete table a runtime seeds into a
//!   Task or Service Session's symbol table from the
//!   [`ServiceEndpoints`] it allocated, mirroring how
//!   [`build_symbol_table`](super::create_job::build_symbol_table) seeds
//!   `Param.*` / `RawParam.*`;
//! - the `pub(crate)` functions seed the same keys as `Unresolved`
//!   placeholders for pass 8 and the job-creation re-checks.
//!
//! Scope — which Services a given entity may reference, and that
//! `bindAddress` is visible only inside the declaring Service — is the
//! caller's decision (see `specs/model/validation.md`, pass 8); this module
//! only knows how to spell and type the symbols.

use openjd_expr::symbol_table::SymbolTable;
use openjd_expr::types::ExprType;
use openjd_expr::value::ExprValue;
use serde::{Deserialize, Serialize};

use crate::error::ModelError;
use crate::template;
use crate::template::ServicePortProtocol;

/// Root of the `Service.*` scope.
pub const SERVICE_SCOPE: &str = "Service";
/// Prefix of a Service's embedded-file symbols: `Service.File.<name>`
/// (Template Schemas §6, §7.3.1), the counterpart of `Task.File` and
/// `Env.File`.
pub const SERVICE_FILE_PREFIX: &str = "Service.File";
/// Root of the `WrappedService.*` scope available in the four
/// `onWrapService*` hooks (Template Schemas §4.3.1).
pub const WRAPPED_SERVICE_SCOPE: &str = "WrappedService";

/// The symbol key `Service.<service>.<port>.port`.
#[must_use]
pub fn service_port_key(service: &str, port: &str) -> String {
    format!("{SERVICE_SCOPE}.{service}.{port}.port")
}

/// The symbol key `Service.<service>.<port>.bindAddress`.
#[must_use]
pub fn service_bind_address_key(service: &str, port: &str) -> String {
    format!("{SERVICE_SCOPE}.{service}.{port}.bindAddress")
}

/// The symbol key `Service.<service>.<port>.connectAddress`.
#[must_use]
pub fn service_connect_address_key(service: &str, port: &str) -> String {
    format!("{SERVICE_SCOPE}.{service}.{port}.connectAddress")
}

/// The symbol key `Service.File.<name>` for a Service's embedded file.
#[must_use]
pub fn service_file_key(file_name: &str) -> String {
    format!("{SERVICE_FILE_PREFIX}.{file_name}")
}

/// The allocated endpoint of one port of a Service (Template Schemas
/// §7.3.1 `Service.<name>.<port>.*`, RFC 0009 "Address forms").
///
/// `bind_address` and `connect_address` are each a hostname, an IPv4
/// literal, or an *unbracketed* IPv6 literal; templates join an address
/// and a port with `join_host_port`, which adds the brackets an IPv6
/// authority needs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceEndpoint {
    /// The port number allocated (or requested via `<ServicePort>.port`)
    /// for this port, in the space of [`protocol`](Self::protocol). The
    /// same number is used for binding and connecting.
    pub port: u16,
    /// The port's `<ServicePort>.protocol` (§9.2 item 3): the protocol the
    /// number was allocated in and the service process binds it with.
    /// Reported as `WrappedService.Protocols[i]`. `TCP` is the default and
    /// omitted from JSON.
    #[serde(default, skip_serializing_if = "ServicePortProtocol::is_default")]
    pub protocol: ServicePortProtocol,
    /// The interface address the service process must bind to so that
    /// entities in the Service's scope can reach it (`0.0.0.0` / `::` for a
    /// distributed scheduler, `127.0.0.1` for a single-host runner).
    pub bind_address: String,
    /// The hostname or IP address entities in the Service's scope use to
    /// reach this port.
    pub connect_address: String,
}

/// The allocated endpoints of every port of one Service, in the Service's
/// port declaration order — the order `WrappedService.PortNames` /
/// `.Ports` / `.BindAddresses` / `.Protocols` are reported in.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceEndpoints {
    /// The Service's `name` — the first component of its `Service.*` keys.
    pub name: String,
    /// `(port name, endpoint)` for each declared port, in declaration order.
    pub ports: Vec<(String, ServiceEndpoint)>,
}

impl ServiceEndpoints {
    /// Pair a Service's name with the endpoint of each of its ports, in
    /// declaration order.
    #[must_use]
    pub fn new(name: impl Into<String>, ports: Vec<(String, ServiceEndpoint)>) -> Self {
        Self {
            name: name.into(),
            ports,
        }
    }
}

/// Seed one Service's `Service.<name>.<port>.port` (int) and
/// `.connectAddress` (string) into `symtab`, plus `.bindAddress` (string)
/// when `include_bind_address` is set.
///
/// `bindAddress` is in scope only within the declaring Service (Template
/// Schemas §9, §7.3.1), so a runtime passes `true` for the Service whose
/// own Session it is seeding and `false` for every other Service in scope.
///
/// # Errors
///
/// A key collides with an existing scalar entry in `symtab` (for example
/// `Service` already bound as a value); this cannot happen on a table
/// seeded only by this crate's builders.
pub fn add_service_symbols(
    symtab: &mut SymbolTable,
    endpoints: &ServiceEndpoints,
    include_bind_address: bool,
) -> Result<(), ModelError> {
    for (port_name, endpoint) in &endpoints.ports {
        symtab.set(
            &service_port_key(&endpoints.name, port_name),
            ExprValue::Int(i64::from(endpoint.port)),
        )?;
        symtab.set(
            &service_connect_address_key(&endpoints.name, port_name),
            ExprValue::String(endpoint.connect_address.clone()),
        )?;
        if include_bind_address {
            symtab.set(
                &service_bind_address_key(&endpoints.name, port_name),
                ExprValue::String(endpoint.bind_address.clone()),
            )?;
        }
    }
    Ok(())
}

/// Build the `Service.*` symbol table for one Session.
///
/// `in_scope` holds every Service whose `port` and `connectAddress` the
/// Session may reference (RFC 0009 "The `Service.*` scope": for a Task
/// Session, the Job Services and the Step's Services; for a Service
/// Session, the Services earlier in the start order). `declaring`, for a
/// Service Session, is the Service whose actions the Session runs: it
/// additionally sees its own `bindAddress`. A Service listed in both is
/// seeded once, with `bindAddress`.
///
/// Layer the result onto the Session's `Param.*` / `Session.*` table (or
/// seed it the same way with [`add_service_symbols`]). The counterpart for
/// `Env.File.*` and `Task.File.*` is the runtime's embedded-file
/// materialization, and [`service_file_key`] spells a Service's own
/// `Service.File.<name>` entries.
///
/// # Errors
///
/// Propagates the key-collision error of [`add_service_symbols`], which
/// cannot occur on a fresh table.
pub fn build_service_symbol_table(
    in_scope: &[ServiceEndpoints],
    declaring: Option<&ServiceEndpoints>,
) -> Result<SymbolTable, ModelError> {
    let mut symtab = SymbolTable::new();
    for endpoints in in_scope {
        if declaring.is_some_and(|d| d.name == endpoints.name) {
            continue;
        }
        add_service_symbols(&mut symtab, endpoints, false)?;
    }
    if let Some(endpoints) = declaring {
        add_service_symbols(&mut symtab, endpoints, true)?;
    }
    Ok(symtab)
}

/// Seed the `WrappedService.*` group for one of the four `onWrapService*`
/// hooks (Template Schemas §4.3.1): `WrappedService.Name` (string), and the
/// four parallel lists `WrappedService.PortNames` (`list[string]`),
/// `WrappedService.Ports` (`list[int]`), `WrappedService.BindAddresses`
/// (`list[string]`) and `WrappedService.Protocols` (`list[string]`, each
/// `"TCP"` or `"UDP"`), index *i* of each describing the Service's *i*-th
/// declared port.
///
/// # Errors
///
/// A key collides with an existing scalar entry in `symtab`, or a list
/// cannot be constructed; neither occurs on a table seeded only by this
/// crate's builders.
pub fn add_wrapped_service_symbols(
    symtab: &mut SymbolTable,
    endpoints: &ServiceEndpoints,
) -> Result<(), ModelError> {
    symtab.set(
        &format!("{WRAPPED_SERVICE_SCOPE}.Name"),
        ExprValue::String(endpoints.name.clone()),
    )?;
    let names = endpoints
        .ports
        .iter()
        .map(|(name, _)| ExprValue::String(name.clone()))
        .collect();
    let ports = endpoints
        .ports
        .iter()
        .map(|(_, e)| ExprValue::Int(i64::from(e.port)))
        .collect();
    let binds = endpoints
        .ports
        .iter()
        .map(|(_, e)| ExprValue::String(e.bind_address.clone()))
        .collect();
    let protocols = endpoints
        .ports
        .iter()
        .map(|(_, e)| ExprValue::String(e.protocol.as_str().to_string()))
        .collect();
    symtab.set(
        &format!("{WRAPPED_SERVICE_SCOPE}.PortNames"),
        ExprValue::make_list(names, ExprType::STRING).map_err(ModelError::Expression)?,
    )?;
    symtab.set(
        &format!("{WRAPPED_SERVICE_SCOPE}.Ports"),
        ExprValue::make_list(ports, ExprType::INT).map_err(ModelError::Expression)?,
    )?;
    symtab.set(
        &format!("{WRAPPED_SERVICE_SCOPE}.BindAddresses"),
        ExprValue::make_list(binds, ExprType::STRING).map_err(ModelError::Expression)?,
    )?;
    symtab.set(
        &format!("{WRAPPED_SERVICE_SCOPE}.Protocols"),
        ExprValue::make_list(protocols, ExprType::STRING).map_err(ModelError::Expression)?,
    )?;
    Ok(())
}

/// The names of the Services that `service` references through
/// `Service.<name>.<port>.*` in its host-resolved fields — `variables`,
/// every action's `command` / `args` / `timeout` / `cancelation`, embedded
/// files' `data`, the `<ServiceScript>.let` bindings, and every field of its
/// `serviceEnvironments` (which have the Service's own scope, so a reference
/// made only there still orders the start) — excluding its own name and
/// `Service.File.*`.
///
/// RFC 0009 ordering constraint 2: no action of a Service Session begins
/// until every Service it references is READY, and Services that do not
/// reference one another may start concurrently. A scheduler derives the
/// start-ordering edges from this set; template validation (pass 8) has
/// already confirmed every reference names a Service earlier in the start
/// order, so the result is acyclic.
///
/// # Examples
///
/// ```
/// # use openjd_model::job::service_symbols::referenced_service_names;
/// # use openjd_model::{create_job, decode_job_template};
/// # use serde_json::json;
/// let template = decode_job_template(json!({
///     "specificationVersion": "jobtemplate-2023-09",
///     "extensions": ["SERVICE", "EXPR"],
///     "name": "J",
///     "jobServices": [
///         {"name": "A", "ports": [{"name": "p"}], "script": {"actions": {"onRun": {"command": "a"}}}},
///         {"name": "B", "ports": [{"name": "p"}], "script": {"actions": {"onRun": {
///             "command": "b", "args": ["{{ Service.A.p.port }}", "{{ Service.B.p.bindAddress }}"]}}}}
///     ],
///     "steps": [{"name": "S", "script": {"actions": {"onRun": {"command": "s"}}}}]
/// }), Some(&["SERVICE", "EXPR"]), &Default::default()).unwrap();
/// let job = create_job(&template, &Default::default(), &template.default_validation_context()).unwrap();
/// let services = job.job_services.as_ref().unwrap();
/// assert!(referenced_service_names(&services[0]).is_empty());
/// assert_eq!(
///     referenced_service_names(&services[1]).into_iter().collect::<Vec<_>>(),
///     vec!["A".to_string()]
/// );
/// ```
#[must_use]
pub fn referenced_service_names(service: &super::Service) -> std::collections::BTreeSet<String> {
    let mut symbols = std::collections::HashSet::new();
    if let Some(vars) = &service.variables {
        for fs in vars.values() {
            symbols.extend(fs.accessed_symbols());
        }
    }
    for action in service.script.actions.iter_actions() {
        symbols.extend(action.command.accessed_symbols());
        for arg in action.args.iter().flatten() {
            symbols.extend(arg.accessed_symbols());
        }
        if let Some(timeout) = &action.timeout {
            symbols.extend(timeout.accessed_symbols());
        }
        match &action.cancelation {
            Some(super::CancelationMode::NotifyThenTerminate {
                notify_period_in_seconds: Some(n),
            }) => symbols.extend(n.accessed_symbols()),
            Some(super::CancelationMode::DeferredMode {
                mode,
                notify_period_in_seconds,
            }) => {
                symbols.extend(mode.accessed_symbols());
                if let Some(n) = notify_period_in_seconds {
                    symbols.extend(n.accessed_symbols());
                }
            }
            _ => {}
        }
    }
    for file in service.script.embedded_files.iter().flatten() {
        if let Some(data) = &file.data {
            symbols.extend(data.accessed_symbols());
        }
    }
    for binding in service.script.let_bindings.iter().flatten() {
        if let Some(eq_pos) = binding.find('=') {
            let expr = binding[eq_pos + 1..].trim();
            if let Ok(parsed) = openjd_expr::eval::ParsedExpression::new(expr) {
                symbols.extend(parsed.accessed_symbols().iter().cloned());
            }
        }
    }
    for env in service.service_environments.iter().flatten() {
        symbols.extend(crate::job::create_job::collect_env_accessed_symbols(env));
    }

    let file_prefix = format!("{SERVICE_FILE_PREFIX}.");
    symbols
        .into_iter()
        .filter(|s| s.starts_with(&format!("{SERVICE_SCOPE}.")) && !s.starts_with(&file_prefix))
        .filter_map(|s| s.split('.').nth(1).map(str::to_string))
        .filter(|name| *name != service.name)
        .collect()
}

// ── Unresolved placeholders (template validation and job creation) ────

/// Seed `Unresolved` placeholders for the `Service.<name>.<port>.*` values
/// of every Service in `in_scope` (`port` as `unresolved[int]`,
/// `connectAddress` as `unresolved[string]`), and for `declaring` — the
/// Service whose own body is being checked — also `bindAddress`. The same
/// key layout as [`build_service_symbol_table`], so a reference that
/// type-checks here resolves at run time.
///
/// A Service in `in_scope` that is also `declaring` is seeded once, with
/// `bindAddress`.
pub(crate) fn add_unresolved_service_symbols<'a>(
    symtab: &mut SymbolTable,
    in_scope: impl IntoIterator<Item = &'a template::Service>,
    declaring: Option<&template::Service>,
) -> Result<(), ModelError> {
    for service in in_scope {
        if declaring.is_some_and(|d| d.name == service.name) {
            continue;
        }
        add_unresolved_service(symtab, service, false)?;
    }
    if let Some(service) = declaring {
        add_unresolved_service(symtab, service, true)?;
    }
    Ok(())
}

fn add_unresolved_service(
    symtab: &mut SymbolTable,
    service: &template::Service,
    include_bind_address: bool,
) -> Result<(), ModelError> {
    for port in &service.ports {
        symtab.set(
            &service_port_key(&service.name, &port.name),
            ExprValue::unresolved(ExprType::INT),
        )?;
        symtab.set(
            &service_connect_address_key(&service.name, &port.name),
            ExprValue::unresolved(ExprType::STRING),
        )?;
        if include_bind_address {
            symtab.set(
                &service_bind_address_key(&service.name, &port.name),
                ExprValue::unresolved(ExprType::STRING),
            )?;
        }
    }
    Ok(())
}

/// Seed `Unresolved(path)` placeholders for `Service.File.<name>` for each
/// embedded file of `service`'s script.
pub(crate) fn add_unresolved_service_file_symbols(
    symtab: &mut SymbolTable,
    service: &template::Service,
) -> Result<(), ModelError> {
    for f in service.script.embedded_files.iter().flatten() {
        symtab.set(
            &service_file_key(&f.name),
            ExprValue::unresolved(ExprType::PATH),
        )?;
    }
    Ok(())
}

/// Seed `Unresolved` placeholders for the `WrappedService.*` group, typed as
/// [`add_wrapped_service_symbols`] binds them.
pub(crate) fn add_unresolved_wrapped_service_symbols(
    symtab: &mut SymbolTable,
) -> Result<(), ModelError> {
    for (name, ty) in [
        ("Name", ExprType::STRING),
        ("PortNames", ExprType::list(ExprType::STRING)),
        ("Ports", ExprType::list(ExprType::INT)),
        ("BindAddresses", ExprType::list(ExprType::STRING)),
        ("Protocols", ExprType::list(ExprType::STRING)),
    ] {
        symtab.set(
            &format!("{WRAPPED_SERVICE_SCOPE}.{name}"),
            ExprValue::unresolved(ty),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoints() -> ServiceEndpoints {
        ServiceEndpoints::new(
            "Cache",
            vec![
                (
                    "main".to_string(),
                    ServiceEndpoint {
                        port: 6379,
                        protocol: ServicePortProtocol::Tcp,
                        bind_address: "0.0.0.0".to_string(),
                        connect_address: "cache.example".to_string(),
                    },
                ),
                (
                    "metrics".to_string(),
                    ServiceEndpoint {
                        port: 9100,
                        protocol: ServicePortProtocol::Udp,
                        bind_address: "::".to_string(),
                        connect_address: "2001:db8::5".to_string(),
                    },
                ),
            ],
        )
    }

    #[test]
    fn keys_are_spelled_per_spec() {
        assert_eq!(service_port_key("S", "p"), "Service.S.p.port");
        assert_eq!(
            service_bind_address_key("S", "p"),
            "Service.S.p.bindAddress"
        );
        assert_eq!(
            service_connect_address_key("S", "p"),
            "Service.S.p.connectAddress"
        );
        assert_eq!(service_file_key("Run"), "Service.File.Run");
    }

    #[test]
    fn task_session_table_omits_bind_address() {
        let st = build_service_symbol_table(&[endpoints()], None).unwrap();
        assert_eq!(
            st.get_value("Service.Cache.main.port"),
            Some(&ExprValue::Int(6379))
        );
        assert_eq!(
            st.get_value("Service.Cache.metrics.connectAddress"),
            Some(&ExprValue::String("2001:db8::5".into()))
        );
        assert!(!st.contains("Service.Cache.main.bindAddress"));
    }

    #[test]
    fn service_session_table_includes_own_bind_address_once() {
        let own = endpoints();
        let earlier = ServiceEndpoints::new(
            "Db",
            vec![(
                "sql".to_string(),
                ServiceEndpoint {
                    port: 5432,
                    protocol: ServicePortProtocol::Tcp,
                    bind_address: "127.0.0.1".to_string(),
                    connect_address: "127.0.0.1".to_string(),
                },
            )],
        );
        // `own` listed in scope as well: seeded once, with bindAddress.
        let st = build_service_symbol_table(&[earlier, own.clone()], Some(&own)).unwrap();
        assert_eq!(
            st.get_value("Service.Cache.main.bindAddress"),
            Some(&ExprValue::String("0.0.0.0".into()))
        );
        assert_eq!(
            st.get_value("Service.Db.sql.port"),
            Some(&ExprValue::Int(5432))
        );
        assert!(!st.contains("Service.Db.sql.bindAddress"));
    }

    #[test]
    fn wrapped_service_lists_are_parallel_and_ordered() {
        let mut st = SymbolTable::new();
        add_wrapped_service_symbols(&mut st, &endpoints()).unwrap();
        assert_eq!(
            st.get_value("WrappedService.Name"),
            Some(&ExprValue::String("Cache".into()))
        );
        let names = st.get_value("WrappedService.PortNames").unwrap();
        assert_eq!(
            names.list_elements().unwrap(),
            vec![
                ExprValue::String("main".into()),
                ExprValue::String("metrics".into())
            ]
        );
        let ports = st.get_value("WrappedService.Ports").unwrap();
        assert_eq!(
            ports.list_elements().unwrap(),
            vec![ExprValue::Int(6379), ExprValue::Int(9100)]
        );
        let binds = st.get_value("WrappedService.BindAddresses").unwrap();
        assert_eq!(
            binds.list_elements().unwrap(),
            vec![
                ExprValue::String("0.0.0.0".into()),
                ExprValue::String("::".into())
            ]
        );
        let protocols = st.get_value("WrappedService.Protocols").unwrap();
        assert_eq!(
            protocols.list_elements().unwrap(),
            vec![
                ExprValue::String("TCP".into()),
                ExprValue::String("UDP".into())
            ]
        );
    }

    #[test]
    fn endpoint_protocol_defaults_to_tcp_and_is_omitted_from_json() {
        let tcp: ServiceEndpoint = serde_json::from_str(
            r#"{"port": 80, "bindAddress": "0.0.0.0", "connectAddress": "h"}"#,
        )
        .unwrap();
        assert_eq!(tcp.protocol, ServicePortProtocol::Tcp);
        assert_eq!(
            serde_json::to_string(&tcp).unwrap(),
            r#"{"port":80,"bindAddress":"0.0.0.0","connectAddress":"h"}"#
        );
        let udp = ServiceEndpoint {
            protocol: ServicePortProtocol::Udp,
            ..tcp
        };
        let json = serde_json::to_string(&udp).unwrap();
        assert!(json.contains(r#""protocol":"UDP""#), "{json}");
        assert_eq!(serde_json::from_str::<ServiceEndpoint>(&json).unwrap(), udp);
    }

    #[test]
    fn referenced_service_names_walks_every_host_resolved_field() {
        use crate::job;
        let fs = |s: &str| openjd_expr::FormatString::new(s).unwrap();
        let action = |cmd: &str, args: &[&str]| job::Action {
            command: fs(cmd),
            args: Some(args.iter().map(|a| fs(a)).collect()),
            timeout: None,
            cancelation: None,
        };
        let mut svc = job::Service {
            name: "Self".into(),
            description: None,
            document: job::Document::JobTemplate,
            host_requirements: None,
            service_environments: Some(vec![job::Environment {
                name: "E".into(),
                description: None,
                run_scope: None,
                script: Some(job::EnvironmentScript {
                    let_bindings: None,
                    actions: job::EnvironmentActions {
                        on_enter: Some(action(
                            "setup",
                            &[
                                "{{ Service.FromEnv.p.port }}",
                                "{{ Service.Self.p.bindAddress }}",
                            ],
                        )),
                        on_wrap_env_enter: None,
                        on_wrap_task_run: None,
                        on_wrap_env_exit: None,
                        on_wrap_service_enter: None,
                        on_wrap_service_run: None,
                        on_wrap_service_readiness_check: None,
                        on_wrap_service_exit: None,
                        on_exit: None,
                    },
                    embedded_files: None,
                }),
                variables: None,
                resolved_symtab: None,
            }]),
            ports: vec![job::ServicePort {
                name: "p".into(),
                port: None,
                protocol: ServicePortProtocol::Tcp,
            }],
            readiness_check: job::ServiceReadinessCheck::Stdout { timeout_seconds: 1 },
            restart_policy: job::ServiceRestartPolicy {
                max_attempts: 0,
                completed_tasks: job::CompletedTasksPolicy::Rerun,
            },
            variables: Some(
                [(
                    "V".to_string(),
                    fs("{{ Service.FromVar.p.connectAddress }}"),
                )]
                .into_iter()
                .collect(),
            ),
            script: job::ServiceScript {
                let_bindings: Some(vec!["x = Service.FromLet.p.port + 1".into()]),
                actions: job::ServiceActions {
                    on_enter: Some(action("{{ Service.FromCmd.p.connectAddress }}", &[])),
                    on_run: action(
                        "run",
                        &[
                            "{{ Service.Self.p.bindAddress }}",
                            "{{ Service.FromArg.p.port }}",
                        ],
                    ),
                    on_readiness_check: None,
                    on_exit: None,
                },
                embedded_files: Some(vec![job::EmbeddedFile {
                    name: "F".into(),
                    file_type: crate::types::FileType::Text,
                    filename: None,
                    data: Some(fs("{{ Service.FromFile.p.port }} {{ Service.File.F }}")),
                    runnable: None,
                    end_of_line: None,
                }]),
            },
            resolved_symtab: None,
        };
        let names: Vec<String> = referenced_service_names(&svc).into_iter().collect();
        assert_eq!(
            names,
            vec!["FromArg", "FromCmd", "FromEnv", "FromFile", "FromLet", "FromVar"]
        );

        // No references at all — including the Service's own name and
        // Service.File.* — is an empty set.
        svc.variables = None;
        svc.service_environments = None;
        svc.script.let_bindings = None;
        svc.script.actions.on_enter = None;
        svc.script.actions.on_run = action("run", &["{{ Service.Self.p.port }}"]);
        svc.script.embedded_files = Some(vec![job::EmbeddedFile {
            name: "F".into(),
            file_type: crate::types::FileType::Text,
            filename: None,
            data: Some(fs("{{ Service.File.F }}")),
            runnable: None,
            end_of_line: None,
        }]);
        assert!(referenced_service_names(&svc).is_empty());
    }

    #[test]
    fn unresolved_placeholders_match_concrete_types() {
        let svc: template::Service = serde_saphyr::from_str(
            "name: Cache\nports: [{name: main}]\nscript:\n  actions: {onRun: {command: x}}\n  embeddedFiles: [{name: Conf, type: TEXT, data: d}]\n",
        )
        .unwrap();
        let mut st = SymbolTable::new();
        add_unresolved_service_symbols(&mut st, [&svc], Some(&svc)).unwrap();
        add_unresolved_service_file_symbols(&mut st, &svc).unwrap();
        add_unresolved_wrapped_service_symbols(&mut st).unwrap();
        assert_eq!(
            st.get_value("Service.Cache.main.port"),
            Some(&ExprValue::unresolved(ExprType::INT))
        );
        assert_eq!(
            st.get_value("Service.Cache.main.bindAddress"),
            Some(&ExprValue::unresolved(ExprType::STRING))
        );
        assert_eq!(
            st.get_value("Service.File.Conf"),
            Some(&ExprValue::unresolved(ExprType::PATH))
        );
        assert_eq!(
            st.get_value("WrappedService.Ports"),
            Some(&ExprValue::unresolved(ExprType::list(ExprType::INT)))
        );
        // Not the declaring Service: no bindAddress.
        let mut other = SymbolTable::new();
        add_unresolved_service_symbols(&mut other, [&svc], None).unwrap();
        assert!(other.contains("Service.Cache.main.connectAddress"));
        assert!(!other.contains("Service.Cache.main.bindAddress"));
    }
}
