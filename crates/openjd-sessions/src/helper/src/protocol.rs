// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;

/// Expected length of the per-helper authentication token in bytes.
///
/// The session generates a 22-character ASCII string from a 64-element
/// URL-safe alphabet (128 bits of entropy, 6 bits per character). The helper
/// stores and compares raw bytes so it doesn't have to know anything about
/// the alphabet — only the length.
pub const AUTH_TOKEN_LEN: usize = 22;

/// A run-command request from the session.
#[derive(Debug, Deserialize)]
pub struct RunCommand {
    pub command: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    pub cwd: String,
    /// Set when the Action may report `openjd_output_var` values (RFC 0010):
    /// the helper then reads the file of each `@` directive as the job user
    /// and sends it as `Response::OutputFile` frames in place of the line.
    /// Absent, every line is sent as `Response::Out`.
    #[serde(default, rename = "outputFiles")]
    pub output_files: Option<OutputFiles>,
}

/// How the helper resolves `@` directives for one Action.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct OutputFiles {
    /// The most bytes of a file the parent can accept: its per-Task output
    /// limit. The helper reads at most one byte more, so the parent can tell
    /// a file that exceeds it.
    #[serde(rename = "maxBytes")]
    pub max_bytes: usize,
}

/// Cancel method, matching the OpenJD spec's `cancelationMethod` semantics:
///
/// - `Terminate`: immediate hard kill (process tree).
/// - `NotifyThenTerminate { notify_period_in_seconds }`: send a platform-
///   appropriate soft signal (SIGTERM / CTRL_BREAK), then escalate to a hard
///   kill after `notify_period_in_seconds` if the child hasn't exited.
///
/// Wire format:
///   `{"token":"...","cancel":"TERMINATE"}`
///   `{"token":"...","cancel":"NOTIFY_THEN_TERMINATE","notifyPeriodInSeconds":<u64>}`
#[derive(Debug, Clone)]
pub enum CancelMethod {
    Terminate,
    NotifyThenTerminate { notify_period_in_seconds: u64 },
}

/// Commands received on stdin from the session.
///
/// Every command carries a copy of the shared auth token. The helper verifies
/// the token in constant time before acting on the command; mismatches are
/// reported as `{"error":"invalid token"}` and ignored.
#[derive(Debug)]
pub enum Command {
    Run { token: String, run: RunCommand },
    Cancel { token: String, method: CancelMethod },
    Shutdown { token: String },
}

impl Command {
    /// The token string carried by this command.
    pub fn token(&self) -> &str {
        match self {
            Command::Run { token, .. } => token,
            Command::Cancel { token, .. } => token,
            Command::Shutdown { token } => token,
        }
    }
}

/// Responses sent on stdout to the session.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Response {
    Pid {
        pid: u32,
    },
    /// One line of child output. The flags are serialized only when set, so
    /// an ordinary line is `{"out":"..."}`.
    Out {
        out: String,
        /// The line was cut to fit a limit; the rest was discarded.
        #[serde(skip_serializing_if = "is_false")]
        truncated: bool,
        /// The line was not valid UTF-8 and was decoded with `\xNN` escapes.
        #[serde(skip_serializing_if = "is_false")]
        lossy: bool,
    },
    /// The header of an `@` directive the helper resolved, sent in place of
    /// its `Out` line and followed by `OutputFileData` frames.
    OutputFile {
        #[serde(rename = "outputFile")]
        output_file: OutputFileHeader,
    },
    /// The next part of the data an `OutputFile` header announced.
    OutputFileData {
        #[serde(rename = "outputFileData")]
        output_file_data: String,
    },
    Exited {
        exited: i32,
    },
    Error {
        error: String,
    },
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// The header of a resolved `@` directive.
///
/// The `OutputFileData` frames that follow carry, as one string split into
/// frames that each fit the response line limit, the directive's line
/// (`lineLen` bytes) and then the first `validUtf8` bytes of the file. The
/// line can be up to 64 KiB, which would not fit in one frame once escaped,
/// so it travels in the data rather than in the header.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputFileHeader {
    /// The byte length of the line at the start of the data.
    pub line_len: usize,
    /// The byte range of the output's name in the line.
    pub name: [usize; 2],
    /// The task selector's value, if the directive has one.
    pub task: Option<i64>,
    /// The byte range of the file path in the line.
    pub path: [usize; 2],
    /// Why the file could not be read: a phrase that completes "the @ file
    /// '<path>' ...". When set, the data holds only the line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// How many bytes were read: the file's length, or `maxBytes + 1` for a
    /// longer file.
    pub size: usize,
    /// How many of those bytes, from the start, are valid UTF-8, and so are
    /// in the data. When less than `size`, the byte after them is not valid
    /// UTF-8.
    pub valid_utf8: usize,
}

