// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Service types per spec §9 (`SERVICE` extension, RFC 0009).
//!
//! A Service is a long-lived process with named ports that a scheduler
//! starts before any Task of a Step in its *scope* is scheduled and keeps
//! running until no such Task remains. A Job Template declares its Services
//! in one `services` list; each Service's scope — the set of Steps whose
//! Tasks depend on it — is computed from the template's `Service.*`
//! references (§9.1, [`crate::template::service_scope`]). A Job Template
//! that reads an external Service's endpoint declares a
//! [`ServiceRequirement`] in `requiresServices` (§9.8). The types here are
//! the unresolved template shapes; the `Service.*` format-string scope and
//! job creation of Services are implemented separately.

use super::actions::Action;
use super::constrained_strings::Description;
use super::environment::EmbeddedFile;
use super::host_requirements::HostRequirements;
use super::step::StepDependency;
use crate::format_string::FormatString;
use serde::Deserialize;
use std::collections::HashMap;

/// §9 `<Service>` — a long-lived process with named ports, available with
/// the `SERVICE` extension.
///
/// `name` is a `<ServiceName>` (§9.2): an identifier that is not `File`.
/// It is held as a plain `String` so that the identifier constraints are
/// reported with a field path by template validation rather than as a
/// serde error.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Service {
    /// §9.2 `<ServiceName>`: the first component of `Service.<name>.*`
    /// references. Unique within its list and, in a Job Template, distinct
    /// from every `requiresServices` name.
    pub name: String,
    pub description: Option<Description>,
    /// Expression bindings evaluated once at job creation (EXPR). Not in
    /// scope: `Session.*`, `Service.*`.
    #[serde(rename = "let")]
    pub let_bindings: Option<Vec<String>>,
    /// §9 item 4 — Steps that must complete before the Service is started,
    /// with the shape of a Step's `dependencies`. At least one element when
    /// given; each names a Step of the same Job Template that is not in the
    /// Service's own scope; not permitted on an Environment Template's
    /// Services.
    pub dependencies: Option<Vec<StepDependency>>,
    /// Requirements the service host must satisfy. Independent of the
    /// `hostRequirements` of any Step whose Tasks use the Service.
    pub host_requirements: Option<HostRequirements>,
    /// §9.3 The named ports the Service exposes: 1–10 entries with unique
    /// names, each TCP (the default) or UDP. No two ports of the same
    /// protocol may have the same `port` number (§9 item 5.4).
    pub ports: Vec<ServicePort>,
    /// §9.4 How readiness and, after READY, health are determined. `None`
    /// means `{ type: TCP_CONNECT }` on every declared TCP port; a Service
    /// none of whose ports is TCP must declare a `STDOUT` or `COMMAND`
    /// check (§9 item 6). See [`health_check`](Self::health_check).
    pub health_check: Option<ServiceHealthCheck>,
    /// §9.5 What happens when `onRun` exits before the scope ends. `None`
    /// means `{ maxAttempts: 0 }`: never relaunched, and the exit fails the
    /// scope; see [`restart_policy`](Self::restart_policy).
    pub restart_policy: Option<ServiceRestartPolicy>,
    /// Environment variables set for every action of the Service's script
    /// (same schema as `<Environment>.variables`). Not propagated to the
    /// entities in the Service's scope.
    pub variables: Option<HashMap<String, FormatString>>,
    /// §9.6 The actions the Service runs on its host.
    pub script: ServiceScript,
}

impl Service {
    /// The effective health check: the declared one, or the §9 default
    /// `{ type: TCP_CONNECT }` applied to every declared TCP port.
    pub fn health_check(&self) -> ServiceHealthCheck {
        self.health_check.clone().unwrap_or_default()
    }

    /// The effective restart policy: the declared one, or the §9 default
    /// `{ maxAttempts: 0 }` (no `completedTasks`, which a Service never
    /// relaunched does not need).
    pub fn restart_policy(&self) -> ServiceRestartPolicy {
        self.restart_policy.clone().unwrap_or_default()
    }

    /// The names of the declared ports, in declaration order.
    pub fn port_names(&self) -> impl Iterator<Item = &str> {
        self.ports.iter().map(|p| p.name.as_str())
    }

