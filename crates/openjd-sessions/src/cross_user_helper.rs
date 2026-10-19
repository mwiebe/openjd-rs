// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Persistent cross-user helper process for subprocess execution.
//!
//! On POSIX, the helper is launched via `sudo -u <user> -i <helper_path>`.
//! On Windows, the helper is launched via `CreateProcessAsUserW` / `CreateProcessWithLogonW`.
//!
//! In both cases the helper communicates over newline-delimited JSON on stdin/stdout,
//! avoiding per-action login costs and enabling reliable cancellation from the
//! target user's context.

use std::io::Read;
use std::path::Path;

use crate::action::{ActionMessage, ActionState};
use crate::error::SessionError;
use crate::logging::LogContent;
use crate::output_vars::{Begin, HelperFileParts, LineOutcome};

/// Maximum bytes for a single JSON response line from the helper's stdout.
/// Matches the helper binary's own `MAX_LINE_LENGTH` (128 KB).
pub(crate) const MAX_RESPONSE_LINE_LENGTH: usize = 128 * 1024;
use crate::session_log;
use crate::session_user::SessionUser;

/// Length of the shared auth token in characters.
///
/// Matches the helper's `AUTH_TOKEN_LEN` constant. The helper rejects
/// `--auth-token` values of any other length.
pub(crate) const AUTH_TOKEN_LEN: usize = 22;

/// 64-character URL-safe alphabet used to render the auth token.
///
/// Chosen so that `byte & 0x3F` is a uniform index with no rejection
/// sampling. The character set avoids `+` / `/` / `=` / whitespace so the
/// token is safe in argv, JSON, filenames, and log lines without quoting.
const AUTH_TOKEN_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Generate a fresh per-helper auth token: [`AUTH_TOKEN_LEN`] ASCII characters
/// drawn from [`AUTH_TOKEN_ALPHABET`]. Uses the OS CSPRNG via `getrandom::fill`.
///
/// Each character contributes 6 bits of entropy (64-char alphabet), so
/// 22 characters ≥ 128 bits of entropy.
pub(crate) fn generate_auth_token() -> Result<String, SessionError> {
    let mut raw = [0u8; AUTH_TOKEN_LEN];
    getrandom::fill(&mut raw).map_err(|e| {
        SessionError::HelperCommunication(format!("Failed to generate auth token: {e}"))
    })?;
    let mut out = String::with_capacity(AUTH_TOKEN_LEN);
    for b in raw {
        out.push(AUTH_TOKEN_ALPHABET[(b & 0x3F) as usize] as char);
    }
    Ok(out)
}

/// Return `cmd` extended with `"token": <auth_token>`.
///
/// `cmd` must be a JSON object, since every helper command is an object.
/// If the caller's map already contains a `"token"` entry, it is overwritten
/// — `augment_with_token` is the sole owner of that field in outgoing
/// commands.
fn augment_with_token(
    cmd: &serde_json::Value,
    auth_token: &str,
) -> Result<serde_json::Value, SessionError> {
    let serde_json::Value::Object(map) = cmd else {
        return Err(SessionError::HelperCommunication(format!(
            "helper command must be a JSON object, got: {cmd}"
        )));
    };
    let mut with_token = map.clone();
    with_token.insert(
        "token".to_string(),
        serde_json::Value::String(auth_token.to_string()),
    );
    Ok(serde_json::Value::Object(with_token))
}

// ── Async helper reader ──

/// Future type returned by `AsyncHelperReader::next_response`.
type NextResponseFuture<'a> = std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Option<Result<serde_json::Value, SessionError>>>
            + Send
            + 'a,
    >,
>;

/// Async stream of JSON responses from the helper's stdout.
/// Both platforms use a reader thread + async channel.
pub(crate) trait AsyncHelperReader: Send {
    /// Receive the next JSON response. Returns None when the helper exits.
    fn next_response(&mut self) -> NextResponseFuture<'_>;
}

