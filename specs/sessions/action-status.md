# Action Status

## Purpose

`ActionStatus` is a snapshot of the current or most recently completed action's
state. It is the primary data structure passed to the user callback after every
`openjd_*` directive and at action start/completion, enabling real-time monitoring
of action progress by the worker agent.

## Struct

```rust
#[derive(Debug, Clone)]
pub struct ActionStatus {
    pub state: ActionState,
    pub progress: Option<f64>,
    pub status_message: Option<String>,
    pub fail_message: Option<String>,
    pub exit_code: Option<i32>,
    pub started_at: Option<SystemTime>,
    pub ended_at: Option<SystemTime>,
}
```

## Fields

| Field | Type | Set by | Description |
|---|---|---|---|
| `state` | `ActionState` | Session | Current lifecycle state of the action |
| `progress` | `Option<f64>` | `openjd_progress` | 0.0–100.0 progress percentage |
| `status_message` | `Option<String>` | `openjd_status` | Human-readable status text |
| `fail_message` | `Option<String>` | `openjd_fail` / `CancelMarkFailed` | Reason for failure |
| `exit_code` | `Option<i32>` | Subprocess exit | Process exit code, `None` while running |
| `started_at` | `Option<SystemTime>` | Session | When the action began |
| `ended_at` | `Option<SystemTime>` | Session | When the action completed |

## ActionState

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionState {
    Running,
    Success,
    Failed,
    Canceled,
    Timeout,
}
```

Terminal states are `Success`, `Failed`, `Canceled`, and `Timeout`. Only `Running`
is non-terminal.

## Lifecycle

1. **Action starts**: Session sets `state = Running`, `started_at = now()`,
   all other fields `None`. Callback fires.

2. **Directives arrive**: `apply_message` updates the relevant field and fires
   the callback after each message:
   - `Progress(v)` → `progress = Some(v)`
   - `Status(s)` → `status_message = Some(s)`
   - `Fail(s)` → `fail_message = Some(s)`, unless an `InvalidOutputVar` already
     gave the reason
   - `CancelMarkFailed { fail_message }` → `fail_message = Some(fail_message)`
     (same exception), triggers cancellation
   - `InvalidOutputVar { fail_message }` → as `CancelMarkFailed`, and the message
     is final for the action: a later `Fail` (such as an `openjd_fail` printed
     by a cancel handler) or `CancelMarkFailed` does not replace it, because
     RFC 0010 requires the failure of an invalid `openjd_output_var` directive
     to name the output, the problem, and the text. The flag resets with each
     action.
   - `OutputVar { .. }` changes no field and does not fire this callback; it
     goes to `SessionConfig::output_callback` instead (see
     [action-messages.md](action-messages.md))

3. **Action ends**: Session sets `state` to the terminal state, `exit_code` from
   the subprocess, `ended_at = now()`. Callback fires. For a Task whose Step
   declares output variables (`CONTROL_FLOW`), the runner checks the outputs are
   complete before this; a missing output sends `Fail` with its message and the
   terminal state is `Failed`, so the callback never reports `Success` first.

## Reset between actions

`progress`, `status_message`, and `fail_message` are reset to `None` at the start
of each new action. This prevents stale values from a previous action leaking into
the next one's callback invocations.

## Callback signature

```rust
pub type SessionCallbackType = Box<dyn Fn(&str, &ActionStatus) + Send + Sync>;
```

The callback receives the session ID and a reference to the current `ActionStatus`.
It is invoked synchronously within `apply_message` and at action start/end. The
callback must not block — it should enqueue work rather than perform I/O directly.

## Why Option fields instead of defaults

Using `Option<f64>` for progress (rather than defaulting to 0.0) distinguishes
"no progress reported" from "0% progress". The worker agent uses this distinction
to decide whether to display a progress bar. Similarly, `Option<String>` for
messages distinguishes "no message" from "empty message".
