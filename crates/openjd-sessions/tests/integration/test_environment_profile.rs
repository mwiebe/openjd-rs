// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Per-document extension profiles for entered Environments
//! (`Session::enter_environment_with_profile`,
//! `ServiceSessionConfig::environment_profiles`).
//!
//! Template Schemas §1.2 item 3 / RFC 0009 "Environment Template": an
//! extension applies to the document that lists it, so an Environment that
//! a scheduler attaches from an Environment Template declaring `SERVICE`
//! may call `join_host_port` even though the Job Template — the Session's
//! own profile — declares no extensions, and vice versa. These tests pin
//! that the library (and the `REDACTED_ENV_VARS` redaction gate) follow the
//! Environment's own profile, for `variables`, `onEnter`, `onExit`, and in a
//! Service Session's scope Environments.

#![cfg(unix)] // sh availability

use std::collections::HashMap;

use openjd_expr::format_string::FormatString;
use openjd_expr::symbol_table::{SerializedSymbolTable, SymbolTable};
use openjd_expr::ExprValue;
use openjd_model::job::service_symbols::{ServiceEndpoint, ServiceEndpoints};
use openjd_model::job::{
    Action, CompletedTasksPolicy, Environment, EnvironmentActions, EnvironmentScript, RunScope,
    Service, ServiceActions, ServicePort, ServiceReadinessCheck, ServiceRestartPolicy,
    ServiceScript,
};
use openjd_model::{ModelExtension, ModelProfile, SpecificationRevision};
use openjd_sessions::session::Session;
use openjd_sessions::{
    ServiceSession, ServiceSessionConfig, SessionConfig, SessionError, StickyBitPolicy,
};
use tempfile::TempDir;

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

/// `[SERVICE, EXPR]`, the profile of an Environment Template that uses
/// `join_host_port`.
fn service_profile() -> ModelProfile {
    ModelProfile::new(SpecificationRevision::V2023_09).with_extensions(
        [ModelExtension::Service, ModelExtension::Expr]
            .into_iter()
            .collect(),
    )
}

/// `[EXPR]` only: the profile of a plain Job Template with expressions.
fn expr_profile() -> ModelProfile {
    ModelProfile::new(SpecificationRevision::V2023_09)
        .with_extensions([ModelExtension::Expr].into_iter().collect())
}

/// The `Service.Kv.main.*` symbols a runtime seeds for a `runScope: [TASK]`
/// client Environment, frozen into a `resolved_symtab`.
fn kv_symtab() -> SerializedSymbolTable {
    let mut st = SymbolTable::new();
    st.set("Service.Kv.main.port", ExprValue::Int(6379))
        .unwrap();
    st.set(
        "Service.Kv.main.connectAddress",
        ExprValue::String("127.0.0.1".into()),
    )
    .unwrap();
    SerializedSymbolTable::from_symtab(&st)
}

fn env(
    name: &str,
    run_scope: Option<Vec<RunScope>>,
    variables: &[(&str, &str)],
    on_enter: Option<Action>,
    on_exit: Option<Action>,
    resolved_symtab: Option<SerializedSymbolTable>,
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
        variables: Some(
            variables
                .iter()
                .map(|(k, v)| (k.to_string(), fs(v)))
                .collect(),
        ),
        resolved_symtab,
    }
}

/// The RFC 0009 queue example's client Environment: `KV_ADDR` composed with
/// `join_host_port`, published to Tasks only.
fn kv_client_env() -> Environment {
    env(
        "KvClient",
        Some(vec![RunScope::Task]),
        &[(
            "KV_ADDR",
            "{{ join_host_port(Service.Kv.main.connectAddress, Service.Kv.main.port) }}",
        )],
        Some(sh(
            "echo enter sees KV_ADDR=$KV_ADDR; echo '{{ join_host_port(Service.Kv.main.connectAddress, Service.Kv.main.port + 1) }}'",
        )),
        Some(sh(
            "echo exit '{{ join_host_port(Service.Kv.main.connectAddress, Service.Kv.main.port) }}'",
        )),
        Some(kv_symtab()),
    )
}

