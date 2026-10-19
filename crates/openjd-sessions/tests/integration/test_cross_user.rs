// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Cross-user tests for openjd-sessions.
//!
//! All tests are `#[ignore]` — they require a Docker container with sudo
//! configured and the following env vars set:
//!   OPENJD_TEST_SUDO_TARGET_USER / OPENJD_TEST_SUDO_SHARED_GROUP
//!   OPENJD_TEST_SUDO_DISJOINT_USER / OPENJD_TEST_SUDO_DISJOINT_GROUP
//!
//! Run with: cargo test -p openjd-sessions --features test-utils -- --include-ignored
//!
//! These tests are POSIX-only (sudo, process groups, file ownership).
#![cfg(unix)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use openjd_sessions::action::ActionState;
use openjd_sessions::session::{Session, SessionCancelHandle, SessionConfig};
use openjd_sessions::session_user::PosixSessionUser;
use openjd_sessions::tempdir::TempDir;

fn target_user() -> Option<Arc<PosixSessionUser>> {
    let user = std::env::var("OPENJD_TEST_SUDO_TARGET_USER").ok()?;
    let group = std::env::var("OPENJD_TEST_SUDO_SHARED_GROUP").ok()?;
    Some(Arc::new(PosixSessionUser::new(&user, Some(&group))))
}

fn disjoint_user() -> Option<Arc<PosixSessionUser>> {
    let user = std::env::var("OPENJD_TEST_SUDO_DISJOINT_USER").ok()?;
    let group = std::env::var("OPENJD_TEST_SUDO_DISJOINT_GROUP").ok()?;
    Some(Arc::new(PosixSessionUser::new(&user, Some(&group))))
}

fn require_target_user() -> Arc<PosixSessionUser> {
    target_user()
        .expect("OPENJD_TEST_SUDO_TARGET_USER and OPENJD_TEST_SUDO_SHARED_GROUP must be set")
}

fn require_disjoint_user() -> Arc<PosixSessionUser> {
    disjoint_user()
        .expect("OPENJD_TEST_SUDO_DISJOINT_USER and OPENJD_TEST_SUDO_DISJOINT_GROUP must be set")
}

fn make_session(user: Arc<PosixSessionUser>) -> Session {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    // Leak so the dir outlives the session constructor
    std::mem::forget(tmp);
    let config = SessionConfig {
        limits: Default::default(),
        session_id: "cross-user-test".into(),
        job_parameter_values: HashMap::new(),
        path_mapping_rules: None,
        retain_working_dir: false,
        callback: None,
        os_env_vars: None,
        session_root_directory: Some(root),
        user: Some(user),
        profile: None,
        cancel_token: None,
        debug_collect_stdout: true,
        echo_openjd_directives: true,
        control_flow: false,
        output_callback: None,
        sticky_bit_policy: openjd_sessions::StickyBitPolicy::Disabled,
    };
    Session::with_config(config).unwrap()
}

/// A cross-user Session whose Job lists `CONTROL_FLOW`.
fn make_cf_session(user: Arc<PosixSessionUser>) -> Session {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let config = SessionConfig {
        limits: Default::default(),
        session_id: "cross-user-cf-test".into(),
        job_parameter_values: HashMap::new(),
        path_mapping_rules: None,
        retain_working_dir: false,
        callback: None,
        os_env_vars: None,
        session_root_directory: Some(root),
        user: Some(user),
        profile: None,
        cancel_token: None,
        debug_collect_stdout: true,
        echo_openjd_directives: true,
        control_flow: true,
        output_callback: None,
        sticky_bit_policy: openjd_sessions::StickyBitPolicy::Disabled,
    };
    Session::with_config(config).unwrap()
}

fn support_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("support")
}

// === Cross-user subprocess tests ===

/// Run `whoami` as target user — verify stdout contains the target username.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_subprocess_basic() {
    let user = require_target_user();
    let mut session = make_session(user.clone());
    let r = session
        .run_subprocess("whoami", None, None, None, true, None)
        .await
        .unwrap();
    assert_eq!(r.state, ActionState::Success);
    assert!(
        r.stdout.trim().contains(&user.user),
        "Expected '{}' in stdout: {}",
        user.user,
        r.stdout
    );
    session.cleanup();
}

/// Run long_running.sh (traps SIGTERM) with a timeout — process should be killed
/// before completing all iterations, and the trap handler should fire.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_subprocess_notify() {
    let user = require_target_user();
    let mut session = make_session(user.clone());
    let script = support_dir().join("long_running.sh");
    let r = session
        .run_subprocess(
            &script.to_string_lossy(),
            None,
            Some(Duration::from_secs(3)),
            None,
            true,
            None,
        )
        .await
        .unwrap();
    assert!(
        r.state == ActionState::Timeout || r.state == ActionState::Failed,
        "Expected Timeout or Failed, got {:?}",
        r.state
    );
    assert!(
        r.stdout.contains("Log from test 0"),
        "Should see early output"
    );
    assert!(
        !r.stdout.contains("Log from test 19"),
        "Should not complete all iterations"
    );
    session.cleanup();
}