    /// The names of the declared ports whose `protocol` is `TCP`, in
    /// declaration order — the ports a `TCP_CONNECT` health check
    /// probes when it names none (§9 item 6, §9.4 item 2).
    pub fn tcp_port_names(&self) -> impl Iterator<Item = &str> {
        self.ports
            .iter()
            .filter(|p| p.protocol == ServicePortProtocol::Tcp)
            .map(|p| p.name.as_str())
    }
}

/// §9.3 `<ServicePort>` — one named port of a Service.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServicePort {
    /// The second component of `Service.<service>.<port>.*` references. An
    /// identifier other than `File`, unique within the Service.
    pub name: String,
    /// `<posinteger> | <posintstring>` (`@fmtstring`): a specific port
    /// number (1–65535), in the space of the port's `protocol`, that the
    /// Service requires on its host. When absent the runtime allocates a
    /// port of that protocol. Modeled like `<Action>.timeout`: an integer
    /// is accepted and held as its decimal text; a format string is
    /// resolved at job creation.
    pub port: Option<FormatString>,
    /// §9.3 item 3: the transport protocol the service process binds the
    /// port with and the scheduler publishes or forwards it as. Not a
    /// format string. Default `TCP`.
    #[serde(default)]
    pub protocol: ServicePortProtocol,
}

/// §9.3 item 3 `<ServicePort>.protocol` — the transport protocol of one
/// port. TCP and UDP port numbers are separate spaces: a number is
/// requested or allocated in the space of this protocol, and two ports may
/// share a number when their protocols differ.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize, serde::Serialize,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ServicePortProtocol {
    /// A TCP port; the only kind a `TCP_CONNECT` health check can
    /// probe. The default.
    #[default]
    Tcp,
    /// A UDP port. It cannot be probed by `TCP_CONNECT`.
    Udp,
}

impl ServicePortProtocol {
    /// The schema spelling of this value (`"TCP"` / `"UDP"`), also the
    /// value reported in `WrappedService.Protocols`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Tcp => "TCP",
            Self::Udp => "UDP",
        }
    }

    /// Whether this is the default protocol, for
    /// `#[serde(skip_serializing_if)]` on a serialized Job.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl std::fmt::Display for ServicePortProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// §9.8 `<ServiceRequirement>` — one entry of a Job Template's
/// `requiresServices`: the Job Template reads the endpoint of an external
/// Service named `name`, declared by an attached Environment Template, on
/// the ports listed.
///
/// A requirement puts `Service.<name>.<port>.port` and
/// `Service.<name>.<port>.connectAddress` in scope throughout the Job
/// Template for each declared port (never `bindAddress`), and at submission
/// the scheduler matches it to exactly one attached Service with that
/// `name` declaring every listed port with the same `protocol`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceRequirement {
    /// §9.8 item 1: the `name` of the external Service required — a
    /// `<ServiceName>` that no inline Service of the same Job Template
    /// bears.
    pub name: String,
    /// §9.8 item 2: the ports of the Service the Job Template uses. 1–10
    /// entries with unique names.
    pub ports: Vec<ServiceRequirementPort>,
}

impl ServiceRequirement {
    /// The names of the declared ports, in declaration order.
    pub fn port_names(&self) -> impl Iterator<Item = &str> {
        self.ports.iter().map(|p| p.name.as_str())
    }
}

/// §9.8.1 `<ServiceRequirementPort>` — one port a requirement declares.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceRequirementPort {
    /// The `name` of a port the external Service declares; the second
    /// component of `Service.<service>.<port>.*` references. An identifier
    /// other than `File`.
    pub name: String,
    /// The protocol the Job Template expects the port to carry; the matched
    /// Service's port must declare the same. Default `TCP`.
    #[serde(default)]
    pub protocol: ServicePortProtocol,
}

