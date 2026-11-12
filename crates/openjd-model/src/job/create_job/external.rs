// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Attaching Environment Templates to a Job (`ENV_PARAMS`, Template
//! Schemas §3.2.1 "Resolving external Environments" and §3.2.2): the
//! submission stage of
//! [`create_job_with_environment_templates`](super::create_job_with_environment_templates).
//!
//! A submission combines one Job Template with zero or more Environment
//! Templates that the scheduler attaches, in the scheduler's order. Job
//! creation proper keeps each enabled `environment:` entry that names no
//! Job Environment as a [`FrozenEntry`], and each disabled one by name,
//! and, with `ENV_PARAMS`, defers the Job Template's own `jobEnvironments`
//! with each Step's pending values for them.
//! [`apply_environment_templates`] then resolves those entries against the
//! attached Environment Templates by name, fixes the part of each Step's
//! Environment set that the attachments contribute (§4 item 4
//! `activation`; an attachment from an Environment Template that does not
//! list `ENV_PARAMS` behaves as `ALWAYS`; a disabled entry declines a
//! `BY_DEFAULT` attachment, is an error for an `ALWAYS` one, and has no
//! effect on a pre-extension one, §3.2 item 3.2), computes every Step's
//! effective values for each attached Environment it would enter, decides
//! each attachment's own `enableIf` (§4 item 6) per Step with the values
//! that are known, checks constraint 5 for every attachment and
//! constraints 6 and 7 for those it does not disable (§3.2.2), checks the
//! `WRAP_ACTIONS` single-layer rule over each Step's final set, and builds
//! the combined Job. It decides the deferred Job Environments the same way,
//! after the attachments.
//!
//! [`check_environment_templates`] runs the checks that need no Job
//! Parameter values, before any are known.

use std::collections::{BTreeMap, HashMap, HashSet};

use openjd_expr::ExprValue;

use crate::error::{
    path_field, path_index, path_to_string, ModelError, PathElement, ValidationErrors,
};
use crate::job;
use crate::template::{
    EnvironmentParameterValue, EnvironmentTemplate, JobTemplate, StepDependency,
};
use crate::types::{JobParameterValues, ModelExtension, ValidationContext};

use super::build_symbol_table;
use super::env_params::{
    report_missing, DeclaringEnvironment, EnableIf, FrozenEntry, ResolvePolicy, Supplied,
};
use super::staged::{Stage, StagedErrors, ANY};
use crate::template::validate_v2023_09::env_params::{
    self as tv, always_entry_disabled_message, capped_name_list, EVERY_JOB_ENVIRONMENT,
};
use crate::template::{apply_exclusive_groups, EnvironmentActivation, Known, KnownEntry};

/// The model name submission-time errors report under: these checks relate
/// several documents, so no single template is "the" model.
const SUBMISSION: &str = "Submission";

/// One Environment Template attached to a submission, with an optional
/// label naming the document in messages.
///
/// Without a label the document is named `EnvironmentTemplate[i]`, its
/// 0-based attachment index. A label, typically the file path the template
/// was read from, replaces that name, so a message reads
/// `conda.yaml -> environment` instead of
/// `EnvironmentTemplate[0] -> environment`.
#[derive(Debug, Clone, Copy)]
pub struct AttachedEnvironmentTemplate<'a> {
    /// The decoded, validated template.
    pub template: &'a EnvironmentTemplate,
    /// How to name this document in messages; `None` for the positional
    /// default.
    pub label: Option<&'a str>,
}

impl<'a> AttachedEnvironmentTemplate<'a> {
    /// Attach `template` with the positional default name.
    #[must_use]
    pub fn new(template: &'a EnvironmentTemplate) -> Self {
        Self {
            template,
            label: None,
        }
    }

    /// Name this document `label` in messages.
    #[must_use]
    pub fn with_label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }
}

impl<'a> From<&'a EnvironmentTemplate> for AttachedEnvironmentTemplate<'a> {
    fn from(template: &'a EnvironmentTemplate) -> Self {
        Self::new(template)
    }
}

/// A condition a submission should be warned about but that does not
/// reject it (§3.2.1 item 4; RFC "External Environments are named
/// implicitly").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionWarning {
    /// Where the condition is: relative to the Job Template when no
    /// Environment Template is attached, and otherwise starting with the
    /// document (`JobTemplate`, `EnvironmentTemplate[i]`, or the
    /// attachment's label).
    pub path: Vec<PathElement>,
    /// What the condition is.
    pub message: String,
}

/// `path: message`, as the error paths are written.
impl std::fmt::Display for SubmissionWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", path_to_string(&self.path), self.message)
    }
}

/// A Job created from a Job Template and the Environment Templates
/// attached to its submission, returned by
/// [`create_job_with_environment_templates`](super::create_job_with_environment_templates).
#[derive(Debug, Clone, PartialEq)]
pub struct CreatedJob {
    /// The combined Job: its `job_environments` are the attached
    /// Environments, in attachment order, followed by the Job Template's
    /// own, which is the Job's Environment order, the order a Session enters
    /// them (Template Schemas §1.1 item 7). Each Step's
    /// `environment_parameters` and `environments` cover both.
    pub job: job::Job,
    /// The document of each entry of `job.job_environments`, index for
    /// index: [`job::Document::EnvironmentTemplate`] for the attached ones,
    /// then [`job::Document::JobTemplate`].
    pub environment_documents: Vec<job::Document>,
    /// Conditions to warn about, in the order found: an attached
    /// Environment named like a Job Environment, which no entry of the Job
    /// Template can reach (§3.2.1 item 4), when the Job Template lists
    /// `ENV_PARAMS` (without it no entry exists, and the submission gets
    /// no warning it did not get before the extension); an enabled `required: false`
    /// entry that resolves to no Environment (RFC "External Environments
    /// are named implicitly" item 3); and an enabled `required: false`
    /// entry whose `parameters` an attached Environment that declares none
    /// ignores (§3.2.1 item 1).
    pub warnings: Vec<SubmissionWarning>,
    /// For each Step of `job`, index for index, the Environments disabled
    /// for that Step (Template Schemas §3.2.2): those its own `enableIf`
    /// (§4 item 6) disabled, and those an earlier Environment of its
    /// `exclusiveGroup` superseded (§4 item 7). Its Sessions would otherwise
    /// have entered them, and `job` therefore leaves them out of the Step's
    /// [`environments`](job::Step::environments) and
    /// [`step_environments`](job::Step::step_environments). For display;
    /// empty for a Step with none.
    pub disabled_environments: Vec<StepDisabledEnvironments>,
    /// For each Step of `job`, index for index, every Environment the Step
    /// considered, in the Job's Environment order, with whether its
    /// Sessions enter it or which decision left it out (declined,
    /// disabled, superseded), and the Step's `required: false` entries that
    /// named no Environment. A superset of
    /// [`disabled_environments`](Self::disabled_environments), which is its
    /// `Disabled` and `Superseded` items.
    pub environment_statuses: Vec<StepEnvironmentStatuses>,
    /// For each Step of `job`, index for index, the host requirements that
    /// resolved to nothing for that Step and were discarded (`ENV_PARAMS`,
    /// Template Schemas §3.3): the Step's own first, then each entered
    /// Environment's, in Job order, each in written order with amounts
    /// first. For display; empty for a Step with none.
    pub discarded_host_requirements: Vec<Vec<DiscardedHostRequirement>>,
}

/// A host requirement that resolved to nothing for a Step and was
/// discarded (`ENV_PARAMS`, Template Schemas §3.3): an amount whose `min`
/// and `max` both resolved to `null`, or an attribute whose `anyOf` and
/// `allOf`, each written as one expression, resolved to `null`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscardedHostRequirement {
    /// The Environment whose `hostRequirements` held it, by `(document,
    /// environment)`; `None` for the Step's own.
    pub environment: Option<job::EnvironmentRef>,
    /// Whether it was an amount or an attribute requirement.
    pub kind: HostRequirementKind,
    /// Its resolved capability name.
    pub name: String,
}

/// The kind of a `hostRequirements` entry (Template Schemas §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostRequirementKind {
    /// An `amounts` entry (§3.3.1).
    Amount,
    /// An `attributes` entry (§3.3.2).
    Attribute,
}

impl From<crate::template::validate_v2023_09::helpers::CapabilityKind> for HostRequirementKind {
    fn from(kind: crate::template::validate_v2023_09::helpers::CapabilityKind) -> Self {
        match kind {
            crate::template::validate_v2023_09::helpers::CapabilityKind::Amount => Self::Amount,
            crate::template::validate_v2023_09::helpers::CapabilityKind::Attribute => {
                Self::Attribute
            }
        }
    }
}

/// The Environments disabled for one Step of a [`CreatedJob`]: by their
/// `enableIf` (Template Schemas §4 item 6), or, for a Job Environment, by
/// being superseded by an earlier one of its `exclusiveGroup` (§4 item 7).
/// It is the `Disabled` and `Superseded` items of the Step's
/// [`StepEnvironmentStatuses`], which also says which of the two each is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepDisabledEnvironments {
    /// Job Environments of the combined Job (attached ones first, then the
    /// Job Template's), in that order, by `(document, environment)`.
    pub environments: Vec<job::EnvironmentRef>,
    /// The names of the Step's own `stepEnvironments` entries, in written
    /// order.
    pub step_environments: Vec<String>,
}

/// Whether one Step's Sessions enter an Environment they considered, and
/// if not, which decision of Template Schemas §7.4.1 left it out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnvironmentStatus {
    /// The Step's Sessions enter it.
    Entered,
    /// A `BY_DEFAULT` Environment that a disabled `environment:` entry of
    /// the Step declines (§3.2 item 3.2, §7.4.1 step 4). Its own `enableIf`
    /// is not evaluated for the Step. Never an attached Environment whose
    /// Environment Template does not list `ENV_PARAMS`: it behaves as
    /// `ALWAYS`, and a disabled entry has no effect on it (§4 item 4).
    Declined,
    /// Its own `enableIf` (§4 item 6) is `false` for the Step (§7.4.1 step
    /// 6).
    Disabled,
    /// An earlier Environment of its `exclusiveGroup` remains in the Step's
    /// set (§4 item 7, §7.4.1 step 6). Its own `enableIf` is not evaluated
    /// for the Step.
    Superseded,
}

/// Where an Environment of a Job comes from, the three parts of the Job's
/// Environment order (Template Schemas §1.1 item 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnvironmentScope {
    /// An attached Environment Template's `environment`.
    Attached,
    /// A `jobEnvironments` entry of the Job Template.
    Job,
    /// A `stepEnvironments` entry of the Step.
    Step,
}

/// One Environment a Step's Sessions considered, and whether they enter it
/// ([`StepEnvironmentStatuses`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StepEnvironmentStatus {
    /// The Environment's `name`.
    pub name: String,
    /// The document that declares it: [`job::Document::EnvironmentTemplate`]
    /// for an attached one, else [`job::Document::JobTemplate`].
    pub document: job::Document,
    /// Its part of the Job's Environment order. `Attached` exactly when
    /// `document` is an Environment Template.
    pub scope: EnvironmentScope,
    /// Whether the Step's Sessions enter it, or why not.
    pub status: EnvironmentStatus,
    /// The Environment's `exclusiveGroup` (Template Schemas §4 item 7),
    /// whatever its `status`; `None` for one without, which includes every
    /// Step Environment (the field is not permitted there).
    pub exclusive_group: Option<String>,
    /// For a `Superseded` one, the member of its `exclusiveGroup` the Step's
    /// Sessions enter: the earliest one in the Job's Environment order that
    /// remains in the Step's set (§7.4.1 step 6). `None` for every other
    /// `status`.
    pub superseded_by: Option<SupersedingEnvironment>,
}

