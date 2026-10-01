// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Applying Environment Templates to a created Job (Template Schemas §1.2.2
//! "Services from Environment Templates", RFC 0009 "Environment Template").
//!
//! A submission combines one Job Template with zero or more Environment
//! Templates that the scheduler attaches, in the scheduler's order. Before
//! RFC 0009 an Environment Template contributed only an Environment, placed
//! in the Job's `jobEnvironments` ahead of the Job Template's own; with the
//! `SERVICE` extension it may also define `services`, which become
//! **external Services** of every Job submitted through it. Two checks
//! relate documents that only the scheduler sees together and are therefore
//! performed here, at submission, rather than at template validation:
//!
//! 1. the external-Service name collision rule (§1.2.2 item 2), and
//! 2. the wrapping-Environment rule (§1.2.2 item 3): a wrapping Environment
//!    from a document that does not declare `SERVICE` may not have a
//!    Service placed in its scope.
//!
//! [`apply_environment_templates`] runs both checks against the combined
//! Job, instantiates the external Services with each template's own
//! profile and the merged job parameters, re-runs the carried-forward
//! checks on each attached Environment, and returns the pieces the caller
//! merges into the Job (or [`AppliedEnvironmentTemplates::into_combined_job`]
//! merges for it).

use openjd_expr::ExprValue;

use crate::error::{
    path_field, path_index, path_to_string, ModelError, PathElement, ValidationErrors,
};
use crate::job;
use crate::template::validate_v2023_09::EffectiveLimits;
use crate::template::EnvironmentTemplate;
use crate::types::{CallerLimits, JobParameterValues, ModelExtension};

use super::instantiate::{self, InstantiateCtx};
use super::{build_symbol_table, EvalBudgets};

/// The model name under which the submission-time checks report: these
/// checks relate several documents, so no single template is "the" model.
const SUBMISSION: &str = "Submission";

/// One Environment Template attached to a submission, with an optional
/// label naming the document in error messages.
///
/// Without a label the document is named `EnvironmentTemplate[i]` (its
/// 0-based attachment index, the same indexing error paths use). A label —
/// typically the file path the template was read from — replaces that
/// name wholesale, so a message reads `queue-cache.environment.yaml ->
/// services[0]` instead of `EnvironmentTemplate[0] -> services[0]`.
#[derive(Debug, Clone, Copy)]
pub struct AttachedEnvironmentTemplate<'a> {
    /// The decoded, validated template.
    pub template: &'a EnvironmentTemplate,
    /// How to name this document in error messages; `None` for the
    /// positional default.
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

    /// Name this document `label` in error messages.
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

/// What a set of Environment Templates contributes to a Job, in the order
/// the submission combines them (Template Schemas §1.2.2 item 1).
///
/// Produced by [`apply_environment_templates`]. Callers either read the two
/// lists directly (a runtime that enters the attached Environments and
/// starts the external Services itself) or call
/// [`into_combined_job`](Self::into_combined_job) to fold them into the Job.
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedEnvironmentTemplates {
    /// The external Services, instantiated: in attachment order, and within
    /// one template in its `services` order. These precede every Service in
    /// the Job Template's `jobServices` in the combined list, which is
    /// started and stopped as a single list.
    pub external_services: Vec<job::Service>,
    /// The attached Environments, in attachment order, converted with each
    /// template's parameter symbol table frozen into `resolved_symtab`. A
    /// services-only template contributes none. These precede the Job
    /// Template's own `jobEnvironments`.
    pub environments: Vec<job::Environment>,
}

