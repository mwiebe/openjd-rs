# Real-Time Action Message Streaming

## Problem

The OpenJD specification defines `openjd_*` stdout messages that actions emit during
execution to convey progress, status, environment variable changes, and failure reasons
to the runtime. The spec says these are intercepted to "convey information about the
Action to the render management system" — implying real-time delivery, not batch.

## ActionMessage

```rust
#[derive(Debug, Clone)]
pub enum ActionMessage {
    Progress(f64),
    Status(String),
    Fail(String),
    SetEnv { name: String, value: String },
    UnsetEnv { name: String },
    RedactedEnv { name: String, value: String },
    /// CONTROL_FLOW: a value the action's output collector accepted.
    OutputVar { task: Option<i64>, name: String, value: ExprValue },
    CancelMarkFailed { fail_message: String },
}
```

### Why an enum instead of a trait or callback

The Python library uses a `logging.Filter` that mutates session state directly from
within the filter callback. This works in Python (GIL protects concurrent access) but
would require `Arc<Mutex<Session>>` in Rust.

An enum sent through a channel decouples the producer (subprocess stdout loop) from the
consumer (session state machine). The session processes messages with `&mut self`, no
locks needed.

### Why CancelMarkFailed carries a fail_message

When the `ActionFilter` detects a malformed `openjd_*` directive (e.g., wrong case or
missing space after colon), it emits `CancelMarkFailed` to cancel the action and mark
it as failed. The `fail_message` field provides context about what was malformed, which
the session stores as `action_fail_message` and reports via the callback. Without this
field, the consumer would only know the action was canceled but not why.

The env-directive path sends a fixed "Action canceled due to malformed command". An invalid
`openjd_output_var` directive sends the output collector's message instead, because RFC
0010 requires the failure to name the output, the problem, and the offending text, and
applying `CancelMarkFailed` overwrites any earlier `fail_message`.

### Why OutputVar is a message and not an ActionStatus field

RFC 0010 §Committing values lets a runtime forward each value as soon as it is accepted.
`OutputVar` carries one accepted value (under `collect: APPEND`, the new element) with the
Task's `CHUNK[INT]` value in a chunked Step. The Session passes it to
`SessionConfig::output_callback` and changes no `ActionStatus` field, so the status
callback does not fire for it. Putting the values on `ActionStatus` would re-clone a
growing `APPEND` list on every status callback, because `action_status()` builds a new
struct each time. The values are not committed: the authoritative set is
`ActionResult::outputs` when the action succeeds.

## Channel Architecture

```
┌──────────────┐     ActionMessage      ┌─────────────────┐
│  subprocess  │ ──── mpsc channel ───► │ Session          │
│  stdout loop │                        │ drive_action()   │
└──────────────┘                        └─────────────────┘
       │                                     │
       │ ActionFilter                        ├─ update progress/status
       │ parses lines                        ├─ apply env var changes
       │                                     ├─ trigger cancel on Fail
       │                                     └─ invoke user callback
```

### Why unbounded channel

`tokio::sync::mpsc::unbounded_channel` is used instead of a bounded channel because:

1. Backpressure on stdout would stall the subprocess, potentially causing deadlocks
   if the subprocess is waiting for stdout to drain before exiting
2. The message rate is bounded by subprocess stdout throughput (one message per line),
   which is inherently limited by I/O
3. The consumer (`drive_action`) processes messages as fast as they arrive — there's
   no slow consumer problem

A bounded channel would add complexity (choosing a buffer size, handling `SendError`)
without meaningful benefit.

## Session::drive_action

The core async loop that runs an action to completion while processing messages:

```rust
async fn drive_action(
    &mut self,
    action_future: impl Future<Output = Result<SubprocessResult, SessionError>>,
    mut message_rx: mpsc::UnboundedReceiver<ActionMessage>,
) -> ActionResult {
    tokio::pin!(action_future);
    loop {
        tokio::select! {
            result = &mut action_future => {
                // Subprocess exited — drain remaining messages
                while let Ok(msg) = message_rx.try_recv() {
                    self.apply_message(msg);
                }
                return self.finalize_action(result);
            }
            Some(msg) = message_rx.recv() => {
                self.apply_message(msg);
            }
        }
    }
}
```

### Why this concurrency model works without locks

The subprocess future and the message receiver run in the same `async` task via
`tokio::select!`. Only one branch executes at a time (cooperative scheduling), so
`&mut self` is safe. The subprocess sends messages through the channel — it never
touches session state directly.

This is fundamentally different from the Python approach where the `ActionMonitoringFilter`
runs on a daemon thread and mutates session state through the GIL. The Rust approach
eliminates all shared mutable state.

### Why drain after subprocess exit

When the subprocess exits, there may be messages in the channel that were sent but not
yet received (the channel has internal buffering). Draining ensures no messages are lost,
particularly `SetEnv`/`UnsetEnv` messages that affect subsequent actions.

## Callback Invocation

After each `apply_message`, the user callback is invoked with the current `ActionStatus`:

```rust
fn apply_message(&mut self, msg: ActionMessage) {
    match msg {
        ActionMessage::Progress(v) => self.action_status.progress = Some(v),
        ActionMessage::Status(s) => self.action_status.status_message = Some(s),
        // ... etc
    }
    if let Some(ref callback) = self.callback {
        callback(&self.session_id, &self.action_status);
    }
}
```

This matches the Python library's behavior where the callback fires on every directive.
The worker agent uses this to forward real-time progress to the Deadline service.