/// Run long_running.sh with timeout — SIGKILL cannot be trapped.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_subprocess_terminate() {
    let user = require_target_user();
    let mut session = make_session(user.clone());
    let script = support_dir().join("long_running_ignore.sh");
    let r = session
        .run_subprocess(
            &script.to_string_lossy(),
            None,
            Some(Duration::from_secs(3)),
            None,
            true,
            None,
        )
        .await
        .unwrap();
    assert!(
        r.state == ActionState::Timeout || r.state == ActionState::Failed,
        "Expected Timeout or Failed, got {:?}",
        r.state
    );
    session.cleanup();
}

/// Run spawn_child.sh with timeout — parent and child should both be killed.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_subprocess_terminate_tree() {
    let user = require_target_user();
    let mut session = make_session(user.clone());
    let script = support_dir().join("spawn_child.sh");
    let r = session
        .run_subprocess(
            &script.to_string_lossy(),
            None,
            Some(Duration::from_secs(3)),
            None,
            true,
            None,
        )
        .await
        .unwrap();
    assert!(
        r.state == ActionState::Timeout || r.state == ActionState::Failed,
        "Expected Timeout or Failed, got {:?}",
        r.state
    );
    assert!(
        r.stdout.contains("Log from test 0"),
        "Should see early output"
    );
    assert!(
        !r.stdout.contains("Log from test 19"),
        "Should not complete all iterations"
    );
    session.cleanup();
}

// === Cross-user runner identity tests ===

/// Verify the subprocess runs as the target user's UID, not ours.
///
/// Note on including UIDs in assertion messages:
/// A numeric POSIX UID is not sensitive information. UIDs are public on any
/// POSIX system — visible via `id`, `ps`, `stat`, `/proc`, and `/etc/passwd`.
/// This test runs in an isolated Docker container (`#[cfg(unix)] #[ignore]`,
/// executed only by the Cross-User (Linux) CI job) against a throwaway test
/// account whose UID has no meaning outside that container.
///
/// The entire purpose of this test is to verify that the subprocess runs as
/// the expected UID, so including both the observed and expected UIDs in the
/// assertion message is essential for debugging failures. Static analyzers
/// that flag `assert_eq!` with a UID as "cleartext logging of sensitive
/// information" are producing a false positive here.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_runner_uid() {
    let user = require_target_user();
    let mut session = make_session(user.clone());
    let r = session
        .run_subprocess("id", Some(&["-u".into()]), None, None, true, None)
        .await
        .unwrap();
    assert_eq!(r.state, ActionState::Success);
    let our_uid = nix::unistd::geteuid().as_raw().to_string();
    let output_uid = r.stdout.trim();
    assert_ne!(output_uid, our_uid, "Should run as different user");
    let target_uid = nix::unistd::User::from_name(&user.user)
        .unwrap()
        .unwrap()
        .uid
        .as_raw()
        .to_string();
    assert_eq!(
        r.stdout.trim(),
        target_uid,
        "Expected UID {} in output: {}",
        target_uid,
        r.stdout
    );
    session.cleanup();
}

/// Verify env vars are propagated to the cross-user subprocess.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_runner_env_vars() {
    let user = require_target_user();
    let mut session = make_session(user.clone());
    let mut env = HashMap::new();
    env.insert("OPENJD_TEST_VAR".into(), "test_value_123".into());
    let r = session
        .run_subprocess("env", None, None, Some(&env), true, None)
        .await
        .unwrap();
    assert_eq!(r.state, ActionState::Success);
    assert!(
        r.stdout.contains("OPENJD_TEST_VAR=test_value_123"),
        "Env var not found in: {}",
        r.stdout
    );
    session.cleanup();
}

/// Verify host-process env vars do NOT leak into the cross-user subprocess.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_no_env_inheritance() {
    let unique_var = format!("OPENJD_UNIQUE_{}", std::process::id());
    // SAFETY: single-threaded test setup before session creation
    unsafe { std::env::set_var(&unique_var, "should_not_appear") };
    let user = require_target_user();
    let mut session = make_session(user.clone());
    let r = session
        .run_subprocess("env", None, None, None, true, None)
        .await
        .unwrap();
    assert_eq!(r.state, ActionState::Success);
    assert!(
        !r.stdout.contains(&unique_var),
        "Host env var leaked: {}",
        unique_var
    );
    unsafe { std::env::remove_var(&unique_var) };
    session.cleanup();
}

// === Cross-user session cleanup tests ===