impl AppliedEnvironmentTemplates {
    /// Fold the attachments into `job`: `job_services` becomes the external
    /// Services followed by the Job's own, and `job_environments` the
    /// attached Environments followed by the Job's own. Lists that would be
    /// empty stay `None`. `job.extensions` is left as the Job Template
    /// declared it — an extension applies to the document that lists it
    /// (§1.2 item 3), and each external Service and attached Environment
    /// carries its own `resolved_symtab`.
    #[must_use]
    pub fn into_combined_job(self, mut job: job::Job) -> job::Job {
        if !self.external_services.is_empty() {
            let mut services = self.external_services;
            services.extend(job.job_services.take().into_iter().flatten());
            job.job_services = Some(services);
        }
        if !self.environments.is_empty() {
            let mut envs = self.environments;
            envs.extend(job.job_environments.take().into_iter().flatten());
            job.job_environments = Some(envs);
        }
        job
    }
}

/// Where a Service in the combined Job comes from, for error messages.
#[derive(Clone, Copy)]
enum ServiceSource {
    External { doc: usize, index: usize },
    JobService(usize),
    StepService { step: usize, index: usize },
}

/// A Service of the combined Job: its name and where it is declared.
struct CombinedService<'a> {
    name: &'a str,
    source: ServiceSource,
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

    /// The full path of a Service's declaration.
    fn service_path(&self, source: ServiceSource) -> Vec<PathElement> {
        match source {
            ServiceSource::External { doc, index } => {
                path_index(&path_field(&self.path(Some(doc)), "services"), index)
            }
            ServiceSource::JobService(k) => {
                path_index(&path_field(&self.path(None), "jobServices"), k)
            }
            ServiceSource::StepService { step, index } => path_index(
                &path_field(
                    &path_index(&path_field(&self.path(None), "steps"), step),
                    "stepServices",
                ),
                index,
            ),
        }
    }

    /// `external Service 'X' (EnvironmentTemplate[0] -> services[1])` or
    /// `Service 'X' (JobTemplate -> jobServices[0])`.
    fn describe_service(&self, svc: &CombinedService<'_>) -> String {
        let kind = match svc.source {
            ServiceSource::External { .. } => "external Service",
            _ => "Service",
        };
        format!(
            "{kind} '{}' ({})",
            svc.name,
            path_to_string(&self.service_path(svc.source))
        )
    }
}