/// Unix: async reads using a reader thread + channel (same pattern as Windows).
/// While AsyncFd could theoretically provide zero-thread async reads on Unix,
/// it doesn't implement AsyncRead directly. The reader thread approach is
/// simple, uniform across platforms, and has negligible overhead.
#[cfg(unix)]
pub(crate) struct UnixAsyncHelperReader {
    rx: tokio::sync::mpsc::UnboundedReceiver<Result<serde_json::Value, SessionError>>,
    _thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl UnixAsyncHelperReader {
    pub fn new(stdout: std::process::ChildStdout) -> Result<Self, SessionError> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let thread = std::thread::spawn(move || {
            use std::io::BufRead;
            let mut reader = std::io::BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match Read::take(&mut reader, MAX_RESPONSE_LINE_LENGTH as u64).read_line(&mut line)
                {
                    Ok(0) => break,
                    Ok(n) => {
                        if n == MAX_RESPONSE_LINE_LENGTH && !line.ends_with('\n') {
                            // Line exceeded limit — discard remainder and report error
                            let mut discard = Vec::new();
                            let _ = reader.read_until(b'\n', &mut discard);
                            let result = Err(SessionError::HelperCommunication(format!(
                                "response line exceeds {MAX_RESPONSE_LINE_LENGTH} byte limit"
                            )));
                            if tx.send(result).is_err() {
                                break;
                            }
                            continue;
                        }
                        let result = serde_json::from_str(line.trim_end()).map_err(|e| {
                            SessionError::HelperCommunication(format!("parse error: {e}"))
                        });
                        if tx.send(result).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            rx,
            _thread: Some(thread),
        })
    }
}

#[cfg(unix)]
impl AsyncHelperReader for UnixAsyncHelperReader {
    fn next_response(&mut self) -> NextResponseFuture<'_> {
        Box::pin(async { self.rx.recv().await })
    }
}

/// Windows: reader thread relays lines from the sync pipe to an async channel.
#[cfg(windows)]
pub(crate) struct WindowsAsyncHelperReader {
    rx: tokio::sync::mpsc::UnboundedReceiver<Result<serde_json::Value, SessionError>>,
    _thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(windows)]
impl WindowsAsyncHelperReader {
    pub fn new(stdout: std::fs::File) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let thread = std::thread::spawn(move || {
            use std::io::BufRead;
            let mut reader = std::io::BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match Read::take(&mut reader, MAX_RESPONSE_LINE_LENGTH as u64).read_line(&mut line)
                {
                    Ok(0) => break,
                    Ok(n) => {
                        if n == MAX_RESPONSE_LINE_LENGTH && !line.ends_with('\n') {
                            // Line exceeded limit — discard remainder and report error
                            let mut discard = Vec::new();
                            let _ = reader.read_until(b'\n', &mut discard);
                            let result = Err(SessionError::HelperCommunication(format!(
                                "response line exceeds {MAX_RESPONSE_LINE_LENGTH} byte limit"
                            )));
                            if tx.send(result).is_err() {
                                break;
                            }
                            continue;
                        }
                        let result = serde_json::from_str(line.trim_end()).map_err(|e| {
                            SessionError::HelperCommunication(format!("parse error: {e}"))
                        });
                        if tx.send(result).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            rx,
            _thread: Some(thread),
        }
    }
}

#[cfg(windows)]
impl AsyncHelperReader for WindowsAsyncHelperReader {
    fn next_response(&mut self) -> NextResponseFuture<'_> {
        Box::pin(async { self.rx.recv().await })
    }
}

/// Manages a long-lived helper process for cross-user command execution (POSIX).
///
/// On POSIX: launched via `sudo -u <user> -i <helper_path> --auth-token <TOKEN>`.
/// Communicates over newline-delimited JSON on stdin/stdout. Every request
/// carries the shared `auth_token` string; the helper rejects anything else.
#[cfg(unix)]
pub(crate) struct CrossUserHelper {
    child: std::process::Child,
    stdin: std::io::BufWriter<std::process::ChildStdin>,
    pub(crate) async_reader: UnixAsyncHelperReader,
    /// 22-char auth token shared with the helper. Included in every command
    /// sent over `stdin` and over the dup'd `cancel_writer`.
    auth_token: String,
}

/// Windows variant: the child is a raw process handle from `CreateProcessAsUserW`,
/// not a `std::process::Child`. We wrap stdin/stdout from the pipe handles.
#[cfg(windows)]
pub(crate) struct CrossUserHelperWin {
    process_handle: windows::Win32::Foundation::HANDLE,
    stdin: std::io::BufWriter<std::fs::File>,
    pub(crate) async_reader: WindowsAsyncHelperReader,
    /// 22-char auth token shared with the helper. Included in every command
    /// sent over `stdin` and over the dup'd `cancel_writer`.
    auth_token: String,
}

// SAFETY: `CrossUserHelperWin` is Send because all of its fields can be
// sent across threads:
// - `process_handle: HANDLE` is a Windows kernel object handle (pointer-
//   sized integer). Kernel handles are process-wide and safe to use from
//   any thread. `HANDLE` is `!Send` in `windows-rs` out of caution, but the
//   process handle here is only used for wait/terminate operations that
//   accept any thread's handle.
// - `stdin: BufWriter<File>` is already Send.
// - `async_reader: WindowsAsyncHelperReader` owns an `UnboundedReceiver`
//   and a `JoinHandle`, both of which are Send.
#[cfg(windows)]
unsafe impl Send for CrossUserHelperWin {}

#[cfg(unix)]
impl CrossUserHelper {
    /// Spawn the helper binary as the given user via sudo (POSIX).
    ///
    /// Generates a fresh per-helper auth token, passes it to the helper as
    /// `--auth-token <TOKEN>`, and stores it on the returned struct so every
    /// subsequent command can echo it back.
    ///
    /// Returns `(helper, cancel_writer)` where `cancel_writer` is a dup'd copy
    /// of the helper's stdin fd. This allows sending cancel commands even while
    /// the helper struct is moved to a runner during action execution.
    #[cfg(unix)]
    pub fn spawn(
        helper_path: &Path,
        user: &dyn SessionUser,
    ) -> Result<(Self, std::fs::File), SessionError> {
        let auth_token = generate_auth_token()?;
        let mut child = std::process::Command::new("sudo")
            .args([
                "-u",
                user.user(),
                "-i",
                &helper_path.to_string_lossy(),
                "--auth-token",
                &auth_token,
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|source| SessionError::SubprocessStart {
                // Don't include the token in error messages.
                command: format!(
                    "sudo -u {} -i {} --auth-token <redacted>",
                    user.user(),
                    helper_path.display()
                ),
                source,
            })?;

        let child_stdin = child.stdin.take().expect("stdin was piped");

        // Dup the stdin fd so we can write cancel commands from Session::cancel_action
        // while the helper is owned by a runner.
        use std::os::unix::io::{AsFd, FromRawFd, IntoRawFd};
        let dup_fd = nix::unistd::dup(child_stdin.as_fd()).map_err(|e| {
            SessionError::HelperCommunication(format!("Failed to dup helper stdin fd: {e}"))
        })?;
        let cancel_writer = unsafe { FromRawFd::from_raw_fd(dup_fd.into_raw_fd()) };

        let stdin = std::io::BufWriter::new(child_stdin);
        let async_reader =
            UnixAsyncHelperReader::new(child.stdout.take().expect("stdout was piped"))?;

        Ok((
            Self {
                child,
                stdin,
                async_reader,
                auth_token,
            },
            cancel_writer,
        ))
    }

    /// The shared auth token. Callers that write directly to the helper's
    /// stdin (like the cancel_writer path) must include this in every request.
    pub fn auth_token(&self) -> &str {
        &self.auth_token
    }

    /// Send a JSON command to the helper, injecting the shared auth token.
    ///
    /// `cmd` must be a JSON object; the helper's protocol requires every
    /// command to be an object. The token is inserted as `"token"` before
    /// the line is written.
    pub fn send_command(&mut self, cmd: &serde_json::Value) -> Result<(), SessionError> {
        use std::io::Write;
        let payload = augment_with_token(cmd, &self.auth_token)?;
        serde_json::to_writer(&mut self.stdin, &payload).map_err(|e| {
            SessionError::HelperCommunication(format!("Failed to write to helper stdin: {e}"))
        })?;
        self.stdin.write_all(b"\n").map_err(|e| {
            SessionError::HelperCommunication(format!("Failed to write newline to helper: {e}"))
        })?;
        self.stdin.flush().map_err(|e| {
            SessionError::HelperCommunication(format!("Failed to flush helper stdin: {e}"))
        })?;
        Ok(())
    }

    /// Send a tokenized shutdown command and wait for the child to exit.
    pub fn shutdown(&mut self) {
        let _ = self.send_command(&serde_json::json!({"shutdown": true}));
        let _ = self.child.wait();
    }
}

#[cfg(unix)]
impl Drop for CrossUserHelper {
    fn drop(&mut self) {
        // Safety net: kill the child if shutdown() wasn't called.
        if let Ok(None) = self.child.try_wait() {
            log::warn!(target: "openjd.sessions", "CrossUserHelper dropped without shutdown(), killing child process");
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(windows)]
impl CrossUserHelperWin {
    /// Spawn the helper binary as the given user via `CreateProcessAsUserW` (Windows).
    ///
    /// Generates a fresh per-helper auth token, passes it to the helper as
    /// `--auth-token <TOKEN>`, and stores it on the returned struct so every
    /// subsequent command can echo it back.
    ///
    /// Returns `(helper, cancel_writer)` where `cancel_writer` is a `DuplicateHandle`'d
    /// copy of the helper's stdin pipe. This allows sending cancel commands even while
    /// the helper struct is moved to a runner during action execution.
    pub fn spawn(
        helper_path: &Path,
        user: &dyn SessionUser,
    ) -> Result<(Self, std::fs::File), SessionError> {
        use crate::session_user::WindowsSessionUser;
        use std::collections::HashMap;
        use std::os::windows::io::FromRawHandle;
        use windows::Win32::Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS};
        use windows::Win32::System::Threading::GetCurrentProcess;

        let wu = user
            .as_any()
            .downcast_ref::<WindowsSessionUser>()
            .ok_or_else(|| {
                SessionError::Runtime("Cross-user on Windows requires WindowsSessionUser".into())
            })?;

        let auth_token = generate_auth_token()?;

        let spawned = crate::win32::spawn_as_user_with_stdin(
            &[
                helper_path.to_string_lossy().to_string(),
                "--auth-token".to_string(),
                auth_token.clone(),
            ],
            &HashMap::new(),
            None, // working_dir — helper doesn't need one
            wu.password(),
            wu.user(),
            wu.logon_token(),
        )
        .map_err(|e| SessionError::SubprocessStart {
            // Don't include the token in error messages.
            command: format!(
                "spawn_as_user_with_stdin {} --auth-token <redacted>",
                helper_path.display()
            ),
            source: std::io::Error::other(e),
        })?;

        let stdin_write = spawned.stdin_write.ok_or_else(|| {
            SessionError::HelperCommunication(
                "spawn_as_user_with_stdin did not return stdin pipe".into(),
            )
        })?;

        // Convert OwnedHandle → File for stdin
        let stdin_file: std::fs::File = stdin_write.into();

        // DuplicateHandle to create a cancel_writer (like dup() on POSIX)
        let cancel_writer = unsafe {
            use std::os::windows::io::AsRawHandle;
            let current_process = GetCurrentProcess();
            let src_handle = windows::Win32::Foundation::HANDLE(stdin_file.as_raw_handle());
            let mut dup_handle = windows::Win32::Foundation::HANDLE::default();
            DuplicateHandle(
                current_process,
                src_handle,
                current_process,
                &mut dup_handle,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
            .map_err(|e| {
                SessionError::HelperCommunication(format!(
                    "DuplicateHandle for cancel_writer failed: {e}"
                ))
            })?;
            std::fs::File::from_raw_handle(dup_handle.0 as std::os::windows::io::RawHandle)
        };

        // Convert stdout OwnedHandle → File
        let stdout_file: std::fs::File = spawned.stdout_read.into();

        let stdin = std::io::BufWriter::new(stdin_file);
        let async_reader = WindowsAsyncHelperReader::new(stdout_file);

        Ok((
            Self {
                process_handle: spawned.process_handle,
                stdin,
                async_reader,
                auth_token,
            },
            cancel_writer,
        ))
    }

    /// The shared auth token. Callers that write directly to the helper's
    /// stdin (like the cancel_writer path) must include this in every request.
    pub fn auth_token(&self) -> &str {
        &self.auth_token
    }

    /// Send a JSON command to the helper, injecting the shared auth token.
    pub fn send_command(&mut self, cmd: &serde_json::Value) -> Result<(), SessionError> {
        use std::io::Write;
        let payload = augment_with_token(cmd, &self.auth_token)?;
        serde_json::to_writer(&mut self.stdin, &payload).map_err(|e| {
            SessionError::HelperCommunication(format!("Failed to write to helper stdin: {e}"))
        })?;
        self.stdin.write_all(b"\n").map_err(|e| {
            SessionError::HelperCommunication(format!("Failed to write newline to helper: {e}"))
        })?;
        self.stdin.flush().map_err(|e| {
            SessionError::HelperCommunication(format!("Failed to flush helper stdin: {e}"))
        })?;
        Ok(())
    }

    /// Send a tokenized shutdown command and wait for the process to exit.
    pub fn shutdown(&mut self) {
        let _ = self.send_command(&serde_json::json!({"shutdown": true}));
        unsafe {
            let _ =
                windows::Win32::System::Threading::WaitForSingleObject(self.process_handle, 5000);
            let _ = windows::Win32::Foundation::CloseHandle(self.process_handle);
            self.process_handle = windows::Win32::Foundation::INVALID_HANDLE_VALUE;
        }
    }
}

#[cfg(windows)]
impl Drop for CrossUserHelperWin {
    fn drop(&mut self) {
        // Safety net: terminate the process if shutdown() wasn't called.
        if !self.process_handle.is_invalid() {
            log::warn!(target: "openjd.sessions", "CrossUserHelperWin dropped without shutdown(), terminating child process");
            unsafe {
                let _ = windows::Win32::System::Threading::TerminateProcess(self.process_handle, 1);
                let _ = windows::Win32::Foundation::CloseHandle(self.process_handle);
            }
        }
    }
}

/// Trait for helpers that provide both async reading and sync command sending.
pub(crate) trait AsyncHelper: Send {
    fn async_reader(&mut self) -> &mut dyn AsyncHelperReader;
    fn send_command(&mut self, cmd: &serde_json::Value) -> Result<(), SessionError>;
    /// The helper's shared auth token. Used by callers that bypass
    /// `send_command` (e.g. the cancel_writer path in `run_via_helper`).
    fn auth_token(&self) -> &str;
}

#[cfg(unix)]
impl AsyncHelper for CrossUserHelper {
    fn async_reader(&mut self) -> &mut dyn AsyncHelperReader {
        &mut self.async_reader
    }
    fn send_command(&mut self, cmd: &serde_json::Value) -> Result<(), SessionError> {
        CrossUserHelper::send_command(self, cmd)
    }
    fn auth_token(&self) -> &str {
        CrossUserHelper::auth_token(self)
    }
}

#[cfg(windows)]
impl AsyncHelper for CrossUserHelperWin {
    fn async_reader(&mut self) -> &mut dyn AsyncHelperReader {
        &mut self.async_reader
    }
    fn send_command(&mut self, cmd: &serde_json::Value) -> Result<(), SessionError> {
        CrossUserHelperWin::send_command(self, cmd)
    }
    fn auth_token(&self) -> &str {
        CrossUserHelperWin::auth_token(self)
    }
}

/// Execute a subprocess via a CrossUserHelper, returning the result.
///
/// This is async — the helper stdout is read via `AsyncHelperReader`, allowing
/// `drive_action`'s select loop to process `ActionMessage`s (progress, status,
/// env vars) concurrently while the helper runs.
pub(crate) async fn run_via_helper(
    helper: &mut dyn AsyncHelper,
    config: &crate::subprocess::SubprocessConfig,
    filter: &mut crate::action_filter::ActionFilter,
    session_id: &str,
    message_tx: tokio::sync::mpsc::UnboundedSender<ActionMessage>,
    cancel_writer: Option<&std::fs::File>,
) -> Result<crate::subprocess::SubprocessResult, SessionError> {
    // Windows note: executable resolution happens inside the helper
    // (helper/src/runner_win.rs::locate_executable), which runs as the
    // target user — it can resolve executables in directories only that
    // user can read, and its PATH fallback is the target user's own
    // environment (matching what the workload actually runs under).
    // Resolution failures come back over the protocol as an error response
    // and are mapped to SessionError::SubprocessStart below.
    let args = &config.args;

    // Build the env map (only set values; unsets are excluded).
    let env: serde_json::Map<String, serde_json::Value> = config
        .env_vars
        .iter()
        .filter_map(|(k, v)| {
            v.as_ref()
                .map(|val| (k.clone(), serde_json::Value::String(val.clone())))
        })
        .collect();

    let mut cmd = serde_json::json!({
        "command": args[0],
        "args": &args[1..],
        "env": env,
        "cwd": config.working_dir,
    });
    // An Action that may report output values has the helper read each `@`
    // file as the job user (RFC 0010 §File values item 3). For any other
    // Action the helper leaves `openjd_output_var:` lines alone, so they reach
    // the filter here unchanged: ordinary output, or invalid when Forbidden.
    if let Some(max_bytes) = filter.output_vars_mut().max_bytes() {
        cmd["outputFiles"] = serde_json::json!({ "maxBytes": max_bytes });
    }

    helper.send_command(&cmd)?;

    // Log the actual command (not the helper protocol)
    session_log!(
        info,
        session_id,
        LogContent::FILE_PATH | LogContent::PROCESS_CONTROL,
        "Running command {}",
        crate::subprocess::format_command_for_log(args)
    );

    // Timeout as async future instead of OS thread
    let timeout_fut = match config.timeout {
        Some(d) => {
            use futures_util::FutureExt;
            tokio::time::sleep(d).boxed()
        }
        None => {
            use futures_util::FutureExt;
            futures_util::future::pending::<()>().boxed()
        }
    };
    tokio::pin!(timeout_fut);
    let mut timed_out = false;

    let mut stdout_collected = String::new();
    let mut saw_fail = false;
    // A long line or resolved `@` directive whose data frames are arriving.
    let mut pending_slot: Option<PendingFrames> = None;

    loop {
        tokio::select! {
            biased;
            resp = helper.async_reader().next_response() => {
                let resp = match resp {
                    None => return Err(SessionError::HelperCommunication(
                        "Helper process closed stdout unexpectedly".into(),
                    )),
                    Some(r) => r?,
                };

                if let Some(pid) = resp.get("pid").and_then(|v| v.as_i64()) {
                    session_log!(
                        info,
                        session_id,
                        LogContent::PROCESS_CONTROL,
                        "Command started as pid: {}",
                        pid
                    );
                    continue;
                }

                if let Some(pending) = pending_slot.as_mut() {
                    // Between an `outLong` or `outputFile` header and its last
                    // data frame, only data frames may arrive.
                    let Some(data) = resp.get("data").and_then(|v| v.as_str()) else {
                        return Err(SessionError::HelperCommunication(format!(
                            "Expected a data frame from the helper, got: {resp}"
                        )));
                    };
                    if !pending.push(data)? {
                        continue;
                    }
                    let complete = pending_take(&mut pending_slot)?;
                    let outcome = complete.outcome(filter);
                    log_line(complete.line(), outcome, filter, session_id, &message_tx, config.debug_collect_stdout, &mut stdout_collected, &mut saw_fail);
                    continue;
                }

                if let Some(header) = resp.get("outputFile") {
                    let Some(max_bytes) = filter.output_vars_mut().max_bytes() else {
                        return Err(SessionError::HelperCommunication(
                            "The helper sent an outputFile frame for an Action that can't \
                             report output values"
                                .into(),
                        ));
                    };
                    start_frames(PendingFrames::file(header, max_bytes)?, &mut pending_slot, filter, session_id, &message_tx, config.debug_collect_stdout, &mut stdout_collected, &mut saw_fail)?;
                    continue;
                }

                if let Some(header) = resp.get("outLong") {
                    if filter.output_vars_mut().max_bytes().is_none() {
                        return Err(SessionError::HelperCommunication(
                            "The helper sent an outLong frame for an Action that can't \
                             report output values"
                                .into(),
                        ));
                    }
                    start_frames(PendingFrames::long_line(header)?, &mut pending_slot, filter, session_id, &message_tx, config.debug_collect_stdout, &mut stdout_collected, &mut saw_fail)?;
                    continue;
                }

                if let Some(line) = resp.get("out").and_then(|v| v.as_str()) {
                    let line = crate::subprocess::truncate_line(line);
                    let flag = |key: &str| resp.get(key).and_then(|v| v.as_bool()).unwrap_or(false);
                    let output = out_line_outcome(filter, line, flag("truncated"), flag("lossy"));
                    log_line(line, output, filter, session_id, &message_tx, config.debug_collect_stdout, &mut stdout_collected, &mut saw_fail);
                    continue;
                }

                if let Some(code) = resp.get("exited").and_then(|v| v.as_i64()) {
                    let exit_code = code as i32;
                    session_log!(
                        info,
                        session_id,
                        LogContent::PROCESS_CONTROL,
                        "Process exit code: {}",
                        exit_code
                    );

                    let canceled = config
                        .cancel_request_rx
                        .as_ref()
                        .is_some_and(|rx| rx.has_changed().unwrap_or(false));

                    let state = if canceled {
                        ActionState::Canceled
                    } else if timed_out {
                        ActionState::Timeout
                    } else if saw_fail {
                        ActionState::Failed
                    } else if exit_code == 0 {
                        ActionState::Success
                    } else {
                        ActionState::Failed
                    };
                    return Ok(crate::subprocess::SubprocessResult {
                        state,
                        exit_code: Some(exit_code),
                        stdout: stdout_collected,
                    });
                }

                if let Some(msg) = resp.get("error").and_then(|v| v.as_str()) {
                    return Err(SessionError::SubprocessStart {
                        command: args[0].clone(),
                        source: std::io::Error::other(msg.to_string()),
                    });
                }

                return Err(SessionError::HelperCommunication(format!(
                    "Unexpected helper response: {}",
                    resp
                )));
            }
            _ = &mut timeout_fut, if !timed_out => {
                timed_out = true;
                // Send cancel to helper via the cancel_writer
                if let Some(writer) = cancel_writer {
                    use std::io::Write;
                    let mut w = writer.try_clone().map_err(|e| {
                        SessionError::HelperCommunication(format!("Failed to clone cancel_writer: {e}"))
                    })?;
                    let token = helper.auth_token();
                    let cancel_method = &config.cancel_method;
                    let notify_period = match cancel_method {
                        crate::runner::CancelMethod::NotifyThenTerminate { terminate_delay } => {
                            terminate_delay.as_secs()
                        }
                        crate::runner::CancelMethod::Terminate => 0,
                    };
                    // The token alphabet excludes `"` and `\`, so no
                    // escaping is needed to embed it in a JSON literal.
                    if notify_period == 0 {
                        let _ = writeln!(w, r#"{{"token":"{token}","cancel":"TERMINATE"}}"#);
                    } else {
                        let _ = writeln!(
                            w,
                            r#"{{"token":"{token}","cancel":"NOTIFY_THEN_TERMINATE","notifyPeriodInSeconds":{notify_period}}}"#
                        );
                    }
                    let _ = w.flush();
                }
            }
        }
    }
}

/// Process and log one line of the helper's output, as the direct path does.
#[allow(clippy::too_many_arguments)]
fn log_line(
    line: &str,
    output: Option<LineOutcome>,
    filter: &mut crate::action_filter::ActionFilter,
    session_id: &str,
    message_tx: &tokio::sync::mpsc::UnboundedSender<ActionMessage>,
    collect: bool,
    stdout_collected: &mut String,
    saw_fail: &mut bool,
) {
    let (display, log) =
        crate::subprocess::process_line(line, output, filter, session_id, message_tx, saw_fail);
    if log {
        session_log!(info, session_id, LogContent::COMMAND_OUTPUT, "{}", display);
    }
    if collect {
        stdout_collected.push_str(&display);
        stdout_collected.push('\n');
    }
}

/// What `Begin` on an ordinary `out` line of the helper path comes to.
///
/// The helper resolves every `@` directive the parser reads as one (a parity
/// test pins its recognizer to the parser), so an `@` directive that arrives
/// as an `out` line in an Action that may report outputs means the two
/// disagree; it is invalid rather than read here, because the parent never
/// reads the file.
fn out_line_outcome(
    filter: &mut crate::action_filter::ActionFilter,
    line: &str,
    truncated: bool,
    lossy: bool,
) -> Option<LineOutcome> {
    match filter.output_vars_mut().begin(line, truncated, lossy) {
        Begin::NotDirective => None,
        Begin::Done(outcome) => Some(outcome),
        Begin::ReadFile(request) => {
            Some(filter.output_vars_mut().unresolved_on_helper_path(&request))
        }
    }
}

/// Hold `frames` until its data arrives, or process it at once when there is
/// none to wait for.
#[allow(clippy::too_many_arguments)]
fn start_frames(
    frames: PendingFrames,
    slot: &mut Option<PendingFrames>,
    filter: &mut crate::action_filter::ActionFilter,
    session_id: &str,
    message_tx: &tokio::sync::mpsc::UnboundedSender<ActionMessage>,
    collect: bool,
    stdout_collected: &mut String,
    saw_fail: &mut bool,
) -> Result<(), SessionError> {
    if frames.is_complete() {
        let complete = frames.complete()?;
        let outcome = complete.outcome(filter);
        log_line(
            complete.line(),
            outcome,
            filter,
            session_id,
            message_tx,
            collect,
            stdout_collected,
            saw_fail,
        );
    } else {
        *slot = Some(frames);
    }
    Ok(())
}

/// Take the completed frames out of `slot`.
fn pending_take(slot: &mut Option<PendingFrames>) -> Result<CompleteFrames, SessionError> {
    slot.take().expect("caller checked the slot").complete()
}

/// What a header announced: a long directive line, or a resolved `@`
/// directive.
#[derive(Debug)]
enum FramesKind {
    /// `outLong`: the data is the line; the flags mean what they mean on
    /// `out`.
    Line { truncated: bool, lossy: bool },
    /// `outputFile`: the data is the line, then the valid UTF-8 prefix of the
    /// file's content.
    File {
        parts: HelperFileParts,
        error: Option<String>,
        size: usize,
        valid_utf8: usize,
    },
}

/// A header and the `data` frames received for it so far.
#[derive(Debug)]
struct PendingFrames {
    line_len: usize,
    expected: usize,
    data: String,
    kind: FramesKind,
}

fn bad_header(kind: &str, header: &serde_json::Value) -> SessionError {
    SessionError::HelperCommunication(format!("Malformed {kind} header: {header}"))
}

fn header_usize(header: &serde_json::Value, key: &str) -> Option<usize> {
    header
        .get(key)
        .and_then(|v| v.as_u64())
        .and_then(|v| usize::try_from(v).ok())
}

impl PendingFrames {
    /// Check an `outLong` header.
    fn long_line(header: &serde_json::Value) -> Result<Self, SessionError> {
        let bad = || bad_header("outLong", header);
        let len = header_usize(header, "len").ok_or_else(bad)?;
        let flag = |key: &str| match header.get(key) {
            None => Ok(false),
            Some(v) => v.as_bool().ok_or_else(bad),
        };
        let kind = FramesKind::Line {
            truncated: flag("truncated")?,
            lossy: flag("lossy")?,
        };
        if len > crate::subprocess::LOG_LINE_MAX_LENGTH {
            return Err(bad());
        }
        Ok(Self {
            line_len: len,
            expected: len,
            data: String::new(),
            kind,
        })
    }

    /// Check an `outputFile` header against what the helper was asked to do.
    /// Anything inconsistent is a protocol error, not a directive error: the
    /// helper is trusted only to report what it read, its output stream can
    /// carry what processes of the job user write to it, and its numbers
    /// bound how much the parent buffers.
    fn file(header: &serde_json::Value, max_bytes: usize) -> Result<Self, SessionError> {
        let bad = || bad_header("outputFile", header);
        let range_of = |key: &str| -> Option<std::ops::Range<usize>> {
            let a = header.get(key)?.as_array()?;
            match a.as_slice() {
                [s, e] => {
                    Some(usize::try_from(s.as_u64()?).ok()?..usize::try_from(e.as_u64()?).ok()?)
                }
                _ => None,
            }
        };
        let line_len = header_usize(header, "lineLen").ok_or_else(bad)?;
        let size = header_usize(header, "size").ok_or_else(bad)?;
        let valid_utf8 = header_usize(header, "validUtf8").ok_or_else(bad)?;
        let task = match header.get("task") {
            None | Some(serde_json::Value::Null) => None,
            Some(v) => Some(v.as_i64().ok_or_else(bad)?),
        };
        let error = match header.get("error") {
            None => None,
            Some(v) => Some(v.as_str().ok_or_else(bad)?.to_string()),
        };
        let parts = HelperFileParts {
            name: range_of("name").ok_or_else(bad)?,
            task,
            path: range_of("path").ok_or_else(bad)?,
        };
        if line_len > crate::subprocess::LOG_LINE_MAX_LENGTH
            || size > max_bytes.saturating_add(1)
            || valid_utf8 > size
            || (error.is_some() && size != 0)
        {
            return Err(bad());
        }
        Ok(Self {
            line_len,
            expected: line_len + valid_utf8,
            data: String::new(),
            kind: FramesKind::File {
                parts,
                error,
                size,
                valid_utf8,
            },
        })
    }

    fn is_complete(&self) -> bool {
        self.data.len() == self.expected
    }

    /// Add a data frame; `true` once the data is complete.
    fn push(&mut self, data: &str) -> Result<bool, SessionError> {
        if self.data.len() + data.len() > self.expected {
            return Err(SessionError::HelperCommunication(
                "The helper sent more data than its header announced".into(),
            ));
        }
        self.data.push_str(data);
        Ok(self.is_complete())
    }

    /// The complete data split into the line and what follows it. The split
    /// must fall on a character boundary: a header whose `lineLen` cuts a
    /// character is a protocol error.
    fn complete(self) -> Result<CompleteFrames, SessionError> {
        if !self.data.is_char_boundary(self.line_len) {
            return Err(SessionError::HelperCommunication(
                "The helper's header puts the end of the line inside a character".into(),
            ));
        }
        let mut line = self.data;
        let rest = line.split_off(self.line_len);
        Ok(match self.kind {
            FramesKind::Line { truncated, lossy } => CompleteFrames::Line {
                line,
                truncated,
                lossy,
            },
            FramesKind::File {
                parts,
                error,
                size,
                valid_utf8,
            } => {
                let content = match error {
                    Some(error) => Err(error),
                    None => {
                        let mut content = rest.into_bytes();
                        if valid_utf8 < size {
                            // Stand-in bytes for what was not valid UTF-8, so
                            // the collector reports where the content stops
                            // being UTF-8 and the size check sees the size
                            // the helper read.
                            content.resize(size, 0xFF);
                        }
                        Ok(content)
                    }
                };
                CompleteFrames::File {
                    line,
                    parts,
                    content,
                }
            }
        })
    }
}

/// Frames whose data has all arrived.
#[derive(Debug)]
enum CompleteFrames {
    Line {
        line: String,
        truncated: bool,
        lossy: bool,
    },
    File {
        line: String,
        parts: HelperFileParts,
        content: Result<Vec<u8>, String>,
    },
}

impl CompleteFrames {
    fn line(&self) -> &str {
        match self {
            Self::Line { line, .. } | Self::File { line, .. } => line,
        }
    }

    /// Process the line as the Action's `openjd_output_var` state says.
    fn outcome(&self, filter: &mut crate::action_filter::ActionFilter) -> Option<LineOutcome> {
        match self {
            Self::Line {
                line,
                truncated,
                lossy,
            } => out_line_outcome(filter, line, *truncated, *lossy),
            Self::File {
                line,
                parts,
                content,
            } => Some(
                filter
                    .output_vars_mut()
                    .finish_helper_file(line, parts, content.clone()),
            ),
        }
    }
}

#[cfg(test)]
mod frames_tests {
    use super::*;
    use crate::output_vars::OutputVars;
    use openjd_expr::{ExprValue, PathFormat};
    use openjd_model::outputs::OutputCollector;
    use serde_json::json;

    fn filter() -> crate::action_filter::ActionFilter {
        let declared: Vec<openjd_model::job::OutputVariable> =
            serde_json::from_str(r#"[{"name": "N", "type": "STRING"}]"#).unwrap();
        let collector =
            OutputCollector::new(&declared, 100, &[], PathFormat::host(), None).unwrap();
        let mut f = crate::action_filter::ActionFilter::new("s", true, false);
        f.set_output_vars(OutputVars::allowed(collector, std::env::temp_dir(), 100));
        f
    }

    fn header(line_len: usize, size: usize, valid: usize) -> serde_json::Value {
        json!({"lineLen": line_len, "name": [19, 20], "task": null,
               "path": [22, 23], "size": size, "validUtf8": valid})
    }

    fn message(e: SessionError) -> String {
        e.to_string()
    }

    #[test]
    fn file_header_is_validated() {
        let ok = header(23, 2, 2);
        assert!(PendingFrames::file(&ok, 100).is_ok());
        for (bad, why) in [
            (header(65_537, 0, 0), "line over the line limit"),
            (header(23, 102, 102), "size over max_bytes + 1"),
            (header(23, 2, 3), "validUtf8 over size"),
            (
                json!({"lineLen": 23, "name": [19, 20], "path": [22, 23], "size": 1,
                       "validUtf8": 1, "error": "does not exist"}),
                "an error with data",
            ),
            (
                json!({"lineLen": 23, "size": 0, "validUtf8": 0}),
                "no ranges",
            ),
            (
                json!({"lineLen": 23, "name": [19], "path": [22, 23], "size": 0, "validUtf8": 0}),
                "a short range",
            ),
            (
                json!({"lineLen": 23, "name": [19, 20], "path": [22, 23], "task": "x",
                       "size": 0, "validUtf8": 0}),
                "a task that is not an integer",
            ),
        ] {
            let e = PendingFrames::file(&bad, 100).expect_err(why);
            assert!(
                message(e).starts_with("Cross-user helper error: Malformed outputFile header: "),
                "{why}"
            );
        }
        // Exactly max_bytes + 1 is allowed: that is how the helper reports a
        // file that is too large.
        assert!(PendingFrames::file(&header(23, 101, 101), 100).is_ok());
    }

    #[test]
    fn long_line_header_is_validated() {
        assert!(PendingFrames::long_line(&json!({"len": 65_536})).is_ok());
        for bad in [
            json!({"len": 65_537}),
            json!({}),
            json!({"len": 3, "lossy": "yes"}),
        ] {
            let e = PendingFrames::long_line(&bad).unwrap_err();
            assert!(message(e).starts_with("Cross-user helper error: Malformed outLong header: "));
        }
    }

    #[test]
    fn data_must_match_the_header() {
        let mut p = PendingFrames::file(&header(23, 2, 2), 100).unwrap();
        assert!(!p.push("openjd_output_var: N=@f").unwrap());
        assert!(p.push("\"x").is_ok());
        assert_eq!(
            message(p.push("\"y").unwrap_err()),
            "Cross-user helper error: The helper sent more data than its header announced"
        );
    }

    #[test]
    fn line_end_inside_a_character_is_refused() {
        // "é" is two bytes. A file header whose lineLen is 1 and whose
        // content is 1 byte announces 2 bytes of data, and the line ends
        // inside the character.
        let mut p = PendingFrames::file(
            &json!({"lineLen": 1, "name": [0, 1], "task": null, "path": [0, 1],
                    "size": 1, "validUtf8": 1}),
            100,
        )
        .unwrap();
        assert!(p.push("é").unwrap());
        assert_eq!(
            message(p.complete().unwrap_err()),
            "Cross-user helper error: The helper's header puts the end of the line inside a \
             character"
        );
    }

    #[test]
    fn invalid_utf8_content_keeps_the_reported_size() {
        let line = "openjd_output_var: N=@f";
        let mut p = PendingFrames::file(&header(23, 50, 2), 100).unwrap();
        assert!(p.push(&format!("{line}ab")).unwrap());
        match p.complete().unwrap() {
            CompleteFrames::File {
                line: l, content, ..
            } => {
                assert_eq!(l, line);
                let content = content.unwrap();
                assert_eq!(content.len(), 50);
                assert_eq!(&content[..3], b"ab\xff");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn complete_file_frames_feed_the_collector() {
        let line = "openjd_output_var: N=@f";
        let mut p = PendingFrames::file(&header(23, 3, 3), 100).unwrap();
        assert!(p.push(&format!("{line}\"x\"")).unwrap());
        let mut f = filter();
        match p.complete().unwrap().outcome(&mut f) {
            Some(LineOutcome::Accepted { name, value, .. }) => {
                assert_eq!((name.as_str(), value), ("N", ExprValue::String("x".into())));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn long_lines_are_processed_as_out_lines() {
        let text = format!("openjd_output_var: N=\"{}\"", "\\\\".repeat(10));
        let mut p = PendingFrames::long_line(&json!({"len": text.len()})).unwrap();
        assert!(p.push(&text).unwrap());
        let mut f = filter();
        assert!(matches!(
            p.complete().unwrap().outcome(&mut f),
            Some(LineOutcome::Accepted { .. })
        ));
        // With a flag, it is rejected like an out line with it.
        let mut p =
            PendingFrames::long_line(&json!({"len": text.len(), "truncated": true})).unwrap();
        assert!(p.push(&text).unwrap());
        let mut f = filter();
        assert!(matches!(
            p.complete().unwrap().outcome(&mut f),
            Some(LineOutcome::Invalid(m)) if m.starts_with("Invalid openjd_output_var directive: the line is longer")
        ));
    }

    #[test]
    fn unresolved_at_directive_on_the_helper_path_is_invalid() {
        let mut f = filter();
        assert!(matches!(
            out_line_outcome(&mut f, "openjd_output_var: N=@f", false, false),
            Some(LineOutcome::Invalid(m)) if m == "Output variable 'N': the cross-user helper did not read the @ file 'f'"
        ));
        let mut f = filter();
        assert!(out_line_outcome(&mut f, "hello", false, false).is_none());
    }
}

#[cfg(test)]
mod token_tests {
    use super::*;

    #[test]
    fn token_is_22_chars_from_url_safe_alphabet() {
        for _ in 0..100 {
            let t = generate_auth_token().unwrap();
            assert_eq!(
                t.len(),
                AUTH_TOKEN_LEN,
                "token should be {AUTH_TOKEN_LEN} chars",
            );
            assert!(t.is_ascii(), "token must be ASCII");
            for c in t.bytes() {
                assert!(
                    AUTH_TOKEN_ALPHABET.contains(&c),
                    "token contains non-alphabet byte: {c:#x}",
                );
            }
        }
    }

    #[test]
    fn generated_tokens_differ() {
        // This could technically collide, but the probability over two
        // 128-bit draws is astronomically small. If this ever flakes, the
        // CSPRNG is broken.
        let a = generate_auth_token().unwrap();
        let b = generate_auth_token().unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn augment_with_token_inserts_token() {
        let cmd = serde_json::json!({"command": "echo", "args": ["hi"]});
        let out = augment_with_token(&cmd, "T-O-K-E-N").unwrap();
        assert_eq!(out.get("token").and_then(|v| v.as_str()), Some("T-O-K-E-N"),);
        assert_eq!(out.get("command").and_then(|v| v.as_str()), Some("echo"));
    }

    #[test]
    fn augment_rejects_non_object() {
        let err = augment_with_token(&serde_json::json!("shutdown"), "tok").unwrap_err();
        assert!(err.to_string().contains("must be a JSON object"));
    }

    #[test]
    fn augment_overrides_caller_supplied_token() {
        // augment_with_token is the sole owner of the "token" field on
        // outgoing commands — a caller-supplied one must be overwritten
        // so callers can't accidentally (or intentionally) poison it.
        let cmd = serde_json::json!({"token": "bad", "command": "x"});
        let out = augment_with_token(&cmd, "good").unwrap();
        assert_eq!(
            out.get("token").and_then(|v| v.as_str()),
            Some("good"),
            "augment_with_token must not let the caller override the token",
        );
    }
}