/// Session cleanup deletes files owned by target user with restrictive permissions.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_session_cleanup() {
    let user = require_target_user();
    let mut session = make_session(user.clone());
    let working_dir = session.working_directory().to_path_buf();

    // Create files owned by target user with owner-only permissions
    let subdir = working_dir.join("subdir");
    let subdir_s = subdir.to_string_lossy().to_string();
    let file = working_dir.join("subdir/file.test");
    let file_s = file.to_string_lossy().to_string();
    let owner = format!("{}:{}", user.user, user.user);
    let cmds: &[&[&str]] = &[
        &["sudo", "-u", &user.user, "-i", "mkdir", &subdir_s],
        &["sudo", "-u", &user.user, "-i", "chown", &owner, &subdir_s],
        &["sudo", "-u", &user.user, "-i", "chmod", "700", &subdir_s],
        &["sudo", "-u", &user.user, "-i", "touch", &file_s],
        &["sudo", "-u", &user.user, "-i", "chown", &owner, &file_s],
        &["sudo", "-u", &user.user, "-i", "chmod", "600", &file_s],
    ];
    for cmd in cmds {
        let status = std::process::Command::new(cmd[0])
            .args(&cmd[1..])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "Setup command failed: {:?}", cmd);
    }

    session.cleanup();
    assert!(!working_dir.exists(), "Working directory should be deleted");
}

// === Cross-user Session::run_subprocess identity test ===

/// Verify Session::run_subprocess runs as the configured target user.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_session_run_subprocess() {
    let user = require_target_user();
    let mut session = make_session(user.clone());
    let r = session
        .run_subprocess("whoami", None, None, None, true, None)
        .await
        .unwrap();
    assert_eq!(r.state, ActionState::Success);
    assert!(r.stdout.trim().contains(&user.user));
    session.cleanup();
}

// === Cross-user Session cancel-handle test ===

/// Repeatedly attempt to deliver a cancel through `handle` until it finds a
/// running action, starting after a short delay so the cancel lands
/// mid-action in the healthy case. Resolves to whether delivery succeeded.
///
/// A single fixed-delay cancel is racy in both directions: a slowly starting
/// subprocess may not be registered yet (the cancel finds nothing), and a
/// subprocess that exits early ends the run before the delay elapses,
/// leaving the cancel result unrecorded when the assertions execute.
fn spawn_cancel_with_retry(
    handle: SessionCancelHandle,
    mark_action_failed: bool,
) -> tokio::task::JoinHandle<bool> {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if handle.cancel(None, mark_action_failed) {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
}

/// A `SessionCancelHandle` must be able to cancel a subprocess that runs via
/// the cross-user helper: the helper path registers per-action cancel state,
/// and the handle delivers the cancel command over the helper pipe. This is
/// the cross-user counterpart of the same-user handle tests in
/// `test_session.rs` (which cannot exercise helper routing).
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_session_cancel_handle_cancels_helper_subprocess() {
    let user = require_target_user();
    let mut session = make_session(user);
    let canceller = spawn_cancel_with_retry(session.cancel_handle(), false);

    let started = std::time::Instant::now();
    let result = session
        .run_subprocess("sleep", Some(&["60".to_string()]), None, None, true, None)
        .await;
    let elapsed = started.elapsed();

    let delivered = canceller.await.expect("canceller task must not panic");
    match result {
        // With the cancel-request channel wired into the helper config, the
        // exit must classify as Canceled rather than Failed.
        Ok(r) => assert_eq!(
            r.state,
            ActionState::Canceled,
            "canceled helper subprocess must classify as Canceled; exit_code: {:?}, stdout: {}",
            r.exit_code,
            r.stdout
        ),
        // A canceled helper subprocess may surface as an error result — but
        // only when the cancel was actually delivered. Otherwise the
        // subprocess failed on its own: fail with the underlying error.
        Err(e) => assert!(
            delivered,
            "subprocess failed before any cancel was delivered: {e}"
        ),
    }
    assert!(
        delivered,
        "handle must find the in-flight helper action and deliver the cancel"
    );
    assert!(
        elapsed < Duration::from_secs(30),
        "cancel must interrupt the 60s sleep, took {elapsed:?}"
    );
    session.cleanup();
}

/// `mark_action_failed=true` must convert a canceled helper subprocess to
/// Failed, matching drive_action's conversion and the
/// `SessionCancelHandle::cancel` contract.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_session_cancel_handle_mark_failed_helper_subprocess() {
    let user = require_target_user();
    let mut session = make_session(user);
    let canceller = spawn_cancel_with_retry(session.cancel_handle(), true);

    let result = session
        .run_subprocess("sleep", Some(&["60".to_string()]), None, None, true, None)
        .await;

    let delivered = canceller.await.expect("canceller task must not panic");
    match result {
        Ok(r) => assert_eq!(
            r.state,
            ActionState::Failed,
            "mark_action_failed must report the canceled helper subprocess as Failed; exit_code: {:?}, stdout: {}",
            r.exit_code,
            r.stdout
        ),
        // Tolerated only when the cancel was actually delivered — otherwise
        // the subprocess failed on its own: fail with the underlying error.
        Err(e) => assert!(
            delivered,
            "subprocess failed before any cancel was delivered: {e}"
        ),
    }
    // Without this, a subprocess that fails on its own also yields Failed
    // and the test passes without exercising cancellation at all.
    assert!(
        delivered,
        "cancel must be delivered for this test to exercise mark_action_failed"
    );
    session.cleanup();
}

// === Cross-user TempDir tests ===