/// Apply the Environment Templates of a submission to `job`, the Job that
/// [`create_job`](super::create_job) built from the Job Template alone
/// (Template Schemas §1.2.2, RFC 0009 "Environment Template").
///
/// `attached` is in the scheduler's order. `job_parameter_values` are the
/// merged values [`preprocess_job_parameters`](super::preprocess_job_parameters)
/// produced for the same submission (every template's `parameterDefinitions`
/// merged per §1.2.1), and `caller_limits` the caller's policy, applied to
/// every template as it was to the Job Template.
///
/// Each attached template is evaluated under **its own** profile
/// ([`EnvironmentTemplate::profile`] plus `caller_limits`): an extension
/// applies to the document that lists it, so the Job Template need not
/// declare `SERVICE` or `EXPR` for an attachment to use them, and vice
/// versa. The symbol table for a template is the `Param.*` / `RawParam.*`
/// table of the merged job parameters, plus `Job.Name` when that template
/// declares `EXPR`.
///
/// The function:
///
/// 1. Runs the two submission-time checks against the **combined** Job and
///    reports every violation at once, as a `ModelValidation` error for
///    `Submission` whose paths start with the document (`JobTemplate`,
///    `EnvironmentTemplate[i]`, or the attachment's label):
///    - §1.2.2 item 2 — the name of an external Service must not equal the
///      name of any other external Service, nor of any Service in the Job
///      Template's `jobServices` or any Step's `stepServices`; each
///      collision is reported at the external Service, naming both sources.
///    - §1.2.2 item 3 — a wrapping Environment (one defining any
///      `WRAP_ACTIONS` hook) in a document that does not declare `SERVICE`
///      has the default `runScope` and no `onWrapService*` hooks, so no
///      Service may be placed in its scope: for a Job Environment (the Job
///      Template's or an attached one) that is every Service of the
///      combined `jobServices` and every Step's `stepServices`; for a Step
///      Environment, the combined `jobServices` and that Step's
///      `stepServices`. Reported at the Environment, naming its document as
///      the cause, with the spec's remedy.
/// 2. Instantiates each external Service through the same code path as a
///    `jobServices` entry — `<Service>.let`, `hostRequirements`, the numeric
///    `@fmtstring` fields, the carried-forward re-checks, its
///    `serviceEnvironments` — in Job scope, seeing the Services before it in
///    its own document (a `Service.*` reference to another document's Service
///    is a template-validation error, so none can reach here). Errors carry
///    the document in their path or message. A Service's `serviceEnvironments`
///    are not subject to rule 3 above: they share their Service's document,
///    which declares `SERVICE`, and have the effective `runScope: [SERVICE]`.
/// 3. Re-runs the carried-forward resolved-value checks on each attached
///    Environment against a check table holding its own document's Services
///    (when its `runScope` excludes `SERVICE`), then converts it with
///    [`convert_environment_with_symtab`](super::convert_environment_with_symtab).
///
/// Attachments that define no Services behave exactly as before RFC 0009:
/// their Environments are converted and the Service checks have nothing to
/// examine. The 10-element cap on `services` is per document (enforced at
/// template validation); the combined list is not capped here.
pub fn apply_environment_templates(
    job: &job::Job,
    attached: &[AttachedEnvironmentTemplate<'_>],
    job_parameter_values: &JobParameterValues,
    caller_limits: &CallerLimits,
) -> Result<AppliedEnvironmentTemplates, ModelError> {
    let docs = Documents { attached };

    // ── The combined Service list, in start order, with provenance ──
    let mut combined: Vec<CombinedService<'_>> = Vec::new();
    for (doc, att) in attached.iter().enumerate() {
        for (index, svc) in att.template.services().iter().enumerate() {
            combined.push(CombinedService {
                name: &svc.name,
                source: ServiceSource::External { doc, index },
            });
        }
    }
    let external_count = combined.len();
    for (k, svc) in job.job_services.iter().flatten().enumerate() {
        combined.push(CombinedService {
            name: &svc.name,
            source: ServiceSource::JobService(k),
        });
    }
    let combined_job_services = combined.len();
    // Step Services, grouped by step: `step_service_ranges[i]` indexes into
    // `combined`.
    let mut step_service_ranges: Vec<std::ops::Range<usize>> = Vec::with_capacity(job.steps.len());
    for (step, st) in job.steps.iter().enumerate() {
        let start = combined.len();
        for (index, svc) in st.step_services.iter().flatten().enumerate() {
            combined.push(CombinedService {
                name: &svc.name,
                source: ServiceSource::StepService { step, index },
            });
        }
        step_service_ranges.push(start..combined.len());
    }

    let mut errors = ValidationErrors::default();

    // ── §1.2.2 item 2: external-Service name collisions ──
    for i in 0..external_count {
        let ext = &combined[i];
        // Earlier external Services (a repeat within one document is a
        // template-validation error, so a hit here is always across
        // documents), then every Service the Job Template declares.
        let others = combined[..i]
            .iter()
            .chain(combined[external_count..].iter());
        for other in others {
            if other.name == ext.name {
                errors.add(
                    &docs.service_path(ext.source),
                    format!(
                        "{} has the same name as {}; the name of an external Service must not \
                         equal the name of any other external Service, nor of any Service in the \
                         Job Template's jobServices or any Step's stepServices (RFC 0009, \
                         Template Schemas §1.2.2 item 2).",
                        docs.describe_service(ext),
                        docs.describe_service(other),
                    ),
                );
            }
        }
    }

    // ── §1.2.2 item 3: wrapping Environments from SERVICE-less documents ──
    //
    // A document that does not declare SERVICE cannot write `runScope` or
    // the `onWrapService*` hooks (both are gated), so any wrapping
    // Environment it defines is entered in every Service Session in its
    // scope and cannot wrap the Service. The scope of a Job Environment is
    // every Service of the combined Job; a Step Environment's is the
    // combined `jobServices` plus its Step's `stepServices`.
    let job_declares_service = job
        .extensions
        .as_ref()
        .is_some_and(|exts| exts.contains(&ModelExtension::Service));
    let job_env_scope = || {
        combined.iter().take(combined_job_services).chain(
            step_service_ranges
                .iter()
                .flat_map(|r| combined[r.clone()].iter()),
        )
    };

    for (doc, att) in attached.iter().enumerate() {
        if att
            .template
            .profile()
            .has_extension(ModelExtension::Service)
        {
            continue;
        }
        if let Some(env) = att.template.environment() {
            if env
                .script
                .as_ref()
                .is_some_and(|s| s.actions.has_any_wrap_hook())
            {
                if let Some(first) = job_env_scope().next() {
                    errors.add(
                        &path_field(&docs.path(Some(doc)), "environment"),
                        wrapper_message(&docs, &env.name, Some(doc), first),
                    );
                }
            }
        }
    }
    if !job_declares_service {
        let is_wrapper = |env: &job::Environment| {
            env.script
                .as_ref()
                .is_some_and(|s| s.actions.has_any_wrap_hook())
        };
        let job_path = docs.path(None);
        for (i, env) in job.job_environments.iter().flatten().enumerate() {
            if is_wrapper(env) {
                if let Some(first) = job_env_scope().next() {
                    errors.add(
                        &path_index(&path_field(&job_path, "jobEnvironments"), i),
                        wrapper_message(&docs, &env.name, None, first),
                    );
                }
            }
        }
        for (step, st) in job.steps.iter().enumerate() {
            for (j, env) in st.step_environments.iter().flatten().enumerate() {
                if is_wrapper(env) {
                    // A Step Environment is entered only in the Sessions of
                    // that Step's own Services (a Job Service's Session enters
                    // `jobEnvironments` alone), so only `stepServices` are in
                    // its scope.
                    let first = combined[step_service_ranges[step].clone()].iter().next();
                    if let Some(first) = first {
                        let step_path = path_index(&path_field(&job_path, "steps"), step);
                        errors.add(
                            &path_index(&path_field(&step_path, "stepEnvironments"), j),
                            wrapper_message(&docs, &env.name, None, first),
                        );
                    }
                }
            }
        }
    }
    errors.into_result(SUBMISSION)?;

    // ── Instantiate the external Services and convert the Environments ──
    let base_symtab = build_symbol_table(job_parameter_values)?;
    let mut external_services = Vec::with_capacity(external_count);
    let mut environments = Vec::new();
    for (doc, att) in attached.iter().enumerate() {
        let doc_path = docs.path(Some(doc));
        let ctx = att
            .template
            .default_validation_context()
            .with_caller_limits(caller_limits.clone());
        let has_expr = ctx.profile.has_extension(ModelExtension::Expr);
        let limits = EffectiveLimits::from_context(&ctx);
        let budgets = EvalBudgets::from_ctx(&ctx);
        let mut symtab = base_symtab.clone();
        if has_expr {
            symtab.set("Job.Name", ExprValue::String(job.name.clone()))?;
        }
        let services = att.template.services();
        let icx = InstantiateCtx {
            has_expr,
            limits: &limits,
            ctx: &ctx,
            budgets,
            job_services: services,
        };

        let services_path = path_field(&[], "services");
        for (k, svc) in services.iter().enumerate() {
            let instantiated = instantiate::instantiate_service(
                svc,
                &symtab,
                icx,
                &path_index(&services_path, k),
                services[..k].iter(),
            )
            .map_err(|e| in_document(e, &doc_path))?;
            external_services.push(instantiated);
        }

        if let Some(env) = att.template.environment() {
            // The same carried-forward re-checks `create_job` runs on a
            // `jobEnvironments` entry, against this document's Services
            // (seeded only when the Environment's runScope excludes SERVICE).
            let env_symtab = instantiate::build_env_check_symtab(
                env,
                &symtab,
                has_expr,
                &ctx,
                budgets,
                services.iter(),
            )
            .map_err(|e| in_document(e, &doc_path))?;
            let mut check_errors = ValidationErrors::default();
            crate::template::validate_v2023_09::format_strings::check_carried_forward_environment(
                env,
                &env_symtab,
                &ctx,
                limits.max_env_var_value_len,
                &path_field(&[], "environment"),
                &mut check_errors,
            );
            check_errors
                .into_result(SUBMISSION)
                .map_err(|e| in_document(e, &doc_path))?;
            environments.push(instantiate::convert_environment_with_symtab(
                env,
                Some(&symtab),
            ));
        }
    }

    Ok(AppliedEnvironmentTemplates {
        external_services,
        environments,
    })
}

/// The §1.2.2 item 3 message for wrapping Environment `env_name`, defined
/// by `doc` (`None` for the Job Template), with `in_scope` the first Service
/// the combined Job places in its scope.
fn wrapper_message(
    docs: &Documents<'_>,
    env_name: &str,
    doc: Option<usize>,
    in_scope: &CombinedService<'_>,
) -> String {
    let document = docs.name(doc);
    format!(
        "wrapping Environment '{env_name}' is defined by {document}, which does not declare the \
         SERVICE extension, so it has the default runScope (every kind of Session) and cannot \
         define the onWrapService* hooks; but the combined Job places {} in its scope, and the \
         Service would run in a Session the Environment enters but cannot wrap. Declare SERVICE \
         in {document} and either define onWrapServiceEnter, onWrapServiceRun, \
         onWrapServiceReadinessCheck, and onWrapServiceExit, or declare a runScope that excludes \
         SERVICE (RFC 0009, Template Schemas §1.2.2 item 3).",
        docs.describe_service(in_scope),
    )
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
        } => ModelError::FormatStringError {
            message: format!("{}: {message}", path_to_string(doc)),
            input,
            start,
            end,
        },
        ModelError::Expression(e) => ModelError::Expression(openjd_expr::ExpressionError::new(
            format!("{}: {e}", path_to_string(doc)),
        )),
        other => other,
    }
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
            &CallerLimits::default(),
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

        let ext = CombinedService {
            name: "S",
            source: ServiceSource::External { doc: 1, index: 2 },
        };
        assert_eq!(
            docs.describe_service(&ext),
            "external Service 'S' (queue/b.yaml -> services[2])"
        );
        let js = CombinedService {
            name: "S",
            source: ServiceSource::JobService(0),
        };
        assert_eq!(
            docs.describe_service(&js),
            "Service 'S' (JobTemplate -> jobServices[0])"
        );
        let ss = CombinedService {
            name: "S",
            source: ServiceSource::StepService { step: 3, index: 1 },
        };
        assert_eq!(
            docs.describe_service(&ss),
            "Service 'S' (JobTemplate -> steps[3] -> stepServices[1])"
        );
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
    fn into_combined_job_leaves_empty_lists_as_none() {
        let job = job::Job {
            name: "J".to_string(),
            description: None,
            extensions: None,
            parameters: Default::default(),
            steps: Vec::new(),
            job_environments: None,
            job_services: None,
        };
        let applied = AppliedEnvironmentTemplates {
            external_services: Vec::new(),
            environments: Vec::new(),
        };
        let combined = applied.into_combined_job(job.clone());
        assert_eq!(combined, job);
    }

    #[test]
    fn attached_environment_template_from_ref_has_no_label() {
        let t = env_template(PLAIN);
        let att: AttachedEnvironmentTemplate<'_> = (&t).into();
        assert!(att.label.is_none());
        assert!(std::ptr::eq(att.template, &t));
    }
}
