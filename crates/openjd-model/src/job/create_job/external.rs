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
//! effective values for each attached Environment that applies to it,
//! checks constraints 5 and 7 for every one of those, whatever its own
//! `enableIf` or `exclusiveGroup` (§3.2 constraint 7, §7.4.1 step 5),
//! decides each attachment's own `enableIf` (§4 item 6) per Step with the
//! Step's concrete values (§3.2.2 item 2), checks constraint 6 for those
//! it does not disable (§3.2.2), checks the
//! `WRAP_ACTIONS` single-layer rule over each Step's final set, and builds
//! the combined Job. It decides the deferred Job Environments the same way,
//! after the attachments.
//!
//! [`check_environment_templates`] runs the checks that need no Job
//! Parameter values, before any are known.
//!
//! With the `SERVICE` extension (RFC 0009, Template Schemas §1.2.2
//! "Services from Environment Templates") an Environment Template may
//! define `services` as well as, or instead of, an `environment`. Those
//! become **external Services** of every Job submitted through it. Every
//! Service of the combined Job selects Environments exactly as a Step does
//! (RFC 0009 "Services run inside Environments"): a Job Template's Service
//! among all of the combined Job's `jobEnvironments`, through the same
//! per-Step decisions as the Steps, and an Environment Template's Service
//! among those of its own document and of the Environment Templates
//! attached before it, by their `activation`, declining one with a disabled
//! `environment:` entry and supplying no values (§1.2 *services* constraint
//! 5). There is no exception for an Environment that lists the Service.
//! Each Service's effective values, Environment statuses and combined host
//! requirements are computed as a Step's are, its errors filed after every
//! Step's in each §7.4.1 step. Its scope is then computed by the §9.1 rules
//! over the combined Job, with the dependency graph's cycle check
//! (`composition`). The checks that relate documents only the scheduler
//! sees together are performed here, at submission, rather than at
//! template validation (§9.9):
//!
//! - the **requirement matching** rule (§1.2.2 item 2): each entry of the
//!   Job Template's `externalServices` matches exactly one attached Service
//!   with its `name`, declaring every listed port with the same `protocol`;
//! - the **wrapping-Environment** rule (§1.2.2 item 4, wrap hook rule 1): a
//!   wrapping Environment that the Sessions of a Service with a non-empty
//!   scope enter defines the four `onWrapService*` hooks, and one from a
//!   document that declares `SERVICE` that a Task Session enters defines
//!   RFC 0008's three;
//! - the cycles of `<StepDependency>` constraint 3 through an attached
//!   Environment or Service, or through an `enableIf` not known before the
//!   Job is created (§7.6 rule 5) or an `exclusiveGroup`.
//!
//! Service names are per document (§1.2.2 item 3): an external Service
//! may share its `name` with a Service of the Job Template or of another
//! attachment, and nothing here rejects that unless a requirement names it.
//! A `Service.*` reference resolves within its own document — or, for a
//! required Service, to the attached Service the requirement was bound to —
//! so each instantiated Service is stamped with its [`job::Document`] for a
//! scheduler to keep same-named Services distinct.
//!
//! A services-only attachment (no `environment`) takes no part in the
//! `ENV_PARAMS` resolution: no entry can name it, and it contributes no
//! Environment to `Job.EnvNames` or the combined `jobEnvironments`.

use std::collections::{BTreeMap, HashMap, HashSet};

use openjd_expr::ExprValue;
use serde::{Deserialize, Serialize};

use crate::error::{
    path_field, path_index, path_to_string, ModelError, PathElement, ValidationErrors,
};
use crate::job;
use crate::template::{
    EnvironmentParameterValue, EnvironmentTemplate, JobTemplate, StepDependency,
};
use crate::types::{JobParameterValues, ModelExtension, ValidationContext};

use super::build_symbol_table;
use super::composition;
use super::env_params::{
    report_missing, DeclaringEnvironment, EnableIf, FrozenEntry, ResolvePolicy, StepSelection,
    Supplied, Who,
};
use super::instantiate::{self, InstantiateCtx};
use super::staged::{Stage, StagedErrors, ANY};
use super::EvalBudgets;
use crate::template::validate_v2023_09::env_params::{
    self as tv, always_entry_disabled_message, capped_name_list,
};
use crate::template::validate_v2023_09::format_strings::{self as tvfs, KnownEnableIfs};
use crate::template::validate_v2023_09::EffectiveLimits;
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
    /// `environment_parameters` and `environments` cover both. Its
    /// `services` (RFC 0009) are the external Services, in attachment order
    /// and within one template in its `services` order, each with its
    /// [`scope`](job::Service::scope) computed over the combined Job by the
    /// §9.1 rules and its [`document`](job::Service::document) set,
    /// followed by the Job Template's own; an external Service with an
    /// empty scope is never started. Every Service carries the Job
    /// Environments its Sessions enter and its effective values for them
    /// ([`job::Service::environments`],
    /// [`job::Service::environment_parameters`]), and its combined host
    /// requirements. A list that would be empty is `None`.
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
    /// For each Service of `job` (RFC 0009), index for index with
    /// [`job.services`](job::Job::services), every Environment the Service
    /// considered, with whether its Sessions enter it or which decision
    /// left it out, as [`environment_statuses`](Self::environment_statuses)
    /// gives them for a Step: a Service selects Environments exactly as a
    /// Step does (RFC 0009 "Services run inside Environments"). A Job
    /// Template's Service considers the Environments of the whole combined
    /// Job, an Environment Template's only those of its own document and of
    /// the Environment Templates attached before it, and neither has Step
    /// Environments. An external Service can request nothing, so it never
    /// considers an `ON_REQUEST` Environment and has no `not_found` entries.
    /// An external Service whose scope is empty is never started, but its
    /// statuses are still given. Empty for a Job without Services.
    pub service_environment_statuses: Vec<StepEnvironmentStatuses>,
    /// For each Service of `job` (RFC 0009), index for index with
    /// [`job.services`](job::Job::services), the host requirements that
    /// resolved to nothing for that Service and were discarded, in the order
    /// of [`discarded_host_requirements`](Self::discarded_host_requirements):
    /// the Service's own first, then each entered Environment's. For
    /// display; empty for a Service with none, or for a Job without
    /// Services.
    pub discarded_service_host_requirements: Vec<Vec<DiscardedHostRequirement>>,
    /// The attached Service each of the Job Template's `externalServices`
    /// entries was matched to (RFC 0009, Template Schemas §1.2.2 item 2), in
    /// requirement order. A runtime seeds `Service.<requirement>.<port>.port`
    /// / `.connectAddress` in the Job Template's Task Sessions and Job
    /// Environments from the bound Service's endpoints.
    pub requirement_bindings: Vec<RequirementBinding>,
}

/// A host requirement that resolved to nothing for a Step or a Service
/// (RFC 0009) and was discarded (`ENV_PARAMS`, Template Schemas §3.3): an
/// amount whose `min`
/// and `max` both resolved to `null`, or an attribute whose `anyOf` and
/// `allOf`, each written as one expression, resolved to `null`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscardedHostRequirement {
    /// The Environment whose `hostRequirements` held it, by `(document,
    /// environment)`; `None` for the Step's (or Service's) own.
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

/// One `externalServices` entry of the Job Template matched to the attached
/// Service that provides it (Template Schemas §1.2.2 item 2, §9.8).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequirementBinding {
    /// The requirement's `name` — also the bound Service's `name`, since
    /// matching is by name.
    pub requirement: String,
    /// The attached Environment Template that declares the bound Service.
    pub document: job::Document,
    /// The bound Service's `name` (equal to `requirement`).
    pub service: String,
}

