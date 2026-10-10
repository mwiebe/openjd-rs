// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Integration tests for applying Environment Templates to a Job under RFC
//! 0009 (Template Schemas §1.2.2 "Services from Environment Templates" and
//! §9.7's submission-time check) via `apply_environment_templates`:
//!
//! 1. **Merge rule 1** — the RFC's queue-cache example: the external
//!    `Cache` Service is instantiated first, before the Job Template's own
//!    `services`, with the attachment's parameters and `Job.Name` in
//!    scope; attached Environments precede the Job's own; a submission with
//!    several attachments orders Services by attachment then `services`
//!    order; Environment-only attachments behave as before RFC 0009.
//! 2. **Merge rule 2** — Service names are scoped to their document: an
//!    external Service named like an inline Service, or like another
//!    attachment's Service, is accepted and kept distinct through
//!    `job::Service::document`; the documents of the combined Environments
//!    are reported alongside.
//! 3. **§1.2.2 item 4** — the wrapping-Environment rule in both directions
//!    (a SERVICE-less wrapper attachment with a Job that declares Services;
//!    a SERVICE-less Job Template Job-level wrapper with an attachment that
//!    defines a Service), its non-application to Step Environments, the
//!    non-rejection when the
//!    wrapper declares `SERVICE` with the four hooks or with `runScope:
//!    [TASK]`, and the non-rejection when nothing places a Service in scope.
//! 4. **Scope** — an external Service has every Step in its scope whether
//!    or not any Step lists it (Environment Template Services are never
//!    "unused"); it sees another Service of its own document only when it
//!    lists `service:<name>` in its `dependencies`, and that list may name
//!    only Services of the same document; a Job Template cannot reference
//!    an external Service it does not require (template validation); an
//!    attachment cannot reference another attachment's Service (template
//!    validation). The Job Templates used here list `service:<name>` on the
//!    Step that uses each inline Service, as the declared-dependency rules
//!    require. Requirement matching (§1.2.2 item 2) is covered in
//!    `test_service_requirements.rs`.
//! 5. **Per-document cap** — three documents of 10 Services each combine to
//!    30; the cap is per document, not on the combined list.
//! 6. **Errors from inside a document** carry the document in their path or
//!    message, and labels replace the positional document name.

use std::sync::LazyLock;

use openjd_expr::ExprValue;
use openjd_model::job::{self, CompletedTasksPolicy, RunScope, ServiceScope};
use openjd_model::template::EnvironmentTemplate;
use openjd_model::{
    apply_environment_templates, create_job, decode_environment_template, decode_job_template,
    AppliedEnvironmentTemplates, AttachedEnvironmentTemplate, CallerLimits,
    JobParameterInputValues, ModelError,
};

const EXTS: &[&str] = &["EXPR", "SERVICE", "FEATURE_BUNDLE_1", "WRAP_ACTIONS"];

const RFC_QUEUE_CACHE: &str = include_str!("../fixtures/rfc0009/queue-cache.environment.yaml");
const RFC_CONSUMER: &str = include_str!("../fixtures/rfc0009/queue-cache-consumer.job.yaml");
/// The RFC's Valkey and coordinator examples. Under the declared-dependency
/// rules the Step that uses the Service lists `service:<name>`, as the RFC's
/// examples do; [`with_service_dependency`] adds the entry when the fixture
/// predates it, and is a no-op once the fixture carries it.
static RFC_VALKEY: LazyLock<String> = LazyLock::new(|| {
    with_service_dependency(
        include_str!("../fixtures/rfc0009/valkey-shared-store.job.yaml"),
        "ProcessFrames",
        "Cache",
    )
});
static RFC_COORDINATOR: LazyLock<String> = LazyLock::new(|| {
    with_service_dependency(
        include_str!("../fixtures/rfc0009/step-coordinator.job.yaml"),
        "RenderTiles",
        "Coordinator",
    )
});

/// Adds `service: <service>` to the `dependencies` of the Step
/// named `step` (a top-level `  - name: <step>` entry), creating the list
/// when the Step has none. Unchanged when the template already lists it.
fn with_service_dependency(template: &str, step: &str, service: &str) -> String {
    let entry = format!("      - service: {service}\n");
    if template.contains(entry.trim_start()) {
        return template.to_string();
    }
    let name_line = format!("  - name: {step}\n");
    let at = template
        .find(&name_line)
        .unwrap_or_else(|| panic!("no Step named {step}"))
        + name_line.len();
    let (head, tail) = template.split_at(at);
    match tail.strip_prefix("    dependencies:\n") {
        Some(rest) => format!("{head}    dependencies:\n{entry}{rest}"),
        None => format!("{head}    dependencies:\n{entry}{tail}"),
    }
}

fn yaml_val(s: &str) -> serde_json::Value {
    serde_saphyr::from_str(s).unwrap()
}

fn decode_job(template: &str) -> openjd_model::template::JobTemplate {
    decode_job_template(yaml_val(template), Some(EXTS), &CallerLimits::default())
        .expect("job template should validate")
}

fn decode_env(template: &str) -> EnvironmentTemplate {
    decode_environment_template(yaml_val(template), Some(EXTS), &CallerLimits::default())
        .expect("environment template should validate")
}

/// `create_job` for the Job Template alone, then `apply_environment_templates`
/// with the attachments in order. Returns the Job and the application.
fn submit(
    job_template: &str,
    attachments: &[&str],
    params: &[(&str, &str)],
) -> Result<(job::Job, AppliedEnvironmentTemplates), ModelError> {
    let jt = decode_job(job_template);
    let ets: Vec<EnvironmentTemplate> = attachments.iter().map(|t| decode_env(t)).collect();
    submit_decoded(&jt, &ets, params)
}

