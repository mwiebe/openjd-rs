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
    /// this Environment. Absent means [`EnvironmentActivation::ByDefault`] in
    /// a document that lists `ENV_PARAMS`; see [`activation`](Self::activation).
    /// Template validation rejects the field without `ENV_PARAMS` and on a
    /// `stepEnvironments` entry.
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
    /// of type `bool` (§7.6) deciding whether a Step's Sessions enter this
    /// Environment. Permitted on every position: a `jobEnvironments` entry,
    /// a `stepEnvironments` entry, and an Environment Template's
    /// `environment`. Template validation checks it in the scope §7.3.2 gives:
    /// for a Job Environment or an Environment Template, exactly its
    /// `hostRequirements` scope; for a Step
    /// Environment, the Step's template scope. Job creation resolves it once
    /// for each Step whose Sessions would otherwise enter the Environment
    /// and for which no earlier Environment of its `exclusiveGroup`
    /// supersedes it, with that Step's effective values, and a Step for
    /// which it is `false` does not enter it; see
    /// [`literal_enabled`](Self::literal_enabled).
    #[serde(default, deserialize_with = "super::expr_field::deserialize_enable_if")]
    pub enable_if: Option<ExprField>,
    /// §4 item 7 `exclusiveGroup` (`ENV_PARAMS` extension): a name shared
    /// by Environments of which a Step's Sessions enter at most one. For
    /// each Step, of the Environments its Sessions would enter that share a
    /// group and whose `enableIf` is not `false` for it, only the first in
    /// the Job's Environment order (§1.1 item 7) is entered; the others are
    /// *superseded* for the Step, which disables them for it (§3.2.2), so
    /// they are not held to constraint 7 and the Step's values for them are
    /// not checked. Job creation takes them in that order, and once one is
    /// entered, it does not evaluate the `enableIf` of the later ones for
    /// the Step (§7.4.1 step 6, before the checks of step 7). Template
    /// validation checks that the value is an `<Identifier>` (§7.1), and
    /// rejects the field without `ENV_PARAMS`, on a `stepEnvironments`
    /// entry, and on an Environment with `activation: ALWAYS` (constraint
    /// 1).
    pub exclusive_group: Option<String>,
    pub script: Option<EnvironmentScript>,
    pub variables: Option<HashMap<String, FormatString>>,
}

impl Environment {
    /// `activation` with its default applied (§4 item 4: absent means
    /// `BY_DEFAULT`), as it applies in a document that lists `ENV_PARAMS`.
    ///
    /// An Environment from a document that does not list `ENV_PARAMS`
    /// behaves as one with `activation: ALWAYS` and no `enableIf`, except
    /// that a Step's disabled `environment:` entry for it has no effect and
    /// is not an error (§4 item 4, §3.2 item 3.2). Job creation applies this
    /// to each attached Environment Template that does not list the
    /// extension; see
    /// [`create_job_with_environment_templates`](crate::create_job_with_environment_templates).
    /// This method does not know the document, so it returns the value as
    /// written, or `BY_DEFAULT`. In a Job Template that does not list
    /// `ENV_PARAMS` the two readings agree: no Step has an `environment:`
    /// entry and no Environment has an `enableIf`, so every Session enters
    /// every Job Environment either way.
    #[must_use]
    pub fn activation(&self) -> EnvironmentActivation {
        self.activation.unwrap_or_default()
    }

    /// Whether `enableIf` is known before the Job is created (§4, after item
    /// 10; §7.6 item 1): `Some(true)` when it is absent or the boolean
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

    /// Whether the Environment is known before the Job is created not to
    /// be disabled for any Step: its `activation` is `ALWAYS`, which cannot
    /// be disabled (§3.2.2 item 6; its `enableIf` being `false` for a Step
    /// is an error of its own), or its `enableIf` is absent or `true`.
    pub(crate) fn certainly_not_disabled(&self) -> bool {
        self.activation() == EnvironmentActivation::Always || self.literal_enabled() == Some(true)
    }