fn session_config(root: &TempDir, id: &str, profile: Option<ModelProfile>) -> SessionConfig {
    SessionConfig {
        limits: Default::default(),
        session_id: id.into(),
        job_parameter_values: Default::default(),
        session_root_directory: Some(root.path().to_path_buf()),
        path_mapping_rules: None,
        retain_working_dir: false,
        callback: None,
        os_env_vars: None,
        user: None,
        profile,
        cancel_token: None,
        debug_collect_stdout: true,
        echo_openjd_directives: true,
        log_tag: None,
        sticky_bit_policy: StickyBitPolicy::Strict,
    }
}

fn session(root: &TempDir, profile: Option<ModelProfile>) -> Session {
    Session::with_config(session_config(root, "env-profile", profile)).unwrap()
}

/// The configuration of an external Service's Session: its own document's
/// `[SERVICE, EXPR]` profile.
fn service_session_config(root: &TempDir, id: &str) -> SessionConfig {
    session_config(root, id, Some(service_profile()))
}

fn lines(s: &str) -> Vec<&str> {
    s.lines().filter(|l| !l.is_empty()).collect()
}

// ────────────────────────────────────────────────────────────────────
// Task Session
// ────────────────────────────────────────────────────────────────────

/// Bug B1 of the exploratory report: without a per-Environment profile a
/// SERVICE-gated function in an attached Environment is unknown to a
/// Session whose own profile (the Job Template's) has no SERVICE.
#[tokio::test]
async fn environment_without_its_own_profile_uses_the_sessions_library() {
    let root = TempDir::new().unwrap();
    let mut session = session(&root, Some(expr_profile()));
    let err = session
        .enter_environment(&kv_client_env(), Some(&kv_symtab()), None, None)
        .await
        .unwrap_err();
    assert!(
        matches!(
            &err,
            SessionError::FormatString { context, reason }
                if context == "env var 'KV_ADDR'" && reason.contains("Unknown function: 'join_host_port'")
        ),
        "{err}"
    );
}

/// The fix: entered with its own `[SERVICE, EXPR]` profile, the
/// Environment's `variables`, `onEnter`, and `onExit` all resolve with
/// `join_host_port` while the Session's profile stays `[EXPR]`.
#[tokio::test]
async fn environment_with_its_own_profile_uses_that_documents_library() {
    let root = TempDir::new().unwrap();
    let mut session = session(&root, Some(expr_profile()));
    let (id, enter_out) = session
        .enter_environment_with_profile(
            &kv_client_env(),
            Some(&kv_symtab()),
            None,
            None,
            Some(&service_profile()),
        )
        .await
        .unwrap();
    assert_eq!(
        lines(&enter_out),
        vec!["enter sees KV_ADDR=127.0.0.1:6379", "127.0.0.1:6380"]
    );
    assert_eq!(
        session.evaluate_env_vars(None).get("KV_ADDR"),
        Some(&Some("127.0.0.1:6379".to_string()))
    );
    // The Session's own extensions are unchanged by the Environment's.
    assert_eq!(session.get_enabled_extensions(), vec!["EXPR"]);

    // onExit runs after the Environment left the stack, still with its own
    // library.
    let exit_out = session
        .exit_environment(&id, Some(&kv_symtab()), true, None)
        .await
        .unwrap();
    assert_eq!(lines(&exit_out), vec!["exit 127.0.0.1:6379"]);
}

/// A Session with no profile at all (a worker agent that never sets one)
/// behaves the same: the Environment's profile decides.
#[tokio::test]
async fn environment_profile_applies_when_the_session_has_none() {
    let root = TempDir::new().unwrap();
    let mut session = session(&root, None);
    let (_, out) = session
        .enter_environment_with_profile(
            &kv_client_env(),
            Some(&kv_symtab()),
            None,
            None,
            Some(&service_profile()),
        )
        .await
        .unwrap();
    assert_eq!(
        lines(&out),
        vec!["enter sees KV_ADDR=127.0.0.1:6379", "127.0.0.1:6380"]
    );
}