/// The Environment that superseded another for a Step
/// ([`StepEnvironmentStatus::superseded_by`]): an attached or Job
/// Environment, since a Step Environment has no `exclusiveGroup`. `name`
/// and `document` identify it among the Job's Environments, also when an
/// attached Environment and a Job Environment share a name; `scope`
/// follows from `document` and is given so the winner's status item can
/// be matched on the same three fields.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SupersedingEnvironment {
    /// The Environment's `name`.
    pub name: String,
    /// The document that declares it, as in
    /// [`StepEnvironmentStatus::document`].
    pub document: job::Document,
    /// Its part of the Job's Environment order: `Attached` or `Job`.
    pub scope: EnvironmentScope,
}

/// An enabled `environment:` entry with `required: false` that resolved to
/// no Environment (Template Schemas §3.2.1 item 2), so it has no effect.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OptionalEntryNotFound {
    /// The entry's 0-based position in the Step's `dependencies` list.
    pub dependency_index: usize,
    /// The Environment name the entry gives.
    pub environment: String,
}

/// Every Environment one Step of a [`CreatedJob`] considered, with whether
/// its Sessions enter it, and its optional entries that named nothing.
///
/// An Environment is *considered* by a Step when the Step's Sessions would
/// enter it by its `activation` (Template Schemas §4 item 4): every
/// `ALWAYS` and `BY_DEFAULT` Environment, every `ON_REQUEST` one an enabled
/// `environment:` entry of the Step requests, and every Step Environment of
/// the Step. An `ON_REQUEST` Environment the Step does not request is not
/// listed. A Job Template without `ENV_PARAMS` has neither `activation`,
/// `enableIf` nor `exclusiveGroup`, so its own Environments are all
/// `Entered`; only attached ones can be otherwise. An attached one whose
/// Environment Template does not list `ENV_PARAMS` behaves as `ALWAYS`
/// (Template Schemas §4 item 4), so it is `Entered` too.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepEnvironmentStatuses {
    /// The considered Environments in the Job's Environment order: attached
    /// ones in attachment order, then the Job Template's `jobEnvironments`,
    /// then the Step's `stepEnvironments`, each in written order. The
    /// `Entered` ones, in this order, are the Environments the Step's
    /// Sessions enter.
    pub environments: Vec<StepEnvironmentStatus>,
    /// The Step's enabled `required: false` entries that named no
    /// Environment, in `dependencies` order. Each also produced a
    /// [`SubmissionWarning`].
    pub not_found: Vec<OptionalEntryNotFound>,
}

/// The documents of a submission, for naming them in paths.
struct Documents<'a> {
    attached: &'a [AttachedEnvironmentTemplate<'a>],
}

impl Documents<'_> {
    /// The path prefix naming document `doc` (`JobTemplate` for the Job
    /// Template, else the attachment's label or `EnvironmentTemplate[i]`).
    fn path(&self, doc: Option<usize>) -> Vec<PathElement> {
        match doc {
            None => vec![PathElement::Field("JobTemplate".to_string())],
            Some(i) => match self.attached[i].label {
                Some(label) => vec![PathElement::Field(label.to_string())],
                None => vec![
                    PathElement::Field("EnvironmentTemplate".to_string()),
                    PathElement::Index(i),
                ],
            },
        }
    }
}

/// The attached Environment Templates of a submission, set up for
/// resolving entries against them. Shared by
/// [`apply_environment_templates`] and [`check_environment_templates`],
/// so both report the same conditions with the same messages.
struct Attachments<'a> {
    /// Each attachment's name in messages, for the lists of attachments,
    /// which are capped like every list in these messages, since labels
    /// are caller-supplied and may be long.
    doc_names: Vec<String>,
    /// Each attachment's own validation context: its profile, with the
    /// caller's limits.
    contexts: Vec<ValidationContext>,
    declaring: Vec<DeclaringEnvironment<'a>>,
    by_name: HashMap<&'a str, Vec<usize>>,
    /// Attachments no entry can reach: named like a Job Environment
    /// (§3.2.1 item 4) or like another attachment (item 3). The missing
    /// parameters of one with `activation: ALWAYS` or `BY_DEFAULT` are
    /// reported once, at the attachment, not per Step; one with
    /// `ON_REQUEST` is never entered.
    unreachable: HashSet<usize>,
    /// The attachment-level errors for an unreachable attachment with
    /// `activation: ALWAYS` or `BY_DEFAULT` and parameters without a
    /// `default`, by attachment, path, and message. They are reported by
    /// [`report_unreachable`](Self::report_unreachable) only for an
    /// attachment that its own `enableIf` does not disable (§3.2.2): a
    /// disabled one is not entered, so its parameters need no value. An
    /// `ALWAYS` one cannot be disabled (§3.2.2 item 6), so its error is
    /// always reported.
    unreachable_required: Vec<(usize, Vec<PathElement>, String)>,
}

/// How an unreachable attachment's error says a `BY_DEFAULT` attachment
/// with an `enableIf` is entered.
const ENTERED_ENABLE_IF: &str =
    "Its activation is BY_DEFAULT and its enableIf does not disable it, so Sessions enter it";
/// The same, for an attachment whose `enableIf` could not be evaluated for
/// any Step that decided it.
const ENTERED_ENABLE_IF_FAILED: &str = "Its activation is BY_DEFAULT and its enableIf could not \
     be evaluated, so it is checked as if Sessions enter it";

/// An enabled entry of a Step that resolved to exactly one attachment, in
/// [`apply_environment_templates`].
struct ResolvedEntry<'e> {
    /// The attachment's index.
    doc: usize,
    /// The entry.
    entry: &'e FrozenEntry,
    /// The entry's full path.
    path: Vec<PathElement>,
    /// Whether the attachment ignores the entry's `parameters` (§3.2.1
    /// item 1), so that it supplies none.
    ignored: bool,
}

/// What one `environment:` entry naming no Job Environment resolves to.
enum EntryMatch<'m> {
    /// No attachment has the name; the error or warning is reported.
    Absent,
    /// Exactly one attachment, by index.
    One(usize),
    /// Two or more; the error is reported.
    Many(&'m [usize]),
}

