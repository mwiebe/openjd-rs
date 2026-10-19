// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Action state and result types.

/// State of an action execution.
///
/// ```
/// use openjd_sessions::ActionState;
///
/// let state = ActionState::Success;
/// assert_eq!(state, ActionState::Success);
/// assert_ne!(state, ActionState::Failed);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionState {
    Running,
    Success,
    Failed,
    Canceled,
    Timeout,
}

impl std::fmt::Display for ActionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => write!(f, "Running"),
            Self::Success => write!(f, "Success"),
            Self::Failed => write!(f, "Failed"),
            Self::Canceled => write!(f, "Canceled"),
            Self::Timeout => write!(f, "Timeout"),
        }
    }
}

/// A parsed openjd stdout message from a running action.
#[derive(Debug, Clone)]
pub enum ActionMessage {
    /// `openjd_progress: <number>`
    Progress(f64),
    /// `openjd_status: <message>`
    Status(String),
    /// `openjd_fail: <message>`
    Fail(String),
    /// `openjd_env: <var>=<value>`
    SetEnv { name: String, value: String },
    /// `openjd_unset_env: <var>`
    UnsetEnv { name: String },
    /// `openjd_redacted_env: <var>=<value>`
    RedactedEnv { name: String, value: String },
    /// `openjd_output_var: <name>=<value>` (RFC 0010, `CONTROL_FLOW`): a value
    /// the output collector accepted. `task` is the Task's `CHUNK[INT]` value
    /// in a chunked Step, `None` otherwise. Under `collect: APPEND`, `value`
    /// is the new element. The value is not committed until the Action
    /// succeeds.
    OutputVar {
        task: Option<i64>,
        name: String,
        value: openjd_expr::ExprValue,
    },
    /// Request to cancel the action and mark it as failed (from a malformed
    /// env command or an invalid `openjd_output_var` directive), with the
    /// message to fail it with.
    CancelMarkFailed { fail_message: String },
}

impl std::fmt::Display for ActionMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Progress(v) => write!(f, "Progress({v})"),
            Self::Status(s) => write!(f, "Status({s})"),
            Self::Fail(s) => write!(f, "Fail({s})"),
            Self::SetEnv { name, .. } => write!(f, "SetEnv({name})"),
            Self::UnsetEnv { name } => write!(f, "UnsetEnv({name})"),
            Self::RedactedEnv { name, .. } => write!(f, "RedactedEnv({name})"),
            Self::OutputVar {
                task: Some(t),
                name,
                ..
            } => write!(f, "OutputVar({name}[{t}])"),
            Self::OutputVar {
                task: None, name, ..
            } => write!(f, "OutputVar({name})"),
            Self::CancelMarkFailed { fail_message } => {
                write!(f, "CancelMarkFailed({fail_message})")
            }
        }
    }
}

/// The output values of an Action (RFC 0010 `CONTROL_FLOW`): each Task's
/// values, keyed by the Task's `CHUNK[INT]` value in a chunked Step (in the
/// chunk's order) and by `None` otherwise.
pub type ActionOutputs = indexmap::IndexMap<Option<i64>, openjd_model::outputs::TaskOutputs>;

/// Result of running an action.
#[derive(Debug)]
pub struct ActionResult {
    pub state: ActionState,
    pub exit_code: Option<i32>,
    pub stdout: String,
    /// The output values the Action reported, when the Step declares output
    /// variables and the Action succeeded; `None` otherwise. Keyed by
    /// `CHUNK[INT]` value, not position, because a chunk's values are sorted
    /// while the Step's Tasks iterate in a list range's written order. A
    /// `PATH` value is in the submission path context.
    pub outputs: Option<ActionOutputs>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_state_display() {
        assert_eq!(ActionState::Running.to_string(), "Running");
        assert_eq!(ActionState::Success.to_string(), "Success");
        assert_eq!(ActionState::Failed.to_string(), "Failed");
        assert_eq!(ActionState::Canceled.to_string(), "Canceled");
        assert_eq!(ActionState::Timeout.to_string(), "Timeout");
    }

    #[test]
    fn action_message_display() {
        assert_eq!(ActionMessage::Progress(50.0).to_string(), "Progress(50)");
        assert_eq!(ActionMessage::Status("ok".into()).to_string(), "Status(ok)");
        assert_eq!(ActionMessage::Fail("err".into()).to_string(), "Fail(err)");
        assert_eq!(
            ActionMessage::SetEnv {
                name: "K".into(),
                value: "V".into()
            }
            .to_string(),
            "SetEnv(K)"
        );
        assert_eq!(
            ActionMessage::UnsetEnv { name: "K".into() }.to_string(),
            "UnsetEnv(K)"
        );
        assert_eq!(
            ActionMessage::RedactedEnv {
                name: "K".into(),
                value: "V".into()
            }
            .to_string(),
            "RedactedEnv(K)"
        );
        assert_eq!(
            ActionMessage::CancelMarkFailed {
                fail_message: "bad".into()
            }
            .to_string(),
            "CancelMarkFailed(bad)"
        );
        assert_eq!(
            ActionMessage::OutputVar {
                task: None,
                name: "N".into(),
                value: openjd_expr::ExprValue::Int(1)
            }
            .to_string(),
            "OutputVar(N)"
        );
        assert_eq!(
            ActionMessage::OutputVar {
                task: Some(-4),
                name: "N".into(),
                value: openjd_expr::ExprValue::Int(1)
            }
            .to_string(),
            "OutputVar(N[-4])"
        );
    }
}
