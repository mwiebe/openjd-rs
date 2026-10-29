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
//! creation proper computes effective values for the Job Template's own
//! `jobEnvironments` and keeps each enabled `environment:` entry that names
//! no Job Environment as a [`FrozenEntry`]. [`apply_environment_templates`]
//! then resolves those entries against the attached Environment Templates
//! by name, fixes the part of each Step's Environment set that the
//! attachments contribute (§4 item 4 `activation`), computes every Step's
//! effective values for each attached Environment it would enter, decides
//! each attachment's own `enableIf` (§4 item 6) per Step with the values
//! that are known, checks constraint 5 for every attachment and
//! constraints 6 and 7 for those it does not disable (§3.2.2), checks the
//! `WRAP_ACTIONS` single-layer rule over each Step's final set, and builds
//! the combined Job. Job creation proper decides the Job Template's own Job
//! Environments the same way.
//!
//! [`check_environment_templates`] runs the checks that need no Job
//! Parameter values, before any are known.

use std::collections::{BTreeMap, HashMap, HashSet};

use openjd_expr::ExprValue;

use crate::error::{
    path_field, path_index, path_to_string, ModelError, PathElement, ValidationErrors,
};
use crate::job;
use crate::template::{EnvironmentTemplate, JobTemplate};
use crate::types::{JobParameterValues, ModelExtension, ValidationContext};

use super::build_symbol_table;
use super::env_params::{
    report_missing, DeclaringEnvironment, EnableIf, FrozenEntry, ResolvePolicy, StepSelection,
    Supplied,
};
use crate::template::validate_v2023_09::env_params::{self as tv, capped_name_list};
use crate::template::EnvironmentActivation;

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
    /// Where the condition is, starting with the document (`JobTemplate`,
    /// `EnvironmentTemplate[i]`, or the attachment's label).
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
    /// own, which is the order a Session enters them. Each Step's
    /// `environment_parameters` and `environments` cover both.
    pub job: job::Job,
    /// The document of each entry of `job.job_environments`, index for
    /// index: [`job::Document::EnvironmentTemplate`] for the attached ones,
    /// then [`job::Document::JobTemplate`].
    pub environment_documents: Vec<job::Document>,
    /// Conditions to warn about, in the order found: an attached
    /// Environment named like a Job Environment, which no entry of the Job
    /// Template can reach (§3.2.1 item 4), when the Job Template lists
    /// `ENV_PARAMS` (without it no entry exists, and the submission
    /// behaves as before the extension); and an enabled `required: false`
    /// entry that resolves to no Environment (RFC "External Environments
    /// are named implicitly" item 3).
    pub warnings: Vec<SubmissionWarning>,
    /// For each Step of `job`, index for index, the Environments that its
    /// `enableIf` (Template Schemas §4 item 6) disabled for that Step:
    /// ones its Sessions would otherwise have entered, and which `job`
    /// therefore leaves out of the Step's
    /// [`environments`](job::Step::environments) and
    /// [`step_environments`](job::Step::step_environments). For display;
    /// empty for a Step with none.
    pub disabled_environments: Vec<StepDisabledEnvironments>,
}

/// The Environments an `enableIf` (Template Schemas §4 item 6) disabled
/// for one Step of a [`CreatedJob`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepDisabledEnvironments {
    /// Job Environments of the combined Job (attached ones first, then the
    /// Job Template's), in that order, by `(document, environment)`.
    pub environments: Vec<job::EnvironmentRef>,
    /// The names of the Step's own `stepEnvironments` entries, in written
    /// order.
    pub step_environments: Vec<String>,
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
    /// caller's limits and submission path policy.
    contexts: Vec<ValidationContext>,
    declaring: Vec<DeclaringEnvironment<'a>>,
    by_name: HashMap<&'a str, Vec<usize>>,
    /// Attachments no entry can reach: named like a Job Environment
    /// (§3.2.1 item 4) or like another attachment (item 3). The missing
    /// parameters of one with `activation: ALWAYS` are reported once, at the
    /// attachment, not per Step; one with `ON_REQUEST` is never
    /// entered.
    unreachable: HashSet<usize>,
    /// The attachment-level errors for an unreachable attachment with
    /// `activation: ALWAYS` and parameters without a `default`, by
    /// attachment, path, and message. They are reported by
    /// [`report_unreachable`](Self::report_unreachable) only for an
    /// attachment that its own `enableIf` does not disable (§3.2.2): a
    /// disabled one is not entered, so its parameters need no value.
    unreachable_required: Vec<(usize, Vec<PathElement>, String)>,
}