impl<'a> Attachments<'a> {
    /// Index `attached`, reporting the attachment-level conditions: an
    /// attachment named like a Job Environment (a warning when the Job
    /// Template lists `ENV_PARAMS`, `job_lists_env_params`, since only
    /// then could an entry have named it; and an error when it has
    /// parameters without a `default`), an ambiguous name with such
    /// parameters, and every literal `default` that fails its checks
    /// (§4.5 item 2).
    fn new(
        attached: &'a [AttachedEnvironmentTemplate<'a>],
        job_env_names: &HashSet<&str>,
        job_lists_env_params: bool,
        default_symtab: &openjd_expr::SymbolTable,
        ctx: &ValidationContext,
        errors: &mut ValidationErrors,
        warnings: &mut Vec<SubmissionWarning>,
    ) -> Self {
        let docs = Documents { attached };
        let doc_names: Vec<String> = (0..attached.len())
            .map(|d| path_to_string(&docs.path(Some(d))))
            .collect();
        let mut contexts = Vec::with_capacity(attached.len());
        let mut declaring = Vec::with_capacity(attached.len());
        let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut unreachable: HashSet<usize> = HashSet::new();
        let mut unreachable_required = Vec::new();
        // How the messages below say the attachment is entered. An
        // `ALWAYS` one cannot be disabled (§3.2.2 item 6), whatever its
        // `enableIf`.
        let entered = |env: &crate::template::Environment, activation| match activation {
            EnvironmentActivation::ByDefault if env.enable_if.is_some() => ENTERED_ENABLE_IF,
            // With a group, an earlier Environment of it may supersede this
            // one for some Steps (§4 item 7).
            EnvironmentActivation::ByDefault if env.group().is_some() => {
                "Its activation is BY_DEFAULT and an earlier Environment of its exclusiveGroup \
                 does not supersede it, so Sessions enter it"
            }
            EnvironmentActivation::ByDefault => {
                "Its activation is BY_DEFAULT, so every Session enters it"
            }
            // An `ON_REQUEST` attachment has no such error.
            EnvironmentActivation::Always | EnvironmentActivation::OnRequest => {
                "Its activation is ALWAYS, so every Session enters it"
            }
        };
        for (doc, att) in attached.iter().enumerate() {
            let env = att.template.environment();
            // §4 item 4: an Environment from a document that does not list
            // `ENV_PARAMS` behaves as `ALWAYS`.
            let pre_extension = !att
                .template
                .profile()
                .has_extension(ModelExtension::EnvParams);
            let activation = if pre_extension {
                EnvironmentActivation::Always
            } else {
                env.activation()
            };
            let att_ctx = att
                .template
                .default_validation_context()
                .with_caller_limits(ctx.caller_limits.clone());
            let env_path = path_field(&docs.path(Some(doc)), "environment");
            if job_env_names.contains(env.name.as_str()) {
                // No entry can supply a parameter of this attachment, so
                // one without a `default` rejects the submission. Say so
                // here, at the attachment, rather than once per Step with
                // "must supply" advice no entry can follow. (A warning is
                // dropped when the submission is rejected.)
                // With `activation: ON_REQUEST` no Session enters it,
                // since no entry can request it.
                let required = required_names(env);
                if !required.is_empty() && activation != EnvironmentActivation::OnRequest {
                    unreachable_required.push((
                        doc,
                        path_field(&env_path, "envParameterDefinitions"),
                        format!(
                            "Environment '{}' has the same name as a jobEnvironments entry of \
                             the Job Template, so no 'environment:' dependency can reach it \
                             (Template Schemas §3.2.1 item 4), and it has parameters without a \
                             default that no Step can supply: {}. {}, and the submission is \
                             rejected (§3.2.1).",
                            env.name,
                            capped_name_list(required.iter().copied(), required.len()),
                            entered(env, activation)
                        ),
                    ));
                }
                // Without ENV_PARAMS in the Job Template there are no
                // entries to reach any attachment, and such a submission
                // gets no warning it did not get before the extension (RFC
                // Backward compatibility).
                if job_lists_env_params {
                    warnings.push(SubmissionWarning {
                        path: path_field(&env_path, "name"),
                        message: format!(
                            "Environment '{}' has the same name as a jobEnvironments entry of \
                             the Job Template, so no 'environment:' dependency of the Job \
                             Template can reach it (Template Schemas §3.2.1 item 4).",
                            env.name
                        ),
                    });
                }
                unreachable.insert(doc);
            }
            by_name.entry(env.name.as_str()).or_default().push(doc);
            declaring.push(DeclaringEnvironment::new(
                job::Document::environment_template(doc, att.label),
                env,
                &env_path,
                pre_extension,
            ));
            contexts.push(att_ctx);
        }
        // §3.2.1 item 3: an entry naming an Environment that two or more
        // attachments define is rejected, so no entry can supply their
        // parameters. One without a `default` rejects the submission; say
        // so once per attachment, rather than per Step with "must supply"
        // advice that an entry could only follow by naming the ambiguous
        // name.
        for (doc, att) in attached.iter().enumerate() {
            let env = att.template.environment();
            let Some(others) = by_name.get(env.name.as_str()).filter(|d| d.len() > 1) else {
                continue;
            };
            unreachable.insert(doc);
            let required = required_names(env);
            let activation = declaring[doc].activation;
            if required.is_empty() || activation == EnvironmentActivation::OnRequest {
                continue;
            }
            unreachable_required.push((
                doc,
                path_field(
                    &path_field(&docs.path(Some(doc)), "environment"),
                    "envParameterDefinitions",
                ),
                format!(
                    "Environment '{}' is defined by {} attached Environment Templates ({}), so \
                     no 'environment:' dependency can reach it (Template Schemas §3.2.1 item \
                     3), and it has parameters without a default that no Step can supply: {}. \
                     {}, and the submission is rejected (§3.2.1).",
                    env.name,
                    others.len(),
                    capped_name_list(others.iter().map(|d| doc_names[*d].as_str()), others.len()),
                    capped_name_list(required.iter().copied(), required.len()),
                    entered(env, activation)
                ),
            ));
        }
        for (de, att_ctx) in declaring.iter().zip(&contexts) {
            de.check_literal_defaults(default_symtab, ResolvePolicy::new(att_ctx), errors);
        }
        Self {
            doc_names,
            contexts,
            declaring,
            by_name,
            unreachable,
            unreachable_required,
        }
    }

    /// Report the attachment-level error of each unreachable attachment
    /// with `activation: ALWAYS` or `BY_DEFAULT` and parameters without a
    /// `default` (see
    /// [`unreachable_required`](Self::unreachable_required)) for which
    /// `not_disabled(doc)` holds: a Step's Sessions enter it, as its own
    /// `enableIf` does not disable it for every Step. Each goes to `add`
    /// with the attachment's index, its path and its message.
    fn report_unreachable(
        &self,
        mut add: impl FnMut(usize, &[PathElement], String),
        not_disabled: impl Fn(usize) -> bool,
        only_failed: impl Fn(usize) -> bool,
    ) {
        for (doc, path, message) in &self.unreachable_required {
            if not_disabled(*doc) {
                // An attachment whose `enableIf` failed for every Step that
                // decided it is checked as if it is not disabled; say so
                // rather than that its enableIf does not disable it.
                let message = if only_failed(*doc) {
                    message.replace(ENTERED_ENABLE_IF, ENTERED_ENABLE_IF_FAILED)
                } else {
                    message.clone()
                };
                add(*doc, path, message);
            }
        }
    }

    /// §3.2.1 item 1: whether an enabled entry that resolved to attachment
    /// `doc` has its `parameters` ignored, which is when the attachment
    /// declares no `envParameterDefinitions`, the entry has
    /// `required: false`, and it supplies parameters. An ignored entry
    /// supplies no values: its values are not resolved, and constraints 5
    /// and 6 do not apply to them. It still refers to the attachment, so it
    /// still requests an `ON_REQUEST` one. Each ignored entry gets a
    /// warning at its `parameters`, naming the Environment and the ignored
    /// keys. `entry_path` is the entry's full path.
    fn ignores_parameters(
        &self,
        doc: usize,
        required: bool,
        params: &BTreeMap<String, EnvironmentParameterValue>,
        entry_path: &[PathElement],
        warnings: &mut Vec<SubmissionWarning>,
    ) -> bool {
        let de = &self.declaring[doc];
        if required || params.is_empty() || de.declares_parameters() {
            return false;
        }
        warnings.push(SubmissionWarning {
            path: path_field(entry_path, "parameters"),
            message: ignored_parameters_message(&de.label, params),
        });
        true
    }

    /// Resolve one entry naming `environment`, which is not a Job
    /// Environment, by name (§3.2.1 items 1–3), reporting a missing
    /// required Environment or an ambiguous name into `errors` and a
    /// missing optional one into `warnings`. `env_path` is the entry's
    /// `environment` key.
    fn match_entry(
        &self,
        environment: &str,
        required: bool,
        env_path: Vec<PathElement>,
        errors: &mut ValidationErrors,
        warnings: &mut Vec<SubmissionWarning>,
    ) -> EntryMatch<'_> {
        let candidates = self.by_name.get(environment).map_or(&[][..], Vec::as_slice);
        match candidates {
            [] => {
                let attached_list = if self.doc_names.is_empty() {
                    "no Environment Template is attached".to_string()
                } else {
                    format!(
                        "none of the attached Environment Templates ({}) defines it",
                        capped_name_list(
                            self.doc_names.iter().map(String::as_str),
                            self.doc_names.len()
                        )
                    )
                };
                if required {
                    errors.add(
                        &env_path,
                        format!(
                            "names Environment '{environment}', which is not a jobEnvironments \
                             entry of the Job Template, and {attached_list}. The entry is \
                             required; set 'required: false' if the Step can run without the \
                             Environment (Template Schemas §3.2.1 item 2)."
                        ),
                    );
                } else {
                    warnings.push(SubmissionWarning {
                        path: env_path,
                        message: format!(
                            "names Environment '{environment}', which is not a jobEnvironments \
                             entry of the Job Template, and {attached_list}. The entry has \
                             'required: false', so it has no effect."
                        ),
                    });
                }
                EntryMatch::Absent
            }
            [doc] => EntryMatch::One(*doc),
            many => {
                errors.add(
                    &env_path,
                    format!(
                        "names Environment '{environment}', which {} attached Environment \
                         Templates define ({}); an entry must match exactly one (Template \
                         Schemas §3.2.1 item 3).",
                        many.len(),
                        capped_name_list(
                            many.iter().map(|d| self.doc_names[*d].as_str()),
                            many.len()
                        )
                    ),
                );
                EntryMatch::Many(many)
            }
        }
    }

    /// The labels of the attachments named in `names`, the names of a
    /// Step's entries that their `enableIf` disabled, for the constraint 7
    /// message.
    fn disabled_labels<'n>(&self, names: impl Iterator<Item = &'n str>) -> HashSet<&str> {
        names
            .filter_map(|n| self.by_name.get(n))
            .flatten()
            .map(|d| self.declaring[*d].label.as_str())
            .collect()
    }

    /// Sort the parameters without a `default` of attachment `doc` that a
    /// Step does not supply (`names`): nothing to do when there are none
    /// or the attachment is unreachable (reported once, at the
    /// attachment); an error at `step_path` when the Step has a
    /// `stepEnvironments` entry of the same name (§3.2.1 item 5), since no
    /// entry of the Step can reach it, so it is entered only because its
    /// `activation` is `ALWAYS` or `BY_DEFAULT`; otherwise pushed to
    /// `missing` for the Step's one constraint 7 error.
    #[allow(clippy::too_many_arguments)]
    fn sort_missing<'n>(
        &self,
        doc: usize,
        names: Vec<&'n str>,
        step_env_names: &HashSet<&str>,
        step_path: &[PathElement],
        missing: &mut Vec<(String, Vec<&'n str>)>,
        errors: &mut ValidationErrors,
    ) {
        let de = &self.declaring[doc];
        if names.is_empty() || self.unreachable.contains(&doc) {
            return;
        }
        if step_env_names.contains(de.name) {
            errors.add(
                step_path,
                format!(
                    "cannot supply the parameters without a default of Environment {}: {}. It \
                     has the same name as a stepEnvironments entry of this Step, so no \
                     'environment:' dependency of this Step can reach it (Template Schemas \
                     §3.2.1 item 5), and its activation is {}, so this Step's Sessions enter it.",
                    de.label,
                    capped_name_list(names.iter().copied(), names.len()),
                    de.activation.as_str()
                ),
            );
        } else {
            missing.push((de.label.clone(), names));
        }
    }
}

/// The warning for an entry whose `parameters` an attachment that declares
/// none ignores (§3.2.1 item 1), naming the Environment by `label` and the
/// ignored keys, capped as the other name lists are.
fn ignored_parameters_message(
    label: &str,
    params: &BTreeMap<String, EnvironmentParameterValue>,
) -> String {
    format!(
        "Environment {label} declares no parameters, and this entry has 'required: false', so \
         its parameters are ignored: {}. The Step's Sessions get no values from it (Template \
         Schemas §3.2.1 item 1).",
        capped_name_list(params.keys().map(String::as_str), params.len())
    )
}

/// The names of `env`'s parameters without a `default`.
fn required_names(env: &crate::template::Environment) -> Vec<&str> {
    env.env_parameter_definitions
        .iter()
        .flatten()
        .filter(|d| d.default.is_none())
        .map(|d| d.name())
        .collect()
}

/// The constraint 7 reason for attached Environments, which completes
/// [`missing_parameters_message`](crate::template::validate_v2023_09::env_params::missing_parameters_message).
const EVERY_ATTACHED: &str = "every attached Environment with activation ALWAYS, each one with \
     activation BY_DEFAULT that the Step does not decline, and each one \
     with activation ON_REQUEST that the Step requests; Template Schemas §3.2.1";