/// TempDir with target user has correct group ownership and mode 0o770.
#[tokio::test]
#[ignore]
async fn test_cross_user_tempdir_permissions() {
    use std::os::unix::fs::MetadataExt;
    let user = require_target_user();
    let td = TempDir::new(None, None, Some(&*user)).unwrap();
    let meta = std::fs::metadata(td.path()).unwrap();
    let expected_gid = nix::unistd::Group::from_name(&user.group)
        .unwrap()
        .unwrap()
        .gid
        .as_raw();
    assert_eq!(meta.gid(), expected_gid, "Group should be {}", user.group);
    assert_eq!(meta.mode() & 0o777, 0o770, "Mode should be 0o770");
    assert_eq!(
        meta.uid(),
        nix::unistd::geteuid().as_raw(),
        "Owner should be us"
    );
}

/// TempDir cleanup works when files are owned by the target user.
#[tokio::test]
#[ignore]
async fn test_cross_user_tempdir_cleanup() {
    let user = require_target_user();
    let mut td = TempDir::new(None, None, Some(&*user)).unwrap();
    let testfile = td.path().join("testfile.txt");
    let status = std::process::Command::new("sudo")
        .args(["-u", &user.user, "-i", "touch", &testfile.to_string_lossy()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(testfile.exists());
    td.cleanup().unwrap();
    assert!(!testfile.exists());
    assert!(!td.path().exists());
}

/// TempDir with disjoint user (not in shared group) — chown should fail.
#[tokio::test]
#[ignore]
async fn test_cross_user_tempdir_disjoint_fails() {
    let user = require_disjoint_user();
    let result = TempDir::new(None, None, Some(&*user));
    // Python raises RuntimeError. In Rust, chown failure is currently silent
    // (let _ = nix::unistd::chown...), so the dir may be created but with wrong group.
    if let Ok(td) = result {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(td.path()).unwrap();
        let disjoint_gid = nix::unistd::Group::from_name(&user.group)
            .unwrap()
            .unwrap()
            .gid
            .as_raw();
        assert_ne!(
            meta.gid(),
            disjoint_gid,
            "Should not be able to chown to disjoint group"
        );
    }
    // Err is also acceptable — means the crate properly rejects it
}

// === Cross-user embedded files permission tests ===
// Mirrors Python TestMaterializeFilePosix::test_changes_owner and test_changes_owner_runnable.

/// Embedded file with cross-user: group is changed, mode is 0o660 (non-runnable).
#[tokio::test]
#[ignore]
async fn test_cross_user_embedded_file_permissions() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let user = require_target_user();
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("testfile.txt");

    openjd_sessions::embedded_files::write_embedded_file_with_options(
        &path,
        "some text data",
        false,
        None,
    )
    .unwrap();
    openjd_sessions::embedded_files::chown_for_user(&path, &*user, false).unwrap();

    let meta = std::fs::metadata(&path).unwrap();
    let expected_gid = nix::unistd::Group::from_name(&user.group)
        .unwrap()
        .unwrap()
        .gid
        .as_raw();
    assert_eq!(
        meta.uid(),
        nix::unistd::geteuid().as_raw(),
        "File owner is this process's owner"
    );
    assert_eq!(meta.gid(), expected_gid, "File group is the user's group");
    let mode = meta.permissions().mode();
    assert_eq!(mode & 0o700, 0o600, "Owner has r/w");
    assert_eq!(mode & 0o070, 0o060, "Group has r/w");
    assert_eq!(mode & 0o007, 0, "Others have no permissions");
}

/// Embedded file with cross-user + runnable: group is changed, mode is 0o770.
#[tokio::test]
#[ignore]
async fn test_cross_user_embedded_file_runnable_permissions() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let user = require_target_user();
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("testfile.sh");

    openjd_sessions::embedded_files::write_embedded_file_with_options(
        &path,
        "#!/bin/bash",
        true,
        None,
    )
    .unwrap();
    openjd_sessions::embedded_files::chown_for_user(&path, &*user, true).unwrap();

    let meta = std::fs::metadata(&path).unwrap();
    let expected_gid = nix::unistd::Group::from_name(&user.group)
        .unwrap()
        .unwrap()
        .gid
        .as_raw();
    assert_eq!(
        meta.uid(),
        nix::unistd::geteuid().as_raw(),
        "File owner is this process's owner"
    );
    assert_eq!(meta.gid(), expected_gid, "File group is the user's group");
    let mode = meta.permissions().mode();
    assert_eq!(mode & 0o700, 0o700, "Owner has r/w/x");
    assert_eq!(mode & 0o070, 0o070, "Group has r/w/x");
    assert_eq!(mode & 0o007, 0, "Others have no permissions");
}

// === Cross-user helper stdout framing regression tests ===
//
// Exercise the bounded, non-blocking stdout reader in `helper/src/runner.rs`.
// `#[ignore]`; need the Docker + sudo fixtures like `test_cross_user_subprocess_basic`.