fn submit_decoded(
    jt: &openjd_model::template::JobTemplate,
    ets: &[EnvironmentTemplate],
    params: &[(&str, &str)],
) -> Result<(job::Job, AppliedEnvironmentTemplates), ModelError> {
    let td = tempfile::TempDir::new().unwrap();
    let dir = td.path().to_str().unwrap();
    let input: JobParameterInputValues = params
        .iter()
        .map(|(k, v)| (k.to_string(), ExprValue::String(v.to_string())))
        .collect();
    let processed = openjd_model::preprocess_job_parameters(
        jt,
        &input,
        ets,
        &openjd_model::PathParameterOptions::new(dir, dir),
    )?;
    let job = create_job(jt, &processed, &jt.default_validation_context())?;
    let attached: Vec<AttachedEnvironmentTemplate<'_>> = ets.iter().map(Into::into).collect();
    let applied =
        apply_environment_templates(&job, &attached, &processed, &CallerLimits::default())?;
    Ok((job, applied))
}

fn submit_ok(
    job_template: &str,
    attachments: &[&str],
    params: &[(&str, &str)],
) -> (job::Job, AppliedEnvironmentTemplates) {
    submit(job_template, attachments, params).expect("submission should succeed")
}

fn submit_err(job_template: &str, attachments: &[&str], params: &[(&str, &str)]) -> String {
    submit(job_template, attachments, params)
        .expect_err("submission should fail")
        .to_string()
}

fn service_names(services: &[job::Service]) -> Vec<&str> {
    services.iter().map(|s| s.name.as_str()).collect()
}

// ════════════════════════════════════════════════════════════════════
// Templates used across tests
// ════════════════════════════════════════════════════════════════════

/// A minimal Job Template that declares nothing.
const PLAIN_JOB: &str = r#"
specificationVersion: "jobtemplate-2023-09"
name: Plain
steps:
  - name: Render
    script:
      actions:
        onRun:
          command: echo
"#;

/// A Job Template with an RFC 0008 wrapper as a Job Environment and no
/// `SERVICE` declaration.
const WRAPPER_JOB: &str = r#"
specificationVersion: "jobtemplate-2023-09"
name: Wrapped
extensions: [WRAP_ACTIONS, EXPR]
jobEnvironments:
  - name: Container
    script:
      actions:
        onWrapEnvEnter: { command: run-in-container, args: ["{{ WrappedAction.Command }}"] }
        onWrapTaskRun: { command: run-in-container, args: ["{{ WrappedAction.Command }}"] }
        onWrapEnvExit: { command: run-in-container, args: ["{{ WrappedAction.Command }}"] }
steps:
  - name: Render
    script:
      actions:
        onRun:
          command: echo
"#;

/// Like `WRAPPER_JOB`, but the wrapper is a Step Environment of the first of
/// two Steps.
const STEP_WRAPPER_JOB: &str = r#"
specificationVersion: "jobtemplate-2023-09"
name: StepWrapped
extensions: [WRAP_ACTIONS, EXPR]
steps:
  - name: Render
    stepEnvironments:
      - name: Container
        script:
          actions:
            onWrapEnvEnter: { command: run-in-container, args: ["{{ WrappedAction.Command }}"] }
            onWrapTaskRun: { command: run-in-container, args: ["{{ WrappedAction.Command }}"] }
            onWrapEnvExit: { command: run-in-container, args: ["{{ WrappedAction.Command }}"] }
    script:
      actions:
        onRun:
          command: echo
  - name: Composite
    script:
      actions:
        onRun:
          command: echo
"#;

/// A SERVICE-less wrapper attachment (the RFC 0008 shape a queue would use
/// for a container launcher).
const WRAPPER_ENV_TEMPLATE: &str = r#"
specificationVersion: "environment-2023-09"
extensions: [WRAP_ACTIONS, EXPR]
environment:
  name: QueueContainer
  script:
    actions:
      onWrapEnvEnter: { command: run-in-container, args: ["{{ WrappedAction.Command }}"] }
      onWrapTaskRun: { command: run-in-container, args: ["{{ WrappedAction.Command }}"] }
      onWrapEnvExit: { command: run-in-container, args: ["{{ WrappedAction.Command }}"] }
"#;

/// The same wrapper, declaring `SERVICE` with all seven hooks.
const SERVICE_AWARE_WRAPPER_ENV_TEMPLATE: &str = r#"
specificationVersion: "environment-2023-09"
extensions: [WRAP_ACTIONS, EXPR, SERVICE]
environment:
  name: QueueContainer
  script:
    actions:
      onWrapEnvEnter: { command: run-in-container, args: ["{{ WrappedAction.Command }}"] }
      onWrapTaskRun: { command: run-in-container, args: ["{{ WrappedAction.Command }}"] }
      onWrapEnvExit: { command: run-in-container, args: ["{{ WrappedAction.Command }}"] }
      onWrapServiceEnter: { command: run-in-container, args: ["{{ WrappedAction.Command }}"] }
      onWrapServiceRun: { command: run-in-container, args: ["{{ WrappedAction.Command }}"] }
      onWrapServiceHealthCheck: { command: run-in-container, args: ["{{ WrappedAction.Command }}"] }
      onWrapServiceExit: { command: run-in-container, args: ["{{ WrappedAction.Command }}"] }
"#;

/// The same wrapper, declaring `SERVICE` and opting out of Service Sessions.
const TASK_ONLY_WRAPPER_ENV_TEMPLATE: &str = r#"
specificationVersion: "environment-2023-09"
extensions: [WRAP_ACTIONS, EXPR, SERVICE]
environment:
  name: QueueContainer
  runScope: [TASK]
  script:
    actions:
      onWrapEnvEnter: { command: run-in-container, args: ["{{ WrappedAction.Command }}"] }
      onWrapTaskRun: { command: run-in-container, args: ["{{ WrappedAction.Command }}"] }
      onWrapEnvExit: { command: run-in-container, args: ["{{ WrappedAction.Command }}"] }
"#;

/// A plain pre-RFC-0009 Environment Template: no extensions, no Services.
const PLAIN_ENV_TEMPLATE: &str = r#"
specificationVersion: "environment-2023-09"
parameterDefinitions:
  - name: Studio
    type: STRING
    default: acme
environment:
  name: StudioSetup
  variables:
    STUDIO: "{{ Param.Studio }}"
"#;