/// The documents of a submission, for naming them in paths and messages.
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

    /// How to refer to document `doc` in prose.
    fn name(&self, doc: Option<usize>) -> String {
        match doc {
            None => "the Job Template".to_string(),
            Some(_) => path_to_string(&self.path(doc)),
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
    /// Per attachment, the Environment it defines, set up for entries; `None`
    /// for a services-only attachment (RFC 0009), which no entry can name.
    declaring: Vec<Option<DeclaringEnvironment<'a>>>,
    by_name: HashMap<&'a str, Vec<usize>>,
    /// Attachments no enabled entry can reach: named like a Job
    /// Environment (§3.2.1 item 4), which no entry reaches, or like another
    /// attachment (item 3), which only a disabled entry reaches, to decline
    /// it. The missing parameters of one with `activation: ALWAYS` or
    /// `BY_DEFAULT` are reported once, at the attachment, not per Step or
    /// Service; one with `ON_REQUEST` is never entered.
    unreachable: HashSet<usize>,
    /// The attachment-level errors for an unreachable attachment with
    /// `activation: ALWAYS` or `BY_DEFAULT` and parameters without a
    /// `default`, by attachment, path, and message. No enabled entry can
    /// supply its parameters, so it applies to every Step and Job Template
    /// Service that does not decline it, whatever its own `enableIf` or
    /// `exclusiveGroup` (§3.2 constraint 7), and
    /// [`report_unreachable`](Self::report_unreachable) reports each one
    /// that applies to some Step or Job Template Service that is not
    /// stopped.
    unreachable_required: Vec<(usize, Vec<PathElement>, String)>,
}

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
    /// parameters that is not also a Job Environment's name, and every
    /// literal `default` that fails its checks (§4.5 item 2).
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
        // Who the messages below name: the Steps, and the Services too
        // when the Job Template lists `SERVICE` (one without it has none).
        let entities = if ctx.profile.has_extension(ModelExtension::Service) {
            "Step or Service"
        } else {
            "Step"
        };
        let every = if entities == "Step" {
            "every Step"
        } else {
            "every Step and Service"
        };
        // How the messages below say the attachment applies to the Steps
        // (§3.2 constraint 7): whatever its `enableIf` or `exclusiveGroup`,
        // since no entry can decline it.
        let entered = |env: &crate::template::Environment| match env.activation() {
            EnvironmentActivation::ByDefault
                if env.enable_if.is_some() || env.group().is_some() =>
            {
                format!(
                    "Its activation is BY_DEFAULT, so it applies to {every} whatever its \
                     enableIf and exclusiveGroup (§3.2 constraint 7)"
                )
            }
            EnvironmentActivation::ByDefault => {
                "Its activation is BY_DEFAULT, so every Session enters it".to_string()
            }
            // An `ON_REQUEST` attachment has no such error.
            EnvironmentActivation::Always | EnvironmentActivation::OnRequest => {
                "Its activation is ALWAYS, so every Session enters it".to_string()
            }
        };
        for (doc, att) in attached.iter().enumerate() {
            // §4 item 4: an Environment from a document that does not list
            // `ENV_PARAMS` behaves as `ALWAYS`.
            let pre_extension = !att
                .template
                .profile()
                .has_extension(ModelExtension::EnvParams);
            let att_ctx = att
                .template
                .default_validation_context()
                .with_caller_limits(ctx.caller_limits.clone());
            let Some(env) = att.template.environment() else {
                declaring.push(None);
                contexts.push(att_ctx);
                continue;
            };
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
                if !required.is_empty() && env.activation() != EnvironmentActivation::OnRequest {
                    unreachable_required.push((
                        doc,
                        path_field(&env_path, "envParameterDefinitions"),
                        format!(
                            "Environment '{}' has the same name as a jobEnvironments entry of \
                             the Job Template, so no 'environment:' dependency can reach it \
                             (Template Schemas §3.2.1 item 4), and it has parameters without a \
                             default that no {entities} can supply: {}. {}, and the \
                             submission is rejected (§3.2.1).",
                            env.name,
                            capped_name_list(required.iter().copied(), required.len()),
                            entered(env)
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
            declaring.push(Some(DeclaringEnvironment::new(
                job::Document::environment_template(doc, att.label),
                env,
                &env_path,
                pre_extension,
            )));
            contexts.push(att_ctx);
        }
        // §3.2.1 item 3: an enabled entry naming an Environment that two or
        // more attachments define is rejected, so no entry can supply their
        // parameters (a disabled one can only decline a `BY_DEFAULT` one).
        // One without a `default` rejects the submission where it applies
        // to a Step; say so once per attachment, rather than per Step with
        // "must supply" advice that an entry could only follow by naming
        // the ambiguous name. An entry naming a Job Environment's name is
        // not resolved against the attachments, so item 3 does not apply
        // to attachments with that name: item 4's error, above, is their
        // only one.
        for (doc, att) in attached.iter().enumerate() {
            let Some(env) = att.template.environment() else {
                continue;
            };
            let Some(others) = by_name.get(env.name.as_str()).filter(|d| d.len() > 1) else {
                continue;
            };
            unreachable.insert(doc);
            if job_env_names.contains(env.name.as_str()) {
                continue;
            }
            let required = required_names(env);
            if required.is_empty() || env.activation() == EnvironmentActivation::OnRequest {
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
                     no enabled 'environment:' dependency can reach it (Template Schemas \
                     §3.2.1 item 3), and it has parameters without a default that no {entities} \
                     can supply: {}. {}, and the submission is rejected (§3.2.1).",
                    env.name,
                    others.len(),
                    capped_name_list(others.iter().map(|d| doc_names[*d].as_str()), others.len()),
                    capped_name_list(required.iter().copied(), required.len()),
                    // A disabled entry can decline a `BY_DEFAULT` one, and
                    // this error is reported only where some Step or
                    // Service does not.
                    match env.activation() {
                        EnvironmentActivation::ByDefault
                            if env.enable_if.is_some() || env.group().is_some() =>
                        {
                            format!(
                                "Its activation is BY_DEFAULT, so it applies to {every} that does \
                                 not decline it with a disabled entry, whatever its enableIf and \
                                 exclusiveGroup (§3.2 constraint 7), and at least one {entities} \
                                 does not decline it"
                            )
                        }
                        EnvironmentActivation::ByDefault => format!(
                            "Its activation is BY_DEFAULT, so it applies to {every} that does not \
                             decline it with a disabled entry, and at least one {entities} does \
                             not decline it"
                        ),
                        _ => entered(env),
                    }
                ),
            ));
        }
        for (de, att_ctx) in declaring.iter().zip(&contexts) {
            let Some(de) = de else {
                continue;
            };
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

    /// The Environment attachment `doc` defines. An entry resolves only to
    /// an attachment in [`by_name`](Self::by_name), which defines one.
    fn de(&self, doc: usize) -> &DeclaringEnvironment<'a> {
        self.declaring[doc]
            .as_ref()
            .expect("an entry resolves only to an attachment that defines an Environment")
    }

    /// Report the attachment-level error of each unreachable attachment
    /// with `activation: ALWAYS` or `BY_DEFAULT` and parameters without a
    /// `default` (see
    /// [`unreachable_required`](Self::unreachable_required)) for which
    /// `applies(doc)` holds: it applies to some Step that is not stopped
    /// (§3.2 constraint 7). Each goes to `add` with the attachment's index,
    /// its path and its message.
    fn report_unreachable(
        &self,
        mut add: impl FnMut(usize, &[PathElement], String),
        applies: impl Fn(usize) -> bool,
    ) {
        for (doc, path, message) in &self.unreachable_required {
            if applies(*doc) {
                add(*doc, path, message.clone());
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
    /// keys and the kind of entity (`kind`). `entry_path` is the entry's
    /// full path.
    fn ignores_parameters(
        &self,
        kind: tv::RequesterKind,
        doc: usize,
        required: bool,
        params: &BTreeMap<String, EnvironmentParameterValue>,
        entry_path: &[PathElement],
        warnings: &mut Vec<SubmissionWarning>,
    ) -> bool {
        let de = self.de(doc);
        if required || params.is_empty() || de.declares_parameters() {
            return false;
        }
        warnings.push(SubmissionWarning {
            path: path_field(entry_path, "parameters"),
            message: ignored_parameters_message(kind, &de.label, params),
        });
        true
    }

    /// Resolve one entry naming `environment`, which is not a Job
    /// Environment, by name (§3.2.1 items 1–3), reporting a missing
    /// required Environment or an ambiguous name into `errors` and a
    /// missing optional one into `warnings`. `env_path` is the entry's
    /// `environment` key; `kind` is the kind of entity the entry belongs to,
    /// for the advice.
    fn match_entry(
        &self,
        kind: tv::RequesterKind,
        environment: &str,
        required: bool,
        env_path: Vec<PathElement>,
        errors: &mut ValidationErrors,
        warnings: &mut Vec<SubmissionWarning>,
    ) -> EntryMatch<'_> {
        match self.candidates(environment) {
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
                             required; set 'required: false' if the {} can run without the \
                             Environment (Template Schemas §3.2.1 item 2).",
                            kind.noun()
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

    /// The attachments that define an Environment named `environment`, in
    /// the order they are attached: none, one, or, for an ambiguous name,
    /// several (§3.2.1 items 1–3).
    fn candidates(&self, environment: &str) -> &[usize] {
        self.by_name.get(environment).map_or(&[][..], Vec::as_slice)
    }

    /// The labels of the attachments named in `names`, the names of a
    /// Step's entries that their `enableIf` disabled, for the constraint 7
    /// message.
    fn disabled_labels<'n>(&self, names: impl Iterator<Item = &'n str>) -> HashSet<&str> {
        names
            .filter_map(|n| self.by_name.get(n))
            .flatten()
            .filter_map(|d| self.declaring[*d].as_ref())
            .map(|de| de.label.as_str())
            .collect()
    }

    /// Sort the parameters without a `default` of attachment `doc` that a
    /// Step does not supply (`names`): nothing to do when there are none
    /// or the attachment is unreachable (reported once, at the
    /// attachment); an error at `step_path` when the Step has a
    /// `stepEnvironments` entry of the same name (§3.2.1 item 5), since no
    /// entry of the Step can reach it, so it applies to the Step only
    /// because its `activation` is `ALWAYS` or `BY_DEFAULT`, whatever its
    /// `enableIf` or group (§3.2 constraint 7); otherwise pushed to
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
        let Some(de) = self.declaring[doc].as_ref() else {
            return;
        };
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
                     §3.2.1 item 5), and its activation is {}, so it applies to this Step (§3.2 \
                     constraint 7).",
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
/// ignored keys, capped as the other name lists are, and the entry's
/// entity as `kind` names it.
fn ignored_parameters_message(
    kind: tv::RequesterKind,
    label: &str,
    params: &BTreeMap<String, EnvironmentParameterValue>,
) -> String {
    format!(
        "Environment {label} declares no parameters, and this entry has 'required: false', so \
         its parameters are ignored: {}. The {}'s Sessions get no values from it (Template \
         Schemas §3.2.1 item 1).",
        capped_name_list(params.keys().map(String::as_str), params.len()),
        kind.noun()
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

/// [`EVERY_ATTACHED`] for a Job Template's Service (RFC 0009).
const SERVICE_EVERY_ATTACHED: &str =
    "every attached Environment with activation ALWAYS, each one with \
     activation BY_DEFAULT that the Service does not decline, and each one \
     with activation ON_REQUEST that the Service requests; Template Schemas §3.2.1, RFC 0009 \
     <StepDependency> item 3";

/// The constraint 7 reason for attached Environments for an entity of
/// `kind`.
fn every_attached(kind: tv::RequesterKind) -> &'static str {
    match kind {
        tv::RequesterKind::Step => EVERY_ATTACHED,
        tv::RequesterKind::Service => SERVICE_EVERY_ATTACHED,
    }
}

/// The constraint 7 reason for an Environment Template's Service (RFC
/// 0009), which completes
/// [`missing_parameters_message`](crate::template::validate_v2023_09::env_params::missing_parameters_message):
/// it can supply no value.
const EVERY_ENVIRONMENT_OF_EXTERNAL_SERVICE: &str =
    "every Environment of its own document and of the Environment Templates attached before it \
     with activation ALWAYS, and each one with activation BY_DEFAULT that it does not decline; \
     a Service in an Environment Template has only disabled 'environment:' dependencies, so it \
     enters each with every parameter at its default; Template Schemas §3.2 constraint 7, RFC \
     0009 \"Services run inside Environments\"";

/// The constraint 7 reason for the Job Environments, for an entity of
/// `kind`.
fn every_job_environment(kind: tv::RequesterKind) -> &'static str {
    kind.every_job_environment()
}

/// One requesting entity of the Job Template for
/// [`apply_environment_templates`]: a Step, or (RFC 0009) one of the Job
/// Template's Services, which selects Environments exactly as a Step does.
struct Requesting<'t> {
    /// `Step 'X'` or `Service 'X'`.
    label: String,
    /// The entity's `name`.
    name: &'t str,
    kind: tv::RequesterKind,
    /// Its full path (`steps[i]` or `services[k]`, after the document's).
    path: Vec<PathElement>,
    /// The index its errors are filed under ([`Who::index`]).
    index: usize,
    /// Its `stepEnvironments` names (none for a Service).
    step_env_names: HashSet<&'t str>,
}

impl Requesting<'_> {
    fn who(&self) -> Who<'_> {
        Who {
            label: &self.label,
            index: self.index,
        }
    }
}

/// Attach the Environment Templates of a submission to the Job that job
/// creation built from the Job Template alone (`body`), decide every
/// Step's and every Service's Environments, and return the combined Job
/// (Template Schemas §3.2.1, §3.2.2, §4 items 4 and 5, §3.3.3, §7.4.1 steps
/// 3–8).
///
/// `body` holds the Job, what job creation decided Step by Step and, with
/// `SERVICE`, Service by Service (the enabled entries that name no Job
/// Environment, frozen; the disabled ones; the Job Environments the
/// entity's Sessions would enter by their `activation`, with its values for
/// them), and the errors found so far. `env_names` is the Job's
/// `Job.EnvNames`, bound for attached Environments whose document lists
/// `EXPR`. `attached` is in the scheduler's order. `job_parameter_values`
/// are the merged values. `ctx` supplies the caller limits, applied to
/// every document, and the Job Template's profile, for its own
/// Environments. Each attached template is evaluated under its own profile
/// ([`EnvironmentTemplate::profile`]): an extension applies to the document
/// that lists it.
///
/// Each attachment named like a `jobEnvironments` entry is unreachable from
/// the Job Template (§3.2.1 item 4). It gets a warning when the Job
/// Template lists `ENV_PARAMS` (without it the Job Template has no entries
/// that could name it, and the submission gets no warning it did not get
/// before the extension), and an error when its `activation` is `ALWAYS` or
/// `BY_DEFAULT`, it has parameters without a `default`, and it applies to
/// some Step or Service that is not stopped, whatever its own `enableIf` or
/// `exclusiveGroup` (§3.2 constraint 7), filed under §7.4.1 step 5.
///
/// §7.4.1 step 3: for each Step's frozen entry, in Step order and then
/// entry order, and then each Job Template Service's (RFC 0009
/// `<StepDependency>` item 3):
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
/// Each entity's disabled entries that name no Job Environment are matched
/// first, by the same rules, except that one matching no attachment has no
/// effect, whatever `required` says, one matching two or more refers to
/// each of them and is not an error (item 3), and no constraint 5 or 6
/// check is made, since it supplies nothing (§3.2.1). One that matches an
/// `ON_REQUEST` attachment has no effect; one that matches a `BY_DEFAULT`
/// one declines it for the entity; one that matches an `ALWAYS` one is an
/// error naming the entity and the Environment (§3.2 item 3.2, §7.4.1 step
/// 4); and one that matches an attachment whose Environment Template does
/// not list `ENV_PARAMS` has no effect, since that attachment behaves as
/// `ALWAYS` with no `enableIf` and no entity can leave it out (§4 item 4).
///
/// An entity with an error from §7.4.1 steps 1–4, here or in job creation,
/// is stopped: steps 5–8 are not taken for it. Every other one has applying
/// to it every attached Environment with `activation: ALWAYS` or from a
/// document that does not list `ENV_PARAMS`, every one with `BY_DEFAULT` it
/// does not decline, and each one with `activation: ON_REQUEST` that one of
/// its enabled entries refers to. For each one that applies to it and
/// declares parameters, the entity gets effective values from its entry if
/// one refers to it and from the `default`s otherwise (an entity whose
/// enabled entry names an ambiguous attachment is stopped, so the one
/// mistake is reported once; a disabled one refers to each of them, §3.2.1
/// item 3). Constraint 5 (keys) and constraint 7 (every parameter
/// without a `default` supplied) are checked for every Environment that
/// applies to it, whatever its own `enableIf` or `exclusiveGroup` (§3.2
/// constraint 7, §7.4.1 step 5). Then the attachments and the Job
/// Environments are decided in the Job's Environment order, attached ones
/// first: one whose `exclusiveGroup` an earlier one that remains in the
/// entity's set already has is superseded, which disables it for the
/// entity, and its own `enableIf` is not evaluated (§4 item 7, §7.4.1 step
/// 6). For any other, its own `enableIf` (§4 item 6) is evaluated for the
/// entity with its concrete values, and is not evaluated, nor disables the
/// Environment, when it reads a value that is missing, fails to resolve, or
/// does not convert (§3.2.2 item 2). One it disables is left out of the
/// entity's set and values; for one with `activation: ALWAYS`, a `false`
/// result is an error naming the entity instead (§4 item 4, §3.2.2 item
/// 6). One it does not disable takes its group. The value checks
/// (constraint 6) are made only for one that is not disabled (§7.4.1 step
/// 7). A literal `default` of an attached Environment is checked even if no
/// entity takes it (§4.5 item 2).
///
/// With `SERVICE` (RFC 0009), before any of this and when
/// `check_services_of_submission` is set (it is not for
/// [`create_job`](super::create_job)), the requirement matching of §1.2.2
/// item 2 (`check_services`) runs and is reported alone on failure. Each
/// attachment's external Services are instantiated (their checks filed with
/// the template-validation-style ones), and select Environments as a Step
/// does among those of their own document and of the attachments before
/// it, with no values supplied: a disabled entry resolves to the
/// document's own Environment if it has that name and otherwise among the
/// earlier attachments' Environments, where one whose name two or more
/// define refers to each of them, as for a Step, and none has no effect
/// (§1.2 *services* constraint 5). An external Service's
/// errors in those entries are reported whatever its scope; its other
/// errors only when its scope is known and not empty, since one nothing
/// uses is never started.
///
/// Every Service's errors are filed after every Step's in each §7.4.1 step
/// ([`Who::index`]): the Job Template's in `services` order, then the
/// attached ones in the combined Job's order. Then, with every Session's
/// set known, the combined dependency graph is checked for a cycle and each
/// Service's §9.1 scope is computed (`composition`); this needs every
/// set, so it is skipped while a Step or Service is stopped. A cycle among
/// one attached Environment Template's Services is reported at its
/// `services`, any other at the Job Template's root. An inline Service
/// whose scope is empty is an error (§9.1 rule 4). Then
/// the `WRAP_ACTIONS` single-layer rule is checked over each Step's final
/// Environment set, attached Environments included (RFC 0008; plan R8), and
/// RFC 0009's wrap hook rule 1 for the wrapping Environments a Task Session
/// or a used Service's Session enters (§1.2.2 item 4), and the single-layer
/// rule for each used Service; an attached Service is used only once the
/// scopes are known. Last, each Step and
/// each used Service gets its combined host requirements (§3.3.3): every
/// Environment's `hostRequirements` resolved with the entity's effective
/// values for each Environment it enters, combined with its own (see
/// `host_requirements::combine_step_host_requirements`).
///
/// Each decision is kept per entity as an [`EnvironmentStatus`] for every
/// Environment it considered. `Step::environments` and
/// `Service::environments`, the entered flags the later checks read, and
/// `CreatedJob::disabled_environments` are all derived from them, and they
/// are returned in `CreatedJob::environment_statuses` and
/// `CreatedJob::service_environment_statuses`.
///
/// Every violation, with job creation's own, is reported in one
/// `ModelValidation` error in §7.4.1 order, as
/// [`finish_errors`](super::finish_errors) describes: for `Submission`,
/// with every path starting with its document (`JobTemplate`,
/// `EnvironmentTemplate[i]`, or the attachment's label), when Environment
/// Templates are attached, and for `JobTemplate`, at the Job Template's
/// paths, when none is.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_environment_templates(
    job_template: &JobTemplate,
    body: super::Body<'_>,
    env_names: &[String],
    attached: &[AttachedEnvironmentTemplate<'_>],
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
    check_services_of_submission: bool,
) -> Result<CreatedJob, ModelError> {
    let super::Body {
        mut job,
        selections: step_selections,
        service_selections,
        job_envs,
        check_errors,
        staged: body_staged,
        limits,
    } = body;
    let job_envs = &job_envs;
    let docs = Documents { attached };
    // Paths start with their document only when Environment Templates are
    // attached (see `finish_errors`).
    let job_path = if attached.is_empty() {
        Vec::new()
    } else {
        docs.path(None)
    };
    // RFC 0009 §1.2.2 item 2, against the attached Services, first: a
    // failure is reported alone. [`create_job`](super::create_job), for the
    // Job Template alone, does not match requirements.
    let requirement_bindings = if check_services_of_submission {
        check_services(
            job.external_services.as_deref().unwrap_or(&[]),
            attached,
            &job_path,
        )?
    } else {
        Vec::new()
    };
    let job_env_names: HashSet<&str> = job
        .job_environments
        .iter()
        .flatten()
        .map(|e| e.name.as_str())
        .collect();

    // The requesting entities of the Job Template: its Steps, then its
    // Services, whose errors are filed after every Step's.
    let n_steps = job_template.steps.len();
    let n_own_services = service_selections.len();
    let requesting: Vec<Requesting<'_>> = job_template
        .steps
        .iter()
        .enumerate()
        .map(|(i, st)| Requesting {
            label: format!("Step '{}'", st.name),
            name: &st.name,
            kind: tv::RequesterKind::Step,
            path: path_index(&path_field(&job_path, "steps"), i),
            index: i,
            step_env_names: st
                .step_environments
                .iter()
                .flatten()
                .map(|e| e.name.as_str())
                .collect(),
        })
        .chain(
            job_template
                .services()
                .iter()
                .enumerate()
                .take(n_own_services)
                .map(|(k, svc)| Requesting {
                    label: format!("Service '{}'", svc.name),
                    name: &svc.name,
                    kind: tv::RequesterKind::Service,
                    path: path_index(&path_field(&job_path, "services"), k),
                    index: n_steps + k,
                    step_env_names: HashSet::new(),
                }),
        )
        .collect();
    let mut selections: Vec<StepSelection<'_>> = step_selections
        .into_iter()
        .chain(service_selections)
        .collect();
    debug_assert_eq!(selections.len(), requesting.len());

    let mut staged = StagedErrors::default();
    let mut warnings = Vec::new();
    // `default`s resolve against the merged Job Parameters and `Job.Name`,
    // without `Job.EnvNames` (§4.5 item 1, §7.3.2; plan A21).
    let mut default_symtab = build_symbol_table(job_parameter_values)?;
    default_symtab.set("Job.Name", ExprValue::String(job.name.clone()))?;
    let job_lists_env_params = job
        .extensions
        .as_ref()
        .is_some_and(|exts| exts.contains(&ModelExtension::EnvParams));
    // The Job Environments job creation left for the group rule, per
    // entity.
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
    // The index in the combined Job's Environments of each attachment's
    // Environment (`None` for a services-only attachment), and the number
    // of attached Environments, which come first.
    let mut att_env: Vec<Option<usize>> = Vec::with_capacity(attached.len());
    let mut n_att_envs = 0;
    for de in &set.declaring {
        att_env.push(de.as_ref().map(|_| {
            n_att_envs += 1;
            n_att_envs - 1
        }));
    }
    // The index in the combined Job's Environments of `jobEnvironments[k]`,
    // and the position its errors are filed at, after every attachment's.
    let own_at = |k: usize| n_att_envs + k;
    let own_pos = |k: usize| attached.len() + k;
    let combined_count = own_at(job_envs.all.len());

    // ── Resolve each entity's frozen entries ──
    // Per entity: the attachment each resolved entry refers to, with the
    // entry.
    // An entry whose `parameters` the attachment ignores (§3.2.1 item 1)
    // is kept, flagged, so that it still requests an `ON_REQUEST`
    // attachment, and supplies `no_parameters` instead.
    let no_parameters = BTreeMap::new();
    let mut resolved: Vec<Vec<ResolvedEntry<'_>>> = Vec::with_capacity(selections.len());
    // Per entity: the `BY_DEFAULT` attachments a disabled entry declines
    // (§3.2 item 3.2).
    let mut declined: Vec<HashSet<usize>> = Vec::with_capacity(selections.len());
    // Per entity: its enabled `required: false` entries that matched no
    // attachment (§3.2.1 item 2).
    let mut not_found: Vec<Vec<OptionalEntryNotFound>> = Vec::with_capacity(selections.len());
    for (r, selection) in requesting.iter().zip(&selections) {
        let deps_path = path_field(&r.path, "dependencies");
        let mut entity_resolved = Vec::new();
        let mut entity_declined = HashSet::new();
        let mut entity_not_found = Vec::new();
        // A disabled entry is resolved too, to learn the `activation` that
        // decides what it means (§3.2.1, §3.2 item 3.2). It supplies nothing,
        // so constraints 5 and 6 are not checked for it, and one that
        // matches no attachment has no effect, whatever `required` says
        // (item 2 does not apply). One whose name two or more attachments
        // define is not an error: it refers to each of them, with a
        // disabled entry's effect on each (item 3).
        for entry in &selection.disabled {
            let entry_path = path_index(&deps_path, entry.dependency_index);
            for &doc in set.candidates(&entry.environment) {
                let de = set.de(doc);
                match de.activation {
                    EnvironmentActivation::OnRequest => {}
                    EnvironmentActivation::ByDefault => {
                        entity_declined.insert(doc);
                    }
                    // From a document that does not list `ENV_PARAMS`: it
                    // behaves as `ALWAYS`, and the entry has no effect and is
                    // not an error (§4 item 4, §3.2 item 3.2, §7.4.1 step 4).
                    EnvironmentActivation::Always if de.pre_extension => {}
                    // No entity can leave it out (§3.2.2 item 6): a §7.4.1
                    // step 4 error, which stops the entity.
                    EnvironmentActivation::Always => staged
                        .at(Stage::Step4, r.index, entry.dependency_index)
                        .add(
                            &path_field(&entry_path, "enableIf"),
                            always_entry_disabled_message(r.kind, r.name, &de.label),
                        ),
                }
            }
        }
        for entry in &selection.frozen {
            let entry_path = path_index(&deps_path, entry.dependency_index);
            match set.match_entry(
                r.kind,
                &entry.environment,
                entry.required,
                path_field(&entry_path, "environment"),
                staged.at(Stage::Step3, r.index, entry.dependency_index),
                &mut warnings,
            ) {
                // With `required: true` it is an error (reported).
                EntryMatch::Absent => {
                    if !entry.required {
                        entity_not_found.push(OptionalEntryNotFound {
                            dependency_index: entry.dependency_index,
                            environment: entry.environment.clone(),
                        });
                    }
                }
                EntryMatch::One(doc) => {
                    let ignored = set.ignores_parameters(
                        r.kind,
                        doc,
                        entry.required,
                        &entry.parameters,
                        &entry_path,
                        &mut warnings,
                    );
                    entity_resolved.push(ResolvedEntry {
                        doc,
                        entry,
                        path: entry_path,
                        ignored,
                    });
                }
                // An ambiguous name (reported) stops the entity after
                // §7.4.1 step 4.
                EntryMatch::Many(_) => {}
            }
        }
        resolved.push(entity_resolved);
        declined.push(entity_declined);
        not_found.push(entity_not_found);
    }

    // ── Each entity's attached Environments, and its values for them ──
    // §3.2.2: the entity's effective values for each attached Environment
    // that applies to it, and constraint 7 for it, whatever its `enableIf`
    // or group (§7.4.1 step 5); then, unless it is superseded, the
    // Environment's own `enableIf` (§4 item 6) with the entity's concrete
    // values; then, for one it does not disable, the value checks (§7.4.1
    // steps 6 and 7).
    // The table each attachment's `enableIf` resolves in, before its
    // `Env.*` symbols: the Job Parameters, `Job.Name`, and `Job.EnvNames`
    // (§7.3.2). Only an attachment that lists `ENV_PARAMS`,
    // and so `EXPR`, has an `enableIf`, so one table serves every
    // attachment.
    let mut base = default_symtab.clone();
    base.set("Job.Name", ExprValue::String(job.name.clone()))?;
    base.set("Job.EnvNames", env_names_value(env_names))?;
    // Per entity, index for index with the attachments: whether the entity
    // considered each and whether its Sessions enter it (`None` for an
    // `ON_REQUEST` one it does not request, or a services-only attachment).
    let mut attached_statuses: Vec<Vec<Option<EnvironmentStatus>>> =
        Vec::with_capacity(selections.len());
    // Per attachment: whether it applies to some Step or Job Template
    // Service that is not stopped (§3.2 constraint 7).
    let mut applies_to_some_entity = vec![false; set.declaring.len()];
    // Per entity: its values for the Job Environments the group rule
    // decided here, in written order.
    let mut deferred_values: Vec<Vec<job::EnvironmentParameterValues>> =
        Vec::with_capacity(selections.len());
    // Per entity: the indices in `jobEnvironments` of the ones decided here
    // that are superseded or that their own `enableIf` disables, each with
    // which of the two.
    let mut left_out_job_environments: Vec<Vec<(usize, EnvironmentStatus)>> =
        Vec::with_capacity(selections.len());
    // Per entity, by index in the combined Job's Environments: for a
    // superseded one, the index of the member of its group the entity's
    // Sessions enter.
    let mut superseded_by: Vec<Vec<Option<usize>>> = Vec::with_capacity(selections.len());
    let mut entity_environment_parameters = Vec::with_capacity(selections.len());
    // §7.4.1: an entity with an error from steps 1–4 has no fixed set of
    // Environments, so steps 5–8 are not taken for it.
    let stopped: Vec<bool> = requesting
        .iter()
        .map(|r| body_staged.stopped(r.index) || staged.stopped(r.index))
        .collect();
    for (e, r) in requesting.iter().enumerate() {
        if stopped[e] {
            attached_statuses.push(vec![None; set.declaring.len()]);
            superseded_by.push(vec![None; combined_count]);
            entity_environment_parameters.push(Vec::new());
            left_out_job_environments.push(Vec::new());
            deferred_values.push(Vec::new());
            if let Some(entity_deferred) = deferred.get_mut(e) {
                entity_deferred.clear();
            }
            continue;
        }
        let who = r.who();
        let mut statuses = Vec::with_capacity(set.declaring.len());
        let mut values = Vec::new();
        let mut missing = Vec::new();
        // §4 item 7, §7.4.1 step 6: the groups an Environment the entity's
        // Sessions enter already has, in the Job's Environment order, so
        // the later ones of each are superseded, each with the index of
        // that Environment, the group's first that remains in the set.
        let mut taken: HashMap<&str, usize> = HashMap::new();
        let mut entity_superseded_by = vec![None; combined_count];
        // The first attachment in `missing`, to file the entity's
        // constraint 7 error under.
        let mut first_missing = None;
        for (doc, (de, att_ctx)) in set.declaring.iter().zip(&set.contexts).enumerate() {
            let (Some(de), Some(env_index)) = (de, att_env[doc]) else {
                statuses.push(None);
                continue;
            };
            let entry = resolved
                .get(e)
                .and_then(|r| r.iter().find(|r| r.doc == doc));
            // §4 item 4, §3.2 item 3.2: every `ALWAYS` attachment, every
            // `BY_DEFAULT` one a disabled entry of the entity does not
            // decline, and each `ON_REQUEST` one an enabled entry of the
            // entity refers to. A declined one's `enableIf` is not
            // evaluated for the entity, and no value check applies.
            let enters = match de.activation {
                EnvironmentActivation::Always => true,
                EnvironmentActivation::ByDefault => {
                    !declined.get(e).is_some_and(|d| d.contains(&doc))
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
            applies_to_some_entity[doc] = true;
            let policy = ResolvePolicy::new(att_ctx);
            let mut pending = de.values_for_step(
                supplied.as_ref(),
                &default_symtab,
                policy,
                staged.at(Stage::Step5, r.index, doc),
            );
            // §3.2 constraint 7, §7.4.1 step 5: the Environment applies to
            // the entity, so its parameters without a `default` are needed
            // whatever its `enableIf` or group decides.
            let before = missing.len();
            set.sort_missing(
                doc,
                std::mem::take(&mut pending.missing),
                &r.step_env_names,
                &r.path,
                &mut missing,
                staged.at(Stage::Step5, r.index, doc),
            );
            if missing.len() > before && first_missing.is_none() {
                first_missing = Some(doc);
            }
            // Superseded by an earlier one of its group: disabled for the
            // entity (§3.2.2), so its values are not checked, and its own
            // `enableIf` is not evaluated (§4 item 7, §7.4.1 step 6).
            if let Some(&winner) = de.env.group().and_then(|group| taken.get(group)) {
                statuses.push(Some(EnvironmentStatus::Superseded));
                entity_superseded_by[env_index] = Some(winner);
                continue;
            }
            let decision = de.decide(&pending, who, &base, policy);
            if let Some(group) = de.env.group().filter(|_| decision != EnableIf::Disabled) {
                taken.insert(group, env_index);
            }
            // A failed `enableIf` is checked as if it does not disable the
            // Environment, so the entity's other mistakes are reported too.
            let not_disabled = decision != EnableIf::Disabled;
            statuses.push(Some(if not_disabled {
                EnvironmentStatus::Entered
            } else {
                EnvironmentStatus::Disabled
            }));
            match decision {
                EnableIf::Disabled => {}
                EnableIf::Enabled | EnableIf::NotEvaluated | EnableIf::Failed => {
                    values.extend(de.commit(pending, who, staged.at(Stage::Step7, r.index, doc)));
                }
            }
        }
        // A disabled entry for an attachment the entity would still enter,
        // an `ALWAYS` one, stops the entity, so no message here needs the
        // disabled note.
        report_missing(
            &r.path,
            &missing,
            every_attached(r.kind),
            r.kind.noun(),
            &HashSet::new(),
            staged.at(Stage::Step5, r.index, first_missing.unwrap_or(ANY)),
        );
        // The Job Environments that apply to the entity, with the values
        // job creation computed for them. The attached ones come first in
        // the Job's Environment order. Each gets constraint 7 (§7.4.1 step
        // 5); one that is not superseded gets its own `enableIf` decided
        // now (step 6), and one that it does not disable its value checks
        // (step 7), at the Job Template's paths.
        let mut job_missing = Vec::new();
        let mut entity_deferred_values = Vec::new();
        let mut entity_left_out = Vec::new();
        let mut first_job_missing = None;
        if let Some(entity_deferred) = deferred.get_mut(e) {
            for mut d in std::mem::take(entity_deferred) {
                let de = &job_envs.environments[d.environment];
                let m = std::mem::take(&mut d.pending.missing);
                if !m.is_empty() {
                    first_job_missing.get_or_insert(d.position);
                    job_missing.push((de.label.clone(), m));
                }
                // `taken` holds the attachments' groups, then those of the
                // earlier deferred ones. A superseded one's own `enableIf`
                // is not evaluated (§4 item 7, §7.4.1 step 6).
                if let Some(&winner) = de.env.group().and_then(|group| taken.get(group)) {
                    entity_left_out.push((d.position, EnvironmentStatus::Superseded));
                    entity_superseded_by[own_at(d.position)] = Some(winner);
                    continue;
                }
                let decision = de.decide(
                    &d.pending,
                    who,
                    &job_envs.enable_if_base,
                    ResolvePolicy::new(ctx),
                );
                if decision == EnableIf::Disabled {
                    entity_left_out.push((d.position, EnvironmentStatus::Disabled));
                    continue;
                }
                if let Some(group) = de.env.group() {
                    taken.insert(group, own_at(d.position));
                }
                let mut scratch = ValidationErrors::default();
                let v = de.commit(d.pending, who, &mut scratch);
                let at = staged.at(Stage::Step7, r.index, own_pos(d.position));
                for mut e in scratch.errors {
                    e.path = job_path.iter().cloned().chain(e.path).collect();
                    at.errors.push(e);
                }
                entity_deferred_values.extend(v);
            }
        }
        left_out_job_environments.push(entity_left_out);
        superseded_by.push(entity_superseded_by);
        report_missing(
            &r.path,
            &job_missing,
            every_job_environment(r.kind),
            r.kind.noun(),
            &HashSet::new(),
            staged.at(
                Stage::Step5,
                r.index,
                first_job_missing.map_or(ANY, own_pos),
            ),
        );
        deferred_values.push(entity_deferred_values);
        attached_statuses.push(statuses);
        entity_environment_parameters.push(values);
    }
    drop(resolved);
    for (selection, left_out) in selections.iter_mut().zip(left_out_job_environments) {
        for (position, status) in left_out {
            selection.job_environment_statuses[position] = Some(status);
        }
    }
    // §7.4.1 step 6: a failed `enableIf` is reported once for all the
    // entities it was resolved for, filed under the first of them, at its
    // Environment's place in the Job's Environment order.
    for (doc, de) in set.declaring.iter().enumerate() {
        let Some(de) = de else {
            continue;
        };
        de.report_enable_if_errors(|first, path, message| {
            staged.at(Stage::Step6, first, doc).add(&path, message);
        });
    }
    for (de, &position) in job_envs.environments.iter().zip(&job_envs.positions) {
        de.report_enable_if_errors(|first, path, message| {
            let path: Vec<PathElement> = job_path.iter().cloned().chain(path).collect();
            staged
                .at(Stage::Step6, first, own_pos(position))
                .add(&path, message);
        });
    }
    // An unreachable attachment with parameters no entity can supply is an
    // error when it applies to some Step or Service of the Job Template,
    // whatever its own `enableIf` or group (§3.2 constraint 7); a stopped
    // one is left out. Not about one entity, so filed after the entities'
    // errors of step 5.
    set.report_unreachable(
        |doc, path, message| staged.at(Stage::Step5, ANY, doc).add(path, message),
        |doc| applies_to_some_entity[doc],
    );

    // ── Instantiate the external Services and convert the Environments ──
    //
    // Each attachment is evaluated under its own profile, with the merged
    // Job Parameters, `Job.Name`, and `Job.EnvNames` when it lists `EXPR`.
    // Its external Services (RFC 0009) are instantiated through the same
    // code path as a `services` entry; their scopes are computed over the
    // combined Job below. Its Environment gets the carried-forward
    // resolved-value checks `create_job` runs on a `jobEnvironments` entry,
    // against the Services of its own document it lists. Every check
    // failure is filed with the template-validation-style errors, with
    // paths prefixed by the document's; a resolution failure aborts alone,
    // naming its document.
    let mut external_services = Vec::new();
    // For each external Service: its attachment and its index there.
    let mut external_sources: Vec<(usize, usize)> = Vec::new();
    // For each external Service: its own discarded host requirements.
    let mut external_discarded: Vec<Vec<DiscardedHostRequirement>> = Vec::new();
    let mut environments = Vec::with_capacity(n_att_envs);
    let mut environment_documents = Vec::with_capacity(n_att_envs);
    for (doc, (att, att_ctx)) in attached.iter().zip(&set.contexts).enumerate() {
        let doc_path = docs.path(Some(doc));
        let document = job::Document::environment_template(doc, att.label);
        let has_expr = att_ctx.profile.has_extension(ModelExtension::Expr);
        let limits = EffectiveLimits::from_context(att_ctx);
        let mut symtab = default_symtab.clone();
        if has_expr {
            symtab.set("Job.Name", ExprValue::String(job.name.clone()))?;
            symtab.set("Job.EnvNames", env_names_value(env_names))?;
        }
        let services = att.template.services();
        let icx = InstantiateCtx {
            has_expr,
            template_expr: has_expr,
            limits: &limits,
            ctx: att_ctx,
            budgets: EvalBudgets::from_ctx(att_ctx),
            services,
            requirements: &[],
        };

        // This document's check failures, re-rooted under its path below.
        let mut doc_errors = ValidationErrors::default();
        let list_path = path_field(&[], "services");
        for (k, svc) in services.iter().enumerate() {
            // It depends only on Services of its own document, which are
            // the ones whose endpoints it may read. Its Environments are
            // decided below, so job creation's own entry resolution is not
            // run for it.
            let instantiated = instantiate::instantiate_service(
                svc,
                &symtab,
                icx,
                &path_index(&list_path, k),
                crate::template::listed_services(svc.dependencies.as_deref(), services),
                job::ServiceScope::steps::<[&str; 0], &str>([]),
                None,
                &mut doc_errors,
                &mut StagedErrors::default(),
            )
            .map_err(|e| in_document(e, &doc_path))?;
            // A declined Service has recorded its failure; the submission
            // fails on the collection below.
            if let Some((mut instantiated, _, discarded)) = instantiated {
                // §1.2.2 item 3: the Service is known by (document, name).
                instantiated.document = document.clone();
                external_services.push(instantiated);
                external_sources.push((doc, k));
                external_discarded.push(discarded);
            }
        }

        if let Some(env) = att.template.environment() {
            // The same carried-forward re-checks `create_job` runs on a
            // `jobEnvironments` entry, against the Services of this
            // document the Environment lists in its `dependencies`.
            let env_path = path_field(&[], "environment");
            let env_symtab = instantiate::build_env_check_symtab(
                env,
                &symtab,
                has_expr,
                att_ctx,
                &env_path,
                &mut doc_errors,
                crate::template::listed_services(env.dependencies.as_deref(), services),
                std::iter::empty(),
            )
            .map_err(|e| in_document(e, &doc_path))?;
            crate::template::validate_v2023_09::format_strings::check_carried_forward_environment(
                env,
                &env_symtab,
                &symtab,
                att_ctx,
                limits.max_env_var_value_len,
                &env_path,
                &mut doc_errors,
            );
            environments.push(super::convert_environment_with_symtab(env, Some(&symtab)));
            environment_documents.push(document);
        }
        // Filed after the attachments' literal `default`s.
        staged.at(Stage::Validation, ANY, 1 + doc).errors.extend(
            doc_errors.errors.into_iter().map(|mut e| {
                e.path = doc_path.iter().cloned().chain(e.path).collect();
                e
            }),
        );
    }
    debug_assert_eq!(environments.len(), n_att_envs);

    // ── The external Services' Environments ──
    //
    // An Environment Template's Service selects Environments as a Step
    // does (RFC 0009 "Services run inside Environments"), among those of
    // its own document and of the Environment Templates attached before it,
    // with no exception for one that lists it. It has only disabled
    // `environment:` entries (§1.2 *services* constraint 5), so it requests
    // nothing, supplies no value, and enters every `ALWAYS` one and every
    // `BY_DEFAULT` one it does not decline, with every parameter at its
    // `default`. The errors of its `environment:` entries (constraint 5:
    // declining an `ALWAYS` one) depend only on the
    // documents and are reported whatever its scope, as `check` reports
    // them; one stops the Service, which then also blocks the cycle check,
    // as a stopped Step does. Its other errors are buffered and reported
    // only once its scope is known and not empty, since one nothing uses
    // is never started and its Sessions enter nothing. Each Service decides the Environments'
    // own `enableIf` with its own copy of them, so that a failure is
    // reported only with the Service it belongs to.
    let mut external_statuses: Vec<Vec<Option<EnvironmentStatus>>> =
        Vec::with_capacity(external_services.len());
    let mut external_superseded_by: Vec<Vec<Option<usize>>> =
        Vec::with_capacity(external_services.len());
    let mut external_values: Vec<Vec<job::EnvironmentParameterValues>> =
        Vec::with_capacity(external_services.len());
    let mut external_errors: Vec<StagedErrors> = Vec::with_capacity(external_services.len());
    let mut external_entry_errors: Vec<StagedErrors> = Vec::with_capacity(external_services.len());
    let mut external_stopped: Vec<bool> = Vec::with_capacity(external_services.len());
    for (x, (svc, (doc, k))) in external_services.iter().zip(&external_sources).enumerate() {
        let svc_t = &attached[*doc].template.services()[*k];
        let label = format!("Service '{}' ({})", svc.name, svc.document);
        let index = n_steps + n_own_services + x;
        let who = Who {
            label: &label,
            index,
        };
        let svc_path = path_index(&path_field(&docs.path(Some(*doc)), "services"), *k);
        let deps_path = path_field(&svc_path, "dependencies");
        let mut buffer = StagedErrors::default();
        let mut entry_errors = StagedErrors::default();
        // The Environments it chooses among, by attachment: its own
        // document's and those of the earlier attachments, each set up
        // afresh for this Service.
        let local: Vec<Option<DeclaringEnvironment<'_>>> = (0..=*doc)
            .map(|d| {
                let att = &attached[d];
                att.template.environment().map(|env| {
                    DeclaringEnvironment::new(
                        job::Document::environment_template(d, att.label),
                        env,
                        &path_field(&docs.path(Some(d)), "environment"),
                        !att.template
                            .profile()
                            .has_extension(ModelExtension::EnvParams),
                    )
                })
            })
            .collect();
        // §1.2 *services* constraint 5: each disabled entry's name resolves
        // to the document's own Environment if it has that name, and
        // otherwise among the earlier attachments' Environments, whatever
        // their `activation`; one that matches none has no effect, and one
        // that matches two or more refers to each of them and is not an
        // error, as for a Step (§3.2.1 item 3). Its effect on each is RFC
        // 0010's for that Environment's `activation`.
        let mut svc_declined: HashSet<usize> = HashSet::new();
        for (j, dep) in svc_t.dependencies.iter().flatten().enumerate() {
            let Some(name) = dep.environment() else {
                continue;
            };
            let entry_path = path_index(&deps_path, j);
            let named = |d: usize| local[d].as_ref().is_some_and(|de| de.name == name);
            let targets: Vec<usize> = if named(*doc) {
                vec![*doc]
            } else {
                (0..*doc).filter(|d| named(*d)).collect()
            };
            for target in targets {
                let de = local[target]
                    .as_ref()
                    .expect("a disabled entry resolves only to a document's Environment");
                match de.activation {
                    EnvironmentActivation::OnRequest => {}
                    EnvironmentActivation::ByDefault => {
                        svc_declined.insert(target);
                    }
                    // From a document that does not list `ENV_PARAMS`: no
                    // effect (§4 item 4).
                    EnvironmentActivation::Always if de.pre_extension => {}
                    EnvironmentActivation::Always => entry_errors.at(Stage::Step4, index, j).add(
                        &path_field(&entry_path, "enableIf"),
                        always_entry_disabled_message(
                            tv::RequesterKind::Service,
                            &svc.name,
                            &de.label,
                        ),
                    ),
                }
            }
        }
        let is_stopped = entry_errors.stopped(index);
        let mut statuses: Vec<Option<EnvironmentStatus>> = vec![None; combined_count];
        let mut svc_superseded_by: Vec<Option<usize>> = vec![None; combined_count];
        let mut values = Vec::new();
        if !is_stopped {
            let mut missing = Vec::new();
            let mut first_missing = None;
            let mut taken: HashMap<&str, usize> = HashMap::new();
            for (d, de) in local.iter().enumerate() {
                let (Some(de), Some(env_index)) = (de, att_env[d]) else {
                    continue;
                };
                let enters = match de.activation {
                    EnvironmentActivation::Always => true,
                    EnvironmentActivation::ByDefault => !svc_declined.contains(&d),
                    EnvironmentActivation::OnRequest => false,
                };
                if !enters {
                    statuses[env_index] = (de.activation == EnvironmentActivation::ByDefault)
                        .then_some(EnvironmentStatus::Declined);
                    continue;
                }
                let policy = ResolvePolicy::new(&set.contexts[d]);
                let mut pending = de.values_for_step(
                    None,
                    &default_symtab,
                    policy,
                    buffer.at(Stage::Step5, index, d),
                );
                let m = std::mem::take(&mut pending.missing);
                if !m.is_empty() {
                    first_missing.get_or_insert(d);
                    missing.push((de.label.clone(), m));
                }
                if let Some(&winner) = de.env.group().and_then(|group| taken.get(group)) {
                    statuses[env_index] = Some(EnvironmentStatus::Superseded);
                    svc_superseded_by[env_index] = Some(winner);
                    continue;
                }
                let decision = de.decide(&pending, who, &base, policy);
                if let Some(group) = de.env.group().filter(|_| decision != EnableIf::Disabled) {
                    taken.insert(group, env_index);
                }
                if decision == EnableIf::Disabled {
                    statuses[env_index] = Some(EnvironmentStatus::Disabled);
                    continue;
                }
                statuses[env_index] = Some(EnvironmentStatus::Entered);
                values.extend(de.commit(pending, who, buffer.at(Stage::Step7, index, d)));
            }
            report_missing(
                &svc_path,
                &missing,
                EVERY_ENVIRONMENT_OF_EXTERNAL_SERVICE,
                "Service",
                &HashSet::new(),
                buffer.at(Stage::Step5, index, first_missing.unwrap_or(ANY)),
            );
            for (d, de) in local.iter().enumerate() {
                let Some(de) = de else {
                    continue;
                };
                de.report_enable_if_errors(|first, path, message| {
                    buffer.at(Stage::Step6, first, d).add(&path, message);
                });
            }
        }
        external_statuses.push(statuses);
        external_superseded_by.push(svc_superseded_by);
        external_values.push(values);
        external_errors.push(buffer);
        external_entry_errors.push(entry_errors);
        external_stopped.push(is_stopped);
    }

    // ── Combine ──
    let own = job.job_environments.take().unwrap_or_default();
    environment_documents.extend(std::iter::repeat_n(job::Document::JobTemplate, own.len()));
    environments.extend(own);
    let own_count = environments.len() - n_att_envs;
    debug_assert_eq!(
        own_count,
        job_template.job_environments.as_ref().map_or(0, Vec::len)
    );
    // The attachment each attached Environment comes from.
    let env_docs: Vec<usize> = att_env
        .iter()
        .enumerate()
        .filter_map(|(doc, e)| e.map(|_| doc))
        .collect();
    let sources: Vec<super::host_requirements::EnvironmentSource<'_>> = environment_documents
        .iter()
        .enumerate()
        .map(|(e, document)| {
            if e < n_att_envs {
                let d = env_docs[e];
                super::host_requirements::EnvironmentSource {
                    environment: attached[d]
                        .template
                        .environment()
                        .expect("an attached Environment's template defines one"),
                    document,
                    path: path_field(&docs.path(Some(d)), "environment"),
                    ctx: &set.contexts[d],
                }
            } else {
                let j = e - n_att_envs;
                super::host_requirements::EnvironmentSource {
                    environment: &job_template.job_environments.as_deref().unwrap_or(&[])[j],
                    document,
                    path: path_index(&path_field(&job_path, "jobEnvironments"), j),
                    ctx,
                }
            }
        })
        .collect();
    // Each entity's status for every Environment of the Job, by index into
    // `environments`: the attached ones, then the Job Template's.
    let statuses_per_entity: Vec<Vec<Option<EnvironmentStatus>>> = attached_statuses
        .into_iter()
        .zip(&selections)
        .map(|(statuses, selection)| {
            let mut all: Vec<Option<EnvironmentStatus>> = statuses
                .into_iter()
                .zip(&att_env)
                .filter(|(_, e)| e.is_some())
                .map(|(s, _)| s)
                .collect();
            all.extend(&selection.job_environment_statuses);
            all
        })
        .collect();
    // An entity stopped after §7.4.1 step 4 has no fixed set; it is counted
    // as entering none, so steps 7 and 8 leave it alone.
    let entered_of = |statuses: &[Option<EnvironmentStatus>], stopped: bool| -> Vec<bool> {
        statuses
            .iter()
            .map(|s| *s == Some(EnvironmentStatus::Entered) && !stopped)
            .collect()
    };
    let refs = |entered: &[bool]| -> Option<Vec<job::EnvironmentRef>> {
        (!entered.iter().all(|e| *e)).then(|| {
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
        })
    };
    // The scope an Environment of the Job has, from its document.
    let scope_of = |document: &job::Document| {
        if document.is_job_template() {
            EnvironmentScope::Job
        } else {
            EnvironmentScope::Attached
        }
    };
    // The statuses of every Environment an entity considered, with the
    // winner of each superseded one.
    let considered_of = |statuses: &[Option<EnvironmentStatus>],
                         superseded_by: &[Option<usize>]|
     -> Vec<StepEnvironmentStatus> {
        environments
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
            .collect()
    };

    // The Steps.
    let mut per_entity = entity_environment_parameters.into_iter();
    let mut deferred_per_entity = deferred_values.into_iter();
    let mut entered_per_step = Vec::with_capacity(n_steps);
    let mut disabled_environments = Vec::with_capacity(n_steps);
    let mut environment_statuses = Vec::with_capacity(n_steps);
    let mut not_found = not_found.into_iter();
    for (i, step) in job.steps.iter_mut().enumerate() {
        // The attachments' values, then the Job Environments', which were
        // committed in written order. Job creation proper sets no values.
        debug_assert!(step.environment_parameters.is_none());
        let mut values = per_entity.next().unwrap_or_default();
        values.extend(deferred_per_entity.next().unwrap_or_default());
        step.environment_parameters = (!values.is_empty()).then_some(values);
        let entered = entered_of(&statuses_per_entity[i], stopped[i]);
        step.environments = refs(&entered);
        entered_per_step.push(entered);
        let considered: Vec<StepEnvironmentStatus> =
            considered_of(&statuses_per_entity[i], &superseded_by[i])
                .into_iter()
                .chain(
                    selections[i]
                        .step_environments
                        .iter()
                        .map(|(name, enabled)| {
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
                        }),
                )
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
            not_found: not_found.next().unwrap_or_default(),
        });
    }

    // The Services: the external ones first, then the Job Template's own,
    // as in `job.services`.
    let own_services = job.services.take().unwrap_or_default();
    let external_count = external_services.len();
    let mut entered_per_service: Vec<Vec<bool>> =
        Vec::with_capacity(external_count + own_services.len());
    let mut service_environment_statuses: Vec<StepEnvironmentStatuses> =
        Vec::with_capacity(external_count + own_services.len());
    for (x, svc) in external_services.iter_mut().enumerate() {
        let values = std::mem::take(&mut external_values[x]);
        svc.environment_parameters = (!values.is_empty()).then_some(values);
        let entered = entered_of(&external_statuses[x], external_stopped[x]);
        svc.environments = refs(&entered);
        entered_per_service.push(entered);
        service_environment_statuses.push(StepEnvironmentStatuses {
            environments: considered_of(&external_statuses[x], &external_superseded_by[x]),
            not_found: Vec::new(),
        });
    }
    let mut own_services = own_services;
    for (k, svc) in own_services.iter_mut().enumerate() {
        let e = n_steps + k;
        debug_assert!(svc.environment_parameters.is_none());
        let mut values = per_entity.next().unwrap_or_default();
        values.extend(deferred_per_entity.next().unwrap_or_default());
        svc.environment_parameters = (!values.is_empty()).then_some(values);
        let entered = entered_of(&statuses_per_entity[e], stopped[e]);
        svc.environments = refs(&entered);
        entered_per_service.push(entered);
        service_environment_statuses.push(StepEnvironmentStatuses {
            environments: considered_of(&statuses_per_entity[e], &superseded_by[e]),
            not_found: not_found.next().unwrap_or_default(),
        });
    }
    let mut services = external_services;
    services.extend(own_services);
    job.job_environments = (!environments.is_empty()).then(|| environments.clone());
    job.services = (!services.is_empty()).then_some(services);
    // Each Service's full path and filing index, in `job.services` order.
    let service_paths: Vec<Vec<PathElement>> = external_sources
        .iter()
        .map(|(doc, k)| path_index(&path_field(&docs.path(Some(*doc)), "services"), *k))
        .chain((0..n_own_services).map(|k| requesting[n_steps + k].path.clone()))
        .collect();
    let service_indices: Vec<usize> = (0..external_count)
        .map(|x| n_steps + n_own_services + x)
        .chain((0..n_own_services).map(|k| n_steps + k))
        .collect();

    // ── RFC 0009: the combined dependency graph and the Services' scopes ──
    //
    // Every edge is known now that every Session's set of Environments is
    // decided (`<StepDependency>` constraint 3, "Service scope"), except
    // for a Step or Service that is stopped: then neither the cycle check
    // nor the scopes are computed, and the checks that need them (an
    // inline Service's empty scope, an external Service's own checks and
    // buffered errors, and the wrapping-Environment rules for an external
    // Service's Sessions) are not made. A cycle is reported, naming it,
    // and the same checks are not made, since every scope is meaningless.
    // The wrapping-Environment rules for the Steps and the Job Template's
    // Services need no scope (a stopped entity's Sessions enter nothing),
    // so they are checked either way.
    // `used[k]`: whether `job.services[k]`'s scope is not empty, as far as
    // it is known.
    let mut used: Vec<bool> = (0..external_count)
        .map(|_| false)
        .chain((0..n_own_services).map(|_| true))
        .collect();
    for entry_errors in external_entry_errors {
        staged.absorb(entry_errors, &[]);
    }
    if !stopped.iter().chain(&external_stopped).any(|s| *s) {
        let analysis = composition::analyze(&job, &environment_documents, &requirement_bindings);
        if let Some(cycle) = analysis.cycle {
            // A cycle among one attached Environment Template's Services is
            // that document's (§1.2 *services* constraint 4), reported at
            // its `services` as template validation reports one; any other
            // at the Job Template's root.
            let path = match analysis.cycle_document {
                Some(job::Document::EnvironmentTemplate { index, .. }) => {
                    path_field(&docs.path(Some(index)), "services")
                }
                _ => job_path.clone(),
            };
            staged
                .at(Stage::Step7, ANY, 0)
                .add(&path, cycle.to_string());
        } else {
            for (k, (svc, scope)) in job
                .services
                .iter_mut()
                .flatten()
                .zip(analysis.scopes)
                .enumerate()
            {
                let empty =
                    matches!(&scope, job::ServiceScope::Steps { steps } if steps.is_empty());
                if k >= external_count && empty {
                    // §9.1 rule 4 at job creation: an `enableIf` that
                    // template validation could not decide left the inline
                    // Service without a Step.
                    staged.at(Stage::Step7, service_indices[k], ANY).add(
                        &service_paths[k],
                        format!(
                            "Service '{}' is unused: no Step is in its scope once every \
                             enableIf and exclusiveGroup is decided (Template Schemas §9.1 rule 4).",
                            svc.name
                        ),
                    );
                }
                used[k] = !empty;
                svc.scope = scope;
            }
        }
    }
    for (buffer, used) in external_errors.into_iter().zip(&used) {
        if *used {
            staged.absorb(buffer, &[]);
        }
    }

    check_wrap_layers(
        &job,
        &environments,
        &environment_documents,
        &job_path,
        &stopped[..n_steps],
        &mut staged,
    );
    let job_ctx = job_template
        .default_validation_context()
        .with_caller_limits(ctx.caller_limits.clone());
    let known = tvfs::known_enable_ifs(job_template, &job_ctx);
    check_wrap_hooks(
        &job,
        job_template,
        &known,
        &environments,
        &environment_documents,
        &sources,
        &used,
        &stopped[..n_steps],
        &service_paths,
        &service_indices,
        &docs,
        &job_path,
        &mut staged,
    );
    // ── §3.3.3: each Step's and each Service's combined host requirements ──
    // Last in §7.4.1: they are resolved with the entity's effective values,
    // and only for the Environments its Sessions enter. Each entity's own
    // discarded requirements come first: the Steps', then the Services' in
    // `job.services` order (the external ones first).
    let mut discarded_host_requirements: Vec<Vec<DiscardedHostRequirement>> = selections[..n_steps]
        .iter()
        .map(|selection| selection.discarded_host_requirements.clone())
        .chain(external_discarded)
        .chain(
            selections[n_steps..]
                .iter()
                .map(|selection| selection.discarded_host_requirements.clone()),
        )
        .collect();
    super::host_requirements::combine_step_host_requirements(
        &mut job,
        &sources,
        &entered_per_step,
        &entered_per_service,
        &service_paths,
        &service_indices,
        &used,
        &job_path,
        &default_symtab,
        &env_names_value(env_names),
        super::EvalBudgets::from_ctx(ctx),
        &mut discarded_host_requirements,
        &mut staged,
    );
    super::finish_errors(check_errors, body_staged, staged, !attached.is_empty())?;
    limits?;
    let discarded_service_host_requirements =
        discarded_host_requirements.split_off(n_steps.min(discarded_host_requirements.len()));

    Ok(CreatedJob {
        job,
        environment_documents,
        warnings,
        disabled_environments,
        environment_statuses,
        discarded_host_requirements,
        service_environment_statuses,
        discarded_service_host_requirements,
        requirement_bindings,
    })
}

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
/// as §7.4.1 step 6 leaves them (§7.4.1 step 7). Template validation
/// checks the Job Template's own layers that are certainly entered
/// (entries and Environments whose `enableIf` is known there, §7.6 rule
/// 5); this check also covers an `enableIf` that is not known there and
/// the attached Environments (plan R8).
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
                wrap_layers_message(&layers, false),
            );
        }
    }
}

