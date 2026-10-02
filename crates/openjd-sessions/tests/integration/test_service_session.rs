// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! End-to-end tests for the RFC 0009 Service Session runtime
//! (`openjd_sessions::ServiceSession`) with real subprocesses.
//!
//! `onRun` output is observed through `ServiceRunExit::stdout`
//! (`debug_collect_stdout`), ordering through trace files that actions
//! append to, and runtime decisions through the session log
//! (`testing_logger`). The `TCP_CONNECT` tests use a tiny `python3` socket
//! listener.

#![cfg(unix)] // sh/bash/python3 availability

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use openjd_expr::format_string::FormatString;
use openjd_expr::symbol_table::{SerializedSymbolTable, SymbolTable};
use openjd_expr::ExprValue;
use openjd_model::job::service_symbols::{ServiceEndpoint, ServiceEndpoints};
use openjd_model::job::ServicePortProtocol;
use openjd_model::job::{
    Action, CancelationMode, CompletedTasksPolicy, EmbeddedFile, Environment, EnvironmentActions,
    EnvironmentScript, RunScope, Service, ServiceActions, ServicePort, ServiceReadinessCheck,
    ServiceRestartPolicy, ServiceScript,
};
use openjd_model::types::{FileType, JobParameterType, JobParameterValue};
use openjd_sessions::action::ActionState;
use openjd_sessions::{
    ActionStatus, PathFormat, PathMappingRule, ServiceReadiness, ServiceSession,
    ServiceSessionConfig, ServiceSessionState, SessionConfig, StickyBitPolicy,
};
use tempfile::TempDir;

// ────────────────────────────────────────────────────────────────────
// Construction helpers
// ────────────────────────────────────────────────────────────────────

fn fs(s: &str) -> FormatString {
    FormatString::new(s).unwrap()
}

fn sh(script: &str) -> Action {
    Action {
        command: fs("sh"),
        args: Some(vec![fs("-c"), fs(script)]),
        timeout: None,
        cancelation: None,
    }
}

/// `sh -c` with NOTIFY_THEN_TERMINATE cancelation (SIGTERM, then SIGKILL
/// after `notify_secs`).
fn sh_ntt(script: &str, notify_secs: u64) -> Action {
    Action {
        cancelation: Some(CancelationMode::NotifyThenTerminate {
            notify_period_in_seconds: Some(fs(&notify_secs.to_string())),
        }),
        ..sh(script)
    }
}

fn tcp_check(ports: &[&str], timeout_seconds: u64) -> ServiceReadinessCheck {
    ServiceReadinessCheck::TcpConnect {
        ports: ports.iter().map(|p| p.to_string()).collect(),
        timeout_seconds,
    }
}

fn stdout_check(timeout_seconds: u64) -> ServiceReadinessCheck {
    ServiceReadinessCheck::Stdout { timeout_seconds }
}

fn command_check(interval_seconds: u64, timeout_seconds: u64) -> ServiceReadinessCheck {
    ServiceReadinessCheck::Command {
        interval_seconds,
        timeout_seconds,
    }
}

/// `sh -c` with an explicit `timeout`.
fn sh_timeout(script: &str, timeout_secs: u64) -> Action {
    Action {
        timeout: Some(fs(&timeout_secs.to_string())),
        ..sh(script)
    }
}

struct ServiceBuilder {
    service: Service,
}

impl ServiceBuilder {
    fn new(name: &str, ports: &[&str], on_run: Action) -> Self {
        Self {
            service: Service {
                name: name.into(),
                description: None,
                document: Default::default(),
                host_requirements: None,
                ports: ports
                    .iter()
                    .map(|p| ServicePort {
                        name: p.to_string(),
                        port: None,
                        protocol: ServicePortProtocol::Tcp,
                    })
                    .collect(),
                readiness_check: tcp_check(ports, 300),
                restart_policy: ServiceRestartPolicy {
                    max_attempts: 0,
                    completed_tasks: CompletedTasksPolicy::Rerun,
                },
                variables: None,
                script: ServiceScript {
                    let_bindings: None,
                    actions: ServiceActions {
                        on_enter: None,
                        on_run,
                        on_readiness_check: None,
                        on_exit: None,
                    },
                    embedded_files: None,
                },
                resolved_symtab: None,
            },
        }
    }
    fn readiness(mut self, check: ServiceReadinessCheck) -> Self {
        self.service.readiness_check = check;
        self
    }
    /// Declare the named port `protocol: UDP`.
    fn udp_port(mut self, name: &str) -> Self {
        let port = self
            .service
            .ports
            .iter_mut()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("no port {name}"));
        port.protocol = ServicePortProtocol::Udp;
        self
    }
    fn on_enter(mut self, a: Action) -> Self {
        self.service.script.actions.on_enter = Some(a);
        self
    }
    fn on_exit(mut self, a: Action) -> Self {
        self.service.script.actions.on_exit = Some(a);
        self
    }
    fn on_readiness_check(mut self, a: Action) -> Self {
        self.service.script.actions.on_readiness_check = Some(a);
        self
    }
    fn variables(mut self, vars: &[(&str, &str)]) -> Self {
        self.service.variables = Some(
            vars.iter()
                .map(|(k, v)| (k.to_string(), fs(v)))
                .collect::<HashMap<_, _>>(),
        );
        self
    }
    fn lets(mut self, bindings: &[&str]) -> Self {
        self.service.script.let_bindings = Some(bindings.iter().map(|b| b.to_string()).collect());
        self
    }
    fn embedded_file(mut self, name: &str, data: &str, runnable: bool) -> Self {
        let file = EmbeddedFile {
            name: name.into(),
            file_type: FileType::Text,
            filename: None,
            data: Some(fs(data)),
            runnable: Some(runnable),
            end_of_line: None,
        };
        self.service
            .script
            .embedded_files
            .get_or_insert_with(Vec::new)
            .push(file);
        self
    }
    fn resolved_symtab(mut self, st: &SymbolTable) -> Self {
        self.service.resolved_symtab = Some(SerializedSymbolTable::from_symtab(st));
        self
    }
    fn build(self) -> Service {
        self.service
    }
}

fn env(
    name: &str,
    run_scope: Option<Vec<RunScope>>,
    on_enter: Option<Action>,
    on_exit: Option<Action>,
) -> Environment {
    Environment {
        name: name.into(),
        description: None,
        run_scope,
        script: Some(EnvironmentScript {
            let_bindings: None,
            actions: EnvironmentActions {
                on_enter,
                on_wrap_env_enter: None,
                on_wrap_task_run: None,
                on_wrap_env_exit: None,
                on_wrap_service_enter: None,
                on_wrap_service_run: None,
                on_wrap_service_readiness_check: None,
                on_wrap_service_exit: None,
                on_exit,
            },
            embedded_files: None,
        }),
        variables: None,
        resolved_symtab: None,
    }
}

fn env_with_vars(name: &str, vars: &[(&str, &str)]) -> Environment {
    let mut e = env(name, None, None, None);
    e.variables = Some(vars.iter().map(|(k, v)| (k.to_string(), fs(v))).collect());
    e
}

/// Reserve a free loopback port by binding and releasing it.
fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

fn endpoints(name: &str, ports: &[(&str, u16)]) -> ServiceEndpoints {
    endpoints_with_protocols(
        name,
        &ports
            .iter()
            .map(|(p, port)| (*p, *port, ServicePortProtocol::Tcp))
            .collect::<Vec<_>>(),
    )
}

fn endpoints_with_protocols(
    name: &str,
    ports: &[(&str, u16, ServicePortProtocol)],
) -> ServiceEndpoints {
    ServiceEndpoints::new(
        name,
        ports
            .iter()
            .map(|(p, port, protocol)| {
                (
                    p.to_string(),
                    ServiceEndpoint {
                        port: *port,
                        protocol: *protocol,
                        bind_address: "127.0.0.1".into(),
                        connect_address: "127.0.0.1".into(),
                    },
                )
            })
            .collect(),
    )
}

fn session_config(root: &TempDir, id: &str) -> SessionConfig {
    SessionConfig {
        limits: Default::default(),
        session_id: id.into(),
        job_parameter_values: Default::default(),
        session_root_directory: Some(root.path().to_path_buf()),
        path_mapping_rules: None,
        retain_working_dir: true,
        callback: None,
        os_env_vars: None,
        user: None,
        profile: None,
        cancel_token: None,
        debug_collect_stdout: true,
        echo_openjd_directives: true,
        log_tag: None,
        sticky_bit_policy: StickyBitPolicy::Strict,
    }
}

fn service_session(
    root: &TempDir,
    service: Service,
    environments: Vec<Environment>,
    endpoints: ServiceEndpoints,
    in_scope: Vec<ServiceEndpoints>,
) -> ServiceSession {
    ServiceSession::with_config(ServiceSessionConfig {
        session: session_config(root, &format!("svc-test:{}", service.name)),
        service,
        environments,
        environment_profiles: vec![],
        endpoints,
        in_scope_endpoints: in_scope,
    })
    .unwrap()
}

/// A `python3` TCP listener that binds `Service.<svc>.main.bindAddress` /
/// `.port`, prints a line per argument, then serves until killed.
fn python_listener(svc: &str, extra_echo: &str) -> Action {
    let script = r#"
import socket, sys
host, port = sys.argv[1], int(sys.argv[2])
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind((host, port))
s.listen(5)
print("listening", host, port, flush=True)
print("extra", sys.argv[3], flush=True)
while True:
    c, _ = s.accept()
    c.close()
"#;
    Action {
        command: fs("python3"),
        args: Some(vec![
            fs("-c"),
            fs(script),
            fs(&format!("{{{{Service.{svc}.main.bindAddress}}}}")),
            fs(&format!("{{{{Service.{svc}.main.port}}}}")),
            fs(extra_echo),
        ]),
        timeout: None,
        cancelation: None,
    }
}

fn read_trace(path: &PathBuf) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn lines(s: &str) -> Vec<&str> {
    s.lines().collect()
}