/// A services-only attachment defining one Service named `name`.
fn service_only_template(name: &str) -> String {
    format!(
        r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, EXPR]
services:
  - name: {name}
    ports: [{{ name: main }}]
    script:
      actions:
        onRun: {{ command: serve, args: ["{{{{ Service.{name}.main.port }}}}"] }}
"#
    )
}

/// A services-only attachment with `n` Services named `<prefix>0..n`.
fn many_services_template(prefix: &str, n: usize) -> String {
    let services: Vec<String> = (0..n)
        .map(|i| {
            format!(
                r#"  - name: {prefix}{i}
    ports: [{{ name: main }}]
    script:
      actions:
        onRun: {{ command: serve }}"#
            )
        })
        .collect();
    format!(
        "specificationVersion: \"environment-2023-09\"\nextensions: [SERVICE, EXPR]\nservices:\n{}\n",
        services.join("\n")
    )
}

/// A Job Template with `n` inline Services named `<prefix>0..n`, each listed
/// in the dependencies of its only Step so that none is unused.
fn many_job_services_job(prefix: &str, n: usize) -> String {
    let services: Vec<String> = (0..n)
        .map(|i| {
            format!(
                r#"  - name: {prefix}{i}
    ports: [{{ name: main }}]
    script:
      actions:
        onRun: {{ command: serve }}"#
            )
        })
        .collect();
    let dependencies: String = (0..n)
        .map(|i| format!("      - service: {prefix}{i}\n"))
        .collect();
    format!(
        "specificationVersion: \"jobtemplate-2023-09\"\nname: Many\nextensions: [SERVICE, EXPR]\nservices:\n{}\nsteps:\n  - name: S\n    dependencies:\n{dependencies}    script:\n      actions:\n        onRun:\n          command: echo\n",
        services.join("\n")
    )
}

// ════════════════════════════════════════════════════════════════════
// 1. Merge rule 1 — ordering and the RFC example
// ════════════════════════════════════════════════════════════════════

#[test]
fn rfc_queue_cache_example_merges_with_the_cache_service_first() {
    // The RFC's third example: a queue cache attachment applied to a Job
    // Template that has never heard of Services.
    let (job, applied) = submit_ok(RFC_CONSUMER, &[RFC_QUEUE_CACHE], &[]);
    assert!(job.services.is_none(), "the consumer declares no Services");
    assert_eq!(service_names(&applied.external_services), ["Cache"]);

    let cache = &applied.external_services[0];
    // An external Service has every Step in its scope (§1.2.2 item 1) and
    // is stamped with its document.
    assert_eq!(cache.scope, ServiceScope::AllSteps);
    assert_eq!(cache.document, job::Document::environment_template(0, None));
    assert!(applied.requirement_bindings.is_empty());
    assert_eq!(
        cache.description.as_deref(),
        Some("A per-Job Valkey store shared by all of the Job's Tasks.")
    );
    assert_eq!(cache.ports.len(), 1);
    assert_eq!(cache.ports[0].name, "main");
    assert_eq!(cache.restart_policy.max_attempts, 3);
    assert_eq!(
        cache.restart_policy.completed_tasks,
        Some(CompletedTasksPolicy::Keep)
    );
    // `hostRequirements` resolved from the attachment's own parameter
    // default (merged into the job parameters by preprocessing).
    let hr = cache.host_requirements.as_ref().expect("hostRequirements");
    let amounts = hr.amounts.as_ref().expect("amounts");
    assert_eq!(amounts.len(), 1);
    assert_eq!(amounts[0].name, "amount.worker.memory");
    assert_eq!(amounts[0].min, Some(8192.0));

    // The attachment's Environment is converted, keeping its runScope.
    assert_eq!(applied.environments.len(), 1);
    let client = &applied.environments[0];
    assert_eq!(client.name, "CacheClient");
    assert_eq!(client.run_scope, Some(vec![RunScope::Task]));
    assert!(client.runs_in(RunScope::Task));
    assert!(!client.runs_in(RunScope::Service));

    // Folding into the Job: the external Service becomes services[0] and
    // the attached Environment jobEnvironments[0].
    let combined = applied.into_combined_job(job);
    assert_eq!(
        service_names(combined.services.as_deref().unwrap()),
        ["Cache"]
    );
    assert_eq!(
        combined
            .job_environments
            .as_deref()
            .unwrap()
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>(),
        ["CacheClient"]
    );
    // The Job Template's own extensions are untouched: the consumer
    // declared none.
    assert!(combined.extensions.is_none());
    assert_eq!(combined.steps.len(), 1);
}

#[test]
fn attachment_parameter_overrides_the_default_in_the_external_service() {
    let (_, applied) = submit_ok(
        RFC_CONSUMER,
        &[RFC_QUEUE_CACHE],
        &[("CacheMemoryMiB", "1024")],
    );
    let hr = applied.external_services[0]
        .host_requirements
        .as_ref()
        .unwrap();
    assert_eq!(hr.amounts.as_ref().unwrap()[0].min, Some(1024.0));
}

#[test]
fn external_services_precede_the_job_templates_own_services() {
    // The Valkey example declares an inline Service `Cache`; the queue attaches
    // `Metrics`. The combined list is `[Metrics, Cache]`.
    let (job, applied) = submit_ok(&RFC_VALKEY, &[&service_only_template("Metrics")], &[]);
    assert_eq!(service_names(job.services.as_deref().unwrap()), ["Cache"]);
    assert_eq!(service_names(&applied.external_services), ["Metrics"]);
    let combined = applied.into_combined_job(job);
    assert_eq!(
        service_names(combined.services.as_deref().unwrap()),
        ["Metrics", "Cache"]
    );
    // The Job Template's SERVICE declaration survives the merge.
    assert!(combined
        .extensions
        .as_ref()
        .unwrap()
        .contains(&openjd_model::ModelExtension::Service));
}

