// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Endpoint allocation for Services run by `openjd run` (RFC 0009 ordering
//! constraint 1, "Placement").
//!
//! This is the **local runner's** policy, not the specification's: `openjd
//! run` is the scheduler and the only host, so every Service binds and is
//! reached on the loopback interface. For each declared port of a Service
//! the allocator picks a free TCP port on `127.0.0.1` (or honors a
//! `<ServicePort>.port` request), and sets `bindAddress` and
//! `connectAddress` both to `127.0.0.1`. A distributed scheduler would
//! choose differently (a wildcard `bindAddress`, a routable hostname for
//! `connectAddress`).
//!
//! Per RFC 0009 "Address forms", both addresses are *bare* addresses — a
//! hostname, an IPv4 literal, or an unbracketed IPv6 literal — never a
//! `host:port` string and never a bracketed IPv6 literal; templates join an
//! address and a port with `join_host_port`.
//!
//! The allocation table lives for the whole run so that two Services never
//! receive the same port number in one run, even when one of them has
//! already released it: a Task that resolved the old endpoint could
//! otherwise connect to the wrong Service.

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};

use openjd_model::job::service_symbols::{ServiceEndpoint, ServiceEndpoints};
use openjd_model::job::Service;

/// The loopback address every Service of a local run binds to and is
/// reached at.
pub(super) const LOOPBACK: Ipv4Addr = Ipv4Addr::LOCALHOST;

/// Allocates loopback TCP ports for the Services of one run.
#[derive(Debug, Default)]
pub(super) struct PortAllocator {
    /// Every port number handed out during the run (never reused).
    allocated: BTreeSet<u16>,
}

impl PortAllocator {
    /// Allocate an endpoint for every declared port of `service`, in
    /// declaration order.
    ///
    /// A port entry with a requested `port` is bound once to confirm it is
    /// available; one without is assigned a free port by binding
    /// `127.0.0.1:0`. The listener is closed immediately — the Service's
    /// own process binds it next — so another process could in principle
    /// take the port in between; RFC 0009 treats that as an instance failure
    /// (`onRun` exits before READY) that a new Service Session, with new
    /// ports, recovers from.
    ///
    /// # Errors
    ///
    /// A requested port that is already allocated to another Service of this
    /// run, or that cannot be bound on the loopback interface. Both are
    /// *start failures* of the Service (RFC 0009 "Failure and restart").
    pub(super) fn allocate(&mut self, service: &Service) -> Result<ServiceEndpoints, String> {
        let mut ports = Vec::with_capacity(service.ports.len());
        // Allocate into a local set first so a failure part-way through
        // does not leave the earlier ports of this Service marked as taken.
        let mut taken = Vec::new();
        for declared in &service.ports {
            let port = match declared.port {
                Some(requested) => {
                    if self.allocated.contains(&requested) || taken.contains(&requested) {
                        return Err(format!(
                            "Service '{}' port '{}' requests TCP port {requested}, which is already \
                             allocated to another Service of this run",
                            service.name, declared.name
                        ));
                    }
                    bind_loopback(requested).map_err(|e| {
                        format!(
                            "Service '{}' port '{}' requests TCP port {requested}, which is not \
                             available on {LOOPBACK}: {e}",
                            service.name, declared.name
                        )
                    })?
                }
                None => {
                    let mut candidate = bind_loopback(0).map_err(|e| {
                        format!(
                            "Service '{}' port '{}': failed to allocate a free TCP port on \
                             {LOOPBACK}: {e}",
                            service.name, declared.name
                        )
                    })?;
                    // The OS hands out ephemeral ports it believes free; it
                    // may return one this run already allocated and
                    // released. Try again until the number is new.
                    let mut tries = 0;
                    while self.allocated.contains(&candidate) || taken.contains(&candidate) {
                        tries += 1;
                        if tries > 64 {
                            return Err(format!(
                                "Service '{}' port '{}': could not find a free TCP port on \
                                 {LOOPBACK} not yet used by this run",
                                service.name, declared.name
                            ));
                        }
                        candidate = bind_loopback(0).map_err(|e| {
                            format!(
                                "Service '{}' port '{}': failed to allocate a free TCP port on \
                                 {LOOPBACK}: {e}",
                                service.name, declared.name
                            )
                        })?;
                    }
                    candidate
                }
            };
            taken.push(port);
            ports.push((
                declared.name.clone(),
                ServiceEndpoint {
                    port,
                    bind_address: LOOPBACK.to_string(),
                    connect_address: LOOPBACK.to_string(),
                },
            ));
        }
        self.allocated.extend(taken);
        Ok(ServiceEndpoints::new(service.name.clone(), ports))
    }
}