/// §9.4 `<ServiceHealthCheck>` — discriminated union on `type`.
///
/// One probe mechanism applied in two phases. Before the instance is READY
/// the probe decides readiness: the first probe runs as soon as `onRun` is
/// launched, one every `readinessIntervalSeconds` after it, and the first
/// success makes the instance READY; `readinessTimeoutSeconds`, measured from
/// the launch of `onRun`, bounds the phase. After READY the probe decides
/// health: one every `healthIntervalSeconds`, and `failureThreshold`
/// consecutive failures make the instance UNHEALTHY — an instance failure.
///
/// The numeric fields are `<posinteger> | <posintstring>` (`@fmtstring`),
/// modeled like `<Action>.timeout`; a format string is resolved at job
/// creation. The field set differs by `type`: `STDOUT` has no
/// `readinessIntervalSeconds` (the ready line arrives when it arrives) and
/// no default `healthIntervalSeconds` (the heartbeat is opt-in), so a
/// `STDOUT` check that gives `readinessIntervalSeconds` is rejected as an
/// unknown field, like `ports` on anything but `TCP_CONNECT`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum ServiceHealthCheck {
    /// A probe succeeds when a TCP connection to each listed port succeeds.
    /// `ports` may name only TCP ports and defaults to every declared TCP
    /// port.
    #[serde(rename = "TCP_CONNECT")]
    TcpConnect {
        ports: Option<Vec<String>>,
        /// Default [`DEFAULT_TCP_CONNECT_READINESS_INTERVAL_SECONDS`](Self::DEFAULT_TCP_CONNECT_READINESS_INTERVAL_SECONDS).
        readiness_interval_seconds: Option<FormatString>,
        /// Default [`DEFAULT_READY_TIMEOUT_SECONDS`](Self::DEFAULT_READY_TIMEOUT_SECONDS).
        readiness_timeout_seconds: Option<FormatString>,
        /// Default [`DEFAULT_HEALTH_INTERVAL_SECONDS`](Self::DEFAULT_HEALTH_INTERVAL_SECONDS).
        health_interval_seconds: Option<FormatString>,
        /// Default [`DEFAULT_FAILURE_THRESHOLD`](Self::DEFAULT_FAILURE_THRESHOLD).
        failure_threshold: Option<FormatString>,
    },
    /// A probe is one invocation of `<ServiceActions>.onHealthCheck`, and
    /// succeeds when it exits 0 while `onRun` is running. Each interval
    /// separates the end of one invocation from the start of the next.
    #[serde(rename = "COMMAND")]
    Command {
        /// Default [`DEFAULT_COMMAND_READINESS_INTERVAL_SECONDS`](Self::DEFAULT_COMMAND_READINESS_INTERVAL_SECONDS).
        readiness_interval_seconds: Option<FormatString>,
        /// Default [`DEFAULT_READY_TIMEOUT_SECONDS`](Self::DEFAULT_READY_TIMEOUT_SECONDS).
        readiness_timeout_seconds: Option<FormatString>,
        /// Default [`DEFAULT_HEALTH_INTERVAL_SECONDS`](Self::DEFAULT_HEALTH_INTERVAL_SECONDS).
        health_interval_seconds: Option<FormatString>,
        /// Default [`DEFAULT_FAILURE_THRESHOLD`](Self::DEFAULT_FAILURE_THRESHOLD).
        failure_threshold: Option<FormatString>,
    },
    /// A probe is an `openjd_service_ready: <message>` line on `onRun`'s
    /// stdout. The first makes the instance READY; after READY the line is
    /// a heartbeat only when `healthIntervalSeconds` is given (no default),
    /// each interval without one being a failed probe. `failureThreshold`
    /// is permitted only together with `healthIntervalSeconds` (§9.4 item 6).
    #[serde(rename = "STDOUT")]
    Stdout {
        /// Default [`DEFAULT_READY_TIMEOUT_SECONDS`](Self::DEFAULT_READY_TIMEOUT_SECONDS).
        readiness_timeout_seconds: Option<FormatString>,
        /// No default: `None` means no heartbeat is expected.
        health_interval_seconds: Option<FormatString>,
        /// Default [`DEFAULT_FAILURE_THRESHOLD`](Self::DEFAULT_FAILURE_THRESHOLD);
        /// meaningful only with `health_interval_seconds`.
        failure_threshold: Option<FormatString>,
    },
}

/// The schema names of the four numeric `<ServiceHealthCheck>` fields, as
/// [`ServiceHealthCheck::numeric_fields`] orders them.
pub const SERVICE_HEALTH_CHECK_NUMERIC_FIELDS: [&str; 4] = [
    "readinessIntervalSeconds",
    "readinessTimeoutSeconds",
    "healthIntervalSeconds",
    "failureThreshold",
];