#[test]
fn several_attachments_order_by_attachment_then_services_order() {
    let two = many_services_template("A", 2);
    let one = service_only_template("B");
    let (job, applied) = submit_ok(&many_job_services_job("J", 2), &[&two, &one], &[]);
    assert_eq!(service_names(&applied.external_services), ["A0", "A1", "B"]);
    assert!(applied.environments.is_empty(), "services-only attachments");
    let combined = applied.into_combined_job(job);
    assert_eq!(
        service_names(combined.services.as_deref().unwrap()),
        ["A0", "A1", "B", "J0", "J1"]
    );
}

#[test]
fn attached_environments_precede_the_jobs_own_and_keep_their_symtab() {
    // A pre-RFC-0009 attachment on a Job with a Job Environment of its own:
    // exactly the Environment-only behavior, unchanged.
    const JOB_WITH_ENV: &str = r#"
specificationVersion: "jobtemplate-2023-09"
name: WithEnv
jobEnvironments:
  - name: JobEnv
    variables: { FROM_JOB: "1" }
steps:
  - name: Render
    script:
      actions:
        onRun:
          command: echo
"#;
    let (job, applied) = submit_ok(JOB_WITH_ENV, &[PLAIN_ENV_TEMPLATE], &[("Studio", "pixar")]);
    assert!(applied.external_services.is_empty());
    assert_eq!(applied.environments.len(), 1);
    let env = &applied.environments[0];
    assert_eq!(env.name, "StudioSetup");
    assert!(env.run_scope.is_none(), "no runScope without SERVICE");
    // The attachment's own parameter is frozen into its resolved_symtab.
    let st = env
        .resolved_symtab
        .as_ref()
        .unwrap()
        .to_symtab(openjd_expr::path_mapping::PathFormat::Posix)
        .unwrap();
    assert_eq!(
        st.get_value("Param.Studio"),
        Some(&ExprValue::String("pixar".to_string()))
    );

    let combined = applied.into_combined_job(job);
    assert_eq!(
        combined
            .job_environments
            .as_deref()
            .unwrap()
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>(),
        ["StudioSetup", "JobEnv"]
    );
    assert!(combined.services.is_none(), "no Services anywhere");
}

#[test]
fn no_attachments_leaves_the_job_unchanged() {
    let (job, applied) = submit_ok(&RFC_VALKEY, &[], &[]);
    assert!(applied.external_services.is_empty());
    assert!(applied.environments.is_empty());
    let before = job.clone();
    assert_eq!(applied.into_combined_job(job), before);
}

#[test]
fn external_service_sees_job_name_when_its_document_declares_expr() {
    // The attachment uses `Job.Name` in its `<Service>.let`; the Job
    // Template declares no extensions at all. The attachment's own EXPR
    // declaration governs.
    const T: &str = r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, EXPR]
services:
  - name: Tagged
    let: ["tag = Job.Name + '-cache'"]
    ports: [{ name: main }]
    script:
      actions:
        onRun: { command: serve, args: ["--tag", "{{ tag }}"] }
"#;
    let (_, applied) = submit_ok(PLAIN_JOB, &[T], &[]);
    let st = applied.external_services[0]
        .resolved_symtab
        .as_ref()
        .unwrap()
        .to_symtab(openjd_expr::path_mapping::PathFormat::Posix)
        .unwrap();
    assert_eq!(
        st.get_value("tag"),
        Some(&ExprValue::String("Plain-cache".to_string()))
    );
}

// ════════════════════════════════════════════════════════════════════
// 2. Merge rule 2 — Service names are scoped to their document
// ════════════════════════════════════════════════════════════════════

#[test]
fn external_service_named_like_a_job_service_is_accepted_and_kept_distinct() {
    // The Valkey example's inline Service is named `Cache`, as is the RFC's
    // queue-cache attachment's: two distinct Services, the external one
    // first, each known by (document, name).
    let (job, applied) = submit_ok(&RFC_VALKEY, &[RFC_QUEUE_CACHE], &[]);
    assert_eq!(service_names(&applied.external_services), ["Cache"]);
    assert_eq!(
        applied.external_services[0].document,
        job::Document::environment_template(0, None)
    );
    assert_eq!(
        applied.external_services[0].document.to_string(),
        "EnvironmentTemplate[0]"
    );
    assert_eq!(
        applied.environment_documents,
        [job::Document::environment_template(0, None)]
    );

    let combined = applied.into_combined_job(job);
    let services = combined.services.as_deref().unwrap();
    assert_eq!(service_names(services), ["Cache", "Cache"]);
    assert_eq!(
        services[0].document,
        job::Document::environment_template(0, None)
    );
    assert_eq!(services[1].document, job::Document::JobTemplate);
    assert_ne!(
        services[0], services[1],
        "distinct Services, not a duplicate"
    );
    // Each resolves `Service.Cache.*` within its own document: the external
    // one is the RFC's valkey-server with `--maxmemory`, the Job Template's
    // has `--save`.
    let args = |svc: &job::Service| -> Vec<String> {
        svc.script
            .actions
            .on_run
            .args
            .as_ref()
            .unwrap()
            .iter()
            .map(|a| a.to_string())
            .collect()
    };
    assert!(args(&services[0]).iter().any(|a| a == "--maxmemory"));
    assert!(args(&services[1]).iter().any(|a| a == "--save"));
}

#[test]
fn external_service_named_like_an_inline_service_is_accepted() {
    // §1.2.2 item 3: the coordinator example's inline `Coordinator` shadows
    // an attached `Coordinator` for the Job Template's references; both run
    // and are kept distinct by their documents. The attached one precedes
    // the inline one in the combined Job and has every Step in its scope.
    let (job, applied) = submit_ok(
        &RFC_COORDINATOR,
        &[&service_only_template("Coordinator")],
        &[],
    );
    assert_eq!(service_names(&applied.external_services), ["Coordinator"]);
    assert!(applied.requirement_bindings.is_empty());
    let combined = applied.into_combined_job(job);
    let services = combined.services.as_deref().unwrap();
    assert_eq!(service_names(services), ["Coordinator", "Coordinator"]);
    assert_eq!(
        services[0].document,
        job::Document::environment_template(0, None)
    );
    assert_eq!(services[0].scope, ServiceScope::AllSteps);
    assert_eq!(services[1].document, job::Document::JobTemplate);
    assert_eq!(services[1].scope, ServiceScope::steps(["RenderTiles"]));
}

