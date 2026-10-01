# Structured Logging

## Overview

`logging.rs` provides structured logging for the sessions crate using the `log` crate's
`kv` feature. Every log record carries structured key-value metadata (`session_id` and
`openjd_log_content`) that consumers use to route and filter log output.

## LogContent

```rust
bitflags! {
    pub struct LogContent: u32 {
        const BANNER          = 0b0000_0001;
        const FILE_PATH       = 0b0000_0010;
        const FILE_CONTENTS   = 0b0000_0100;
        const COMMAND_OUTPUT  = 0b0000_1000;
        const EXCEPTION_INFO  = 0b0001_0000;
        const PROCESS_CONTROL = 0b0010_0000;
        const PARAMETER_INFO  = 0b0100_0000;
        const HOST_INFO       = 0b1000_0000;
    }
}
```

### Why bitflags

The Python library uses `enum.Flag` for `LogContent`, which supports bitwise OR for
combining categories. The `bitflags` crate provides the same semantics in Rust. A log
record can have multiple content categories (e.g., `BANNER | PROCESS_CONTROL`).

The worker agent filters log records by `LogContent` to decide routing:
- `COMMAND_OUTPUT` → CloudWatch (customer-visible)
- `PROCESS_CONTROL` → worker agent logs (operational)
- `HOST_INFO` → session initialization logs

## session_log! Macro

```rust
macro_rules! session_log {
    ($level:ident, $session_id:expr, $content:expr, $($arg:tt)*) => {
        log::$level!(
            session_id = $session_id,
            openjd_log_content = $content.bits();
            $($arg)*
        );
    };
}
```

### Why a macro instead of a function

The `log` crate's macros (`log::info!`, etc.) capture the caller's module path and line
number. Wrapping them in a function would report the logging module's location instead
of the actual call site. A macro preserves the correct source location.

The `kv` feature syntax (`key = value;`) is only available in the `log` macros, not
through a programmatic API, which further necessitates a macro wrapper.

## session_action_log! Macro — per-action attribution (RFC 0009)

```rust
macro_rules! session_action_log {
    ($level:ident, $session_id:expr, $action:expr, $content:expr, $($arg:tt)+) => {
        match ($action as Option<&str>) {
            Some(action) => log::$level!(
                target: "openjd.sessions",
                session_id = $session_id,
                openjd_log_content = $content.bits(),
                openjd_timestamp_usec = timestamp_usec(),
                openjd_action = action;
                "[{}] {}", action, format_args!($($arg)+)
            ),
            None => session_log!($level, $session_id, $content, $($arg)+),
        }
    };
}
```

A Service Session's `onReadinessCheck` runs *while* `onRun` runs, so banners
no longer attribute output lines to the action that produced them. RFC 0009
"Concurrency with `onRun`" rule 3 requires every captured stdout/stderr line
of a Service Session to be attributable to its action, and describes two
implementation forms: a structured field, or a `[<action>] ` text prefix.
`session_action_log!` emits both at once when `$action` is `Some`:

- the structured field `openjd_action = "<action name>"` (alongside
  `session_id`, `openjd_log_content`, `openjd_timestamp_usec`), for
  consumers that read the record's key-values;
- the prefix `[<action name>] ` on the message, for the single plain-text
  log.

With `$action == None` it is exactly `session_log!`, so the untagged path is
unchanged. `session_action_log!` is shorthand for `session_tagged_log!` (below)
with no session tag.

Who sets the tag: `ScriptRunnerBase::action_tag` (set by the Service Session
to `"onReadinessCheck"`, or `"onWrapServiceReadinessCheck"` when wrapped) is
copied into `ActionFilter::action_tag` for the action's run, and
`subprocess::run_subprocess` / `cross_user_helper::run_via_helper` emit every
record about the action — `Running command …`, `Command started as pid`,
`Output:`, each `COMMAND_OUTPUT` line, cancel and timeout notices, `Process
exit code` — through `session_tagged_log!` with that tag. The runner's
`Phase: Running action` subsection banner becomes a single tagged
`PROCESS_CONTROL` line for a tagged action. `onRun`, `onEnter`, `onExit`,
Environment actions and Tasks have no action tag and log exactly as before
(unless the Session has a session tag, next).

