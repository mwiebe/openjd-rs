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

/// The attribution of a log record: a session-wide tag, an action tag, both,
/// or neither (see [`session_tagged_log!`](crate::session_tagged_log)).
///
/// `session` is the Session's [`SessionConfig::log_tag`](crate::session::SessionConfig::log_tag)
/// — set by a runner that merges several Sessions' logs into one stream, as
/// `openjd run` does for a Job's Service Sessions (`Service <name>`), so a
/// Service's `onRun` output interleaved with Task output stays attributable.
/// `action` is the name of an action running concurrently with another of
/// the same Session (`onHealthCheck` / `onWrapServiceHealthCheck`;
/// RFC 0009 "Concurrency with `onRun`" rule 3). A record's message is
/// prefixed `[<session>] [<action>] ` with whichever are present, in that
/// order, and carries the structured fields `openjd_session_tag` and
/// `openjd_action` respectively.
///
/// ```
/// use openjd_sessions::LogTag;
///
/// assert_eq!(LogTag::default().prefix(), "");
/// assert_eq!(LogTag { session: Some("Service Files"), action: None }.prefix(), "[Service Files] ");
/// assert_eq!(
///     LogTag { session: Some("Service Files"), action: Some("onHealthCheck") }.prefix(),
///     "[Service Files] [onHealthCheck] "
/// );
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LogTag<'a> {
    /// The Session-wide tag, if any.
    pub session: Option<&'a str>,
    /// The concurrently running action's name, if any.
    pub action: Option<&'a str>,
}

impl LogTag<'_> {
    /// The message prefix these tags produce: `[<session>] [<action>] `,
    /// with the absent parts omitted; empty when both are `None`.
    #[must_use]
    pub fn prefix(&self) -> String {
        let mut p = String::new();
        if let Some(s) = self.session {
            p.push_str(&format!("[{s}] "));
        }
        if let Some(a) = self.action {
            p.push_str(&format!("[{a}] "));
        }
        p
    }

    /// True when neither tag is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.session.is_none() && self.action.is_none()
    }
}

/// Like [`session_log!`], attributing the record per `$tag` (a [`LogTag`]):
/// the message is prefixed with [`LogTag::prefix`] and the record carries
/// `openjd_session_tag = <session>` and/or `openjd_action = <action>` for
/// the tags that are set. With both `None` the record is exactly a
/// `session_log!` record.
///
/// Usage:
///   session_tagged_log!(info, session_id, tag, LogContent::COMMAND_OUTPUT, "{}", line);
#[macro_export]
macro_rules! session_tagged_log {
    ($level:ident, $session_id:expr, $tag:expr, $content:expr, $($arg:tt)+) => {
        match ($tag as $crate::logging::LogTag<'_>) {
            $crate::logging::LogTag { session: Some(s), action: Some(a) } => log::$level!(
                target: "openjd.sessions",
                session_id = $session_id,
                openjd_log_content = $crate::logging::LogContent::bits(&$content),
                openjd_timestamp_usec = $crate::logging::timestamp_usec(),
                openjd_session_tag = s,
                openjd_action = a;
                "[{}] [{}] {}", s, a, format_args!($($arg)+)
            ),
            $crate::logging::LogTag { session: Some(s), action: None } => log::$level!(
                target: "openjd.sessions",
                session_id = $session_id,
                openjd_log_content = $crate::logging::LogContent::bits(&$content),
                openjd_timestamp_usec = $crate::logging::timestamp_usec(),
                openjd_session_tag = s;
                "[{}] {}", s, format_args!($($arg)+)
            ),
            $crate::logging::LogTag { session: None, action: Some(a) } => log::$level!(
                target: "openjd.sessions",
                session_id = $session_id,
                openjd_log_content = $crate::logging::LogContent::bits(&$content),
                openjd_timestamp_usec = $crate::logging::timestamp_usec(),
                openjd_action = a;
                "[{}] {}", a, format_args!($($arg)+)
            ),
            $crate::logging::LogTag { session: None, action: None } => {
                $crate::session_log!($level, $session_id, $content, $($arg)+)
            }
        }
    };
}

