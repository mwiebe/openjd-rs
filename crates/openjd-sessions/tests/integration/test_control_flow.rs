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
         to int: 1.5 is not a whole number",
    );
}

#[tokio::test]
async fn collector_failures_fail_the_task_with_their_message() {
    for (decl, script, message) in [
        (
            N_INT,
            "echo 'openjd_output_var: M=1'",
            "Output variable 'M': the Step does not declare it (declared: 'N')",
        ),
        (
            N_INT,
            "echo 'openjd_output_var: N=1'; echo 'openjd_output_var: N=2'",
            "Output variable 'N': a second value was reported, but its collect mode is ONCE",
        ),
        (
            N_INT,
            "echo 'openjd_output_var: N=null'",
            "Output variable 'N': value is null; output values cannot be or contain null",
        ),
        (
            N_INT,
            "echo 'openjd_output_var: N[1]=1'",
            "Output variable 'N[1]': a task selector is only allowed in a Step with a CHUNK[INT] \
             parameter",
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
            "Output variable 'N': value 6 exceeds maximum 5",
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
         int: invalid digit found in string",
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
    let e = env("E", Some(sh_action("echo 'openjd_output_var: N=1'")), None);
    let err = f
        .session
        .enter_environment(&e, None, None, None, &EnvironmentOptions::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("onEnter"), "{err}");
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
    assert_eq!(r.state, ActionState::Failed);
    assert!(
        f.fail_message().starts_with(
            "Invalid openjd_output_var directive: the line is longer than the 65536 byte line \
             limit, so it was cut; write a large value to a file and report it with @<file>, in \
             'openjd_output_var: S=\"0000"
        ),
        "{}",
        f.fail_message()
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
    assert_eq!(r.state, ActionState::Failed);
    assert!(
        f.fail_message()
            .starts_with("Output variable 'N': '1\\u{c}' is not one valid JSON value"),
        "{}",
        f.fail_message()
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
            "Output variable 'N': the @ file's content contains a NUL character".to_string(),
        ),
        (
            "printf '1\\377' > z; echo 'openjd_output_var: N=@z'".to_string(),
            "Output variable 'N': the @ file's content is not valid UTF-8 (at byte 1)".to_string(),
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
         CHUNK[INT] parameter",
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
    let mut f = fixture();
    let d = decls(N_INT);
    let err = f
        .session
        .run_task(
            "S",
            &step("true"),
            Some(&chunk(ExprValue::String("1-x".into()))),
            None,
            None,
            &RunTaskOptions {
                output_variables: Some(&d),
                path_inputs: &[],
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "CHUNK[INT] parameter 'F' value '1-x' is not a range expression"
    );
    assert_eq!(
        f.session.action_status().unwrap().state,
        ActionState::Failed
    );
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
    assert_eq!(r.state, ActionState::Failed);
    assert!(
        f.fail_message()
            .starts_with("Output variable 'P': value '/tmp/x' is not a valid PATH output"),
        "{}",
        f.fail_message()
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