impl<'de> Deserialize<'de> for Command {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let serde_json::Value::Object(mut map) = value else {
            return Err(serde::de::Error::custom("expected JSON object"));
        };

        // Extract the token field. Missing or non-string → treat as an empty
        // token; the main loop's constant-time check will still reject it.
        let token = match map.remove("token") {
            Some(serde_json::Value::String(s)) => s,
            Some(_) | None => String::new(),
        };

        if map.contains_key("command") {
            let run: RunCommand = serde_json::from_value(serde_json::Value::Object(map))
                .map_err(serde::de::Error::custom)?;
            Ok(Command::Run { token, run })
        } else if let Some(sig) = map.get("cancel") {
            let method_str = sig
                .as_str()
                .ok_or_else(|| serde::de::Error::custom("cancel must be a string"))?;
            let method = match method_str {
                "TERMINATE" => CancelMethod::Terminate,
                "NOTIFY_THEN_TERMINATE" => {
                    let notify_period_in_seconds = map
                        .get("notifyPeriodInSeconds")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| {
                            serde::de::Error::custom(
                                "NOTIFY_THEN_TERMINATE requires notifyPeriodInSeconds (u64)",
                            )
                        })?;
                    CancelMethod::NotifyThenTerminate {
                        notify_period_in_seconds,
                    }
                }
                other => {
                    return Err(serde::de::Error::custom(format!(
                        "unknown cancel method: {other}"
                    )));
                }
            };
            Ok(Command::Cancel { token, method })
        } else if map
            .get("shutdown")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            Ok(Command::Shutdown { token })
        } else {
            Err(serde::de::Error::custom("unknown command object"))
        }
    }
}