#[test]
fn external_services_named_alike_across_attachments_are_accepted() {
    // Two attachments each define `Cache`: both are kept, in attachment
    // order, each stamped with its own document.
    let (_, applied) = submit_ok(
        RFC_CONSUMER,
        &[RFC_QUEUE_CACHE, &service_only_template("Cache")],
        &[],
    );
    assert_eq!(
        service_names(&applied.external_services),
        ["Cache", "Cache"]
    );
    assert_eq!(
        applied
            .external_services
            .iter()
            .map(|s| s.document.clone())
            .collect::<Vec<_>>(),
        [
            job::Document::environment_template(0, None),
            job::Document::environment_template(1, None),
        ]
    );
}

#[test]
fn labels_name_the_documents_on_external_services() {
    let jt = decode_job(&RFC_VALKEY);
    let et = decode_env(&service_only_template("Cache"));
    let td = tempfile::TempDir::new().unwrap();
    let dir = td.path().to_str().unwrap();
    let processed = openjd_model::preprocess_job_parameters(
        &jt,
        &JobParameterInputValues::default(),
        std::slice::from_ref(&et),
        &openjd_model::PathParameterOptions::new(dir, dir),
    )
    .unwrap();
    let job = create_job(&jt, &processed, &jt.default_validation_context()).unwrap();
    let attached =
        [AttachedEnvironmentTemplate::new(&et).with_label("queue/cache.environment.yaml")];
    let applied =
        apply_environment_templates(&job, &attached, &processed, &CallerLimits::default())
            .expect("same-named Services in different documents are accepted");
    let doc = &applied.external_services[0].document;
    assert_eq!(
        *doc,
        job::Document::environment_template(0, Some("queue/cache.environment.yaml"))
    );
    assert_eq!(doc.to_string(), "queue/cache.environment.yaml");
    assert!(applied.environment_documents.is_empty(), "services-only");
}

#[test]
fn documents_of_the_combined_environments_follow_the_fold() {
    // Two attachments (the first services-only, the second with an
    // Environment) on a Job with a Job Environment of its own.
    const JOB_WITH_ENV: &str = r#"
specificationVersion: "jobtemplate-2023-09"
name: WithEnv
jobEnvironments:
  - name: JobEnv
    variables: { FROM_JOB: "1" }
steps:
  - name: Render
    script:
      actions:
        onRun:
          command: echo
"#;
    let (job, applied) = submit_ok(
        JOB_WITH_ENV,
        &[&service_only_template("Cache"), RFC_QUEUE_CACHE],
        &[],
    );
    assert_eq!(
        applied.environment_documents,
        [job::Document::environment_template(1, None)]
    );
    assert_eq!(
        applied.combined_environment_documents(&job),
        [
            job::Document::environment_template(1, None),
            job::Document::JobTemplate,
        ]
    );
    let combined = applied.into_combined_job(job);
    assert_eq!(
        combined
            .job_environments
            .as_deref()
            .unwrap()
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>(),
        ["CacheClient", "JobEnv"]
    );
}

#[test]
fn duplicates_within_one_document_are_still_rejected() {
    // The per-document rule is unchanged: a repeat within an attachment's
    // `services` list, or within a Job Template's, is a template-validation
    // error.
    let err = decode_environment_template(
        yaml_val(
            r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, EXPR]
services:
  - name: Cache
    ports: [{ name: main }]
    script: { actions: { onRun: { command: serve } } }
  - name: Cache
    ports: [{ name: main }]
    script: { actions: { onRun: { command: serve } } }
"#,
        ),
        Some(EXTS),
        &CallerLimits::default(),
    )
    .expect_err("duplicate within one list")
    .to_string();
    assert_eq!(
        err,
        "Model validation error: 1 validation error for EnvironmentTemplate\n\
         services[1]:\n\tduplicate service name: 'Cache'"
    );

    let err = decode_job_template(
        yaml_val(
            r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [SERVICE, EXPR]
name: Dup
services:
  - name: Cache
    ports: [{ name: main }]
    script: { actions: { onRun: { command: serve } } }
  - name: Cache
    ports: [{ name: main }]
    script: { actions: { onRun: { command: serve } } }
steps:
  - name: Render
    dependencies: [{ service: "Cache" }]
    script: { actions: { onRun: { command: echo } } }
"#,
        ),
        Some(EXTS),
        &CallerLimits::default(),
    )
    .expect_err("duplicate within the Job Template's services")
    .to_string();
    assert_eq!(
        err,
        "Model validation error: 1 validation error for JobTemplate\n\
         services[1]:\n\tduplicate service name: 'Cache'"
    );
}

#[test]
fn document_serializes_only_for_external_services() {
    let (job, applied) = submit_ok(&RFC_VALKEY, &[RFC_QUEUE_CACHE], &[]);
    let combined = applied.into_combined_job(job);
    let value = serde_json::to_value(&combined).unwrap();
    let services = value["services"].as_array().unwrap();
    assert_eq!(
        services[0]["document"],
        serde_json::json!({"kind": "EnvironmentTemplate", "index": 0})
    );
    assert!(
        services[1].get("document").is_none(),
        "the Job Template's own Service omits the default"
    );
    let combined_services = combined.services.as_deref().unwrap();
    for (value, expected) in services.iter().zip(combined_services) {
        let round_trip: job::Service = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(&round_trip, expected);
    }
}

// ════════════════════════════════════════════════════════════════════
// 3. Merge rule 3 — the wrapping-Environment rule
// ════════════════════════════════════════════════════════════════════

const WRAPPER_REMEDY: &str = "Declare SERVICE in {doc} and either define onWrapServiceEnter, \
    onWrapServiceRun, onWrapServiceHealthCheck, and onWrapServiceExit, or declare a runScope \
    that excludes SERVICE (RFC 0009, Template Schemas §1.2.2 item 4).";

