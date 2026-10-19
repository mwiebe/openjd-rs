// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `CONTROL_FLOW` (RFC 0010) in the session runtime: the `openjd_output_var`
//! directive, `@` file values, `ActionResult::outputs`, and path-typed
//! `Steps.*` inputs. Same-user only; the cross-user variants are in
//! `test_cross_user.rs`.
//!
//! The scripts are POSIX shell.
#![cfg(unix)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use openjd_expr::format_string::FormatString;
use openjd_expr::{ExprType, ExprValue, PathFormat, PathMappingRule, SymbolTable};
use openjd_model::job::{
    Action, Environment, EnvironmentActions, EnvironmentScript, OutputVariable, PathInput,
    StepActions, StepScript,
};
use openjd_model::types::{TaskParameterSet, TaskParameterType, TaskParameterValue};
use openjd_sessions::action::{ActionResult, ActionState};
use openjd_sessions::session::{
    EnvironmentOptions, EnvironmentOrigin, RunTaskOptions, Session, SessionConfig,
};
use tempfile::TempDir;

type Reported = Arc<Mutex<Vec<(Option<i64>, String, ExprValue)>>>;

fn fs(s: &str) -> FormatString {
    FormatString::new(s).unwrap()
}

fn sh_action(script: &str) -> Action {
    Action {
        command: fs("sh"),
        args: Some(vec![fs("-c"), fs(script)]),
        timeout: None,
        cancelation: None,
    }
}

fn step(script: &str) -> StepScript {
    StepScript {
        let_bindings: None,
        actions: StepActions {
            on_run: sh_action(script),
        },
        embedded_files: None,
    }
}

fn decls(json: &str) -> Vec<OutputVariable> {
    serde_json::from_str(json).unwrap()
}

struct Fixture {
    _tmp: TempDir,
    session: Session,
    reported: Reported,
}

/// A Session whose Job lists `CONTROL_FLOW` (or not), with an output
/// callback that records every reported value.
fn fixture_with(
    control_flow: bool,
    echo: bool,
    rules: Option<Vec<PathMappingRule>>,
    limits: openjd_sessions::SessionLimits,
) -> Fixture {
    let tmp = TempDir::new().unwrap();
    let reported: Reported = Arc::default();
    let sink = reported.clone();
    let config = SessionConfig {
        limits,
        session_id: "cf".into(),
        job_parameter_values: Default::default(),
        session_root_directory: Some(tmp.path().to_path_buf()),
        path_mapping_rules: rules,
        retain_working_dir: false,
        callback: None,
        os_env_vars: None,
        user: None,
        profile: None,
        cancel_token: None,
        debug_collect_stdout: true,
        echo_openjd_directives: echo,
        control_flow,
        output_callback: Some(Box::new(move |_, task, name, value| {
            sink.lock()
                .unwrap()
                .push((task, name.to_string(), value.clone()))
        })),
        sticky_bit_policy: openjd_sessions::StickyBitPolicy::Disabled,
    };
    let session = Session::with_config(config).unwrap();
    Fixture {
        _tmp: tmp,
        session,
        reported,
    }
}

fn fixture() -> Fixture {
    fixture_with(true, true, None, Default::default())
}

impl Fixture {
    async fn run(&mut self, script: &str, declared: Option<&[OutputVariable]>) -> ActionResult {
        self.run_with(script, declared, None).await
    }

    async fn run_with(
        &mut self,
        script: &str,
        declared: Option<&[OutputVariable]>,
        task: Option<&TaskParameterSet>,
    ) -> ActionResult {
        self.session
            .run_task(
                "S",
                &step(script),
                task,
                None,
                None,
                &RunTaskOptions {
                    output_variables: declared,
                    path_inputs: &[],
                },
            )
            .await
            .unwrap()
    }

    fn fail_message(&self) -> String {
        self.session
            .action_status()
            .and_then(|s| s.fail_message)
            .unwrap_or_default()
    }

    fn reported(&self) -> Vec<(Option<i64>, String, ExprValue)> {
        self.reported.lock().unwrap().clone()
    }

    fn wd(&self) -> std::path::PathBuf {
        self.session.working_directory().to_path_buf()
    }
}

fn unchunked(values: Vec<(&str, ExprValue)>) -> IndexMap<Option<i64>, IndexMap<String, ExprValue>> {
    let mut m = IndexMap::new();
    m.insert(
        None,
        values
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    );
    m
}

/// Assert the Task failed with exactly `message` and returned no outputs.
fn assert_failed(f: &Fixture, r: &ActionResult, message: &str) {
    assert_eq!(r.state, ActionState::Failed, "stdout: {}", r.stdout);
    assert!(r.outputs.is_none());
    assert_eq!(f.fail_message(), message);
}

fn float(v: f64) -> ExprValue {
    ExprValue::Float(openjd_expr::value::Float64::new(v).unwrap())
}

const N_INT: &str = r#"[{"name": "N", "type": "INT"}]"#;

// ── Values returned and reported ────────────────────────────────────

#[tokio::test]
async fn values_are_returned_and_reported_from_stdout_and_stderr() {
    let mut f = fixture();
    let d = decls(
        r#"[{"name": "N", "type": "INT"}, {"name": "S", "type": "STRING"},
            {"name": "E", "type": "LIST[FLOAT]", "collect": "APPEND"}]"#,
    );
    let r = f
        .run(
            r#"echo 'openjd_output_var: N=12'
               echo 'openjd_output_var: S="a=b"' >&2
               echo 'openjd_output_var: E=1'
               echo 'openjd_output_var: E=2.5'"#,
            Some(&d),
        )
        .await;
    assert_eq!(r.state, ActionState::Success);
    assert_eq!(
        r.outputs,
        Some(unchunked(vec![
            ("N", ExprValue::Int(12)),
            ("S", ExprValue::String("a=b".into())),
            (
                "E",
                ExprValue::make_list(vec![float(1.0), float(2.5)], ExprType::FLOAT).unwrap()
            ),
        ]))
    );
    let reported = f.reported();
    assert_eq!(reported.len(), 4);
    assert_eq!(reported[0], (None, "N".to_string(), ExprValue::Int(12)));
    assert_eq!(reported[3].1, "E");
}

#[tokio::test]
async fn no_outputs_when_the_action_fails() {
    let mut f = fixture();
    let d = decls(N_INT);
    let r = f
        .run("echo 'openjd_output_var: N=1'; exit 3", Some(&d))
        .await;
    assert_eq!(r.state, ActionState::Failed);
    assert!(r.outputs.is_none());
    // The value was reported while the Task ran; the caller discards it.
    assert_eq!(f.reported().len(), 1);
}

#[tokio::test]
async fn no_outputs_when_the_action_times_out_or_is_canceled() {
    let mut f = fixture();
    let d = decls(N_INT);
    let mut script = step("echo 'openjd_output_var: N=1'; sleep 30");
    script.actions.on_run.timeout = Some(fs("1"));
    let r = f
        .session
        .run_task(
            "S",
            &script,
            None,
            None,
            None,
            &RunTaskOptions {
                output_variables: Some(&d),
                path_inputs: &[],
            },
        )
        .await
        .unwrap();
    assert_eq!(r.state, ActionState::Timeout);
    assert!(r.outputs.is_none());

    let mut f = fixture();
    let handle = f.session.cancel_handle();
    let canceler = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        handle.cancel(Some(Duration::ZERO), false)
    });
    let r = f
        .run("echo 'openjd_output_var: N=1'; sleep 30", Some(&d))
        .await;
    assert!(canceler.await.unwrap());
    assert_eq!(r.state, ActionState::Canceled);
    assert!(r.outputs.is_none());
}