// ────────────────────────────────────────────────────────────────────
// Readiness
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn tcp_connect_readiness_becomes_ready_then_cancel_and_end() {
    testing_logger::setup();
    let root = TempDir::new().unwrap();
    let port = free_port();
    let service = ServiceBuilder::new(
        "svc",
        &["main"],
        python_listener("svc", "{{Service.svc.main.connectAddress}}"),
    )
    .build();
    let mut ss = service_session(
        &root,
        service,
        vec![],
        endpoints("svc", &[("main", port)]),
        vec![],
    );

    assert_eq!(ss.state(), ServiceSessionState::Created);
    assert!(ss.readiness().is_none());
    ss.enter().await.unwrap();
    assert_eq!(ss.state(), ServiceSessionState::Entered);
    ss.launch().await.unwrap();
    assert_eq!(ss.state(), ServiceSessionState::Running);
    assert_eq!(ss.launch_count(), 1);

    let ready = ss.wait_ready().await.unwrap();
    assert_eq!(ready, ServiceReadiness::Ready { message: None });
    assert!(ss.readiness().unwrap().is_ready());
    // READY requires onRun still running.
    assert_eq!(ss.state(), ServiceSessionState::Running);
    assert!(ss.run_exit().is_none());

    assert!(ss.cancel_run(None));
    let exit = ss.wait_exit().await.unwrap();
    assert_eq!(exit.state, ActionState::Canceled);
    assert!(exit.canceled);
    assert_eq!(ss.state(), ServiceSessionState::Exited);
    assert_eq!(
        lines(&exit.stdout),
        vec![
            format!("listening 127.0.0.1 {port}").as_str(),
            "extra 127.0.0.1"
        ]
    );
    // Canceling again is a no-op once exited.
    assert!(!ss.cancel_run(None));

    let working_dir = ss.session().working_directory().to_path_buf();
    assert!(working_dir.exists());
    ss.end().await.unwrap();
    assert_eq!(ss.state(), ServiceSessionState::Ended);
    // retain_working_dir is set in the test config, so the directory stays.
    assert!(working_dir.exists());

    testing_logger::validate(|logs| {
        let bodies: Vec<&str> = logs.iter().map(|l| l.body.as_str()).collect();
        assert!(bodies.contains(&"--------- Starting Service: svc"));
        assert!(bodies.contains(&"--------- Service onRun: svc (launch 1)"));
        assert!(bodies.contains(&"Readiness check: TCP_CONNECT (timeout 300s)"));
        assert!(bodies.contains(&"Service 'svc' is READY"));
        assert!(bodies.contains(&"Canceling Service 'svc' onRun"));
        assert!(
            bodies.contains(
                &"Service 'svc' onRun exited: Canceled (exit code: N/A), canceled by the runtime"
            ),
            "{bodies:?}"
        );
        assert!(bodies.contains(&"--------- Ending Service: svc"));
    });
}

#[tokio::test]
async fn stdout_readiness_carries_message_and_dedups() {
    testing_logger::setup();
    let root = TempDir::new().unwrap();
    let service = ServiceBuilder::new(
        "svc",
        &["main"],
        sh("echo openjd_service_ready: warmed up; echo openjd_service_ready: again; sleep 30"),
    )
    .readiness(stdout_check(300))
    .build();
    let mut ss = service_session(
        &root,
        service,
        vec![],
        endpoints("svc", &[("main", 5)]),
        vec![],
    );
    let ready = ss.start().await.unwrap();
    assert_eq!(
        ready,
        ServiceReadiness::Ready {
            message: Some("warmed up".into())
        }
    );
    // Give the second line time to arrive: it must not change the message.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        ss.readiness(),
        Some(ServiceReadiness::Ready {
            message: Some("warmed up".into())
        })
    );
    ss.cancel_run(Some(Duration::ZERO));
    let exit = ss.wait_exit().await.unwrap();
    assert_eq!(exit.state, ActionState::Canceled);
    ss.end().await.unwrap();
    testing_logger::validate(|logs| {
        let bodies: Vec<&str> = logs.iter().map(|l| l.body.as_str()).collect();
        assert_eq!(
            bodies
                .iter()
                .filter(|b| **b == "Service 'svc' is READY: warmed up")
                .count(),
            1
        );
        assert!(bodies.contains(&"openjd_service_ready: warmed up"));
        assert!(bodies.contains(&"openjd_service_ready: again"));
    });
}

#[tokio::test]
async fn service_ready_message_ignored_under_tcp_connect_and_readiness_times_out() {
    testing_logger::setup();
    let root = TempDir::new().unwrap();
    let port = free_port();
    let service = ServiceBuilder::new(
        "svc",
        &["main"],
        sh("echo openjd_service_ready: nope; sleep 30"),
    )
    .readiness(tcp_check(&["main"], 2))
    .build();
    let mut ss = service_session(
        &root,
        service,
        vec![],
        endpoints("svc", &[("main", port)]),
        vec![],
    );
    let started = std::time::Instant::now();
    let ready = ss.start().await.unwrap();
    assert_eq!(ready, ServiceReadiness::TimedOut);
    assert!(started.elapsed() >= Duration::from_secs(2));
    assert!(started.elapsed() < Duration::from_secs(10));
    // The instance failed but onRun is still running: the caller cancels it.
    assert_eq!(ss.state(), ServiceSessionState::Running);
    assert!(ss.run_exit().is_none());
    assert!(ss.cancel_run(Some(Duration::ZERO)));
    let exit = ss.wait_exit().await.unwrap();
    assert_eq!(exit.state, ActionState::Canceled);
    assert!(exit.canceled);
    ss.end().await.unwrap();
    testing_logger::validate(|logs| {
        let bodies: Vec<&str> = logs.iter().map(|l| l.body.as_str()).collect();
        assert!(bodies.contains(
            &"Ignoring openjd_service_ready from Service 'svc' onRun: its readiness check type is TCP_CONNECT"
        ), "{bodies:?}");
        assert!(bodies.contains(
            &"Service 'svc' did not become READY within 2s (TCP_CONNECT readiness check)"
        ));
    });
}

#[tokio::test]
async fn stdout_readiness_timeout_when_never_emitted() {
    let root = TempDir::new().unwrap();
    let service = ServiceBuilder::new("svc", &["main"], sh("sleep 30"))
        .readiness(stdout_check(1))
        .build();
    let mut ss = service_session(
        &root,
        service,
        vec![],
        endpoints("svc", &[("main", 5)]),
        vec![],
    );
    assert_eq!(ss.start().await.unwrap(), ServiceReadiness::TimedOut);
    ss.cancel_run(Some(Duration::ZERO));
    ss.wait_exit().await.unwrap();
    ss.end().await.unwrap();
}

#[tokio::test]
async fn tcp_connect_probes_only_listed_ports() {
    // Two declared ports, but the readiness check names only `main`; the
    // listener binds `main` only, so READY must still be reached.
    let root = TempDir::new().unwrap();
    let main_port = free_port();
    let other_port = free_port();
    let service = ServiceBuilder::new("svc", &["main", "other"], python_listener("svc", "x"))
        .readiness(tcp_check(&["main"], 300))
        .build();
    let mut ss = service_session(
        &root,
        service,
        vec![],
        endpoints("svc", &[("main", main_port), ("other", other_port)]),
        vec![],
    );
    assert_eq!(
        ss.start().await.unwrap(),
        ServiceReadiness::Ready { message: None }
    );
    ss.cancel_run(Some(Duration::ZERO));
    ss.wait_exit().await.unwrap();
    ss.end().await.unwrap();
}

// ────────────────────────────────────────────────────────────────────
// Instance exit
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn on_run_exit_before_ready_is_detected() {
    testing_logger::setup();
    let root = TempDir::new().unwrap();
    let service = ServiceBuilder::new("svc", &["main"], sh("echo starting; exit 3"))
        .readiness(stdout_check(300))
        .build();
    let mut ss = service_session(
        &root,
        service,
        vec![],
        endpoints("svc", &[("main", 5)]),
        vec![],
    );
    let ready = ss.start().await.unwrap();
    assert_eq!(ready, ServiceReadiness::ExitedBeforeReady);
    let exit = ss.wait_exit().await.unwrap();
    assert_eq!(exit.state, ActionState::Failed);
    assert_eq!(exit.exit_code, Some(3));
    assert!(!exit.canceled);
    assert_eq!(exit.fail_message, None);
    assert_eq!(lines(&exit.stdout), vec!["starting"]);
    assert_eq!(ss.state(), ServiceSessionState::Exited);
    assert_eq!(ss.run_exit(), Some(exit));
    ss.end().await.unwrap();
    testing_logger::validate(|logs| {
        let bodies: Vec<&str> = logs.iter().map(|l| l.body.as_str()).collect();
        assert!(bodies.contains(&"Service 'svc' onRun exited before becoming READY"));
        assert!(bodies.contains(&"Service 'svc' onRun exited: Failed (exit code: 3)"));
    });
}

#[tokio::test]
async fn on_run_exit_after_ready_reports_status_and_fail_message() {
    let root = TempDir::new().unwrap();
    let service = ServiceBuilder::new(
        "svc",
        &["main"],
        sh("echo openjd_service_ready: up; sleep 1; echo openjd_status: shutting down; echo openjd_fail: disk full; exit 0"),
    )
    .readiness(stdout_check(300))
    .build();
    let statuses: Arc<Mutex<Vec<ActionStatus>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = statuses.clone();
    let mut config = session_config(&root, "svc-test:cb");
    config.callback = Some(Box::new(move |_, status| {
        sink.lock().unwrap().push(status.clone());
    }));
    let mut ss = ServiceSession::with_config(ServiceSessionConfig {
        session: config,
        service,
        environments: vec![],
        environment_profiles: vec![],
        endpoints: endpoints("svc", &[("main", 5)]),
        in_scope_endpoints: vec![],
    })
    .unwrap();
    assert_eq!(
        ss.start().await.unwrap(),
        ServiceReadiness::Ready {
            message: Some("up".into())
        }
    );
    // An exit-watch receiver is the asynchronous notification.
    let mut watch = ss.exit_watch().unwrap();
    watch.changed().await.unwrap();
    let from_watch = watch.borrow().clone().unwrap();
    let exit = ss.wait_exit().await.unwrap();
    assert_eq!(exit, from_watch);
    // openjd_fail makes the action Failed even on exit 0 (existing runtime
    // behavior for every action kind) and supplies the reason.
    assert_eq!(exit.state, ActionState::Failed);
    assert_eq!(exit.exit_code, Some(0));
    assert!(!exit.canceled);
    assert_eq!(exit.fail_message.as_deref(), Some("disk full"));
    // Readiness is unchanged by a later exit.
    assert_eq!(
        ss.readiness(),
        Some(ServiceReadiness::Ready {
            message: Some("up".into())
        })
    );
    let status = ss.action_status().unwrap();
    assert_eq!(status.state, ActionState::Failed);
    assert_eq!(status.status_message.as_deref(), Some("shutting down"));
    assert_eq!(status.fail_message.as_deref(), Some("disk full"));
    assert_eq!(status.exit_code, Some(0));
    ss.end().await.unwrap();

    let statuses = statuses.lock().unwrap();
    assert_eq!(statuses.first().unwrap().state, ActionState::Running);
    assert!(statuses
        .iter()
        .any(|s| s.status_message.as_deref() == Some("shutting down")));
    assert_eq!(statuses.last().unwrap().state, ActionState::Failed);
}

#[tokio::test]
async fn on_run_command_not_found_is_a_failed_exit() {
    let root = TempDir::new().unwrap();
    let service = ServiceBuilder::new(
        "svc",
        &["main"],
        Action {
            command: fs("/nonexistent/openjd-service-binary"),
            args: None,
            timeout: None,
            cancelation: None,
        },
    )
    .readiness(stdout_check(300))
    .build();
    let mut ss = service_session(
        &root,
        service,
        vec![],
        endpoints("svc", &[("main", 5)]),
        vec![],
    );
    assert_eq!(
        ss.start().await.unwrap(),
        ServiceReadiness::ExitedBeforeReady
    );
    let exit = ss.wait_exit().await.unwrap();
    assert_eq!(exit.state, ActionState::Failed);
    assert_eq!(exit.exit_code, None);
    assert!(exit
        .fail_message
        .as_deref()
        .unwrap()
        .starts_with("Failed to start subprocess '/nonexistent/openjd-service-binary'"));
    ss.end().await.unwrap();
}

