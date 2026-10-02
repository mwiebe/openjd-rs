// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Service types per spec §9 (`SERVICE` extension, RFC 0009).
//!
//! A Service is a long-lived process with named TCP ports that a scheduler
//! starts before any Task in its scope is scheduled and keeps running for the
//! lifetime of its scope (the whole Job for `jobServices`, one Step for
//! `stepServices`). The types here are the unresolved template shapes; the
//! `Service.*` format-string scope and job creation of Services are
//! implemented separately.

use super::actions::Action;
use super::constrained_strings::Description;
use super::environment::EmbeddedFile;
use super::host_requirements::HostRequirements;
use crate::format_string::FormatString;
use serde::Deserialize;
use std::collections::HashMap;

/// §9 `<Service>` — a long-lived process with named ports, available with
/// the `SERVICE` extension.
///
/// `name` is a `<ServiceName>` (§9.1): an identifier that is not `File`.
/// It is held as a plain `String` so that the identifier constraints are
/// reported with a field path by template validation rather than as a
/// serde error.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Service {
    /// §9.1 `<ServiceName>`: the first component of `Service.<name>.*`
    /// references. Unique within its list; a Step Service must not share a
    /// name with a Job Service.
    pub name: String,
    pub description: Option<Description>,
    /// Expression bindings evaluated once at job creation (EXPR). Not in
    /// scope: `Session.*`, `Service.*`.
    #[serde(rename = "let")]
    pub let_bindings: Option<Vec<String>>,
    /// Requirements the service host must satisfy. Independent of the
    /// `hostRequirements` of any Step whose Tasks use the Service.
    pub host_requirements: Option<HostRequirements>,
    /// §9.2 The named ports the Service exposes: 1–10 entries with unique
    /// names, each TCP (the default) or UDP. No two ports of the same
    /// protocol may have the same `port` number (§9 item 5.4).
    pub ports: Vec<ServicePort>,
    /// §9.3 How readiness is determined. `None` means
    /// `{ type: TCP_CONNECT }` on every declared TCP port; a Service none
    /// of whose ports is TCP must declare a `STDOUT` or `COMMAND` check
    /// (§9 item 6). See [`readiness_check`](Self::readiness_check).
    pub readiness_check: Option<ServiceReadinessCheck>,
    /// §9.4 What happens when `onRun` exits before the scope ends. `None`
    /// means `{ maxAttempts: 0, completedTasks: RERUN }`; see
    /// [`restart_policy`](Self::restart_policy).
    pub restart_policy: Option<ServiceRestartPolicy>,
    /// Environment variables set for every action of the Service's script
    /// (same schema as `<Environment>.variables`). Not propagated to the
    /// entities in the Service's scope.
    pub variables: Option<HashMap<String, FormatString>>,
    /// §9.5 The actions the Service runs on its host.
    pub script: ServiceScript,
}

impl Service {
    /// The effective readiness check: the declared one, or the §9 default
    /// `{ type: TCP_CONNECT }` applied to every declared TCP port.
    pub fn readiness_check(&self) -> ServiceReadinessCheck {
        self.readiness_check.clone().unwrap_or_default()
    }

    /// The effective restart policy: the declared one, or the §9 default
    /// `{ maxAttempts: 0, completedTasks: RERUN }`.
    pub fn restart_policy(&self) -> ServiceRestartPolicy {
        self.restart_policy.clone().unwrap_or_default()
    }

    /// The names of the declared ports, in declaration order.
    pub fn port_names(&self) -> impl Iterator<Item = &str> {
        self.ports.iter().map(|p| p.name.as_str())
    }

    /// The names of the declared ports whose `protocol` is `TCP`, in
    /// declaration order — the ports a `TCP_CONNECT` readiness check
    /// probes when it names none (§9 item 6, §9.3 item 2).
    pub fn tcp_port_names(&self) -> impl Iterator<Item = &str> {
        self.ports
            .iter()
            .filter(|p| p.protocol == ServicePortProtocol::Tcp)
            .map(|p| p.name.as_str())
    }
}

/// §9.2 `<ServicePort>` — one named port of a Service.
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
    /// §9.2 item 3: the transport protocol the service process binds the
    /// port with and the scheduler publishes or forwards it as. Not a
    /// format string. Default `TCP`.
    #[serde(default)]
    pub protocol: ServicePortProtocol,
}

/// §9.2 item 3 `<ServicePort>.protocol` — the transport protocol of one
/// port. TCP and UDP port numbers are separate spaces: a number is
/// requested or allocated in the space of this protocol, and two ports may
/// share a number when their protocols differ.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize, serde::Serialize,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ServicePortProtocol {
    /// A TCP port; the only kind a `TCP_CONNECT` readiness check can
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

