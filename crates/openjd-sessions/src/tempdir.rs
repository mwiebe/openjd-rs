// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Secure temporary directory creation — mirrors Python `_tempdir.py`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::SessionError;
use crate::session_user::SessionUser;

/// Controls behavior when a parent directory is world-writable without the sticky bit.
///
/// On POSIX systems, a world-writable directory without the sticky bit allows any
/// user to rename or delete files belonging to other users. This is a security risk
/// for session working directories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StickyBitPolicy {
    /// Refuse to create the session if a parent directory is unsafe.
    /// This is the default — fail-closed is the secure choice.
    #[default]
    Strict,
    /// Log a warning but allow the session to proceed.
    Warn,
    /// Skip the check entirely.
    Disabled,
}

/// Returns the OpenJD temp directory, creating it if needed.
///
/// `base_dir` is the OpenJD directory itself — the directory under which
/// individual session and file temp directories will be created.
///
/// - `None` → derive from the environment:
///   - Unix: `<std::env::temp_dir()>/OpenJD` (typically `/tmp/OpenJD`).
///   - Windows: `%PROGRAMDATA%\Amazon\OpenJD` (with a warning and fallback to
///     `C:\ProgramData\Amazon\OpenJD` if `PROGRAMDATA` is unset).
/// - `Some(p)` → use `p` directly.
///
/// Tests should pass `Some(...)` to avoid mutating process-global environment
/// variables, which races with parallel tests that read them.
pub fn openjd_temp_dir(base_dir: Option<&Path>) -> Result<PathBuf, SessionError> {
    let dir = match base_dir {
        Some(p) => p.to_path_buf(),
        None => default_openjd_dir(),
    };

    std::fs::create_dir_all(&dir).map_err(|e| SessionError::TempDir {
        path: dir.clone(),
        source: e,
    })?;
    Ok(dir)
}

/// Compute the default OpenJD directory from the environment.
///
/// Split out so the env-var inspection (and Windows warning) can be tested
/// without `openjd_temp_dir` doing filesystem work, and without tests having
/// to mutate process-global env vars.
fn default_openjd_dir() -> PathBuf {
    #[cfg(unix)]
    {
        std::env::temp_dir().join("OpenJD")
    }

    #[cfg(windows)]
    {
        openjd_dir_from_programdata(std::env::var("PROGRAMDATA").ok())
    }
}

/// Build the Windows OpenJD directory from a `PROGRAMDATA` value.
///
/// Pure function: takes an explicit value (as if from the environment),
/// performs no I/O, and is fully testable without touching real env vars.
/// Logs a warning when `programdata` is `None` and falls back to
/// `C:\ProgramData`.
#[cfg(windows)]
fn openjd_dir_from_programdata(programdata: Option<String>) -> PathBuf {
    let program_data = programdata.unwrap_or_else(|| {
        log::warn!(
            target: "openjd.sessions",
            "Environment variable \"PROGRAMDATA\" is not set. \
             Creating session working directories under C:\\ProgramData"
        );
        r"C:\ProgramData".to_string()
    });
    PathBuf::from(program_data).join("Amazon").join("OpenJD")
}

/// Check parent directories for world-writable dirs missing the sticky bit (POSIX only).
///
/// Returns the first offending path, or `None` if all parents are safe.
#[cfg(unix)]
pub fn find_missing_sticky_bit(root_dir: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;

    const S_IWOTH: u32 = 0o002;
    const S_ISVTX: u32 = 0o1000;

    for parent in root_dir.ancestors().skip(1) {
        if let Ok(meta) = std::fs::metadata(parent) {
            let mode = meta.mode();
            if (mode & S_IWOTH) != 0 && (mode & S_ISVTX) == 0 {
                return Some(parent.to_path_buf());
            }
        }
    }
    None
}

/// A securely-created temporary directory.
///
/// Call [`cleanup()`](TempDir::cleanup) to remove the directory. If dropped
/// without calling `cleanup()`, a best-effort removal is attempted.
///
/// ```
/// use openjd_sessions::TempDir;
///
/// let dir = tempfile::tempdir().unwrap();
/// let mut td = TempDir::new(Some(dir.path()), Some("test-"), None).unwrap();
/// assert!(td.path().exists());
/// td.cleanup().unwrap();
/// assert!(!td.path().exists());
/// ```
pub struct TempDir {
    path: PathBuf,
    cleaned_up: bool,
}

impl std::fmt::Debug for TempDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TempDir")
            .field("path", &self.path)
            .field("cleaned_up", &self.cleaned_up)
            .finish()
    }
}

impl AsRef<std::path::Path> for TempDir {
    fn as_ref(&self) -> &std::path::Path {
        &self.path
    }
}

impl TempDir {
    /// Create a new secure temp directory.
    ///
    /// - `dir`: parent directory (defaults to `openjd_temp_dir(None)`)
    /// - `prefix`: optional name prefix
    /// - `user`: optional session user for cross-user ownership
    pub fn new(
        dir: Option<&Path>,
        prefix: Option<&str>,
        user: Option<&dyn SessionUser>,
    ) -> Result<Self, SessionError> {
        let parent = match dir {
            Some(d) => d.to_path_buf(),
            None => openjd_temp_dir(None)?,
        };

        let prefix = prefix.unwrap_or("");
        let suffix = random_hex();
        let name = format!("{prefix}{suffix}");
        let path = parent.join(name);

        std::fs::create_dir(&path).map_err(|e| SessionError::TempDir {
            path: path.clone(),
            source: e,
        })?;

        Self::secure(path, user)
    }

    /// Create a session working directory directly under `dir`, named with
    /// [`SESSION_DIR_NAME_LENGTH`] random characters from
    /// [`SESSION_DIR_NAME_ALPHABET`].
    ///
    /// A name that already exists is replaced by a fresh one, up to
    /// [`SESSION_DIR_NAME_ATTEMPTS`] times.
    pub(crate) fn new_session_working_dir(
        dir: &Path,
        user: Option<&dyn SessionUser>,
    ) -> Result<Self, SessionError> {
        Self::new_session_working_dir_with(dir, user, random_session_dir_name)
    }

    /// [`TempDir::new_session_working_dir`] with the candidate names supplied
    /// by `next_name`, so tests can force collisions.
    fn new_session_working_dir_with(
        dir: &Path,
        user: Option<&dyn SessionUser>,
        next_name: impl FnMut() -> std::io::Result<String>,
    ) -> Result<Self, SessionError> {
        let path = create_unique_dir(dir, SESSION_DIR_NAME_ATTEMPTS, next_name)?;
        Self::secure(path, user)
    }

