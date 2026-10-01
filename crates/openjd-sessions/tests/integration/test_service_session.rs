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

struct ServiceBuilder {
    service: Service,
}

impl ServiceBuilder {
    fn new(name: &str, ports: &[&str], on_run: Action) -> Self {
        Self {
            service: Service {
                name: name.into(),
                description: None,
                host_requirements: None,
                ports: ports
                    .iter()
                    .map(|p| ServicePort {
                        name: p.to_string(),
                        port: None,
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
    fn on_enter(mut self, a: Action) -> Self {
        self.service.script.actions.on_enter = Some(a);
        self
    }
    fn on_exit(mut self, a: Action) -> Self {
        self.service.script.actions.on_exit = Some(a);
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
    ServiceEndpoints::new(
        name,
        ports
            .iter()
            .map(|(p, port)| {
                (
                    p.to_string(),
                    ServiceEndpoint {
                        port: *port,
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
    let service = ServiceBuilder::new(
        "svc",
        &["main"],
        sh(&format!(
            "echo \"run TOKEN=$TOKEN WD=$OPENJD_SESSION_WORKING_DIR\" >> {t}; echo openjd_service_ready: ok; exit 1"
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
    assert_eq!(
        err.to_string(),
        "Service 'svc' onEnter failed: exit code: 2"
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
    .on_exit(sh("exit 5"))
    .build();
    let mut config = session_config(&root, "svc-test:onexit");
    config.retain_working_dir = false;
    let mut ss = ServiceSession::with_config(ServiceSessionConfig {
        session: config,
        service,
        environments: vec![environment],
        endpoints: endpoints("svc", &[("main", 5)]),
        in_scope_endpoints: vec![],
    })
    .unwrap();
    ss.start().await.unwrap();
    ss.wait_exit().await.unwrap();
    let wd = ss.session().working_directory().to_path_buf();
    let err = ss.end().await.unwrap_err();
    assert_eq!(err.to_string(), "Service 'svc' onExit failed: exit code: 5");
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
                bind_address: "0.0.0.0".into(),
                connect_address: "db.internal".into(),
            },
        )],
    );
    let mut config = session_config(&root, "svc-test:symbols");
    config.profile = Some(
        openjd_model::ModelProfile::new(openjd_model::SpecificationRevision::V2023_09)
            .with_extensions([openjd_model::ModelExtension::Expr].into_iter().collect()),
    );
    let mut ss = ServiceSession::with_config(ServiceSessionConfig {
        session: config,
        service,
        environments: vec![],
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
async fn with_config_rejects_mismatched_service_name_and_command_readiness() {
    let root = TempDir::new().unwrap();
    let service = ServiceBuilder::new("svc", &["main"], sh("sleep 1")).build();
    let err = ServiceSession::with_config(ServiceSessionConfig {
        session: session_config(&root, "svc-test:name"),
        service,
        environments: vec![],
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
        endpoints: endpoints("svc", &[("main", 5)]),
        in_scope_endpoints: vec![],
    })
    .err()
    .unwrap();
    assert_eq!(
        err.to_string(),
        "Service 'svc': the COMMAND readiness check type is not supported by this runtime yet"
    );
}