/// How an unreachable attachment's error says an `ALWAYS` attachment with
/// an `enableIf` is entered.
const ENTERED_ENABLE_IF: &str =
    "Its activation is ALWAYS and its enableIf does not disable it, so Sessions enter it";
/// The same, for an attachment whose `enableIf` could not be evaluated for
/// any Step that decided it.
const ENTERED_ENABLE_IF_FAILED: &str = "Its activation is ALWAYS and its enableIf could not be \
     evaluated, so it is checked as if Sessions enter it";

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
    /// (§4.5 point 1).
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
        // How the messages below say the attachment is entered.
        let entered = |env: &crate::template::Environment| {
            if env.enable_if.is_some() {
                ENTERED_ENABLE_IF
            } else {
                "Its activation is ALWAYS, so every Session enters it"
            }
        };
        for (doc, att) in attached.iter().enumerate() {
            let env = att.template.environment();
            let att_ctx = att
                .template
                .default_validation_context()
                .with_caller_limits(ctx.caller_limits.clone())
                .with_submission_path_format(ctx.submission_path_format)
                .with_allow_uri_path_values(ctx.allow_uri_path_values);
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
                if !required.is_empty() && env.activation() == EnvironmentActivation::Always {
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
                            entered(env)
                        ),
                    ));
                }
                // Without ENV_PARAMS in the Job Template there are no
                // entries to reach any attachment, and such a submission
                // behaves as before the extension (RFC Backward
                // compatibility), with no new warning.
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
                     no 'environment:' dependency can reach it (Template Schemas §3.2.1 item \
                     3), and it has parameters without a default that no Step can supply: {}. \
                     {}, and the submission is rejected (§3.2.1).",
                    env.name,
                    others.len(),
                    capped_name_list(others.iter().map(|d| doc_names[*d].as_str()), others.len()),
                    capped_name_list(required.iter().copied(), required.len()),
                    entered(env)
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
    /// with `activation: ALWAYS` and parameters without a `default` (see
    /// [`unreachable_required`](Self::unreachable_required)) for which
    /// `not_disabled(doc)` holds: a Step's Sessions enter it, as its own
    /// `enableIf` does not disable it for every Step.
    fn report_unreachable(
        &self,
        errors: &mut ValidationErrors,
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
                errors.add(path, message);
            }
        }
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
    /// `activation` is `ALWAYS`; otherwise pushed to `missing` for the Step's one
    /// constraint 7 error.
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
                     §3.2.1 item 5), and its activation is ALWAYS, so every Session enters it.",
                    de.label,
                    capped_name_list(names.iter().copied(), names.len())
                ),
            );
        } else {
            missing.push((de.label.clone(), names));
        }
    }
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
const EVERY_ATTACHED: &str =
    "every attached Environment with activation ALWAYS, and each one with \
     activation ON_REQUEST that the Step requests; Template Schemas §3.2.1";