impl ServiceHealthCheck {
    /// §9.4 item 3 default for `readinessIntervalSeconds` on a `TCP_CONNECT`
    /// check, in seconds.
    pub const DEFAULT_TCP_CONNECT_READINESS_INTERVAL_SECONDS: u64 = 1;
    /// §9.4 item 3 default for `readinessIntervalSeconds` on a `COMMAND`
    /// check, in seconds.
    pub const DEFAULT_COMMAND_READINESS_INTERVAL_SECONDS: u64 = 5;
    /// §9.4 item 4 default for `readinessTimeoutSeconds`, in seconds.
    pub const DEFAULT_READY_TIMEOUT_SECONDS: u64 = 300;
    /// §9.4 item 5 default for `healthIntervalSeconds` on a `TCP_CONNECT` or
    /// `COMMAND` check, in seconds. `STDOUT` has no default.
    pub const DEFAULT_HEALTH_INTERVAL_SECONDS: u64 = 30;
    /// §9.4 item 6 default for `failureThreshold`.
    pub const DEFAULT_FAILURE_THRESHOLD: u64 = 3;

    /// The schema value of the `type` discriminator.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::TcpConnect { .. } => "TCP_CONNECT",
            Self::Command { .. } => "COMMAND",
            Self::Stdout { .. } => "STDOUT",
        }
    }

    /// The `readinessIntervalSeconds` field; always `None` for `STDOUT`,
    /// which has no such field.
    pub fn readiness_interval_seconds(&self) -> Option<&FormatString> {
        match self {
            Self::TcpConnect {
                readiness_interval_seconds,
                ..
            }
            | Self::Command {
                readiness_interval_seconds,
                ..
            } => readiness_interval_seconds.as_ref(),
            Self::Stdout { .. } => None,
        }
    }

    /// The `readinessTimeoutSeconds` field, whichever variant this is.
    pub fn readiness_timeout_seconds(&self) -> Option<&FormatString> {
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
            } => readiness_timeout_seconds.as_ref(),
        }
    }

    /// The `healthIntervalSeconds` field, whichever variant this is.
    pub fn health_interval_seconds(&self) -> Option<&FormatString> {
        match self {
            Self::TcpConnect {
                health_interval_seconds,
                ..
            }
            | Self::Command {
                health_interval_seconds,
                ..
            }
            | Self::Stdout {
                health_interval_seconds,
                ..
            } => health_interval_seconds.as_ref(),
        }
    }

    /// The `failureThreshold` field, whichever variant this is.
    pub fn failure_threshold(&self) -> Option<&FormatString> {
        match self {
            Self::TcpConnect {
                failure_threshold, ..
            }
            | Self::Command {
                failure_threshold, ..
            }
            | Self::Stdout {
                failure_threshold, ..
            } => failure_threshold.as_ref(),
        }
    }

    /// The four numeric `@fmtstring` fields paired with their schema names
    /// ([`SERVICE_HEALTH_CHECK_NUMERIC_FIELDS`] order), each `None` when
    /// not given — or, for `readinessIntervalSeconds` on `STDOUT`, not a
    /// field at all. Every numeric field is a `<posinteger>`, so validators
    /// and job creation treat the four alike.
    pub fn numeric_fields(&self) -> [(&'static str, Option<&FormatString>); 4] {
        [
            (
                SERVICE_HEALTH_CHECK_NUMERIC_FIELDS[0],
                self.readiness_interval_seconds(),
            ),
            (
                SERVICE_HEALTH_CHECK_NUMERIC_FIELDS[1],
                self.readiness_timeout_seconds(),
            ),
            (
                SERVICE_HEALTH_CHECK_NUMERIC_FIELDS[2],
                self.health_interval_seconds(),
            ),
            (
                SERVICE_HEALTH_CHECK_NUMERIC_FIELDS[3],
                self.failure_threshold(),
            ),
        ]
    }
}

impl Default for ServiceHealthCheck {
    /// `{ type: TCP_CONNECT }` on every declared TCP port, with every
    /// default.
    fn default() -> Self {
        Self::TcpConnect {
            ports: None,
            readiness_interval_seconds: None,
            readiness_timeout_seconds: None,
            health_interval_seconds: None,
            failure_threshold: None,
        }
    }
}