/// The single-layer rule's message for a Step whose Sessions would enter
/// the wrapping Environments `layers` (two or more), or a Service's
/// (`service`), which names the Service Sessions' wrap hooks.
fn wrap_layers_message(layers: &[String], service: bool) -> String {
    let hooks = if service {
        "onWrapEnvEnter, onWrapEnvExit, onWrapService*"
    } else {
        "onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit"
    };
    format!(
        "its Sessions would enter {} Environments that define wrap hooks ({hooks}): {}. A \
         Session may have at most one Environment defining wrap hooks (RFC 0008).",
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
/// when its own `enableIf` is known to be `true` (§4, after item 10; §7.6
/// rule 5; `known`), except an `ALWAYS` one, which cannot be disabled or
/// declined (§3.2.2 item 6), and, for one with an `exclusiveGroup`, only
/// when every earlier one of its group is known not to be entered
/// (`entered`, per Step and Environment, from
/// [`certainly_entered_per_step`]). An entry or Environment whose `enableIf`
/// is not known is decided at job creation, which checks the rule again
/// over the full set; an Environment whose `enableIf` is known to be
/// `false` is not entered, and an entry known to be disabled declines a
/// `BY_DEFAULT` one (§3.2 item 3.2). Template validation counts the Job Template's own ungrouped
/// layers the same way, so a Step with only those passes here too.
fn check_attached_wrap_layers(
    job_template: &JobTemplate,
    attached: &[AttachedEnvironmentTemplate<'_>],
    doc_names: &[String],
    entered: &[Vec<bool>],
    known: &KnownEnableIfs,
    job_path: &[PathElement],
    errors: &mut ValidationErrors,
) {
    let wraps = |env: &crate::template::Environment| {
        env.script
            .as_ref()
            .is_some_and(|s| s.actions.has_any_wrap_hook())
    };
    // A services-only attachment (RFC 0009) has no Environment.
    let wrapping_attached: Vec<usize> = (0..attached.len())
        .filter(|d| attached[*d].template.environment().is_some_and(wraps))
        .collect();
    let own_envs = job_template.job_environments.as_deref().unwrap_or(&[]);
    for (i, step) in job_template.steps.iter().enumerate() {
        let mut layers: Vec<String> = wrapping_attached
            .iter()
            .filter(|d| entered[i][**d])
            .map(|d| {
                format!(
                    "'{}' ({})",
                    attached[*d]
                        .template
                        .environment()
                        .map_or("", |env| env.name.as_str()),
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
                .enumerate()
                .filter(|(j, env)| wraps(env) && known.step_environment(i, *j) == Some(true))
                .map(|(_, env)| format!("'{}' (stepEnvironments)", env.name)),
        );
        if layers.len() > 1 {
            errors.add(
                &path_index(&path_field(job_path, "steps"), i),
                wrap_layers_message(&layers, false),
            );
        }
    }
}

/// For each entity of `entities` (the Job Template's Steps, then with
/// `SERVICE` its Services), whether its Sessions are certainly entered into
/// each Environment of the Job, attached ones first and then the Job
/// Template's own, in the Job's Environment order (§1.1 item 7), by
/// attachment index and then `attached.len() + k` for `jobEnvironments[k]`
/// (a services-only attachment, RFC 0009, is never entered): each one's
/// state from what its own `enableIf` (`attached_enabled`, or `known` for a
/// Job Environment) and the entity's entry for it (`known_entries`, per
/// entity and attachment; `own_entries`, per entity and Job Environment)
/// are known to be, then the `exclusiveGroup` bullet (§4, after item 10), so
/// a grouped one counts only when every earlier one of its group is known
/// not to be entered. A Service's Sessions select Environments exactly as a
/// Step's do (RFC 0009 "Services run inside Environments").
fn certainly_entered_per_entity(
    job_template: &JobTemplate,
    attached: &[AttachedEnvironmentTemplate<'_>],
    attached_enabled: &[Option<bool>],
    known_entries: &[Vec<KnownEntry>],
    own_entries: &[Vec<KnownEntry>],
    known: &KnownEnableIfs,
) -> Vec<Vec<bool>> {
    // §4 item 4: an attachment from a document that does not list
    // `ENV_PARAMS` is known to be entered, whatever the entity's entry is.
    let lists_env_params: Vec<bool> = attached
        .iter()
        .map(|a| {
            a.template
                .profile()
                .has_extension(ModelExtension::EnvParams)
        })
        .collect();
    let own_envs = job_template.job_environments.as_deref().unwrap_or(&[]);
    known_entries
        .iter()
        .zip(own_entries)
        .map(|(entries, own)| {
            let states = attached
                .iter()
                .zip(entries)
                .zip(&lists_env_params)
                .zip(attached_enabled)
                .map(
                    |(((a, k), lists), enabled)| match a.template.environment() {
                        Some(env) => (env.group(), env.known_state_in(*lists, *enabled, *k)),
                        None => (None, Known::NotEntered),
                    },
                )
                .chain(
                    own_envs
                        .iter()
                        .zip(own)
                        .enumerate()
                        .map(|(k, (env, entry))| {
                            (
                                env.group(),
                                env.known_state(known.job_environment(k), *entry),
                            )
                        }),
                );
            apply_exclusive_groups(states)
                .into_iter()
                .map(|s| s == Known::Entered)
                .collect()
        })
        .collect()
}

/// RFC 0009 wrap hook rule 1 over the combined Job, once every entity's
/// Environments are known, and the Service Sessions' single-layer rule
/// (RFC 0008). A wrapping Job Environment that a Task Session enters must
/// define `onWrapEnvEnter`, `onWrapTaskRun`, and `onWrapEnvExit`, and one
/// that the Sessions of a Service with a non-empty scope (`used`) enter
/// must define `onWrapEnvEnter`, `onWrapEnvExit`, and the four
/// `onWrapService*` hooks. The Task-Session case is checked here only for
/// an Environment from a document that declares `SERVICE`: without it, RFC
/// 0008's rule that a wrapper defines all three of its hooks was checked
/// at template validation. The Service-Session case covers every document
/// (Template Schemas §1.2.2 item 4): one that does not declare `SERVICE`
/// cannot define the hooks. A wrapping Step Environment is entered only by
/// its Step's Task Sessions; template validation checked the ones whose
/// `enableIf` is known to be `true` (`known`, §7.6 rule 5).
///
/// A Service whose scope is not known to be non-empty (`used` false) is
/// left out of both; a stopped entity enters no Job Environment, and a
/// stopped Step's (`stopped_steps`) Step Environments are skipped, so a
/// stopped entity is never checked.
///
/// The hook errors are filed under §7.4.1 step 7, after every entity's
/// errors, at each Environment's place in the Job's order; a Service's
/// single-layer error under the Service (`service_indices`).
#[allow(clippy::too_many_arguments)]
fn check_wrap_hooks(
    job: &job::Job,
    job_template: &JobTemplate,
    known: &KnownEnableIfs,
    environments: &[job::Environment],
    documents: &[job::Document],
    sources: &[super::host_requirements::EnvironmentSource<'_>],
    used: &[bool],
    stopped_steps: &[bool],
    service_paths: &[Vec<PathElement>],
    service_indices: &[usize],
    docs: &Documents<'_>,
    job_path: &[PathElement],
    staged: &mut StagedErrors,
) {
    use crate::template::validate_v2023_09::wrap_actions::defined_wrap_hooks;
    let services = job.services.as_deref().unwrap_or(&[]);
    let mut hook_errors = ValidationErrors::default();
    let errors = &mut hook_errors;
    let mut check = |env: &job::Environment,
                     doc: &job::Document,
                     path: Vec<PathElement>,
                     (declares_service, declares_env_params): (bool, bool),
                     task: Option<String>,
                     service: Option<&job::Service>| {
        let document = docs.name(match doc {
            job::Document::JobTemplate => None,
            job::Document::EnvironmentTemplate { index, .. } => Some(*index),
        });
        let who = service.map(|svc| {
            if svc.document.is_job_template() {
                format!("Service '{}'", svc.name)
            } else {
                format!("Service '{}' ({})", svc.name, svc.document)
            }
        });
        report_wrap_hooks(
            &env.name,
            env.script
                .as_ref()
                .map(|s| defined_wrap_hooks(&s.actions.wrap_hooks())),
            &document,
            &path,
            declares_service,
            declares_env_params,
            task.as_deref(),
            who.as_deref(),
            errors,
        );
    };
    for ((env, doc), src) in environments.iter().zip(documents).zip(sources) {
        let declares_service = src.ctx.profile.has_extension(ModelExtension::Service);
        // Without SERVICE, RFC 0008's rule that a wrapper defines all three
        // of its hooks was checked at template validation.
        let task = declares_service
            .then(|| {
                job.steps
                    .iter()
                    .find(|s| s.enters_environment(doc, &env.name))
                    .map(|s| format!("Step '{}'", s.name))
            })
            .flatten();
        let service = services
            .iter()
            .zip(used)
            .find(|(s, used)| **used && s.enters_environment(doc, &env.name))
            .map(|(s, _)| s);
        let declares_env_params = src.ctx.profile.has_extension(ModelExtension::EnvParams);
        check(
            env,
            doc,
            src.path.clone(),
            (declares_service, declares_env_params),
            task,
            service,
        );
    }
    // A wrapping Step Environment is entered only by its Step's Task
    // Sessions; template validation checked the ones whose `enableIf` is
    // known to be `true`, and job creation left out the disabled ones.
    if job.has_extension(ModelExtension::Service) {
        for (i, (step, st)) in job.steps.iter().zip(&job_template.steps).enumerate() {
            // A stopped Step's Step Environments are not decided (§7.4.1
            // steps 5–8 are not taken for it).
            if stopped_steps[i] {
                continue;
            }
            for env in step.step_environments.iter().flatten() {
                let Some(j) =
                    st.step_environments
                        .iter()
                        .flatten()
                        .enumerate()
                        .position(|(j, e)| {
                            e.name == env.name && known.step_environment(i, j) != Some(true)
                        })
                else {
                    continue;
                };
                let path = path_index(
                    &path_field(
                        &path_index(&path_field(job_path, "steps"), i),
                        "stepEnvironments",
                    ),
                    j,
                );
                check(
                    env,
                    &job::Document::JobTemplate,
                    path,
                    (true, true),
                    Some(format!("Step '{}'", step.name)),
                    None,
                );
            }
        }
    }
    staged
        .at(Stage::Step7, ANY, 1)
        .errors
        .extend(hook_errors.errors);
    // The Service Sessions' single-layer rule.
    let wraps = |env: &job::Environment| {
        env.script
            .as_ref()
            .is_some_and(|s| s.actions.has_any_wrap_hook())
    };
    for (((svc, used), path), index) in services
        .iter()
        .zip(used)
        .zip(service_paths)
        .zip(service_indices)
    {
        if !used {
            continue;
        }
        let layers: Vec<String> = environments
            .iter()
            .zip(documents)
            .filter(|(env, doc)| wraps(env) && svc.enters_environment(doc, &env.name))
            .map(|(env, doc)| match doc {
                job::Document::JobTemplate => format!("'{}'", env.name),
                other => format!("'{}' ({other})", env.name),
            })
            .collect();
        if layers.len() > 1 {
            let who = if svc.document.is_job_template() {
                format!("Service '{}'", svc.name)
            } else {
                format!("Service '{}' ({})", svc.name, svc.document)
            };
            staged.at(Stage::Step7, *index, ANY).add(
                path,
                format!("{who}: {}", wrap_layers_message(&layers, true)),
            );
        }
    }
}

/// Wrap hook rule 1 (RFC 0009) for one wrapping Environment `name`, with
/// the hooks `defined` (`None` without a script), defined by `document`
/// (named in prose) at `env_path`: when the Sessions of `task` (`Step
/// 'X'`) enter it, it must define the three Task-Session hooks, and when
/// those of `service` (`Service 'X'`, with its document when attached)
/// do, the six Service-Session hooks (Template Schemas §1.2.2 item 4). A
/// document that does not declare `SERVICE` (`declares_service`) cannot
/// define the latter, and the message says so, with the remedies RFC 0009
/// gives (Environment Template item 4), which depend on whether it lists
/// `ENV_PARAMS` (`declares_env_params`).
#[allow(clippy::too_many_arguments)]
fn report_wrap_hooks(
    name: &str,
    defined: Option<Vec<(&'static str, bool)>>,
    document: &str,
    env_path: &[PathElement],
    declares_service: bool,
    declares_env_params: bool,
    task: Option<&str>,
    service: Option<&str>,
    errors: &mut ValidationErrors,
) {
    use crate::template::validate_v2023_09::wrap_actions::{join_hook_names, missing_wrap_hooks};
    let Some(defined) = defined else {
        return;
    };
    let actions_path = path_field(&path_field(env_path, "script"), "actions");
    if let (Some(task), Some((required, missing))) = (task, missing_wrap_hooks(&defined, true)) {
        errors.add(
            &actions_path,
            format!(
                "the Sessions of {task} enter wrapping Environment '{name}', defined by \
                 {document}, which therefore must define {}; missing: {} (RFC 0009 wrap hook \
                 rule 1).",
                join_hook_names(&required),
                missing.join(", ")
            ),
        );
    }
    if let (Some(who), Some((required, missing))) = (service, missing_wrap_hooks(&defined, false)) {
        // RFC 0009 Environment Template item 4: the remedies, for a document
        // that cannot define the hooks.
        let note = if declares_service {
            String::new()
        } else if declares_env_params {
            format!(
                " Its document, {document}, does not declare the SERVICE extension, so it \
                 cannot define the onWrapService* hooks: declare SERVICE and define them, or \
                 keep the Service's Sessions out of the Environment, either by making it \
                 ON_REQUEST, so that only the Steps that request it enter it, or by leaving or \
                 making it BY_DEFAULT and having the Service decline it with a disabled \
                 'environment:' dependency."
            )
        } else {
            format!(
                " Its document, {document}, does not declare the SERVICE extension, so it \
                 cannot define the onWrapService* hooks: declare SERVICE and define them, or \
                 declare ENV_PARAMS and EXPR and either make the Environment ON_REQUEST, so that \
                 only the Steps that request it enter it, or leave it BY_DEFAULT, so that the \
                 Service can decline it with a disabled 'environment:' dependency."
            )
        };
        errors.add(
            &actions_path,
            format!(
                "the Sessions of {who} enter wrapping Environment '{name}', defined by \
                 {document}, which therefore must define {}; missing: {} (RFC 0009 wrap hook \
                 rule 1, Template Schemas §1.2.2 item 4).{note}",
                join_hook_names(&required),
                missing.join(", ")
            ),
        );
    }
}

/// Attribute an error raised while evaluating one attached document to that
/// document: validation errors get `doc` prefixed to every path and report
/// for `Submission`; format-string and expression errors get the document
/// name prefixed to their message.
fn in_document(err: ModelError, doc: &[PathElement]) -> ModelError {
    match err {
        ModelError::ModelValidation(mut ve) => {
            for e in &mut ve.errors {
                let mut path = doc.to_vec();
                path.append(&mut e.path);
                e.path = path;
            }
            ModelError::ModelValidation(ve.with_model_name(SUBMISSION))
        }
        ModelError::FormatStringError {
            message,
            input,
            start,
            end,
            source,
        } => ModelError::FormatStringError {
            message: format!("{}: {message}", path_to_string(doc)),
            input,
            start,
            end,
            source,
        },
        ModelError::Expression(e) => ModelError::Expression(openjd_expr::ExpressionError::new(
            format!("{}: {e}", path_to_string(doc)),
        )),
        other => other,
    }
}

/// The RFC 0009 requirement matching against the combined Job (Template
/// Schemas §1.2.2 item 2): each entry of the Job Template's
/// `externalServices` matches exactly one attached Service with the same
/// `name` (none or two or more is an error naming the documents), and that
/// Service declares every listed port with the same `protocol`, reported
/// at `JobTemplate -> externalServices[i]`. `requirements` are the Job
/// Template's `externalServices`. (Item 4, the wrapping Environments a Service
/// Session enters, needs every entity's Environments and is checked by
/// `check_wrap_hooks`.)
///
/// Returns the requirement bindings, or one `ModelValidation` error holding
/// every violation, shaped as job creation's (for `Submission` with paths
/// starting `JobTemplate` when Environment Templates are attached, for
/// `JobTemplate` at its own paths, `job_path` empty, when none is). Service
/// names are otherwise not compared across documents (§1.2.2 item 3).
fn check_services(
    requirements: &[job::ServiceRequirement],
    attached: &[AttachedEnvironmentTemplate<'_>],
    job_path: &[PathElement],
) -> Result<Vec<RequirementBinding>, ModelError> {
    let docs = Documents { attached };

    let mut errors = ValidationErrors::default();

    // ── §1.2.2 item 2: requirement matching ──
    let mut requirement_bindings = Vec::new();
    let requirements_path = path_field(job_path, "externalServices");
    for (i, req) in requirements.iter().enumerate() {
        let req_path = path_index(&requirements_path, i);
        let candidates: Vec<(usize, &crate::template::Service)> = attached
            .iter()
            .enumerate()
            .flat_map(|(doc, att)| {
                att.template
                    .services()
                    .iter()
                    .filter(|s| s.name == req.name)
                    .map(move |s| (doc, s))
            })
            .collect();
        match candidates.as_slice() {
            [] => {
                let where_ = if attached.is_empty() {
                    "no Environment Template is attached".to_string()
                } else {
                    format!(
                        "none of the attached Environment Templates ({}) defines a Service named \
                         '{}'",
                        (0..attached.len())
                            .map(|d| docs.name(Some(d)))
                            .collect::<Vec<_>>()
                            .join(", "),
                        req.name
                    )
                };
                errors.add(
                    &req_path,
                    format!(
                        "required Service '{}' is not provided: {where_} (Template Schemas \
                         §1.2.2 item 2).",
                        req.name
                    ),
                );
            }
            [(doc, svc)] => {
                for port in &req.ports {
                    match svc.ports.iter().find(|p| p.name == port.name) {
                        None => errors.add(
                            &req_path,
                            format!(
                                "required Service '{}' is provided by {}, which is missing port \
                                 '{}'; its ports: {} (Template Schemas §1.2.2 item 2).",
                                req.name,
                                docs.name(Some(*doc)),
                                port.name,
                                svc.port_names().collect::<Vec<_>>().join(", ")
                            ),
                        ),
                        Some(p) if p.protocol != port.protocol => errors.add(
                            &req_path,
                            format!(
                                "required Service '{}' is provided by {}, whose port '{}' has \
                                 protocol {} but the requirement declares {} (Template Schemas \
                                 §1.2.2 item 2).",
                                req.name,
                                docs.name(Some(*doc)),
                                port.name,
                                p.protocol,
                                port.protocol
                            ),
                        ),
                        Some(_) => {}
                    }
                }
                requirement_bindings.push(RequirementBinding {
                    requirement: req.name.clone(),
                    document: job::Document::environment_template(*doc, attached[*doc].label),
                    service: svc.name.clone(),
                });
            }
            many => {
                let providers: Vec<String> =
                    many.iter().map(|(doc, _)| docs.name(Some(*doc))).collect();
                errors.add(
                    &req_path,
                    format!(
                        "required Service '{}' is ambiguous: {} attached Environment Templates \
                         define a Service with that name ({}); a requirement must match exactly \
                         one (Template Schemas §1.2.2 item 2).",
                        req.name,
                        many.len(),
                        providers.join(", ")
                    ),
                );
            }
        }
    }

    errors.into_result(if attached.is_empty() {
        "JobTemplate"
    } else {
        SUBMISSION
    })?;
    Ok(requirement_bindings)
}

/// Whether `env` defines any wrap hook (RFC 0008).
fn environment_wraps(env: &crate::template::Environment) -> bool {
    env.script
        .as_ref()
        .is_some_and(|s| s.actions.has_any_wrap_hook())
}

/// A Service of the combined Job as [`check_environment_templates`] sees it
/// for the wrapping-Environment checks.
struct ServiceView<'v> {
    /// `Service 'X'`, with its document when attached.
    who: &'v str,
    /// Its full path.
    path: &'v [PathElement],
    /// Whether its Sessions certainly enter each Environment.
    entered: &'v [bool],
    /// Whether its scope is known not to be empty.
    used: bool,
    /// Whether it is an attached Service.
    external: bool,
}

/// An attached (external) Service, for [`check_environment_templates`].
struct ExternalService<'t> {
    /// The attachment that declares it.
    doc: usize,
    svc: &'t crate::template::Service,
    /// `Service 'X' (<document>)`.
    who: String,
    /// Its full path, starting with its document's.
    path: Vec<PathElement>,
    /// The index in the check's Environments of its document's Environment,
    /// when that lists it.
    own_env: Option<usize>,
    /// Whether its Sessions certainly enter each Environment of the check,
    /// index for index.
    entered: Vec<bool>,
    /// The attachments whose Environment applies to it (§3.2 constraint
    /// 7): every `ALWAYS` one among its own and the earlier ones, and
    /// every `BY_DEFAULT` one it does not decline.
    applies: Vec<usize>,
}

/// A requesting entity of the Job Template for
/// [`check_environment_templates`]: a Step, or a Service (RFC 0009
/// `<StepDependency>` item 3).
struct StaticEntity<'t> {
    /// Its `name`.
    name: &'t str,
    /// `Step 'X'` or `Service 'X'`.
    who: String,
    kind: tv::RequesterKind,
    /// Its index among the Job Template's Steps, or among its Services.
    index: usize,
    /// Its full path, starting with its document's when Environment
    /// Templates are attached.
    path: Vec<PathElement>,
    dependencies: &'t [StepDependency],
    /// The names of a Step's `stepEnvironments` (§3.2.1 item 5); none for
    /// a Service.
    step_env_names: HashSet<&'t str>,
    host_requirements: Option<&'t crate::template::HostRequirements>,
}

impl StaticEntity<'_> {
    /// What the `enableIf` of `dependencies[j]` is known to be (§7.6 rule
    /// 5).
    fn known_entry(&self, known: &KnownEnableIfs, j: usize) -> Option<bool> {
        match self.kind {
            tv::RequesterKind::Step => known.entry(self.index, j),
            tv::RequesterKind::Service => known.service_entry(self.index, j),
        }
    }

    /// What the entity's first well-formed `environment:` entry naming
    /// `name` is known to be.
    fn known_entry_for(&self, known: &KnownEnableIfs, name: &str) -> KnownEntry {
        match self
            .dependencies
            .iter()
            .position(|d| d.environment() == Some(name))
        {
            None => KnownEntry::Absent,
            Some(j) => KnownEntry::from_known(self.known_entry(known, j)),
        }
    }
}

/// Check a Job Template against the Environment Templates it would be
/// submitted with, before any Job Parameter values are known (Template
/// Schemas §3.2, last paragraph: "a validator given the Environment
/// Templates may check these earlier"). Reports, in job creation's
/// messages and paths:
///
/// - with `SERVICE` (RFC 0009), first and alone on failure, the requirement
///   matching of §1.2.2 item 2, which needs no Job Parameter value;
/// - the attachment-level conditions: an attachment named like a Job
///   Environment (a warning when the Job Template lists `ENV_PARAMS`), an
///   ambiguous name, and every literal `default` (§4.5 item 2);
/// - for each Step and then (RFC 0009 `<StepDependency>` item 3) each of the
///   Job Template's Services, whose entries work exactly as a Step's: each
///   `environment:` entry that names no Job Environment and whose
///   `enableIf` is known to be `true` (absent, the boolean scalar `true`,
///   or an expression whose value is known, §7.6 rule 5), resolved by name
///   (§3.2.1 items 1–3), with a `required: false` entry that resolves to
///   nothing reported as a warning, and one whose `parameters` an
///   attachment that declares none ignores (§3.2.1 item 1) reported as a
///   warning and checked as an entry with no `parameters`. An entry known
///   to be disabled is resolved too, but supplies nothing and has no
///   effect when it matches no attachment (§3.2.1): it declines a
///   `BY_DEFAULT` attachment, which the checks below then leave out for
///   its entity, requests no `ON_REQUEST` one, is an error for an `ALWAYS`
///   one, naming the entity and the Environment (§3.2 item 3.2), and has no
///   effect on one whose Environment Template does not list `ENV_PARAMS`,
///   which behaves as `ALWAYS` (§4 item 4) and is entered whatever the
///   entry says. A disabled entry whose name two or more attachments
///   define has that effect on each of them, and is not an error; an
///   enabled one is an error (§3.2.1 item 3). One whose
///   `enableIf` is not known is checked only when the Job is created, and
///   so is every attachment it may name, for its entity;
/// - for an entry that resolves, constraint 5 (keys); and, when the
///   attachment is known not to be disabled for the entity (its
///   `activation` is `ALWAYS`, §3.2.2 item 6, or its own `enableIf` is
///   known to be `true`, or reads a literal value of the entity's that does
///   not convert, so it is not evaluated, §3.2.2 item 2; see
///   `enable_if_reads_unconvertible_literal`) and no earlier attachment of
///   its `exclusiveGroup` supersedes or may supersede it (§4 item 7, and
///   the bullets after item 10), constraint 6 for literal values (a PATH
///   value of any form is used as given, §4.5 item 3) and the
///   single-expression rule for a list-typed parameter;
/// - constraint 7 over every entity and every reachable attached
///   Environment that applies to it: those with `activation: ALWAYS`,
///   those with `BY_DEFAULT` it does not decline, and each one with
///   `activation: ON_REQUEST` that one of its enabled entries refers to,
///   whatever the attachment's own `enableIf` or `exclusiveGroup` (§3.2
///   constraint 7). When an entity's entry for an `ALWAYS` Environment with
///   missing parameters is known to be disabled, the message says so;
/// - the value checks for each Job Environment with an `exclusiveGroup`
///   that the entity is known to enter, given the attachments: template
///   validation, which does not know them, leaves these to job creation.
///   With `attached` empty, every such Job Environment that is first in
///   its group is checked. Their constraint 7 does not depend on the group,
///   so template validation checks it;
/// - the unreachable attachments' parameters without a `default`: an error
///   for one with `activation: ALWAYS` or `BY_DEFAULT` that certainly
///   applies to some entity (no enabled entry decides it, and a
///   `BY_DEFAULT` one applies to each entity that does not decline it),
///   whatever its own `enableIf` or `exclusiveGroup` (§3.2 constraint 7),
///   reported after the entities' values, as job creation orders them;
/// - with `SERVICE`, for each attached Service: its disabled entries
///   resolved among its own document's and the earlier attachments'
///   Environments (§1.2 *services* constraint 5; one whose name two or more
///   earlier attachments define refers to each of them, and declining an
///   `ALWAYS` one is an error), and, for one whose scope is known not to be
///   empty, constraint 7 over the Environments that apply to it; then the
///   cycles of `<StepDependency>` constraint 3 over the combined Job whose
///   edges are all known, reported after the values;
/// - the `WRAP_ACTIONS` single-layer rule (RFC 0008) for each Step (one
///   whose only layers are the Job Template's own ungrouped ones passed
///   template validation, which counts them the same way; it counts no
///   grouped one), over the wrapping Environments its Sessions certainly
///   enter: attachments with `activation: ALWAYS` or from a document that
///   does not list `ENV_PARAMS` (whatever the Step's entry for one says),
///   those with `BY_DEFAULT` that no entry of the Step disables or may
///   disable, and the `ON_REQUEST` ones an enabled entry resolved to, Job
///   Environments in the same way, with an entry whose `enableIf` is known
///   to be `true` enabled, and its Step Environments, each only when its
///   own `enableIf` is known to be `true` or it is `ALWAYS`, and, for one
///   with an `exclusiveGroup`, only when every earlier one of its group is
///   known not to be entered, in job creation's form (job creation may
///   list more layers, when an `enableIf` that is not known here brings
///   one in); with `SERVICE`, also RFC 0009's wrap hook rule 1 for the
///   wrapping Environments a Step or a Service known to be used certainly
///   enters (§1.2.2 item 4), and the single-layer rule for such a Service
///   over wrapping Environments of another document;
/// - the Steps' and the used Services' combined host requirements (§3.3.3)
///   over the requirements that are literals (no format-string
///   expression): each entity's own and those of the Environments its
///   Sessions certainly enter, as for the single-layer rule (so not one
///   whose own `enableIf` is not known to be `true`, unless it is
///   `ALWAYS`). A conflict among them is a conflict of the full
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
/// conditions, then entity by entity (the Steps, then the Services) the
/// entries' resolution and the values, then the unreachable attachments,
/// the Steps' wrap layers, the attached Services, a cycle (after which
/// nothing more is checked), the wrap hooks and the Services' wrap layers,
/// and the host requirements. Apart from what a cycle cuts short, it
/// reports every violation it is certain of, so it can report more than
/// job creation, which orders its errors by §7.4.1
/// step and takes no step after step 4 for an entity with an error from
/// steps 1–4 (a value error of an entity whose entry is unresolved, say, is
/// reported here and not by job creation until the entry resolves). Job
/// creation may also list more Environments in an entity's constraint 7
/// error (this one leaves out those it cannot know apply to it, such as
/// one an entry whose `enableIf` is not known names).
pub fn check_environment_templates(
    job_template: &JobTemplate,
    attached: &[AttachedEnvironmentTemplate<'_>],
    ctx: &ValidationContext,
) -> Result<Vec<SubmissionWarning>, ModelError> {
    let docs = Documents { attached };
    // Shaped by the inputs, as job creation's error is: paths start with
    // their document only when Environment Templates are attached.
    let job_path = if attached.is_empty() {
        Vec::new()
    } else {
        docs.path(None)
    };
    let model = if attached.is_empty() {
        "JobTemplate"
    } else {
        SUBMISSION
    };
    // RFC 0009 §1.2.2 item 2 first, as at job creation: it needs no Job
    // Parameter value, and a failure is reported alone.
    let requirements: Vec<job::ServiceRequirement> = job_template
        .external_services()
        .iter()
        .map(|r| job::ServiceRequirement {
            name: r.name.clone(),
            ports: r
                .ports
                .iter()
                .map(|p| job::ServiceRequirementPort {
                    name: p.name.clone(),
                    protocol: p.protocol,
                })
                .collect(),
        })
        .collect();
    let requirement_bindings = check_services(&requirements, attached, &job_path)?;
    let job_env_names: HashSet<&str> = job_template
        .job_environments
        .iter()
        .flatten()
        .map(|e| e.name.as_str())
        .collect();

    let mut errors = ValidationErrors::default();
    let mut warnings = Vec::new();
    // What each `enableIf` is known to be before job creation (§7.6 rule
    // 5), as template validation knows it.
    let job_ctx = job_template
        .default_validation_context()
        .with_caller_limits(ctx.caller_limits.clone());
    let known = tvfs::known_enable_ifs(job_template, &job_ctx);
    // Literal `default`s read no symbols, and the other `default`s are not
    // resolved here.
    let no_symbols = openjd_expr::SymbolTable::new();
    let job_lists_env_params = job_template
        .profile()
        .has_extension(ModelExtension::EnvParams);
    let set = Attachments::new(
        attached,
        &job_env_names,
        job_lists_env_params,
        &no_symbols,
        ctx,
        &mut errors,
        &mut warnings,
    );

    // The requesting entities of the Job Template: its Steps, then its
    // Services, in job creation's order.
    let entities: Vec<StaticEntity<'_>> = job_template
        .steps
        .iter()
        .enumerate()
        .map(|(i, step)| StaticEntity {
            name: &step.name,
            who: format!("Step '{}'", step.name),
            kind: tv::RequesterKind::Step,
            index: i,
            path: path_index(&path_field(&job_path, "steps"), i),
            dependencies: step.dependencies.as_deref().unwrap_or(&[]),
            step_env_names: step
                .step_environments
                .iter()
                .flatten()
                .map(|e| e.name.as_str())
                .collect(),
            host_requirements: step.host_requirements.as_ref(),
        })
        .chain(
            job_template
                .services()
                .iter()
                .enumerate()
                .map(|(k, svc)| StaticEntity {
                    name: &svc.name,
                    who: format!("Service '{}'", svc.name),
                    kind: tv::RequesterKind::Service,
                    index: k,
                    path: path_index(&path_field(&job_path, "services"), k),
                    dependencies: svc.dependencies.as_deref().unwrap_or(&[]),
                    step_env_names: HashSet::new(),
                    host_requirements: svc.host_requirements.as_ref(),
                }),
        )
        .collect();
    let n_steps = job_template.steps.len();

    // Per entity: the attachment each resolved entry refers to, with the
    // entry's values and path. Ambiguous names, and the names of entries
    // whose `enableIf` is not known (§7.6 rule 5), are skipped for the
    // entity.
    let empty = BTreeMap::new();
    let mut resolved: Vec<Vec<(usize, Supplied<'_>)>> = Vec::new();
    let mut unapplied: Vec<HashSet<usize>> = Vec::new();
    // Per entity: the attachments an ambiguous entry named, a subset of
    // `unapplied`, for which job creation computes no values, and those an
    // entry whose `enableIf` is not known may name and whose values are not
    // certain both with and without the entry, for which it may compute
    // none.
    let mut ambiguous: Vec<HashSet<usize>> = Vec::new();
    // Per entity: the names of entries known to be disabled.
    let mut disabled: Vec<Vec<&str>> = Vec::new();
    // Per entity: the `BY_DEFAULT` attachments such an entry declines, and
    // the attachments an entry whose `enableIf` is not known may name
    // (§3.2 item 3.2).
    let mut declined: Vec<HashSet<usize>> = Vec::new();
    let mut undetermined: Vec<HashSet<usize>> = Vec::new();
    for entity in &entities {
        let deps_path = path_field(&entity.path, "dependencies");
        let mut entity_resolved = Vec::new();
        let mut entity_unapplied = HashSet::new();
        let mut entity_ambiguous = HashSet::new();
        let mut entity_disabled = Vec::new();
        let mut entity_declined = HashSet::new();
        let mut entity_undetermined = HashSet::new();
        let mut seen = HashSet::new();
        for (j, dep) in entity.dependencies.iter().enumerate() {
            let Some(name) = dep.environment() else {
                continue;
            };
            // Template validation checked entries naming a Job Environment
            // and reported repeated names (constraint 3).
            if job_env_names.contains(name) || !seen.insert(name) {
                continue;
            }
            let entry_path = path_index(&deps_path, j);
            match entity.known_entry(&known, j) {
                Some(true) => {}
                // A disabled entry supplies nothing, but its name is
                // resolved to learn what it means (§3.2 item 3.2, §3.2.1),
                // as `apply_environment_templates` does: one that matches
                // no attachment has no effect.
                // A name that two or more attachments define is not an
                // error for a disabled entry: it refers to each of them
                // (§3.2.1 item 3).
                Some(false) => {
                    entity_disabled.push(name);
                    for &doc in set.candidates(name) {
                        let de = set.de(doc);
                        match de.activation {
                            EnvironmentActivation::OnRequest => {}
                            EnvironmentActivation::ByDefault => {
                                entity_declined.insert(doc);
                            }
                            // No effect on a pre-extension one, which
                            // behaves as `ALWAYS` (§4 item 4).
                            EnvironmentActivation::Always if de.pre_extension => {}
                            EnvironmentActivation::Always => errors.add(
                                &path_field(&entry_path, "enableIf"),
                                always_entry_disabled_message(entity.kind, entity.name, &de.label),
                            ),
                        }
                    }
                    continue;
                }
                // Whether the entry is enabled is known only when the Job
                // is created (§7.6 rule 5), so the Environment it may name
                // is not checked for this entity (§3.2, checked-at
                // paragraph). A name that two attachments define is an
                // error only for an enabled entry (§3.2.1 item 3), so it is
                // not reported now either (§7.6 rule 5). Job creation has
                // values for that Environment either way only when they are
                // certain both with the entry's values and without them (the
                // `default`s); otherwise it is left out of the
                // host-requirement combination as for an ambiguous entry.
                None => {
                    let candidates = set.candidates(name);
                    entity_unapplied.extend(candidates.iter().copied());
                    entity_undetermined.extend(candidates.iter().copied());
                    let entry = Supplied {
                        params: dep.parameters.as_ref().map_or(&empty, |p| &p.0),
                        symtab: &no_symbols,
                        entry_path: entry_path.clone(),
                    };
                    // With two or more candidates the name is ambiguous
                    // where the entry is enabled, and job creation then stops
                    // the entity and has no values for any of them.
                    let ambiguous_name = candidates.len() > 1;
                    for &doc in candidates {
                        let de = set.de(doc);
                        let policy = ResolvePolicy::new(&set.contexts[doc]);
                        // Constraint 5 (keys) alone does not reject the
                        // values, as at job creation; `uncertain` covers a
                        // missing value and a failed or non-literal one.
                        let mut scratch = ValidationErrors::default();
                        let uncertain = ambiguous_name
                            || [Some(&entry), None].into_iter().any(|supplied| {
                                de.check_step_statically(supplied, true, policy, &mut scratch)
                                    .1
                            });
                        if uncertain {
                            entity_ambiguous.insert(doc);
                        }
                    }
                    continue;
                }
            }
            match set.match_entry(
                entity.kind,
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
                        entity.kind,
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
                    entity_resolved.push((
                        doc,
                        Supplied {
                            params,
                            symtab: &no_symbols,
                            entry_path,
                        },
                    ));
                }
                EntryMatch::Many(many) => {
                    entity_unapplied.extend(many.iter().copied());
                    entity_ambiguous.extend(many.iter().copied());
                }
            }
        }
        resolved.push(entity_resolved);
        unapplied.push(entity_unapplied);
        ambiguous.push(entity_ambiguous);
        disabled.push(entity_disabled);
        declined.push(entity_declined);
        undetermined.push(entity_undetermined);
    }

    // The Job Template's own Environments that have an `exclusiveGroup`.
    // Template validation, which does not know the attachments, never
    // counts one as entered (§4, after item 10), so their value checks are
    // made here for each entity that is known to enter them, with the
    // attachments known. Their constraint 7 does not depend on the group,
    // so template validation checks it.
    let own_envs = job_template.job_environments.as_deref().unwrap_or(&[]);
    // What each attachment's own `enableIf` is known to be.
    let attached_enabled: Vec<Option<bool>> = attached
        .iter()
        .zip(&set.contexts)
        .map(|(a, c)| tvfs::known_environment_template_enable_if(a.template, c))
        .collect();
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
    // Per attachment: whether it applies to some entity, for the
    // unreachable attachments' error (§3.2 constraint 7).
    let mut applies_to_some_entity = vec![false; set.declaring.len()];
    // Per entity: the attachments whose values for it were rejected
    // (constraint 6 or 7), for which job creation has no values either, or
    // are not all literals, which only job creation resolves.
    let mut uncertain_docs: Vec<HashSet<usize>> = Vec::with_capacity(entities.len());
    for (n, entity) in entities.iter().enumerate() {
        let mut missing = Vec::new();
        let mut entity_uncertain = HashSet::new();
        // §4 item 4, §3.2 item 3.2: every `ALWAYS` attachment applies to
        // the entity, every `BY_DEFAULT` one a disabled entry does not
        // decline, and each `ON_REQUEST` one an enabled entry refers to.
        // Constraint 7 holds for each of them, whatever its `enableIf` or
        // group (§3.2 constraint 7). The value checks do not apply to an
        // entity for which the Environment is disabled (§3.2.2), so they are
        // made only for one known not to be: its own `enableIf` is known to
        // be `true` (§7.6 rule 5), and no earlier attachment of its
        // `exclusiveGroup` may supersede it (§4 item 7, and the paragraph
        // after item 10). The Job Template's own Environments come after
        // the attachments in the Job's Environment order, so they supersede
        // none.
        let mut states = Vec::with_capacity(set.declaring.len());
        let mut checks = Vec::with_capacity(set.declaring.len());
        for (doc, (de, att_ctx)) in set.declaring.iter().zip(&set.contexts).enumerate() {
            let Some(de) = de else {
                // A services-only attachment (RFC 0009) has no Environment.
                states.push((None, Known::NotEntered));
                checks.push((false, None));
                continue;
            };
            let supplied = resolved[n].iter().find(|(d, _)| *d == doc).map(|(_, s)| s);
            let enters = match de.activation {
                EnvironmentActivation::Always => true,
                EnvironmentActivation::ByDefault => !declined[n].contains(&doc),
                EnvironmentActivation::OnRequest => supplied.is_some(),
            };
            // `activation` is `ALWAYS` for a pre-extension attachment.
            let enabled = if de.activation == EnvironmentActivation::Always {
                Some(true)
            } else {
                attached_enabled[doc]
            };
            // A `BY_DEFAULT` one that an entry whose `enableIf` is not known
            // may name may still be declined, so it applies to the entity
            // only if no such entry does.
            applies_to_some_entity[doc] |= enters
                && !(de.activation == EnvironmentActivation::ByDefault
                    && undetermined[n].contains(&doc));
            // An attachment an entry whose `enableIf` is not known may name
            // is in `undetermined`: whether that entry requests or declines
            // it is decided at job creation, also for an ambiguous one,
            // which only a disabled entry can decide. No enabled entry
            // decides an unreachable one (named like a Job Environment, or
            // ambiguous, which is reported for an enabled entry): it is
            // entered by its `activation`, `enableIf`, and the disabled
            // entries that decline it, and its values are not checked (job
            // creation stops an entity whose enabled entry names an
            // ambiguous one). An `enableIf` that
            // reads a literal that does not convert is certainly not
            // evaluated, so it does not disable the attachment even when it
            // is known to be `false` (§3.2.2 item 2).
            let not_evaluated = tv::enable_if_reads_unconvertible_literal(
                de.env,
                supplied.map(|s| s.params),
                att_ctx,
            );
            let state = if enabled == Some(false) && !not_evaluated {
                Known::NotEntered
            } else if undetermined[n].contains(&doc) {
                Known::Undetermined
            } else if !enters {
                Known::NotEntered
            } else if enabled == Some(true) || not_evaluated {
                // Known to be `true`, or certainly not evaluated, so not
                // disabled (§3.2.2 item 2).
                Known::Entered
            } else {
                Known::Undetermined
            };
            states.push((de.env.group(), state));
            checks.push((enters && !unapplied[n].contains(&doc), supplied));
        }
        // The Job Template's own Environments come after the attachments in
        // the Job's Environment order. Only the grouped ones are checked
        // here; the others were checked at template validation.
        let entries: Vec<(KnownEntry, Option<&StepDependency>)> = own_envs
            .iter()
            .map(|env| {
                let dep = entity
                    .dependencies
                    .iter()
                    .find(|d| d.environment() == Some(env.name.as_str()));
                (entity.known_entry_for(&known, &env.name), dep)
            })
            .collect();
        for (k, (env, (entry, dep))) in own_envs.iter().zip(&entries).enumerate() {
            let supplied = dep
                .filter(|_| *entry == KnownEntry::Enabled)
                .and_then(|d| d.parameters.as_ref())
                .map(|p| &p.0);
            // An `enableIf` that reads a literal that does not convert is
            // certainly not evaluated, so it does not disable the
            // Environment, even when it is known to be `false` (§3.2.2 item
            // 2): the Environment is then entered whenever its `activation`
            // and the entity's entry enter it.
            let state = if env.known_state(Some(true), *entry) == Known::Entered
                && tv::enable_if_reads_unconvertible_literal(env, supplied, &job_ctx)
            {
                Known::Entered
            } else {
                env.known_state(known.job_environment(k), *entry)
            };
            states.push((env.group(), state));
        }
        let states = apply_exclusive_groups(states);
        for (doc, ((de, att_ctx), (checked, supplied))) in set
            .declaring
            .iter()
            .zip(&set.contexts)
            .zip(checks)
            .enumerate()
        {
            let (Some(de), true) = (de, checked) else {
                continue;
            };
            let values_checked = states[doc] == Known::Entered;
            let (m, uncertain) = de.check_step_statically(
                supplied,
                values_checked,
                ResolvePolicy::new(att_ctx),
                &mut errors,
            );
            if uncertain {
                entity_uncertain.insert(doc);
            }
            set.sort_missing(
                doc,
                m,
                &entity.step_env_names,
                &entity.path,
                &mut missing,
                &mut errors,
            );
        }
        let disabled = set.disabled_labels(disabled[n].iter().copied());
        report_missing(
            &entity.path,
            &missing,
            every_attached(entity.kind),
            entity.kind.noun(),
            &disabled,
            &mut errors,
        );
        // The grouped Job Environments the entity is known to enter, whose
        // values are checked as job creation checks them. Their
        // constraint 7 was checked at template validation.
        let deps_path = path_field(&entity.path, "dependencies");
        for (k, de) in &grouped_job_envs {
            if states[set.declaring.len() + k] != Known::Entered {
                continue;
            }
            let (entry, dep) = entries[*k];
            let supplied = dep.filter(|_| entry == KnownEntry::Enabled).map(|d| {
                let j = entity
                    .dependencies
                    .iter()
                    .position(|x| std::ptr::eq(x, d))
                    .unwrap_or_default();
                Supplied {
                    params: d.parameters.as_ref().map_or(&empty, |p| &p.0),
                    symtab: &no_symbols,
                    entry_path: path_index(&deps_path, j),
                }
            });
            de.check_step_statically(
                supplied.as_ref(),
                true,
                ResolvePolicy::new(&job_ctx),
                &mut errors,
            );
        }
        uncertain_docs.push(entity_uncertain);
    }
    // An unreachable attachment's parameters are needed wherever it
    // applies to an entity, whatever its `enableIf` or group (§3.2
    // constraint 7). Reported after the entities' values, as job creation
    // reports it.
    set.report_unreachable(
        |_, path, message| errors.add(path, message),
        |doc| applies_to_some_entity[doc],
    );
    // What each entity's entry for each attachment, and for each Job
    // Environment, is known to be.
    let known_entries: Vec<Vec<KnownEntry>> = (0..entities.len())
        .map(|n| {
            (0..attached.len())
                .map(|d| {
                    if resolved[n].iter().any(|(r, _)| *r == d) {
                        KnownEntry::Enabled
                    } else if declined[n].contains(&d) {
                        KnownEntry::Disabled
                    } else if undetermined[n].contains(&d) {
                        KnownEntry::Undetermined
                    } else {
                        KnownEntry::Absent
                    }
                })
                .collect()
        })
        .collect();
    let own_entries: Vec<Vec<KnownEntry>> = entities
        .iter()
        .map(|entity| {
            own_envs
                .iter()
                .map(|env| entity.known_entry_for(&known, &env.name))
                .collect()
        })
        .collect();
    // By attachment index, then `attached.len() + k` for
    // `jobEnvironments[k]`.
    let certainly_entered = certainly_entered_per_entity(
        job_template,
        attached,
        &attached_enabled,
        &known_entries,
        &own_entries,
        &known,
    );
    check_attached_wrap_layers(
        job_template,
        attached,
        &set.doc_names,
        &certainly_entered[..n_steps],
        &known,
        &job_path,
        &mut errors,
    );

    // The Environments of the combined Job, in job creation's order: the
    // attachments that define one (a services-only one, RFC 0009, defines
    // none), in attachment order, then the Job Template's own.
    let env_docs: Vec<usize> = (0..attached.len())
        .filter(|d| attached[*d].template.environment().is_some())
        .collect();
    let n_attached_envs = env_docs.len();
    let documents: Vec<job::Document> = env_docs
        .iter()
        .map(|d| job::Document::environment_template(*d, attached[*d].label))
        .chain(std::iter::repeat_n(
            job::Document::JobTemplate,
            own_envs.len(),
        ))
        .collect();
    let sources: Vec<super::host_requirements::EnvironmentSource<'_>> = documents
        .iter()
        .enumerate()
        .map(|(e, document)| {
            if e < n_attached_envs {
                let d = env_docs[e];
                super::host_requirements::EnvironmentSource {
                    environment: attached[d]
                        .template
                        .environment()
                        .expect("env_docs holds attachments that define an Environment"),
                    document,
                    path: path_field(&docs.path(Some(d)), "environment"),
                    ctx: &set.contexts[d],
                }
            } else {
                let j = e - n_attached_envs;
                super::host_requirements::EnvironmentSource {
                    environment: &own_envs[j],
                    document,
                    path: path_index(&path_field(&job_path, "jobEnvironments"), j),
                    ctx,
                }
            }
        })
        .collect();
    // `certainly_entered`, by index into `sources`.
    let entered_all: Vec<Vec<bool>> = certainly_entered
        .iter()
        .map(|row| {
            env_docs
                .iter()
                .map(|d| row[*d])
                .chain(row[attached.len()..].iter().copied())
                .collect()
        })
        .collect();

    // Job creation checks a Service's Sessions only when its scope is not
    // empty (RFC 0009 `<Service>` item 5, wrap hook rule 1), so here only
    // for one whose scope is known to be: one that an `enableIf` that is
    // not known decides is left to job creation.
    let scopes = crate::template::service_scope::compute_service_scopes_with(
        job_template,
        &crate::template::Entrances::from_known(job_template, &known),
    )
    .ok();
    let known_used = |scope: &job::ServiceScope| !matches!(scope, job::ServiceScope::Steps { steps } if steps.is_empty());
    let entity_used: Vec<bool> = entities
        .iter()
        .map(|entity| {
            entity.kind == tv::RequesterKind::Step
                || scopes
                    .as_ref()
                    .is_some_and(|s| known_used(&s.scope_of(entity.name)))
        })
        .collect();

    // The attached (external) Services. Each selects Environments as a Step
    // does among those of its own document and of the earlier attachments
    // (RFC 0009 "Services run inside Environments"), with no exception for
    // one that lists it. It has only disabled `environment:` entries (§1.2
    // *services* constraint 5), resolved to its own document's Environment
    // if it has that name and otherwise among the earlier attachments'; it
    // requests nothing, and enters every `ALWAYS` one and every `BY_DEFAULT`
    // one it does not decline, with every parameter at its `default`, less
    // any whose own `enableIf` is not known to be `true` and any an earlier
    // one of its group may supersede.
    let mut externals: Vec<ExternalService<'_>> = Vec::new();
    for (d, att) in attached.iter().enumerate() {
        for (k, svc) in att.template.services().iter().enumerate() {
            let who = format!(
                "Service '{}' ({})",
                svc.name,
                job::Document::environment_template(d, att.label)
            );
            let path = path_index(&path_field(&docs.path(Some(d)), "services"), k);
            let deps_path = path_field(&path, "dependencies");
            let mut svc_declined: HashSet<usize> = HashSet::new();
            for (j, dep) in svc.dependencies.iter().flatten().enumerate() {
                let Some(name) = dep.environment() else {
                    continue;
                };
                let entry_path = path_index(&deps_path, j);
                let named = |x: usize| {
                    attached[x]
                        .template
                        .environment()
                        .is_some_and(|env| env.name == name)
                };
                // The document's own Environment if it has the name, and
                // otherwise every earlier attachment's that has it: one
                // whose name two or more define refers to each of them, as
                // for a Step (§3.2.1 item 3).
                let targets: Vec<usize> = if named(d) {
                    vec![d]
                } else {
                    (0..d).filter(|x| named(*x)).collect()
                };
                for target in targets {
                    let de = set.de(target);
                    match de.activation {
                        EnvironmentActivation::OnRequest => {}
                        EnvironmentActivation::ByDefault => {
                            svc_declined.insert(target);
                        }
                        EnvironmentActivation::Always if de.pre_extension => {}
                        // The document's own one was reported at template
                        // validation.
                        EnvironmentActivation::Always if target == d => {}
                        EnvironmentActivation::Always => errors.add(
                            &path_field(&entry_path, "enableIf"),
                            always_entry_disabled_message(
                                tv::RequesterKind::Service,
                                &svc.name,
                                &de.label,
                            ),
                        ),
                    }
                }
            }
            // By attachment, then the group rule over them in order.
            let mut applies = Vec::new();
            let states: Vec<(Option<&str>, Known)> = (0..attached.len())
                .map(|x| {
                    let (Some(de), true) = (set.declaring[x].as_ref(), x <= d) else {
                        return (None, Known::NotEntered);
                    };
                    let entry = if svc_declined.contains(&x) {
                        KnownEntry::Disabled
                    } else {
                        KnownEntry::Absent
                    };
                    if de.activation == EnvironmentActivation::Always
                        || (de.activation == EnvironmentActivation::ByDefault
                            && entry == KnownEntry::Absent)
                    {
                        applies.push(x);
                    }
                    let lists = !de.pre_extension;
                    (
                        de.env.group(),
                        de.env.known_state_in(lists, attached_enabled[x], entry),
                    )
                })
                .collect();
            let states = apply_exclusive_groups(states);
            let entered: Vec<bool> = env_docs
                .iter()
                .map(|x| states[*x] == Known::Entered)
                .chain(std::iter::repeat_n(false, own_envs.len()))
                .collect();
            let own_env = env_docs.iter().position(|x| *x == d).filter(|e| {
                sources[*e]
                    .environment
                    .listed_services()
                    .any(|n| n == svc.name)
            });
            externals.push(ExternalService {
                doc: d,
                svc,
                who,
                path,
                own_env,
                entered,
                applies,
            });
        }
    }
    // RFC 0009 `<StepDependency>` constraint 3 over the combined Job, as
    // job creation's `composition::analyze` builds it, with the edges that
    // are certain without Job Parameter values: every entity's own `step:`
    // and `service:` entries, and the edges from entering an Environment
    // that its Sessions certainly enter (`entered_all`,
    // `ExternalService::entered`). Nodes and edges are in job creation's
    // order, so a cycle of these edges alone is the one job creation
    // reports. It is reported with the errors found so far, and the checks
    // after it are not made.
    if let Some(cycle) = certain_cycle(
        job_template,
        attached,
        &externals,
        &entered_all,
        &sources,
        &env_docs,
        &requirement_bindings,
    ) {
        let path = match cycle.1 {
            Some(d) => path_field(&docs.path(Some(d)), "services"),
            None => job_path.clone(),
        };
        errors.add(&path, cycle.0.to_string());
        return Err(errors.into_result(model).unwrap_err());
    }
    // An external Service's scope is known to be non-empty when an entity
    // known to be in some scope certainly enters its own document's
    // Environment that lists it (scope rules 1-3 through an entering edge),
    // when the `externalServices` requirement bound to it has a non-empty
    // known scope (requirement matching passed above, so exactly one
    // attached Service has the requirement's name), or, propagated, when
    // such an external Service of its document lists it (rule 2).
    let required_scopes = scopes
        .as_ref()
        .map(crate::template::ServiceScopes::required_service_scopes)
        .unwrap_or_default();
    let mut external_used: Vec<bool> = externals
        .iter()
        .map(|x| {
            let via_env = x.own_env.is_some_and(|e| {
                entered_all
                    .iter()
                    .zip(&entity_used)
                    .any(|(entered, used)| *used && entered[e])
            });
            let via_requirement = required_scopes
                .get(x.svc.name.as_str())
                .is_some_and(known_used);
            via_env || via_requirement
        })
        .collect();
    loop {
        let mut changed = false;
        for s in 0..externals.len() {
            if external_used[s] {
                continue;
            }
            let target = &externals[s];
            let reached = externals.iter().zip(&external_used).any(|(from, used)| {
                *used
                    && from.doc == target.doc
                    && crate::template::listed_services(
                        from.svc.dependencies.as_deref(),
                        attached[from.doc].template.services(),
                    )
                    .any(|l| l.name == target.svc.name)
            });
            if reached {
                external_used[s] = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Constraint 7 for each external Service known to be used, over the
    // Environments that apply to it (whatever their own `enableIf` or
    // group), in job creation's form.
    // Per external Service, the Environments whose host requirements job
    // creation combines for it: those it enters, less any for which its
    // values were rejected (job creation has none, and skips them) or are
    // not all literals (only job creation resolves them).
    let mut external_host_entered: Vec<Vec<bool>> =
        externals.iter().map(|x| x.entered.clone()).collect();
    for ((x, used), host_entered) in externals
        .iter()
        .zip(&external_used)
        .zip(&mut external_host_entered)
    {
        if !used {
            continue;
        }
        let mut missing = Vec::new();
        for &doc in &x.applies {
            let de = set.de(doc);
            let e = env_docs
                .iter()
                .position(|d| *d == doc)
                .expect("an applying attachment defines an Environment");
            let (m, uncertain) = de.check_step_statically(
                None,
                x.entered[e],
                ResolvePolicy::new(&set.contexts[doc]),
                &mut errors,
            );
            if uncertain {
                host_entered[e] = false;
            }
            if !m.is_empty() {
                missing.push((de.label.clone(), m));
            }
        }
        report_missing(
            &x.path,
            &missing,
            EVERY_ENVIRONMENT_OF_EXTERNAL_SERVICE,
            "Service",
            &HashSet::new(),
            &mut errors,
        );
    }

    // RFC 0009 wrap hook rule 1 (Environment Template item 4) for each
    // wrapping Environment of the combined Job that a Step or a used
    // Service certainly enters, naming the first of each kind, as job
    // creation does. The Services in job creation's order, the external
    // ones first.
    let service_views: Vec<ServiceView<'_>> = externals
        .iter()
        .zip(&external_used)
        .map(|(x, used)| ServiceView {
            who: &x.who,
            path: &x.path,
            entered: &x.entered,
            used: *used,
            external: true,
        })
        .chain(
            entities[n_steps..]
                .iter()
                .zip(&entered_all[n_steps..])
                .zip(&entity_used[n_steps..])
                .map(|((entity, entered), used)| ServiceView {
                    who: &entity.who,
                    path: &entity.path,
                    entered,
                    used: *used,
                    external: false,
                }),
        )
        .collect();
    for (e, src) in sources.iter().enumerate() {
        let declares_service = src.ctx.profile.has_extension(ModelExtension::Service);
        let task = declares_service
            .then(|| {
                entities[..n_steps]
                    .iter()
                    .zip(&entered_all)
                    .find(|(_, entered)| entered[e])
                    .map(|(entity, _)| entity.who.as_str())
            })
            .flatten();
        let service = service_views
            .iter()
            .find(|v| v.used && v.entered[e])
            .map(|v| v.who);
        let document = docs.name((e < n_attached_envs).then(|| env_docs[e]));
        report_wrap_hooks(
            &src.environment.name,
            src.environment.script.as_ref().map(|s| {
                crate::template::validate_v2023_09::wrap_actions::defined_wrap_hooks(
                    &s.actions.wrap_hooks(),
                )
            }),
            &document,
            &src.path,
            declares_service,
            src.ctx.profile.has_extension(ModelExtension::EnvParams),
            task,
            service,
            &mut errors,
        );
    }
    // The single-layer rule for each used Service whose Sessions enter a
    // wrapping Environment of another document (a Job Template Service's
    // own layers were checked at template validation).
    for view in &service_views {
        if !view.used {
            continue;
        }
        let layers: Vec<(bool, String)> = sources
            .iter()
            .enumerate()
            .filter(|(e, src)| view.entered[*e] && environment_wraps(src.environment))
            .map(|(e, src)| {
                if e < n_attached_envs {
                    (
                        true,
                        format!("'{}' ({})", src.environment.name, src.document),
                    )
                } else {
                    (false, format!("'{}'", src.environment.name))
                }
            })
            .collect();
        if layers.len() > 1 && (view.external || layers.iter().any(|(a, _)| *a)) {
            let layers: Vec<String> = layers.into_iter().map(|(_, l)| l).collect();
            errors.add(
                view.path,
                format!("{}: {}", view.who, wrap_layers_message(&layers, true)),
            );
        }
    }

    // §3.3.3: the literal host requirements of each Step and used Service
    // and the Environments its Sessions certainly enter, combined, in job
    // creation's order (Steps, the external Services, the Job Template's
    // Services). An attachment that declares parameters and that an
    // ambiguous entry of the entity names, or an entry whose `enableIf` is
    // not known (unless its values are certain with the entry's values and
    // with the `default`s alike), or whose values for the entity were
    // rejected (constraint 6 or 7), is left out: job creation may have no
    // values for it and then skips its host requirements, so combining it
    // here could report a conflict job creation does not. So is one whose
    // values for the entity are not all literals: job creation may reject
    // them and skip it too. (An `ALWAYS` attachment that an entry whose
    // `enableIf` is not known names is entered either way, so it is kept
    // when its values are certain both ways; a `BY_DEFAULT` one may be
    // declined, so it is not certainly entered.) An Environment whose own
    // `enableIf` is not known to be `true` is not certainly entered (§4,
    // after item 10; §7.6 rule 5), unless it is `ALWAYS` (§3.2.2 item 6),
    // nor is one an earlier one of its group may supersede.
    let declares = |d: usize| {
        attached[d]
            .template
            .environment()
            .and_then(|env| env.env_parameter_definitions.as_ref())
            .is_some_and(|p| !p.is_empty())
    };
    let entered: Vec<Vec<bool>> = entered_all
        .iter()
        .zip(&ambiguous)
        .zip(&uncertain_docs)
        .map(|((all, ambiguous), uncertain)| {
            all.iter()
                .enumerate()
                .map(|(e, x)| {
                    *x && !(e < n_attached_envs
                        && ((ambiguous.contains(&env_docs[e]) && declares(env_docs[e]))
                            || uncertain.contains(&env_docs[e])))
                })
                .collect()
        })
        .collect();
    let entity_target = |i: usize| super::host_requirements::LiteralTarget {
        who: &entities[i].who,
        path: &entities[i].path,
        own: entities[i].host_requirements,
        entered: &entered[i],
    };
    let targets: Vec<super::host_requirements::LiteralTarget<'_>> = (0..n_steps)
        .map(entity_target)
        .chain(
            externals
                .iter()
                .zip(&external_used)
                .zip(&external_host_entered)
                .filter(|((_, used), _)| **used)
                .map(
                    |((x, _), entered)| super::host_requirements::LiteralTarget {
                        who: &x.who,
                        path: &x.path,
                        own: x.svc.host_requirements.as_ref(),
                        entered,
                    },
                ),
        )
        .chain(
            (n_steps..entities.len())
                .filter(|i| entity_used[*i])
                .map(entity_target),
        )
        .collect();
    super::host_requirements::check_literal_combinations(
        &targets,
        &sources,
        &job_ctx,
        super::EvalBudgets::from_ctx(ctx),
        &mut errors,
    );
    errors.into_result(model)?;
    Ok(warnings)
}

/// The first cycle of the combined dependency graph (RFC 0009
/// `<StepDependency>` constraint 3) over the edges that are certain
/// without Job Parameter values, for [`check_environment_templates`]. The
/// nodes are the Job Template's Steps, then the Services in job creation's
/// order (`externals`, then the Job Template's own), labeled and connected
/// as [`composition::analyze`] does: each entity's `step:` and `service:`
/// entries whose `enableIf` is absent or `true`, then, node by node, an
/// edge to each Service listed by each Environment of `sources` its
/// Sessions certainly enter (`entered_all` for the Job Template's Steps and
/// Services, index for index; `ExternalService::entered` for the external
/// ones). A `service:` entry of the Job Template that names no Service of
/// its own names the attached Service its requirement is bound to. Returns
/// the cycle with the attachment every node of it is a Service of, when
/// there is one.
fn certain_cycle(
    job_template: &JobTemplate,
    attached: &[AttachedEnvironmentTemplate<'_>],
    externals: &[ExternalService<'_>],
    entered_all: &[Vec<bool>],
    sources: &[super::host_requirements::EnvironmentSource<'_>],
    env_docs: &[usize],
    bindings: &[RequirementBinding],
) -> Option<(crate::template::ServiceDependencyCycle, Option<usize>)> {
    use crate::template::service_scope::DependencyGraph;
    let steps = &job_template.steps;
    let own = job_template.services();
    let n_steps = steps.len();
    let n_ext = externals.len();
    let document = |d: usize| job::Document::environment_template(d, attached[d].label);
    // The node a `service: <name>` written in document `doc` (`None` for
    // the Job Template) names.
    let resolve = |doc: Option<usize>, name: &str| -> Option<usize> {
        let external = |d: usize, name: &str| {
            externals
                .iter()
                .position(|x| x.doc == d && x.svc.name == name)
                .map(|k| n_steps + k)
        };
        match doc {
            Some(d) => external(d, name),
            None => own
                .iter()
                .position(|s| s.name == name)
                .map(|k| n_steps + n_ext + k)
                .or_else(|| {
                    let b = bindings.iter().find(|b| b.requirement == name)?;
                    (0..attached.len())
                        .find(|d| document(*d) == b.document)
                        .and_then(|d| external(d, &b.service))
                }),
        }
    };
    let mut graph = DependencyGraph::default();
    let mut step_index: HashMap<&str, usize> = HashMap::new();
    for step in steps {
        let i = graph.node(format!("Step {}", step.name));
        step_index.entry(step.name.as_str()).or_insert(i);
    }
    for x in externals {
        graph.node(format!("Service {} ({})", x.svc.name, document(x.doc)));
    }
    for svc in own {
        graph.node(format!("Service {}", svc.name));
    }
    let certain = |d: &&crate::template::StepDependency| d.literal_enabled() == Some(true);
    for (i, step) in steps.iter().enumerate() {
        for dep in step.dependencies.iter().flatten().filter(certain) {
            let to = match (dep.step(), dep.service()) {
                (Some(name), _) => step_index.get(name).copied(),
                (_, Some(name)) => resolve(None, name),
                _ => None,
            };
            if let Some(to) = to.filter(|to| *to != i) {
                graph.edge(i, to, None);
            }
        }
    }
    let services = externals
        .iter()
        .map(|x| (Some(x.doc), x.svc))
        .chain(own.iter().map(|svc| (None, svc)));
    for (k, (doc, svc)) in services.enumerate() {
        let i = n_steps + k;
        for dep in svc.dependencies.iter().flatten().filter(certain) {
            let to = match (dep.step(), dep.service()) {
                (Some(name), _) if doc.is_none() => step_index.get(name).copied(),
                (_, Some(name)) => resolve(doc, name),
                _ => None,
            };
            if let Some(to) = to.filter(|to| *to != i) {
                graph.edge(i, to, None);
            }
        }
    }
    let n_attached_envs = env_docs.len();
    let env_lists: Vec<Vec<usize>> = sources
        .iter()
        .enumerate()
        .map(|(e, src)| {
            let doc = (e < n_attached_envs).then(|| env_docs[e]);
            let mut out = Vec::new();
            for name in src.environment.listed_services() {
                if let Some(k) = resolve(doc, name) {
                    if !out.contains(&k) {
                        out.push(k);
                    }
                }
            }
            out
        })
        .collect();
    let env_labels: Vec<String> = sources
        .iter()
        .enumerate()
        .map(|(e, src)| {
            if e < n_attached_envs {
                format!(
                    "Job Environment '{}' ({})",
                    src.environment.name, src.document
                )
            } else {
                format!("Job Environment '{}'", src.environment.name)
            }
        })
        .collect();
    let rows = entered_all[..n_steps]
        .iter()
        .chain(externals.iter().map(|x| &x.entered))
        .chain(&entered_all[n_steps..]);
    for (i, row) in rows.enumerate() {
        for (e, enters) in row.iter().enumerate() {
            if !enters {
                continue;
            }
            for &k in &env_lists[e] {
                graph.edge(i, k, Some(env_labels[e].clone()));
            }
        }
    }
    let cycle = graph.find_cycle()?;
    // The attachment every node of the cycle is a Service of, when there is
    // one: as in `composition::analyze`, the cycle is then that document's.
    let node_doc = |label: &String| -> Option<usize> {
        let i = graph.labels.iter().position(|l| l == label)?;
        (n_steps..n_steps + n_ext)
            .contains(&i)
            .then(|| externals[i - n_steps].doc)
    };
    let first = cycle.path.first().and_then(node_doc);
    let doc = first.filter(|d| cycle.path.iter().all(|l| node_doc(l) == Some(*d)));
    Some((cycle, doc))
}

#[cfg(test)]
mod tests {
    //! The full decode + create + apply pipeline is exercised in
    //! `tests/integration/test_service_external.rs`; these unit tests pin
    //! the helpers that build document paths and relabel errors.

    use super::*;

    fn env_template(yaml: &str) -> EnvironmentTemplate {
        let value: serde_json::Value = serde_saphyr::from_str(yaml).unwrap();
        crate::decode_environment_template(
            value,
            Some(&["SERVICE", "EXPR"]),
            &crate::types::CallerLimits::default(),
        )
        .unwrap()
    }

    const PLAIN: &str = r#"
specificationVersion: "environment-2023-09"
environment:
  name: E
  variables: { X: "1" }
"#;

    #[test]
    fn document_paths_and_names() {
        let a = env_template(PLAIN);
        let b = env_template(PLAIN);
        let attached = [
            AttachedEnvironmentTemplate::new(&a),
            AttachedEnvironmentTemplate::new(&b).with_label("queue/b.yaml"),
        ];
        let docs = Documents {
            attached: &attached,
        };
        assert_eq!(path_to_string(&docs.path(None)), "JobTemplate");
        assert_eq!(
            path_to_string(&docs.path(Some(0))),
            "EnvironmentTemplate[0]"
        );
        assert_eq!(path_to_string(&docs.path(Some(1))), "queue/b.yaml");
        assert_eq!(docs.name(None), "the Job Template");
        assert_eq!(docs.name(Some(0)), "EnvironmentTemplate[0]");
        assert_eq!(docs.name(Some(1)), "queue/b.yaml");
    }

    #[test]
    fn in_document_prefixes_paths_and_messages() {
        let doc = vec![
            PathElement::Field("EnvironmentTemplate".to_string()),
            PathElement::Index(2),
        ];

        let mut ve = ValidationErrors::default();
        ve.add(&path_field(&[], "services"), "boom");
        let err = in_document(ve.into_result("JobTemplate").unwrap_err(), &doc);
        assert_eq!(
            err.to_string(),
            "Model validation error: 1 validation error for Submission\n\
             EnvironmentTemplate[2] -> services:\n\tboom"
        );

        let err = in_document(
            ModelError::FormatStringError {
                message: "bad".to_string(),
                input: None,
                start: None,
                end: None,
                source: None,
            },
            &doc,
        );
        assert_eq!(
            err.to_string(),
            "Format string error: EnvironmentTemplate[2]: bad"
        );

        let err = in_document(
            ModelError::Expression(openjd_expr::ExpressionError::new("oops")),
            &doc,
        );
        assert_eq!(
            err.to_string(),
            "Expression error: EnvironmentTemplate[2]: oops"
        );

        // Other variants pass through untouched.
        let err = in_document(ModelError::Compatibility("c".to_string()), &doc);
        assert_eq!(err.to_string(), "Compatibility error: c");
    }

    #[test]
    fn attached_environment_template_from_ref_has_no_label() {
        let t = env_template(PLAIN);
        let att: AttachedEnvironmentTemplate<'_> = (&t).into();
        assert!(att.label.is_none());
        assert!(std::ptr::eq(att.template, &t));
    }
}