#[tokio::test]
async fn on_run_declared_timeout_is_an_instance_failure() {
    let root = TempDir::new().unwrap();
    let service = ServiceBuilder::new(
        "svc",
        &["main"],
        Action {
            timeout: Some(fs("1")),
            ..sh("echo openjd_service_ready: up; sleep 30")
        },
    )
    .readiness(stdout_check(300))
    .build();
    let mut ss = service_session(
        &root,
        service,
        vec![],
        endpoints("svc", &[("main", 5)]),
        vec![],
    );
    assert!(ss.start().await.unwrap().is_ready());
    let exit = ss.wait_exit().await.unwrap();
    assert_eq!(exit.state, ActionState::Timeout);
    assert!(!exit.canceled);
    ss.end().await.unwrap();
}

// ────────────────────────────────────────────────────────────────────
// Environment variables
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn on_enter_env_vars_reach_on_run_and_precedence_holds() {
    let root = TempDir::new().unwrap();
    let port = free_port();
    // Environment < Service variables < onEnter: A is set at all three
    // levels, B at the first two, C only by the Environment; D is unset by
    // onEnter; E is redacted by onEnter.
    let environment = env_with_vars(
        "provision",
        &[("A", "env"), ("B", "env"), ("C", "env"), ("D", "env")],
    );
    let service = ServiceBuilder::new(
        "svc",
        &["main"],
        // The redacted E must not appear in captured output; prove it reached
        // the process by printing its length instead.
        sh(r#"echo "A=$A B=$B C=$C D=${D:-unset} E_LEN=${#E} PORT=$PORT WD=$OPENJD_SESSION_WORKING_DIR"; echo openjd_service_ready: ok; sleep 30"#),
    )
    .readiness(stdout_check(300))
    .variables(&[("A", "svc"), ("B", "svc-{{Service.svc.main.port}}"), ("PORT", "{{Service.svc.main.port}}")])
    .on_enter(sh(r#"echo "enter sees A=$A B=$B C=$C PORT=$PORT"; echo openjd_env: A=enter; echo openjd_unset_env: D; echo openjd_redacted_env: E=s3cret"#))
    .build();
    let mut config = session_config(&root, "svc-test:env");
    config.profile = Some(
        openjd_model::ModelProfile::new(openjd_model::SpecificationRevision::V2023_09)
            .with_extensions(
                [openjd_model::ModelExtension::RedactedEnvVars]
                    .into_iter()
                    .collect(),
            ),
    );
    let mut ss = ServiceSession::with_config(ServiceSessionConfig {
        session: config,
        service,
        environments: vec![environment],
        environment_profiles: vec![],
        endpoints: endpoints("svc", &[("main", port)]),
        in_scope_endpoints: vec![],
    })
    .unwrap();
    assert!(ss.start().await.unwrap().is_ready());
    ss.cancel_run(Some(Duration::ZERO));
    let exit = ss.wait_exit().await.unwrap();
    let wd = ss.session().working_directory().display().to_string();
    assert_eq!(
        lines(&exit.stdout),
        vec![
            format!("A=enter B=svc-{port} C=env D=unset E_LEN=6 PORT={port} WD={wd}").as_str(),
            "openjd_service_ready: ok",
        ]
    );
    ss.end().await.unwrap();
}

#[tokio::test]
async fn env_messages_from_on_run_and_on_exit_are_ignored() {
    testing_logger::setup();
    let root = TempDir::new().unwrap();
    let trace = root.path().join("trace.txt");
    let service = ServiceBuilder::new(
        "svc",
        &["main"],
        sh("echo openjd_env: FROM_RUN=1; echo openjd_unset_env: PATH; echo openjd_service_ready: ok; exit 0"),
    )
    .readiness(stdout_check(300))
    .on_exit(sh(&format!(
        "echo openjd_env: FROM_EXIT=1; echo \"exit FROM_RUN=${{FROM_RUN:-unset}} PATH_SET=${{PATH:+yes}}\" >> {}",
        trace.display()
    )))
    .build();
    let mut ss = service_session(
        &root,
        service,
        vec![],
        endpoints("svc", &[("main", 5)]),
        vec![],
    );
    ss.start().await.unwrap();
    let exit = ss.wait_exit().await.unwrap();
    // Not canceled or failed by the ignored (and would-be malformed) lines.
    assert_eq!(exit.state, ActionState::Success);
    ss.end().await.unwrap();
    assert_eq!(read_trace(&trace), vec!["exit FROM_RUN=unset PATH_SET=yes"]);
    testing_logger::validate(|logs| {
        let bodies: Vec<&str> = logs.iter().map(|l| l.body.as_str()).collect();
        assert!(bodies.contains(
            &"Ignoring openjd_env from Service 'svc' onRun: environment variable messages are honored only from onEnter"
        ));
        assert!(bodies.contains(
            &"Ignoring openjd_unset_env from Service 'svc' onRun: environment variable messages are honored only from onEnter"
        ));
        assert!(bodies.contains(
            &"Ignoring openjd_env from Service 'svc' onExit: environment variable messages are honored only from onEnter"
        ));
    });
}

// ────────────────────────────────────────────────────────────────────
// Cancel, onExit, relaunch
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn end_cancels_running_on_run_with_its_method_then_runs_on_exit_and_exits_envs() {
    let root = TempDir::new().unwrap();
    let trace = root.path().join("trace.txt");
    let t = trace.display().to_string();
    let environment = env(
        "outer",
        None,
        Some(sh(&format!("echo env-enter >> {t}"))),
        Some(sh(&format!("echo env-exit >> {t}"))),
    );
    let service = ServiceBuilder::new(
        "svc",
        &["main"],
        sh_ntt(
            &format!("trap 'echo run-got-term >> {t}; exit 0' TERM; echo openjd_service_ready: ok; while true; do sleep 0.1; done"),
            10,
        ),
    )
    .readiness(stdout_check(300))
    .on_enter(sh(&format!("echo svc-enter >> {t}")))
    .on_exit(sh(&format!("echo svc-exit >> {t}")))
    .build();
    let mut ss = service_session(
        &root,
        service,
        vec![environment],
        endpoints("svc", &[("main", 5)]),
        vec![],
    );
    assert!(ss.start().await.unwrap().is_ready());
    assert_eq!(ss.state(), ServiceSessionState::Running);
    let started = std::time::Instant::now();
    ss.end().await.unwrap();
    // SIGTERM was honored promptly (well under the 10s grace).
    assert!(started.elapsed() < Duration::from_secs(8));
    assert_eq!(ss.state(), ServiceSessionState::Ended);
    let exit = ss.run_exit().unwrap();
    assert_eq!(exit.state, ActionState::Canceled);
    assert!(exit.canceled);
    assert_eq!(
        read_trace(&trace),
        vec![
            "env-enter",
            "svc-enter",
            "run-got-term",
            "svc-exit",
            "env-exit"
        ]
    );
    assert!(ss.session().environments_entered().is_empty());
}

#[tokio::test]
async fn relaunch_within_session_preserves_on_enter_env_and_does_not_rerun_on_enter() {
    let root = TempDir::new().unwrap();
    let trace = root.path().join("trace.txt");
    let t = trace.display().to_string();
    // onRun announces readiness and then waits for a stop file before exiting
    // with status 1, so that READY is always observed while onRun is still
    // running (the RFC's "onRun exit wins" rule would otherwise make a fast
    // exit race the readiness line).
    let stop = root.path().join("stop");
    let stop_s = stop.display().to_string();
    let service = ServiceBuilder::new(
        "svc",
        &["main"],
        sh(&format!(
            "echo \"run TOKEN=$TOKEN WD=$OPENJD_SESSION_WORKING_DIR\" >> {t}; echo openjd_service_ready: ok; \
             while [ ! -e {stop_s} ]; do sleep 0.05; done; rm -f {stop_s}; exit 1"
        )),
    )
    .readiness(stdout_check(300))
    .on_enter(sh(&format!("echo svc-enter >> {t}; echo openjd_env: TOKEN=abc123")))
    .on_exit(sh(&format!("echo svc-exit >> {t}")))
    .build();
    let mut ss = service_session(
        &root,
        service,
        vec![],
        endpoints("svc", &[("main", 5)]),
        vec![],
    );
    let wd = ss.session().working_directory().display().to_string();

    assert!(ss.start().await.unwrap().is_ready());
    std::fs::write(&stop, b"").unwrap();
    let first = ss.wait_exit().await.unwrap();
    assert_eq!(first.exit_code, Some(1));
    assert!(!first.canceled);
    assert_eq!(ss.state(), ServiceSessionState::Exited);

    // Relaunch: same Session, same working directory, onEnter not re-run.
    ss.launch().await.unwrap();
    assert_eq!(ss.launch_count(), 2);
    assert_eq!(ss.state(), ServiceSessionState::Running);
    assert_eq!(ss.readiness(), Some(ServiceReadiness::Pending));
    assert!(ss.wait_ready().await.unwrap().is_ready());
    std::fs::write(&stop, b"").unwrap();
    let second = ss.wait_exit().await.unwrap();
    assert_eq!(second.exit_code, Some(1));
    ss.end().await.unwrap();

    assert_eq!(
        read_trace(&trace),
        vec![
            "svc-enter",
            format!("run TOKEN=abc123 WD={wd}").as_str(),
            format!("run TOKEN=abc123 WD={wd}").as_str(),
            "svc-exit",
        ]
    );
}

#[tokio::test]
async fn launch_is_rejected_while_on_run_is_running() {
    let root = TempDir::new().unwrap();
    let service = ServiceBuilder::new(
        "svc",
        &["main"],
        sh("echo openjd_service_ready: ok; sleep 30"),
    )
    .readiness(stdout_check(300))
    .build();
    let mut ss = service_session(
        &root,
        service,
        vec![],
        endpoints("svc", &[("main", 5)]),
        vec![],
    );
    assert!(ss.start().await.unwrap().is_ready());
    // Constraint 5: at most one running onRun per Session.
    let err = ss.launch().await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "Service Session must be in ENTERED or EXITED state, current: RUNNING"
    );
    ss.cancel_run(Some(Duration::ZERO));
    ss.wait_exit().await.unwrap();
    ss.end().await.unwrap();
    assert_eq!(
        ss.end().await.unwrap_err().to_string(),
        "Service Session must be in CREATED or ENTERED or RUNNING or EXITED or START_FAILED state, current: ENDED"
    );
}

// ────────────────────────────────────────────────────────────────────
// Start failures and onExit failures
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn on_enter_failure_is_a_start_failure_and_end_still_runs_on_exit() {
    let root = TempDir::new().unwrap();
    let trace = root.path().join("trace.txt");
    let t = trace.display().to_string();
    let environment = env(
        "outer",
        None,
        Some(sh(&format!("echo env-enter >> {t}"))),
        Some(sh(&format!("echo env-exit >> {t}"))),
    );
    let service = ServiceBuilder::new("svc", &["main"], sh(&format!("echo run >> {t}; sleep 30")))
        .on_enter(sh(&format!(
            "echo svc-enter >> {t}; echo openjd_fail: no license; exit 2"
        )))
        .on_exit(sh(&format!("echo svc-exit >> {t}")))
        .build();
    let mut ss = service_session(
        &root,
        service,
        vec![environment],
        endpoints("svc", &[("main", 5)]),
        vec![],
    );
    let err = ss.enter().await.unwrap_err();
    // RFC 0009 <ServiceActions>: the openjd_fail message accompanies the
    // start failure it explains.
    assert_eq!(
        err.to_string(),
        "Service 'svc' onEnter failed: exit code: 2; openjd_fail: no license"
    );
    assert_eq!(ss.state(), ServiceSessionState::StartFailed);
    let status = ss.action_status().unwrap();
    assert_eq!(status.state, ActionState::Failed);
    assert_eq!(status.fail_message.as_deref(), Some("no license"));
    assert_eq!(
        ss.launch().await.unwrap_err().to_string(),
        "Service Session must be in ENTERED or EXITED state, current: START_FAILED"
    );
    assert!(ss.wait_ready().await.is_err());
    ss.end().await.unwrap();
    // onExit runs because an action of the Service (onEnter) ran; onRun never did.
    assert_eq!(
        read_trace(&trace),
        vec!["env-enter", "svc-enter", "svc-exit", "env-exit"]
    );
}

#[tokio::test]
async fn cancel_handle_cancels_a_running_on_enter() {
    let root = TempDir::new().unwrap();
    let trace = root.path().join("trace.txt");
    let t = trace.display().to_string();
    let service = ServiceBuilder::new("svc", &["main"], sh("sleep 30"))
        .on_enter(sh_ntt(
            &format!(
                "trap 'echo enter-got-term >> {t}; exit 0' TERM; while true; do sleep 0.1; done"
            ),
            10,
        ))
        .on_exit(sh(&format!("echo svc-exit >> {t}")))
        .build();
    let mut ss = service_session(
        &root,
        service,
        vec![],
        endpoints("svc", &[("main", 5)]),
        vec![],
    );
    let handle = ss.cancel_handle();
    let canceler = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        handle.cancel(None, false)
    });
    let err = ss.enter().await.unwrap_err();
    assert_eq!(err.to_string(), "Service 'svc' onEnter failed: canceled");
    assert!(canceler.await.unwrap(), "an action was running to cancel");
    assert_eq!(ss.state(), ServiceSessionState::StartFailed);
    assert_eq!(ss.action_status().unwrap().state, ActionState::Canceled);
    ss.end().await.unwrap();
    assert_eq!(read_trace(&trace), vec!["enter-got-term", "svc-exit"]);
}