/// Bind `127.0.0.1:port` (port 0 = any free port), return the bound port,
/// and release the listener.
fn bind_loopback(port: u16) -> std::io::Result<u16> {
    let listener = TcpListener::bind(SocketAddrV4::new(LOOPBACK, port))?;
    Ok(listener.local_addr()?.port())
}

/// `name → 127.0.0.1:port, …` for log lines.
pub(super) fn describe_endpoints(endpoints: &ServiceEndpoints) -> String {
    endpoints
        .ports
        .iter()
        .map(|(name, e)| format!("{name} -> {}:{}", e.connect_address, e.port))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use openjd_model::job::{
        Action, CompletedTasksPolicy, ServiceActions, ServicePort, ServiceReadinessCheck,
        ServiceRestartPolicy, ServiceScript,
    };

    fn service(name: &str, ports: &[(&str, Option<u16>)]) -> Service {
        Service {
            name: name.into(),
            description: None,
            host_requirements: None,
            ports: ports
                .iter()
                .map(|(n, p)| ServicePort {
                    name: (*n).into(),
                    port: *p,
                })
                .collect(),
            readiness_check: ServiceReadinessCheck::Stdout { timeout_seconds: 1 },
            restart_policy: ServiceRestartPolicy {
                max_attempts: 0,
                completed_tasks: CompletedTasksPolicy::Rerun,
            },
            variables: None,
            script: ServiceScript {
                let_bindings: None,
                actions: ServiceActions {
                    on_enter: None,
                    on_run: Action {
                        command: openjd_expr::FormatString::new("x").unwrap(),
                        args: None,
                        timeout: None,
                        cancelation: None,
                    },
                    on_readiness_check: None,
                    on_exit: None,
                },
                embedded_files: None,
            },
            resolved_symtab: None,
        }
    }

    #[test]
    fn allocates_distinct_loopback_ports_in_declaration_order() {
        let mut alloc = PortAllocator::default();
        let a = alloc
            .allocate(&service("A", &[("main", None), ("metrics", None)]))
            .unwrap();
        let b = alloc.allocate(&service("B", &[("main", None)])).unwrap();
        assert_eq!(a.name, "A");
        assert_eq!(a.ports[0].0, "main");
        assert_eq!(a.ports[1].0, "metrics");
        let mut all: Vec<u16> = a
            .ports
            .iter()
            .chain(b.ports.iter())
            .map(|(_, e)| e.port)
            .collect();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 3, "ports must be distinct within a run");
        for (_, e) in a.ports.iter().chain(b.ports.iter()) {
            assert_eq!(e.bind_address, "127.0.0.1");
            assert_eq!(e.connect_address, "127.0.0.1");
            assert_ne!(e.port, 0);
        }
    }

    #[test]
    fn honors_a_requested_port_and_rejects_a_duplicate_request() {
        let mut alloc = PortAllocator::default();
        let free = bind_loopback(0).unwrap();
        let a = alloc
            .allocate(&service("A", &[("main", Some(free))]))
            .unwrap();
        assert_eq!(a.ports[0].1.port, free);
        let err = alloc
            .allocate(&service("B", &[("main", Some(free))]))
            .unwrap_err();
        assert_eq!(
            err,
            format!(
                "Service 'B' port 'main' requests TCP port {free}, which is already allocated to \
                 another Service of this run"
            )
        );
    }

    #[test]
    fn a_requested_port_held_by_another_process_is_a_start_failure() {
        let mut alloc = PortAllocator::default();
        let holder = TcpListener::bind(SocketAddrV4::new(LOOPBACK, 0)).unwrap();
        let held = holder.local_addr().unwrap().port();
        let err = alloc
            .allocate(&service("A", &[("main", Some(held))]))
            .unwrap_err();
        assert!(
            err.starts_with(&format!(
                "Service 'A' port 'main' requests TCP port {held}, which is not available on \
                 127.0.0.1: "
            )),
            "{err}"
        );
        // Nothing was recorded for the failed Service.
        assert!(alloc.allocated.is_empty());
    }

    #[test]
    fn describes_endpoints_for_logs() {
        let e = ServiceEndpoints::new(
            "A",
            vec![
                (
                    "main".into(),
                    ServiceEndpoint {
                        port: 1234,
                        bind_address: "127.0.0.1".into(),
                        connect_address: "127.0.0.1".into(),
                    },
                ),
                (
                    "metrics".into(),
                    ServiceEndpoint {
                        port: 1235,
                        bind_address: "127.0.0.1".into(),
                        connect_address: "127.0.0.1".into(),
                    },
                ),
            ],
        );
        assert_eq!(
            describe_endpoints(&e),
            "main -> 127.0.0.1:1234, metrics -> 127.0.0.1:1235"
        );
    }
}