/// Constant-time equality check between a received token and the expected
/// bytes. Returns `false` for any length mismatch before touching contents,
/// which is fine: the token's length is fixed and public.
///
/// For equal-length inputs, runs `a.len()` byte operations regardless of
/// where the first mismatching byte sits, so the timing of the compare
/// doesn't reveal how many bytes matched.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Write a JSON response line to stdout and flush.
pub fn send(response: &Response) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = serde_json::to_writer(&mut lock, response);
    let _ = lock.write_all(b"\n");
    let _ = lock.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_response_serializes_to_bare_out_envelope() {
        // The framer's size guard assumes Response::Out serializes as exactly
        // {"out":"..."} (10-byte envelope); a tag or extra field would break
        // its budget math silently, so pin the shape here.
        let json = serde_json::to_string(&Response::Out {
            out: "x".into(),
            truncated: false,
            lossy: false,
        })
        .unwrap();
        assert_eq!(json, r#"{"out":"x"}"#);
    }

    #[test]
    fn out_response_flags_serialize_when_set() {
        // The framer's OUT_ENVELOPE budget assumes this exact shape.
        let json = serde_json::to_string(&Response::Out {
            out: "x".into(),
            truncated: true,
            lossy: true,
        })
        .unwrap();
        assert_eq!(json, r#"{"out":"x","truncated":true,"lossy":true}"#);
        assert_eq!(
            json.len() - "x".len() + "\n".len(),
            crate::framer::OUT_ENVELOPE,
            "OUT_ENVELOPE is this envelope (minus the 1-byte payload) plus the newline"
        );
    }

    #[test]
    fn output_file_frames_serialize() {
        let header = Response::OutputFile {
            output_file: OutputFileHeader {
                line_len: 22,
                name: [19, 20],
                task: Some(3),
                path: [22, 22],
                error: None,
                size: 2,
                valid_utf8: 2,
            },
        };
        assert_eq!(
            serde_json::to_string(&header).unwrap(),
            r#"{"outputFile":{"lineLen":22,"name":[19,20],"task":3,"path":[22,22],"size":2,"validUtf8":2}}"#
        );
        let data = Response::OutputFileData {
            output_file_data: "ab".into(),
        };
        assert_eq!(
            serde_json::to_string(&data).unwrap(),
            r#"{"outputFileData":"ab"}"#
        );
    }

    #[test]
    fn parses_run_command_output_files() {
        let cmd: Command = serde_json::from_str(
            r#"{"token":"t","command":"echo","args":[],"cwd":"/tmp","outputFiles":{"maxBytes":1048576}}"#,
        )
        .unwrap();
        match cmd {
            Command::Run { run, .. } => {
                assert_eq!(run.output_files.unwrap().max_bytes, 1_048_576)
            }
            _ => panic!("expected Run"),
        }
        let cmd: Command =
            serde_json::from_str(r#"{"token":"t","command":"echo","args":[],"cwd":"/tmp"}"#)
                .unwrap();
        match cmd {
            Command::Run { run, .. } => assert!(run.output_files.is_none()),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn parses_run_command_with_token() {
        let cmd: Command = serde_json::from_str(
            r#"{"token":"AbCdEfGhIjKlMnOpQrStUv","command":"echo","args":["hi"],"cwd":"/tmp"}"#,
        )
        .unwrap();
        match cmd {
            Command::Run { token, run } => {
                assert_eq!(token, "AbCdEfGhIjKlMnOpQrStUv");
                assert_eq!(run.command, "echo");
                assert_eq!(run.args, vec!["hi"]);
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn parses_cancel_with_token() {
        let cmd: Command =
            serde_json::from_str(r#"{"token":"AbCdEfGhIjKlMnOpQrStUv","cancel":"TERMINATE"}"#)
                .unwrap();
        match cmd {
            Command::Cancel { token, method } => {
                assert_eq!(token, "AbCdEfGhIjKlMnOpQrStUv");
                assert!(matches!(method, CancelMethod::Terminate));
            }
            _ => panic!("expected Cancel"),
        }
    }

    #[test]
    fn parses_notify_then_terminate_with_token() {
        let cmd: Command = serde_json::from_str(
            r#"{"token":"t","cancel":"NOTIFY_THEN_TERMINATE","notifyPeriodInSeconds":5}"#,
        )
        .unwrap();
        match cmd {
            Command::Cancel {
                method:
                    CancelMethod::NotifyThenTerminate {
                        notify_period_in_seconds: 5,
                    },
                ..
            } => {}
            _ => panic!("expected NotifyThenTerminate(5)"),
        }
    }

    #[test]
    fn parses_shutdown_with_token() {
        let cmd: Command =
            serde_json::from_str(r#"{"token":"AbCdEfGhIjKlMnOpQrStUv","shutdown":true}"#).unwrap();
        match cmd {
            Command::Shutdown { token } => assert_eq!(token, "AbCdEfGhIjKlMnOpQrStUv"),
            _ => panic!("expected Shutdown"),
        }
    }

    #[test]
    fn missing_token_parses_as_empty_string() {
        // The main loop is responsible for rejecting this; the deserializer
        // just succeeds with an empty token so the command loop stays aligned.
        let cmd: Command =
            serde_json::from_str(r#"{"command":"echo","args":[],"cwd":"/tmp"}"#).unwrap();
        assert_eq!(cmd.token(), "");
    }

    #[test]
    fn bare_shutdown_string_is_rejected() {
        // The old bare-string form is no longer accepted: shutdown is always
        // an object with a token.
        let err = serde_json::from_str::<Command>(r#""shutdown""#).unwrap_err();
        assert!(err.to_string().contains("expected JSON object"));
    }

    #[test]
    fn constant_time_eq_basic() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"", b"x"));
        assert!(constant_time_eq(b"", b""));
    }
}