#[tokio::test]
async fn environment_on_enter_failure_is_a_start_failure_without_on_exit() {
    let root = TempDir::new().unwrap();
    let trace = root.path().join("trace.txt");
    let t = trace.display().to_string();
    let good = env(
        "good",
        None,
        Some(sh(&format!("echo good-enter >> {t}"))),
        Some(sh(&format!("echo good-exit >> {t}"))),
    );
    let bad = env(
        "bad",
        None,
        Some(sh(&format!("echo bad-enter >> {t}; exit 7"))),
        Some(sh(&format!("echo bad-exit >> {t}"))),
    );
    let service = ServiceBuilder::new("svc", &["main"], sh("sleep 30"))
        .on_enter(sh(&format!("echo svc-enter >> {t}")))
        .on_exit(sh(&format!("echo svc-exit >> {t}")))
        .build();
    let mut ss = service_session(
        &root,
        service,
        vec![good, bad],
        endpoints("svc", &[("main", 5)]),
        vec![],
    );
    let err = ss.enter().await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "Environment 'bad' onEnter failed: exit code: 7"
    );
    assert_eq!(ss.state(), ServiceSessionState::StartFailed);
    ss.end().await.unwrap();
    // No action of the Service ran, so onExit does not; both Environments
    // are exited (a failed onEnter still counts as entered).
    assert_eq!(
        read_trace(&trace),
        vec!["good-enter", "bad-enter", "bad-exit", "good-exit"]
    );
}

#[tokio::test]
async fn on_exit_failure_is_reported_after_full_teardown() {
    let root = TempDir::new().unwrap();
    let trace = root.path().join("trace.txt");
    let t = trace.display().to_string();
    let environment = env(
        "outer",
        None,
        None,
        Some(sh(&format!("echo env-exit >> {t}"))),
    );
    let service = ServiceBuilder::new(
        "svc",
        &["main"],
        sh("echo openjd_service_ready: ok; exit 0"),
    )
    .readiness(stdout_check(300))
    .on_exit(sh("echo openjd_fail: cleanup incomplete; exit 5"))
    .build();
    let mut config = session_config(&root, "svc-test:onexit");
    config.retain_working_dir = false;
    let mut ss = ServiceSession::with_config(ServiceSessionConfig {
        session: config,
        service,
        environments: vec![environment],
        environment_profiles: vec![],
        endpoints: endpoints("svc", &[("main", 5)]),
        in_scope_endpoints: vec![],
    })
    .unwrap();
    ss.start().await.unwrap();
    ss.wait_exit().await.unwrap();
    let wd = ss.session().working_directory().to_path_buf();
    let err = ss.end().await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "Service 'svc' onExit failed: exit code: 5; openjd_fail: cleanup incomplete"
    );
    assert_eq!(ss.state(), ServiceSessionState::Ended);
    assert_eq!(read_trace(&trace), vec!["env-exit"]);
    assert!(
        !wd.exists(),
        "working directory is deleted even after an onExit failure"
    );
}

// ────────────────────────────────────────────────────────────────────
// Environments and runScope
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn task_scoped_environment_is_not_entered() {
    testing_logger::setup();
    let root = TempDir::new().unwrap();
    let trace = root.path().join("trace.txt");
    let t = trace.display().to_string();
    let both = env("both", None, Some(sh(&format!("echo both >> {t}"))), None);
    let task_only = env(
        "task-only",
        Some(vec![RunScope::Task]),
        Some(sh(&format!("echo task-only >> {t}"))),
        None,
    );
    let service_only = env(
        "service-only",
        Some(vec![RunScope::Service]),
        Some(sh(&format!("echo service-only >> {t}"))),
        None,
    );
    let service = ServiceBuilder::new(
        "svc",
        &["main"],
        sh("echo openjd_service_ready: ok; sleep 30"),
    )
    .readiness(stdout_check(300))
    .build();
    let mut ss = service_session(
        &root,
        service,
        vec![both, task_only, service_only],
        endpoints("svc", &[("main", 5)]),
        vec![],
    );
    ss.enter().await.unwrap();
    assert_eq!(ss.session().environments_entered().len(), 2);
    assert_eq!(read_trace(&trace), vec!["both", "service-only"]);
    ss.end().await.unwrap();
    testing_logger::validate(|logs| {
        assert!(logs
            .iter()
            .any(|l| l.body
                == "Skipping Environment 'task-only': its runScope does not include SERVICE"));
    });
}

// ────────────────────────────────────────────────────────────────────
// Symbol scopes
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn service_symbols_resolve_in_on_run_args() {
    let root = TempDir::new().unwrap();
    let port = free_port();
    let db_port = free_port();
    let mut base = SymbolTable::new();
    base.set("Job.Name", ExprValue::String("render".into()))
        .unwrap();
    base.set("Step.Name", ExprValue::String("comp".into()))
        .unwrap();
    base.set("Param.Foo", ExprValue::String("bar".into()))
        .unwrap();
    base.set("Scale", ExprValue::Int(3)).unwrap(); // a <Service>.let value
    let on_run = Action {
        command: fs("sh"),
        args: Some(vec![
            fs("-c"),
            fs(r#"printf '%s\n' "$@"; echo openjd_service_ready: ok; sleep 30"#),
            fs("argv0"),
            fs("{{Service.svc.main.port}}"),
            fs("{{Service.svc.main.bindAddress}}"),
            fs("{{Service.svc.main.connectAddress}}"),
            fs("{{Service.db.p.port}}"),
            fs("{{Service.db.p.connectAddress}}"),
            fs("{{Session.WorkingDirectory}}"),
            fs("{{Param.Foo}}"),
            fs("{{Job.Name}}/{{Step.Name}}"),
            fs("{{Scaled}}"),
            fs("{{ join_host_port(Service.db.p.connectAddress, Service.db.p.port) }}"),
        ]),
        timeout: None,
        cancelation: None,
    };
    let service = ServiceBuilder::new("svc", &["main"], on_run)
        .readiness(stdout_check(300))
        .lets(&["Scaled = Scale * Service.svc.main.port"])
        .resolved_symtab(&base)
        .build();
    let db = ServiceEndpoints::new(
        "db",
        vec![(
            "p".to_string(),
            ServiceEndpoint {
                port: db_port,
                protocol: ServicePortProtocol::Tcp,
                bind_address: "0.0.0.0".into(),
                connect_address: "db.internal".into(),
            },
        )],
    );
    let mut config = session_config(&root, "svc-test:symbols");
    config.profile = Some(
        openjd_model::ModelProfile::new(openjd_model::SpecificationRevision::V2023_09)
            .with_extensions(
                [
                    openjd_model::ModelExtension::Expr,
                    openjd_model::ModelExtension::Service,
                ]
                .into_iter()
                .collect(),
            ),
    );
    let mut ss = ServiceSession::with_config(ServiceSessionConfig {
        session: config,
        service,
        environments: vec![],
        environment_profiles: vec![],
        endpoints: endpoints("svc", &[("main", port)]),
        in_scope_endpoints: vec![db],
    })
    .unwrap();
    assert!(ss.start().await.unwrap().is_ready());
    ss.cancel_run(Some(Duration::ZERO));
    let exit = ss.wait_exit().await.unwrap();
    let wd = ss.session().working_directory().display().to_string();
    assert_eq!(
        lines(&exit.stdout),
        vec![
            port.to_string().as_str(),
            "127.0.0.1",
            "127.0.0.1",
            db_port.to_string().as_str(),
            "db.internal",
            wd.as_str(),
            "bar",
            "render/comp",
            (3 * i64::from(port)).to_string().as_str(),
            format!("db.internal:{db_port}").as_str(),
            "openjd_service_ready: ok",
        ]
    );
    ss.end().await.unwrap();
}

#[tokio::test]
async fn earlier_service_bind_address_is_not_in_scope() {
    let root = TempDir::new().unwrap();
    let service = ServiceBuilder::new(
        "svc",
        &["main"],
        sh("echo {{Service.db.p.bindAddress}}; sleep 30"),
    )
    .readiness(stdout_check(300))
    .build();
    let db = ServiceEndpoints::new(
        "db",
        vec![(
            "p".to_string(),
            ServiceEndpoint {
                port: 9,
                protocol: ServicePortProtocol::Tcp,
                bind_address: "0.0.0.0".into(),
                connect_address: "db".into(),
            },
        )],
    );
    let mut ss = service_session(
        &root,
        service,
        vec![],
        endpoints("svc", &[("main", 5)]),
        vec![db],
    );
    // Resolution fails when the action is launched; that surfaces as a
    // failed onRun exit (an instance failure), not as a launch error.
    assert_eq!(
        ss.start().await.unwrap(),
        ServiceReadiness::ExitedBeforeReady
    );
    let exit = ss.wait_exit().await.unwrap();
    assert_eq!(exit.state, ActionState::Failed);
    assert_eq!(exit.exit_code, None);
    let msg = exit.fail_message.unwrap();
    assert!(
        msg.starts_with("Failed to resolve argument:") && msg.contains("Service.db.p.bindAddress"),
        "{msg}"
    );
    ss.end().await.unwrap();
}

#[tokio::test]
async fn service_file_resolves_for_embedded_file() {
    let root = TempDir::new().unwrap();
    let port = free_port();
    let service = ServiceBuilder::new(
        "svc",
        &["main"],
        Action {
            command: fs("{{Service.File.serve}}"),
            args: Some(vec![fs("{{Service.File.config}}")]),
            timeout: None,
            cancelation: None,
        },
    )
    .readiness(stdout_check(300))
    .embedded_file(
        "serve",
        "#!/bin/sh\necho \"config=$(cat \"$1\")\"\necho openjd_service_ready: ok\nsleep 30\n",
        true,
    )
    .embedded_file("config", "port={{Service.svc.main.port}}", false)
    .build();
    let mut ss = service_session(
        &root,
        service,
        vec![],
        endpoints("svc", &[("main", port)]),
        vec![],
    );
    assert!(ss.start().await.unwrap().is_ready());
    ss.cancel_run(Some(Duration::ZERO));
    let exit = ss.wait_exit().await.unwrap();
    assert_eq!(
        lines(&exit.stdout),
        vec![
            format!("config=port={port}").as_str(),
            "openjd_service_ready: ok"
        ]
    );
    // Files live in the Session's embedded-files directory.
    let files_dir = ss.session().files_directory();
    assert_eq!(std::fs::read_dir(files_dir).unwrap().count(), 2);
    ss.end().await.unwrap();
}

#[tokio::test]
async fn path_mapping_rules_are_materialized_and_applied() {
    let root = TempDir::new().unwrap();
    let service = ServiceBuilder::new(
        "svc",
        &["main"],
        sh("echo {{Session.HasPathMappingRules}}; echo {{Session.PathMappingRulesFile}}; echo {{Param.Scene}}; echo openjd_service_ready: ok; exit 0"),
    )
    .readiness(stdout_check(300))
    .build();
    let mut config = session_config(&root, "svc-test:pathmap");
    config.path_mapping_rules = Some(vec![PathMappingRule {
        source_path_format: PathFormat::Posix,
        source_path: "/mnt/artist".into(),
        destination_path: "/mnt/worker".into(),
    }]);
    config.job_parameter_values.insert(
        "Scene".into(),
        JobParameterValue {
            param_type: JobParameterType::Path,
            value: ExprValue::String("/mnt/artist/shot.blend".into()),
        },
    );
    let mut ss = ServiceSession::with_config(ServiceSessionConfig {
        session: config,
        service,
        environments: vec![],
        environment_profiles: vec![],
        endpoints: endpoints("svc", &[("main", 5)]),
        in_scope_endpoints: vec![],
    })
    .unwrap();
    ss.start().await.unwrap();
    let exit = ss.wait_exit().await.unwrap();
    let out = lines(&exit.stdout);
    assert_eq!(out[0], "true");
    let rules_file = PathBuf::from(out[1]);
    assert!(rules_file.starts_with(ss.session().working_directory()));
    let rules: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&rules_file).unwrap()).unwrap();
    assert_eq!(rules["version"], "pathmapping-1.0");
    assert_eq!(rules["path_mapping_rules"][0]["source_path"], "/mnt/artist");
    assert_eq!(
        rules["path_mapping_rules"][0]["destination_path"],
        "/mnt/worker"
    );
    assert_eq!(out[2], "/mnt/worker/shot.blend");
    ss.end().await.unwrap();
}