    /// Apply ownership and permissions to the just-created directory `path`.
    fn secure(path: PathBuf, user: Option<&dyn SessionUser>) -> Result<Self, SessionError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if let Some(u) = user.filter(|u| !u.is_process_user()) {
                // Cross-user: chown group then set 0o770
                // chown before chmod — security: don't grant group access if chown fails
                if let Ok(Some(grp)) = nix::unistd::Group::from_name(u.group()) {
                    nix::unistd::chown(&path, None, Some(grp.gid)).map_err(|e| {
                        SessionError::PathPermissions {
                            path: path.display().to_string(),
                            reason: format!(
                                "Could not change ownership (error: {e}). Please ensure that uid {} is a member of group {}.",
                                nix::unistd::geteuid(), u.group()
                            ),
                        }
                    })?;
                }
                0o770
            } else {
                0o700
            };
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).map_err(
                |e| SessionError::TempDir {
                    path: path.clone(),
                    source: e,
                },
            )?;
        }

        // Windows: set DACL — full control for process user, modify for session user.
        #[cfg(windows)]
        {
            if let Some(u) = user.filter(|u| !u.is_process_user()) {
                // Fail closed: a cross-user directory without the DACL grant is
                // unusable by the session user (and silently skipping the grant
                // previously masked the failure until a subprocess hit it).
                let process_user = crate::win32::get_process_user().map_err(|e| {
                    SessionError::PathPermissions {
                        path: path.display().to_string(),
                        reason: format!("Could not determine the process user for DACL setup: {e}"),
                    }
                })?;
                if let Err(e) = crate::win32_permissions::set_permissions(
                    &path.to_string_lossy(),
                    &[process_user.as_str()],
                    &[u.user()],
                    &[],
                ) {
                    return Err(SessionError::PathPermissions {
                        path: path.display().to_string(),
                        reason: e.to_string(),
                    });
                }
            }
        }

        Ok(Self {
            path,
            cleaned_up: false,
        })
    }

    #[allow(
        missing_docs,
        reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
    )]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Remove the directory and all contents.
    ///
    /// On Windows a failed removal is retried after 1, 2, 4, 8 and 16 s, because
    /// a process that has just exited, or been terminated, can hold its file
    /// handles for a while afterwards. Each failed attempt is logged as a
    /// warning naming the files that cannot be deleted and the processes
    /// holding them open.
    ///
    /// # Errors
    ///
    /// [`SessionError::TempDir`] with the last error when removal still fails,
    /// including `NotFound` when the directory was gone before the first attempt.
    pub fn cleanup(&mut self) -> Result<(), SessionError> {
        self.cleanup_with_delays(None, CLEANUP_RETRY_DELAYS)
    }

    /// [`Self::cleanup`], retrying a failed removal after each of `delays` and
    /// logging each failed attempt against `session_id`.
    pub(crate) fn cleanup_with_delays(
        &mut self,
        session_id: Option<&str>,
        delays: &[Duration],
    ) -> Result<(), SessionError> {
        if self.cleaned_up {
            return Ok(());
        }
        self.cleaned_up = true;
        let dir = self.path.clone();
        remove_dir_all_with_retry(
            &self.path,
            delays,
            |p| std::fs::remove_dir_all(p),
            std::thread::sleep,
            |err, retry_in| {
                if cfg!(windows) {
                    log_failed_attempt(session_id, &dir, err, retry_in);
                }
            },
        )
        .map_err(|e| SessionError::TempDir {
            path: self.path.clone(),
            source: e,
        })
    }

    /// Record the directory as removed without touching the disk. For a
    /// directory nested in one whose own cleanup removes it.
    pub(crate) fn mark_cleaned_up(&mut self) {
        self.cleaned_up = true;
    }
}

/// Delays before each retry of a failed [`TempDir::cleanup`]: about 31 s in total.
///
/// Windows refuses to delete a file or directory while any process holds a
/// handle to it. Other platforms do not retry: a failure there is a permission
/// problem that waiting will not fix.
#[cfg(windows)]
pub(crate) const CLEANUP_RETRY_DELAYS: &[Duration] = &[
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
];
/// Delays before each retry of a failed [`TempDir::cleanup`]. None off Windows.
#[cfg(not(windows))]
pub(crate) const CLEANUP_RETRY_DELAYS: &[Duration] = &[];

/// How much a failed cleanup attempt looks at to find what blocks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScanLimits {
    /// Most files checked for one that cannot be deleted. Checking 10 000
    /// files takes a few seconds on Windows.
    check: usize,
    /// Most undeletable files reported, and passed to the holder lookup.
    report: usize,
    /// Most program files that refuse writing asked about, one at a time,
    /// to find which are running images. Each query takes about 45 ms for a
    /// read-only file.
    settle: usize,
}

/// The limits every failed cleanup attempt uses.
const SCAN_LIMITS: ScanLimits = ScanLimits {
    check: 10_000,
    report: 256,
    settle: 16,
};
/// Most undeletable file names a failed-attempt log line lists.
const MAX_FILES_TO_NAME: usize = 3;

/// What opening a file says about whether deleting it would fail now.
#[cfg_attr(not(windows), allow(dead_code, reason = "only Windows checks files"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeleteCheck {
    /// Deleting it would fail.
    Refused,
    /// Nothing seen stops deleting it.
    Allowed,
    /// A program file that refuses writing, so opening it cannot tell whether
    /// it is a running image. Settled by asking which files a process has open.
    Unknown,
}

/// The content of the warning logged for each failed cleanup attempt. It
/// includes `EXCEPTION_INFO`, which `openjd run` prints and the worker agent
/// routes to its own log.
pub(crate) const FAILED_ATTEMPT_LOG_CONTENT: crate::logging::LogContent =
    crate::logging::LogContent::FILE_PATH
        .union(crate::logging::LogContent::PROCESS_CONTROL)
        .union(crate::logging::LogContent::EXCEPTION_INFO);

/// A process holding a file open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileHolder {
    pub(crate) pid: u32,
    pub(crate) name: String,
}

/// Calls `remove` on `path`, and again after each of `delays` until it succeeds.
///
/// `on_failure` is called after every failed attempt with the delay before
/// the next one, or `None` after the last. `NotFound` on the first attempt is
/// returned as is, with no `on_failure` call. On a retry it counts as success:
/// an earlier attempt that reported a failure partway through can still have
/// removed the directory.
fn remove_dir_all_with_retry(
    path: &Path,
    delays: &[Duration],
    mut remove: impl FnMut(&Path) -> std::io::Result<()>,
    mut sleep: impl FnMut(Duration),
    mut on_failure: impl FnMut(&std::io::Error, Option<Duration>),
) -> std::io::Result<()> {
    let mut result = remove(path);
    for &delay in delays {
        let Err(e) = &result else { break };
        if e.kind() == std::io::ErrorKind::NotFound {
            break;
        }
        on_failure(e, Some(delay));
        sleep(delay);
        result = match remove(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        };
    }
    if let Err(e) = &result {
        if e.kind() != std::io::ErrorKind::NotFound {
            on_failure(e, None);
        }
    }
    result
}

/// Log one failed removal of `dir`: the files that cannot be deleted, and who
/// is holding them open. Only Windows calls this; no other platform retries.
fn log_failed_attempt(
    session_id: Option<&str>,
    dir: &Path,
    err: &std::io::Error,
    retry_in: Option<Duration>,
) {
    #[cfg(windows)]
    let (check, settle) = (
        crate::win32_file_locks::check_deletion,
        crate::win32_file_locks::held_by_some_process,
    );
    #[cfg(not(windows))]
    let (check, settle) = (
        |_: &Path| DeleteCheck::Allowed,
        |_: &[PathBuf], _: usize| (Vec::new(), false),
    );
    let (stuck, truncated) = undeletable_files(dir, SCAN_LIMITS, check, settle);
    #[cfg(windows)]
    let holders = (!stuck.is_empty()).then(|| crate::win32_file_locks::processes_holding(&stuck));
    #[cfg(not(windows))]
    let holders: Option<Result<Vec<FileHolder>, String>> = None;
    let message = describe_failed_attempt(dir, err, &stuck, truncated, holders.as_ref(), retry_in);
    let content = FAILED_ATTEMPT_LOG_CONTENT;
    match session_id {
        Some(id) => crate::session_log!(warn, id, content, "{}", message),
        None => log::warn!(
            target: "openjd.sessions",
            openjd_log_content = content.bits(),
            openjd_timestamp_usec = crate::logging::timestamp_usec();
            "{}", message
        ),
    }
}

