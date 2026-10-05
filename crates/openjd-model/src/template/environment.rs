// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Environment types per spec §4.

use super::actions::EnvironmentActions;
use super::constrained_strings::Description;
use crate::format_string::FormatString;
use crate::types::{EndOfLine, FileType};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

/// §4 Environment
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Environment {
    pub name: String,
    pub description: Option<Description>,
    /// §4 item 3 (RFC 0009) — the kinds of Session this Environment is
    /// entered in, as `<RunScopeName>`s. Requires the `SERVICE` extension.
    /// `None` means the default: `[TASK]` when the Environment references a
    /// `Service.*` value, every kind of Session otherwise. Held as plain
    /// strings so that an unrecognized name is reported with a field path by
    /// template validation rather than as a serde error; use
    /// [`runs_in`](Self::runs_in) to query the effective scope.
    pub run_scope: Option<Vec<String>>,
    pub script: Option<EnvironmentScript>,
    pub variables: Option<HashMap<String, FormatString>>,
}

impl Environment {
    /// True iff this Environment is entered in Sessions of kind `kind` (§4
    /// item 3, RFC 0009): exactly the kinds the list names when `runScope`
    /// is given; otherwise the default, which follows the Environment's own
    /// text — `[TASK]` when any of its format strings references a
    /// `Service.*` value (see [`references_service`](Self::references_service)),
    /// every kind otherwise. Unrecognized names, which template validation
    /// rejects, never match.
    pub fn runs_in(&self, kind: RunScope) -> bool {
        match &self.run_scope {
            None => !self.default_run_scope_is_task_only() || kind == RunScope::Task,
            Some(names) => names.iter().any(|n| n == kind.as_str()),
        }
    }

    /// The kinds of Session this Environment is entered in, in
    /// [`RunScope::ALL`] order: the effective `runScope` (§4 item 3).
    pub fn effective_run_scope(&self) -> impl Iterator<Item = RunScope> + '_ {
        RunScope::ALL
            .iter()
            .copied()
            .filter(move |kind| self.runs_in(*kind))
    }

    /// True when any format string of this Environment (`variables`,
    /// actions, embedded files, script `let`) references a `Service.*`
    /// value — the condition that makes an absent `runScope` default to
    /// `[TASK]` (§4 item 3).
    pub fn references_service(&self) -> bool {
        super::service_scope::environment_references_service(self)
    }

    /// True when `runScope` is absent and defaults to `[TASK]` because the
    /// Environment references `Service.*`.
    pub fn default_run_scope_is_task_only(&self) -> bool {
        self.run_scope.is_none() && self.references_service()
    }
}

/// §4 item 3 `<RunScopeName>` (RFC 0009): a kind of Session. Every Session
/// has exactly one kind, and an Environment is entered in a Session iff that
/// kind is in the Environment's effective `runScope`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RunScope {
    /// Sessions that run Tasks — the only kind that exists without the
    /// `SERVICE` extension.
    Task,
    /// Service Sessions, in which a Service's actions run.
    Service,
}

impl RunScope {
    /// Every recognized `<RunScopeName>`, in schema order.
    pub const ALL: [RunScope; 2] = [RunScope::Task, RunScope::Service];

    /// The schema spelling of this name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Task => "TASK",
            Self::Service => "SERVICE",
        }
    }
}

impl fmt::Display for RunScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RunScope {
    type Err = String;

    /// Parse a schema spelling (`TASK`, `SERVICE`); the error names the
    /// unrecognized value.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|kind| kind.as_str() == s)
            .ok_or_else(|| format!("unknown run scope name '{s}'"))
    }
}

/// §4.1 EnvironmentScript
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentScript {
    #[serde(rename = "let")]
    pub let_bindings: Option<Vec<String>>,
    pub actions: EnvironmentActions,
    pub embedded_files: Option<Vec<EmbeddedFile>>,
}

/// §6 EmbeddedFile
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmbeddedFile {
    pub name: String,
    #[serde(rename = "type")]
    pub file_type: FileType,
    pub filename: Option<String>,
    pub data: Option<FormatString>,
    pub runnable: Option<bool>,
    #[serde(rename = "endOfLine")]
    pub end_of_line: Option<EndOfLine>,
}

#[cfg(test)]
mod tests {
    //! The decode + validate surface is covered by
    //! `tests/integration/test_service_environments.rs`; these pin the
    //! accessor semantics on bare structs.

    use super::*;

    fn env(run_scope: Option<&[&str]>) -> Environment {
        Environment {
            name: "E".into(),
            description: None,
            run_scope: run_scope.map(|names| names.iter().map(|s| s.to_string()).collect()),
            script: None,
            variables: None,
        }
    }

    #[test]
    fn runs_in_defaults_to_every_kind() {
        let e = env(None);
        assert!(e.runs_in(RunScope::Task));
        assert!(e.runs_in(RunScope::Service));
        assert_eq!(e.effective_run_scope().count(), 2);
        assert!(!e.references_service());
        assert!(!e.default_run_scope_is_task_only());
    }

    #[test]
    fn default_follows_a_service_reference() {
        let mut e = env(None);
        e.variables = Some(
            [(
                "ADDR".to_string(),
                FormatString::new("{{ Service.Cache.main.connectAddress }}").unwrap(),
            )]
            .into_iter()
            .collect(),
        );
        assert!(e.references_service());
        assert!(e.default_run_scope_is_task_only());
        assert!(e.runs_in(RunScope::Task));
        assert!(!e.runs_in(RunScope::Service));
        assert_eq!(
            e.effective_run_scope().collect::<Vec<_>>(),
            vec![RunScope::Task]
        );
        // An explicit list is exhaustive and honored even with a reference
        // (validation rejects SERVICE here; the accessor just reports it).
        e.run_scope = Some(vec!["SERVICE".to_string()]);
        assert!(e.runs_in(RunScope::Service));
        assert!(!e.default_run_scope_is_task_only());
    }

    #[test]
    fn runs_in_is_exhaustive_and_ignores_unknown_names() {
        let e = env(Some(&["SERVICE", "WORKER"]));
        assert!(!e.runs_in(RunScope::Task));
        assert!(e.runs_in(RunScope::Service));
        assert_eq!(
            e.effective_run_scope().collect::<Vec<_>>(),
            vec![RunScope::Service]
        );
        // An empty (invalid) list is entered nowhere rather than everywhere.
        assert_eq!(env(Some(&[])).effective_run_scope().count(), 0);
    }

    #[test]
    fn run_scope_parse_round_trip() {
        for kind in RunScope::ALL {
            assert_eq!(kind.as_str().parse::<RunScope>().unwrap(), kind);
            assert_eq!(kind.to_string(), kind.as_str());
        }
        assert_eq!(
            "Task".parse::<RunScope>().unwrap_err(),
            "unknown run scope name 'Task'"
        );
    }
}