// ────────────────────────────────────────────────────────────────────
// Construction checks
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn with_config_rejects_incomplete_endpoint_assignment() {
    let root = TempDir::new().unwrap();
    let service = ServiceBuilder::new("svc", &["main", "metrics"], sh("sleep 1")).build();
    let err = ServiceSession::with_config(ServiceSessionConfig {
        session: session_config(&root, "svc-test:ports"),
        service,
        environments: vec![],
        environment_profiles: vec![],
        endpoints: endpoints("svc", &[("main", 5)]),
        in_scope_endpoints: vec![],
    })
    .err()
    .unwrap();
    assert_eq!(
        err.to_string(),
        "Service 'svc' port 'metrics' has no endpoint assignment"
    );
}

#[tokio::test]
async fn with_config_rejects_mismatched_service_name_and_command_without_check() {
    let root = TempDir::new().unwrap();
    let service = ServiceBuilder::new("svc", &["main"], sh("sleep 1")).build();
    let err = ServiceSession::with_config(ServiceSessionConfig {
        session: session_config(&root, "svc-test:name"),
        service,
        environments: vec![],
        environment_profiles: vec![],
        endpoints: endpoints("other", &[("main", 5)]),
        in_scope_endpoints: vec![],
    })
    .err()
    .unwrap();
    assert_eq!(
        err.to_string(),
        "Service 'svc' was given the endpoint assignment of Service 'other'"
    );

    let service = ServiceBuilder::new("svc", &["main"], sh("sleep 1"))
        .readiness(ServiceReadinessCheck::Command {
            interval_seconds: 5,
            timeout_seconds: 300,
        })
        .build();
    let err = ServiceSession::with_config(ServiceSessionConfig {
        session: session_config(&root, "svc-test:command"),
        service,
        environments: vec![],
        environment_profiles: vec![],
        endpoints: endpoints("svc", &[("main", 5)]),
        in_scope_endpoints: vec![],
    })
    .err()
    .unwrap();
    assert_eq!(
        err.to_string(),
        "Service 'svc': readiness check type is COMMAND but onReadinessCheck is not defined"
    );
}

#[tokio::test]
async fn with_config_checks_each_endpoint_protocol_against_the_declared_port() {
    let root = TempDir::new().unwrap();
    // The scheduler allocated `ingest` as TCP, but the port is UDP.
    let service = ServiceBuilder::new("svc", &["ingest", "api"], sh("sleep 1"))
        .udp_port("ingest")
        .readiness(tcp_check(&["api"], 300))
        .build();
    let err = ServiceSession::with_config(ServiceSessionConfig {
        session: session_config(&root, "svc-test:protocol"),
        service,
        environments: vec![],
        environment_profiles: vec![],
        endpoints: endpoints("svc", &[("ingest", 5), ("api", 6)]),
        in_scope_endpoints: vec![],
    })
    .err()
    .unwrap();
    assert_eq!(
        err.to_string(),
        "Service 'svc' port 'ingest' is declared UDP but its endpoint assignment is TCP"
    );

    // A TCP_CONNECT check that names the UDP port (model validation
    // forbids this; the runtime refuses rather than probing it).
    let service = ServiceBuilder::new("svc", &["ingest", "api"], sh("sleep 1"))
        .udp_port("ingest")
        .readiness(tcp_check(&["api", "ingest"], 300))
        .build();
    let err = ServiceSession::with_config(ServiceSessionConfig {
        session: session_config(&root, "svc-test:probe-udp"),
        service,
        environments: vec![],
        environment_profiles: vec![],
        endpoints: endpoints_with_protocols(
            "svc",
            &[
                ("ingest", 5, ServicePortProtocol::Udp),
                ("api", 6, ServicePortProtocol::Tcp),
            ],
        ),
        in_scope_endpoints: vec![],
    })
    .err()
    .unwrap();
    assert_eq!(
        err.to_string(),
        "Service 'svc': TCP_CONNECT readiness check names port 'ingest', whose protocol is UDP; \
         only TCP ports can be probed"
    );

    // Matching protocols construct fine.
    let service = ServiceBuilder::new("svc", &["ingest", "api"], sh("sleep 1"))
        .udp_port("ingest")
        .readiness(tcp_check(&["api"], 300))
        .build();
    ServiceSession::with_config(ServiceSessionConfig {
        session: session_config(&root, "svc-test:protocol-ok"),
        service,
        environments: vec![],
        environment_profiles: vec![],
        endpoints: endpoints_with_protocols(
            "svc",
            &[
                ("ingest", 5, ServicePortProtocol::Udp),
                ("api", 6, ServicePortProtocol::Tcp),
            ],
        ),
        in_scope_endpoints: vec![],
    })
    .unwrap();
}

