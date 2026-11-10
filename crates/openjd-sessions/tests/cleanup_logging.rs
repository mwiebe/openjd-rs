// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! The structured fields of the records cleanup logs.
//!
//! `openjd run` prints a session record when its `openjd_log_content` has
//! `EXCEPTION_INFO` and its level is `Warn` or more severe, and the worker
//! agent routes it by the same field and `session_id`. `testing_logger`
//! captures only the level and text, so this binary installs its own logger,
//! which also keeps the key-values. It is a test binary of its own, not a
//! module of `tests/integration.rs`, because a process has one global logger
//! and the `integration` binary's tests install `testing_logger`.

use std::sync::Mutex;

use log::kv::{Key, Value, VisitSource};
use openjd_sessions::{Session, SessionConfig};

struct Captured {
    level: log::Level,
    target: String,
    body: String,
    content: Option<u64>,
    session_id: Option<String>,
}

struct CapturingLogger;

static RECORDS: Mutex<Vec<Captured>> = Mutex::new(Vec::new());
static LOGGER: CapturingLogger = CapturingLogger;

impl log::Log for CapturingLogger {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        struct Fields {
            content: Option<u64>,
            session_id: Option<String>,
        }
        impl<'kvs> VisitSource<'kvs> for Fields {
            fn visit_pair(
                &mut self,
                key: Key<'kvs>,
                value: Value<'kvs>,
            ) -> Result<(), log::kv::Error> {
                match key.as_str() {
                    "openjd_log_content" => self.content = value.to_u64(),
                    "session_id" => self.session_id = Some(value.to_string()),
                    _ => {}
                }
                Ok(())
            }
        }
        let mut fields = Fields {
            content: None,
            session_id: None,
        };
        let _ = record.key_values().visit(&mut fields);
        RECORDS.lock().unwrap().push(Captured {
            level: record.level(),
            target: record.target().to_string(),
            body: record.args().to_string(),
            content: fields.content,
            session_id: fields.session_id,
        });
    }

    fn flush(&self) {}
}

fn install_logger() {
    // Every test calls this; only the first call installs the logger.
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(log::LevelFilter::Trace);
}

/// A warning or error's level, `openjd_log_content`, `session_id` and text.
type Problem = (log::Level, Option<u64>, Option<String>, String);

/// The warnings and errors whose text mentions `dir`. Tests run in parallel,
/// so each picks out its own records by its directory. Each must have the
/// `openjd.sessions` target, which log consumers filter on.
fn problems_about(dir: &std::path::Path) -> Vec<Problem> {
    let dir = dir.display().to_string();
    let matched: Vec<(String, Problem)> = RECORDS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|r| r.level <= log::Level::Warn && r.body.contains(&dir))
        .map(|r| {
            let fields = (r.level, r.content, r.session_id.clone(), r.body.clone());
            (r.target.clone(), fields)
        })
        .collect();
    for (target, (_, _, _, body)) in &matched {
        assert_eq!(target, "openjd.sessions", "{body}");
    }
    matched.into_iter().map(|(_, fields)| fields).collect()
}

fn config(root: &std::path::Path, session_id: &str) -> SessionConfig {
    SessionConfig {
        limits: Default::default(),
        session_id: session_id.into(),
        job_parameter_values: Default::default(),
        session_root_directory: Some(root.to_path_buf()),
        path_mapping_rules: None,
        retain_working_dir: false,
        callback: None,
        os_env_vars: None,
        user: None,
        profile: None,
        cancel_token: None,
        debug_collect_stdout: false,
        echo_openjd_directives: true,
        sticky_bit_policy: openjd_sessions::StickyBitPolicy::Disabled,
    }
}

/// `FILE_PATH | EXCEPTION_INFO`.
const CLEANUP_ERROR_CONTENT: u64 = 0b0001_0010;
/// `FILE_PATH | EXCEPTION_INFO | PROCESS_CONTROL`.
#[cfg(windows)]
const FAILED_ATTEMPT_CONTENT: u64 = 0b0011_0010;

/// A working directory that is already gone: removal fails at once with
/// `NotFound`, which is not retried, and the error carries the session ID
/// and `EXCEPTION_INFO`.
#[test]
fn cleanup_error_carries_session_id_and_exception_info() {
    install_logger();
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::with_config(config(root.path(), "session-gone")).unwrap();
    let wd = session.working_directory().to_path_buf();
    std::fs::remove_dir_all(&wd).unwrap();
    session.cleanup();
    let problems = problems_about(&wd);
    assert_eq!(problems.len(), 1, "{:?}", problems);
    let (level, content, session_id, body) = &problems[0];
    assert_eq!(*level, log::Level::Error);
    assert_eq!(*content, Some(CLEANUP_ERROR_CONTENT));
    assert_eq!(session_id.as_deref(), Some("session-gone"));
    let start = format!(
        "Files within temporary directory {} could not be deleted: ",
        wd.display()
    );
    assert!(body.starts_with(&start), "{body}");
}

#[cfg(windows)]
fn hold_briefly(path: &std::path::Path) -> std::thread::JoinHandle<()> {
    use std::os::windows::fs::OpenOptionsExt;
    let held = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(path)
        .unwrap();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(500));
        drop(held);
    })
}

/// A file held briefly in the embedded files directory: the one failed
/// attempt is warned about against the session, with `EXCEPTION_INFO`.
#[cfg(windows)]
#[test]
fn session_cleanup_warning_carries_session_id_and_exception_info() {
    install_logger();
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::with_config(config(root.path(), "session-held")).unwrap();
    let wd = session.working_directory().to_path_buf();
    let release = hold_briefly(&session.files_directory().join("locked.bin"));
    session.cleanup();
    release.join().unwrap();
    assert!(!wd.exists());
    let fields: Vec<_> = problems_about(&wd)
        .into_iter()
        .map(|(level, content, session_id, _)| (level, content, session_id))
        .collect();
    assert_eq!(
        fields,
        [(
            log::Level::Warn,
            Some(FAILED_ATTEMPT_CONTENT),
            Some("session-held".to_string())
        )]
    );
}

/// `TempDir::cleanup` outside a session logs the same warning with no
/// session ID.
#[cfg(windows)]
#[test]
fn tempdir_cleanup_warning_carries_exception_info() {
    install_logger();
    let root = tempfile::tempdir().unwrap();
    let mut td = openjd_sessions::tempdir::TempDir::new(Some(root.path()), None, None).unwrap();
    let release = hold_briefly(&td.path().join("locked.bin"));
    td.cleanup().unwrap();
    release.join().unwrap();
    let fields: Vec<_> = problems_about(td.path())
        .into_iter()
        .map(|(level, content, session_id, _)| (level, content, session_id))
        .collect();
    assert_eq!(
        fields,
        [(log::Level::Warn, Some(FAILED_ATTEMPT_CONTENT), None)]
    );
}
