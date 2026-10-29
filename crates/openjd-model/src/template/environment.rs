// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Environment types per spec §4.

use super::actions::EnvironmentActions;
use super::constrained_strings::Description;
use super::env_parameters::EnvironmentParameterDefinition;
use super::expr_field::ExprField;
use super::host_requirements::HostRequirements;
use crate::format_string::FormatString;
use crate::types::{EndOfLine, FileType};
use serde::Deserialize;
use std::collections::HashMap;

/// §4 Environment
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub struct Environment {
    pub name: String,
    pub description: Option<Description>,
    /// §4 item 3 `envParameterDefinitions` (`ENV_PARAMS` extension): the
    /// parameters a Step may supply through an `environment:` entry of its
    /// `dependencies`. Template validation rejects the field without
    /// `ENV_PARAMS` and on a `stepEnvironments` entry.
    pub env_parameter_definitions: Option<Vec<EnvironmentParameterDefinition>>,
    /// §4 item 4 `activation` (`ENV_PARAMS` extension): which Sessions enter
    /// this Environment. Absent means [`EnvironmentActivation::Always`]; see
    /// [`activation`](Self::activation). Template validation rejects the
    /// field without `ENV_PARAMS` and on a `stepEnvironments` entry.
    pub activation: Option<EnvironmentActivation>,
    /// §4 item 5 `hostRequirements` (`ENV_PARAMS` extension): requirements
    /// on the Worker Host that every Session entering this Environment must
    /// satisfy, in addition to the Step's. The Step's type and constraints.
    /// Its format strings are resolved at job creation once for each Step
    /// whose Sessions enter the Environment, with that Step's effective
    /// values, and combined with the Step's own (§3.3.3); see
    /// [`job::Step::host_requirements`](crate::job::Step::host_requirements).
    /// Template validation rejects the field without `ENV_PARAMS` and on a
    /// `stepEnvironments` entry.
    pub host_requirements: Option<HostRequirements>,
    /// §4 item 6 `enableIf` (`ENV_PARAMS`): an expression field
    /// of type `bool` (§7.5) deciding whether a Step's Sessions enter this
    /// Environment. Permitted on every position: a `jobEnvironments` entry,
    /// a `stepEnvironments` entry, and an Environment Template's
    /// `environment`. Template validation checks it in the scope §4 item 6
    /// constraints 1–3 give: for a Job Environment or an Environment
    /// Template, exactly its `hostRequirements` scope; for a Step
    /// Environment, the Step's template scope. Job creation resolves it once
    /// for each Step whose Sessions would otherwise enter the Environment,
    /// with that Step's effective values, and a Step for which it is `false`
    /// does not enter it; see [`literal_enabled`](Self::literal_enabled).
    #[serde(default, deserialize_with = "super::expr_field::deserialize_enable_if")]
    pub enable_if: Option<ExprField>,
    pub script: Option<EnvironmentScript>,
    pub variables: Option<HashMap<String, FormatString>>,
}

impl Environment {
    /// `activation` with its default applied (§4 item 4: absent means
    /// `ALWAYS`).
    #[must_use]
    pub fn activation(&self) -> EnvironmentActivation {
        self.activation.unwrap_or_default()
    }

    /// Whether `enableIf` is known before the Job is created (§4 item 6
    /// tail, §7.5 item 1): `Some(true)` when it is absent or the boolean
    /// scalar `true`, `Some(false)` for the scalar `false`, and `None`
    /// otherwise, an expression included (even a constant one such as
    /// `"true"`, which is evaluated when the Job is created).
    #[must_use]
    pub fn literal_enabled(&self) -> Option<bool> {
        match &self.enable_if {
            None => Some(true),
            Some(field) => field.as_bool(),
        }
    }
}

/// §4 item 4 `<EnvironmentActivation>` (`ENV_PARAMS` extension): which Sessions of a
/// Job enter an Environment. An unrecognized value is rejected at decode
/// (constraint 1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EnvironmentActivation {
    /// `ALWAYS`: every Session of the Job enters the Environment, in its
    /// position in the Job's Environment order. The default, and the
    /// behavior of every Environment without the extension.
    #[default]
    Always,
    /// `ON_REQUEST`: only the Sessions of a Step that requests the
    /// Environment through an enabled `environment:` entry of its
    /// `dependencies` enter it.
    OnRequest,
}

impl EnvironmentActivation {
    /// The value as a template writes it: `ALWAYS` or `ON_REQUEST`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Always => "ALWAYS",
            Self::OnRequest => "ON_REQUEST",
        }
    }
}

/// §4.1 EnvironmentScript
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
pub struct EnvironmentScript {
    #[serde(rename = "let")]
    pub let_bindings: Option<Vec<String>>,
    pub actions: EnvironmentActions,
    pub embedded_files: Option<Vec<EmbeddedFile>>,
}

/// §6 EmbeddedFile
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(
    missing_docs,
    reason = "missing-docs burn-down: predates the lint; document these items, then delete this attribute"
)]
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