#[tokio::test]
async fn exit_zero_after_an_invalid_directive_fails() {
    let mut f = fixture();
    let d = decls(N_INT);
    let r = f.run("echo 'openjd_output_var: N=1.5'", Some(&d)).await;
    assert_failed(
        &f,
        &r,
        "Output variable 'N': value (float) '1.5' does not convert to int: Cannot coerce float \
         to int: 1.5 is not a whole number, in 'openjd_output_var: N=1.5'",
    );
}

#[tokio::test]
async fn collector_failures_fail_the_task_with_their_message() {
    for (decl, script, message) in [
        (
            N_INT,
            "echo 'openjd_output_var: M=1'",
            "Output variable 'M': the Step does not declare it (declared: 'N'), in \
             'openjd_output_var: M=1'",
        ),
        (
            N_INT,
            "echo 'openjd_output_var: N=1'; echo 'openjd_output_var: N=2'",
            "Output variable 'N': a second value was reported, but its collect mode is ONCE, in \
             'openjd_output_var: N=2'",
        ),
        (
            N_INT,
            "echo 'openjd_output_var: N=null'",
            "Output variable 'N': value is null; output values cannot be or contain null, in \
             'openjd_output_var: N=null'",
        ),
        (
            N_INT,
            r#"echo 'openjd_output_var: N={"a": 1}'"#,
            "Output variable 'N': value is a JSON object; output values cannot be or contain \
             objects, in 'openjd_output_var: N={\"a\": 1}'",
        ),
        (
            N_INT,
            "echo 'openjd_output_var: N[1]=1'",
            "Output variable 'N[1]': a task selector is only allowed in a Step with a CHUNK[INT] \
             parameter, in 'openjd_output_var: N[1]=1'",
        ),
        (
            N_INT,
            "true",
            "Output variable 'N': no value was reported (collect mode ONCE)",
        ),
        (
            r#"[{"name": "N", "type": "INT", "collect": "LAST"}]"#,
            "true",
            "Output variable 'N': no value was reported (collect mode LAST)",
        ),
        (
            r#"[{"name": "N", "type": "INT", "constraints": {"maxValue": {"int": 5}}}]"#,
            "echo 'openjd_output_var: N=6'",
            "Output variable 'N': value 6 exceeds maximum 5, in 'openjd_output_var: N=6'",
        ),
        (
            r#"[{"name": "L", "type": "LIST[INT]", "collect": "APPEND",
                 "constraints": {"maxLength": 1}}]"#,
            "echo 'openjd_output_var: L=1'; echo 'openjd_output_var: L=2'",
            "Output variable 'L': list length 2 exceeds maximum 1, in 'openjd_output_var: L=2'",
        ),
    ] {
        let mut f = fixture();
        let d = decls(decl);
        let r = f.run(script, Some(&d)).await;
        assert_failed(&f, &r, message);
    }
}

#[tokio::test]
async fn last_replaces_and_append_collects() {
    let mut f = fixture();
    let d = decls(
        r#"[{"name": "L", "type": "INT", "collect": "LAST"},
            {"name": "A", "type": "LIST[INT]", "collect": "APPEND"}]"#,
    );
    let r = f
        .run(
            "echo 'openjd_output_var: L=1'; echo 'openjd_output_var: L=2'",
            Some(&d),
        )
        .await;
    assert_eq!(
        r.outputs,
        Some(unchunked(vec![
            ("L", ExprValue::Int(2)),
            ("A", ExprValue::make_list(vec![], ExprType::INT).unwrap()),
        ]))
    );
}

// ── Recognition, logging, cancel at once ────────────────────────────

#[tokio::test]
async fn directive_needs_no_space_after_the_colon() {
    let mut f = fixture();
    let d = decls(N_INT);
    let r = f.run("echo 'openjd_output_var:N=1'", Some(&d)).await;
    assert_eq!(r.outputs, Some(unchunked(vec![("N", ExprValue::Int(1))])));
}

#[tokio::test]
async fn empty_directive_fails_the_task() {
    let mut f = fixture();
    let d = decls(N_INT);
    let r = f.run("echo 'openjd_output_var: '", Some(&d)).await;
    assert_failed(
        &f,
        &r,
        "Invalid openjd_output_var directive: expected an output name (an identifier) \
         followed by '=', in ' '",
    );
}

#[tokio::test]
async fn invalid_directive_cancels_the_action_at_once() {
    let mut f = fixture();
    let d = decls(N_INT);
    let start = Instant::now();
    let r = f
        .run("echo 'openjd_output_var: N=\"x\"'; sleep 30", Some(&d))
        .await;
    assert!(
        start.elapsed() < Duration::from_secs(15),
        "took {:?}; the invalid directive must cancel the action",
        start.elapsed()
    );
    assert_failed(
        &f,
        &r,
        "Output variable 'N': value (string) 'x' does not convert to int: Cannot convert 'x' to \
         int: invalid digit found in string, in 'openjd_output_var: N=\"x\"'",
    );
}

/// Log lines of the action's output, as the session logged them.
fn logged(captured: &[testing_logger::CapturedLog]) -> Vec<String> {
    captured.iter().map(|l| l.body.clone()).collect()
}

#[tokio::test]
async fn valid_directives_follow_echo_and_invalid_ones_are_always_logged() {
    let d = decls(N_INT);
    for echo in [true, false] {
        testing_logger::setup();
        let mut f = fixture_with(true, echo, None, Default::default());
        f.run("echo 'openjd_output_var: N=1'", Some(&d)).await;
        let mut f = fixture_with(true, echo, None, Default::default());
        f.run("echo 'openjd_output_var: N=x'", Some(&d)).await;
        testing_logger::validate(|captured| {
            let lines = logged(captured);
            assert_eq!(
                lines.iter().any(|l| l.ends_with("openjd_output_var: N=1")),
                echo,
                "echo={echo}: {lines:#?}"
            );
            assert!(
                lines.iter().any(|l| l.ends_with(
                    "openjd_output_var: N=x -- ERROR: Output variable 'N': 'x' is not one \
                     valid JSON value: expected value at line 1 column 1"
                )),
                "echo={echo}: {lines:#?}"
            );
        });
    }
}

// ── The three treatments ────────────────────────────────────────────

fn env(name: &str, on_enter: Option<Action>, on_wrap_env_enter: Option<Action>) -> Environment {
    Environment {
        name: name.into(),
        description: None,
        script: Some(EnvironmentScript {
            let_bindings: None,
            actions: EnvironmentActions {
                on_enter,
                on_wrap_env_enter,
                on_wrap_task_run: None,
                on_wrap_env_exit: None,
                on_exit: None,
            },
            embedded_files: None,
        }),
        variables: None,
        resolved_symtab: None,
        enabled: true,
    }
}

const FORBIDDEN: &str = "Invalid openjd_output_var directive: this Action can't report output \
                         values; only the onRun action of a Step that declares outputVariables \
                         can, in 'openjd_output_var: N=1'";

#[tokio::test]
async fn step_without_output_variables_fails_on_a_directive() {
    let mut f = fixture();
    let r = f.run("echo 'openjd_output_var: N=1'", None).await;
    assert_failed(&f, &r, FORBIDDEN);
}