/// §9.3 `<ServiceReadinessCheck>` — discriminated union on `type`.
///
/// The numeric fields are `<posinteger> | <posintstring>` (`@fmtstring`),
/// modeled like `<Action>.timeout`; a format string is resolved at job
/// creation. `timeoutSeconds` is measured from the start of `onRun` and
/// defaults to [`DEFAULT_TIMEOUT_SECONDS`](Self::DEFAULT_TIMEOUT_SECONDS).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum ServiceReadinessCheck {
    /// READY once a TCP connection to each listed port succeeds. `ports`
    /// may name only TCP ports and defaults to every declared TCP port.
    #[serde(rename = "TCP_CONNECT")]
    TcpConnect {
        ports: Option<Vec<String>>,
        timeout_seconds: Option<FormatString>,
    },
    /// READY once `<ServiceActions>.onReadinessCheck` exits 0 while `onRun`
    /// is running. `intervalSeconds` (default
    /// [`DEFAULT_COMMAND_INTERVAL_SECONDS`](Self::DEFAULT_COMMAND_INTERVAL_SECONDS))
    /// separates the end of one invocation from the start of the next.
    #[serde(rename = "COMMAND")]
    Command {
        interval_seconds: Option<FormatString>,
        timeout_seconds: Option<FormatString>,
    },
    /// READY once `onRun` writes `openjd_service_ready: <message>` to
    /// stdout.
    #[serde(rename = "STDOUT")]
    Stdout {
        timeout_seconds: Option<FormatString>,
    },
}

impl ServiceReadinessCheck {
    /// §9.3 default for `timeoutSeconds`, in seconds.
    pub const DEFAULT_TIMEOUT_SECONDS: u64 = 300;
    /// §9.3 default for `COMMAND`'s `intervalSeconds`, in seconds.
    pub const DEFAULT_COMMAND_INTERVAL_SECONDS: u64 = 5;

    /// The schema value of the `type` discriminator.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::TcpConnect { .. } => "TCP_CONNECT",
            Self::Command { .. } => "COMMAND",
            Self::Stdout { .. } => "STDOUT",
        }
    }

    /// The `timeoutSeconds` field, whichever variant this is.
    pub fn timeout_seconds(&self) -> Option<&FormatString> {
        match self {
            Self::TcpConnect {
                timeout_seconds, ..
            }
            | Self::Command {
                timeout_seconds, ..
            }
            | Self::Stdout { timeout_seconds } => timeout_seconds.as_ref(),
        }
    }
}

impl Default for ServiceReadinessCheck {
    /// `{ type: TCP_CONNECT }` on every declared TCP port, with the default
    /// timeout.
    fn default() -> Self {
        Self::TcpConnect {
            ports: None,
            timeout_seconds: None,
        }
    }
}

/// §9.4 `<ServiceRestartPolicy>`.
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
    /// launched. Default `RERUN`; see
    /// [`completed_tasks`](Self::completed_tasks).
    pub completed_tasks: Option<CompletedTasksPolicy>,
}

impl ServiceRestartPolicy {
    /// §9.4 default for `maxAttempts`: launched exactly once, never
    /// relaunched.
    pub const DEFAULT_MAX_ATTEMPTS: i64 = 0;

    /// The effective `completedTasks` value, defaulting to
    /// [`CompletedTasksPolicy::Rerun`].
    pub fn completed_tasks(&self) -> CompletedTasksPolicy {
        self.completed_tasks.unwrap_or_default()
    }
}

/// §9.4 `completedTasks` — what a Service restart does to the Tasks in its
/// scope that completed against, or were running against, the previous
/// instance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Deserialize, serde::Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CompletedTasksPolicy {
    /// Completed Tasks remain complete and running Tasks continue. Also
    /// declares the Service resumable (it may be suspended while idle).
    Keep,
    /// Completed Tasks are requeued and running Tasks are canceled and
    /// requeued without counting as a Task failure. The safe default.
    #[default]
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

/// §9.5 `<ServiceScript>`.
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

/// §9.6 `<ServiceActions>`.
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
    /// Run repeatedly while `onRun` runs when `readinessCheck.type` is
    /// `COMMAND`; exit 0 means READY. Must be defined if and only if the
    /// readiness check type is `COMMAND`. Default timeout
    /// [`ON_READINESS_CHECK_DEFAULT_TIMEOUT_SECONDS`](Self::ON_READINESS_CHECK_DEFAULT_TIMEOUT_SECONDS).
    pub on_readiness_check: Option<Action>,
    /// Cleanup run after the other actions have stopped for the last time
    /// in a Service Session. Default timeout
    /// [`ON_EXIT_DEFAULT_TIMEOUT_SECONDS`](Self::ON_EXIT_DEFAULT_TIMEOUT_SECONDS).
    pub on_exit: Option<Action>,
}

impl ServiceActions {
    /// RFC 0009 default `timeout` for `onReadinessCheck`, in seconds; bounds
    /// one invocation.
    pub const ON_READINESS_CHECK_DEFAULT_TIMEOUT_SECONDS: u64 = 30;
    /// RFC 0009 default `timeout` for `onExit`, in seconds (five minutes,
    /// as for an Environment's `onExit`).
    pub const ON_EXIT_DEFAULT_TIMEOUT_SECONDS: u64 = 300;