/// Invalid UTF-8 (0x97) between valid lines must not abort the run: it streams
/// on, backslash-escaped. Pre-fix `read_line` returned InvalidData.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_invalid_utf8_output_continues() {
    let user = require_target_user();
    let mut session = make_session(user);
    // \227 is octal for 0x97 (POSIX-portable printf escape).
    let args = [
        "-c".to_string(),
        "printf 'before\\n\\227\\nafter\\n'".to_string(),
    ];
    let r = session
        .run_subprocess("sh", Some(&args), None, None, true, None)
        .await
        .unwrap();
    assert_eq!(r.state, ActionState::Success, "stdout: {}", r.stdout);
    assert!(r.stdout.contains("before"), "stdout: {}", r.stdout);
    assert!(
        r.stdout.contains(r"\x97"),
        "escaped byte missing: {}",
        r.stdout
    );
    assert!(r.stdout.contains("after"), "stdout: {}", r.stdout);
    session.cleanup();
}

/// Per-line memory stays bounded: a 1 MiB newline-less line yields one
/// truncated Out (<= 64 KiB) and "END" still streams. Pre-fix `read_line`
/// buffered the whole line and overflowed the response limit.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_bounded_single_out_long_line() {
    let user = require_target_user();
    let mut session = make_session(user);
    let args = [
        "-c".to_string(),
        "head -c 1048576 /dev/zero | tr '\\0' 'a'; printf '\\nEND\\n'".to_string(),
    ];
    let r = session
        .run_subprocess("sh", Some(&args), None, None, true, None)
        .await
        .unwrap();
    assert_eq!(
        r.state,
        ActionState::Success,
        "stdout len: {}",
        r.stdout.len()
    );
    // The unbounded 'a' run must be truncated to exactly the 64 KiB prefix (one Out).
    let a_count = r.stdout.chars().filter(|&c| c == 'a').count();
    assert_eq!(
        a_count,
        64 * 1024,
        "long line must preserve exactly the bounded 64 KiB prefix, got {a_count}"
    );
    assert!(r.stdout.contains("END"), "trailing line missing");
    session.cleanup();
}

/// Stress the bounded reader with 10 MiB on one newline-less line, written in
/// 10240 separate 1 KiB writes so the reader wakes thousands of times mid-line.
/// Still exactly one truncated Out (the 64 KiB prefix) and "END" still streams:
/// the discard path holds across many `fill_buf` wakeups without stalling the
/// pipe. Pre-fix `read_line` buffered all 10 MiB and overflowed the response
/// limit.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_bounded_single_out_10mib_repeated_flushes() {
    let user = require_target_user();
    let mut session = make_session(user);
    // One 1 KiB chunk emitted 10240 times = 10 MiB, each `printf` a separate
    // write(2) with no newline, so every wakeup lands mid-line.
    let args = [
        "-c".to_string(),
        "chunk=$(head -c 1024 /dev/zero | tr '\\0' 'a'); i=0; while [ $i -lt 10240 ]; do printf '%s' \"$chunk\"; i=$((i+1)); done; printf '\\nEND\\n'".to_string(),
    ];
    let r = session
        .run_subprocess("sh", Some(&args), None, None, true, None)
        .await
        .unwrap();
    assert_eq!(
        r.state,
        ActionState::Success,
        "stdout len: {}",
        r.stdout.len()
    );
    // Exactly the 64 KiB prefix survives. More would mean the cap leaked; a
    // multiple would mean the line was emitted more than once.
    let a_count = r.stdout.chars().filter(|&c| c == 'a').count();
    assert_eq!(
        a_count,
        64 * 1024,
        "10 MiB line must yield exactly one bounded 64 KiB prefix, got {a_count}"
    );
    assert!(r.stdout.contains("END"), "trailing line missing");
    session.cleanup();
}

/// Cancel stays responsive during a partial line: a newline-less line then
/// sleep must still cancel promptly. Pre-fix `read_line` blocked on the
/// missing newline until the sleep ended.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_cancel_during_partial_line() {
    let user = require_target_user();
    let mut session = make_session(user);
    let canceller = spawn_cancel_with_retry(session.cancel_handle(), false);

    let args = ["-c".to_string(), "printf x; sleep 300".to_string()];
    let started = std::time::Instant::now();
    let result = session
        .run_subprocess("sh", Some(&args), None, None, true, None)
        .await
        .expect("cross-user subprocess must return a cancellation result");
    let elapsed = started.elapsed();

    let delivered = canceller.await.expect("canceller task must not panic");
    assert_eq!(
        result.state,
        ActionState::Canceled,
        "cancel must end the action as Canceled; exit_code: {:?}, stdout: {}",
        result.exit_code,
        result.stdout
    );
    assert!(
        delivered,
        "handle must find the in-flight helper action and deliver the cancel"
    );
    assert!(
        elapsed < Duration::from_secs(30),
        "cancel must interrupt the partial-line sleep, took {elapsed:?}"
    );
    session.cleanup();
}