fn wrapper_message(env: &str, doc: &str, in_scope: &str) -> String {
    format!(
        "wrapping Environment '{env}' is defined by {doc}, which does not declare the SERVICE \
         extension, so it has the default runScope (every kind of Session) and cannot define the \
         onWrapService* hooks; but the combined Job places {in_scope} in its scope, and the \
         Service would run in a Session the Environment enters but cannot wrap. {}",
        WRAPPER_REMEDY.replace("{doc}", doc)
    )
}

#[test]
fn queue_wrapper_attached_to_a_job_declaring_a_job_service_is_rejected() {
    // Direction 1: a queue's RFC 0008 wrapper template, a Job Template with
    // `services`.
    let msg = submit_err(&RFC_VALKEY, &[WRAPPER_ENV_TEMPLATE], &[]);
    assert_eq!(
        msg,
        format!(
            "Model validation error: 1 validation error for Submission\n\
             EnvironmentTemplate[0] -> environment:\n\t{}",
            wrapper_message(
                "QueueContainer",
                "EnvironmentTemplate[0]",
                "Service 'Cache' (JobTemplate -> services[0])"
            )
        )
    );
}

#[test]
fn queue_wrapper_attached_to_a_job_whose_service_is_scoped_to_one_step_is_rejected() {
    // A Job Environment is entered by every Service Session, including that
    // of a Service whose scope is one Step (the coordinator example).
    let msg = submit_err(&RFC_COORDINATOR, &[WRAPPER_ENV_TEMPLATE], &[]);
    assert_eq!(
        msg,
        format!(
            "Model validation error: 1 validation error for Submission\n\
             EnvironmentTemplate[0] -> environment:\n\t{}",
            wrapper_message(
                "QueueContainer",
                "EnvironmentTemplate[0]",
                "Service 'Coordinator' (JobTemplate -> services[0])"
            )
        )
    );
}

#[test]
fn queue_wrapper_attached_alongside_another_attachments_service_is_rejected() {
    // Both documents come from the queue; the Job Template declares nothing.
    let msg = submit_err(PLAIN_JOB, &[WRAPPER_ENV_TEMPLATE, RFC_QUEUE_CACHE], &[]);
    assert_eq!(
        msg,
        format!(
            "Model validation error: 1 validation error for Submission\n\
             EnvironmentTemplate[0] -> environment:\n\t{}",
            wrapper_message(
                "QueueContainer",
                "EnvironmentTemplate[0]",
                "external Service 'Cache' (EnvironmentTemplate[1] -> services[0])"
            )
        )
    );
}

#[test]
fn job_template_wrapper_submitted_to_a_queue_attaching_a_service_is_rejected() {
    // Direction 2: a Job Template with an RFC 0008 wrapper Job Environment,
    // a queue that attaches a Service.
    let msg = submit_err(WRAPPER_JOB, &[RFC_QUEUE_CACHE], &[]);
    assert_eq!(
        msg,
        format!(
            "Model validation error: 1 validation error for Submission\n\
             JobTemplate -> jobEnvironments[0]:\n\t{}",
            wrapper_message(
                "Container",
                "the Job Template",
                "external Service 'Cache' (EnvironmentTemplate[0] -> services[0])"
            )
        )
    );
}

#[test]
fn job_template_step_wrapper_is_not_in_scope_of_job_services() {
    // §1.2.2 item 4, last sentence: a Service Session enters only
    // `jobEnvironments`, so a wrapping Step Environment is never entered by
    // one and is not subject to the check, whatever the Service's scope.
    let (_job, applied) = submit_ok(STEP_WRAPPER_JOB, &[RFC_QUEUE_CACHE], &[]);
    assert_eq!(applied.external_services.len(), 1);
}

#[test]
fn wrapper_declaring_service_with_the_four_hooks_is_accepted() {
    let (job, applied) = submit_ok(&RFC_VALKEY, &[SERVICE_AWARE_WRAPPER_ENV_TEMPLATE], &[]);
    assert_eq!(applied.environments.len(), 1);
    assert!(applied.environments[0]
        .script
        .as_ref()
        .unwrap()
        .actions
        .has_any_service_wrap_hook());
    assert_eq!(
        service_names(applied.into_combined_job(job).services.as_deref().unwrap()),
        ["Cache"]
    );
}

#[test]
fn wrapper_declaring_service_with_task_run_scope_is_accepted() {
    let (_, applied) = submit_ok(&RFC_COORDINATOR, &[TASK_ONLY_WRAPPER_ENV_TEMPLATE], &[]);
    assert_eq!(applied.environments.len(), 1);
    assert_eq!(
        applied.environments[0].run_scope,
        Some(vec![RunScope::Task])
    );
}

#[test]
fn wrapper_without_any_service_in_scope_is_accepted() {
    // Both directions, with nothing to wrap: a SERVICE-less wrapper
    // attachment on a plain Job, and a SERVICE-less wrapper Job Template
    // with a plain attachment. Exactly the pre-RFC-0009 behavior.
    let (_, applied) = submit_ok(PLAIN_JOB, &[WRAPPER_ENV_TEMPLATE], &[]);
    assert_eq!(applied.environments.len(), 1);
    let (_, applied) = submit_ok(WRAPPER_JOB, &[PLAIN_ENV_TEMPLATE], &[]);
    assert_eq!(applied.environments.len(), 1);
    let (_, applied) = submit_ok(STEP_WRAPPER_JOB, &[PLAIN_ENV_TEMPLATE], &[]);
    assert_eq!(applied.environments.len(), 1);
}

#[test]
fn job_template_wrapper_in_a_service_declaring_job_is_not_this_check() {
    // A Job Template that declares SERVICE is subject to template
    // validation's hooks-follow-runScope rule (pass 10), not the
    // submission-time rule: with `runScope: [TASK]` it validates and the
    // submission with an attached Service succeeds.
    const T: &str = r#"
specificationVersion: "jobtemplate-2023-09"
name: Wrapped
extensions: [WRAP_ACTIONS, EXPR, SERVICE]
jobEnvironments:
  - name: Container
    runScope: [TASK]
    script:
      actions:
        onWrapEnvEnter: { command: run-in-container, args: ["{{ WrappedAction.Command }}"] }
        onWrapTaskRun: { command: run-in-container, args: ["{{ WrappedAction.Command }}"] }
        onWrapEnvExit: { command: run-in-container, args: ["{{ WrappedAction.Command }}"] }
steps:
  - name: Render
    script:
      actions:
        onRun:
          command: echo
"#;
    let (job, applied) = submit_ok(T, &[RFC_QUEUE_CACHE], &[]);
    let combined = applied.into_combined_job(job);
    assert_eq!(
        service_names(combined.services.as_deref().unwrap()),
        ["Cache"]
    );
}