/// The inverse: a Job Template with SERVICE attaching a plain EXPR-only
/// Environment Template — the Environment resolves with its own (smaller)
/// library, and the Task still resolves with the Session's.
#[tokio::test]
async fn plain_environment_in_a_service_session_profile() {
    let root = TempDir::new().unwrap();
    let mut session = session(&root, Some(service_profile()));
    let plain = env(
        "Plain",
        None,
        &[("GREETING", "{{ 'hello'.upper() }}")],
        Some(sh("echo $GREETING")),
        None,
        None,
    );
    let (_, out) = session
        .enter_environment_with_profile(&plain, None, None, None, Some(&expr_profile()))
        .await
        .unwrap();
    assert_eq!(lines(&out), vec!["HELLO"]);

    // A SERVICE function in the plain Environment is rejected under its own
    // profile even though the Session would accept it.
    let mut too_much = plain.clone();
    too_much.name = "TooMuch".into();
    too_much.variables = Some(HashMap::from([(
        "ADDR".to_string(),
        fs("{{ join_host_port('h', 1) }}"),
    )]));
    let err = session
        .enter_environment_with_profile(&too_much, None, None, None, Some(&expr_profile()))
        .await
        .unwrap_err();
    assert!(
        matches!(
            &err,
            SessionError::FormatString { context, reason }
                if context == "env var 'ADDR'" && reason.contains("Unknown function: 'join_host_port'")
        ),
        "{err}"
    );
    // ... and accepted with the Session's own library.
    session
        .enter_environment(&too_much, None, None, None)
        .await
        .unwrap();
}

/// `openjd_redacted_env` from an Environment's `onEnter` is honored under
/// the Environment's own `REDACTED_ENV_VARS`, not the Session's.
#[tokio::test]
async fn redacted_env_follows_the_environments_profile() {
    let redacting = ModelProfile::new(SpecificationRevision::V2023_09)
        .with_extensions([ModelExtension::RedactedEnvVars].into_iter().collect());
    let secret_env = |name: &str| {
        env(
            name,
            None,
            &[],
            Some(sh("echo openjd_redacted_env: TOKEN=s3cret")),
            None,
            None,
        )
    };

    let root = TempDir::new().unwrap();
    // Session without the extension: an Environment that declares it sets
    // the variable; one that does not (no profile → the Session's) does not.
    let mut session = session(&root, Some(expr_profile()));
    session
        .enter_environment_with_profile(&secret_env("Declares"), None, None, None, Some(&redacting))
        .await
        .unwrap();
    assert_eq!(
        session.evaluate_env_vars(None).get("TOKEN"),
        Some(&Some("s3cret".to_string()))
    );
    let (id, _) = session
        .enter_environment_with_output(&secret_env("Plain"), None, None, None)
        .await
        .unwrap();
    // Plain's directive was ignored, so TOKEN is still the first value and
    // not recorded as a change of the second Environment.
    assert_eq!(
        session.evaluate_env_vars(None).get("TOKEN"),
        Some(&Some("s3cret".to_string()))
    );
    session
        .exit_environment(&id, None, true, None)
        .await
        .unwrap();
    assert_eq!(
        session.evaluate_env_vars(None).get("TOKEN"),
        Some(&Some("s3cret".to_string()))
    );
}

