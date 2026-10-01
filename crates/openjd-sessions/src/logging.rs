// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Structured logging with content classification.
//!
//! Mirrors Python's `openjd.sessions._logging.LogContent(Flag)`.
//! The worker agent uses `openjd_log_content` to route log records:
//! only EXCEPTION_INFO | PROCESS_CONTROL | HOST_INFO go to the worker log;
//! all records go to CloudWatch via the session log stream.

use bitflags::bitflags;

bitflags! {
    /// Describes the content of a log record, used by consumers to filter/route logs.
    ///
    /// ```
    /// use openjd_sessions::LogContent;
    ///
    /// let content = LogContent::COMMAND_OUTPUT | LogContent::PROCESS_CONTROL;
    /// assert!(content.contains(LogContent::COMMAND_OUTPUT));
    /// assert!(!content.contains(LogContent::BANNER));
    /// ```
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct LogContent: u32 {
        const BANNER = 1 << 0;
        const FILE_PATH = 1 << 1;
        const FILE_CONTENTS = 1 << 2;
        const COMMAND_OUTPUT = 1 << 3;
        const EXCEPTION_INFO = 1 << 4;
        const PROCESS_CONTROL = 1 << 5;
        const PARAMETER_INFO = 1 << 6;
        const HOST_INFO = 1 << 7;
    }
}

impl log::kv::ToValue for LogContent {
    fn to_value(&self) -> log::kv::Value<'_> {
        log::kv::Value::from(self.bits())
    }
}

/// Emit a structured log record with session_id, openjd_log_content, and
/// a precise timestamp captured at the point of the log call.
///
/// Usage:
///   session_log!(info, session_id, LogContent::HOST_INFO, "message {}", arg);
#[macro_export]
macro_rules! session_log {
    ($level:ident, $session_id:expr, $content:expr, $($arg:tt)+) => {
        log::$level!(
            target: "openjd.sessions",
            session_id = $session_id,
            openjd_log_content = $crate::logging::LogContent::bits(&$content),
            openjd_timestamp_usec = $crate::logging::timestamp_usec();
            $($arg)+
        )
    };
}

/// Return the current time as microseconds since the Unix epoch (u64).
/// Used by `session_log!` to attach a precise timestamp to each log record.
pub fn timestamp_usec() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

/// Like [`session_log!`], attributing the record to one action of the
/// Session when `$action` (an `Option<&str>`) is `Some`: the record then
/// carries the structured field `openjd_action = <name>` and its message is
/// prefixed with `[<name>] `. With `None` the record is exactly a
/// `session_log!` record.
///
/// This is how a Service Session satisfies RFC 0009 "Concurrency with
/// `onRun`" rule 3 (log attribution): `onReadinessCheck` runs while `onRun`
/// runs, so every line of its output — and every process-control line about
/// it — is tagged, while `onRun`'s lines stay untagged.
///
/// Usage:
///   session_action_log!(info, session_id, Some("onReadinessCheck"), LogContent::COMMAND_OUTPUT, "{}", line);
#[macro_export]
macro_rules! session_action_log {
    ($level:ident, $session_id:expr, $action:expr, $content:expr, $($arg:tt)+) => {
        match ($action as Option<&str>) {
            Some(action) => log::$level!(
                target: "openjd.sessions",
                session_id = $session_id,
                openjd_log_content = $crate::logging::LogContent::bits(&$content),
                openjd_timestamp_usec = $crate::logging::timestamp_usec(),
                openjd_action = action;
                "[{}] {}", action, format_args!($($arg)+)
            ),
            None => $crate::session_log!($level, $session_id, $content, $($arg)+),
        }
    };
}

/// Log a section banner (major section separator).
pub fn log_section_banner(session_id: &str, title: &str) {
    session_log!(info, session_id, LogContent::BANNER, "");
    session_log!(
        info,
        session_id,
        LogContent::BANNER,
        "=============================================="
    );
    session_log!(info, session_id, LogContent::BANNER, "--------- {}", title);
    session_log!(
        info,
        session_id,
        LogContent::BANNER,
        "=============================================="
    );
}

/// Log a subsection banner (minor section separator).
pub fn log_subsection_banner(session_id: &str, title: &str) {
    session_log!(
        info,
        session_id,
        LogContent::BANNER,
        "----------------------------------------------"
    );
    session_log!(info, session_id, LogContent::BANNER, "{}", title);
    session_log!(
        info,
        session_id,
        LogContent::BANNER,
        "----------------------------------------------"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_action_log_tags_and_prefixes_only_when_an_action_is_given() {
        testing_logger::setup();
        crate::session_action_log!(
            info,
            "sid",
            Some("onReadinessCheck"),
            LogContent::COMMAND_OUTPUT,
            "line {}",
            1
        );
        crate::session_action_log!(info, "sid", None, LogContent::COMMAND_OUTPUT, "line {}", 2);
        testing_logger::validate(|logs| {
            assert_eq!(logs.len(), 2);
            assert_eq!(logs[0].body, "[onReadinessCheck] line 1");
            assert_eq!(logs[1].body, "line 2");
            assert_eq!(logs[0].target, "openjd.sessions");
        });
    }
}