/// The regular files under `dir` that cannot be deleted, at most
/// `limits.report` of them, checking at most `limits.check` files. The flag
/// is true when there may be more: the scan or `settle` stopped early, or
/// `limits.report` was reached.
///
/// `check` looks at one file. The files it cannot decide on are passed to
/// `settle` together, once, with `limits.settle`, and it returns those that
/// cannot be deleted and whether it stopped early.
///
/// The walk is depth-first in listing order, entering each subdirectory
/// when it is listed, as `remove_dir_all` does, so the file that stopped
/// `remove_dir_all` comes first. That matters because
/// `remove_dir_all` stops at the first file it cannot delete and leaves the
/// rest in place, so the files left are not the ones to report: each is
/// checked instead. The walk only reads, and does not follow symbolic links
/// or junctions, so a link swapped in while it runs can at worst change what
/// is reported.
fn undeletable_files(
    dir: &Path,
    limits: ScanLimits,
    mut check: impl FnMut(&Path) -> DeleteCheck,
    settle: impl FnOnce(&[PathBuf], usize) -> (Vec<PathBuf>, bool),
) -> (Vec<PathBuf>, bool) {
    let mut found = Vec::new();
    let mut unknown = Vec::new();
    let mut checked = 0;
    let mut truncated = false;
    let mut open: Vec<std::fs::ReadDir> = std::fs::read_dir(dir).into_iter().collect();
    'walk: while let Some(entries) = open.last_mut() {
        let Some(entry) = entries.next() else {
            open.pop();
            continue;
        };
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if let Ok(sub) = std::fs::read_dir(entry.path()) {
                open.push(sub);
            }
        } else if kind.is_file() {
            if checked == limits.check || found.len() == limits.report {
                truncated = true;
                break 'walk;
            }
            checked += 1;
            let path = entry.path();
            match check(&path) {
                DeleteCheck::Refused => found.push(path),
                DeleteCheck::Unknown => unknown.push(path),
                DeleteCheck::Allowed => {}
            }
        }
    }
    if found.len() < limits.report && !unknown.is_empty() {
        let (held, stopped_early) = settle(&unknown, limits.settle);
        truncated |= stopped_early;
        found.extend(held);
    }
    if found.len() >= limits.report {
        truncated = true;
        found.truncate(limits.report);
    }
    (found, truncated)
}

/// `err`'s text as Python's `strerror` gives it: without std's ` (os error N)`
/// suffix, or the full stop that ends Windows system messages.
pub(crate) fn error_reason(err: &std::io::Error) -> String {
    let text = err.to_string();
    let text = match err.raw_os_error() {
        Some(code) => text
            .strip_suffix(&format!(" (os error {code})"))
            .map(str::to_string)
            .unwrap_or(text),
        None => text,
    };
    text.strip_suffix('.').map(str::to_string).unwrap_or(text)
}

/// The text of a failed-attempt log line. `stuck` are the files that cannot be
/// deleted; `holders` is `None` when no lookup was made.
fn describe_failed_attempt(
    dir: &Path,
    err: &std::io::Error,
    stuck: &[PathBuf],
    truncated: bool,
    holders: Option<&Result<Vec<FileHolder>, String>>,
    retry_in: Option<Duration>,
) -> String {
    let mut msg = format!(
        "Could not delete temporary directory {}: {}.",
        dir.display(),
        error_reason(err)
    );
    if !stuck.is_empty() {
        let names: Vec<String> = stuck
            .iter()
            .take(MAX_FILES_TO_NAME)
            .map(|p| p.strip_prefix(dir).unwrap_or(p).display().to_string())
            .collect();
        let more = if truncated { "At least " } else { "" };
        msg.push_str(&format!(
            " {more}{} file(s) could not be deleted, including {}.",
            stuck.len(),
            names.join(", ")
        ));
        msg.push_str(&describe_holders(holders));
    } else if truncated {
        msg.push_str(
            " No file that cannot be deleted was found, but not every file could be checked.",
        );
    }
    match retry_in {
        Some(d) => msg.push_str(&format!(" Retrying in {} s.", d.as_secs_f64())),
        None => msg.push_str(" Giving up."),
    }
    msg
}