/// Like [`session_log!`], attributing the record to one action of the
/// Session when `$action` (an `Option<&str>`) is `Some`: the record then
/// carries the structured field `openjd_action = <name>` and its message is
/// prefixed with `[<name>] `. With `None` the record is exactly a
/// `session_log!` record. Shorthand for
/// [`session_tagged_log!`](crate::session_tagged_log) with no
/// session tag.
///
/// This is how a Service Session satisfies RFC 0009 "Concurrency with
/// `onRun`" rule 3 (log attribution): `onHealthCheck` runs while `onRun`
/// runs, so every line of its output — and every process-control line about
/// it — is tagged, while `onRun`'s lines stay untagged.
///
/// Usage:
///   session_action_log!(info, session_id, Some("onHealthCheck"), LogContent::COMMAND_OUTPUT, "{}", line);
#[macro_export]
macro_rules! session_action_log {
    ($level:ident, $session_id:expr, $action:expr, $content:expr, $($arg:tt)+) => {
        $crate::session_tagged_log!(
            $level,
            $session_id,
            $crate::logging::LogTag { session: None, action: ($action as Option<&str>) },
            $content,
            $($arg)+
        )
    };
}

/// Log a section banner (major section separator). With a session tag
/// (see [`LogTag`]) the four-line banner becomes one tagged `BANNER` line,
/// `[<tag>] --------- <title>`, so a Session whose log is merged with
/// others' marks its sections without interleaving separator lines with
/// their output — the same reduction the runner applies to its `Phase:
/// Running action` subsection banner for a tagged action.
pub fn log_section_banner_tagged(session_id: &str, session_tag: Option<&str>, title: &str) {
    match session_tag {
        Some(tag) => session_tagged_log!(
            info,
            session_id,
            LogTag {
                session: Some(tag),
                action: None
            },
            LogContent::BANNER,
            "--------- {}",
            title
        ),
        None => log_section_banner(session_id, title),
    }
}

/// Log a one-line note about the Session's structure — a decision a reader
/// of the merged run log needs, such as an Environment skipped because its
/// `runScope` excludes the Session's kind. With a tag it is a tagged
/// `BANNER` line, `[<tag>] <text>`, so a runner that prints a tagged
/// Session's banners shows it beside the `Entering Environment` banners it
/// stands in for; without a tag it is `PROCESS_CONTROL`, like the rest of
/// the Session's bookkeeping.
pub fn log_session_note_tagged(session_id: &str, session_tag: Option<&str>, text: &str) {
    match session_tag {
        Some(tag) => session_tagged_log!(
            info,
            session_id,
            LogTag {
                session: Some(tag),
                action: None
            },
            LogContent::BANNER,
            "{}",
            text
        ),
        None => session_log!(info, session_id, LogContent::PROCESS_CONTROL, "{}", text),
    }
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
            Some("onHealthCheck"),
            LogContent::COMMAND_OUTPUT,
            "line {}",
            1
        );
        crate::session_action_log!(info, "sid", None, LogContent::COMMAND_OUTPUT, "line {}", 2);
        testing_logger::validate(|logs| {
            assert_eq!(logs.len(), 2);
            assert_eq!(logs[0].body, "[onHealthCheck] line 1");
            assert_eq!(logs[1].body, "line 2");
            assert_eq!(logs[0].target, "openjd.sessions");
        });
    }

    #[test]
    fn session_tagged_log_prefixes_session_then_action() {
        testing_logger::setup();
        let both = LogTag {
            session: Some("Service Files"),
            action: Some("onHealthCheck"),
        };
        crate::session_tagged_log!(info, "sid", both, LogContent::COMMAND_OUTPUT, "l{}", 1);
        let session_only = LogTag {
            session: Some("Service Files"),
            action: None,
        };
        crate::session_tagged_log!(
            info,
            "sid",
            session_only,
            LogContent::COMMAND_OUTPUT,
            "l{}",
            2
        );
        crate::session_tagged_log!(
            info,
            "sid",
            LogTag::default(),
            LogContent::COMMAND_OUTPUT,
            "l{}",
            3
        );
        log_section_banner_tagged("sid", Some("Service Files"), "Entering Environment: E");
        log_section_banner_tagged("sid", None, "Entering Environment: E");
        testing_logger::validate(|logs| {
            assert_eq!(logs[0].body, "[Service Files] [onHealthCheck] l1");
            assert_eq!(logs[1].body, "[Service Files] l2");
            assert_eq!(logs[2].body, "l3");
            assert_eq!(
                logs[3].body,
                "[Service Files] --------- Entering Environment: E"
            );
            // The untagged banner keeps its four lines.
            assert_eq!(logs.len(), 4 + 4);
            assert_eq!(logs[6].body, "--------- Entering Environment: E");
        });
    }
}
