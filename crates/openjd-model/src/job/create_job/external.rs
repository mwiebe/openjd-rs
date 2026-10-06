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
//! **external Services** of every Job submitted through it, with every Step
//! in their scope. Two checks relate documents that only the scheduler sees
//! together and are therefore performed here, at submission, rather than at
//! template validation (§9.9):
//!
//! - the **requirement matching** rule (§1.2.2 item 2): each entry of the
//!   Job Template's `requiresServices` matches exactly one attached Service
//!   with its `name`, declaring every listed port with the same `protocol`;
//! - the **wrapping-Environment** rule (§1.2.2 item 4): a wrapping
//!   Environment from a document that does not declare `SERVICE`, placed in
//!   the combined Job's `jobEnvironments`, may not meet any Service.
//!
//! Inline Services shadow external ones (§1.2.2 item 3): an external Service
//! may share its `name` with a Service of the Job Template or of another
//! attachment, and nothing here rejects that unless a requirement names it.
//! A `Service.*` reference resolves within its own document — or, for a
//! required Service, to the attached Service the requirement was bound to —
//! so each instantiated Service is stamped with its [`job::Document`] for a
//! scheduler to keep same-named Services distinct.
//!
//! [`apply_environment_templates`] runs the checks against the combined
//! Job, instantiates the external Services with each template's own profile
//! and the merged job parameters, re-runs the carried-forward checks on each
//! attached Environment, and returns the pieces the caller merges into the
//! Job (or [`AppliedEnvironmentTemplates::into_combined_job`] merges for
//! it), together with the requirement bindings.

use openjd_expr::ExprValue;
use serde::{Deserialize, Serialize};

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
    /// one template in its `services` order, each with
    /// [`ServiceScope::AllSteps`](job::ServiceScope::AllSteps). These precede
    /// every Service in the Job Template's `services` in the combined list.
    pub external_services: Vec<job::Service>,
    /// The attached Service each of the Job Template's `requiresServices`
    /// entries was matched to (§1.2.2 item 2), in requirement order. A
    /// runtime seeds `Service.<requirement>.<port>.port` /
    /// `.connectAddress` in the Job Template's Task Sessions and Job
    /// Environments from the bound Service's endpoints.
    pub requirement_bindings: Vec<RequirementBinding>,
    /// The attached Environments, in attachment order, converted with each
    /// template's parameter symbol table frozen into `resolved_symtab`. A
    /// services-only template contributes none. These precede the Job
    /// Template's own `jobEnvironments`.
    pub environments: Vec<job::Environment>,
    /// The document each entry of `environments` comes from, index for
    /// index. A runtime that seeds `Service.*` for an attached Environment
    /// uses this to seed that document's Services only (§1.2.2 item 2: a
    /// `Service.*` reference resolves within its own document), while the
    /// Job Template's own Environments and Tasks see the Job Template's
    /// Services alone.
    pub environment_documents: Vec<job::Document>,
}

impl AppliedEnvironmentTemplates {
    /// The document of every entry of the combined `job_environments` that
    /// [`into_combined_job`](Self::into_combined_job) builds for `job`:
    /// `environment_documents`, then [`job::Document::JobTemplate`] for each
    /// of the Job's own `job_environments`. A runtime that folds the lists
    /// keeps this alongside the combined Job to know which document's
    /// Services each Job Environment may reference.
    #[must_use]
    pub fn combined_environment_documents(&self, job: &job::Job) -> Vec<job::Document> {
        let own = job.job_environments.as_ref().map_or(0, Vec::len);
        self.environment_documents
            .iter()
            .cloned()
            .chain(std::iter::repeat_n(job::Document::JobTemplate, own))
            .collect()
    }

    /// Fold the attachments into `job`: `services` becomes the external
    /// Services followed by the Job's own, and `job_environments` the
    /// attached Environments followed by the Job's own. Lists that would be
    /// empty stay `None`. `job.extensions` is left as the Job Template
    /// declared it — an extension applies to the document that lists it
    /// (§1.2 item 3), and each external Service and attached Environment
    /// carries its own `resolved_symtab`. Which document each Service came
    /// from survives the fold in [`job::Service::document`]; for the
    /// Environments, keep [`combined_environment_documents`](Self::combined_environment_documents),
    /// and for the requirements
    /// [`requirement_bindings`](Self::requirement_bindings).
    #[must_use]
    pub fn into_combined_job(self, mut job: job::Job) -> job::Job {
        if !self.external_services.is_empty() {
            let mut services = self.external_services;
            services.extend(job.services.take().into_iter().flatten());
            job.services = Some(services);
        }
        if !self.environments.is_empty() {
            let mut envs = self.environments;
            envs.extend(job.job_environments.take().into_iter().flatten());
            job.job_environments = Some(envs);
        }
        job
    }
}

/// One `requiresServices` entry of the Job Template matched to the attached
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