/// The sentence naming who holds the remaining files. Empty when no lookup was made.
fn describe_holders(holders: Option<&Result<Vec<FileHolder>, String>>) -> String {
    match holders {
        Some(Ok(h)) if !h.is_empty() => {
            let mut seen = std::collections::HashSet::new();
            let list: Vec<String> = h
                .iter()
                .filter(|x| seen.insert(x.pid))
                .map(|x| format!("{} (pid {})", x.name, x.pid))
                .collect();
            format!(" Files are still locked open by: {}.", list.join(", "))
        }
        Some(Ok(_)) => " No running process was found holding them open.".to_string(),
        Some(Err(e)) => format!(" The processes holding them open could not be determined: {e}."),
        None => String::new(),
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if !self.cleaned_up {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

fn random_hex() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Length of a session working directory's name.
///
/// Kept short because the path counts against Windows `MAX_PATH` (260) for
/// job applications that are not long-path aware. The name carries no part
/// of the session ID; the session log records the path beside the ID.
/// Matches openjd-sessions-for-python PR #348.
pub(crate) const SESSION_DIR_NAME_LENGTH: usize = 8;

/// Characters a session working directory's name is drawn from. Lower case
/// only, so two names never differ only in case (which case-insensitive
/// filesystems treat as the same name), and no character is reserved on
/// Windows.
pub(crate) const SESSION_DIR_NAME_ALPHABET: &[u8; 37] = b"abcdefghijklmnopqrstuvwxyz0123456789_";

/// How many names [`TempDir::new_session_working_dir`] tries before giving
/// up.
pub(crate) const SESSION_DIR_NAME_ATTEMPTS: usize = 10_000;

/// Generate one candidate session working directory name:
/// [`SESSION_DIR_NAME_LENGTH`] characters drawn uniformly from
/// [`SESSION_DIR_NAME_ALPHABET`] with the OS CSPRNG.
fn random_session_dir_name() -> std::io::Result<String> {
    session_dir_name_from(|buf| {
        getrandom::fill(buf).map_err(|e| {
            std::io::Error::other(format!("Failed to generate a random directory name: {e}"))
        })
    })
}

/// Build a session working directory name from the random bytes that `fill`
/// writes, calling it again for more bytes as needed.
///
/// Rejection sampling keeps the draw uniform: 222 is the largest multiple of
/// 37 that fits in a byte, so bytes >= 222 are discarded and each kept byte
/// maps to `SESSION_DIR_NAME_ALPHABET[byte % 37]`.
fn session_dir_name_from(
    mut fill: impl FnMut(&mut [u8]) -> std::io::Result<()>,
) -> std::io::Result<String> {
    const LIMIT: u8 =
        (256 / SESSION_DIR_NAME_ALPHABET.len() * SESSION_DIR_NAME_ALPHABET.len()) as u8;
    let mut name = String::with_capacity(SESSION_DIR_NAME_LENGTH);
    let mut buf = [0u8; 16];
    while name.len() < SESSION_DIR_NAME_LENGTH {
        fill(&mut buf)?;
        for b in buf {
            if b < LIMIT && name.len() < SESSION_DIR_NAME_LENGTH {
                let i = usize::from(b) % SESSION_DIR_NAME_ALPHABET.len();
                name.push(char::from(SESSION_DIR_NAME_ALPHABET[i]));
            }
        }
    }
    Ok(name)
}

/// Create a new directory under `parent` named by `next_name`, drawing a
/// fresh name each time the candidate already exists.
///
/// Each attempt is an exclusive `create_dir`, so an existing directory is
/// never reused and concurrent callers never share a path. Any other error,
/// or a failure of `next_name` (reported against `parent`), is returned at
/// once. After `attempts` names that all exist, fails with an
/// `AlreadyExists` error naming `parent`.
fn create_unique_dir(
    parent: &Path,
    attempts: usize,
    mut next_name: impl FnMut() -> std::io::Result<String>,
) -> Result<PathBuf, SessionError> {
    for _ in 0..attempts {
        let name = next_name().map_err(|source| SessionError::TempDir {
            path: parent.to_path_buf(),
            source,
        })?;
        let path = parent.join(name);
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(SessionError::TempDir { path, source: e }),
        }
    }
    Err(SessionError::TempDir {
        path: parent.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("No usable temporary directory name found after {attempts} attempts"),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs a closure when dropped, also while a failing test unwinds, so a
    /// test that fails does not leave its directory, process or ACL change
    /// behind. The closure must not panic.
    #[cfg(windows)]
    pub(crate) struct OnDrop<F: FnMut()>(pub(crate) F);

    #[cfg(windows)]
    impl<F: FnMut()> Drop for OnDrop<F> {
        fn drop(&mut self) {
            (self.0)()
        }
    }

    #[test]
    fn tempdir_debug() {
        let td = TempDir::new(None, None, None).unwrap();
        let dbg = format!("{td:?}");
        assert!(dbg.contains("TempDir"));
        assert!(dbg.contains("cleaned_up: false"));
    }

    #[test]
    fn tempdir_as_ref_path() {
        let td = TempDir::new(None, None, None).unwrap();
        let p: &std::path::Path = td.as_ref();
        assert_eq!(p, td.path());
    }

    fn sharing_violation() -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, "file in use")
    }

    #[test]
    fn remove_retry_stops_at_the_first_success() {
        let mut calls = 0;
        let mut slept = Vec::new();
        let delays = [
            Duration::from_millis(1),
            Duration::from_millis(2),
            Duration::from_millis(3),
        ];
        let result = remove_dir_all_with_retry(
            Path::new("unused"),
            &delays,
            |_| {
                calls += 1;
                if calls < 3 {
                    Err(sharing_violation())
                } else {
                    Ok(())
                }
            },
            |d| slept.push(d),
            |_, _| {},
        );
        assert!(result.is_ok());
        assert_eq!(calls, 3);
        assert_eq!(slept, delays[..2]);
    }

    #[test]
    fn remove_retry_reports_each_failure_with_the_next_delay() {
        let mut reported = Vec::new();
        let delays = [Duration::from_millis(1), Duration::from_millis(2)];
        let _ = remove_dir_all_with_retry(
            Path::new("unused"),
            &delays,
            |_| Err(sharing_violation()),
            |_| {},
            |e, next| reported.push((e.kind(), next)),
        );
        let kind = std::io::ErrorKind::PermissionDenied;
        assert_eq!(
            reported,
            [
                (kind, Some(delays[0])),
                (kind, Some(delays[1])),
                (kind, None)
            ]
        );
    }

    #[test]
    fn remove_retry_reports_nothing_when_the_first_attempt_succeeds() {
        let mut reported = 0;
        let result = remove_dir_all_with_retry(
            Path::new("unused"),
            &[Duration::ZERO],
            |_| Ok(()),
            |_| {},
            |_, _| reported += 1,
        );
        assert!(result.is_ok());
        assert_eq!(reported, 0);
    }

    fn holder(pid: u32, name: &str) -> FileHolder {
        FileHolder {
            pid,
            name: name.to_string(),
        }
    }

    #[test]
    fn failed_attempt_names_files_holders_and_next_delay() {
        let dir = Path::new("root");
        let remaining = [dir.join("a.bin"), dir.join("sub").join("b.bin")];
        let holders = Ok(vec![
            holder(42, "UnrealShaderCompileWorker.exe"),
            holder(42, "UnrealShaderCompileWorker.exe"),
            holder(7, "p4.exe"),
        ]);
        let msg = describe_failed_attempt(
            dir,
            &sharing_violation(),
            &remaining,
            false,
            Some(&holders),
            Some(Duration::from_secs(4)),
        );
        let sub_b = Path::new("sub").join("b.bin");
        assert_eq!(
            msg,
            format!(
                "Could not delete temporary directory root: file in use. 2 file(s) could not be \
                 deleted, including a.bin, {}. Files are still locked open by: \
                 UnrealShaderCompileWorker.exe (pid 42), p4.exe (pid 7). Retrying in 4 s.",
                sub_b.display()
            )
        );
    }

    #[test]
    fn failed_attempt_says_when_no_holder_was_found_or_lookup_failed() {
        let dir = Path::new("root");
        let left = [dir.join("a")];
        let err = sharing_violation();
        let start = "Could not delete temporary directory root: file in use. \
                     1 file(s) could not be deleted, including a.";
        let none = Ok(Vec::new());
        assert_eq!(
            describe_failed_attempt(dir, &err, &left, false, Some(&none), None),
            format!("{start} No running process was found holding them open. Giving up.")
        );
        let failed = Err("RmGetList failed (5)".to_string());
        assert_eq!(
            describe_failed_attempt(dir, &err, &left, false, Some(&failed), None),
            format!(
                "{start} The processes holding them open could not be determined: \
                 RmGetList failed (5). Giving up."
            )
        );
        assert_eq!(
            describe_failed_attempt(dir, &err, &left, false, None, None),
            format!("{start} Giving up.")
        );
        let no_files = describe_failed_attempt(dir, &err, &[], false, Some(&none), None);
        assert_eq!(
            no_files,
            "Could not delete temporary directory root: file in use. Giving up."
        );
    }

    /// At most three of the files are named; the count covers them all.
    #[test]
    fn failed_attempt_names_at_most_three_files() {
        let dir = Path::new("root");
        let stuck: Vec<PathBuf> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|n| dir.join(n))
            .collect();
        let msg = describe_failed_attempt(dir, &sharing_violation(), &stuck, false, None, None);
        assert_eq!(
            msg,
            "Could not delete temporary directory root: file in use. 5 file(s) could not be \
             deleted, including a, b, c. Giving up."
        );
    }

    #[test]
    fn failed_attempt_says_at_least_when_the_file_list_was_capped() {
        let dir = Path::new("root");
        let msg = describe_failed_attempt(
            dir,
            &sharing_violation(),
            &[dir.join("a")],
            true,
            None,
            None,
        );
        assert_eq!(
            msg,
            "Could not delete temporary directory root: file in use. At least 1 file(s) \
             could not be deleted, including a. Giving up."
        );
        let none_found = describe_failed_attempt(dir, &sharing_violation(), &[], true, None, None);
        assert_eq!(
            none_found,
            "Could not delete temporary directory root: file in use. No file that cannot be \
             deleted was found, but not every file could be checked. Giving up."
        );
    }

    /// The reason drops std's ` (os error N)` suffix and a final full stop, as
    /// Python's `strerror` does, and keeps other errors whole.
    #[test]
    fn failed_attempt_reason_drops_the_os_error_code() {
        let os = std::io::Error::from_raw_os_error(2);
        let text = os.to_string();
        let bare = text
            .strip_suffix(" (os error 2)")
            .expect("std appends the code");
        let bare = bare.strip_suffix('.').unwrap_or(bare);
        assert_eq!(error_reason(&os), bare);
        assert!(!bare.ends_with('.') && !bare.is_empty(), "{bare}");
        assert_eq!(error_reason(&sharing_violation()), "file in use");
        let ended = std::io::Error::other("file in use.");
        assert_eq!(error_reason(&ended), "file in use");
    }

    /// Limits for the walk tests, with a `settle` limit the tests can spot.
    fn limits(check: usize, report: usize) -> ScanLimits {
        ScanLimits {
            check,
            report,
            settle: 7,
        }
    }

    fn no_settle(_: &[PathBuf], _: usize) -> (Vec<PathBuf>, bool) {
        panic!("no file was undecided");
    }

    /// Walks subdirectories, reports only the files the check refuses, and
    /// stops at either limit.
    #[test]
    fn undeletable_files_reports_only_flagged_files_and_caps() {
        let td = TempDir::new(None, None, None).unwrap();
        std::fs::create_dir(td.path().join("sub")).unwrap();
        for name in ["a", "x1", "x2"] {
            std::fs::write(td.path().join(name), b"x").unwrap();
        }
        std::fs::write(td.path().join("sub").join("x3"), b"x").unwrap();
        let flagged = |p: &Path| {
            if p.file_name().unwrap().to_string_lossy().starts_with('x') {
                DeleteCheck::Refused
            } else {
                DeleteCheck::Allowed
            }
        };
        let (all, truncated) = undeletable_files(td.path(), limits(10, 10), flagged, no_settle);
        assert_eq!(names(all), ["x1", "x2", "x3"]);
        assert!(!truncated);
        let (two, truncated) = undeletable_files(td.path(), limits(10, 2), flagged, no_settle);
        assert_eq!(two.len(), 2);
        assert!(truncated, "stopped at the reporting limit");
        let mut checked = 0;
        let count = |_: &Path| {
            checked += 1;
            DeleteCheck::Allowed
        };
        let (_, truncated) = undeletable_files(td.path(), limits(3, 10), count, no_settle);
        assert_eq!(checked, 3);
        assert!(truncated, "stopped at the scan limit");
        let mut checked = 0;
        let count = |_: &Path| {
            checked += 1;
            DeleteCheck::Allowed
        };
        let (none, truncated) = undeletable_files(td.path(), limits(4, 10), count, no_settle);
        assert_eq!((none.len(), checked, truncated), (0, 4, false));
    }

    fn names(found: Vec<PathBuf>) -> Vec<String> {
        let mut n: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        n.sort();
        n
    }

    /// The walk enters each subdirectory where it is listed, as
    /// `remove_dir_all` does, so files are reported in that order, and a
    /// file in the first subdirectory is found even when a later one holds
    /// more files than the scan checks. (NTFS lists names in order; other
    /// file systems need not, so this runs on Windows only.)
    #[cfg(windows)]
    #[test]
    fn undeletable_files_walks_in_listing_order() {
        let td = TempDir::new(None, None, None).unwrap();
        for sub in ["a", "b", "c", "d"] {
            std::fs::create_dir(td.path().join(sub)).unwrap();
            std::fs::write(td.path().join(sub).join(format!("{sub}.bin")), b"x").unwrap();
        }
        std::fs::write(td.path().join("z_root.bin"), b"x").unwrap();
        let (found, _) = undeletable_files(
            td.path(),
            limits(10, 10),
            |_| DeleteCheck::Refused,
            no_settle,
        );
        let relative: Vec<PathBuf> = found
            .iter()
            .map(|p| p.strip_prefix(td.path()).unwrap().to_path_buf())
            .collect();
        let expected: Vec<PathBuf> = ["a/a.bin", "b/b.bin", "c/c.bin", "d/d.bin", "z_root.bin"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(relative, expected);

        let td = TempDir::new(None, None, None).unwrap();
        std::fs::create_dir(td.path().join("a_sub")).unwrap();
        std::fs::write(td.path().join("a_sub").join("locked.bin"), b"x").unwrap();
        std::fs::create_dir(td.path().join("b_sub")).unwrap();
        for i in 0..10 {
            std::fs::write(td.path().join("b_sub").join(format!("{i}.bin")), b"x").unwrap();
        }
        let locked = |p: &Path| {
            if p.ends_with("locked.bin") {
                DeleteCheck::Refused
            } else {
                DeleteCheck::Allowed
            }
        };
        let (found, truncated) = undeletable_files(td.path(), limits(3, 10), locked, no_settle);
        assert_eq!(names(found), ["locked.bin"]);
        assert!(truncated);
    }

    /// Undecided files go to `settle` together, once, after the walk, with
    /// the settle limit; the ones it returns are reported, and its stopping
    /// early marks the result incomplete.
    #[test]
    fn undeletable_files_settles_undecided_files_together() {
        let td = TempDir::new(None, None, None).unwrap();
        for name in ["r1", "u1", "u2", "u3", "ok"] {
            std::fs::write(td.path().join(name), b"x").unwrap();
        }
        let check = |p: &Path| match p.file_name().unwrap().to_string_lossy().as_bytes()[0] {
            b'r' => DeleteCheck::Refused,
            b'u' => DeleteCheck::Unknown,
            _ => DeleteCheck::Allowed,
        };
        let mut asked = Vec::new();
        let (found, truncated) =
            undeletable_files(td.path(), limits(10, 10), check, |unknown, max| {
                asked.push((names(unknown.to_vec()), max));
                let held = unknown
                    .iter()
                    .filter(|p| p.ends_with("u2"))
                    .cloned()
                    .collect();
                (held, false)
            });
        let u = |s: &str| s.to_string();
        assert_eq!(asked, [(vec![u("u1"), u("u2"), u("u3")], 7)]);
        assert_eq!(names(found), ["r1", "u2"]);
        assert!(!truncated);

        let (found, truncated) =
            undeletable_files(td.path(), limits(10, 10), check, |_, _| (Vec::new(), true));
        assert_eq!(names(found), ["r1"]);
        assert!(truncated, "settle stopped early");

        let (found, truncated) = undeletable_files(td.path(), limits(10, 1), check, |_, _| {
            panic!("the limit was reached")
        });
        assert_eq!(names(found), ["r1"]);
        assert!(truncated);

        let (found, truncated) =
            undeletable_files(td.path(), limits(10, 2), check, |unknown, _| {
                (unknown.to_vec(), false)
            });
        assert_eq!(found.len(), 2, "settled files are capped at the limit too");
        assert!(truncated);

        // Undecided files met before the refused ones that reach the limit
        // are not settled: the result is full already.
        let td = TempDir::new(None, None, None).unwrap();
        for name in ["a_undecided", "b_refused"] {
            std::fs::write(td.path().join(name), b"x").unwrap();
        }
        let check = |p: &Path| {
            if p.ends_with("b_refused") {
                DeleteCheck::Refused
            } else {
                DeleteCheck::Unknown
            }
        };
        let (found, truncated) = undeletable_files(td.path(), limits(10, 1), check, |_, _| {
            panic!("the limit was reached")
        });
        assert_eq!(names(found), ["b_refused"]);
        assert!(truncated);
    }

    /// Written as literals so a changed constant fails here.
    #[test]
    fn failed_attempt_limits_and_log_content() {
        assert_eq!(
            SCAN_LIMITS,
            ScanLimits {
                check: 10_000,
                report: 256,
                settle: 16
            }
        );
        assert_eq!(MAX_FILES_TO_NAME, 3);
        // FILE_PATH | EXCEPTION_INFO | PROCESS_CONTROL
        assert_eq!(FAILED_ATTEMPT_LOG_CONTENT.bits(), 0b0011_0010);
    }

    #[test]
    fn remove_retry_returns_the_last_error_when_exhausted() {
        let mut calls = 0;
        let delays = [Duration::ZERO, Duration::ZERO];
        let result = remove_dir_all_with_retry(
            Path::new("unused"),
            &delays,
            |_| {
                calls += 1;
                Err(sharing_violation())
            },
            |_| {},
            |_, _| {},
        );
        let err = result.expect_err("every attempt failed");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(calls, 3, "one attempt plus one per delay");
    }

    #[test]
    fn remove_retry_treats_not_found_on_a_retry_as_success() {
        let mut calls = 0;
        let result = remove_dir_all_with_retry(
            Path::new("unused"),
            &[Duration::ZERO],
            |_| {
                calls += 1;
                if calls == 1 {
                    Err(sharing_violation())
                } else {
                    Err(std::io::Error::from(std::io::ErrorKind::NotFound))
                }
            },
            |_| {},
            |_, _| {},
        );
        assert!(result.is_ok());
    }

    #[test]
    fn remove_retry_keeps_not_found_on_the_first_attempt_as_an_error() {
        let mut reported = 0;
        let result = remove_dir_all_with_retry(
            Path::new("unused"),
            &[Duration::ZERO],
            |_| Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
            |_| {},
            |_, _| reported += 1,
        );
        assert_eq!(
            result.expect_err("directory was never there").kind(),
            std::io::ErrorKind::NotFound
        );
        assert_eq!(reported, 0, "a missing directory is not a lock to report");
    }

    #[cfg(not(windows))]
    #[test]
    fn cleanup_does_not_retry_off_windows() {
        assert!(CLEANUP_RETRY_DELAYS.is_empty());
    }

    /// Written as literals so a changed constant fails here.
    #[cfg(windows)]
    #[test]
    fn cleanup_retry_delays_double_from_one_second() {
        let secs: Vec<u64> = CLEANUP_RETRY_DELAYS.iter().map(|d| d.as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 16]);
    }

    /// A file held open without `FILE_SHARE_DELETE` for less time than the
    /// retries cover: the first removal fails with a sharing violation, a
    /// retry succeeds, and the failed attempt is logged naming the holder.
    #[cfg(windows)]
    #[test]
    fn cleanup_waits_for_a_briefly_held_file() {
        use std::os::windows::fs::OpenOptionsExt;
        testing_logger::setup();
        let mut td = TempDir::new(None, None, None).unwrap();
        let dir = td.path().to_path_buf();
        let _remove = OnDrop(move || {
            let _ = std::fs::remove_dir_all(&dir);
        });
        let held = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(td.path().join("locked.bin"))
            .unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            drop(held);
        });
        let result = td.cleanup_with_delays(Some("session-1"), CLEANUP_RETRY_DELAYS);
        release.join().unwrap();
        result.unwrap();
        assert!(!td.path().exists());
        let expected = held_file_warning(td.path(), "locked.bin", "Retrying in 1 s.");
        testing_logger::validate(|logs| {
            let warns: Vec<_> = logs
                .iter()
                .filter(|l| l.level == log::Level::Warn)
                .map(|l| l.body.clone())
                .collect();
            assert_eq!(warns, std::slice::from_ref(&expected));
        });
    }

    /// The warning for a file this test process holds without sharing deletion.
    #[cfg(windows)]
    fn held_file_warning(dir: &Path, file: &str, end: &str) -> String {
        let exe = std::env::current_exe().unwrap();
        format!(
            "Could not delete temporary directory {}: The process cannot access the file \
             because it is being used by another process. 1 file(s) could not be deleted, \
             including {file}. Files are still locked open by: {} (pid {}). {end}",
            dir.display(),
            exe.file_name().unwrap().to_string_lossy(),
            std::process::id()
        )
    }

    /// The warning names the file that blocks deletion, and finds its holder,
    /// even when it sits in a subdirectory behind more free files than the
    /// holder lookup inspects.
    #[cfg(windows)]
    #[test]
    fn failed_attempt_names_a_held_file_among_many_free_ones() {
        use std::os::windows::fs::OpenOptionsExt;
        testing_logger::setup();
        let td = TempDir::new(None, None, None).unwrap();
        std::fs::create_dir(td.path().join("a_sub")).unwrap();
        for i in 0..300 {
            std::fs::write(td.path().join(format!("b{i:03}.txt")), b"x").unwrap();
        }
        let _held = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(td.path().join("a_sub").join("locked.bin"))
            .unwrap();
        let err = std::fs::remove_dir_all(td.path()).expect_err("a held file blocks removal");
        log_failed_attempt(
            Some("session-1"),
            td.path(),
            &err,
            Some(Duration::from_secs(1)),
        );
        let file = Path::new("a_sub").join("locked.bin");
        let expected =
            held_file_warning(td.path(), &file.display().to_string(), "Retrying in 1 s.");
        testing_logger::validate(|logs| {
            let warns: Vec<_> = logs
                .iter()
                .filter(|l| l.level == log::Level::Warn)
                .collect();
            assert_eq!(warns.len(), 1);
            assert_eq!(warns[0].body, expected);
        });
    }

    /// The holder lookup covers every file reported, not only the first: here
    /// a file this process holds and, after it, a running program held by
    /// its own process.
    #[cfg(windows)]
    #[test]
    fn failed_attempt_names_the_holders_of_every_file() {
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::process::CommandExt;
        testing_logger::setup();
        let td = TempDir::new(None, None, None).unwrap();
        let _held = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(td.path().join("a_locked.bin"))
            .unwrap();
        let exe = td.path().join("b_ping.exe");
        let system = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        std::fs::copy(Path::new(&system).join("System32").join("ping.exe"), &exe).unwrap();
        // CREATE_NO_WINDOW, so the test opens no console.
        let mut child = std::process::Command::new(&exe)
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .creation_flags(0x0800_0000)
            .spawn()
            .unwrap();
        let ping_pid = child.id();
        let stop = OnDrop(move || {
            let _ = child.kill();
            let _ = child.wait();
        });
        let err = std::fs::remove_dir_all(td.path()).expect_err("held files block removal");
        log_failed_attempt(Some("session-1"), td.path(), &err, None);
        drop(stop);
        let exe_name = std::env::current_exe().unwrap();
        let this = format!(
            "{} (pid {})",
            exe_name.file_name().unwrap().to_string_lossy(),
            std::process::id()
        );
        let ping = format!("b_ping.exe (pid {ping_pid})");
        let start = format!(
            "Could not delete temporary directory {}: The process cannot access the file \
             because it is being used by another process. 2 file(s) could not be deleted, \
             including a_locked.bin, b_ping.exe. Files are still locked open by: ",
            td.path().display()
        );
        testing_logger::validate(|logs| {
            let warns: Vec<_> = logs
                .iter()
                .filter(|l| l.level == log::Level::Warn)
                .collect();
            assert_eq!(warns.len(), 1);
            let body = &warns[0].body;
            let holders: Vec<&str> = body
                .strip_prefix(&start)
                .and_then(|rest| rest.strip_suffix(". Giving up."))
                .unwrap_or_else(|| panic!("unexpected warning: {body}"))
                .split(", ")
                .collect();
            // A scanner inspecting the new executable can appear too.
            assert!(holders.contains(&this.as_str()), "{body}");
            assert!(holders.contains(&ping.as_str()), "{body}");
        });
    }

    /// A running program whose ACL denies writing cannot be told apart by
    /// opening it, so cleanup asks the Restart Manager about it, and the
    /// warning names it and its process.
    #[cfg(windows)]
    #[test]
    fn cleanup_names_a_running_program_that_refuses_writing() {
        use std::os::windows::process::CommandExt;
        let run_hidden = |program: &str, args: &[&std::ffi::OsStr]| {
            // CREATE_NO_WINDOW, so the test opens no console.
            let status = std::process::Command::new(program)
                .args(args)
                .stdout(std::process::Stdio::null())
                .creation_flags(0x0800_0000)
                .status()
                .unwrap();
            assert!(status.success(), "{program} {args:?}");
        };
        testing_logger::setup();
        let mut td = TempDir::new(None, None, None).unwrap();
        let exe = td.path().join("ping.exe");
        let system = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        std::fs::copy(Path::new(&system).join("System32").join("ping.exe"), &exe).unwrap();
        let everyone = "*S-1-1-0";
        let deny = format!("{everyone}:(WD)");
        run_hidden(
            "icacls",
            &[exe.as_os_str(), "/deny".as_ref(), deny.as_ref()],
        );
        let mut child = std::process::Command::new(&exe)
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .creation_flags(0x0800_0000)
            .spawn()
            .unwrap();
        let pid = child.id();
        // Stops ping, lifts the deny and removes the directory, also if
        // cleanup panics.
        let (guard_exe, dir) = (exe.clone(), td.path().to_path_buf());
        let restore = OnDrop(move || {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::process::Command::new("icacls")
                .args([
                    guard_exe.as_os_str(),
                    "/remove:d".as_ref(),
                    everyone.as_ref(),
                ])
                .stdout(std::process::Stdio::null())
                .creation_flags(0x0800_0000)
                .status();
            let _ = std::fs::remove_dir_all(&dir);
        });
        let result = td.cleanup_with_delays(Some("session-1"), &[]);
        drop(restore);
        assert!(!td.path().exists(), "the directory was left behind");
        assert!(result.is_err(), "the running image was deleted");
        let start = format!(
            "Could not delete temporary directory {}: Access is denied. 1 file(s) could not \
             be deleted, including ping.exe. Files are still locked open by: ",
            td.path().display()
        );
        let ping = format!("ping.exe (pid {pid})");
        testing_logger::validate(|logs| {
            let warns: Vec<_> = logs
                .iter()
                .filter(|l| l.level == log::Level::Warn)
                .collect();
            assert_eq!(warns.len(), 1);
            let body = &warns[0].body;
            // Other holders can appear beside ping.exe, such as a scanner
            // inspecting the new executable.
            let holders = body
                .strip_prefix(&start)
                .and_then(|rest| rest.strip_suffix(". Giving up."))
                .unwrap_or_else(|| panic!("unexpected warning: {body}"));
            assert!(holders.split(", ").any(|h| h == ping), "{body}");
        });
    }

    /// A file held open for the whole cleanup: every attempt is logged, the
    /// last ends "Giving up.", and the error is returned.
    #[cfg(windows)]
    #[test]
    fn cleanup_reports_a_file_held_throughout() {
        use std::os::windows::fs::OpenOptionsExt;
        testing_logger::setup();
        let mut td = TempDir::new(None, None, None).unwrap();
        let dir = td.path().to_path_buf();
        let _remove = OnDrop(move || {
            let _ = std::fs::remove_dir_all(&dir);
        });
        let held = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(td.path().join("locked.bin"))
            .unwrap();
        let result = td.cleanup_with_delays(Some("session-1"), &[Duration::from_millis(100)]);
        let Err(SessionError::TempDir { source, .. }) = result else {
            panic!("expected a TempDir error, got {result:?}");
        };
        assert_eq!(source.raw_os_error(), Some(32));
        let expected = [
            held_file_warning(td.path(), "locked.bin", "Retrying in 0.1 s."),
            held_file_warning(td.path(), "locked.bin", "Giving up."),
        ];
        testing_logger::validate(|logs| {
            let warns: Vec<_> = logs
                .iter()
                .filter(|l| l.level == log::Level::Warn)
                .map(|l| l.body.clone())
                .collect();
            assert_eq!(warns, expected);
        });
        drop(held);
        std::fs::remove_dir_all(td.path()).unwrap();
    }

    /// The naming parameters match tempdir.md, written as literals so a
    /// changed constant fails here.
    #[test]
    fn session_dir_name_parameters_match_spec() {
        assert_eq!(SESSION_DIR_NAME_LENGTH, 8);
        assert_eq!(
            SESSION_DIR_NAME_ALPHABET,
            b"abcdefghijklmnopqrstuvwxyz0123456789_"
        );
        assert_eq!(SESSION_DIR_NAME_ATTEMPTS, 10_000);
    }

    /// A candidate name is exactly 8 characters from `[a-z0-9_]`.
    #[test]
    fn random_session_dir_name_length_and_alphabet() {
        for _ in 0..1000 {
            let name = random_session_dir_name().unwrap();
            assert_eq!(name.len(), 8, "{name:?}");
            assert!(
                name.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "{name:?}"
            );
        }
    }

    /// Bytes >= 222 are rejected and each kept byte maps to
    /// `ALPHABET[byte % 37]`. 221, the largest kept byte, maps to `_`.
    #[test]
    fn session_dir_name_from_rejects_biased_bytes() {
        let bytes: [u8; 12] = [222, 255, 0, 36, 37, 221, 1, 2, 3, 4, 25, 26];
        let mut calls = 0;
        let name = session_dir_name_from(|buf| {
            calls += 1;
            buf.fill(222);
            buf[..bytes.len()].copy_from_slice(&bytes);
            Ok(())
        })
        .unwrap();
        // 222 and 255 rejected; 0->a, 36->_, 37->a, 221->_, 1->b, 2->c, 3->d, 4->e.
        assert_eq!(name, "a_a_bcde");
        assert_eq!(calls, 1);
    }

    /// When a buffer yields too few accepted bytes, more are requested.
    #[test]
    fn session_dir_name_from_refills_until_full() {
        let mut calls = 0;
        let name = session_dir_name_from(|buf| {
            calls += 1;
            buf.fill(255);
            buf[0] = 24 + calls; // one accepted byte per call: z, 0, 1, ...
            Ok(())
        })
        .unwrap();
        assert_eq!(name, "z0123456");
        assert_eq!(calls, 8);
    }

    /// Every alphabet character is reachable, including the last one (`_`).
    #[test]
    fn random_session_dir_name_uses_whole_alphabet() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..2000 {
            seen.extend(random_session_dir_name().unwrap().bytes());
        }
        assert_eq!(seen.len(), SESSION_DIR_NAME_ALPHABET.len(), "{seen:?}");
    }

    /// An existing name is skipped and left untouched, and the next
    /// candidate is created.
    #[test]
    fn create_unique_dir_retries_on_collision() {
        let parent = tempfile::tempdir().unwrap();
        let taken = parent.path().join("taken000");
        std::fs::create_dir(&taken).unwrap();
        std::fs::write(taken.join("marker"), "x").unwrap();
        let mut names = vec!["taken000", "fresh000"].into_iter();
        let path =
            create_unique_dir(parent.path(), 10, || Ok(names.next().unwrap().to_string())).unwrap();
        assert_eq!(path, parent.path().join("fresh000"));
        assert!(path.is_dir());
        assert!(taken.join("marker").exists());
        assert_eq!(names.next(), None, "exactly two candidates are drawn");
    }

    /// When every candidate exists, creation stops after `attempts` tries
    /// with an error naming the parent directory.
    #[test]
    fn create_unique_dir_gives_up_after_attempts() {
        let parent = tempfile::tempdir().unwrap();
        std::fs::create_dir(parent.path().join("taken000")).unwrap();
        let mut calls = 0;
        let err = create_unique_dir(parent.path(), 3, || {
            calls += 1;
            Ok("taken000".to_string())
        })
        .unwrap_err();
        assert_eq!(calls, 3);
        assert_eq!(
            err.to_string(),
            format!(
                "Failed to create temp directory in {}: \
                 No usable temporary directory name found after 3 attempts",
                parent.path().display()
            )
        );
    }

    /// An error other than `AlreadyExists` is returned at once, naming the
    /// candidate path, without drawing another name.
    #[test]
    fn create_unique_dir_returns_other_errors_immediately() {
        let parent = tempfile::tempdir().unwrap();
        let missing = parent.path().join("missing");
        let mut calls = 0;
        let err = create_unique_dir(&missing, 10, || {
            calls += 1;
            Ok("abcdefgh".to_string())
        })
        .unwrap_err();
        assert_eq!(calls, 1);
        let SessionError::TempDir { path, source } = &err else {
            panic!("expected SessionError::TempDir, got {err:?}");
        };
        assert_eq!(path, &missing.join("abcdefgh"));
        assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
    }

    /// A failure to generate a name is reported against the parent directory.
    #[test]
    fn create_unique_dir_reports_name_generation_failure() {
        let parent = tempfile::tempdir().unwrap();
        let err = create_unique_dir(parent.path(), 10, || {
            Err(std::io::Error::other("no entropy"))
        })
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "Failed to create temp directory in {}: no entropy",
                parent.path().display()
            )
        );
    }

    /// The 10,000th candidate is still tried, and 10,000 taken names fail.
    #[test]
    fn new_session_working_dir_retries_up_to_attempt_bound() {
        let parent = tempfile::tempdir().unwrap();
        std::fs::create_dir(parent.path().join("taken000")).unwrap();

        let mut calls = 0;
        let mut td = TempDir::new_session_working_dir_with(parent.path(), None, || {
            calls += 1;
            Ok(if calls < 10_000 {
                "taken000"
            } else {
                "fresh000"
            }
            .to_string())
        })
        .unwrap();
        assert_eq!(calls, 10_000);
        assert_eq!(td.path(), parent.path().join("fresh000"));
        td.cleanup().unwrap();

        let err = TempDir::new_session_working_dir_with(parent.path(), None, || {
            Ok("taken000".to_string())
        })
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "Failed to create temp directory in {}: \
                 No usable temporary directory name found after 10000 attempts",
                parent.path().display()
            )
        );
    }

    /// The directory is created directly under the root with an
    /// 8-character name and, on POSIX, mode 0700.
    #[test]
    fn new_session_working_dir_creates_short_name_under_root() {
        let parent = tempfile::tempdir().unwrap();
        let mut td = TempDir::new_session_working_dir(parent.path(), None).unwrap();
        assert_eq!(td.path().parent(), Some(parent.path()));
        let name = td.path().file_name().unwrap().to_str().unwrap();
        assert_eq!(name.len(), SESSION_DIR_NAME_LENGTH);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(td.path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        td.cleanup().unwrap();
        assert!(!td.path().exists());
    }

    /// Mirrors Python TestSession::test_posix_permissions_warning.
    /// Creates a world-writable dir without the sticky bit and verifies detection.
    #[cfg(unix)]
    #[test]
    fn find_missing_sticky_bit_detects_world_writable_without_sticky() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("world_writable");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();

        let child = dir.join("child");
        std::fs::create_dir(&child).unwrap();

        let result = find_missing_sticky_bit(&child);
        assert_eq!(result, Some(dir));
    }

    /// Mirrors Python TestSession::test_posix_permissions_no_warning.
    /// A dir with the sticky bit set should not be flagged.
    #[cfg(unix)]
    #[test]
    fn find_missing_sticky_bit_none_when_sticky_set() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("sticky_dir");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o1777)).unwrap();

        let child = dir.join("child");
        std::fs::create_dir(&child).unwrap();

        let result = find_missing_sticky_bit(&child);
        assert_eq!(result, None);
    }

    /// Mirrors Python TestTempDirWindows::test_windows_temp_dir — verifies the
    /// warning when `PROGRAMDATA` is unset.
    ///
    /// Tests the pure helper `openjd_dir_from_programdata` directly so we
    /// don't have to mutate the real `PROGRAMDATA` env var, which would race
    /// with parallel tests that read it.
    #[cfg(windows)]
    #[test]
    fn openjd_dir_from_programdata_warns_when_unset() {
        testing_logger::setup();
        let dir = openjd_dir_from_programdata(None);
        assert_eq!(dir, PathBuf::from(r"C:\ProgramData\Amazon\OpenJD"));
        testing_logger::validate(|captured_logs| {
            assert!(
                captured_logs.iter().any(|log| {
                    log.level == log::Level::Warn && log.body.contains("PROGRAMDATA")
                }),
                "Expected a warning about PROGRAMDATA not being set"
            );
        });
    }

    /// Confirms the helper composes the path correctly when `PROGRAMDATA` is
    /// provided. No warning should be emitted.
    #[cfg(windows)]
    #[test]
    fn openjd_dir_from_programdata_uses_provided_value() {
        testing_logger::setup();
        let dir = openjd_dir_from_programdata(Some(r"D:\ProgramData".to_string()));
        assert_eq!(dir, PathBuf::from(r"D:\ProgramData\Amazon\OpenJD"));
        testing_logger::validate(|captured_logs| {
            assert!(
                !captured_logs
                    .iter()
                    .any(|log| log.level == log::Level::Warn),
                "Expected no warning when PROGRAMDATA is provided"
            );
        });
    }

    /// Mirrors Python TestTempDirWindows::test_windows_temp_dir — positive
    /// path: when an explicit OpenJD directory is provided, `openjd_temp_dir`
    /// uses it and creates it (and any missing parents) on disk.
    ///
    /// This test passes the directory directly via the `base_dir` parameter
    /// instead of mutating `PROGRAMDATA`, which would race with parallel
    /// tests that read it.
    #[cfg(windows)]
    #[test]
    fn openjd_temp_dir_honors_base_dir_override() {
        let custom_root = std::env::temp_dir().join("OpenJDBaseDirTest");
        // Ensure a clean slate so create_dir_all does real work.
        let _ = std::fs::remove_dir_all(&custom_root);

        let openjd_dir = custom_root.join("Amazon").join("OpenJD");
        let dir = openjd_temp_dir(Some(&openjd_dir))
            .expect("openjd_temp_dir should succeed with override");
        assert_eq!(
            dir, openjd_dir,
            "openjd_temp_dir should return the path it was given"
        );
        assert!(dir.exists(), "openjd_temp_dir should create the directory");
        assert!(
            custom_root.join("Amazon").exists(),
            "missing parents should be created as a side effect"
        );

        // Clean up
        let _ = std::fs::remove_dir_all(&custom_root);
    }
}