/// Attach the Environment Templates of a submission to `job`, the Job that
/// job creation built from the Job Template alone, and return the combined
/// Job (Template Schemas §3.2.1, §3.2.2, §4 items 4 and 5, §3.3.3).
///
/// `selections` holds, Step by Step, what job creation decided:
/// the enabled entries that name no Job Environment, frozen, and which
/// `jobEnvironments` entries the Step's Sessions enter. `env_names` is the
/// Job's `Job.EnvNames`, bound for attached Environments whose document
/// lists `EXPR`. `attached` is in the scheduler's order.
/// `job_parameter_values` are the merged values. `ctx` supplies the caller
/// limits and the submission path policy, applied to every document; its
/// profile is not read. Each attached template is evaluated under its own
/// profile ([`EnvironmentTemplate::profile`]): an extension applies to the
/// document that lists it.
///
/// Each attachment named like a `jobEnvironments` entry is unreachable from
/// the Job Template (§3.2.1 item 4). It gets a warning when the Job
/// Template lists `ENV_PARAMS` (without it the Job Template has no entries
/// that could name it, and the submission behaves as before the
/// extension), and an error when its `activation` is `ALWAYS` and it has
/// parameters without a `default`.
///
/// For each Step's frozen entry, in Step order and then entry order:
///
/// 1. exactly one attached Environment with the entry's name: the entry
///    refers to it, whatever `required` says;
/// 2. none: with `required: true` (or `required` absent) the submission is
///    rejected, naming the entry and the Environment; with
///    `required: false` the entry is ignored and a warning is recorded;
/// 3. two or more: the submission is rejected, naming the entry and the
///    attachments.
///
/// A Step's Sessions enter every attached Environment with `activation: ALWAYS`
/// and each one with `activation: ON_REQUEST` that one of its entries
/// refers to. For each one it enters that declares parameters, the Step
/// gets effective values from its entry if one refers to it and from the
/// `default`s otherwise (except an attachment that one of the Step's
/// rejected entries names, an ambiguous name, which is skipped for that
/// Step so the one mistake is reported once). The attachment's own
/// `enableIf` (§4 item 6) is then decided for the Step with the values that
/// are known (§3.2.2), and one it disables is left out of the Step's set
/// and values. Constraint 5 (keys) is checked for every attachment the
/// Step would enter; the value checks
/// (constraint 6) and constraint 7 (every parameter without a `default`
/// supplied) only for one its `enableIf` does not disable. A literal
/// `default` of an attached Environment is checked even if no Step takes it
/// (§4.5 point 1). The Job Template's own Job Environments were decided the
/// same way by job creation (`selections`). Then the `WRAP_ACTIONS`
/// single-layer rule is checked over each Step's
/// final Environment set, attached Environments included (RFC 0008; plan
/// R8). Last, each Step gets its
/// combined host requirements (§3.3.3): every Environment's
/// `hostRequirements` resolved with the Step's effective values for each
/// Environment it enters, combined with the Step's own (see
/// `host_requirements::combine_step_host_requirements`).
///
/// Every violation is reported in one `ModelValidation` error for
/// `Submission`, in this order: the attachments' literal `default`s, then
/// Step by Step the entries' resolution, then each Step's values, then
/// the attachments' failed `enableIf`s, then the unreachable attachments'
/// parameters without a `default`, then each Step's wrap layers, then the
/// Environments' resolved host requirements and each Step's combination
/// conflicts. Each path starts with the document
/// (`JobTemplate`, `EnvironmentTemplate[i]`, or the attachment's label).
///
/// The function does not re-run the carried-forward resolved-value checks
/// on the attached Environments' own format strings, which job creation
/// runs on the Job Template's.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_environment_templates(
    job_template: &JobTemplate,
    mut job: job::Job,
    selections: Vec<StepSelection>,
    env_names: &[String],
    attached: &[AttachedEnvironmentTemplate<'_>],
    job_parameter_values: &JobParameterValues,
    ctx: &ValidationContext,
) -> Result<CreatedJob, ModelError> {
    let job_path = Documents { attached }.path(None);
    let job_env_names: HashSet<&str> = job
        .job_environments
        .iter()
        .flatten()
        .map(|e| e.name.as_str())
        .collect();

    let mut errors = ValidationErrors::default();
    let mut warnings = Vec::new();
    // `default`s resolve against the merged Job Parameters, without
    // `Job.Name` or `Job.EnvNames` (§4.5 item 4; plan A21).
    let default_symtab = build_symbol_table(job_parameter_values)?;
    let job_lists_env_params = job
        .extensions
        .as_ref()
        .is_some_and(|exts| exts.contains(&ModelExtension::EnvParams));
    let set = Attachments::new(
        attached,
        &job_env_names,
        job_lists_env_params,
        &default_symtab,
        ctx,
        &mut errors,
        &mut warnings,
    );

    // ── Resolve each Step's frozen entries ──
    // Per Step: the attachment each resolved entry refers to, with the
    // entry.
    let mut resolved: Vec<Vec<(usize, &FrozenEntry, Vec<PathElement>)>> =
        Vec::with_capacity(selections.len());
    // Per Step: the attachments an entry named but could not be applied to
    // (an ambiguous name), already reported. Their values are not computed
    // for that Step, so the rejected entry does not also produce "must
    // supply" or "does not supply" errors that contradict what the entry
    // wrote.
    let mut unapplied: Vec<HashSet<usize>> = Vec::with_capacity(selections.len());
    for (i, selection) in selections.iter().enumerate() {
        let deps_path = path_field(
            &path_index(&path_field(&job_path, "steps"), i),
            "dependencies",
        );
        let mut step_resolved = Vec::new();
        let mut step_unapplied = HashSet::new();
        for entry in &selection.frozen {
            let entry_path = path_index(&deps_path, entry.dependency_index);
            match set.match_entry(
                &entry.environment,
                entry.required,
                path_field(&entry_path, "environment"),
                &mut errors,
                &mut warnings,
            ) {
                EntryMatch::Absent => {}
                EntryMatch::One(doc) => step_resolved.push((doc, entry, entry_path)),
                EntryMatch::Many(many) => step_unapplied.extend(many.iter().copied()),
            }
        }
        resolved.push(step_resolved);
        unapplied.push(step_unapplied);
    }

    // ── Each Step's attached Environments, and its values for them ──
    // §3.2.2: the Step's effective values for each attached Environment its
    // Sessions would enter, then the Environment's own `enableIf` (§4 item
    // 6) with the values that are known, then, for one it does not
    // disable, the value checks and constraint 7 (§7.4).
    // The table each attachment's `enableIf` resolves in, before its
    // `Env.*` symbols: the Job Parameters, `Job.Name`, and `Job.EnvNames`
    // (§4 item 6 constraint 1). Only an attachment that lists `ENV_PARAMS`,
    // and so `EXPR`, has an `enableIf`, so one table serves every
    // attachment.
    let mut base = default_symtab.clone();
    base.set("Job.Name", ExprValue::String(job.name.clone()))?;
    base.set("Job.EnvNames", env_names_value(env_names))?;
    let mut attached_entered: Vec<Vec<bool>> = Vec::with_capacity(job.steps.len());
    // Per attachment: whether its `enableIf` did not disable it for some
    // Step.
    let mut entered_by_some_step = vec![false; set.declaring.len()];
    // Whether some Step's decision was other than a failed `enableIf`.
    let mut decided_by_some_step = vec![false; set.declaring.len()];
    let mut attached_disabled: Vec<Vec<usize>> = Vec::with_capacity(job.steps.len());
    let mut step_environment_parameters = Vec::with_capacity(job.steps.len());
    for (i, step) in job.steps.iter().enumerate() {
        let step_path = path_index(&path_field(&job_path, "steps"), i);
        let mut entered = Vec::with_capacity(set.declaring.len());
        let mut disabled_docs = Vec::new();
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
        for (doc, (de, att_ctx)) in set.declaring.iter().zip(&set.contexts).enumerate() {
            let entry = resolved
                .get(i)
                .and_then(|r| r.iter().find(|(d, _, _)| *d == doc));
            // §4 item 4: every `ALWAYS` attachment, and each `ON_REQUEST`
            // one an entry of the Step refers to.
            let enters = de.activation == EnvironmentActivation::Always || entry.is_some();
            if !enters || unapplied.get(i).is_some_and(|u| u.contains(&doc)) {
                entered.push(enters);
                continue;
            }
            let supplied = entry.map(|(_, e, path)| Supplied {
                params: &e.parameters,
                symtab: &e.symtab,
                entry_path: path.clone(),
            });
            let policy = ResolvePolicy::new(att_ctx);
            let pending =
                de.values_for_step(supplied.as_ref(), &default_symtab, policy, &mut errors);
            let decision = de.decide(&pending, &step.name, &base, policy);
            // A failed `enableIf` is checked as if it does not disable the
            // Environment, so the Step's other mistakes are reported too.
            let not_disabled = decision != EnableIf::Disabled;
            entered_by_some_step[doc] |= not_disabled;
            decided_by_some_step[doc] |= decision != EnableIf::Failed;
            entered.push(not_disabled);
            match decision {
                EnableIf::Disabled => disabled_docs.push(doc),
                EnableIf::Enabled | EnableIf::Unresolved | EnableIf::Failed => {
                    let (v, m) = de.commit(pending, &step.name, &mut errors);
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
                    set.sort_missing(
                        doc,
                        m,
                        &step_env_names,
                        &step_path,
                        &mut missing,
                        &mut errors,
                    );
                }
            }
        }
        let disabled = selections
            .get(i)
            .map(|sel| set.disabled_labels(sel.disabled.iter().map(String::as_str)))
            .unwrap_or_default();
        report_missing(
            &step_path,
            &missing,
            EVERY_ATTACHED,
            &disabled,
            &unresolved,
            &failed,
            &mut errors,
        );
        attached_entered.push(entered);
        attached_disabled.push(disabled_docs);
        step_environment_parameters.push(values);
    }
    // A failed `enableIf` is reported once for all the Steps it was
    // resolved for.
    for de in &set.declaring {
        de.report_enable_if_errors(&mut errors);
    }
    // An unreachable attachment with parameters no Step can supply is an
    // error when some Step's Sessions enter it; one whose own `enableIf`
    // disables it for every Step is not entered (§3.2.2). One whose
    // `enableIf` is absent or `true` is entered by every Step that decides
    // it, and counts as entered also when no Step does (every Step names it
    // by an ambiguous entry, which is reported). One whose `enableIf` failed
    // for a Step is checked as if that Step enters it.
    set.report_unreachable(
        &mut errors,
        |doc| entered_by_some_step[doc] || set.declaring[doc].certainly_enabled,
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
    let mut entered_per_step = Vec::with_capacity(job.steps.len());
    for ((step, selection), attached_entered) in
        job.steps.iter_mut().zip(&selections).zip(&attached_entered)
    {
        let mut values = per_step.next().unwrap_or_default();
        values.extend(step.environment_parameters.take().into_iter().flatten());
        step.environment_parameters = (!values.is_empty()).then_some(values);
        let entered: Vec<bool> = attached_entered
            .iter()
            .chain(&selection.job_environments_entered)
            .copied()
            .collect();
        entered_per_step.push(entered);
    }
    // Each Step's disabled Environments, by index into `environments`:
    // the attached ones, then the Job Template's.
    let disabled: Vec<Vec<usize>> = attached_disabled
        .into_iter()
        .zip(&selections)
        .map(|(mut docs, selection)| {
            docs.extend(
                selection
                    .disabled_job_environments
                    .iter()
                    .map(|k| attached.len() + k),
            );
            docs
        })
        .collect();
    let mut disabled_environments = Vec::with_capacity(job.steps.len());
    for (((step, selection), entered), disabled) in job
        .steps
        .iter_mut()
        .zip(&selections)
        .zip(&entered_per_step)
        .zip(disabled)
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
        disabled_environments.push(StepDisabledEnvironments {
            environments: disabled
                .into_iter()
                .map(|e| job::EnvironmentRef {
                    document: environment_documents[e].clone(),
                    environment: environments[e].name.clone(),
                })
                .collect(),
            step_environments: selection.disabled_step_environments.clone(),
        });
    }
    check_wrap_layers(
        &job,
        &environments,
        &environment_documents,
        &job_path,
        &mut errors,
    );
    // ── §3.3.3: each Step's combined host requirements ──
    // Last in §7.4: they are resolved with the Step's effective values, and
    // only for the Environments the Step's Sessions enter.
    super::host_requirements::combine_step_host_requirements(
        &mut job,
        &sources,
        &entered_per_step,
        &job_path,
        &default_symtab,
        &env_names_value(env_names),
        super::EvalBudgets::from_ctx(ctx),
        &mut errors,
    );
    errors.into_result(SUBMISSION)?;

    job.job_environments = (!environments.is_empty()).then_some(environments);
    Ok(CreatedJob {
        job,
        environment_documents,
        warnings,
        disabled_environments,
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
/// after every `enableIf`. Template validation checks the Job Template's
/// own layers that are certainly entered (entries and Environments whose
/// `enableIf` is absent or a boolean scalar); this check also covers an
/// `enableIf` expression and the attached Environments (plan R8).
fn check_wrap_layers(
    job: &job::Job,
    environments: &[job::Environment],
    documents: &[job::Document],
    job_path: &[PathElement],
    errors: &mut ValidationErrors,
) {
    let wraps = |env: &job::Environment| {
        env.script
            .as_ref()
            .is_some_and(|s| s.actions.has_any_wrap_hook())
    };
    for (i, step) in job.steps.iter().enumerate() {
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
            errors.add(
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
/// ALWAYS` and the `ON_REQUEST` ones an entry resolved to (`resolved`),
/// then the Job Environments with `ALWAYS` and the `ON_REQUEST` ones an
/// entry whose `enableIf` is absent or the scalar `true` requests, then its
/// Step Environments; each only when its own `enableIf` is absent or the
/// scalar `true` (§4 item 6 tail). An entry or Environment whose `enableIf`
/// is an expression (even a constant one) is decided at job creation, which
/// checks the rule again over the full set; one whose `enableIf` is the
/// scalar `false` is not entered. A Step none of whose layers is
/// attached is left to template validation, which checks the Job
/// Template's own layers.
fn check_attached_wrap_layers(
    job_template: &JobTemplate,
    attached: &[AttachedEnvironmentTemplate<'_>],
    doc_names: &[String],
    resolved: &[Vec<usize>],
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
    if wrapping_attached.is_empty() {
        return;
    }
    for (i, step) in job_template.steps.iter().enumerate() {
        let mut layers: Vec<String> = wrapping_attached
            .iter()
            .filter(|d| {
                let env = attached[**d].template.environment();
                env.literal_enabled() == Some(true)
                    && (env.activation() == EnvironmentActivation::Always
                        || resolved[i].contains(d))
            })
            .map(|d| {
                format!(
                    "'{}' ({})",
                    attached[*d].template.environment().name,
                    doc_names[*d]
                )
            })
            .collect();
        if layers.is_empty() {
            continue;
        }
        let requested: HashSet<&str> = step
            .dependencies
            .iter()
            .flatten()
            .filter(|d| d.literal_enabled() == Some(true))
            .filter_map(|d| d.environment())
            .collect();
        layers.extend(
            job_template
                .job_environments
                .iter()
                .flatten()
                .filter(|env| {
                    wraps(env)
                        && env.literal_enabled() == Some(true)
                        && (env.activation() == EnvironmentActivation::Always
                            || requested.contains(env.name.as_str()))
                })
                .map(|env| format!("'{}'", env.name)),
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
///   Environment (a warning when the Job Template lists `ENV_PARAMS`, and
///   an error when its `activation` is `ALWAYS`, it has parameters without a
///   `default`, and its own `enableIf` is absent, `true`, or certainly
///   unresolved), an ambiguous
///   name with such an attachment, and every literal `default` (§4.5 point
///   1);
/// - each `environment:` entry that names no Job Environment and whose
///   `enableIf` is absent or the boolean scalar `true`, resolved by name
///   (§3.2.1 items 1–3), with a `required: false` entry that resolves to
///   nothing reported as a warning. An entry whose `enableIf` is `false` is
///   as if absent; one whose `enableIf` is an expression (an expression
///   field, §7.5, so even a constant one such as `"true"`) is checked only
///   when the Job is created, and so is every attachment it may name, for
///   its Step;
/// - for an entry that resolves, constraint 5 (keys); and, when the
///   attachment's own `enableIf` is absent or `true`, or certainly
///   unresolved for the Step (it reads only values that stay unknown; see
///   `enable_if_certainly_unresolved`), constraint 6 for literal values
///   (including PATH absoluteness in
///   [`ValidationContext::submission_path_format`]) and the
///   single-expression rule for a list-typed parameter;
/// - constraint 7 over every Step and every reachable attached
///   Environment it enters whose own `enableIf` is absent or `true`, or
///   certainly unresolved (with the unresolved note): those
///   with `activation: ALWAYS`, and each one with `activation: ON_REQUEST`
///   that one of its entries refers to. When a Step's entry for an
///   Environment with missing parameters was disabled by a scalar
///   `enableIf: false`, the message says so. An attachment whose
///   `enableIf` is `false` is disabled, so neither its values nor
///   constraint 7 apply (Template Schemas §3.2.2); one whose `enableIf` is
///   an expression is undetermined, so a violation is not certain here
///   (§7.5 rule 5) and job creation makes those checks;
/// - the `WRAP_ACTIONS` single-layer rule (RFC 0008) for each Step whose
///   Sessions enter a wrapping attached Environment, over the wrapping
///   Environments its Sessions certainly enter: attachments with `activation:
///   ALWAYS` and the `ON_REQUEST` ones an entry resolved to, Job
///   Environments with `ALWAYS` and the `ON_REQUEST` ones an entry whose
///   `enableIf` is absent or the scalar `true` requests, and its Step
///   Environments, each only when its own `enableIf` is absent or the
///   scalar `true`, in job creation's form (job creation may list more
///   layers, when an `enableIf` expression on an entry or an Environment
///   brings one in);
/// - the Steps' combined host requirements (§3.3.3) over the requirements
///   that are literals (no format-string expression): each Step's own and
///   those of the Environments its Sessions certainly enter, as for the
///   single-layer rule (so not one whose own `enableIf` is the scalar
///   `false` or an expression). A conflict among them is a conflict of the full
///   combination, reported in job creation's form; when a non-literal
///   requirement of the same capability also takes part, job creation may
///   name other sources or bounds.
///
/// Format-string values, non-literal `default`s, and non-literal
/// `hostRequirements` are not resolved: they need Job Parameter values, so
/// they are checked when the Job is created. Entries that name a
/// `jobEnvironments` entry were
/// checked by template validation and are not checked again.
///
/// `ctx` supplies the caller limits and the submission path policy, as for
/// job creation; its profile is not read.
///
/// Returns the warnings, or one `ModelValidation` error for `Submission`
/// holding every violation, in the order job creation reports them.
pub fn check_environment_templates(
    job_template: &JobTemplate,
    attached: &[AttachedEnvironmentTemplate<'_>],
    ctx: &ValidationContext,
) -> Result<Vec<SubmissionWarning>, ModelError> {
    let job_path = Documents { attached }.path(None);
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
    // An unreachable attachment's parameters are needed only where it is
    // not disabled, which is known here only for an `enableIf` that is
    // absent or `true`, or certainly unresolved with no values supplied
    // (§3.2.2, §7.5 rule 5).
    set.report_unreachable(
        &mut errors,
        |doc| {
            let de = &set.declaring[doc];
            de.certainly_enabled
                || tv::enable_if_certainly_unresolved(de.env, None, &set.contexts[doc])
        },
        // No `enableIf` is evaluated here, so none has failed.
        |_| false,
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
    for (i, step) in job_template.steps.iter().enumerate() {
        let deps_path = path_field(
            &path_index(&path_field(&job_path, "steps"), i),
            "dependencies",
        );
        let mut step_resolved = Vec::new();
        let mut step_unapplied = HashSet::new();
        let mut step_ambiguous = HashSet::new();
        let mut step_disabled = Vec::new();
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
            match dep.literal_enabled() {
                Some(true) => {}
                // A disabled entry is as if absent (§3.2 item 2).
                Some(false) => {
                    step_disabled.push(name);
                    continue;
                }
                // Whether the entry is enabled is known only when the Job
                // is created, so neither its resolution nor the
                // Environment it may name is checked for this Step (§3.2,
                // checked-at paragraph).
                None => {
                    if let Some(candidates) = set.by_name.get(name) {
                        step_unapplied.extend(candidates.iter().copied());
                    }
                    continue;
                }
            }
            let entry_path = path_index(&deps_path, j);
            match set.match_entry(
                name,
                dep.is_required(),
                path_field(&entry_path, "environment"),
                &mut errors,
                &mut warnings,
            ) {
                EntryMatch::Absent => {}
                EntryMatch::One(doc) => step_resolved.push((
                    doc,
                    Supplied {
                        params: dep.parameters.as_ref().map_or(&empty, |p| &p.0),
                        symtab: &no_symbols,
                        entry_path,
                    },
                )),
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
    }

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
        for (doc, (de, att_ctx)) in set.declaring.iter().zip(&set.contexts).enumerate() {
            let supplied = resolved[i].iter().find(|(d, _)| *d == doc).map(|(_, s)| s);
            // §4 item 4: the Step's Sessions enter every `ALWAYS` attachment
            // and each `ON_REQUEST` one an entry refers to.
            let enters = de.activation == EnvironmentActivation::Always || supplied.is_some();
            if !enters || unapplied[i].contains(&doc) {
                continue;
            }
            // The value checks and constraint 7 do not apply to a Step for
            // which the Environment is disabled (§3.2.2), so they are made
            // only for one whose own `enableIf` is absent or `true`, or is
            // certainly unresolved with the Step's values: one whose
            // `enableIf` is `false` is disabled, and any other expression is
            // undetermined (§7.5 rule 5).
            let certainly_unresolved = !de.certainly_enabled
                && tv::enable_if_certainly_unresolved(de.env, supplied.map(|s| s.params), att_ctx);
            let m = de.check_step_statically(
                supplied,
                de.certainly_enabled || certainly_unresolved,
                ResolvePolicy::new(att_ctx),
                &mut errors,
            );
            if certainly_unresolved && !m.is_empty() {
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
    }
    let resolved_docs: Vec<Vec<usize>> = resolved
        .iter()
        .map(|r| r.iter().map(|(d, _)| *d).collect())
        .collect();
    check_attached_wrap_layers(
        job_template,
        attached,
        &set.doc_names,
        &resolved_docs,
        &job_path,
        &mut errors,
    );

    // §3.3.3: the literal host requirements of each Step and the
    // Environments its Sessions certainly enter, combined.
    let own_envs = job_template.job_environments.as_deref().unwrap_or(&[]);
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
    // of the Step names is left out: job creation computes no values for
    // it and then skips its host requirements, so combining it here would
    // report a conflict job creation does not. (An `ALWAYS` attachment that
    // an entry with an `enableIf` expression names is entered either way,
    // so it is kept.)
    let declares = |d: usize| {
        attached[d]
            .template
            .environment()
            .env_parameter_definitions
            .as_ref()
            .is_some_and(|p| !p.is_empty())
    };
    let entered: Vec<Vec<bool>> = job_template
        .steps
        .iter()
        .zip(&resolved_docs)
        .zip(&ambiguous)
        .map(|((step, resolved), ambiguous)| {
            let requested: HashSet<&str> = step
                .dependencies
                .iter()
                .flatten()
                .filter(|d| d.literal_enabled() == Some(true))
                .filter_map(|d| d.environment())
                .collect();
            // An Environment whose own `enableIf` is not absent or `true`
            // is not certainly entered (§4 item 6 tail).
            (0..attached.len())
                .map(|d| {
                    let env = attached[d].template.environment();
                    env.literal_enabled() == Some(true)
                        && (env.activation() == EnvironmentActivation::Always
                            || resolved.contains(&d))
                        && !(ambiguous.contains(&d) && declares(d))
                })
                .chain(own_envs.iter().map(|env| {
                    env.literal_enabled() == Some(true)
                        && (env.activation() == EnvironmentActivation::Always
                            || requested.contains(env.name.as_str()))
                }))
                .collect()
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
    errors.into_result(SUBMISSION)?;
    Ok(warnings)
}