    /// Whether a Step's Sessions are known before the Job is created to enter
    /// the Environment (§4, after item 10), where `entry` is what the Step's
    /// `environment:` entry
    /// for it is known to be: always for `ALWAYS`, which cannot be disabled
    /// or declined (§3.2.2 item 6); for `BY_DEFAULT` when its `enableIf`
    /// is absent or `true` and the Step has no entry for it or an enabled
    /// one (a disabled entry declines it, and an expression may, §3.2 item
    /// 3.2); and for `ON_REQUEST` when its `enableIf` is absent or `true` and
    /// an enabled entry requests it.
    ///
    /// This is before the `exclusiveGroup` rule (§4 item 7); see
    /// [`known_state`](Self::known_state) and [`apply_exclusive_groups`].
    pub(crate) fn certainly_entered(&self, entry: KnownEntry) -> bool {
        self.known_state(entry) == Known::Entered
    }

    /// What is known before the Job is created about whether a Step's
    /// Sessions enter the Environment, where `entry` is what the Step's
    /// `environment:` entry for it is known to be (§4, after item 10, its
    /// first three bullets), before the `exclusiveGroup` rule: entered as
    /// [`certainly_entered`](Self::certainly_entered) describes; not
    /// entered when its own `enableIf` is the scalar `false` (unless it is
    /// `ALWAYS`, which cannot be disabled), when it is `BY_DEFAULT` and a
    /// disabled entry declines it, or when it is `ON_REQUEST` and no
    /// enabled entry requests it; and undetermined otherwise.
    pub(crate) fn known_state(&self, entry: KnownEntry) -> Known {
        self.known_state_in(true, entry)
    }

    /// [`known_state`](Self::known_state) for an Environment of a document
    /// that lists `ENV_PARAMS` exactly when `lists_env_params`. One from a
    /// document that does not list it behaves as `ALWAYS` with no
    /// `enableIf`, and a disabled entry has no effect on it (§4 item 4,
    /// §3.2 item 3.2), so it is known to be entered whatever `entry` is.
    pub(crate) fn known_state_in(&self, lists_env_params: bool, entry: KnownEntry) -> Known {
        let activation = self.activation();
        if !lists_env_params || activation == EnvironmentActivation::Always {
            return Known::Entered;
        }
        let requested = match (activation, entry) {
            (EnvironmentActivation::ByDefault, KnownEntry::Absent | KnownEntry::Enabled) => {
                Some(true)
            }
            (EnvironmentActivation::OnRequest, KnownEntry::Enabled) => Some(true),
            (_, KnownEntry::Undetermined) => None,
            _ => Some(false),
        };
        match (requested, self.literal_enabled()) {
            (Some(false), _) | (_, Some(false)) => Known::NotEntered,
            (Some(true), Some(true)) => Known::Entered,
            _ => Known::Undetermined,
        }
    }

    /// The `exclusiveGroup` that the superseding rule (§4 item 7) applies
    /// with: none for an Environment with `activation: ALWAYS`, which
    /// cannot be disabled (§3.2.2 item 6) and must not have one
    /// (constraint 1, reported by template validation).
    pub(crate) fn group(&self) -> Option<&str> {
        self.exclusive_group
            .as_deref()
            .filter(|_| self.activation() != EnvironmentActivation::Always)
    }
}

/// What is known before the Job is created about whether a Step's Sessions
/// enter one Environment (§4, after item 10; §7.6 rule 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Known {
    /// Known to be entered.
    Entered,
    /// Known not to be entered.
    NotEntered,
    /// Decided only when the Job is created.
    Undetermined,
}

/// Apply the `exclusiveGroup` bullet of §4 (after item 10) to one Step's
/// Environments, given in the Job's Environment order (§1.1 item 7) as each
/// one's [`Environment::group`] and its state before the rule
/// ([`Environment::known_state`], or a sharper one): one with a group is
/// known to be entered only when, in addition, every earlier one of its
/// group is known not to be entered, and is known not to be entered when an
/// earlier one of its group would, by its state before the rule, be known
/// to be entered, since that one, or one before it, is then the group's
/// first. Any other one keeps its state.
pub(crate) fn apply_exclusive_groups<'g>(
    states: impl IntoIterator<Item = (Option<&'g str>, Known)>,
) -> Vec<Known> {
    // Per group: whether an earlier member is known to be entered, and
    // whether one is undetermined.
    let mut seen: HashMap<&str, (bool, bool)> = HashMap::new();
    states
        .into_iter()
        .map(|(group, state)| {
            let Some(group) = group else {
                return state;
            };
            let earlier = seen.entry(group).or_insert((false, false));
            let result = match (*earlier, state) {
                (_, Known::NotEntered) | ((true, _), _) => Known::NotEntered,
                ((false, false), Known::Entered) => Known::Entered,
                _ => Known::Undetermined,
            };
            earlier.0 |= state == Known::Entered;
            earlier.1 |= state == Known::Undetermined;
            result
        })
        .collect()
}