/// A mixed TCP+UDP Service whose `TCP_CONNECT` check (the model's default
/// set: every TCP port) probes the TCP port only: the UDP port is never
/// connected to, and the Service is READY once the TCP listener is up.
#[tokio::test]
async fn tcp_connect_readiness_probes_only_the_listed_tcp_port_of_a_mixed_service() {
    let root = TempDir::new().unwrap();
    let tcp_port = free_port();
    let udp_port = {
        let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        s.local_addr().unwrap().port()
    };
    // Bind the UDP port and the TCP port; report both.
    let on_run = Action {
        command: fs("python3"),
        args: Some(vec![
            fs("-c"),
            fs(r#"
import socket, sys
u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
u.bind((sys.argv[1], int(sys.argv[2])))
t = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
t.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
t.bind((sys.argv[3], int(sys.argv[4])))
t.listen(5)
print("bound", sys.argv[2], sys.argv[4], flush=True)
while True:
    c, _ = t.accept()
    c.close()
"#),
            fs("{{Service.svc.ingest.bindAddress}}"),
            fs("{{Service.svc.ingest.port}}"),
            fs("{{Service.svc.api.bindAddress}}"),
            fs("{{Service.svc.api.port}}"),
        ]),
        timeout: None,
        cancelation: None,
    };
    let service = ServiceBuilder::new("svc", &["ingest", "api"], on_run)
        .udp_port("ingest")
        .readiness(tcp_check(&["api"], 30))
        .build();
    let mut ss = service_session(
        &root,
        service,
        vec![],
        endpoints_with_protocols(
            "svc",
            &[
                ("ingest", udp_port, ServicePortProtocol::Udp),
                ("api", tcp_port, ServicePortProtocol::Tcp),
            ],
        ),
        vec![],
    );
    assert!(ss.start().await.unwrap().is_ready());
    ss.cancel_run(Some(Duration::ZERO));
    let exit = ss.wait_exit().await.unwrap();
    assert_eq!(
        lines(&exit.stdout),
        vec![format!("bound {udp_port} {tcp_port}")]
    );
    ss.end().await.unwrap();
}

// ────────────────────────────────────────────────────────────────────
// COMMAND readiness (onReadinessCheck) and its concurrency rules
// ────────────────────────────────────────────────────────────────────

/// A check script that counts its invocations in `counter` and exits 0 on
/// the `ready_on`th. Each invocation prints `attempt N`.
fn counting_check(counter: &str, ready_on: u32) -> String {
    format!(
        "n=$(cat {counter} 2>/dev/null || echo 0); n=$((n+1)); echo $n > {counter}; \
         echo \"attempt $n\"; [ $n -ge {ready_on} ]"
    )
}

fn read_counter(path: &PathBuf) -> u32 {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .trim()
        .parse()
        .unwrap_or(0)
}

#[tokio::test]
async fn command_readiness_succeeds_on_third_attempt_and_check_stops_after_ready() {
    testing_logger::setup();
    let root = TempDir::new().unwrap();
    let counter = root.path().join("counter.txt");
    let c = counter.display().to_string();
    let service = ServiceBuilder::new("svc", &["main"], sh("echo service line; sleep 60"))
        .readiness(command_check(1, 300))
        .on_readiness_check(sh(&counting_check(&c, 3)))
        .build();
    let mut ss = service_session(
        &root,
        service,
        vec![],
        endpoints("svc", &[("main", 5)]),
        vec![],
    );
    let started = std::time::Instant::now();
    let ready = ss.start().await.unwrap();
    assert_eq!(ready, ServiceReadiness::Ready { message: None });
    // Two 1 s intervals separate the three invocations.
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(read_counter(&counter), 3);
    assert_eq!(ss.state(), ServiceSessionState::Running);

    // Rule 4: once READY the action is not run again.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(read_counter(&counter), 3);

    ss.cancel_run(Some(Duration::ZERO));
    let exit = ss.wait_exit().await.unwrap();
    assert_eq!(exit.state, ActionState::Canceled);
    ss.end().await.unwrap();

    testing_logger::validate(|logs| {
        let bodies: Vec<&str> = logs.iter().map(|l| l.body.as_str()).collect();
        assert!(bodies.contains(&"Readiness check: COMMAND (timeout 300s)"));
        // Rule 3: the check's output is tagged, onRun's is not.
        assert!(
            bodies.contains(&"[onReadinessCheck] attempt 1"),
            "{bodies:?}"
        );
        assert!(bodies.contains(&"[onReadinessCheck] attempt 2"));
        assert!(bodies.contains(&"[onReadinessCheck] attempt 3"));
        assert!(bodies.contains(&"service line"));
        assert!(!bodies.iter().any(|b| b.contains("] service line")));
        // Process-control lines of the check are tagged too.
        assert!(bodies.contains(&"[onReadinessCheck] Service 'svc' readiness check invocation 1"));
        assert!(bodies.contains(
            &"[onReadinessCheck] Readiness check invocation 1: not ready (exit code: 1)"
        ));
        assert!(bodies
            .contains(&"[onReadinessCheck] Readiness check invocation 3: ready (exit code: 0)"));
        assert!(bodies.contains(&"Service 'svc' is READY"));
        assert_eq!(
            bodies
                .iter()
                .filter(|b| b
                    .starts_with("[onReadinessCheck] Service 'svc' readiness check invocation"))
                .count(),
            3
        );
    });
}

/// `SessionConfig::log_tag`: every record of the Service Session — the
/// scope Environment's output, `onEnter`'s, `onRun`'s, the check's — is
/// prefixed with the session tag, the concurrent check's lines with its
/// action tag after it, and each section banner becomes one tagged line.
#[tokio::test]
async fn session_log_tag_prefixes_every_record_and_collapses_banners() {
    testing_logger::setup();
    let root = TempDir::new().unwrap();
    let counter = root.path().join("counter.txt");
    let c = counter.display().to_string();
    let service = ServiceBuilder::new("svc", &["main"], sh("echo service line; sleep 60"))
        .readiness(command_check(1, 300))
        .on_readiness_check(sh(&counting_check(&c, 1)))
        .on_enter(sh("echo enter line"))
        .build();
    let environment = env("Scope", None, Some(sh("echo env line")), None);
    let mut config = session_config(&root, "svc-test:tag");
    config.log_tag = Some("Service svc".into());
    let mut ss = ServiceSession::with_config(ServiceSessionConfig {
        session: config,
        service,
        environments: vec![environment],
        environment_profiles: vec![],
        endpoints: endpoints("svc", &[("main", 5)]),
        in_scope_endpoints: vec![],
    })
    .unwrap();
    assert_eq!(
        ss.start().await.unwrap(),
        ServiceReadiness::Ready { message: None }
    );
    ss.cancel_run(Some(Duration::ZERO));
    let _ = ss.wait_exit().await.unwrap();
    ss.end().await.unwrap();

    testing_logger::validate(|logs| {
        let bodies: Vec<&str> = logs.iter().map(|l| l.body.as_str()).collect();
        for expected in [
            "[Service svc] --------- Starting Service: svc",
            "[Service svc] --------- Entering Environment: Scope",
            "[Service svc] env line",
            "[Service svc] --------- Service onEnter: svc",
            "[Service svc] enter line",
            "[Service svc] --------- Service onRun: svc (launch 1)",
            "[Service svc] service line",
            "[Service svc] [onReadinessCheck] attempt 1",
            "[Service svc] [onReadinessCheck] Service 'svc' readiness check invocation 1",
            "[Service svc] --------- Ending Service: svc",
            "[Service svc] --------- Exiting Environment: Scope",
        ] {
            assert!(
                bodies.contains(&expected),
                "missing {expected:?} in {bodies:?}"
            );
        }
        // No four-line banner separators from this Session.
        assert!(
            !bodies.contains(&"=============================================="),
            "{bodies:?}"
        );
        // Every COMMAND_OUTPUT line carries the tag.
        for log in logs {
            if log.body.ends_with(" line") || log.body.ends_with("attempt 1") {
                assert!(log.body.starts_with("[Service svc] "), "{}", log.body);
            }
        }
    });
}

#[tokio::test]
async fn command_readiness_waits_interval_between_invocations() {
    let root = TempDir::new().unwrap();
    let counter = root.path().join("counter.txt");
    let times = root.path().join("times.txt");
    let c = counter.display().to_string();
    let service = ServiceBuilder::new("svc", &["main"], sh("sleep 60"))
        .readiness(command_check(2, 300))
        .on_readiness_check(sh(&format!(
            "date +%s%N >> {}; {}",
            times.display(),
            counting_check(&c, 2)
        )))
        .build();
    let mut ss = service_session(
        &root,
        service,
        vec![],
        endpoints("svc", &[("main", 5)]),
        vec![],
    );
    assert!(ss.start().await.unwrap().is_ready());
    let stamps: Vec<u128> = read_trace(&times)
        .iter()
        .map(|s| s.parse().unwrap())
        .collect();
    assert_eq!(stamps.len(), 2);
    // The second invocation starts intervalSeconds (2 s) after the first ends.
    let gap = Duration::from_nanos((stamps[1] - stamps[0]) as u64);
    assert!(gap >= Duration::from_secs(2), "gap {gap:?}");
    assert!(gap < Duration::from_secs(5), "gap {gap:?}");
    ss.cancel_run(Some(Duration::ZERO));
    ss.wait_exit().await.unwrap();
    ss.end().await.unwrap();
}

#[tokio::test]
async fn command_readiness_invocation_timeout_counts_as_not_ready() {
    testing_logger::setup();
    let root = TempDir::new().unwrap();
    let counter = root.path().join("counter.txt");
    let c = counter.display().to_string();
    // The first invocation hangs past its 1 s timeout; the second exits 0.
    let service = ServiceBuilder::new("svc", &["main"], sh("sleep 60"))
        .readiness(command_check(1, 300))
        .on_readiness_check(sh_timeout(
            &format!(
                "n=$(cat {c} 2>/dev/null || echo 0); n=$((n+1)); echo $n > {c}; \
                 if [ $n -eq 1 ]; then sleep 30; fi; exit 0"
            ),
            1,
        ))
        .build();
    let mut ss = service_session(
        &root,
        service,
        vec![],
        endpoints("svc", &[("main", 5)]),
        vec![],
    );
    let started = std::time::Instant::now();
    assert!(ss.start().await.unwrap().is_ready());
    // 1 s timeout + 1 s interval, well under the 30 s the first check slept.
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(read_counter(&counter), 2);
    ss.cancel_run(Some(Duration::ZERO));
    ss.wait_exit().await.unwrap();
    ss.end().await.unwrap();
    testing_logger::validate(|logs| {
        let bodies: Vec<&str> = logs.iter().map(|l| l.body.as_str()).collect();
        assert!(
            bodies.contains(
                &"[onReadinessCheck] Readiness check invocation 1: not ready (exceeded its timeout)"
            ),
            "{bodies:?}"
        );
        assert!(bodies
            .contains(&"[onReadinessCheck] Readiness check invocation 2: ready (exit code: 0)"));
    });
}

#[tokio::test]
async fn command_readiness_timeout_is_not_a_service_failure_and_stops_the_check() {
    testing_logger::setup();
    let root = TempDir::new().unwrap();
    let counter = root.path().join("counter.txt");
    let c = counter.display().to_string();
    let service = ServiceBuilder::new("svc", &["main"], sh("sleep 60"))
        .readiness(command_check(1, 2))
        .on_readiness_check(sh(&counting_check(&c, 1000)))
        .build();
    let mut ss = service_session(
        &root,
        service,
        vec![],
        endpoints("svc", &[("main", 5)]),
        vec![],
    );
    assert_eq!(ss.start().await.unwrap(), ServiceReadiness::TimedOut);
    // onRun is still running: the restart decision is the caller's.
    assert_eq!(ss.state(), ServiceSessionState::Running);
    assert!(ss.run_exit().is_none());
    let n = read_counter(&counter);
    assert!((1..=3).contains(&n), "{n}");
    // The check does not run again after the decision.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(read_counter(&counter), n);
    ss.cancel_run(Some(Duration::ZERO));
    let exit = ss.wait_exit().await.unwrap();
    assert!(exit.canceled);
    ss.end().await.unwrap();
    testing_logger::validate(|logs| {
        let bodies: Vec<&str> = logs.iter().map(|l| l.body.as_str()).collect();
        assert!(bodies
            .contains(&"Service 'svc' did not become READY within 2s (COMMAND readiness check)"));
    });
}

#[tokio::test]
async fn on_run_exit_during_check_invocation_cancels_it_with_its_method() {
    testing_logger::setup();
    let root = TempDir::new().unwrap();
    let trace = root.path().join("trace.txt");
    let t = trace.display().to_string();
    // The check hangs; its TERM trap records the cancel. onRun exits after
    // 1 s, while the first invocation is in flight.
    let service = ServiceBuilder::new(
        "svc",
        &["main"],
        sh(&format!("sleep 1; echo run-exit >> {t}; exit 0")),
    )
    .readiness(command_check(1, 300))
    .on_readiness_check(sh_ntt(
        &format!("trap 'echo check-term >> {t}; exit 143' TERM; echo check-start >> {t}; while true; do sleep 0.1; done"),
        5,
    ))
    .build();
    let mut ss = service_session(
        &root,
        service,
        vec![],
        endpoints("svc", &[("main", 5)]),
        vec![],
    );
    let started = std::time::Instant::now();
    assert_eq!(
        ss.start().await.unwrap(),
        ServiceReadiness::ExitedBeforeReady
    );
    let exit = ss.wait_exit().await.unwrap();
    assert_eq!(exit.state, ActionState::Success);
    assert!(!exit.canceled);
    // The in-flight check was SIGTERMed (NOTIFY_THEN_TERMINATE), not left
    // to its 30 s default timeout.
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(
        read_trace(&trace),
        vec!["check-start", "run-exit", "check-term"]
    );
    ss.end().await.unwrap();
    testing_logger::validate(|logs| {
        let bodies: Vec<&str> = logs.iter().map(|l| l.body.as_str()).collect();
        assert!(
            bodies.contains(
                &"[onReadinessCheck] Canceling readiness check invocation 1: its result will be discarded"
            ),
            "{bodies:?}"
        );
        assert!(bodies.contains(&"Service 'svc' onRun exited before becoming READY"));
        // The discarded invocation reports no readiness outcome.
        assert!(!bodies
            .iter()
            .any(|b| b.starts_with("[onReadinessCheck] Readiness check invocation 1:")));
    });
}

#[tokio::test]
async fn messages_on_check_stdout_are_logged_and_ignored() {
    testing_logger::setup();
    let root = TempDir::new().unwrap();
    let counter = root.path().join("counter.txt");
    let c = counter.display().to_string();
    // Invocation 1 emits every message kind and exits 1: none is honored,
    // including openjd_service_ready. Invocation 2 emits openjd_fail and
    // exits 0: the exit status wins — READY.
    let service = ServiceBuilder::new(
        "svc",
        &["main"],
        sh("echo openjd_status: from onRun; sleep 60"),
    )
    .readiness(command_check(1, 300))
    .on_readiness_check(sh(&format!(
        "n=$(cat {c} 2>/dev/null || echo 0); n=$((n+1)); echo $n > {c}; \
         if [ $n -eq 1 ]; then \
           echo openjd_service_ready: not really; \
           echo openjd_status: from check; \
           echo openjd_progress: 50; \
           echo openjd_env: FROM_CHECK=1; \
           echo openjd_unset_env: HOME; \
           echo openjd_redacted_env: SECRET=hunter2; \
           echo openjd_fail: not yet; \
           exit 1; \
         fi; \
         echo openjd_fail: ignored on exit 0; exit 0"
    )))
    .build();
    let mut ss = service_session(
        &root,
        service,
        vec![],
        endpoints("svc", &[("main", 5)]),
        vec![],
    );
    ss.enter().await.unwrap();
    ss.launch().await.unwrap();
    let ready = ss.wait_ready().await.unwrap();
    assert_eq!(ready, ServiceReadiness::Ready { message: None });
    assert_eq!(read_counter(&counter), 2);
    // The Service's status comes from onRun only.
    let status = ss.action_status().unwrap();
    assert_eq!(status.state, ActionState::Running);
    assert_eq!(status.status_message.as_deref(), Some("from onRun"));
    assert_eq!(status.progress, None);
    assert_eq!(status.fail_message, None);
    ss.cancel_run(Some(Duration::ZERO));
    let exit = ss.wait_exit().await.unwrap();
    assert_eq!(exit.fail_message, None);
    ss.end().await.unwrap();
    testing_logger::validate(|logs| {
        let bodies: Vec<&str> = logs.iter().map(|l| l.body.as_str()).collect();
        for what in [
            "openjd_service_ready",
            "openjd_status",
            "openjd_progress",
            "openjd_env",
            "openjd_unset_env",
            "openjd_redacted_env",
            "openjd_fail",
        ] {
            let expected = format!(
                "[onReadinessCheck] Ignoring {what} from Service 'svc' onReadinessCheck: \
                 messages on the readiness check's stdout are not honored"
            );
            assert!(
                bodies.contains(&expected.as_str()),
                "missing {expected:?}: {bodies:?}"
            );
        }
        // The redacted value never reaches the log.
        assert!(!bodies.iter().any(|b| b.contains("hunter2")));
        assert!(bodies.contains(&"[onReadinessCheck] openjd_redacted_env: SECRET=********"));
    });
}

#[tokio::test]
async fn end_during_check_invocation_cancels_check_before_on_exit() {
    let root = TempDir::new().unwrap();
    let trace = root.path().join("trace.txt");
    let t = trace.display().to_string();
    let service = ServiceBuilder::new(
        "svc",
        &["main"],
        sh_ntt(
            &format!("trap 'echo run-term >> {t}; exit 0' TERM; while true; do sleep 0.1; done"),
            5,
        ),
    )
    .readiness(command_check(1, 300))
    .on_readiness_check(sh_ntt(
        &format!("trap 'echo check-term >> {t}; exit 143' TERM; echo check-start >> {t}; while true; do sleep 0.1; done"),
        5,
    ))
    .on_exit(sh(&format!("echo svc-exit >> {t}")))
    .build();
    let mut ss = service_session(
        &root,
        service,
        vec![],
        endpoints("svc", &[("main", 5)]),
        vec![],
    );
    ss.enter().await.unwrap();
    ss.launch().await.unwrap();
    // Let the first invocation start.
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(ss.readiness(), Some(ServiceReadiness::Pending));
    let started = std::time::Instant::now();
    ss.end().await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(8));
    assert_eq!(ss.state(), ServiceSessionState::Ended);
    let got = read_trace(&trace);
    assert_eq!(got[0], "check-start");
    assert_eq!(got.last().unwrap(), "svc-exit");
    assert!(got.contains(&"run-term".to_string()));
    assert!(got.contains(&"check-term".to_string()));
    assert_eq!(got.len(), 4);
    assert_eq!(ss.readiness(), Some(ServiceReadiness::ExitedBeforeReady));
}