    /// The RFC 0009 default `timeout` of the named action when the template
    /// gives none: `onEnter` and `onRun` have no default (`None`),
    /// `onReadinessCheck` 30 seconds, `onExit` 300 seconds. Returns `None`
    /// for a name that is not a `<ServiceActions>` slot.
    pub fn default_timeout_seconds(action_name: &str) -> Option<u64> {
        match action_name {
            "onReadinessCheck" => Some(Self::ON_READINESS_CHECK_DEFAULT_TIMEOUT_SECONDS),
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
            ("onReadinessCheck", self.on_readiness_check.as_ref()),
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
        assert!(svc.readiness_check.is_none());
        assert!(matches!(
            svc.readiness_check(),
            ServiceReadinessCheck::TcpConnect {
                ports: None,
                timeout_seconds: None
            }
        ));
        let policy = svc.restart_policy();
        assert!(policy.max_attempts.is_none());
        assert_eq!(policy.completed_tasks(), CompletedTasksPolicy::Rerun);
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
readinessCheck:
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
readinessCheck:
  type: COMMAND
  intervalSeconds: "{{ Param.Interval }}"
  timeoutSeconds: 60
restartPolicy:
  maxAttempts: "{{ Param.Attempts }}"
  completedTasks: KEEP
script:
  actions:
    onRun:
      command: valkey-server
    onReadinessCheck:
      command: valkey-cli
"#,
        );
        assert_eq!(svc.ports[0].port.as_ref().unwrap().raw(), "6379");
        assert_eq!(
            svc.ports[1].port.as_ref().unwrap().raw(),
            "{{ Param.Port }}"
        );
        match svc.readiness_check.as_ref().unwrap() {
            ServiceReadinessCheck::Command {
                interval_seconds,
                timeout_seconds,
            } => {
                assert_eq!(
                    interval_seconds.as_ref().unwrap().raw(),
                    "{{ Param.Interval }}"
                );
                assert_eq!(timeout_seconds.as_ref().unwrap().raw(), "60");
            }
            other => panic!("unexpected readiness check {other:?}"),
        }
        let policy = svc.restart_policy.as_ref().unwrap();
        assert_eq!(
            policy.max_attempts.as_ref().unwrap().raw(),
            "{{ Param.Attempts }}"
        );
        assert_eq!(policy.completed_tasks(), CompletedTasksPolicy::Keep);
    }

    #[test]
    fn readiness_check_type_names_and_timeout_accessor() {
        let tcp: ServiceReadinessCheck =
            serde_saphyr::from_str("type: TCP_CONNECT\nports: [main]\ntimeoutSeconds: 10").unwrap();
        assert_eq!(tcp.type_name(), "TCP_CONNECT");
        assert_eq!(tcp.timeout_seconds().unwrap().raw(), "10");
        let stdout: ServiceReadinessCheck = serde_saphyr::from_str("type: STDOUT").unwrap();
        assert_eq!(stdout.type_name(), "STDOUT");
        assert!(stdout.timeout_seconds().is_none());
        let cmd: ServiceReadinessCheck = serde_saphyr::from_str("type: COMMAND").unwrap();
        assert_eq!(cmd.type_name(), "COMMAND");
    }

    #[test]
    fn readiness_check_rejects_unknown_type_and_foreign_fields() {
        let err = serde_saphyr::from_str::<ServiceReadinessCheck>("type: HTTP").unwrap_err();
        assert!(
            err.to_string().contains("unknown variant `HTTP`"),
            "got: {err}"
        );
        // `ports` belongs to TCP_CONNECT only.
        let err = serde_saphyr::from_str::<ServiceReadinessCheck>("type: STDOUT\nports: [main]")
            .unwrap_err();
        assert!(
            err.to_string().contains("unknown field `ports`"),
            "got: {err}"
        );
        // `intervalSeconds` belongs to COMMAND only.
        let err = serde_saphyr::from_str::<ServiceReadinessCheck>(
            "type: TCP_CONNECT\nintervalSeconds: 5",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("unknown field `intervalSeconds`"),
            "got: {err}"
        );
    }

    #[test]
    fn completed_tasks_policy_spelling() {
        assert_eq!(CompletedTasksPolicy::default(), CompletedTasksPolicy::Rerun);
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
            ServiceActions::default_timeout_seconds("onReadinessCheck"),
            Some(30)
        );
        assert_eq!(ServiceActions::default_timeout_seconds("onExit"), Some(300));
        assert_eq!(
            ServiceActions::default_timeout_seconds("onWrapTaskRun"),
            None
        );
        assert_eq!(ServiceReadinessCheck::DEFAULT_TIMEOUT_SECONDS, 300);
        assert_eq!(ServiceReadinessCheck::DEFAULT_COMMAND_INTERVAL_SECONDS, 5);
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