/// Exploratory report stumble S6: an `openjd_redacted_env` that is dropped
/// because the document declares no `REDACTED_ENV_VARS` is announced with a
/// WARN `COMMAND_OUTPUT` record naming the variable (never the value), so
/// the redacted `TOKEN=********` line in the log is not mistaken for a set
/// variable. The decision follows the Environment's own profile: under a
/// Session without the extension, a plain Environment warns and sets
/// nothing; one entered with a profile that declares it sets the variable
/// and warns of nothing — and the other way round.
#[tokio::test]
async fn redacted_env_without_the_extension_warns_in_the_environments_output() {
    const WARNING: &str = "Received openjd_redacted_env for 'TOKEN' but the REDACTED_ENV_VARS \
                           extension is not declared; the variable is not set.";
    let redacting = ModelProfile::new(SpecificationRevision::V2023_09)
        .with_extensions([ModelExtension::RedactedEnvVars].into_iter().collect());
    let secret_env = |name: &str| {
        env(
            name,
            None,
            &[],
            Some(sh("echo openjd_redacted_env: TOKEN=s3cret")),
            None,
            None,
        )
    };
    let warnings = |logs: &[testing_logger::CapturedLog]| -> Vec<String> {
        logs.iter()
            .filter(|l| l.level == log::Level::Warn)
            .map(|l| l.body.clone())
            .collect()
    };

    // Session without the extension; a plain Environment (the Session's
    // profile) warns, the Session has no TOKEN, and the value is redacted
    // everywhere — including the warning.
    testing_logger::setup();
    let root = TempDir::new().unwrap();
    let mut plain_session = session(&root, Some(expr_profile()));
    let (_, output) = plain_session
        .enter_environment_with_output(&secret_env("Plain"), None, None, None)
        .await
        .unwrap();
    assert_eq!(plain_session.evaluate_env_vars(None).get("TOKEN"), None);
    assert_eq!(lines(&output), vec!["openjd_redacted_env: TOKEN=********"]);
    testing_logger::validate(|logs| {
        assert_eq!(warnings(logs), vec![WARNING.to_string()]);
        let bodies: Vec<&str> = logs.iter().map(|l| l.body.as_str()).collect();
        assert!(
            bodies.contains(&"openjd_redacted_env: TOKEN=********"),
            "{bodies:?}"
        );
        assert!(!bodies.iter().any(|b| b.contains("s3cret")), "{bodies:?}");
    });

    // The same Session; an Environment entered with a profile that declares
    // the extension sets the variable and no warning is logged.
    testing_logger::setup();
    plain_session
        .enter_environment_with_profile(&secret_env("Declares"), None, None, None, Some(&redacting))
        .await
        .unwrap();
    assert_eq!(
        plain_session.evaluate_env_vars(None).get("TOKEN"),
        Some(&Some("s3cret".to_string()))
    );
    testing_logger::validate(|logs| {
        assert!(warnings(logs).is_empty(), "{:?}", warnings(logs));
    });

    // And vice versa: a Session that declares the extension sets the variable
    // from its own Environments without a warning, while an Environment
    // entered with a profile lacking it warns and sets nothing.
    testing_logger::setup();
    let root2 = TempDir::new().unwrap();
    let mut redacting_session = session(&root2, Some(redacting.clone()));
    redacting_session
        .enter_environment_with_output(&secret_env("Own"), None, None, None)
        .await
        .unwrap();
    assert_eq!(
        redacting_session.evaluate_env_vars(None).get("TOKEN"),
        Some(&Some("s3cret".to_string()))
    );
    testing_logger::validate(|logs| {
        assert!(warnings(logs).is_empty(), "{:?}", warnings(logs));
    });
    testing_logger::setup();
    let other = env(
        "Attached",
        None,
        &[],
        Some(sh("echo openjd_redacted_env: OTHER=s3cret2")),
        None,
        None,
    );
    redacting_session
        .enter_environment_with_profile(&other, None, None, None, Some(&expr_profile()))
        .await
        .unwrap();
    assert_eq!(redacting_session.evaluate_env_vars(None).get("OTHER"), None);
    testing_logger::validate(|logs| {
        assert_eq!(warnings(logs), vec![WARNING.replace("'TOKEN'", "'OTHER'")]);
        assert!(!logs.iter().any(|l| l.body.contains("s3cret2")));
    });
}

/// Path-mapping rules added after an Environment is entered reach the
/// Environment's own library too (`apply_path_mapping` in its `onExit`).
#[tokio::test]
async fn environment_library_tracks_path_mapping_rule_changes() {
    let root = TempDir::new().unwrap();
    let mut session = session(&root, Some(expr_profile()));
    let mapped = env(
        "Mapped",
        None,
        &[],
        None,
        Some(sh(
            "echo '{{ apply_path_mapping(\"/src/x\") }}' '{{ join_host_port(\"h\", 1) }}'",
        )),
        None,
    );
    let (id, _) = session
        .enter_environment_with_profile(&mapped, None, None, None, Some(&service_profile()))
        .await
        .unwrap();
    session.extend_path_mapping_rules(vec![openjd_sessions::PathMappingRule {
        source_path_format: openjd_sessions::PathFormat::Posix,
        source_path: "/src".into(),
        destination_path: "/dst".into(),
    }]);
    let out = session
        .exit_environment(&id, None, true, None)
        .await
        .unwrap();
    assert_eq!(lines(&out), vec!["/dst/x h:1"]);
}