#[tokio::test]
async fn service_file_usable_from_check_and_never_rewritten() {
    let root = TempDir::new().unwrap();
    let counter = root.path().join("counter.txt");
    let c = counter.display().to_string();
    let service = ServiceBuilder::new(
        "svc",
        &["main"],
        Action {
            command: fs("{{Service.File.serve}}"),
            args: None,
            timeout: None,
            cancelation: None,
        },
    )
    .readiness(command_check(1, 300))
    .on_readiness_check(Action {
        command: fs("{{Service.File.probe}}"),
        args: Some(vec![fs("{{Service.File.config}}")]),
        timeout: None,
        cancelation: None,
    })
    .embedded_file("serve", "#!/bin/sh\nsleep 60\n", true)
    .embedded_file(
        "probe",
        &format!(
            "#!/bin/sh\necho \"config=$(cat \\\"$1\\\")\"\n{}\n",
            counting_check(&c, 2)
        ),
        true,
    )
    .embedded_file("config", "port={{Service.svc.main.port}}", false)
    .build();
    let mut ss = service_session(
        &root,
        service,
        vec![],
        endpoints("svc", &[("main", 4242)]),
        vec![],
    );
    ss.enter().await.unwrap();
    // Rule 1: embedded files are written once, at enter(); no later action
    // (check invocations, relaunch) rewrites a file onRun was given.
    let files_dir = ss.session().files_directory().to_path_buf();
    let snapshot = || -> Vec<(PathBuf, std::time::SystemTime, Vec<u8>)> {
        let mut v: Vec<_> = std::fs::read_dir(&files_dir)
            .unwrap()
            .map(|e| {
                let p = e.unwrap().path();
                let m = std::fs::metadata(&p).unwrap().modified().unwrap();
                let d = std::fs::read(&p).unwrap();
                (p, m, d)
            })
            .collect();
        v.sort();
        v
    };
    let before = snapshot();
    assert_eq!(before.len(), 3);
    ss.launch().await.unwrap();
    assert!(ss.wait_ready().await.unwrap().is_ready());
    assert_eq!(read_counter(&counter), 2);
    assert_eq!(snapshot(), before);
    ss.cancel_run(Some(Duration::ZERO));
    ss.wait_exit().await.unwrap();
    // Relaunch: a fresh readiness check against unchanged files.
    std::fs::remove_file(&counter).unwrap();
    ss.launch().await.unwrap();
    assert!(ss.wait_ready().await.unwrap().is_ready());
    assert_eq!(snapshot(), before);
    ss.cancel_run(Some(Duration::ZERO));
    ss.wait_exit().await.unwrap();
    ss.end().await.unwrap();
}

// ────────────────────────────────────────────────────────────────────
// onWrapService* hooks (WRAP_ACTIONS + SERVICE)
// ────────────────────────────────────────────────────────────────────

/// A wrap hook: a bash wrapper that records `<tag>` and the
/// `WrappedService.*` values to `trace`, then execs the wrapped command with
/// its args.
fn forwarding_hook(tag: &str, trace: &str) -> Action {
    let script = format!(
        "echo \"[{tag}] name={{{{WrappedService.Name}}}} \
         ports={{{{len(WrappedService.Ports)}}}} \
         p0={{{{WrappedService.PortNames[0]}}}}:{{{{WrappedService.Ports[0]}}}}@{{{{WrappedService.BindAddresses[0]}}}}/{{{{lower(WrappedService.Protocols[0])}}}} \
         p1={{{{WrappedService.PortNames[1]}}}}:{{{{WrappedService.Ports[1]}}}}@{{{{WrappedService.BindAddresses[1]}}}}/{{{{lower(WrappedService.Protocols[1])}}}} \
         cmd={{{{WrappedAction.Command}}}} nargs=$#\" >> '{trace}'\n\
         exec {{{{WrappedAction.Command}}}} \"$@\""
    );
    Action {
        command: fs("bash"),
        args: Some(vec![
            fs("-c"),
            fs(&script),
            fs("--"),
            fs("{{WrappedAction.Args}}"),
        ]),
        timeout: None,
        cancelation: None,
    }
}

fn service_wrap_env(
    name: &str,
    run_scope: Option<Vec<RunScope>>,
    hooks: [Option<Action>; 4],
) -> Environment {
    let [enter, run, check, exit] = hooks;
    Environment {
        name: name.into(),
        description: None,
        run_scope,
        script: Some(EnvironmentScript {
            let_bindings: None,
            actions: EnvironmentActions {
                on_enter: Some(sh("true")),
                on_wrap_env_enter: Some(sh("true")),
                on_wrap_task_run: None,
                on_wrap_env_exit: Some(sh("true")),
                on_wrap_service_enter: enter,
                on_wrap_service_run: run,
                on_wrap_service_readiness_check: check,
                on_wrap_service_exit: exit,
                on_exit: None,
            },
            embedded_files: None,
        }),
        variables: None,
        resolved_symtab: None,
    }
}

#[tokio::test]
async fn wrap_hooks_replace_the_service_actions_and_forward_messages() {
    testing_logger::setup();
    let root = TempDir::new().unwrap();
    let trace = root.path().join("trace.txt");
    let t = trace.display().to_string();
    let wrapper = service_wrap_env(
        "Wrapper",
        None,
        [
            Some(forwarding_hook("onWrapServiceEnter", &t)),
            Some(forwarding_hook("onWrapServiceRun", &t)),
            Some(forwarding_hook("onWrapServiceReadinessCheck", &t)),
            Some(forwarding_hook("onWrapServiceExit", &t)),
        ],
    );
    let service = ServiceBuilder::new(
        "svc",
        &["main", "metrics"],
        sh(&format!(
            "echo from-enter=$FROM_ENTER; echo openjd_service_ready: up on {{{{Service.svc.main.port}}}}; \
             echo run >> {t}; sleep 60"
        )),
    )
    .readiness(stdout_check(300))
    .on_enter(sh("echo openjd_env: FROM_ENTER=yes"))
    .on_exit(sh(&format!("echo exit >> {t}")))
    // `metrics` is a UDP port: WrappedService.Protocols reports it.
    .udp_port("metrics")
    .build();
    let mut ss = service_session(
        &root,
        service,
        vec![wrapper],
        endpoints_with_protocols(
            "svc",
            &[
                ("main", 4100, ServicePortProtocol::Tcp),
                ("metrics", 4101, ServicePortProtocol::Udp),
            ],
        ),
        vec![],
    );
    let ready = ss.start().await.unwrap();
    // openjd_service_ready is honored through the wrapper's forwarded stdout.
    assert_eq!(
        ready,
        ServiceReadiness::Ready {
            message: Some("up on 4100".into())
        }
    );
    ss.cancel_run(Some(Duration::ZERO));
    let exit = ss.wait_exit().await.unwrap();
    // openjd_env from the wrapped onEnter reached onRun.
    assert_eq!(lines(&exit.stdout)[0], "from-enter=yes");
    ss.end().await.unwrap();

    let values =
        "name=svc ports=2 p0=main:4100@127.0.0.1/tcp p1=metrics:4101@127.0.0.1/udp cmd=sh nargs=2";
    assert_eq!(
        read_trace(&trace),
        vec![
            format!("[onWrapServiceEnter] {values}"),
            format!("[onWrapServiceRun] {values}"),
            "run".to_string(),
            format!("[onWrapServiceExit] {values}"),
            "exit".to_string(),
        ]
    );
    testing_logger::validate(|logs| {
        let bodies: Vec<&str> = logs.iter().map(|l| l.body.as_str()).collect();
        assert!(bodies.contains(
            &"Service 'svc' onEnter: running onWrapServiceEnter of wrapping Environment 'Wrapper' in its place"
        ));
        assert!(bodies.contains(
            &"Service 'svc' onRun: running onWrapServiceRun of wrapping Environment 'Wrapper' in its place"
        ));
        assert!(bodies.contains(
            &"Service 'svc' onExit: running onWrapServiceExit of wrapping Environment 'Wrapper' in its place"
        ));
        // STDOUT readiness: onWrapServiceReadinessCheck never runs.
        assert!(!bodies
            .iter()
            .any(|b| b.contains("onWrapServiceReadinessCheck")));
        assert!(bodies.contains(&"from-enter=yes"));
    });
}