/// §9.5 `<ServiceRestartPolicy>`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceRestartPolicy {
    /// `<integer> | <intstring>` (`@fmtstring`), `>= 0`: how many times the
    /// scheduler relaunches the Service after a failure. The initial launch
    /// is not counted. Default
    /// [`DEFAULT_MAX_ATTEMPTS`](Self::DEFAULT_MAX_ATTEMPTS). Modeled like
    /// `<Action>.timeout`; a format string is resolved at job creation.
    pub max_attempts: Option<FormatString>,
    /// What happens to completed and running Tasks when a new instance is
    /// launched. No default: required when `maxAttempts` is greater than 0
    /// (§9.5 item 2, §9.9 item 12 — checked at template validation for a
    /// literal `maxAttempts`, at job creation for a format string); may be
    /// omitted when `maxAttempts` is 0, in which case it matters only when
    /// the Service is stopped and started again because a Service it lists
    /// began a new Service Session, where an omitted value is read as
    /// `RERUN` (see `job::ServiceRestartPolicy`).
    pub completed_tasks: Option<CompletedTasksPolicy>,
}

impl ServiceRestartPolicy {
    /// §9.5 default for `maxAttempts`: launched exactly once, never
    /// relaunched.
    pub const DEFAULT_MAX_ATTEMPTS: i64 = 0;
}

/// §9.5 `completedTasks` — what a Service restart does to the Tasks in its
/// scope that completed against, or were running against, the previous
/// instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, serde::Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CompletedTasksPolicy {
    /// Completed Tasks remain complete and running Tasks continue. Also
    /// declares the Service resumable (it may be suspended while idle).
    Keep,
    /// Completed Tasks are requeued and running Tasks are canceled and
    /// requeued without counting as a Task failure.
    Rerun,
}

impl CompletedTasksPolicy {
    /// The schema spelling of this value.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Keep => "KEEP",
            Self::Rerun => "RERUN",
        }
    }
}

/// §9.6 `<ServiceScript>`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceScript {
    /// Expression bindings evaluated on the service host when the Service
    /// is started (EXPR); the host-context counterpart of `<Service>.let`.
    #[serde(rename = "let")]
    pub let_bindings: Option<Vec<String>>,
    pub actions: ServiceActions,
    /// Materialized to the Service Session's working directory before each
    /// action runs; reachable as `Service.File.<name>`.
    pub embedded_files: Option<Vec<EmbeddedFile>>,
}

/// §9.7 `<ServiceActions>`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceActions {
    /// One-time setup, run once per Service Session before the first
    /// `onRun`. No default timeout.
    pub on_enter: Option<Action>,
    /// The long-lived action whose process *is* the service. Any exit
    /// before the scheduler cancels it is an instance failure. No default
    /// timeout.
    pub on_run: Action,
    /// Run repeatedly while `onRun` runs when `healthCheck.type` is
    /// `COMMAND`: before READY to decide whether the instance is ready,
    /// afterwards to decide whether it still is; exit 0 is a successful
    /// probe. Must be defined if and only if the health check type is
    /// `COMMAND`. Default timeout
    /// [`ON_HEALTH_CHECK_DEFAULT_TIMEOUT_SECONDS`](Self::ON_HEALTH_CHECK_DEFAULT_TIMEOUT_SECONDS).
    pub on_health_check: Option<Action>,
    /// Cleanup run after the other actions have stopped for the last time
    /// in a Service Session. Default timeout
    /// [`ON_EXIT_DEFAULT_TIMEOUT_SECONDS`](Self::ON_EXIT_DEFAULT_TIMEOUT_SECONDS).
    pub on_exit: Option<Action>,
}

impl ServiceActions {
    /// RFC 0009 default `timeout` for `onHealthCheck`, in seconds; bounds
    /// one invocation.
    pub const ON_HEALTH_CHECK_DEFAULT_TIMEOUT_SECONDS: u64 = 30;
    /// RFC 0009 default `timeout` for `onExit`, in seconds (five minutes,
    /// as for an Environment's `onExit`).
    pub const ON_EXIT_DEFAULT_TIMEOUT_SECONDS: u64 = 300;