#[tokio::test]
async fn job_template_on_enter_fails_and_environment_template_is_ordinary() {
    let mut f = fixture();
    // The sleep makes the cancel, not the exit, end the action, so the exit
    // code is reliably absent.
    let e = env(
        "E",
        Some(sh_action("echo 'openjd_output_var: N=1'; sleep 5")),
        None,
    );
    let err = f
        .session
        .enter_environment(&e, None, None, None, &EnvironmentOptions::default())
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Environment 'E' onEnter failed: exit code: N/A"
    );
    assert_eq!(f.fail_message(), FORBIDDEN);

    let mut f = fixture();
    let options = EnvironmentOptions {
        origin: EnvironmentOrigin::EnvironmentTemplate,
        path_inputs: &[],
    };
    let (_, out) = f
        .session
        .enter_environment_with_output(&e, None, None, None, &options)
        .await
        .unwrap();
    assert!(out.contains("openjd_output_var: N=1"));
}

#[tokio::test]
async fn without_control_flow_a_directive_is_ordinary_output() {
    let mut f = fixture_with(false, true, None, Default::default());
    let d = decls(N_INT);
    let r = f.run("echo 'openjd_output_var: N=x'", Some(&d)).await;
    assert_eq!(r.state, ActionState::Success);
    assert!(r.outputs.is_none());
    assert!(f.reported().is_empty());
}

#[tokio::test]
async fn wrap_env_enter_follows_the_wrapped_environment() {
    // A wrap hook runs in place of the inner Environment's onEnter, so it is
    // treated as that action: invalid for a Job Template Environment,
    // ordinary output for an Environment Template one.
    for (origin, fails) in [
        (EnvironmentOrigin::JobTemplate, true),
        (EnvironmentOrigin::EnvironmentTemplate, false),
    ] {
        let mut f = fixture();
        let wrapper = env("W", None, Some(sh_action("echo 'openjd_output_var: N=1'")));
        f.session
            .enter_environment(
                &wrapper,
                None,
                None,
                None,
                &EnvironmentOptions {
                    origin: EnvironmentOrigin::EnvironmentTemplate,
                    path_inputs: &[],
                },
            )
            .await
            .unwrap();
        let inner = env("I", Some(sh_action("true")), None);
        let r = f
            .session
            .enter_environment(
                &inner,
                None,
                None,
                None,
                &EnvironmentOptions {
                    origin,
                    path_inputs: &[],
                },
            )
            .await;
        assert_eq!(r.is_err(), fails, "{origin:?}: {r:?}");
    }
}