/// Attach the Environment Templates of a submission to the Job that job
/// creation built from the Job Template alone (`body`), decide every
/// Step's Environments, and return the combined Job (Template Schemas
/// §3.2.1, §3.2.2, §4 items 4 and 5, §3.3.3, §7.4.1 steps 3–8).
///
/// `body` holds the Job, what job creation decided Step by Step (the
/// enabled entries that name no Job Environment, frozen; the disabled ones;
/// the Job Environments the Step's Sessions would enter by their
/// `activation`, with the Step's values for them), and the errors found so
/// far. `env_names` is the Job's `Job.EnvNames`, bound for attached
/// Environments whose document lists `EXPR`. `attached` is in the
/// scheduler's order. `job_parameter_values` are the merged values. `ctx`
/// supplies the caller limits, applied to every document, and the Job
/// Template's profile, for its own Environments. Each attached template is
/// evaluated under its own profile ([`EnvironmentTemplate::profile`]): an
/// extension applies to the document that lists it.
///
/// Each attachment named like a `jobEnvironments` entry is unreachable from
/// the Job Template (§3.2.1 item 4). It gets a warning when the Job
/// Template lists `ENV_PARAMS` (without it the Job Template has no entries
/// that could name it, and the submission gets no warning it did not get
/// before the extension), and an error when its `activation` is `ALWAYS` or
/// `BY_DEFAULT`, it has parameters without a `default`, and a Step's
/// Sessions enter it.
///
/// §7.4.1 step 3: for each Step's frozen entry, in Step order and then
/// entry order:
///
/// 1. exactly one attached Environment with the entry's name: the entry
///    refers to it, whatever `required` says. When the Environment declares
///    no parameters and the entry has `required: false`, its `parameters`
///    are ignored, with a warning: it supplies no values and its values are
///    not resolved or checked, but it still requests an `ON_REQUEST`
///    Environment (§3.2.1 item 1);
/// 2. none: with `required: true` (or `required` absent) the submission is
///    rejected, naming the entry and the Environment; with
///    `required: false` the entry is ignored and a warning is recorded;
/// 3. two or more: the submission is rejected, naming the entry and the
///    attachments.
///
/// Each Step's disabled entries that name no Job Environment are matched
/// first, by the same rules, except that one matching no attachment has no
/// effect, whatever `required` says, and no constraint 5 or 6 check is
/// made, since it supplies nothing (§3.2.1). One that matches an
/// `ON_REQUEST` attachment has no effect; one that matches a `BY_DEFAULT`
/// one declines it for the Step; one that matches an `ALWAYS` one is an
/// error naming the Step and the Environment (§3.2 item 3.2, §7.4.1 step
/// 4); and one that matches an attachment whose Environment Template does
/// not list `ENV_PARAMS` has no effect, since that attachment behaves as
/// `ALWAYS` with no `enableIf` and no Step can leave it out (§4 item 4).
///
/// A Step with an error from §7.4.1 steps 1–4, here or in job creation, is
/// stopped: steps 5–8 are not taken for it. For every other Step, its
/// Sessions would enter every attached Environment with `activation:
/// ALWAYS` or from a document that does not list `ENV_PARAMS`, every one
/// with `BY_DEFAULT` it does not decline, and each one
/// with `activation: ON_REQUEST` that one of its enabled entries refers to.
/// For each one it enters that declares parameters, the Step gets
/// effective values from its entry if one refers to it and from the
/// `default`s otherwise (a Step whose entry names an ambiguous attachment
/// is stopped, so the one mistake is reported once). Then the attachments
/// and the
/// Job Environments are decided in the Job's Environment order, attached
/// ones first: one whose `exclusiveGroup` an earlier one that remains in
/// the Step's set already has is superseded, which disables it for the
/// Step, and its own `enableIf` is not evaluated (§4 item 7, §7.4.1 step
/// 6). For any other, its own `enableIf` (§4 item 6) is decided for the
/// Step with the values that are known (§3.2.2), and one it disables is
/// left out of the Step's set and values; for one with `activation:
/// ALWAYS`, a `false` result is an error naming the Step instead (§4 item
/// 4, §3.2.2 item 6). One it does not disable takes its group. Constraint
/// 5 (keys) is checked for every Environment the Step would enter; the
/// value checks (constraint 6) and constraint 7 (every parameter without a
/// `default` supplied) only for one that is not disabled (§7.4.1 step 7). A
/// literal `default` of an attached Environment is checked even if no Step
/// takes it (§4.5 item 2). Then the `WRAP_ACTIONS` single-layer rule is
/// checked over each Step's final Environment set, attached Environments
/// included (RFC 0008; plan R8). Last, each Step gets its combined host
/// requirements (§3.3.3): every Environment's `hostRequirements` resolved
/// with the Step's effective values for each Environment it enters,
/// combined with the Step's own (see
/// `host_requirements::combine_step_host_requirements`).
///
/// Each decision is kept per Step as an [`EnvironmentStatus`] for every
/// Environment the Step considered, the attachments' made here and the Job
/// Environments' started by job creation and finished here, with the Step
/// Environments' from job creation. `Step::environments`, the entered flags
/// the later checks read, and `CreatedJob::disabled_environments` are all
/// derived from them, and they are returned in
/// `CreatedJob::environment_statuses`, with each Step's `required: false`
/// entries that matched nothing.
///
/// Every violation, with job creation's own, is reported in one
/// `ModelValidation` error in §7.4.1 order, as
/// [`finish_errors`](super::finish_errors) describes: for `Submission`,
/// with every path starting with its document (`JobTemplate`,
/// `EnvironmentTemplate[i]`, or the attachment's label), when Environment
/// Templates are attached, and for `JobTemplate`, at the Job Template's
/// paths, when none is.
///
/// The function does not re-run the carried-forward resolved-value checks
/// on the attached Environments' own format strings, which job creation
/// runs on the Job Template's.
pub(crate) fn apply_environment_templates(
    job_template: &JobTemplate,
    body: super::Body<'_>,
    env_names: &[String],
    attached: &[AttachedEnvironmentTemplate<'_>],
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
) -> Result<CreatedJob, ModelError> {
    let super::Body {
        mut job,
        mut selections,
        job_envs,
        check_errors,
        staged: body_staged,
        limits,
    } = body;
    let job_envs = &job_envs;
    // Paths start with their document only when Environment Templates are
    // attached (see `finish_errors`).
    let job_path = if attached.is_empty() {
        Vec::new()
    } else {
        Documents { attached }.path(None)
    };
    // The position of `jobEnvironments[k]` in the Job's Environment order.
    let own_at = |k: usize| attached.len() + k;
    // Each Step's index, by name, to file an error naming several Steps
    // under the first.
    let step_index: HashMap<&str, usize> = job_template
        .steps
        .iter()
        .enumerate()
        .map(|(i, st)| (st.name.as_str(), i))
        .collect();
    let job_env_names: HashSet<&str> = job
        .job_environments
        .iter()
        .flatten()
        .map(|e| e.name.as_str())
        .collect();

    let mut staged = StagedErrors::default();
    let mut warnings = Vec::new();
    // `default`s resolve against the merged Job Parameters, without
    // `Job.Name` or `Job.EnvNames` (§4.5 item 1, §7.3.2; plan A21).
    let default_symtab = build_symbol_table(job_parameter_values)?;
    let job_lists_env_params = job
        .extensions
        .as_ref()
        .is_some_and(|exts| exts.contains(&ModelExtension::EnvParams));
    // The Job Environments job creation left for the group rule, per Step.
    let mut deferred: Vec<Vec<_>> = selections
        .iter_mut()
        .map(|selection| std::mem::take(&mut selection.deferred))
        .collect();
    let set = Attachments::new(
        attached,
        &job_env_names,
        job_lists_env_params,
        &default_symtab,
        ctx,
        staged.at(Stage::Validation, ANY, 0),
        &mut warnings,
    );

    // ── Resolve each Step's frozen entries ──
    // Per Step: the attachment each resolved entry refers to, with the
    // entry.
    // An entry whose `parameters` the attachment ignores (§3.2.1 item 1)
    // is kept, flagged, so that it still requests an `ON_REQUEST`
    // attachment, and supplies `no_parameters` instead.
    let no_parameters = BTreeMap::new();
    let mut resolved: Vec<Vec<ResolvedEntry<'_>>> = Vec::with_capacity(selections.len());
    // Per Step: the `BY_DEFAULT` attachments a disabled entry declines
    // (§3.2 item 3.2).
    let mut declined: Vec<HashSet<usize>> = Vec::with_capacity(selections.len());
    // Per Step: its enabled `required: false` entries that matched no
    // attachment (§3.2.1 item 2).
    let mut not_found: Vec<Vec<OptionalEntryNotFound>> = Vec::with_capacity(selections.len());
    for (i, selection) in selections.iter().enumerate() {
        let deps_path = path_field(
            &path_index(&path_field(&job_path, "steps"), i),
            "dependencies",
        );
        let mut step_resolved = Vec::new();
        let mut step_declined = HashSet::new();
        let mut step_not_found = Vec::new();
        // A disabled entry is resolved too, to learn the `activation` that
        // decides what it means (§3.2.1, §3.2 item 3.2). It supplies nothing,
        // so constraints 5 and 6 are not checked for it, and one that
        // matches no attachment has no effect, whatever `required` says
        // (item 2 does not apply). An ambiguous name is an error as for an
        // enabled entry (item 3).
        for entry in &selection.disabled {
            if !set.by_name.contains_key(entry.environment.as_str()) {
                continue;
            }
            let entry_path = path_index(&deps_path, entry.dependency_index);
            match set.match_entry(
                &entry.environment,
                true,
                path_field(&entry_path, "environment"),
                staged.at(Stage::Step3, i, entry.dependency_index),
                &mut warnings,
            ) {
                EntryMatch::Absent => {}
                EntryMatch::One(doc) => {
                    let de = &set.declaring[doc];
                    match de.activation {
                        EnvironmentActivation::OnRequest => {}
                        EnvironmentActivation::ByDefault => {
                            step_declined.insert(doc);
                        }
                        // From a document that does not list `ENV_PARAMS`:
                        // it behaves as `ALWAYS`, and the entry has no
                        // effect and is not an error (§4 item 4, §3.2 item
                        // 3.2, §7.4.1 step 4).
                        EnvironmentActivation::Always if de.pre_extension => {}
                        // No Step can leave it out (§3.2.2 item 6): a
                        // §7.4.1 step 4 error, which stops the Step.
                        EnvironmentActivation::Always => {
                            staged.at(Stage::Step4, i, entry.dependency_index).add(
                                &path_field(&entry_path, "enableIf"),
                                always_entry_disabled_message(
                                    &job_template.steps[i].name,
                                    &de.label,
                                ),
                            )
                        }
                    }
                }
                // An ambiguous name (reported) stops the Step after §7.4.1
                // step 4, so no value of its is computed and the rejected
                // entry does not also produce "must supply" or "does not
                // supply" errors.
                EntryMatch::Many(_) => {}
            }
        }
        for entry in &selection.frozen {
            let entry_path = path_index(&deps_path, entry.dependency_index);
            match set.match_entry(
                &entry.environment,
                entry.required,
                path_field(&entry_path, "environment"),
                staged.at(Stage::Step3, i, entry.dependency_index),
                &mut warnings,
            ) {
                // With `required: true` it is an error (reported).
                EntryMatch::Absent => {
                    if !entry.required {
                        step_not_found.push(OptionalEntryNotFound {
                            dependency_index: entry.dependency_index,
                            environment: entry.environment.clone(),
                        });
                    }
                }
                EntryMatch::One(doc) => {
                    let ignored = set.ignores_parameters(
                        doc,
                        entry.required,
                        &entry.parameters,
                        &entry_path,
                        &mut warnings,
                    );
                    step_resolved.push(ResolvedEntry {
                        doc,
                        entry,
                        path: entry_path,
                        ignored,
                    });
                }
                // An ambiguous name (reported) stops the Step after §7.4.1
                // step 4, so no value of its is computed and the rejected
                // entry does not also produce "must supply" or "does not
                // supply" errors.
                EntryMatch::Many(_) => {}
            }
        }
        resolved.push(step_resolved);
        declined.push(step_declined);
        not_found.push(step_not_found);
    }

    // ── Each Step's attached Environments, and its values for them ──
    // §3.2.2: the Step's effective values for each attached Environment its
    // Sessions would enter; then, unless it is superseded, the
    // Environment's own `enableIf` (§4 item 6) with the values that are
    // known; then, for one it does not disable, the value checks and
    // constraint 7 (§7.4.1 steps 6 and 7).
    // The table each attachment's `enableIf` resolves in, before its
    // `Env.*` symbols: the Job Parameters, `Job.Name`, and `Job.EnvNames`
    // (§7.3.2). Only an attachment that lists `ENV_PARAMS`,
    // and so `EXPR`, has an `enableIf`, so one table serves every
    // attachment.
    let mut base = default_symtab.clone();
    base.set("Job.Name", ExprValue::String(job.name.clone()))?;
    base.set("Job.EnvNames", env_names_value(env_names))?;
    // Per Step, index for index with the attachments: whether the Step
    // considered each and whether its Sessions enter it (`None` for an
    // `ON_REQUEST` one it does not request).
    let mut attached_statuses: Vec<Vec<Option<EnvironmentStatus>>> =
        Vec::with_capacity(job.steps.len());
    // Per attachment: whether its `enableIf` did not disable it for some
    // Step.
    let mut entered_by_some_step = vec![false; set.declaring.len()];
    // Whether some Step's decision was other than a failed `enableIf`.
    let mut decided_by_some_step = vec![false; set.declaring.len()];
    // Per Step: its values for the Job Environments the group rule decided
    // here, in written order.
    let mut deferred_values: Vec<Vec<job::EnvironmentParameterValues>> =
        Vec::with_capacity(job.steps.len());
    // Per Step: the indices in `jobEnvironments` of the ones decided here
    // that are superseded or that their own `enableIf` disables, each with
    // which of the two.
    let mut left_out_job_environments: Vec<Vec<(usize, EnvironmentStatus)>> =
        Vec::with_capacity(job.steps.len());
    // Per Step, by position in the Job's Environment order (the
    // attachments, then `jobEnvironments`): for a superseded one, the
    // position of the member of its group the Step's Sessions enter.
    let combined_count = own_at(job_envs.all.len());
    let mut superseded_by: Vec<Vec<Option<usize>>> = Vec::with_capacity(job.steps.len());
    let mut step_environment_parameters = Vec::with_capacity(job.steps.len());
    // §7.4.1: a Step with an error from steps 1–4 has no fixed set of
    // Environments, so steps 5–8 are not taken for it.
    let stopped: Vec<bool> = (0..job.steps.len())
        .map(|i| body_staged.stopped(i) || staged.stopped(i))
        .collect();
    for (i, step) in job.steps.iter().enumerate() {
        if stopped[i] {
            attached_statuses.push(vec![None; set.declaring.len()]);
            superseded_by.push(vec![None; combined_count]);
            step_environment_parameters.push(Vec::new());
            left_out_job_environments.push(Vec::new());
            deferred_values.push(Vec::new());
            if let Some(step_deferred) = deferred.get_mut(i) {
                step_deferred.clear();
            }
            continue;
        }
        let step_path = path_index(&path_field(&job_path, "steps"), i);
        let mut statuses = Vec::with_capacity(set.declaring.len());
        let mut values = Vec::new();
        let mut missing = Vec::new();
        let mut unresolved = HashSet::new();
        let mut failed = HashSet::new();
        let step_env_names: HashSet<&str> = step
            .step_environments
            .iter()
            .flatten()
            .map(|e| e.name.as_str())
            .collect();
        // §4 item 7, §7.4.1 step 6: the groups an Environment the Step's
        // Sessions enter already has, in the Job's Environment order, so
        // the later ones of each are superseded, each with the position of
        // that Environment, the group's first that remains in the set.
        let mut taken: HashMap<&str, usize> = HashMap::new();
        let mut step_superseded_by = vec![None; combined_count];
        // The first attachment in `missing`, to file the Step's constraint
        // 7 error under.
        let mut first_missing = None;
        for (doc, (de, att_ctx)) in set.declaring.iter().zip(&set.contexts).enumerate() {
            let entry = resolved
                .get(i)
                .and_then(|r| r.iter().find(|r| r.doc == doc));
            // §4 item 4, §3.2 item 3.2: every `ALWAYS` attachment, every
            // `BY_DEFAULT` one a disabled entry of the Step does not
            // decline, and each `ON_REQUEST` one an enabled entry of the
            // Step refers to. A declined one's `enableIf` is not evaluated
            // for the Step, and no value check applies.
            let enters = match de.activation {
                EnvironmentActivation::Always => true,
                EnvironmentActivation::ByDefault => {
                    !declined.get(i).is_some_and(|d| d.contains(&doc))
                }
                EnvironmentActivation::OnRequest => entry.is_some(),
            };
            if !enters {
                // Only a declined `BY_DEFAULT` one is considered.
                statuses.push(
                    (de.activation == EnvironmentActivation::ByDefault)
                        .then_some(EnvironmentStatus::Declined),
                );
                continue;
            }
            let supplied = entry.map(|r| Supplied {
                params: if r.ignored {
                    &no_parameters
                } else {
                    &r.entry.parameters
                },
                symtab: &r.entry.symtab,
                entry_path: r.path.clone(),
            });
            let policy = ResolvePolicy::new(att_ctx);
            let pending = de.values_for_step(
                supplied.as_ref(),
                &default_symtab,
                policy,
                staged.at(Stage::Step5, i, doc),
            );
            // Superseded by an earlier one of its group: disabled for the
            // Step (§3.2.2), so its values are not checked, and its own
            // `enableIf` is not evaluated (§4 item 7, §7.4.1 step 6).
            if let Some(&winner) = de.env.group().and_then(|group| taken.get(group)) {
                statuses.push(Some(EnvironmentStatus::Superseded));
                step_superseded_by[doc] = Some(winner);
                continue;
            }
            let decision = de.decide(&pending, &step.name, &base, policy);
            decided_by_some_step[doc] |= decision != EnableIf::Failed;
            if let Some(group) = de.env.group().filter(|_| decision != EnableIf::Disabled) {
                taken.insert(group, doc);
            }
            // A failed `enableIf` is checked as if it does not disable the
            // Environment, so the Step's other mistakes are reported too.
            let not_disabled = decision != EnableIf::Disabled;
            entered_by_some_step[doc] |= not_disabled;
            statuses.push(Some(if not_disabled {
                EnvironmentStatus::Entered
            } else {
                EnvironmentStatus::Disabled
            }));
            match decision {
                EnableIf::Disabled => {}
                EnableIf::Enabled | EnableIf::Unresolved | EnableIf::Failed => {
                    let (v, m) = de.commit(pending, &step.name, staged.at(Stage::Step7, i, doc));
                    values.extend(v);
                    if !m.is_empty() {
                        match decision {
                            EnableIf::Unresolved => {
                                unresolved.insert(de.label.as_str());
                            }
                            EnableIf::Failed => {
                                failed.insert(de.label.as_str());
                            }
                            _ => {}
                        }
                    }
                    let before = missing.len();
                    set.sort_missing(
                        doc,
                        m,
                        &step_env_names,
                        &step_path,
                        &mut missing,
                        staged.at(Stage::Step7, i, doc),
                    );
                    if missing.len() > before && first_missing.is_none() {
                        first_missing = Some(doc);
                    }
                }
            }
        }
        // A disabled entry for an attachment the Step would still enter, an
        // `ALWAYS` one, stops the Step, so no message here needs the
        // disabled note.
        report_missing(
            &step_path,
            &missing,
            EVERY_ATTACHED,
            &HashSet::new(),
            &unresolved,
            &failed,
            staged.at(Stage::Step7, i, first_missing.unwrap_or(ANY)),
        );
        // The Job Environments the Step's Sessions would enter, with the
        // values job creation computed for them. The attached ones come
        // first in the Job's Environment order. One that is not superseded
        // gets its own `enableIf` decided now (§7.4.1 step 6), and one that
        // it does not disable its value checks and constraint 7 (step 7), at
        // the Job Template's paths.
        let mut job_missing = Vec::new();
        let mut job_unresolved = HashSet::new();
        let mut job_failed = HashSet::new();
        let mut step_deferred_values = Vec::new();
        let mut step_left_out = Vec::new();
        let mut first_job_missing = None;
        if let Some(step_deferred) = deferred.get_mut(i) {
            for d in std::mem::take(step_deferred) {
                let de = &job_envs.environments[d.environment];
                // `taken` holds the attachments' groups, then those of the
                // earlier deferred ones. A superseded one's own `enableIf`
                // is not evaluated (§4 item 7, §7.4.1 step 6).
                if let Some(&winner) = de.env.group().and_then(|group| taken.get(group)) {
                    step_left_out.push((d.position, EnvironmentStatus::Superseded));
                    step_superseded_by[own_at(d.position)] = Some(winner);
                    continue;
                }
                let decision = de.decide(
                    &d.pending,
                    &step.name,
                    &job_envs.enable_if_base,
                    ResolvePolicy::new(ctx),
                );
                if decision == EnableIf::Disabled {
                    step_left_out.push((d.position, EnvironmentStatus::Disabled));
                    continue;
                }
                if let Some(group) = de.env.group() {
                    taken.insert(group, own_at(d.position));
                }
                let mut scratch = ValidationErrors::default();
                let (v, m) = de.commit(d.pending, &step.name, &mut scratch);
                let at = staged.at(Stage::Step7, i, own_at(d.position));
                for mut e in scratch.errors {
                    e.path = job_path.iter().cloned().chain(e.path).collect();
                    at.errors.push(e);
                }
                step_deferred_values.extend(v);
                if !m.is_empty() {
                    match decision {
                        EnableIf::Unresolved => {
                            job_unresolved.insert(de.label.as_str());
                        }
                        EnableIf::Failed => {
                            job_failed.insert(de.label.as_str());
                        }
                        _ => {}
                    }
                    first_job_missing.get_or_insert(d.position);
                    job_missing.push((de.label.clone(), m));
                }
            }
        }
        left_out_job_environments.push(step_left_out);
        superseded_by.push(step_superseded_by);
        report_missing(
            &step_path,
            &job_missing,
            EVERY_JOB_ENVIRONMENT,
            &HashSet::new(),
            &job_unresolved,
            &job_failed,
            staged.at(Stage::Step7, i, first_job_missing.map_or(ANY, own_at)),
        );
        deferred_values.push(step_deferred_values);
        attached_statuses.push(statuses);
        step_environment_parameters.push(values);
    }
    drop(resolved);
    for (selection, left_out) in selections.iter_mut().zip(left_out_job_environments) {
        for (position, status) in left_out {
            selection.job_environment_statuses[position] = Some(status);
        }
    }
    // §7.4.1 step 6: a failed `enableIf` is reported once for all the
    // Steps it was resolved for, filed under the first of them, at its
    // Environment's place in the Job's Environment order.
    let first_step = |name: &str| step_index.get(name).copied().unwrap_or(ANY);
    for (doc, de) in set.declaring.iter().enumerate() {
        de.report_enable_if_errors(|first, path, message| {
            staged
                .at(Stage::Step6, first_step(first), doc)
                .add(&path, message);
        });
    }
    for (de, &position) in job_envs.environments.iter().zip(&job_envs.positions) {
        de.report_enable_if_errors(|first, path, message| {
            let path: Vec<PathElement> = job_path.iter().cloned().chain(path).collect();
            staged
                .at(Stage::Step6, first_step(first), own_at(position))
                .add(&path, message);
        });
    }
    // An unreachable attachment with parameters no Step can supply is an
    // error when some Step's Sessions enter it; one whose own `enableIf`
    // disables it, or an earlier one of whose group supersedes it, for
    // every Step is not entered (§3.2.2), and a stopped Step enters none.
    // One whose `enableIf` failed for a Step is checked as if that Step
    // enters it. Not about one Step, so filed after the Steps' errors of
    // step 7.
    set.report_unreachable(
        |doc, path, message| staged.at(Stage::Step7, ANY, doc).add(path, message),
        |doc| entered_by_some_step[doc],
        |doc| entered_by_some_step[doc] && !decided_by_some_step[doc],
    );

    // ── Convert the attached Environments ──
    let mut environments = Vec::with_capacity(attached.len());
    let mut environment_documents = Vec::with_capacity(attached.len());
    for (doc, (att, att_ctx)) in attached.iter().zip(&set.contexts).enumerate() {
        let mut symtab = default_symtab.clone();
        if att_ctx.profile.has_extension(ModelExtension::Expr) {
            symtab.set("Job.Name", ExprValue::String(job.name.clone()))?;
            symtab.set("Job.EnvNames", env_names_value(env_names))?;
        }
        environments.push(super::convert_environment_with_symtab(
            att.template.environment(),
            Some(&symtab),
        ));
        environment_documents.push(job::Document::environment_template(doc, att.label));
    }

    // ── Combine ──
    let own = job.job_environments.take().unwrap_or_default();
    environment_documents.extend(std::iter::repeat_n(job::Document::JobTemplate, own.len()));
    environments.extend(own);
    let docs = Documents { attached };
    let own_count = environments.len() - attached.len();
    debug_assert_eq!(
        own_count,
        job_template.job_environments.as_ref().map_or(0, Vec::len)
    );
    let sources: Vec<super::host_requirements::EnvironmentSource<'_>> = environment_documents
        .iter()
        .enumerate()
        .map(|(e, document)| {
            if e < attached.len() {
                super::host_requirements::EnvironmentSource {
                    environment: attached[e].template.environment(),
                    document,
                    path: path_field(&docs.path(Some(e)), "environment"),
                    ctx: &set.contexts[e],
                }
            } else {
                let j = e - attached.len();
                super::host_requirements::EnvironmentSource {
                    environment: &job_template.job_environments.as_deref().unwrap_or(&[])[j],
                    document,
                    path: path_index(&path_field(&job_path, "jobEnvironments"), j),
                    ctx,
                }
            }
        })
        .collect();
    let mut per_step = step_environment_parameters.into_iter();
    let mut deferred_per_step = deferred_values.into_iter();
    let mut entered_per_step = Vec::with_capacity(job.steps.len());
    // Each Step's status for every Environment of the Job, by index into
    // `environments`: the attached ones, then the Job Template's.
    let statuses_per_step: Vec<Vec<Option<EnvironmentStatus>>> = attached_statuses
        .into_iter()
        .zip(&selections)
        .map(|(mut statuses, selection)| {
            statuses.extend(&selection.job_environment_statuses);
            statuses
        })
        .collect();
    for ((step, statuses), stopped) in job.steps.iter_mut().zip(&statuses_per_step).zip(&stopped) {
        // The attachments' values, then the Job Environments', which were
        // committed in written order. Job creation proper sets no values.
        debug_assert!(step.environment_parameters.is_none());
        let mut values = per_step.next().unwrap_or_default();
        values.extend(deferred_per_step.next().unwrap_or_default());
        step.environment_parameters = (!values.is_empty()).then_some(values);
        // A Step stopped after §7.4.1 step 4 has no fixed set; it is
        // counted as entering none, so steps 7 and 8 leave it alone.
        let entered: Vec<bool> = statuses
            .iter()
            .map(|s| *s == Some(EnvironmentStatus::Entered) && !stopped)
            .collect();
        entered_per_step.push(entered);
    }
    let mut disabled_environments = Vec::with_capacity(job.steps.len());
    let mut environment_statuses = Vec::with_capacity(job.steps.len());
    // The scope an Environment of the Job has, from its document.
    let scope_of = |document: &job::Document| {
        if document.is_job_template() {
            EnvironmentScope::Job
        } else {
            EnvironmentScope::Attached
        }
    };
    for (((((step, selection), entered), statuses), not_found), superseded_by) in job
        .steps
        .iter_mut()
        .zip(&selections)
        .zip(&entered_per_step)
        .zip(&statuses_per_step)
        .zip(not_found)
        .zip(&superseded_by)
    {
        step.environments = (!entered.iter().all(|e| *e)).then(|| {
            environments
                .iter()
                .zip(&environment_documents)
                .zip(entered)
                .filter(|(_, e)| **e)
                .map(|((env, document), _)| job::EnvironmentRef {
                    document: document.clone(),
                    environment: env.name.clone(),
                })
                .collect()
        });
        let considered: Vec<StepEnvironmentStatus> = environments
            .iter()
            .zip(&environment_documents)
            .zip(statuses)
            .zip(&sources)
            .zip(superseded_by)
            .filter_map(|((((env, document), status), source), winner)| {
                let status = (*status)?;
                debug_assert_eq!(
                    winner.is_some(),
                    status == EnvironmentStatus::Superseded,
                    "a winner is recorded exactly for a superseded Environment"
                );
                Some(StepEnvironmentStatus {
                    name: env.name.clone(),
                    document: document.clone(),
                    scope: scope_of(document),
                    status,
                    exclusive_group: source.environment.exclusive_group.clone(),
                    superseded_by: winner.map(|w| SupersedingEnvironment {
                        name: environments[w].name.clone(),
                        document: environment_documents[w].clone(),
                        scope: scope_of(&environment_documents[w]),
                    }),
                })
            })
            .chain(selection.step_environments.iter().map(|(name, enabled)| {
                StepEnvironmentStatus {
                    name: name.clone(),
                    document: job::Document::JobTemplate,
                    scope: EnvironmentScope::Step,
                    status: if *enabled {
                        EnvironmentStatus::Entered
                    } else {
                        EnvironmentStatus::Disabled
                    },
                    // Template validation rejects `exclusiveGroup` on a
                    // `stepEnvironments` entry, so none is superseded.
                    exclusive_group: None,
                    superseded_by: None,
                }
            }))
            .collect();
        let left_out = |s: &&StepEnvironmentStatus| {
            matches!(
                s.status,
                EnvironmentStatus::Disabled | EnvironmentStatus::Superseded
            )
        };
        disabled_environments.push(StepDisabledEnvironments {
            environments: considered
                .iter()
                .filter(|s| s.scope != EnvironmentScope::Step)
                .filter(left_out)
                .map(|s| job::EnvironmentRef {
                    document: s.document.clone(),
                    environment: s.name.clone(),
                })
                .collect(),
            step_environments: considered
                .iter()
                .filter(|s| s.scope == EnvironmentScope::Step)
                .filter(left_out)
                .map(|s| s.name.clone())
                .collect(),
        });
        environment_statuses.push(StepEnvironmentStatuses {
            environments: considered,
            not_found,
        });
    }
    check_wrap_layers(
        &job,
        &environments,
        &environment_documents,
        &job_path,
        &stopped,
        &mut staged,
    );
    // ── §3.3.3: each Step's combined host requirements ──
    // Last in §7.4.1: they are resolved with the Step's effective values, and
    // only for the Environments the Step's Sessions enter. The Step's own
    // discarded requirements come first.
    let mut discarded_host_requirements: Vec<Vec<DiscardedHostRequirement>> = selections
        .iter()
        .map(|selection| selection.discarded_host_requirements.clone())
        .collect();
    super::host_requirements::combine_step_host_requirements(
        &mut job,
        &sources,
        &entered_per_step,
        &job_path,
        &default_symtab,
        &env_names_value(env_names),
        super::EvalBudgets::from_ctx(ctx),
        &mut discarded_host_requirements,
        &mut staged,
    );
    super::finish_errors(check_errors, body_staged, staged, !attached.is_empty())?;
    limits?;

    job.job_environments = (!environments.is_empty()).then_some(environments);
    Ok(CreatedJob {
        job,
        environment_documents,
        warnings,
        disabled_environments,
        environment_statuses,
        discarded_host_requirements,
    })
}

/// `Job.EnvNames` as an expression value: a `list[string]`.
pub(crate) fn env_names_value(names: &[String]) -> ExprValue {
    ExprValue::make_list(
        names.iter().cloned().map(ExprValue::String).collect(),
        openjd_expr::ExprType::STRING,
    )
    // A list of strings is always constructible.
    .unwrap_or(ExprValue::Null)
}

/// The `WRAP_ACTIONS` single-layer rule (RFC 0008) over each Step's final
/// Environment set: the combined Job Environments it enters
/// (`environments`, with their `documents`) and its own Step Environments,
/// as §7.4.1 step 6 leaves them (§7.4.1 step 7). Template validation checks the Job Template's
/// own layers that are certainly entered (entries and Environments whose
/// `enableIf` is absent or a boolean scalar); this check also covers an
/// `enableIf` expression and the attached Environments (plan R8).
fn check_wrap_layers(
    job: &job::Job,
    environments: &[job::Environment],
    documents: &[job::Document],
    job_path: &[PathElement],
    stopped: &[bool],
    staged: &mut StagedErrors,
) {
    let wraps = |env: &job::Environment| {
        env.script
            .as_ref()
            .is_some_and(|s| s.actions.has_any_wrap_hook())
    };
    for (i, step) in job.steps.iter().enumerate() {
        if stopped[i] {
            continue;
        }
        let mut layers: Vec<String> = environments
            .iter()
            .zip(documents)
            .filter(|(env, doc)| wraps(env) && step.enters_environment(doc, &env.name))
            .map(|(env, doc)| match doc {
                job::Document::JobTemplate => format!("'{}'", env.name),
                other => format!("'{}' ({other})", env.name),
            })
            .collect();
        layers.extend(
            step.step_environments
                .iter()
                .flatten()
                .filter(|env| wraps(env))
                .map(|env| format!("'{}' (stepEnvironments)", env.name)),
        );
        if layers.len() > 1 {
            staged.at(Stage::Step7, i, ANY).add(
                &path_index(&path_field(job_path, "steps"), i),
                wrap_layers_message(&layers),
            );
        }
    }
}

/// The single-layer rule's message for a Step whose Sessions would enter
/// the wrapping Environments `layers` (two or more).
fn wrap_layers_message(layers: &[String]) -> String {
    format!(
        "its Sessions would enter {} Environments that define wrap hooks (onWrapEnvEnter, \
         onWrapTaskRun, onWrapEnvExit): {}. A Session may have at most one Environment defining \
         wrap hooks (RFC 0008).",
        layers.len(),
        capped_name_list(layers.iter().map(String::as_str), layers.len())
    )
}

/// The `WRAP_ACTIONS` single-layer rule for [`check_environment_templates`]:
/// for each Step, the wrapping Environments its Sessions certainly enter,
/// in the order a Session enters them: the attachments with `activation:
/// ALWAYS` or from a document that does not list `ENV_PARAMS` (§4 item
/// 4), the `BY_DEFAULT` ones the Step has no entry for or an enabled
/// one, and the `ON_REQUEST` ones an enabled entry resolved to, then the
/// Job Environments in the same way, then its Step Environments; each only
/// when its own `enableIf` is absent or the scalar `true` (§4, after item
/// 10), except an `ALWAYS` one, which cannot be disabled or declined
/// (§3.2.2 item 6), and, for one with an `exclusiveGroup`, only when every
/// earlier one of its group is known not to be entered (`entered`, per Step
/// and Environment, from [`certainly_entered_per_step`]). An entry or
/// Environment whose `enableIf` is an expression (even a constant one) is
/// decided at job creation, which checks the rule again over the full set;
/// one whose `enableIf` is the scalar `false` is not entered, and an entry
/// whose `enableIf` is the scalar `false` declines a `BY_DEFAULT` one (§3.2
/// item 3.2). Template validation counts the Job Template's own ungrouped
/// layers the same way, so a Step with only those passes here too.
fn check_attached_wrap_layers(
    job_template: &JobTemplate,
    attached: &[AttachedEnvironmentTemplate<'_>],
    doc_names: &[String],
    entered: &[Vec<bool>],
    job_path: &[PathElement],
    errors: &mut ValidationErrors,
) {
    let wraps = |env: &crate::template::Environment| {
        env.script
            .as_ref()
            .is_some_and(|s| s.actions.has_any_wrap_hook())
    };
    let wrapping_attached: Vec<usize> = (0..attached.len())
        .filter(|d| wraps(attached[*d].template.environment()))
        .collect();
    let own_envs = job_template.job_environments.as_deref().unwrap_or(&[]);
    for (i, step) in job_template.steps.iter().enumerate() {
        let mut layers: Vec<String> = wrapping_attached
            .iter()
            .filter(|d| entered[i][**d])
            .map(|d| {
                format!(
                    "'{}' ({})",
                    attached[*d].template.environment().name,
                    doc_names[*d]
                )
            })
            .collect();
        // Every Step is checked: one whose layers are all ungrouped
        // Environments of the Job Template passed template validation,
        // which counts them the same way, so it adds no error here.
        layers.extend(
            own_envs
                .iter()
                .enumerate()
                .filter(|(j, env)| wraps(env) && entered[i][attached.len() + j])
                .map(|(_, env)| format!("'{}'", env.name)),
        );
        layers.extend(
            step.step_environments
                .iter()
                .flatten()
                .filter(|env| wraps(env) && env.literal_enabled() == Some(true))
                .map(|env| format!("'{}' (stepEnvironments)", env.name)),
        );
        if layers.len() > 1 {
            errors.add(
                &path_index(&path_field(job_path, "steps"), i),
                wrap_layers_message(&layers),
            );
        }
    }
}

/// For each Step, whether its Sessions are certainly entered into each
/// Environment of the Job, attached ones first and then the Job Template's
/// own, in the Job's Environment order (§1.1 item 7): each one's state from
/// what the Step's entry for it is known to be (`known`, per Step and
/// attachment; for a Job Environment, the Step's entry as written), then
/// the `exclusiveGroup` bullet (§4, after item 10), so a grouped one counts
/// only when every earlier one of its group is known not to be entered.
fn certainly_entered_per_step(
    job_template: &JobTemplate,
    attached: &[AttachedEnvironmentTemplate<'_>],
    known: &[Vec<KnownEntry>],
) -> Vec<Vec<bool>> {
    // §4 item 4: an attachment from a document that does not list
    // `ENV_PARAMS` is known to be entered, whatever the Step's entry is.
    let lists_env_params: Vec<bool> = attached
        .iter()
        .map(|a| {
            a.template
                .profile()
                .has_extension(ModelExtension::EnvParams)
        })
        .collect();
    let own_envs = job_template.job_environments.as_deref().unwrap_or(&[]);
    job_template
        .steps
        .iter()
        .zip(known)
        .map(|(step, known)| {
            let states = attached
                .iter()
                .zip(known)
                .zip(&lists_env_params)
                .map(|((a, k), lists)| {
                    let env = a.template.environment();
                    (env.group(), env.known_state_in(*lists, *k))
                })
                .chain(own_envs.iter().map(|env| {
                    (
                        env.group(),
                        env.known_state(KnownEntry::in_step(step, &env.name)),
                    )
                }));
            apply_exclusive_groups(states)
                .into_iter()
                .map(|s| s == Known::Entered)
                .collect()
        })
        .collect()
}

/// Check a Job Template against the Environment Templates it would be
/// submitted with, before any Job Parameter values are known (Template
/// Schemas §3.2, last paragraph: "a validator given the Environment
/// Templates can check constraints 5–7 early"; prototype plan A9).
///
/// This runs the checks of
/// [`create_job_with_environment_templates`](super::create_job_with_environment_templates)'s
/// submission stage that need no Job Parameter values, with the same
/// messages and paths:
///
/// - the attachment-level conditions: an attachment named like a Job
///   Environment (a warning when the Job Template lists `ENV_PARAMS`), an
///   ambiguous name, and every literal `default` (§4.5 item 2);
/// - each `environment:` entry that names no Job Environment and whose
///   `enableIf` is absent or the boolean scalar `true`, resolved by name
///   (§3.2.1 items 1–3), with a `required: false` entry that resolves to
///   nothing reported as a warning, and one whose `parameters` an
///   attachment that declares none ignores (§3.2.1 item 1) reported as a
///   warning and checked as an entry with no `parameters`. An entry whose
///   `enableIf` is `false` is resolved too, but supplies nothing and has no effect when it matches
///   no attachment (§3.2.1): it declines a `BY_DEFAULT` attachment, which
///   the checks below then leave out for its Step, requests no
///   `ON_REQUEST` one, is an error for an `ALWAYS` one, naming the
///   Step and the Environment (§3.2 item 3.2), and has no effect on one
///   whose Environment Template does not list `ENV_PARAMS`, which behaves
///   as `ALWAYS` (§4 item 4) and is entered whatever the entry says; an
///   ambiguous name is an error either way. One whose `enableIf` is an expression (an expression
///   field, §7.6, so even a constant one such as `"true"`) is checked only
///   when the Job is created, and so is every attachment it may name, for
///   its Step;
/// - for an entry that resolves, constraint 5 (keys); and, when the
///   attachment's `activation` is `ALWAYS` (§3.2.2 item 6) or its own
///   `enableIf` is absent or `true`, or certainly
///   unresolved for the Step (it reads only values that stay unknown; see
///   `enable_if_certainly_unresolved`), constraint 6 for literal values
///   (a PATH value of any form is used as given, §4.5 item 3) and the
///   single-expression rule for a list-typed parameter;
/// - constraint 7 over every Step and every reachable attached
///   Environment it enters that is `ALWAYS` or whose own `enableIf` is
///   absent or `true`, or certainly unresolved (with the unresolved note):
///   those with `activation: ALWAYS`, those with `BY_DEFAULT` it does not
///   decline, and each one with `activation: ON_REQUEST` that one of its
///   enabled entries refers to. When a Step's entry for an `ALWAYS`
///   Environment with missing parameters was disabled by a scalar
///   `enableIf: false`, the message says so. An attachment whose
///   `enableIf` is `false` is disabled, unless it is `ALWAYS`, so neither
///   its values nor constraint 7 apply (Template Schemas §3.2.2); any other
///   one whose `enableIf` is
///   an expression is undetermined, so a violation is not certain here
///   (§7.6 rule 5) and job creation makes those checks. Nor do they apply
///   to an attachment an earlier one of its `exclusiveGroup` supersedes or
///   may supersede (§4 item 7, and the bullets after item 10): an
///   attachment's state for the Step goes through the group rule, and only
///   one known to be entered is checked;
/// - the same checks for each Job Environment with an `exclusiveGroup`
///   that the Step is known to enter, given the attachments: template
///   validation, which does not know them, leaves these to job creation.
///   With `attached` empty, every such Job Environment that is first in
///   its group is checked;
/// - the unreachable attachments' parameters without a `default`: an error
///   for one with `activation: ALWAYS`, or `BY_DEFAULT` with its own
///   `enableIf` absent, `true`, or certainly unresolved, that some Step is
///   known to enter (no entry decides it, so this holds unless an earlier
///   attachment of its `exclusiveGroup` supersedes or may supersede it for
///   every Step), reported after the Steps' values, as job creation orders
///   them;
/// - the `WRAP_ACTIONS` single-layer rule (RFC 0008) for each Step (one
///   whose only layers are the Job Template's own ungrouped ones passed
///   template validation, which counts them the same way; it counts no
///   grouped one), over the wrapping
///   Environments its Sessions certainly enter: attachments with `activation:
///   ALWAYS` or from a document that does not list `ENV_PARAMS` (whatever
///   the Step's entry for one says), those with `BY_DEFAULT` that no entry
///   of the Step disables or
///   may disable, and the `ON_REQUEST` ones an enabled entry resolved to,
///   Job Environments in the same way, with an entry whose `enableIf` is
///   absent or the scalar `true` enabled,
///   and its Step Environments, each only when its own `enableIf` is absent
///   or the scalar `true` or it is `ALWAYS`, and, for one with an
///   `exclusiveGroup`, only when every earlier one of its group is known
///   not to be entered, in job creation's form (job
///   creation may list more layers, when an `enableIf` expression on an
///   entry or an Environment brings one in);
/// - the Steps' combined host requirements (§3.3.3) over the requirements
///   that are literals (no format-string expression): each Step's own and
///   those of the Environments its Sessions certainly enter, as for the
///   single-layer rule (so not one whose own `enableIf` is the scalar
///   `false` or an expression, unless it is `ALWAYS`). A conflict among them is a conflict of the full
///   combination, reported in job creation's form; when a non-literal
///   requirement of the same capability also takes part, job creation may
///   name other sources or bounds.
///
/// Format-string values, non-literal `default`s, and non-literal
/// `hostRequirements` are not resolved: they need Job Parameter values, so
/// they are checked when the Job is created. Entries that name an
/// ungrouped `jobEnvironments` entry were
/// checked by template validation and are not checked again.
///
/// `ctx` supplies the caller limits, as for job creation; the attachments
/// are checked under their own profiles.
///
/// Returns the warnings, or one `ModelValidation` error holding every
/// violation, shaped as job creation's is: for `Submission`, with every
/// path starting with its document, when Environment Templates are
/// attached, and for `JobTemplate`, at the Job Template's paths, when none
/// is. The errors are in this function's own order: the attachment-level
/// conditions, then Step by Step the entries' resolution and the values,
/// then the unreachable attachments, the wrap layers and the host
/// requirements. It reports every violation it is certain of, so it can
/// report more than job creation, which orders its errors by §7.4.1 step
/// and takes no step after step 4 for a Step with an error from steps 1–4
/// (a value error of a Step whose entry is unresolved, say, is reported
/// here and not by job creation until the entry resolves). Job creation
/// may also list more Environments in a Step's constraint 7 error (this
/// one leaves out those it cannot know the Step enters).
pub fn check_environment_templates(
    job_template: &JobTemplate,
    attached: &[AttachedEnvironmentTemplate<'_>],
    ctx: &ValidationContext,
) -> Result<Vec<SubmissionWarning>, ModelError> {
    // Shaped by the inputs, as job creation's error is: paths start with
    // their document only when Environment Templates are attached.
    let job_path = if attached.is_empty() {
        Vec::new()
    } else {
        Documents { attached }.path(None)
    };
    let job_env_names: HashSet<&str> = job_template
        .job_environments
        .iter()
        .flatten()
        .map(|e| e.name.as_str())
        .collect();

    let mut errors = ValidationErrors::default();
    let mut warnings = Vec::new();
    // Literal `default`s read no symbols, and the other `default`s are not
    // resolved here.
    let no_symbols = openjd_expr::SymbolTable::new();
    let set = Attachments::new(
        attached,
        &job_env_names,
        job_template
            .profile()
            .has_extension(ModelExtension::EnvParams),
        &no_symbols,
        ctx,
        &mut errors,
        &mut warnings,
    );
    // Per Step: the attachment each resolved entry refers to, with the
    // entry's values and path. Ambiguous names, and the names of entries
    // whose `enableIf` is an expression, are skipped for the Step.
    let empty = BTreeMap::new();
    let mut resolved: Vec<Vec<(usize, Supplied<'_>)>> = Vec::new();
    let mut unapplied: Vec<HashSet<usize>> = Vec::new();
    // Per Step: the attachments an ambiguous entry named, a subset of
    // `unapplied`, for which job creation computes no values.
    let mut ambiguous: Vec<HashSet<usize>> = Vec::new();
    // Per Step: the names of entries a scalar `enableIf: false` disabled.
    let mut disabled: Vec<Vec<&str>> = Vec::new();
    // Per Step: the `BY_DEFAULT` attachments such an entry declines, and
    // the attachments an entry whose `enableIf` is an expression may name
    // (§3.2 item 3.2).
    let mut declined: Vec<HashSet<usize>> = Vec::new();
    let mut undetermined: Vec<HashSet<usize>> = Vec::new();
    for (i, step) in job_template.steps.iter().enumerate() {
        let deps_path = path_field(
            &path_index(&path_field(&job_path, "steps"), i),
            "dependencies",
        );
        let mut step_resolved = Vec::new();
        let mut step_unapplied = HashSet::new();
        let mut step_ambiguous = HashSet::new();
        let mut step_disabled = Vec::new();
        let mut step_declined = HashSet::new();
        let mut step_undetermined = HashSet::new();
        let mut seen = HashSet::new();
        for (j, dep) in step.dependencies.iter().flatten().enumerate() {
            let Some(name) = dep.environment() else {
                continue;
            };
            // A Job Environment entry was checked at template validation;
            // a repeated entry is a template-validation error.
            if job_env_names.contains(name) || !seen.insert(name) {
                continue;
            }
            let entry_path = path_index(&deps_path, j);
            match dep.literal_enabled() {
                Some(true) => {}
                // A disabled entry supplies nothing, but its name is
                // resolved to learn what it means (§3.2 item 3.2, §3.2.1),
                // as `apply_environment_templates` does: one that matches
                // no attachment has no effect.
                Some(false) => {
                    step_disabled.push(name);
                    if !set.by_name.contains_key(name) {
                        continue;
                    }
                    match set.match_entry(
                        name,
                        true,
                        path_field(&entry_path, "environment"),
                        &mut errors,
                        &mut warnings,
                    ) {
                        EntryMatch::Absent => {}
                        EntryMatch::One(doc) => {
                            let de = &set.declaring[doc];
                            match de.activation {
                                EnvironmentActivation::OnRequest => {}
                                EnvironmentActivation::ByDefault => {
                                    step_declined.insert(doc);
                                }
                                // No effect on a pre-extension one, which
                                // behaves as `ALWAYS` (§4 item 4).
                                EnvironmentActivation::Always if de.pre_extension => {}
                                EnvironmentActivation::Always => errors.add(
                                    &path_field(&entry_path, "enableIf"),
                                    always_entry_disabled_message(&step.name, &de.label),
                                ),
                            }
                        }
                        EntryMatch::Many(many) => {
                            step_unapplied.extend(many.iter().copied());
                            step_ambiguous.extend(many.iter().copied());
                        }
                    }
                    continue;
                }
                // Whether the entry is enabled is known only when the Job
                // is created, so the Environment it may name is not checked
                // for this Step (§3.2, checked-at paragraph). Its name is
                // resolved either way (§3.2.1), so a name that two
                // attachments define is an error whatever the `enableIf`
                // resolves to, and is reported now (§7.6 rule 5).
                None => {
                    if let Some(candidates) = set.by_name.get(name) {
                        if candidates.len() > 1 {
                            set.match_entry(
                                name,
                                true,
                                path_field(&entry_path, "environment"),
                                &mut errors,
                                &mut warnings,
                            );
                            // As for a disabled entry, the ambiguous
                            // attachments are not combined for this Step.
                            step_ambiguous.extend(candidates.iter().copied());
                        }
                        step_unapplied.extend(candidates.iter().copied());
                        step_undetermined.extend(candidates.iter().copied());
                    }
                    continue;
                }
            }
            match set.match_entry(
                name,
                dep.is_required(),
                path_field(&entry_path, "environment"),
                &mut errors,
                &mut warnings,
            ) {
                EntryMatch::Absent => {}
                EntryMatch::One(doc) => {
                    let params = dep.parameters.as_ref().map_or(&empty, |p| &p.0);
                    // §3.2.1 item 1: the entry still refers to the
                    // attachment, with no values.
                    let params = if set.ignores_parameters(
                        doc,
                        dep.is_required(),
                        params,
                        &entry_path,
                        &mut warnings,
                    ) {
                        &empty
                    } else {
                        params
                    };
                    step_resolved.push((
                        doc,
                        Supplied {
                            params,
                            symtab: &no_symbols,
                            entry_path,
                        },
                    ));
                }
                EntryMatch::Many(many) => {
                    step_unapplied.extend(many.iter().copied());
                    step_ambiguous.extend(many.iter().copied());
                }
            }
        }
        resolved.push(step_resolved);
        unapplied.push(step_unapplied);
        ambiguous.push(step_ambiguous);
        disabled.push(step_disabled);
        declined.push(step_declined);
        undetermined.push(step_undetermined);
    }

    // The Job Template's own Environments that have an `exclusiveGroup`.
    // Template validation, which does not know the attachments, never
    // counts one as entered (§4, after item 10), so their value checks and
    // constraint 7 are made here for each Step that is known to enter
    // them, with the attachments known.
    let own_envs = job_template.job_environments.as_deref().unwrap_or(&[]);
    let job_ctx = job_template
        .default_validation_context()
        .with_caller_limits(ctx.caller_limits.clone());
    let grouped_job_envs: Vec<(usize, DeclaringEnvironment<'_>)> = own_envs
        .iter()
        .enumerate()
        .filter(|(_, env)| env.group().is_some())
        .map(|(k, env)| {
            (
                k,
                DeclaringEnvironment::new(
                    job::Document::JobTemplate,
                    env,
                    &path_index(&path_field(&job_path, "jobEnvironments"), k),
                    false,
                ),
            )
        })
        .collect();
    // Per attachment: whether some Step is known to enter it, for the
    // unreachable attachments' error.
    let mut known_entered_by_some_step = vec![false; set.declaring.len()];
    for (i, step) in job_template.steps.iter().enumerate() {
        let step_path = path_index(&path_field(&job_path, "steps"), i);
        let mut missing = Vec::new();
        let mut unresolved = HashSet::new();
        let step_env_names: HashSet<&str> = step
            .step_environments
            .iter()
            .flatten()
            .map(|e| e.name.as_str())
            .collect();
        // §4 item 4, §3.2 item 3.2: the Step's Sessions enter every
        // `ALWAYS` attachment, every `BY_DEFAULT` one a disabled entry does
        // not decline, and each `ON_REQUEST` one an enabled entry refers
        // to. The value checks and constraint 7 do not apply to a Step for
        // which the Environment is disabled (§3.2.2), so they are made only
        // for one known not to be: its own `enableIf` is absent or `true`,
        // or is certainly unresolved with the Step's values (one whose
        // `enableIf` is `false` is disabled, and any other expression is
        // undetermined, §7.6 rule 5), and no earlier attachment of its
        // `exclusiveGroup` may supersede it (§4 item 7, and the paragraph
        // after item 10). The Job Template's own Environments come after
        // the attachments in the Job's Environment order, so they supersede
        // none.
        let mut states = Vec::with_capacity(set.declaring.len());
        let mut checks = Vec::with_capacity(set.declaring.len());
        for (doc, (de, att_ctx)) in set.declaring.iter().zip(&set.contexts).enumerate() {
            let supplied = resolved[i].iter().find(|(d, _)| *d == doc).map(|(_, s)| s);
            let enters = match de.activation {
                EnvironmentActivation::Always => true,
                EnvironmentActivation::ByDefault => !declined[i].contains(&doc),
                EnvironmentActivation::OnRequest => supplied.is_some(),
            };
            let certainly_unresolved = !de.certainly_enabled
                && tv::enable_if_certainly_unresolved(de.env, supplied.map(|s| s.params), att_ctx);
            // An attachment an entry with an `enableIf` expression may name
            // is in `undetermined`: whether that entry requests or declines
            // it is decided at job creation. No entry decides an unreachable
            // one (named like a Job Environment, or ambiguous, which is
            // reported): it is entered by its `activation` and `enableIf`
            // alone, and its values are not checked (job creation stops a
            // Step whose entry names an ambiguous one).
            let state = if de.activation != EnvironmentActivation::Always
                && de.env.literal_enabled() == Some(false)
            {
                Known::NotEntered
            } else if undetermined[i].contains(&doc) && !set.unreachable.contains(&doc) {
                Known::Undetermined
            } else if !enters {
                Known::NotEntered
            } else if de.certainly_enabled || certainly_unresolved {
                Known::Entered
            } else {
                Known::Undetermined
            };
            states.push((de.env.group(), state));
            checks.push((
                enters && !unapplied[i].contains(&doc),
                supplied,
                certainly_unresolved,
            ));
        }
        // The Job Template's own Environments come after the attachments in
        // the Job's Environment order. Only the grouped ones are checked
        // here; the others were checked at template validation.
        let entries: Vec<(KnownEntry, Option<&StepDependency>)> = own_envs
            .iter()
            .map(|env| {
                let dep = step
                    .dependencies
                    .iter()
                    .flatten()
                    .find(|d| d.environment() == Some(env.name.as_str()));
                (KnownEntry::of(dep), dep)
            })
            .collect();
        for (env, (entry, dep)) in own_envs.iter().zip(&entries) {
            let supplied = dep
                .filter(|_| *entry == KnownEntry::Enabled)
                .and_then(|d| d.parameters.as_ref())
                .map(|p| &p.0);
            let state = match env.known_state(*entry) {
                Known::Undetermined
                    if *entry != KnownEntry::Undetermined
                        && tv::enable_if_certainly_unresolved(env, supplied, &job_ctx) =>
                {
                    Known::Entered
                }
                state => state,
            };
            states.push((env.group(), state));
        }
        let states = apply_exclusive_groups(states);
        for (doc, &state) in states.iter().take(set.declaring.len()).enumerate() {
            known_entered_by_some_step[doc] |= state == Known::Entered;
        }
        for (doc, ((de, att_ctx), (checked, supplied, certainly_unresolved))) in set
            .declaring
            .iter()
            .zip(&set.contexts)
            .zip(checks)
            .enumerate()
        {
            if !checked {
                continue;
            }
            let values_checked = states[doc] == Known::Entered;
            let m = de.check_step_statically(
                supplied,
                values_checked,
                ResolvePolicy::new(att_ctx),
                &mut errors,
            );
            if values_checked && certainly_unresolved && !m.is_empty() {
                unresolved.insert(de.label.as_str());
            }
            set.sort_missing(
                doc,
                m,
                &step_env_names,
                &step_path,
                &mut missing,
                &mut errors,
            );
        }
        let disabled = set.disabled_labels(disabled[i].iter().copied());
        report_missing(
            &step_path,
            &missing,
            EVERY_ATTACHED,
            &disabled,
            &unresolved,
            &HashSet::new(),
            &mut errors,
        );
        // The grouped Job Environments the Step is known to enter, as job
        // creation checks them.
        let deps_path = path_field(&step_path, "dependencies");
        let mut job_missing = Vec::new();
        let mut job_unresolved = HashSet::new();
        for (k, de) in &grouped_job_envs {
            if states[set.declaring.len() + k] != Known::Entered {
                continue;
            }
            let (entry, dep) = entries[*k];
            let supplied = dep.filter(|_| entry == KnownEntry::Enabled).map(|d| {
                let j = step
                    .dependencies
                    .iter()
                    .flatten()
                    .position(|x| std::ptr::eq(x, d))
                    .unwrap_or_default();
                Supplied {
                    params: d.parameters.as_ref().map_or(&empty, |p| &p.0),
                    symtab: &no_symbols,
                    entry_path: path_index(&deps_path, j),
                }
            });
            let m = de.check_step_statically(
                supplied.as_ref(),
                true,
                ResolvePolicy::new(&job_ctx),
                &mut errors,
            );
            if !m.is_empty() {
                if !de.certainly_enabled {
                    job_unresolved.insert(de.label.as_str());
                }
                job_missing.push((de.label.clone(), m));
            }
        }
        report_missing(
            &step_path,
            &job_missing,
            EVERY_JOB_ENVIRONMENT,
            &HashSet::new(),
            &job_unresolved,
            &HashSet::new(),
            &mut errors,
        );
    }
    // An unreachable attachment's parameters are needed only where it is
    // not disabled, which is known here only for an `enableIf` that is
    // absent or `true`, or certainly unresolved with no values supplied
    // (§3.2.2, §7.6 rule 5), and where no earlier Environment of its
    // `exclusiveGroup` may supersede it (§4 item 7): that is, where some
    // Step is known to enter it. Reported after the Steps' values, as job
    // creation reports it.
    set.report_unreachable(
        |_, path, message| errors.add(path, message),
        |doc| known_entered_by_some_step[doc],
        // No `enableIf` is evaluated here, so none has failed.
        |_| false,
    );
    // What each Step's entry for each attachment is known to be.
    let known: Vec<Vec<KnownEntry>> = (0..job_template.steps.len())
        .map(|i| {
            (0..attached.len())
                .map(|d| {
                    if resolved[i].iter().any(|(r, _)| *r == d) {
                        KnownEntry::Enabled
                    } else if declined[i].contains(&d) {
                        KnownEntry::Disabled
                    } else if undetermined[i].contains(&d) {
                        KnownEntry::Undetermined
                    } else {
                        KnownEntry::Absent
                    }
                })
                .collect()
        })
        .collect();
    let certainly_entered = certainly_entered_per_step(job_template, attached, &known);
    check_attached_wrap_layers(
        job_template,
        attached,
        &set.doc_names,
        &certainly_entered,
        &job_path,
        &mut errors,
    );

    // §3.3.3: the literal host requirements of each Step and the
    // Environments its Sessions certainly enter, combined.
    let documents: Vec<job::Document> = (0..attached.len())
        .map(|d| job::Document::environment_template(d, attached[d].label))
        .chain(std::iter::repeat_n(
            job::Document::JobTemplate,
            own_envs.len(),
        ))
        .collect();
    let docs = Documents { attached };
    let sources: Vec<super::host_requirements::EnvironmentSource<'_>> = documents
        .iter()
        .enumerate()
        .map(|(e, document)| {
            if e < attached.len() {
                super::host_requirements::EnvironmentSource {
                    environment: attached[e].template.environment(),
                    document,
                    path: path_field(&docs.path(Some(e)), "environment"),
                    ctx: &set.contexts[e],
                }
            } else {
                let j = e - attached.len();
                super::host_requirements::EnvironmentSource {
                    environment: &own_envs[j],
                    document,
                    path: path_index(&path_field(&job_path, "jobEnvironments"), j),
                    ctx,
                }
            }
        })
        .collect();
    // An attachment that declares parameters and that an ambiguous entry
    // of the Step names is left out: no value of it is known, and job
    // creation stops such a Step before host requirements, so combining it
    // here would report a conflict job creation does not. (An `ALWAYS` attachment that
    // an entry with an `enableIf` expression names is entered either way,
    // so it is kept; a `BY_DEFAULT` one may be declined, so it is not
    // certainly entered.)
    let declares = |d: usize| {
        attached[d]
            .template
            .environment()
            .env_parameter_definitions
            .as_ref()
            .is_some_and(|p| !p.is_empty())
    };
    // An Environment whose own `enableIf` is not absent or `true` is not
    // certainly entered (§4, after item 10), unless it is `ALWAYS` (§3.2.2
    // item 6), nor is one an earlier one of its group may supersede.
    let entered: Vec<Vec<bool>> = certainly_entered
        .into_iter()
        .zip(&ambiguous)
        .map(|(mut entered, ambiguous)| {
            for (d, e) in entered.iter_mut().enumerate().take(attached.len()) {
                *e &= !(ambiguous.contains(&d) && declares(d));
            }
            entered
        })
        .collect();
    let step_ctx = job_template
        .default_validation_context()
        .with_caller_limits(ctx.caller_limits.clone());
    super::host_requirements::check_literal_combinations(
        job_template,
        &sources,
        &entered,
        &job_path,
        &step_ctx,
        super::EvalBudgets::from_ctx(ctx),
        &mut errors,
    );
    errors.into_result(if attached.is_empty() {
        "JobTemplate"
    } else {
        SUBMISSION
    })?;
    Ok(warnings)
}