## LogTag and session_tagged_log! — session-wide attribution

```rust
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LogTag<'a> {
    pub session: Option<&'a str>,   // SessionConfig::log_tag
    pub action: Option<&'a str>,    // the concurrently running action (rule 3)
}
impl LogTag<'_> {
    pub fn prefix(&self) -> String;   // "[<session>] [<action>] ", absent parts omitted
    pub fn is_empty(&self) -> bool;
}

macro_rules! session_tagged_log {
    ($level:ident, $session_id:expr, $tag:expr, $content:expr, $($arg:tt)+) => { … };
}
```

A single-host runner that merges several Sessions' logs into one stream —
`openjd run`, whose Job's Service Sessions log alongside the Task Session —
loses attribution once a Service's `onRun` output interleaves with Task
output (the RFC's rule-3 assumption, "`onRun` is the only stream present",
does not hold there). `SessionConfig::log_tag: Option<String>` is the
Session-wide answer: the runner sets it (the CLI to `Service <name>`, with
`(from <document>)` for an external Service), the Session stores it and
copies it onto every runner it builds (`EnvironmentScriptRunner`,
`StepScriptRunner`, `new_runner_base`, `new_detached_runner_base`:
`ScriptRunnerBase::session_tag`) and onto the `ActionFilter` of each action
(`set_session_tag`), and `session_tagged_log!` emits, for the tags that are
set:

- the structured fields `openjd_session_tag = "<tag>"` and/or
  `openjd_action = "<action>"`;