/// Where a Service in the combined Job comes from, for error messages.
#[derive(Clone, Copy)]
enum ServiceSource {
    External { doc: usize, index: usize },
    JobService(usize),
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
                path_index(&path_field(&self.path(None), "services"), k)
            }
        }
    }

    /// `external Service 'X' (EnvironmentTemplate[0] -> services[1])` or
    /// `Service 'X' (JobTemplate -> services[0])`.
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
///    - §1.2.2 item 2 — each entry of the Job Template's `requiresServices`
///      matches exactly one attached Service with the same `name` (none or
///      two or more is an error naming the documents), and that Service
///      declares every listed port with the same `protocol` (a missing port
///      or a protocol mismatch is an error naming the port). Reported at
///      `JobTemplate -> requiresServices[i]`.
///    - §1.2.2 item 4 — a wrapping Environment (one defining any
///      `WRAP_ACTIONS` hook) in a document that does not declare `SERVICE`
///      has the default `runScope` and no `onWrapService*` hooks; Service
///      Sessions enter the combined `jobEnvironments` only, so such an
///      Environment there, with any Service in the combined Job, is
///      rejected at the Environment, naming its document as the cause, with
///      the spec's remedy. A wrapping Environment in a Step's
///      `stepEnvironments` is never entered by a Service Session and is not
///      checked.
///
///    Service names are otherwise **not** compared across documents (§1.2.2
///    item 3): an external Service may be named like a Service of the Job
///    Template or of another attachment. Each instantiated external Service
///    carries its attachment as its [`job::Service::document`], and the Job
///    Template's own keep [`job::Document::JobTemplate`], so a scheduler
///    keys Services on `(document, name)`.
/// 2. Instantiates each external Service through the same code path as a
///    `services` entry — `<Service>.let`, `hostRequirements`, the numeric
///    `@fmtstring` fields, the carried-forward re-checks — with every Step
///    in its scope, seeing the other Services of its own document (a
///    `Service.*` reference to another document's Service is a
///    template-validation error, so none can reach here). Errors carry the
///    document in their path or message.
/// 3. Re-runs the carried-forward resolved-value checks on each attached
///    Environment against a check table holding its own document's Services
///    (when its effective `runScope` excludes `SERVICE`), then converts it
///    with [`convert_environment_with_symtab`](super::convert_environment_with_symtab)
///    and records its document in `environment_documents`.
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

    // ── The combined Service list, with provenance ──
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
    for (k, svc) in job.services.iter().flatten().enumerate() {
        combined.push(CombinedService {
            name: &svc.name,
            source: ServiceSource::JobService(k),
        });
    }

    let mut errors = ValidationErrors::default();

    // ── §1.2.2 item 2: requirement matching ──
    let mut requirement_bindings = Vec::new();
    let requirements_path = path_field(&docs.path(None), "requiresServices");
    for (i, req) in job.requires_services.iter().flatten().enumerate() {
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

    // ── §1.2.2 item 4: wrapping Environments from SERVICE-less documents ──
    //
    // A document that does not declare SERVICE cannot write `runScope` or
    // the `onWrapService*` hooks (both are gated), so any wrapping
    // Environment it defines is entered in every Service Session and cannot
    // wrap the Service. Service Sessions enter the combined `jobEnvironments`
    // only, so every Service of the combined Job is in such an Environment's
    // scope; a Step Environment is never entered by one.
    let job_declares_service = job
        .extensions
        .as_ref()
        .is_some_and(|exts| exts.contains(&ModelExtension::Service));
    let is_wrapper = |env: &job::Environment| {
        env.script
            .as_ref()
            .is_some_and(|s| s.actions.has_any_wrap_hook())
    };

    if let Some(first) = combined.first() {
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
                    errors.add(
                        &path_field(&docs.path(Some(doc)), "environment"),
                        wrapper_message(&docs, &env.name, Some(doc), first),
                    );
                }
            }
        }
        if !job_declares_service {
            let job_path = docs.path(None);
            for (i, env) in job.job_environments.iter().flatten().enumerate() {
                if is_wrapper(env) {
                    errors.add(
                        &path_index(&path_field(&job_path, "jobEnvironments"), i),
                        wrapper_message(&docs, &env.name, None, first),
                    );
                }
            }
        }
    }
    errors.into_result(SUBMISSION)?;

    // ── Instantiate the external Services and convert the Environments ──
    let base_symtab = build_symbol_table(job_parameter_values)?;
    let mut external_services = Vec::with_capacity(external_count);
    let mut environments = Vec::new();
    let mut environment_documents = Vec::new();
    for (doc, att) in attached.iter().enumerate() {
        let doc_path = docs.path(Some(doc));
        let document = job::Document::environment_template(doc, att.label);
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
            services,
            requirements: &[],
        };

        let services_path = path_field(&[], "services");
        for (k, svc) in services.iter().enumerate() {
            // An external Service has every Step in its scope (§1.2.2 item
            // 1) and depends only on Services of its own document, which
            // are the ones whose endpoints it may read.
            let mut instantiated = instantiate::instantiate_service(
                svc,
                &symtab,
                icx,
                &path_index(&services_path, k),
                crate::template::listed_services(svc.dependencies.as_deref(), services),
                job::ServiceScope::AllSteps,
            )
            .map_err(|e| in_document(e, &doc_path))?;
            // §1.2.2 item 3: the Service is known by (document, name).
            instantiated.document = document.clone();
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
                std::iter::empty(),
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
            environment_documents.push(document);
        }
    }

    Ok(AppliedEnvironmentTemplates {
        external_services,
        requirement_bindings,
        environments,
        environment_documents,
    })
}

/// The §1.2.2 item 4 message for wrapping Environment `env_name`, defined
/// by `doc` (`None` for the Job Template), with `in_scope` the first Service
/// of the combined Job (every Service is in a Job Environment's scope).
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
         onWrapServiceHealthCheck, and onWrapServiceExit, or declare a runScope that excludes \
         SERVICE (RFC 0009, Template Schemas §1.2.2 item 4).",
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
            "Service 'S' (JobTemplate -> services[0])"
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
            services: None,
            requires_services: None,
        };
        let applied = AppliedEnvironmentTemplates {
            external_services: Vec::new(),
            requirement_bindings: Vec::new(),
            environments: Vec::new(),
            environment_documents: Vec::new(),
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