/// JSON-expansion guard end to end: 100 KiB of 0x01 expands 6x (`\u0001`) to
/// ~600 KiB, past the 128 KiB limit. Must complete with a truncated Out, not a
/// per-line HelperCommunication error.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_control_char_line_within_response_limit() {
    let user = require_target_user();
    let mut session = make_session(user);
    let args = [
        "-c".to_string(),
        "head -c 102400 /dev/zero | tr '\\0' '\\001'; printf '\\n'".to_string(),
    ];
    let r = session
        .run_subprocess("sh", Some(&args), None, None, true, None)
        .await
        .unwrap();
    assert_eq!(
        r.state,
        ActionState::Success,
        "control-char line must not trip the response limit; stdout len: {}",
        r.stdout.len()
    );
    let control_count = r.stdout.chars().filter(|&c| c == '\u{1}').count();
    assert!(
        control_count > 0,
        "bounded control-character output must be delivered, not silently dropped"
    );
    assert!(
        control_count <= 64 * 1024,
        "control-character output must remain bounded"
    );
    session.cleanup();
}

/// EOF flush: a trailing newline-less line is still delivered on exit.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_eof_partial_line_preserved() {
    let user = require_target_user();
    let mut session = make_session(user);
    let args = ["-c".to_string(), "printf 'tail-no-newline'".to_string()];
    let r = session
        .run_subprocess("sh", Some(&args), None, None, true, None)
        .await
        .unwrap();
    assert_eq!(r.state, ActionState::Success, "stdout: {}", r.stdout);
    assert!(
        r.stdout.contains("tail-no-newline"),
        "EOF partial line dropped: {}",
        r.stdout
    );
    session.cleanup();
}

// === CONTROL_FLOW: openjd_output_var through the helper ===
//
// The helper resolves `@` directives as the job user (RFC 0010 §File values
// item 3) and frames lines with truncation and lossy-decode flags. These run
// the same directives as the same-user tests in `test_control_flow.rs`.

fn sh_step(script: &str) -> openjd_model::job::StepScript {
    let fs = |s: &str| openjd_expr::format_string::FormatString::new(s).unwrap();
    openjd_model::job::StepScript {
        let_bindings: None,
        actions: openjd_model::job::StepActions {
            on_run: openjd_model::job::Action {
                command: fs("sh"),
                args: Some(vec![fs("-c"), fs(script)]),
                timeout: None,
                cancelation: None,
            },
        },
        embedded_files: None,
    }
}

async fn run_cf(
    session: &mut Session,
    script: &str,
    declared: &str,
    task: Option<&openjd_model::types::TaskParameterSet>,
) -> openjd_sessions::action::ActionResult {
    let declared: Vec<openjd_model::job::OutputVariable> = serde_json::from_str(declared).unwrap();
    session
        .run_task(
            "S",
            &sh_step(script),
            task,
            None,
            None,
            &openjd_sessions::session::RunTaskOptions {
                output_variables: Some(&declared),
                path_inputs: &[],
            },
        )
        .await
        .unwrap()
}

fn fail_message(session: &Session) -> String {
    session
        .action_status()
        .and_then(|s| s.fail_message)
        .unwrap_or_default()
}

fn single(r: &openjd_sessions::action::ActionResult, name: &str) -> Option<openjd_expr::ExprValue> {
    r.outputs.as_ref()?.get(&None)?.get(name).cloned()
}

/// An `@` file larger than the helper's 128 KiB response line, and under
/// the 1 MiB limit, arrives intact; the directive forms with whitespace give
/// the same values as on the direct path.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_output_var_at_files() {
    let user = require_target_user();
    let mut session = make_cf_session(user);
    let r = run_cf(
        &mut session,
        "printf '\"%0600000d\"' 0 > big.json
         echo 'openjd_output_var: Big=@big.json'
         printf 1 > a; printf 2 > b; printf 3 > c; printf 4 > d
         echo 'openjd_output_var: A= @a'
         printf 'openjd_output_var:\tB=@b  \t\n'
         echo 'openjd_output_var:C=@c'
         echo 'openjd_output_var: D=1'",
        r#"[{"name": "Big", "type": "STRING"}, {"name": "A", "type": "INT"},
            {"name": "B", "type": "INT"}, {"name": "C", "type": "INT"},
            {"name": "D", "type": "INT"}]"#,
        None,
    )
    .await;
    assert_eq!(r.state, ActionState::Success, "{}", fail_message(&session));
    assert_eq!(
        single(&r, "Big"),
        Some(openjd_expr::ExprValue::String("0".repeat(600_000)))
    );
    for (n, v) in [("A", 1), ("B", 2), ("C", 3), ("D", 1)] {
        assert_eq!(single(&r, n), Some(openjd_expr::ExprValue::Int(v)), "{n}");
    }
    session.cleanup();
}