- the message prefix `[<session>] [<action>] ` in that order — the session
  tag first, so a Service's concurrent check reads `[Service Files]
  [onReadinessCheck] CHECK_OK` and its `onRun` `[Service Files] line`. The
  action tag's meaning (rule 3) is unchanged; the session tag is purely
  additive.

With both `None` it is exactly `session_log!`. The `log_tag` also changes the
shape of the Session's **section banners**: `Session::log_banner(title)` calls
`log_section_banner_tagged(session_id, log_tag, title)`, which with a tag
emits one `BANNER` line, `[<tag>] --------- <title>`, instead of the four
lines below — separator lines interleaved with another Session's output
would be noise, and this is the same reduction the runner applies to its
`Phase: Running action` subsection banner for a tagged action. All of the
Session's section banners go through it (`Entering Environment: …`,
`Exiting Environment: …`, `Running Task`, `Session Cleanup`, and the Service
Session's `Starting Service: …`, `Service onEnter: …`, `Service onRun: …
(launch N)`, `Ending Service: …`, `Service onExit: …`). A consumer that merges
streams can therefore show a tagged Session's banners (the CLI does, for the
Service Sessions) while still printing its own for the untagged Session.
Redaction is applied to a line before any prefix is added. `log_tag: None`
(the default) leaves every record exactly as before.
Redaction is applied to the line before the prefix is added, so the prefix
never hides or splits a redacted value.

## Banner Helpers

```rust
pub fn log_section_banner(session_id: &str, title: &str);
pub fn log_section_banner_tagged(session_id: &str, session_tag: Option<&str>, title: &str);
pub fn log_subsection_banner(session_id: &str, title: &str);
```

`log_section_banner_tagged` with `Some(tag)` emits the single tagged line
described above; with `None` it is `log_section_banner`.

Emit formatted banner lines matching the Python library's output:

### Section banner format

```
<blank line>
==============================================
--------- {title}
==============================================
```

The separator line is exactly 46 `=` characters. The title line is prefixed with
`--------- ` (9 dashes + space). Each line is emitted as a separate `session_log!`
call with `LogContent::BANNER`.

### Subsection banner format

```
----------------------------------------------
{title}
----------------------------------------------
```

The separator line is exactly 46 `-` characters. The title line has no prefix.

**Stability note**: The banner format (separator length, prefix characters) is not
part of the public API contract. Consumers should not parse banner lines to extract
structured information — use the `openjd_log_content` key-value metadata instead.

These are used at session lifecycle boundaries (enter environment, run task, cleanup)
to provide visual structure in log output.

## Logging Coverage

| Module | Content Type | What's Logged |
|--------|-------------|---------------|
| `session.rs` | `HOST_INFO` | Version, platform, architecture at init |
| `session.rs` | `FILE_PATH` | Working directory, files directory paths |
| `session.rs` | `BANNER` | Section banners for enter/exit/run/cleanup |
| `subprocess.rs` | `PROCESS_CONTROL` | PID start, SIGTERM, SIGKILL, exit code, spawn failures |
| `subprocess.rs` | `COMMAND_OUTPUT` | Stdout/stderr lines from the subprocess |
| `subprocess.rs` | `BANNER` | Output header banner |
| `runner/env_script.rs` | `BANNER` | Subsection banner before action execution |
| `runner/step_script.rs` | `BANNER` | Subsection banner before action execution |
| `embedded_files.rs` | `FILE_PATH` | File write paths |
| `embedded_files.rs` | `FILE_CONTENTS` | File data content (debug level only) |
| `action_filter.rs` | `COMMAND_OUTPUT` (WARN) | `Received openjd_redacted_env for 'NAME' but the REDACTED_ENV_VARS extension is not declared; the variable is not set.` — see below |

## Log levels and classification

The `log` level of a record and its `LogContent` are independent axes, as in Python
(`LOG.warning(..., extra=LogExtraInfo(openjd_log_content=...))`): the level says how
serious the record is, the content says *what it is about* and therefore where a
consumer routes it. Almost every record of the crate is `info`. The exceptions, and
how they are classified:

- **Operational warnings** about the runtime itself — the sticky-bit warning in
  `Session::with_config`, a cross-user helper dropped without `shutdown()`, a
  `CTRL_BREAK` / delayed-terminate fallback in `subprocess.rs`, the
  `redactions_enabled = true` malformed `openjd_redacted_env` notice — are plain
  `log::warn!` records with **no** `openjd_log_content`. They are for whoever runs the
  runtime, not the template author, and a consumer that routes by `LogContent` leaves
  them out of the action's output (the CLI's `SessionLogger` drops them).
- **Warnings addressed to the template author** about *their action's output* carry
  `LogContent::COMMAND_OUTPUT` at level `warn`, through `session_tagged_log!` so they
  carry the action's `session_id`, `openjd_timestamp_usec` and session / action tags
  like every other line of the action. Today there is one: the `ActionFilter`'s
  `Received openjd_redacted_env for 'NAME' but the REDACTED_ENV_VARS extension is not
  declared; the variable is not set.` (see
  [action-filter.md](action-filter.md#when-the-document-does-not-declare-redacted_env_vars)),
  which matches the classification of Python's `Received openjd_redacted_env message but
  REDACTED_ENV_VARS extension is not enabled` warning. Routing it with the command output
  puts it next to the redacted `NAME=********` line it explains, in the one stream the
  author reads. It is subject to the action's `openjd_session_runtime_loglevel` like
  the output it accompanies (suppressed above `WARNING`); the `info`-level output lines
  are suppressed above `INFO`.

A consumer must therefore not assume `COMMAND_OUTPUT` implies `info`, nor that `warn`
implies "not the action's output": route by `openjd_log_content`, and read the level
for severity.

## Consumer Integration

Log consumers (e.g., the worker agent) inspect the `openjd_log_content` key-value pair
on each log record to determine routing. The value is a `u32` bitfield that can be
decoded back to `LogContent` flags:

```rust
// In the consumer's log handler:
if let Some(content) = record.key_values().get("openjd_log_content") {
    let flags = LogContent::from_bits_truncate(content.to_u64().unwrap() as u32);
    if flags.contains(LogContent::COMMAND_OUTPUT) {
        // Route to CloudWatch
    }
}
```

This structured approach avoids parsing log message text to determine content type,
which would be fragile and slow.