#[tokio::test]
async fn wrap_hooks_run_only_for_actions_the_service_defines() {
    let root = TempDir::new().unwrap();
    let trace = root.path().join("trace.txt");
    let t = trace.display().to_string();
    let wrapper = service_wrap_env(
        "Wrapper",
        Some(vec![RunScope::Service]),
        [
            Some(forwarding_hook("onWrapServiceEnter", &t)),
            Some(forwarding_hook("onWrapServiceRun", &t)),
            Some(forwarding_hook("onWrapServiceReadinessCheck", &t)),
            Some(forwarding_hook("onWrapServiceExit", &t)),
        ],
    );
    // No onEnter, no onExit, TCP_CONNECT readiness: only onRun exists to wrap.
    let service = ServiceBuilder::new("svc", &["main", "metrics"], python_listener("svc", "x"))
        .readiness(tcp_check(&["main"], 300))
        .build();
    let port = free_port();
    let mut ss = service_session(
        &root,
        service,
        vec![wrapper],
        endpoints("svc", &[("main", port), ("metrics", 7)]),
        vec![],
    );
    assert!(ss.start().await.unwrap().is_ready());
    ss.end().await.unwrap();
    assert_eq!(
        read_trace(&trace),
        vec![format!(
            "[onWrapServiceRun] name=svc ports=2 p0=main:{port}@127.0.0.1/tcp p1=metrics:7@127.0.0.1/tcp cmd=python3 nargs=5"
        )]
    );
}

#[tokio::test]
async fn wrapped_readiness_check_runs_concurrently_with_wrapped_on_run() {
    testing_logger::setup();
    let root = TempDir::new().unwrap();
    let trace = root.path().join("trace.txt");
    let counter = root.path().join("counter.txt");
    let t = trace.display().to_string();
    let c = counter.display().to_string();
    let wrapper = service_wrap_env(
        "Wrapper",
        None,
        [
            None,
            Some(forwarding_hook("onWrapServiceRun", &t)),
            Some(forwarding_hook("onWrapServiceReadinessCheck", &t)),
            None,
        ],
    );
    let service = ServiceBuilder::new(
        "svc",
        &["main", "metrics"],
        sh(&format!(
            "echo run-start >> {t}; echo service line; while true; do sleep 0.1; done"
        )),
    )
    .readiness(command_check(1, 300))
    // READY on the 2nd attempt, but only once onRun has started.
    .on_readiness_check(sh(&format!(
        "grep -q run-start {t} || exit 1; {}",
        counting_check(&c, 2)
    )))
    .build();
    let mut ss = service_session(
        &root,
        service,
        vec![wrapper],
        endpoints("svc", &[("main", 4200), ("metrics", 4201)]),
        vec![],
    );
    assert_eq!(
        ss.start().await.unwrap(),
        ServiceReadiness::Ready { message: None }
    );
    assert_eq!(ss.state(), ServiceSessionState::Running);
    ss.cancel_run(Some(Duration::ZERO));
    ss.wait_exit().await.unwrap();
    ss.end().await.unwrap();
    let got = read_trace(&trace);
    let hooks: Vec<&String> = got.iter().filter(|l| l.starts_with('[')).collect();
    let values =
        "name=svc ports=2 p0=main:4200@127.0.0.1/tcp p1=metrics:4201@127.0.0.1/tcp cmd=sh nargs=2";
    assert_eq!(hooks[0], &format!("[onWrapServiceRun] {values}"));
    // Every check invocation went through the hook, while onRun ran.
    assert!(hooks.len() >= 3, "{got:?}");
    assert!(hooks[1..]
        .iter()
        .all(|h| **h == format!("[onWrapServiceReadinessCheck] {values}")));
    testing_logger::validate(|logs| {
        let bodies: Vec<&str> = logs.iter().map(|l| l.body.as_str()).collect();
        // The wrapped check's output is attributed to the hook that ran.
        assert!(
            bodies.contains(&"[onWrapServiceReadinessCheck] attempt 1"),
            "{bodies:?}"
        );
        assert!(bodies.contains(&"[onWrapServiceReadinessCheck] attempt 2"));
        assert!(bodies.contains(&"service line"));
        assert!(bodies.contains(
            &"Service 'svc' onReadinessCheck: running onWrapServiceReadinessCheck of wrapping Environment 'Wrapper' in its place"
        ));
    });
}

#[tokio::test]
async fn task_only_wrapper_is_skipped_entirely() {
    testing_logger::setup();
    let root = TempDir::new().unwrap();
    let trace = root.path().join("trace.txt");
    let t = trace.display().to_string();
    // A `[TASK]` wrapping Environment: RFC 0008's three hooks, no Service
    // hooks. It is not entered in a Service Session at all.
    let mut wrapper = service_wrap_env(
        "TaskWrapper",
        Some(vec![RunScope::Task]),
        [None, None, None, None],
    );
    wrapper.script.as_mut().unwrap().actions.on_wrap_task_run = Some(sh("true"));
    wrapper.script.as_mut().unwrap().actions.on_enter =
        Some(sh(&format!("echo wrapper-enter >> {t}")));
    let service = ServiceBuilder::new(
        "svc",
        &["main"],
        sh("echo openjd_service_ready: ok; sleep 60"),
    )
    .readiness(stdout_check(300))
    .on_enter(sh(&format!("echo svc-enter >> {t}")))
    .build();
    let mut ss = service_session(
        &root,
        service,
        vec![wrapper],
        endpoints("svc", &[("main", 5)]),
        vec![],
    );
    assert!(ss.start().await.unwrap().is_ready());
    assert!(ss.session().environments_entered().is_empty());
    ss.end().await.unwrap();
    assert_eq!(read_trace(&trace), vec!["svc-enter"]);
    testing_logger::validate(|logs| {
        let bodies: Vec<&str> = logs.iter().map(|l| l.body.as_str()).collect();
        assert!(bodies.contains(
            &"Skipping Environment 'TaskWrapper': its runScope does not include SERVICE"
        ));
        assert!(!bodies.iter().any(|b| b.contains("in its place")));
    });
}

#[tokio::test]
async fn wrapping_scope_environment_wraps_inner_environments_and_the_service_actions() {
    testing_logger::setup();
    let root = TempDir::new().unwrap();
    let trace = root.path().join("trace.txt");
    let t = trace.display().to_string();
    // A plain scope Environment, then a wrapping scope Environment with
    // runScope [SERVICE] (no onWrapTaskRun), then an inner scope
    // Environment that the wrapper's onWrapEnvEnter/Exit wrap.
    let scope_env = env(
        "JobEnv",
        None,
        Some(sh(&format!("echo job-enter >> {t}"))),
        Some(sh(&format!("echo job-exit >> {t}"))),
    );
    let mut wrapper = service_wrap_env(
        "Container",
        Some(vec![RunScope::Service]),
        [
            Some(forwarding_hook("onWrapServiceEnter", &t)),
            Some(forwarding_hook("onWrapServiceRun", &t)),
            None,
            Some(forwarding_hook("onWrapServiceExit", &t)),
        ],
    );
    {
        let actions = &mut wrapper.script.as_mut().unwrap().actions;
        actions.on_enter = Some(sh(&format!("echo container-enter >> {t}")));
        actions.on_exit = Some(sh(&format!("echo container-exit >> {t}")));
        actions.on_wrap_env_enter = Some(Action {
            command: fs("sh"),
            args: Some(vec![
                fs("-c"),
                fs(&format!(
                    "echo \"[onWrapEnvEnter] env={{{{WrappedEnv.Name}}}}\" >> {t}; exec \"$@\""
                )),
                fs("argv0"),
                fs("{{WrappedAction.Command}}"),
                fs("{{WrappedAction.Args}}"),
            ]),
            timeout: None,
            cancelation: None,
        });
        actions.on_wrap_env_exit = Some(Action {
            command: fs("sh"),
            args: Some(vec![
                fs("-c"),
                fs(&format!(
                    "echo \"[onWrapEnvExit] env={{{{WrappedEnv.Name}}}}\" >> {t}; exec \"$@\""
                )),
                fs("argv0"),
                fs("{{WrappedAction.Command}}"),
                fs("{{WrappedAction.Args}}"),
            ]),
            timeout: None,
            cancelation: None,
        });
    }
    let inner = env(
        "Inner",
        None,
        Some(sh(&format!("echo inner-enter >> {t}"))),
        Some(sh(&format!("echo inner-exit >> {t}"))),
    );
    let service = ServiceBuilder::new(
        "svc",
        &["main", "metrics"],
        sh(&format!(
            "echo openjd_service_ready: up; echo run >> {t}; sleep 60"
        )),
    )
    .readiness(stdout_check(300))
    .on_enter(sh(&format!("echo svc-enter >> {t}")))
    .on_exit(sh(&format!("echo svc-exit >> {t}")))
    .build();
    let mut ss = service_session(
        &root,
        service,
        vec![scope_env, wrapper, inner],
        endpoints("svc", &[("main", 4200), ("metrics", 4201)]),
        vec![],
    );
    assert!(ss.start().await.unwrap().is_ready());
    ss.end().await.unwrap();

    let values =
        "name=svc ports=2 p0=main:4200@127.0.0.1/tcp p1=metrics:4201@127.0.0.1/tcp cmd=sh nargs=2";
    assert_eq!(
        read_trace(&trace),
        vec![
            "job-enter".to_string(),
            "container-enter".to_string(),
            "[onWrapEnvEnter] env=Inner".to_string(),
            "inner-enter".to_string(),
            format!("[onWrapServiceEnter] {values}"),
            "svc-enter".to_string(),
            format!("[onWrapServiceRun] {values}"),
            "run".to_string(),
            format!("[onWrapServiceExit] {values}"),
            "svc-exit".to_string(),
            "[onWrapEnvExit] env=Inner".to_string(),
            "inner-exit".to_string(),
            "container-exit".to_string(),
            "job-exit".to_string(),
        ]
    );
    testing_logger::validate(|logs| {
        let bodies: Vec<&str> = logs.iter().map(|l| l.body.as_str()).collect();
        assert!(bodies.contains(
            &"Service 'svc' onRun: running onWrapServiceRun of wrapping Environment 'Container' in its place"
        ));
    });
}