// ────────────────────────────────────────────────────────────────────
// Service Session
// ────────────────────────────────────────────────────────────────────

fn endpoints(name: &str, port: u16) -> ServiceEndpoints {
    ServiceEndpoints::new(
        name,
        vec![(
            "main".to_string(),
            ServiceEndpoint {
                port,
                bind_address: "127.0.0.1".into(),
                connect_address: "127.0.0.1".into(),
            },
        )],
    )
}

fn service(name: &str, on_run: Action) -> Service {
    Service {
        name: name.into(),
        description: None,
        document: Default::default(),
        host_requirements: None,
        service_environments: None,
        ports: vec![ServicePort {
            name: "main".into(),
            port: None,
        }],
        readiness_check: ServiceReadinessCheck::Stdout {
            timeout_seconds: 30,
        },
        restart_policy: ServiceRestartPolicy {
            max_attempts: 0,
            completed_tasks: CompletedTasksPolicy::Keep,
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
    }
}

/// An external Service's Session runs under its own document's `[SERVICE,
/// EXPR]` profile (the Session profile the caller sets), while a Job
/// Environment of a SERVICE-less Job Template that it enters is evaluated
/// under the Job's `[EXPR]` profile given per Environment — and a Job
/// Environment that silently shared the Service's profile would have
/// accepted a function its own document cannot use.
#[tokio::test]
async fn service_session_enters_scope_environments_under_their_own_profiles() {
    let root = TempDir::new().unwrap();
    let job_env = env(
        "JobEnv",
        None,
        &[("FROM_JOB", "{{ 'job'.upper() }}")],
        Some(sh("echo job env sees FROM_JOB=$FROM_JOB")),
        None,
        None,
    );
    let svc = service(
        "Kv",
        sh("echo ADDR={{ join_host_port(Service.Kv.main.bindAddress, Service.Kv.main.port) }} FROM_JOB=$FROM_JOB; echo openjd_service_ready: up; sleep 30"),
    );
    let mut ss = ServiceSession::with_config(ServiceSessionConfig {
        session: service_session_config(&root, "svc-env-profile-ok"),
        service: svc.clone(),
        environments: vec![job_env.clone()],
        environment_profiles: vec![Some(expr_profile())],
        endpoints: endpoints("Kv", 6379),
        in_scope_endpoints: vec![],
    })
    .unwrap();
    assert!(ss.start().await.unwrap().is_ready());
    ss.cancel_run(Some(std::time::Duration::ZERO));
    let exit = ss.wait_exit().await.unwrap();
    assert_eq!(
        lines(&exit.stdout),
        vec![
            "ADDR=127.0.0.1:6379 FROM_JOB=JOB",
            "openjd_service_ready: up"
        ]
    );
    ss.end().await.unwrap();

    // A Job Environment using a SERVICE function is rejected under the Job's
    // own [EXPR] profile: a start failure, even though the Service's Session
    // profile would accept it.
    let mut bad_job_env = job_env.clone();
    bad_job_env.variables = Some(HashMap::from([(
        "ADDR".to_string(),
        fs("{{ join_host_port('h', 1) }}"),
    )]));
    let mut ss = ServiceSession::with_config(ServiceSessionConfig {
        session: service_session_config(&root, "svc-env-profile-bad"),
        service: svc,
        environments: vec![bad_job_env],
        environment_profiles: vec![Some(expr_profile())],
        endpoints: endpoints("Kv", 6380),
        in_scope_endpoints: vec![],
    })
    .unwrap();
    let err = ss.enter().await.unwrap_err();
    assert!(
        matches!(
            &err,
            SessionError::FormatString { context, reason }
                if context == "env var 'ADDR'" && reason.contains("Unknown function: 'join_host_port'")
        ),
        "{err}"
    );
    assert_eq!(
        ss.state(),
        openjd_sessions::ServiceSessionState::StartFailed
    );
    ss.end().await.unwrap();
}