/// What a Step's `environment:` entry for one Environment is known to be
/// before the Job is created (§3.2 item 3.2, checked-at paragraph; §7.6 rule
/// 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KnownEntry {
    /// The Step has no entry for the Environment.
    Absent,
    /// An entry whose `enableIf` is absent or the boolean scalar `true`.
    Enabled,
    /// An entry whose `enableIf` is the boolean scalar `false`: it
    /// requests nothing, declines a `BY_DEFAULT` Environment, is an error
    /// for an `ALWAYS` one, and has no effect on one from a document that
    /// does not list `ENV_PARAMS`.
    Disabled,
    /// An entry whose `enableIf` is an expression, even a constant one,
    /// decided when the Job is created.
    Undetermined,
}

impl KnownEntry {
    /// What `dep`, a Step's entry for an Environment, is known to be;
    /// [`Absent`](Self::Absent) for `None`.
    pub(crate) fn of(dep: Option<&super::StepDependency>) -> Self {
        match dep.map(super::StepDependency::literal_enabled) {
            None => Self::Absent,
            Some(Some(true)) => Self::Enabled,
            Some(Some(false)) => Self::Disabled,
            Some(None) => Self::Undetermined,
        }
    }

    /// What `step`'s first well-formed `environment:` entry naming `name`
    /// is known to be (a second one is a constraint 3 error).
    pub(crate) fn in_step(step: &super::StepTemplate, name: &str) -> Self {
        Self::of(
            step.dependencies
                .iter()
                .flatten()
                .find(|d| d.environment() == Some(name)),
        )
    }
}

/// §4 item 4 `<EnvironmentActivation>` (`ENV_PARAMS` extension): which Sessions of a
/// Job enter an Environment, and whether its own `enableIf` (§4 item 6) may
/// leave a Step's Sessions out. Values are case-sensitive; an unrecognized
/// value is rejected at decode (constraint 1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EnvironmentActivation {
    /// `ALWAYS`: every Session of the Job enters the Environment, in its
    /// position in the Job's Environment order, and it cannot be disabled
    /// or declined: its `enableIf` being `false` for a Step is an error
    /// (§4 item 4, §3.2.2 item 6), and so is a Step's disabled
    /// `environment:` entry for it (§3.2 item 3.2). Each is reported naming
    /// the Step and the Environment: at template validation when it is
    /// known then (a literal `false`, on the Environment, or on an entry
    /// naming a Job Environment), otherwise when the Job is created. An
    /// Environment from a document that does not list `ENV_PARAMS` behaves
    /// as `ALWAYS`, but a disabled entry for it has no effect and is not an
    /// error (see [`Environment::activation`]).
    Always,
    /// `BY_DEFAULT`: every Session of the Job enters the Environment, in its
    /// position in the Job's Environment order, except the Sessions of a
    /// Step that declines it with a disabled `environment:` entry (§3.2
    /// item 3.2), of a Step for which its `enableIf` is `false`, and of a
    /// Step for which an earlier Environment of its `exclusiveGroup`
    /// supersedes it (§4 item 7). The default in a document that lists
    /// `ENV_PARAMS`. An Environment from a document that does not list it
    /// behaves as `ALWAYS` instead, except that a disabled entry for it has
    /// no effect (see [`Environment::activation`]).
    #[default]
    ByDefault,
    /// `ON_REQUEST`: only the Sessions of a Step that requests the
    /// Environment through an enabled `environment:` entry of its
    /// `dependencies` enter it, less any for which its `enableIf` is
    /// `false` or an earlier Environment of its `exclusiveGroup` supersedes
    /// it (§4 item 7).
    OnRequest,
}

impl EnvironmentActivation {
    /// The value as a template writes it: `ALWAYS`, `BY_DEFAULT`, or
    /// `ON_REQUEST`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Always => "ALWAYS",
            Self::ByDefault => "BY_DEFAULT",
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