#[test]
fn same_named_service_does_not_mask_a_wrapper_violation() {
    // The wrapper attachment is still rejected when the other attachment's
    // `Cache` is named like the Valkey Job's: exactly one error, the
    // wrapper's; the name is not one.
    let msg = submit_err(
        &RFC_VALKEY,
        &[WRAPPER_ENV_TEMPLATE, &service_only_template("Cache")],
        &[],
    );
    assert!(
        msg.starts_with("Model validation error: 1 validation error for Submission\n"),
        "{msg}"
    );
    assert!(
        msg.contains(
            "EnvironmentTemplate[0] -> environment:\n\twrapping Environment 'QueueContainer'"
        ),
        "{msg}"
    );
    assert!(!msg.contains("has the same name"), "{msg}");
}

// ════════════════════════════════════════════════════════════════════
// 4. Scope
// ════════════════════════════════════════════════════════════════════

#[test]
fn external_service_sees_other_services_of_its_own_document() {
    // `Second` lists `service:First` and references it; `First` lists
    // `service:Third`, declared after it (the order carries no meaning), and
    // references it. The declared dependencies are carried forward and the
    // listed Services' symbols seeded for the re-check. Every external
    // Service has every Step in its scope (§1.2.2 item 1), dependencies or
    // not.
    const T: &str = r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, EXPR]
services:
  - name: First
    dependencies: [{ service: "Third" }]
    ports: [{ name: main }]
    script:
      actions:
        onRun: { command: serve, args: ["{{ Service.Third.main.port }}"] }
  - name: Second
    dependencies: [{ service: "First" }]
    ports: [{ name: main }]
    script:
      actions:
        onRun: { command: proxy, args: ["{{ Service.First.main.connectAddress }}:{{ Service.First.main.port }}"] }
  - name: Third
    ports: [{ name: main }]
    script:
      actions:
        onRun: { command: serve }
"#;
    let (_, applied) = submit_ok(PLAIN_JOB, &[T], &[]);
    assert_eq!(
        service_names(&applied.external_services),
        ["First", "Second", "Third"]
    );
    for svc in &applied.external_services {
        assert_eq!(svc.scope, ServiceScope::AllSteps, "{}", svc.name);
    }
    assert_eq!(
        applied.external_services[0]
            .depends_on_services()
            .collect::<Vec<_>>(),
        vec!["Third"]
    );
    assert_eq!(
        applied.external_services[1]
            .depends_on_services()
            .collect::<Vec<_>>(),
        vec!["First"]
    );
    assert!(applied.external_services[2]
        .depends_on_services()
        .next()
        .is_none());
}

#[test]
fn attachment_cannot_reference_another_attachments_service() {
    // Template validation: `Service.*` must resolve within the document.
    let err = decode_environment_template(
        yaml_val(
            r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, EXPR]
environment:
  name: UsesCache
  runScope: [TASK]
  variables:
    VALKEY_PORT: "{{ Service.Cache.main.port }}"
"#,
        ),
        Some(EXTS),
        &CallerLimits::default(),
    )
    .expect_err("Cache is not declared in this document");
    let msg = err.to_string();
    assert!(
        msg.contains("environment -> variables -> VALKEY_PORT:") && msg.contains("Service.Cache"),
        "{msg}"
    );
}

#[test]
fn attachment_service_cannot_depend_on_another_attachments_service() {
    // `service:<name>` in an Environment Template names a Service of the
    // same document's `services` only (Template Schemas §9 item 4).
    let err = decode_environment_template(
        yaml_val(
            r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, EXPR]
services:
  - name: Proxy
    dependencies: [{ service: "Cache" }]
    ports: [{ name: main }]
    script: { actions: { onRun: { command: proxy } } }
"#,
        ),
        Some(EXTS),
        &CallerLimits::default(),
    )
    .expect_err("Cache is not declared in this document")
    .to_string();
    assert_eq!(
        err,
        "Model validation error: 1 validation error for EnvironmentTemplate\n\
         services[0] -> dependencies[0]:\n\tdependency 'service: Cache' not found: no Service of \
         that name in this document's services."
    );
}

#[test]
fn attachment_service_referencing_a_sibling_without_listing_it_is_rejected() {
    // An external Service sees another Service of its document only when it
    // lists it; every Step being in its scope does not change that.
    let err = decode_environment_template(
        yaml_val(
            r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, EXPR]
services:
  - name: Back
    ports: [{ name: main }]
    script: { actions: { onRun: { command: serve } } }
  - name: Front
    ports: [{ name: main }]
    script: { actions: { onRun: { command: proxy, args: ["{{ Service.Back.main.port }}"] } } }
"#,
        ),
        Some(EXTS),
        &CallerLimits::default(),
    )
    .expect_err("Front does not list service:Back")
    .to_string();
    assert!(
        err.starts_with(
            "Model validation error: 1 validation error for EnvironmentTemplate\n\
             services[1] -> script -> actions -> onRun -> args[0]:\n\t\
             Failed to parse interpolation expression at [0, 28]. Service 'Front' references \
             Service.Back.main.port but does not list service: Back in dependencies.\n"
        ),
        "{err}"
    );
}

#[test]
fn unlisted_attachment_service_still_has_every_step_in_scope() {
    // Environment Template Services are exempt from the "unused" rule: no
    // Step of the Job lists them, and every Step is in their scope.
    let (_, applied) = submit_ok(&RFC_COORDINATOR, &[&service_only_template("Metrics")], &[]);
    let metrics = &applied.external_services[0];
    assert_eq!(metrics.scope, ServiceScope::AllSteps);
    assert!(metrics.dependencies.is_none());
}