    /// The RFC 0009 default `timeout` of the named action when the template
    /// gives none: `onEnter` and `onRun` have no default (`None`),
    /// `onHealthCheck` 30 seconds, `onExit` 300 seconds. Returns `None`
    /// for a name that is not a `<ServiceActions>` slot.
    pub fn default_timeout_seconds(action_name: &str) -> Option<u64> {
        match action_name {
            "onHealthCheck" => Some(Self::ON_HEALTH_CHECK_DEFAULT_TIMEOUT_SECONDS),
            "onExit" => Some(Self::ON_EXIT_DEFAULT_TIMEOUT_SECONDS),
            _ => None,
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> Service {
        serde_saphyr::from_str(yaml).unwrap()
    }

    const MINIMAL: &str = r#"
name: Cache
ports:
  - name: main
script:
  actions:
    onRun:
      command: valkey-server
"#;

    #[test]
    fn minimal_service_defaults() {
        let svc = parse(MINIMAL);
        assert_eq!(svc.name, "Cache");
        assert!(svc.health_check.is_none());
        assert!(matches!(
            svc.health_check(),
            ServiceHealthCheck::TcpConnect {
                ports: None,
                readiness_interval_seconds: None,
                readiness_timeout_seconds: None,
                health_interval_seconds: None,
                failure_threshold: None,
            }
        ));
        let policy = svc.restart_policy();
        assert!(policy.max_attempts.is_none());
        assert!(policy.completed_tasks.is_none());
        assert_eq!(svc.port_names().collect::<Vec<_>>(), vec!["main"]);
        assert_eq!(svc.ports[0].port, None);
        assert_eq!(svc.ports[0].protocol, ServicePortProtocol::Tcp);
    }

    #[test]
    fn port_protocol_parses_and_defaults_to_tcp() {
        let svc = parse(
            r#"
name: Metrics
ports:
  - name: ingest
    protocol: UDP
  - name: api
  - name: admin
    protocol: TCP
healthCheck:
  type: STDOUT
script:
  actions:
    onRun:
      command: sink
"#,
        );
        let protocols: Vec<ServicePortProtocol> = svc.ports.iter().map(|p| p.protocol).collect();
        assert_eq!(
            protocols,
            vec![
                ServicePortProtocol::Udp,
                ServicePortProtocol::Tcp,
                ServicePortProtocol::Tcp
            ]
        );
        assert_eq!(
            svc.tcp_port_names().collect::<Vec<_>>(),
            vec!["api", "admin"]
        );
        assert_eq!(ServicePortProtocol::default(), ServicePortProtocol::Tcp);
        assert_eq!(ServicePortProtocol::Tcp.as_str(), "TCP");
        assert_eq!(ServicePortProtocol::Udp.as_str(), "UDP");
        assert_eq!(ServicePortProtocol::Udp.to_string(), "UDP");
        assert!(ServicePortProtocol::Tcp.is_default());
        assert!(!ServicePortProtocol::Udp.is_default());
    }

    #[test]
    fn port_protocol_is_a_case_sensitive_literal() {
        for bad in ["udp", "SCTP", "{{ Param.Proto }}"] {
            let err = serde_saphyr::from_str::<ServicePort>(&format!(
                "name: ingest\nprotocol: \"{bad}\""
            ))
            .unwrap_err();
            assert!(
                err.to_string()
                    .contains(&format!("unknown variant `{bad}`")),
                "got: {err}"
            );
        }
    }

    #[test]
    fn numeric_fields_accept_integer_or_format_string() {
        let svc = parse(
            r#"
name: Cache
ports:
  - name: main
    port: 6379
  - name: other
    port: "{{ Param.Port }}"
healthCheck:
  type: COMMAND
  readinessIntervalSeconds: "{{ Param.Interval }}"
  readinessTimeoutSeconds: 60
  healthIntervalSeconds: 10
  failureThreshold: "{{ Param.Strikes }}"
restartPolicy:
  maxAttempts: "{{ Param.Attempts }}"
  completedTasks: KEEP
script:
  actions:
    onRun:
      command: valkey-server
    onHealthCheck:
      command: valkey-cli
"#,
        );
        assert_eq!(svc.ports[0].port.as_ref().unwrap().raw(), "6379");
        assert_eq!(
            svc.ports[1].port.as_ref().unwrap().raw(),
            "{{ Param.Port }}"
        );
        let check = svc.health_check.as_ref().unwrap();
        match check {
            ServiceHealthCheck::Command {
                readiness_interval_seconds,
                readiness_timeout_seconds,
                health_interval_seconds,
                failure_threshold,
            } => {
                assert_eq!(
                    readiness_interval_seconds.as_ref().unwrap().raw(),
                    "{{ Param.Interval }}"
                );
                assert_eq!(readiness_timeout_seconds.as_ref().unwrap().raw(), "60");
                assert_eq!(health_interval_seconds.as_ref().unwrap().raw(), "10");
                assert_eq!(
                    failure_threshold.as_ref().unwrap().raw(),
                    "{{ Param.Strikes }}"
                );
            }
            other => panic!("unexpected health check {other:?}"),
        }
        let raws: Vec<(&str, Option<&str>)> = check
            .numeric_fields()
            .iter()
            .map(|(name, fs)| (*name, fs.map(|f| f.raw())))
            .collect();
        assert_eq!(
            raws,
            vec![
                ("readinessIntervalSeconds", Some("{{ Param.Interval }}")),
                ("readinessTimeoutSeconds", Some("60")),
                ("healthIntervalSeconds", Some("10")),
                ("failureThreshold", Some("{{ Param.Strikes }}")),
            ]
        );
        let policy = svc.restart_policy.as_ref().unwrap();
        assert_eq!(
            policy.max_attempts.as_ref().unwrap().raw(),
            "{{ Param.Attempts }}"
        );
        assert_eq!(policy.completed_tasks, Some(CompletedTasksPolicy::Keep));
    }

    #[test]
    fn health_check_type_names_and_field_accessors() {
        let tcp: ServiceHealthCheck = serde_saphyr::from_str(
            "type: TCP_CONNECT\nports: [main]\nreadinessIntervalSeconds: 2\nreadinessTimeoutSeconds: 10",
        )
        .unwrap();
        assert_eq!(tcp.type_name(), "TCP_CONNECT");
        assert_eq!(tcp.readiness_interval_seconds().unwrap().raw(), "2");
        assert_eq!(tcp.readiness_timeout_seconds().unwrap().raw(), "10");
        assert!(tcp.health_interval_seconds().is_none());
        assert!(tcp.failure_threshold().is_none());
        let stdout: ServiceHealthCheck =
            serde_saphyr::from_str("type: STDOUT\nhealthIntervalSeconds: 15\nfailureThreshold: 2")
                .unwrap();
        assert_eq!(stdout.type_name(), "STDOUT");
        assert!(stdout.readiness_interval_seconds().is_none());
        assert!(stdout.readiness_timeout_seconds().is_none());
        assert_eq!(stdout.health_interval_seconds().unwrap().raw(), "15");
        assert_eq!(stdout.failure_threshold().unwrap().raw(), "2");
        assert_eq!(
            stdout
                .numeric_fields()
                .iter()
                .map(|(n, v)| (*n, v.is_some()))
                .collect::<Vec<_>>(),
            vec![
                ("readinessIntervalSeconds", false),
                ("readinessTimeoutSeconds", false),
                ("healthIntervalSeconds", true),
                ("failureThreshold", true),
            ]
        );
        let cmd: ServiceHealthCheck = serde_saphyr::from_str("type: COMMAND").unwrap();
        assert_eq!(cmd.type_name(), "COMMAND");
        assert!(cmd.numeric_fields().iter().all(|(_, v)| v.is_none()));
    }

    #[test]
    fn health_check_rejects_unknown_type_and_foreign_fields() {
        let err = serde_saphyr::from_str::<ServiceHealthCheck>("type: HTTP").unwrap_err();
        assert!(
            err.to_string().contains("unknown variant `HTTP`"),
            "got: {err}"
        );
        // `ports` belongs to TCP_CONNECT only.
        let err = serde_saphyr::from_str::<ServiceHealthCheck>("type: STDOUT\nports: [main]")
            .unwrap_err();
        assert!(
            err.to_string().contains("unknown field `ports`"),
            "got: {err}"
        );
        // §9.4 item 3: `readinessIntervalSeconds` does not apply to STDOUT.
        let err = serde_saphyr::from_str::<ServiceHealthCheck>(
            "type: STDOUT\nreadinessIntervalSeconds: 1",
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("unknown field `readinessIntervalSeconds`"),
            "got: {err}"
        );
        // The pre-RFC field names are not properties of any form.
        for (ty, old) in [
            ("TCP_CONNECT", "timeoutSeconds"),
            ("COMMAND", "intervalSeconds"),
        ] {
            let err =
                serde_saphyr::from_str::<ServiceHealthCheck>(&format!("type: {ty}\n{old}: 5"))
                    .unwrap_err();
            assert!(
                err.to_string().contains(&format!("unknown field `{old}`")),
                "got: {err}"
            );
        }
    }

    /// The pre-revision spelling of the property is unknown, like any other
    /// (fixture `9.3--health-old-readiness-check-key.invalid`).
    #[test]
    fn readiness_check_is_not_a_service_property() {
        let old_key = ["readiness", "Check"].concat();
        let err = serde_saphyr::from_str::<Service>(&format!(
            "{MINIMAL}{old_key}:\n  type: TCP_CONNECT\n"
        ))
        .unwrap_err();
        assert!(
            err.to_string()
                .contains(&format!("unknown field `{old_key}`")),
            "got: {err}"
        );
    }

    #[test]
    fn completed_tasks_policy_spelling() {
        assert_eq!(CompletedTasksPolicy::Keep.as_str(), "KEEP");
        assert_eq!(CompletedTasksPolicy::Rerun.as_str(), "RERUN");
        let err =
            serde_saphyr::from_str::<ServiceRestartPolicy>("completedTasks: keep").unwrap_err();
        assert!(
            err.to_string().contains("unknown variant `keep`"),
            "got: {err}"
        );
    }

    #[test]
    fn service_actions_slots_and_default_timeouts() {
        let svc = parse(
            r#"
name: Coordinator
ports:
  - name: api
script:
  actions:
    onEnter: { command: init }
    onRun: { command: serve }
    onExit: { command: report }
"#,
        );
        let names: Vec<&str> = svc
            .script
            .actions
            .iter_named()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(names, vec!["onEnter", "onRun", "onExit"]);
        assert_eq!(svc.script.actions.iter_actions().count(), 3);
        assert_eq!(svc.script.actions.named_slots().len(), 4);
        assert_eq!(ServiceActions::default_timeout_seconds("onEnter"), None);
        assert_eq!(ServiceActions::default_timeout_seconds("onRun"), None);
        assert_eq!(
            ServiceActions::default_timeout_seconds("onHealthCheck"),
            Some(30)
        );
        assert_eq!(ServiceActions::default_timeout_seconds("onExit"), Some(300));
        assert_eq!(
            ServiceActions::default_timeout_seconds("onWrapTaskRun"),
            None
        );
        assert_eq!(ServiceHealthCheck::DEFAULT_READY_TIMEOUT_SECONDS, 300);
        assert_eq!(
            ServiceHealthCheck::DEFAULT_TCP_CONNECT_READINESS_INTERVAL_SECONDS,
            1
        );
        assert_eq!(
            ServiceHealthCheck::DEFAULT_COMMAND_READINESS_INTERVAL_SECONDS,
            5
        );
        assert_eq!(ServiceHealthCheck::DEFAULT_HEALTH_INTERVAL_SECONDS, 30);
        assert_eq!(ServiceHealthCheck::DEFAULT_FAILURE_THRESHOLD, 3);
        assert_eq!(ServiceRestartPolicy::DEFAULT_MAX_ATTEMPTS, 0);
    }

    #[test]
    fn unknown_service_field_rejected() {
        let err =
            serde_saphyr::from_str::<Service>(&format!("{MINIMAL}replicas: 2\n")).unwrap_err();
        assert!(
            err.to_string().contains("unknown field `replicas`"),
            "got: {err}"
        );
    }

    /// `serviceEnvironments` was removed from the RFC (Rejected Ideas: "a
    /// Service-scoped Environment list"); it is an unknown property like any
    /// other.
    #[test]
    fn service_environments_is_not_a_property() {
        let err = serde_saphyr::from_str::<Service>(&format!(
            "{MINIMAL}serviceEnvironments:\n  - name: Conda\n    variables: {{ A: b }}\n"
        ))
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("unknown field `serviceEnvironments`"),
            "got: {err}"
        );
    }
}