#[tokio::test]
async fn wrap_task_run_directives_count_for_the_task() {
    let mut f = fixture();
    let mut wrapper = env("W", None, None);
    wrapper.script.as_mut().unwrap().actions.on_wrap_task_run = Some(sh_action(
        "echo 'openjd_output_var: A=1'; {{WrappedAction.Command}} {{repr_sh(WrappedAction.Args)}}",
    ));
    f.session
        .enter_environment(
            &wrapper,
            None,
            None,
            None,
            &EnvironmentOptions {
                origin: EnvironmentOrigin::EnvironmentTemplate,
                path_inputs: &[],
            },
        )
        .await
        .unwrap();
    let d = decls(r#"[{"name": "A", "type": "INT"}, {"name": "B", "type": "INT"}]"#);
    let r = f.run("echo 'openjd_output_var: B=2'", Some(&d)).await;
    assert_eq!(
        r.outputs,
        Some(unchunked(vec![
            ("A", ExprValue::Int(1)),
            ("B", ExprValue::Int(2))
        ])),
        "{}",
        r.stdout
    );
}

// ── Line limits and encoding ────────────────────────────────────────

#[tokio::test]
async fn directive_line_over_64_kib_is_invalid() {
    let mut f = fixture();
    let d = decls(r#"[{"name": "S", "type": "STRING"}]"#);
    let r = f
        .run(r#"printf 'openjd_output_var: S="%070000d"\n' 0"#, Some(&d))
        .await;
    assert_failed(
        &f,
        &r,
        &format!(
            "Invalid openjd_output_var directive: the line is longer than the 65536 byte line \
             limit, so it was cut; write a large value to a file and report it with @<file>, in \
             'openjd_output_var: S=\"{}...'",
            "0".repeat(178)
        ),
    );
}

#[tokio::test]
async fn directive_line_that_is_not_utf8_is_invalid() {
    let mut f = fixture();
    let d = decls(r#"[{"name": "S", "type": "STRING"}]"#);
    let r = f
        .run(r#"printf 'openjd_output_var: S="\377"\n'"#, Some(&d))
        .await;
    assert_failed(
        &f,
        &r,
        r#"Invalid openjd_output_var directive: the line is not valid UTF-8, in 'openjd_output_var: S="\xff"'"#,
    );
}

#[tokio::test]
async fn trailing_form_feed_is_not_directive_whitespace() {
    let mut f = fixture();
    let d = decls(N_INT);
    let r = f
        .run(r#"printf 'openjd_output_var: N=1\f\n'"#, Some(&d))
        .await;
    assert_failed(
        &f,
        &r,
        "Output variable 'N': '1\\u{c}' is not one valid JSON value: trailing characters at \
         line 1 column 2",
    );
    // A CRLF line end is stripped like a LF one.
    let mut f = fixture();
    let r = f
        .run(r#"printf 'openjd_output_var: N=1 \t\r\n'"#, Some(&d))
        .await;
    assert_eq!(r.outputs, Some(unchunked(vec![("N", ExprValue::Int(1))])));
}

// ── @ files ─────────────────────────────────────────────────────────

#[tokio::test]
async fn at_files_relative_absolute_and_symlinked_inside() {
    let mut f = fixture();
    let d = decls(
        r#"[{"name": "A", "type": "LIST[INT]"}, {"name": "B", "type": "INT"},
            {"name": "C", "type": "STRING"}]"#,
    );
    let wd = f.wd();
    let script = format!(
        r#"printf '\357\273\277[1,\n 2]\n' > a.json
           echo 'openjd_output_var: A=@a.json'
           mkdir -p sub; printf 7 > sub/b
           echo 'openjd_output_var: B=@{}/sub/b'
           printf '"x"' > c; ln -s c link
           echo 'openjd_output_var: C= @link  '"#,
        wd.display()
    );
    let r = f.run(&script, Some(&d)).await;
    assert_eq!(
        r.outputs,
        Some(unchunked(vec![
            (
                "A",
                ExprValue::make_list(vec![ExprValue::Int(1), ExprValue::Int(2)], ExprType::INT)
                    .unwrap()
            ),
            ("B", ExprValue::Int(7)),
            ("C", ExprValue::String("x".into())),
        ])),
        "{}",
        r.stdout
    );
}

#[tokio::test]
async fn at_files_that_are_not_regular_files_inside_fail() {
    let outside = TempDir::new().unwrap();
    std::fs::write(outside.path().join("o"), "1").unwrap();
    let o = outside.path().join("o");
    let o = o.display();
    for (setup, message) in [
        (
            format!("echo 'openjd_output_var: N=@{o}'"),
            format!(
                "Output variable 'N': the @ file '{o}' is not inside the Session working directory"
            ),
        ),
        (
            format!("ln -s {o} l; echo 'openjd_output_var: N=@l'"),
            "Output variable 'N': the @ file 'l' is not inside the Session working directory"
                .to_string(),
        ),
        (
            "mkdir d; echo 'openjd_output_var: N=@d'".to_string(),
            "Output variable 'N': the @ file 'd' is not a regular file".to_string(),
        ),
        (
            "echo 'openjd_output_var: N=@missing'".to_string(),
            "Output variable 'N': the @ file 'missing' does not exist".to_string(),
        ),
        (
            "printf '1\\000' > z; echo 'openjd_output_var: N=@z'".to_string(),
            "Output variable 'N': the @ file's content contains a NUL character, in \
             'openjd_output_var: N=@z'"
                .to_string(),
        ),
        (
            "printf '1\\377' > z; echo 'openjd_output_var: N=@z'".to_string(),
            "Output variable 'N': the @ file's content is not valid UTF-8 (at byte 1), in \
             'openjd_output_var: N=@z'"
                .to_string(),
        ),
    ] {
        let mut f = fixture();
        let d = decls(N_INT);
        let r = f.run(&setup, Some(&d)).await;
        assert_failed(&f, &r, &message);
    }
}

#[tokio::test]
async fn at_fifo_fails_without_hanging_the_reader() {
    let mut f = fixture();
    let d = decls(N_INT);
    let start = Instant::now();
    let r = f
        .run("mkfifo p; echo 'openjd_output_var: N=@p'", Some(&d))
        .await;
    assert!(start.elapsed() < Duration::from_secs(15));
    assert_failed(
        &f,
        &r,
        "Output variable 'N': the @ file 'p' is not a regular file",
    );
}

#[tokio::test]
async fn at_file_over_the_size_limit_fails_without_being_read_in_full() {
    let mut f = fixture();
    let d = decls(r#"[{"name": "S", "type": "STRING"}]"#);
    let start = Instant::now();
    // A sparse 64 GiB file: reading it all would take far longer than this
    // test allows.
    let r = f
        .run(
            "truncate -s 64G big; echo 'openjd_output_var: S=@big'",
            Some(&d),
        )
        .await;
    assert!(start.elapsed() < Duration::from_secs(15));
    assert_failed(
        &f,
        &r,
        "Output variable 'S': the @ file 'big' is larger than the 1048576 bytes the Task's \
         output values have left (the limit is 1048576 bytes per Task)",
    );
}

#[tokio::test]
async fn size_limit_comes_from_session_limits() {
    let limits = openjd_sessions::SessionLimits {
        max_task_output_bytes: Some(2 * 1024 * 1024),
        ..Default::default()
    };
    let mut f = fixture_with(true, true, None, limits);
    let d = decls(r#"[{"name": "S", "type": "STRING"}]"#);
    let r = f
        .run(
            r#"printf '"%01500000d"' 0 > v; echo 'openjd_output_var: S=@v'"#,
            Some(&d),
        )
        .await;
    assert_eq!(r.state, ActionState::Success, "{}", f.fail_message());
}

// ── Chunked Steps ───────────────────────────────────────────────────

fn chunk(value: ExprValue) -> TaskParameterSet {
    let mut t = TaskParameterSet::new();
    t.insert(
        "F".into(),
        TaskParameterValue {
            param_type: TaskParameterType::ChunkInt,
            value,
        },
    );
    t
}

#[tokio::test]
async fn chunked_values_are_keyed_by_chunk_value() {
    let d = decls(N_INT);
    for value in [
        ExprValue::RangeExpr("10-12".parse().unwrap()),
        ExprValue::String("10-12".into()),
    ] {
        let mut f = fixture();
        let r = f
            .run_with(
                "for i in 12 10 11; do echo \"openjd_output_var: N[$i]=$i\"; done",
                Some(&d),
                Some(&chunk(value)),
            )
            .await;
        let mut expected = IndexMap::new();
        for i in [10, 11, 12] {
            expected.insert(
                Some(i),
                IndexMap::from([("N".to_string(), ExprValue::Int(i))]),
            );
        }
        assert_eq!(r.outputs, Some(expected));
        assert_eq!(
            f.reported()[0],
            (Some(12), "N".to_string(), ExprValue::Int(12))
        );
    }
}

#[tokio::test]
async fn chunk_selector_rules_and_missing_task() {
    let d = decls(N_INT);
    let mut f = fixture();
    let r = f
        .run_with(
            "echo 'openjd_output_var: N=1'",
            Some(&d),
            Some(&chunk(ExprValue::String("1-2".into()))),
        )
        .await;
    assert_failed(
        &f,
        &r,
        "Output variable 'N': a task selector, as in 'N[<value>]', is required in a Step with a \
         CHUNK[INT] parameter, in 'openjd_output_var: N=1'",
    );
    let mut f = fixture();
    let r = f
        .run_with(
            "echo 'openjd_output_var: N[7]=1'",
            Some(&d),
            Some(&chunk(ExprValue::String("1-2".into()))),
        )
        .await;
    assert_failed(
        &f,
        &r,
        "Output variable 'N[7]': 7 is not a value of the chunk being run (chunk values 1, \
         2), in \
         'openjd_output_var: N[7]=1'",
    );
    let mut f = fixture();
    let r = f
        .run_with(
            "echo 'openjd_output_var: N[1]=1'",
            Some(&d),
            Some(&chunk(ExprValue::String("1-2".into()))),
        )
        .await;
    assert_failed(
        &f,
        &r,
        "Output variable 'N[2]': no value was reported (collect mode ONCE)",
    );
}

#[tokio::test]
async fn unparsable_chunk_fails_the_action() {
    for (text, message) in [
        (
            "1-x",
            "CHUNK[INT] parameter 'F' value '1-x' is not a range expression: Expected integer \
             in '1-x'",
        ),
        (
            "1,1",
            "CHUNK[INT] parameter 'F' value '1,1' is not a range expression: Range expression \
             has overlapping ranges: 1 and 1",
        ),
        (
            "",
            "CHUNK[INT] parameter 'F' value '3' is not a range expression",
        ),
    ] {
        // An empty text stands for an integer value, which is not a range.
        let value = match text {
            "" => ExprValue::Int(3),
            t => ExprValue::String(t.into()),
        };
        let mut f = fixture();
        let d = decls(N_INT);
        let err = f
            .session
            .run_task(
                "S",
                &step("true"),
                Some(&chunk(value)),
                None,
                None,
                &RunTaskOptions {
                    output_variables: Some(&d),
                    path_inputs: &[],
                },
            )
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), message);
        assert_eq!(
            f.session.action_status().unwrap().state,
            ActionState::Failed
        );
    }
}

// ── PATH values ─────────────────────────────────────────────────────

fn rule(src: &str, dst: &str) -> PathMappingRule {
    PathMappingRule {
        source_path_format: PathFormat::Posix,
        source_path: src.into(),
        destination_path: dst.into(),
    }
}

#[tokio::test]
async fn path_output_is_returned_in_the_submission_form() {
    let mut f = fixture_with(
        true,
        true,
        Some(vec![rule("/src", "/mnt/dst")]),
        Default::default(),
    );
    let d = decls(r#"[{"name": "P", "type": "PATH"}, {"name": "L", "type": "LIST[PATH]"}]"#);
    let r = f
        .run(
            r#"echo 'openjd_output_var: P="/mnt/dst/a"'
               echo 'openjd_output_var: L=["/mnt/dst/b", "s3://k"]'"#,
            Some(&d),
        )
        .await;
    let outputs = r.outputs.unwrap();
    assert_eq!(
        outputs[&None]["P"],
        ExprValue::new_path("/src/a", PathFormat::Posix)
    );
    assert_eq!(
        outputs[&None]["L"].to_display_string(),
        r#"["/src/b", "s3://k"]"#
    );

    let mut f = fixture_with(
        true,
        true,
        Some(vec![rule("/src", "/mnt/dst")]),
        Default::default(),
    );
    let r = f
        .run(r#"echo 'openjd_output_var: P="/tmp/x"'"#, Some(&d))
        .await;
    assert_failed(
        &f,
        &r,
        "Output variable 'P': value '/tmp/x' is not a valid PATH output: no path mapping rule \
         maps the path back to the submission path context, so it names a location only this \
         worker can see",
    );
}

/// A consumer Step's resolved_symtab, as `attach_step_inputs` writes it:
/// path-typed `Raw` values stored as strings and listed in `path_inputs`.
fn consumer_symtab() -> (openjd_expr::SerializedSymbolTable, Vec<PathInput>) {
    let mut st = SymbolTable::new();
    st.set(
        "Steps.P.RawOutput.X",
        ExprValue::String("/src/shot/out".into()),
    )
    .unwrap();
    st.set(
        "Steps.P.TaskRawParam.N",
        ExprValue::make_list(
            vec![
                ExprValue::String("/src/a".into()),
                ExprValue::String("/other/b".into()),
            ],
            ExprType::STRING,
        )
        .unwrap(),
    )
    .unwrap();
    st.set("Steps.P.RawOutput.W", ExprValue::String("C:/x/y".into()))
        .unwrap();
    st.set("Steps.Q.RawOutput.Z", ExprValue::Null).unwrap();
    let inputs = vec![
        PathInput {
            symbol: "Steps.P.RawOutput.X".into(),
            ty: ExprType::PATH,
        },
        PathInput {
            symbol: "Steps.P.TaskRawParam.N".into(),
            ty: ExprType::list(ExprType::PATH),
        },
        PathInput {
            symbol: "Steps.P.RawOutput.W".into(),
            ty: ExprType::PATH,
        },
        PathInput {
            symbol: "Steps.Q.RawOutput.Z".into(),
            ty: ExprType::PATH,
        },
    ];
    (openjd_expr::SerializedSymbolTable::from_symtab(&st), inputs)
}

#[tokio::test]
async fn path_inputs_are_mapped_forward_and_raw_forms_kept() {
    let f = fixture_with(
        true,
        true,
        Some(vec![rule("/src", "/mnt/dst")]),
        Default::default(),
    );
    let (base, inputs) = consumer_symtab();
    let st = f
        .session
        .build_symbol_table(None, Some(&base), &inputs)
        .unwrap();
    let get = |k: &str| st.get_value(k).cloned().unwrap();
    assert_eq!(
        get("Steps.P.RawOutput.X"),
        ExprValue::new_path("/src/shot/out", PathFormat::Posix)
    );
    assert_eq!(
        get("Steps.P.Output.X"),
        ExprValue::new_path("/mnt/dst/shot/out", PathFormat::host())
    );
    assert_eq!(
        get("Steps.P.TaskParam.N").to_display_string(),
        r#"["/mnt/dst/a", "/other/b"]"#
    );
    assert_eq!(
        get("Steps.P.TaskRawParam.N").expr_type(),
        ExprType::list(ExprType::PATH)
    );
    // A Windows submission path keeps its own format, so it is not rewritten
    // to the worker's.
    assert_eq!(
        get("Steps.P.RawOutput.W"),
        ExprValue::new_path("C:\\x\\y", PathFormat::Windows)
    );
    // A skipped producer's null stays null, and gets no twin.
    assert_eq!(get("Steps.Q.RawOutput.Z"), ExprValue::Null);
    assert!(st.get_value("Steps.Q.Output.Z").is_none());
}

#[tokio::test]
async fn task_script_reads_path_inputs() {
    let mut f = fixture_with(
        true,
        true,
        Some(vec![rule("/src", "/mnt/dst")]),
        Default::default(),
    );
    let (base, inputs) = consumer_symtab();
    let r = f
        .session
        .run_task(
            "S",
            &step(
                "echo O={{Steps.P.Output.X}} R={{Steps.P.RawOutput.X}} \
                 N={{Steps.P.RawOutput.X.name}}",
            ),
            None,
            Some(&base),
            None,
            &RunTaskOptions {
                output_variables: None,
                path_inputs: &inputs,
            },
        )
        .await
        .unwrap();
    assert_eq!(r.state, ActionState::Success, "{}", f.fail_message());
    assert!(
        r.stdout
            .contains("O=/mnt/dst/shot/out R=/src/shot/out N=out"),
        "{}",
        r.stdout
    );
}

// ── Audit round 1 ───────────────────────────────────────────────────

#[tokio::test]
async fn openjd_fail_after_an_invalid_directive_keeps_its_message() {
    // A cancel handler that prints openjd_fail must not replace the reason
    // RFC 0010 requires: the output, the problem, and the text.
    let mut f = fixture();
    let d = decls(N_INT);
    let mut script = step(
        "trap 'echo \"openjd_fail: trapped\"; exit 0' TERM
         echo 'openjd_output_var: N=x'
         sleep 20 & wait",
    );
    script.actions.on_run.cancelation =
        Some(openjd_model::job::CancelationMode::NotifyThenTerminate {
            notify_period_in_seconds: Some(fs("5")),
        });
    let r = f
        .session
        .run_task(
            "S",
            &script,
            None,
            None,
            None,
            &RunTaskOptions {
                output_variables: Some(&d),
                path_inputs: &[],
            },
        )
        .await
        .unwrap();
    assert_failed(
        &f,
        &r,
        "Output variable 'N': 'x' is not one valid JSON value: expected value at line 1 column 1",
    );
}

/// Records every status callback's state and fail message.
type Statuses = Arc<Mutex<Vec<(ActionState, Option<String>)>>>;

fn status_fixture() -> (Fixture, Statuses) {
    let mut f = fixture();
    let statuses: Statuses = Arc::default();
    let sink = statuses.clone();
    let tmp = TempDir::new().unwrap();
    let config = SessionConfig {
        limits: Default::default(),
        session_id: "cf".into(),
        job_parameter_values: Default::default(),
        session_root_directory: Some(tmp.path().to_path_buf()),
        path_mapping_rules: None,
        retain_working_dir: false,
        callback: Some(Box::new(move |_, status| {
            sink.lock()
                .unwrap()
                .push((status.state, status.fail_message.clone()))
        })),
        os_env_vars: None,
        user: None,
        profile: None,
        cancel_token: None,
        debug_collect_stdout: true,
        echo_openjd_directives: true,
        control_flow: true,
        output_callback: None,
        sticky_bit_policy: openjd_sessions::StickyBitPolicy::Disabled,
    };
    f.session = Session::with_config(config).unwrap();
    f._tmp = tmp;
    (f, statuses)
}

#[tokio::test]
async fn missing_output_fails_without_a_success_status_first() {
    let (mut f, statuses) = status_fixture();
    let d = decls(N_INT);
    let r = f.run("echo hi", Some(&d)).await;
    assert_failed(
        &f,
        &r,
        "Output variable 'N': no value was reported (collect mode ONCE)",
    );
    let statuses = statuses.lock().unwrap().clone();
    assert!(
        !statuses.iter().any(|(s, _)| *s == ActionState::Success),
        "{statuses:?}"
    );
    assert_eq!(
        statuses.last().unwrap(),
        &(
            ActionState::Failed,
            Some("Output variable 'N': no value was reported (collect mode ONCE)".to_string())
        )
    );
    // The session is left as after any failed action.
    assert_eq!(
        f.session.state(),
        openjd_sessions::SessionState::ReadyEnding
    );
}

#[tokio::test]
async fn output_values_fire_no_status_callback() {
    let d = decls(r#"[{"name": "A", "type": "LIST[INT]", "collect": "APPEND"}]"#);
    let (mut f, without) = status_fixture();
    f.run("true", Some(&d)).await;
    let (mut f, with) = status_fixture();
    f.run(
        "for i in 1 2 3 4 5; do echo \"openjd_output_var: A=$i\"; done",
        Some(&d),
    )
    .await;
    assert_eq!(without.lock().unwrap().len(), with.lock().unwrap().len());
}

#[tokio::test]
async fn invalid_directive_is_logged_after_the_log_level_is_raised() {
    testing_logger::setup();
    let mut f = fixture();
    let d = decls(N_INT);
    f.run(
        "echo 'openjd_session_runtime_loglevel: ERROR'; echo 'openjd_output_var: N=x'; \
         echo 'openjd_output_var: N=1' >/dev/null; echo hidden",
        Some(&d),
    )
    .await;
    testing_logger::validate(|captured| {
        let lines = logged(captured);
        assert!(
            lines.iter().any(|l| l.ends_with(
                "openjd_output_var: N=x -- ERROR: Output variable 'N': 'x' is not one valid JSON \
                 value: expected value at line 1 column 1"
            )),
            "{lines:#?}"
        );
        assert!(!lines.iter().any(|l| l.ends_with("hidden")), "{lines:#?}");
    });
}

#[tokio::test]
async fn valid_directive_is_hidden_after_the_log_level_is_raised() {
    testing_logger::setup();
    let mut f = fixture();
    let d = decls(N_INT);
    f.run(
        "echo 'openjd_session_runtime_loglevel: ERROR'; echo 'openjd_output_var: N=1'",
        Some(&d),
    )
    .await;
    testing_logger::validate(|captured| {
        let lines = logged(captured);
        assert!(
            !lines.iter().any(|l| l.ends_with("openjd_output_var: N=1")),
            "{lines:#?}"
        );
    });
}

#[tokio::test]
async fn directive_lines_are_redacted() {
    let mut f = fixture_with(true, true, None, Default::default());
    let d = decls(r#"[{"name": "S", "type": "STRING"}]"#);
    let mut env = env(
        "E",
        Some(sh_action("echo 'openjd_redacted_env: SEC=hunter2'")),
        None,
    );
    env.name = "E".into();
    f.session
        .enter_environment(
            &env,
            None,
            None,
            None,
            &EnvironmentOptions {
                origin: EnvironmentOrigin::EnvironmentTemplate,
                path_inputs: &[],
            },
        )
        .await
        .unwrap();
    let r = f
        .run(
            r#"echo 'openjd_output_var: S="hunter2"'; echo 'openjd_output_var: S=hunter2'"#,
            Some(&d),
        )
        .await;
    assert_eq!(r.state, ActionState::Failed);
    assert!(!r.stdout.contains("hunter2"), "{}", r.stdout);
    assert!(
        r.stdout.contains("openjd_output_var: S=\"********\""),
        "{}",
        r.stdout
    );
}

#[tokio::test]
async fn at_file_size_limit_is_exact() {
    let d = decls(r#"[{"name": "S", "type": "STRING"}]"#);
    // Exactly 1 MiB: accepted.
    let mut f = fixture();
    let r = f
        .run(
            r#"printf '"%01048574d"' 0 > v; echo 'openjd_output_var: S=@v'"#,
            Some(&d),
        )
        .await;
    assert_eq!(r.state, ActionState::Success, "{}", f.fail_message());
    // One byte more: refused.
    let mut f = fixture();
    let r = f
        .run(
            r#"printf '"%01048575d"' 0 > v; echo 'openjd_output_var: S=@v'"#,
            Some(&d),
        )
        .await;
    assert_failed(
        &f,
        &r,
        "Output variable 'S': the @ file 'v' is larger than the 1048576 bytes the Task's output \
         values have left (the limit is 1048576 bytes per Task)",
    );
}

#[tokio::test]
async fn at_file_is_bounded_by_what_the_task_has_left() {
    let mut f = fixture();
    let d = decls(r#"[{"name": "A", "type": "STRING"}, {"name": "B", "type": "STRING"}]"#);
    let r = f
        .run(
            r#"printf '"%0999998d"' 0 > a; echo 'openjd_output_var: A=@a'
               printf '"%099998d"' 0 > b; echo 'openjd_output_var: B=@b'"#,
            Some(&d),
        )
        .await;
    assert_failed(
        &f,
        &r,
        "Output variable 'B': the @ file 'b' is larger than the 48576 bytes the Task's output \
         values have left (the limit is 1048576 bytes per Task)",
    );
}

/// The format of a `path` value (`ExprValue`'s equality ignores it).
fn format_of(v: &ExprValue) -> PathFormat {
    match v {
        ExprValue::Path { format, .. } => *format,
        other => panic!("not a path: {other:?}"),
    }
}

#[tokio::test]
async fn path_inputs_keep_each_stored_format() {
    let f = fixture_with(
        true,
        true,
        Some(vec![rule("/src", "/mnt/dst")]),
        Default::default(),
    );
    let mut st = SymbolTable::new();
    st.set(
        "Steps.P.RawOutput.U",
        ExprValue::String(r"\\srv\share\x".into()),
    )
    .unwrap();
    st.set("Steps.P.RawOutput.W", ExprValue::String("C:/x/y".into()))
        .unwrap();
    st.set("Steps.P.RawOutput.I", ExprValue::String("s3://b/k".into()))
        .unwrap();
    st.set("Steps.P.RawOutput.X", ExprValue::String("/src/a".into()))
        .unwrap();
    // A colon second is a drive only after a letter.
    st.set("Steps.P.RawOutput.C", ExprValue::String("/:x".into()))
        .unwrap();
    let inner = |v: &[&str]| {
        ExprValue::make_list(
            v.iter().map(|s| ExprValue::String(s.to_string())).collect(),
            ExprType::STRING,
        )
        .unwrap()
    };
    st.set(
        "Steps.P.TaskRawParam.G",
        ExprValue::make_list(
            vec![inner(&["/src/a", "/src/b"]), inner(&[])],
            ExprType::list(ExprType::STRING),
        )
        .unwrap(),
    )
    .unwrap();
    let input = |s: &str, ty: ExprType| PathInput {
        symbol: s.into(),
        ty,
    };
    let inputs = vec![
        input("Steps.P.RawOutput.U", ExprType::PATH),
        input("Steps.P.RawOutput.W", ExprType::PATH),
        input("Steps.P.RawOutput.I", ExprType::PATH),
        input("Steps.P.RawOutput.X", ExprType::PATH),
        input("Steps.P.RawOutput.C", ExprType::PATH),
        input(
            "Steps.P.TaskRawParam.G",
            ExprType::list(ExprType::list(ExprType::PATH)),
        ),
    ];
    let base = openjd_expr::SerializedSymbolTable::from_symtab(&st);
    let st = f
        .session
        .build_symbol_table(None, Some(&base), &inputs)
        .unwrap();
    let get = |k: &str| st.get_value(k).cloned().unwrap();
    assert_eq!(format_of(&get("Steps.P.RawOutput.U")), PathFormat::Windows);
    assert_eq!(format_of(&get("Steps.P.RawOutput.W")), PathFormat::Windows);
    assert_eq!(format_of(&get("Steps.P.RawOutput.I")), PathFormat::Uri);
    assert_eq!(format_of(&get("Steps.P.RawOutput.X")), PathFormat::Posix);
    assert_eq!(format_of(&get("Steps.P.RawOutput.C")), PathFormat::Posix);
    assert_eq!(format_of(&get("Steps.P.Output.X")), PathFormat::host());
    assert_eq!(
        get("Steps.P.TaskParam.G").to_display_string(),
        r#"[["/mnt/dst/a", "/mnt/dst/b"], []]"#
    );
    assert_eq!(
        get("Steps.P.TaskRawParam.G").expr_type(),
        ExprType::list(ExprType::list(ExprType::PATH))
    );
}

#[tokio::test]
async fn path_input_errors_name_the_symbol() {
    let f = fixture();
    let mut st = SymbolTable::new();
    st.set("Steps.P.RawOutput.X", ExprValue::String("/a".into()))
        .unwrap();
    st.set("Steps.P.RawOutput.N", ExprValue::Int(3)).unwrap();
    let base = openjd_expr::SerializedSymbolTable::from_symtab(&st);
    let err = |inputs: Vec<PathInput>, base: Option<&openjd_expr::SerializedSymbolTable>| {
        f.session
            .build_symbol_table(None, base, &inputs)
            .unwrap_err()
            .to_string()
    };
    let input = |s: &str, ty: ExprType| PathInput {
        symbol: s.into(),
        ty,
    };
    assert_eq!(
        err(
            vec![input("Steps.P.RawOutput.Y", ExprType::PATH)],
            Some(&base)
        ),
        "Path input 'Steps.P.RawOutput.Y' in resolved_symtab: missing"
    );
    assert_eq!(
        err(vec![input("Steps.P.Output.X", ExprType::PATH)], Some(&base)),
        "Path input 'Steps.P.Output.X' in resolved_symtab: not a \
         Steps.<Step>.RawOutput.<Name> or Steps.<Step>.TaskRawParam.<Name> symbol"
    );
    assert_eq!(
        err(
            vec![input("Steps.P.RawOutput.N", ExprType::PATH)],
            Some(&base)
        ),
        "Path input 'Steps.P.RawOutput.N' in resolved_symtab: it is recorded as \
         path, so its value must be stored as a string, but it is int"
    );
    assert_eq!(
        err(
            vec![input("Steps.P.RawOutput.X", ExprType::list(ExprType::PATH))],
            Some(&base)
        ),
        "Path input 'Steps.P.RawOutput.X' in resolved_symtab: it is recorded as \
         list[path], so its value must be stored as a list of strings, but it is string"
    );
    assert_eq!(
        err(
            vec![input("Steps.P.RawOutput.X", ExprType::INT)],
            Some(&base)
        ),
        "Path input 'Steps.P.RawOutput.X' in resolved_symtab: its type int is not \
         path, list[path], or list[list[path]]"
    );
    assert_eq!(
        err(vec![input("Steps.P.RawOutput.X", ExprType::PATH)], None),
        "Path input 'Steps.P.RawOutput.X' has no value: path inputs are read from \
         the Step's resolved_symtab, and none was given"
    );
}

#[tokio::test]
async fn environments_read_path_inputs_on_enter_and_exit() {
    let mut f = fixture_with(
        true,
        true,
        Some(vec![rule("/src", "/mnt/dst")]),
        Default::default(),
    );
    let (base, inputs) = consumer_symtab();
    let trace = f.wd().join("trace");
    let mut e = env(
        "E",
        Some(sh_action(&format!(
            "echo enter={{{{Steps.P.Output.X}}}} >> {}",
            trace.display()
        ))),
        None,
    );
    e.script.as_mut().unwrap().actions.on_exit = Some(sh_action(&format!(
        "echo exit={{{{Steps.P.Output.X}}}} >> {}",
        trace.display()
    )));
    let options = EnvironmentOptions {
        origin: EnvironmentOrigin::JobTemplate,
        path_inputs: &inputs,
    };
    let id = f
        .session
        .enter_environment(&e, Some(&base), None, None, &options)
        .await
        .unwrap();
    f.session
        .exit_environment(&id, Some(&base), true, None, &options)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&trace).unwrap(),
        "enter=/mnt/dst/shot/out\nexit=/mnt/dst/shot/out\n"
    );
}

#[tokio::test]
async fn exit_environment_follows_its_origin() {
    // An Environment Template's onExit may print a directive; a Job
    // Template's may not.
    for (origin, fails) in [
        (EnvironmentOrigin::EnvironmentTemplate, false),
        (EnvironmentOrigin::JobTemplate, true),
    ] {
        let mut f = fixture();
        let mut e = env("E", None, None);
        e.script.as_mut().unwrap().actions.on_exit =
            Some(sh_action("echo 'openjd_output_var: N=1'"));
        let options = EnvironmentOptions {
            origin,
            path_inputs: &[],
        };
        let id = f
            .session
            .enter_environment(&e, None, None, None, &options)
            .await
            .unwrap();
        let r = f
            .session
            .exit_environment(&id, None, true, None, &options)
            .await;
        assert_eq!(r.is_err(), fails, "{origin:?}");
        if fails {
            assert_eq!(f.fail_message(), FORBIDDEN);
        }
    }
}

#[tokio::test]
async fn directive_lines_up_to_64_kib_are_accepted() {
    // The same lines as the cross-user test_cross_user_output_var_long_lines.
    let mut f = fixture();
    let d = decls(r#"[{"name": "A", "type": "STRING"}, {"name": "B", "type": "STRING"}]"#);
    let r = f
        .run(
            r#"printf 'openjd_output_var: A="%s"\r\n' "$(head -c 65513 /dev/zero | tr '\0' a)"
               printf 'openjd_output_var: B="'; head -c 65512 /dev/zero | tr '\0' '\\'; printf '"\n'"#,
            Some(&d),
        )
        .await;
    assert_eq!(r.state, ActionState::Success, "{}", f.fail_message());
    let outputs = r.outputs.unwrap();
    assert_eq!(outputs[&None]["A"], ExprValue::String("a".repeat(65_513)));
    assert_eq!(outputs[&None]["B"], ExprValue::String("\\".repeat(32_756)));
}

// ── Audit round 2 ───────────────────────────────────────────────────

/// Run `script` as the onRun of a Step declaring `N`, with a 5 second
/// notify period so a TERM trap can print before the action ends.
async fn run_notify(f: &mut Fixture, script: &str) -> ActionResult {
    let d = decls(N_INT);
    let mut script = step(script);
    script.actions.on_run.cancelation =
        Some(openjd_model::job::CancelationMode::NotifyThenTerminate {
            notify_period_in_seconds: Some(fs("5")),
        });
    f.session
        .run_task(
            "S",
            &script,
            None,
            None,
            None,
            &RunTaskOptions {
                output_variables: Some(&d),
                path_inputs: &[],
            },
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn malformed_env_after_an_invalid_directive_keeps_its_message() {
    let mut f = fixture();
    let r = run_notify(
        &mut f,
        "echo 'openjd_output_var: N=x'
         echo 'openjd_env: noequals'
         sleep 20 & wait",
    )
    .await;
    assert_failed(
        &f,
        &r,
        "Output variable 'N': 'x' is not one valid JSON value: expected value at line 1 column 1",
    );
}

#[tokio::test]
async fn openjd_fail_after_a_malformed_env_replaces_its_message() {
    // Only the openjd_output_var failure is final; a malformed openjd_env
    // keeps the last-write-wins behavior of the Python reference.
    let mut f = fixture();
    let r = run_notify(
        &mut f,
        "trap 'echo \"openjd_fail: trapped\"; exit 0' TERM
         echo 'openjd_env: noequals'
         sleep 20 & wait",
    )
    .await;
    assert_failed(&f, &r, "trapped");
}

#[tokio::test]
async fn a_final_fail_message_does_not_carry_into_the_next_action() {
    let mut f = fixture();
    let mut e = env("E", None, None);
    e.script.as_mut().unwrap().actions.on_exit =
        Some(sh_action("echo 'openjd_fail: exit failed'; exit 1"));
    let id = f
        .session
        .enter_environment(&e, None, None, None, &EnvironmentOptions::default())
        .await
        .unwrap();
    let d = decls(N_INT);
    let r = f.run("echo 'openjd_output_var: N=x'", Some(&d)).await;
    assert_eq!(r.state, ActionState::Failed);
    let r = f
        .session
        .exit_environment(&id, None, true, None, &EnvironmentOptions::default())
        .await;
    assert!(r.is_err());
    assert_eq!(f.fail_message(), "exit failed");
}

#[tokio::test]
async fn declared_outputs_do_not_change_a_failure_timeout_or_cancel() {
    // The missing-output check runs only after a successful exit.
    let mut f = fixture();
    let d = decls(N_INT);
    let r = f.run("echo 'openjd_fail: mine'; exit 2", Some(&d)).await;
    assert_failed(&f, &r, "mine");

    let mut f = fixture();
    let mut script = step("sleep 10");
    script.actions.on_run.timeout = Some(fs("1"));
    let r = f
        .session
        .run_task(
            "S",
            &script,
            None,
            None,
            None,
            &RunTaskOptions {
                output_variables: Some(&d),
                path_inputs: &[],
            },
        )
        .await
        .unwrap();
    assert_eq!(r.state, ActionState::Timeout);
    assert_eq!(f.fail_message(), "");

    let mut f = fixture();
    let handle = f.session.cancel_handle();
    let canceler = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        handle.cancel(Some(Duration::ZERO), false)
    });
    let r = f.run("sleep 30", Some(&d)).await;
    assert!(canceler.await.unwrap());
    assert_eq!(r.state, ActionState::Canceled);
    assert_eq!(f.fail_message(), "");
}

#[tokio::test]
async fn missing_output_message_is_in_the_action_output() {
    testing_logger::setup();
    let mut f = fixture();
    let d = decls(N_INT);
    let r = f.run("true", Some(&d)).await;
    assert_failed(
        &f,
        &r,
        "Output variable 'N': no value was reported (collect mode ONCE)",
    );
    testing_logger::validate(|captured| {
        let line = "ERROR: Output variable 'N': no value was reported (collect mode ONCE)";
        assert!(
            logged(captured).iter().any(|l| l == line),
            "{:?}",
            logged(captured)
        );
    });
}

#[tokio::test]
async fn job_environment_without_control_flow_prints_ordinary_output() {
    let mut f = fixture_with(false, true, None, Default::default());
    let e = env("E", Some(sh_action("echo 'openjd_output_var: N=1'")), None);
    let (_, out) = f
        .session
        .enter_environment_with_output(&e, None, None, None, &EnvironmentOptions::default())
        .await
        .unwrap();
    assert!(out.contains("openjd_output_var: N=1"));
}

#[tokio::test]
async fn path_inputs_accept_an_empty_list_stored_as_either_element_type() {
    // An empty list carries no element to tell its type by, so a stored
    // `list[list[string]] []` is a `list[path]` and a `list[string] []` a
    // `list[list[path]]`.
    let f = fixture();
    let mut st = SymbolTable::new();
    st.set(
        "Steps.P.TaskRawParam.A",
        ExprValue::make_list(vec![], ExprType::list(ExprType::STRING)).unwrap(),
    )
    .unwrap();
    st.set(
        "Steps.P.TaskRawParam.B",
        ExprValue::make_list(vec![], ExprType::STRING).unwrap(),
    )
    .unwrap();
    let inputs = vec![
        PathInput {
            symbol: "Steps.P.TaskRawParam.A".into(),
            ty: ExprType::list(ExprType::PATH),
        },
        PathInput {
            symbol: "Steps.P.TaskRawParam.B".into(),
            ty: ExprType::list(ExprType::list(ExprType::PATH)),
        },
    ];
    let base = openjd_expr::SerializedSymbolTable::from_symtab(&st);
    let st = f
        .session
        .build_symbol_table(None, Some(&base), &inputs)
        .unwrap();
    let get = |k: &str| st.get_value(k).cloned().unwrap();
    assert_eq!(
        get("Steps.P.TaskParam.A").expr_type(),
        ExprType::list(ExprType::PATH)
    );
    assert_eq!(
        get("Steps.P.TaskParam.B").expr_type(),
        ExprType::list(ExprType::list(ExprType::PATH))
    );
}

#[test]
fn control_flow_types_are_reexported_at_the_crate_root() {
    let _: Option<openjd_sessions::ActionOutputs> = None;
    let _: Option<openjd_sessions::OutputCallbackType> = None;
    let _ = openjd_sessions::EnvironmentOptions {
        origin: openjd_sessions::EnvironmentOrigin::JobTemplate,
        path_inputs: &[],
    };
    let _ = openjd_sessions::RunTaskOptions::default();
}

#[tokio::test]
async fn inline_values_over_the_size_limit_fail_with_the_line() {
    // 18 APPEND values of 60,002 bytes each pass the 1 MiB per-Task limit.
    let mut f = fixture();
    let d = decls(r#"[{"name": "A", "type": "LIST[STRING]", "collect": "APPEND"}]"#);
    let r = f
        .run(
            r#"v=$(head -c 60000 /dev/zero | tr '\0' a)
               for i in $(seq 18); do echo "openjd_output_var: A=\"$v\""; done"#,
            Some(&d),
        )
        .await;
    assert_failed(
        &f,
        &r,
        &format!(
            "Output variable 'A': the value's 60002 bytes take the Task's output values to \
             1080036 bytes, over the limit of 1048576 bytes, in 'openjd_output_var: A=\"{}...'",
            "a".repeat(178)
        ),
    );
}

// ── Audit round 4 ───────────────────────────────────────────────────

#[tokio::test]
async fn at_directive_failures_name_the_line() {
    for (decl, script, message) in [
        (
            N_INT,
            "printf 2 > f; echo 'openjd_output_var: N=1'; echo 'openjd_output_var: N=@f'",
            "Output variable 'N': a second value was reported, but its collect mode is ONCE, in \
             'openjd_output_var: N=@f'",
        ),
        (
            N_INT,
            "printf x > f; echo 'openjd_output_var: N=@f'",
            "Output variable 'N': the @ file's content is not one valid JSON value: expected \
             value at line 1 column 1, in 'openjd_output_var: N=@f'",
        ),
        (
            N_INT,
            "printf '\"abc\"' > f; echo 'openjd_output_var: N=@f'",
            "Output variable 'N': value (string) does not convert to int, in \
             'openjd_output_var: N=@f'",
        ),
        (
            r#"[{"name": "N", "type": "INT", "constraints": {"minValue": {"int": 5}}}]"#,
            "printf 1 > f; echo 'openjd_output_var: N=@f'",
            "Output variable 'N': value 1 is less than minimum 5, in 'openjd_output_var: N=@f'",
        ),
        (
            r#"[{"name": "E", "type": "LIST[INT]", "collect": "APPEND"}]"#,
            "printf 1 > a; printf x > b; echo 'openjd_output_var: E=@a'; \
             echo 'openjd_output_var: E=@b'",
            "Output variable 'E': the @ file's content is not one valid JSON value: expected \
             value at line 1 column 1, in 'openjd_output_var: E=@b'",
        ),
    ] {
        let mut f = fixture();
        let d = decls(decl);
        let r = f.run(script, Some(&d)).await;
        assert_failed(&f, &r, message);
    }
    // A selector problem on an @ directive, in a chunked Step.
    let mut f = fixture();
    let d = decls(N_INT);
    let r = f
        .run_with(
            "printf 1 > f; echo 'openjd_output_var: N[7]=@f'",
            Some(&d),
            Some(&chunk(ExprValue::String("1-2".into()))),
        )
        .await;
    assert_failed(
        &f,
        &r,
        "Output variable 'N[7]': 7 is not a value of the chunk being run (chunk values 1, 2), \
         in 'openjd_output_var: N[7]=@f'",
    );
}