#[test]
fn job_template_cannot_reference_an_external_service() {
    // Template validation: a Job Template's `Service.*` references resolve
    // to the Services it declares itself, so a Task cannot name `Cache`.
    let err = decode_job_template(
        yaml_val(
            r#"
specificationVersion: "jobtemplate-2023-09"
name: UsesQueueCache
extensions: [SERVICE, EXPR]
steps:
  - name: Render
    script:
      actions:
        onRun: { command: render, args: ["{{ Service.Cache.main.port }}"] }
"#,
        ),
        Some(EXTS),
        &CallerLimits::default(),
    )
    .expect_err("Cache is not declared in the Job Template");
    let msg = err.to_string();
    assert!(
        msg.contains("steps[0] -> script -> actions -> onRun -> args[0]:")
            && msg.contains("Service.Cache"),
        "{msg}"
    );
}

#[test]
fn attached_environment_sees_its_own_documents_services_for_the_recheck() {
    // The RFC attachment's `CacheClient` references `Service.Cache.*`; the
    // carried-forward re-check must seed that symbol (runScope excludes
    // SERVICE), or the re-check would fail. Covered by the success of the
    // RFC example; pinned here with the variables carried as FormatStrings.
    let (_, applied) = submit_ok(RFC_CONSUMER, &[RFC_QUEUE_CACHE], &[]);
    let vars = applied.environments[0].variables.as_ref().unwrap();
    assert_eq!(vars["VALKEY_PORT"].raw(), "{{ Service.Cache.main.port }}");
}

// ════════════════════════════════════════════════════════════════════
// 5. Per-document cap
// ════════════════════════════════════════════════════════════════════

#[test]
fn the_ten_service_cap_is_per_document_not_on_the_combined_list() {
    // Two attachments of 10 Services each plus a Job Template with 10 inline
    // Services: 30 Services in the combined list, each document at the cap.
    let a = many_services_template("A", 10);
    let b = many_services_template("B", 10);
    let (job, applied) = submit_ok(&many_job_services_job("J", 10), &[&a, &b], &[]);
    assert_eq!(applied.external_services.len(), 20);
    let combined = applied.into_combined_job(job);
    let names = service_names(combined.services.as_deref().unwrap());
    assert_eq!(names.len(), 30);
    assert_eq!(names[0], "A0");
    assert_eq!(names[10], "B0");
    assert_eq!(names[20], "J0");
    assert_eq!(names[29], "J9");

    // Eleven in one document is still rejected — at template validation.
    let err = decode_environment_template(
        yaml_val(&many_services_template("C", 11)),
        Some(EXTS),
        &CallerLimits::default(),
    )
    .expect_err("11 Services in one document");
    assert!(
        err.to_string()
            .contains("services:\n\tmust not contain more than 10 elements."),
        "{err}"
    );
}

// ════════════════════════════════════════════════════════════════════
// 6. Errors from inside a document carry the document
// ════════════════════════════════════════════════════════════════════

#[test]
fn external_service_numeric_field_error_names_the_document() {
    // The port resolves out of range only once the parameter is bound.
    const T: &str = r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, EXPR]
parameterDefinitions:
  - name: Port
    type: INT
    default: 70000
services:
  - name: Svc
    ports: [{ name: main, port: "{{ Param.Port }}" }]
    script:
      actions:
        onRun: { command: serve }
"#;
    let msg = submit_err(PLAIN_JOB, &[T], &[]);
    assert_eq!(
        msg,
        "Model validation error: 1 validation error for Submission\n\
         EnvironmentTemplate[0] -> services[0] -> ports[0] -> port:\n\
         \tmust be between 1 and 65535."
    );
}

#[test]
fn external_service_let_error_names_the_document() {
    const T: &str = r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, EXPR]
parameterDefinitions:
  - name: Divisor
    type: INT
    default: 0
services:
  - name: Svc
    let: ["share = 100 // Param.Divisor"]
    ports: [{ name: main }]
    script:
      actions:
        onRun: { command: serve, args: ["{{ share }}"] }
"#;
    let msg = submit_err(PLAIN_JOB, &[T], &[]);
    assert!(
        msg.starts_with("Expression error: EnvironmentTemplate[0]: service let binding 'share': "),
        "{msg}"
    );
}

#[test]
fn attached_environment_variable_recheck_names_the_document() {
    // The variable's resolved length exceeds the §4.4.2 bound once the
    // parameter is bound; the Job Template is fine.
    let long = "x".repeat(2049);
    const T: &str = r#"
specificationVersion: "environment-2023-09"
parameterDefinitions:
  - name: Big
    type: STRING
environment:
  name: Big
  variables:
    BIG: "{{ Param.Big }}"
"#;
    let msg = submit_err(PLAIN_JOB, &[T], &[("Big", &long)]);
    assert!(
        msg.starts_with(
            "Model validation error: 1 validation error for Submission\n\
             EnvironmentTemplate[0] -> environment -> variables -> BIG:\n\t"
        ),
        "{msg}"
    );
    assert!(msg.contains("2048"), "{msg}");
}

#[test]
fn attached_environment_template_profile_reflects_its_own_extensions() {
    let et = decode_env(RFC_QUEUE_CACHE);
    let profile = et.profile();
    assert!(profile.has_extension(openjd_model::ModelExtension::Service));
    assert!(profile.has_extension(openjd_model::ModelExtension::Expr));
    assert!(profile.has_extension(openjd_model::ModelExtension::FeatureBundle1));
    assert!(!profile.has_extension(openjd_model::ModelExtension::WrapActions));
    assert_eq!(
        profile.revision(),
        openjd_model::SpecificationRevision::V2023_09
    );
    let plain = decode_env(PLAIN_ENV_TEMPLATE);
    assert!(plain.profile().extensions().is_empty());
    assert_eq!(
        plain.default_validation_context().profile.revision(),
        openjd_model::SpecificationRevision::V2023_09
    );
}