/// A chunked Step's selector reaches the parent through the helper.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_output_var_chunk_selector() {
    let user = require_target_user();
    let mut session = make_cf_session(user);
    let mut task = openjd_model::types::TaskParameterSet::new();
    task.insert(
        "F".into(),
        openjd_model::types::TaskParameterValue {
            param_type: openjd_model::types::TaskParameterType::ChunkInt,
            value: openjd_expr::ExprValue::String("3".into()),
        },
    );
    let r = run_cf(
        &mut session,
        "printf 7 > f; echo 'openjd_output_var: N[3]=@f'",
        r#"[{"name": "N", "type": "INT"}]"#,
        Some(&task),
    )
    .await;
    assert_eq!(r.state, ActionState::Success, "{}", fail_message(&session));
    assert_eq!(
        r.outputs.unwrap()[&Some(3)]["N"],
        openjd_expr::ExprValue::Int(7)
    );
    session.cleanup();
}

/// Each failure through the helper fails the Task with the same message as
/// on the direct path.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_output_var_failures() {
    let user = require_target_user();
    for (script, message) in [
        (
            "echo 'openjd_output_var: M=@missing'",
            "Output variable 'M': the Step does not declare it (declared: 'N'), in \
             'openjd_output_var: M=@missing'"
                .to_string(),
        ),
        (
            "echo 'openjd_output_var: N=@missing'",
            "Output variable 'N': the @ file 'missing' does not exist".to_string(),
        ),
        (
            "truncate -s 2M big; echo 'openjd_output_var: N=@big'",
            "Output variable 'N': the @ file 'big' is larger than the 1048576 bytes the Task's \
             output values have left (the limit is 1048576 bytes per Task)"
                .to_string(),
        ),
        (
            "mkfifo p; echo 'openjd_output_var: N=@p'",
            "Output variable 'N': the @ file 'p' is not a regular file".to_string(),
        ),
        (
            "printf '1\\377' > z; echo 'openjd_output_var: N=@z'",
            "Output variable 'N': the @ file's content is not valid UTF-8 (at byte 1), in \
             'openjd_output_var: N=@z'"
                .to_string(),
        ),
        (
            "printf 'openjd_output_var: N=1\\377\\n'",
            r"Invalid openjd_output_var directive: the line is not valid UTF-8, in 'openjd_output_var: N=1\xff'"
                .to_string(),
        ),
        (
            "printf 'openjd_output_var: N=@f%070000d\\n' 0",
            format!(
                "Invalid openjd_output_var directive: the line is longer than the 65536 byte \
                 line limit, so it was cut; write a large value to a file and report it with \
                 @<file>, in '{}...'",
                &format!("openjd_output_var: N=@f{}", "0".repeat(70_000))[..200]
            ),
        ),
    ] {
        let mut session = make_cf_session(user.clone());
        let r = run_cf(&mut session, script, r#"[{"name": "N", "type": "INT"}]"#, None).await;
        assert_eq!(r.state, ActionState::Failed, "{script}");
        assert!(r.outputs.is_none());
        assert_eq!(fail_message(&session), message, "{script}");
        session.cleanup();
    }
}

/// A form feed after the value is not directive whitespace on either path:
/// the Unix helper strips only the line terminator.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_output_var_form_feed() {
    let user = require_target_user();
    let mut session = make_cf_session(user);
    let r = run_cf(
        &mut session,
        "printf 'openjd_output_var: N=1\\f\\n'",
        r#"[{"name": "N", "type": "INT"}]"#,
        None,
    )
    .await;
    assert_eq!(r.state, ActionState::Failed);
    assert_eq!(
        fail_message(&session),
        "Output variable 'N': '1\\u{c}' is not one valid JSON value: trailing characters at \
         line 1 column 2"
    );
    session.cleanup();
}

/// A file the job user can't read, hard-linked into the working directory
/// by another user, fails the Task: the file is read as the job user, never
/// by the Session's own user (RFC 0010 §File values item 3).
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_output_var_unreadable_hard_link() {
    use std::os::unix::fs::PermissionsExt;
    let user = require_target_user();
    let mut session = make_cf_session(user);
    let secret_dir = tempfile::TempDir::new().unwrap();
    let secret = secret_dir.path().join("secret");
    std::fs::write(&secret, "42").unwrap();
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::hard_link(&secret, session.working_directory().join("link")).unwrap();
    let r = run_cf(
        &mut session,
        "echo 'openjd_output_var: N=@link'",
        r#"[{"name": "N", "type": "INT"}]"#,
        None,
    )
    .await;
    assert_eq!(r.state, ActionState::Failed);
    assert_eq!(
        fail_message(&session),
        "Output variable 'N': the @ file 'link' can't be opened: Permission denied (os error 13)"
    );
    session.cleanup();
}

/// Environment actions: a Job Template Environment's directive cancels and
/// fails the action; an Environment Template's is ordinary output, and the
/// helper is not asked to read its file.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_output_var_environment_actions() {
    use openjd_sessions::session::{EnvironmentOptions, EnvironmentOrigin};
    let user = require_target_user();
    let fs = |s: &str| openjd_expr::format_string::FormatString::new(s).unwrap();
    let env = openjd_model::job::Environment {
        name: "E".into(),
        description: None,
        script: Some(openjd_model::job::EnvironmentScript {
            let_bindings: None,
            actions: openjd_model::job::EnvironmentActions {
                on_enter: Some(openjd_model::job::Action {
                    command: fs("sh"),
                    args: Some(vec![
                        fs("-c"),
                        fs("printf 1 > f; echo 'openjd_output_var: N=@f'"),
                    ]),
                    timeout: None,
                    cancelation: None,
                }),
                on_wrap_env_enter: None,
                on_wrap_task_run: None,
                on_wrap_env_exit: None,
                on_exit: None,
            },
            embedded_files: None,
        }),
        variables: None,
        resolved_symtab: None,
        enabled: true,
    };
    let mut session = make_cf_session(user.clone());
    let err = session
        .enter_environment(&env, None, None, None, &EnvironmentOptions::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("onEnter"), "{err}");
    assert_eq!(
        fail_message(&session),
        "Invalid openjd_output_var directive: this Action can't report output values; only \
         the onRun action of a Step that declares outputVariables can, in \
         'openjd_output_var: N=@f'"
    );
    session.cleanup();

    let mut session = make_cf_session(user);
    let (_, out) = session
        .enter_environment_with_output(
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
    assert!(out.contains("openjd_output_var: N=@f"), "{out}");
    session.cleanup();
}

/// Directive lines of up to 64 KiB are accepted through the helper as on
/// the direct path (RFC 0010 §Limits item 1): with a CRLF line end at the
/// cap, and when JSON escaping makes the line too long for one response.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_output_var_long_lines() {
    let user = require_target_user();
    let mut session = make_cf_session(user);
    let r = run_cf(
        &mut session,
        r#"printf 'openjd_output_var: A="%s"\r\n' "$(head -c 65513 /dev/zero | tr '\0' a)"
           printf 'openjd_output_var: B="'; head -c 65512 /dev/zero | tr '\0' '\\'; printf '"\n'"#,
        r#"[{"name": "A", "type": "STRING"}, {"name": "B", "type": "STRING"}]"#,
        None,
    )
    .await;
    assert_eq!(r.state, ActionState::Success, "{}", fail_message(&session));
    assert_eq!(
        single(&r, "A"),
        Some(openjd_expr::ExprValue::String("a".repeat(65_513)))
    );
    assert_eq!(
        single(&r, "B"),
        Some(openjd_expr::ExprValue::String("\\".repeat(32_756)))
    );
    session.cleanup();
}

/// A directive line over the 64 KiB cap that JSON escaping makes too long
/// for one response travels as `outLong` with its `truncated` flag, and
/// fails as on the direct path.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_output_var_long_cut_line() {
    let user = require_target_user();
    let mut session = make_cf_session(user);
    let r = run_cf(
        &mut session,
        r#"printf 'openjd_output_var: S='; head -c 65520 /dev/zero | tr '\0' '\\'; printf '\n'"#,
        r#"[{"name": "S", "type": "STRING"}]"#,
        None,
    )
    .await;
    assert_eq!(r.state, ActionState::Failed);
    assert_eq!(
        fail_message(&session),
        format!(
            "Invalid openjd_output_var directive: the line is longer than the 65536 byte line \
             limit, so it was cut; write a large value to a file and report it with @<file>, in \
             'openjd_output_var: S={}...'",
            "\\".repeat(179)
        )
    );
    session.cleanup();
}

/// After an invalid directive, a later `@` line is ordinary output, as on
/// the direct path, though the helper still resolves it.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_output_var_at_line_after_an_invalid_directive() {
    let user = require_target_user();
    let mut session = make_cf_session(user);
    let r = run_cf(
        &mut session,
        "printf 1 > f; echo 'openjd_output_var: N=x'; echo 'openjd_output_var: N=@f'; sleep 5",
        r#"[{"name": "N", "type": "INT"}]"#,
        None,
    )
    .await;
    assert_eq!(r.state, ActionState::Failed);
    assert_eq!(
        fail_message(&session),
        "Output variable 'N': 'x' is not one valid JSON value: expected value at line 1 column 1"
    );
    assert!(
        r.stdout.lines().any(|l| l == "openjd_output_var: N=@f"),
        "{}",
        r.stdout
    );
    session.cleanup();
}

/// A cancel that lands as soon as the action is armed, usually before the
/// helper has read the Run command (which the idle helper would drop), still
/// cancels the run instead of letting it finish.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_cross_user_early_cancel_cancels_the_run() {
    let user = require_target_user();
    let mut session = make_cf_session(user);
    let handle = session.cancel_handle();
    let canceller = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if handle.cancel(None, false) {
                return true;
            }
            std::hint::spin_loop();
        }
        false
    });
    let started = std::time::Instant::now();
    let r = run_cf(
        &mut session,
        "sleep 30",
        r#"[{"name": "N", "type": "INT"}]"#,
        None,
    )
    .await;
    let elapsed = started.elapsed();
    assert!(canceller.join().unwrap());
    assert_eq!(r.state, ActionState::Canceled);
    // At most the 5 second notify period of a helper-pipe cancel.
    assert!(elapsed < Duration::from_secs(10), "took {elapsed:?}");
    session.cleanup();
}
