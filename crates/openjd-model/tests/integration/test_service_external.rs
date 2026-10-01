// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Integration tests for applying Environment Templates to a Job under RFC
//! 0009 (Template Schemas §1.2.2 "Services from Environment Templates" and
//! §9.7's two submission-time checks) via `apply_environment_templates`:
//!
//! 1. **Merge rule 1** — the RFC's queue-cache example: the external
//!    `Cache` Service is instantiated first, before the Job Template's own
//!    `jobServices`, with the attachment's parameters and `Job.Name` in
//!    scope; attached Environments precede the Job's own; a submission with
//!    several attachments orders Services by attachment then `services`
//!    order; Environment-only attachments behave as before RFC 0009.
//! 2. **Merge rule 2** — name collisions with a Job Service, a Step Service,
//!    and another attachment, each naming both sources.
//! 3. **Merge rule 3** — the wrapping-Environment rule in both directions
//!    (a SERVICE-less wrapper attachment with a Job that declares Services;
//!    a SERVICE-less Job Template wrapper, job- and step-level, with an
//!    attachment that defines a Service), the non-rejection when the
//!    wrapper declares `SERVICE` with the four hooks or with `runScope:
//!    [TASK]`, and the non-rejection when nothing places a Service in scope.
//! 4. **Scope** — an external Service sees only earlier Services of its own
//!    document; a Job Template cannot reference an external Service by name
//!    (template validation); an attachment cannot reference another
//!    attachment's Service (template validation).
//! 5. **Per-document cap** — three documents of 10 Services each combine to
//!    30; the cap is per document, not on the combined list.
//! 6. **Errors from inside a document** carry the document in their path or
//!    message, and labels replace the positional document name.

use openjd_expr::ExprValue;
use openjd_model::job::{self, CompletedTasksPolicy, RunScope};
use openjd_model::template::EnvironmentTemplate;
use openjd_model::{
    apply_environment_templates, create_job, decode_environment_template, decode_job_template,
    AppliedEnvironmentTemplates, AttachedEnvironmentTemplate, CallerLimits,
    JobParameterInputValues, ModelError,
};

const EXTS: &[&str] = &["EXPR", "SERVICE", "FEATURE_BUNDLE_1", "WRAP_ACTIONS"];

const RFC_QUEUE_CACHE: &str = include_str!("../fixtures/rfc0009/queue-cache.environment.yaml");
const RFC_CONSUMER: &str = include_str!("../fixtures/rfc0009/queue-cache-consumer.job.yaml");
const RFC_VALKEY: &str = include_str!("../fixtures/rfc0009/valkey-shared-store.job.yaml");
const RFC_COORDINATOR: &str = include_str!("../fixtures/rfc0009/per-step-coordinator.job.yaml");

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
      onWrapServiceReadinessCheck: { command: run-in-container, args: ["{{ WrappedAction.Command }}"] }
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

/// A Job Template with `n` Job Services named `<prefix>0..n`.
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
    format!(
        "specificationVersion: \"jobtemplate-2023-09\"\nname: Many\nextensions: [SERVICE, EXPR]\njobServices:\n{}\nsteps:\n  - name: S\n    script:\n      actions:\n        onRun:\n          command: echo\n",
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
    assert!(
        job.job_services.is_none(),
        "the consumer declares no Services"
    );
    assert_eq!(service_names(&applied.external_services), ["Cache"]);

    let cache = &applied.external_services[0];
    assert_eq!(
        cache.description.as_deref(),
        Some("A per-Job Valkey store shared by all of the Job's Tasks.")
    );
    assert_eq!(cache.ports.len(), 1);
    assert_eq!(cache.ports[0].name, "main");
    assert_eq!(cache.restart_policy.max_attempts, 3);
    assert_eq!(
        cache.restart_policy.completed_tasks,
        CompletedTasksPolicy::Keep
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

    // Folding into the Job: the external Service becomes jobServices[0] and
    // the attached Environment jobEnvironments[0].
    let combined = applied.into_combined_job(job);
    assert_eq!(
        service_names(combined.job_services.as_deref().unwrap()),
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
fn external_services_precede_the_job_templates_own_job_services() {
    // The Valkey example declares a Job Service `Cache`; the queue attaches
    // `Metrics`. The combined list is `[Metrics, Cache]`.
    let (job, applied) = submit_ok(RFC_VALKEY, &[&service_only_template("Metrics")], &[]);
    assert_eq!(
        service_names(job.job_services.as_deref().unwrap()),
        ["Cache"]
    );
    assert_eq!(service_names(&applied.external_services), ["Metrics"]);
    let combined = applied.into_combined_job(job);
    assert_eq!(
        service_names(combined.job_services.as_deref().unwrap()),
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
        service_names(combined.job_services.as_deref().unwrap()),
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
    assert!(combined.job_services.is_none(), "no Services anywhere");
}

#[test]
fn no_attachments_leaves_the_job_unchanged() {
    let (job, applied) = submit_ok(RFC_VALKEY, &[], &[]);
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
// 2. Merge rule 2 — name collisions
// ════════════════════════════════════════════════════════════════════

#[test]
fn external_service_colliding_with_a_job_service_is_rejected() {
    // The Valkey example's Job Service is named `Cache`, as is the RFC's
    // queue-cache attachment's — the collision the spec warns about.
    let msg = submit_err(RFC_VALKEY, &[RFC_QUEUE_CACHE], &[]);
    assert_eq!(
        msg,
        "Model validation error: 1 validation error for Submission\n\
         EnvironmentTemplate[0] -> services[0]:\n\
         \texternal Service 'Cache' (EnvironmentTemplate[0] -> services[0]) has the same name as \
         Service 'Cache' (JobTemplate -> jobServices[0]); the name of an external Service must \
         not equal the name of any other external Service, nor of any Service in the Job \
         Template's jobServices or any Step's stepServices (RFC 0009, Template Schemas §1.2.2 \
         item 2)."
    );
}

#[test]
fn external_service_colliding_with_a_step_service_is_rejected() {
    // The coordinator example's Step Service is `Coordinator` on steps[0].
    let msg = submit_err(
        RFC_COORDINATOR,
        &[&service_only_template("Coordinator")],
        &[],
    );
    assert_eq!(
        msg,
        "Model validation error: 1 validation error for Submission\n\
         EnvironmentTemplate[0] -> services[0]:\n\
         \texternal Service 'Coordinator' (EnvironmentTemplate[0] -> services[0]) has the same \
         name as Service 'Coordinator' (JobTemplate -> steps[0] -> stepServices[0]); the name of \
         an external Service must not equal the name of any other external Service, nor of any \
         Service in the Job Template's jobServices or any Step's stepServices (RFC 0009, Template \
         Schemas §1.2.2 item 2)."
    );
}

#[test]
fn external_services_colliding_across_attachments_are_rejected() {
    // Two attachments each define `Cache`; the second is the one reported,
    // against the first.
    let msg = submit_err(
        RFC_CONSUMER,
        &[RFC_QUEUE_CACHE, &service_only_template("Cache")],
        &[],
    );
    assert_eq!(
        msg,
        "Model validation error: 1 validation error for Submission\n\
         EnvironmentTemplate[1] -> services[0]:\n\
         \texternal Service 'Cache' (EnvironmentTemplate[1] -> services[0]) has the same name as \
         external Service 'Cache' (EnvironmentTemplate[0] -> services[0]); the name of an \
         external Service must not equal the name of any other external Service, nor of any \
         Service in the Job Template's jobServices or any Step's stepServices (RFC 0009, Template \
         Schemas §1.2.2 item 2)."
    );
}

#[test]
fn every_collision_is_reported_at_once() {
    // `Cache` collides with the Valkey Job Service, and the two attachments
    // collide with each other on `Dup0`: two errors in one result.
    let first = many_services_template("Dup", 1); // Dup0
    const SECOND: &str = r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, EXPR]
services:
  - name: Cache
    ports: [{ name: main }]
    script:
      actions:
        onRun: { command: serve }
  - name: Dup0
    ports: [{ name: main }]
    script:
      actions:
        onRun: { command: serve }
"#;
    let msg = submit_err(RFC_VALKEY, &[&first, SECOND], &[]);
    assert!(
        msg.starts_with("Model validation error: 2 validation errors for Submission\n"),
        "{msg}"
    );
    assert!(
        msg.contains(
            "EnvironmentTemplate[1] -> services[0]:\n\texternal Service 'Cache' \
             (EnvironmentTemplate[1] -> services[0]) has the same name as Service 'Cache' \
             (JobTemplate -> jobServices[0]);"
        ),
        "{msg}"
    );
    assert!(
        msg.contains(
            "EnvironmentTemplate[1] -> services[1]:\n\texternal Service 'Dup0' \
             (EnvironmentTemplate[1] -> services[1]) has the same name as external Service 'Dup0' \
             (EnvironmentTemplate[0] -> services[0]);"
        ),
        "{msg}"
    );
}

#[test]
fn labels_name_the_documents_in_collision_messages() {
    let jt = decode_job(RFC_VALKEY);
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
    let msg = apply_environment_templates(&job, &attached, &processed, &CallerLimits::default())
        .expect_err("collision")
        .to_string();
    assert_eq!(
        msg,
        "Model validation error: 1 validation error for Submission\n\
         queue/cache.environment.yaml -> services[0]:\n\
         \texternal Service 'Cache' (queue/cache.environment.yaml -> services[0]) has the same \
         name as Service 'Cache' (JobTemplate -> jobServices[0]); the name of an external Service \
         must not equal the name of any other external Service, nor of any Service in the Job \
         Template's jobServices or any Step's stepServices (RFC 0009, Template Schemas §1.2.2 \
         item 2)."
    );
}

// ════════════════════════════════════════════════════════════════════
// 3. Merge rule 3 — the wrapping-Environment rule
// ════════════════════════════════════════════════════════════════════

const WRAPPER_REMEDY: &str = "Declare SERVICE in {doc} and either define onWrapServiceEnter, \
    onWrapServiceRun, onWrapServiceReadinessCheck, and onWrapServiceExit, or declare a runScope \
    that excludes SERVICE (RFC 0009, Template Schemas §1.2.2 item 3).";

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
    // `jobServices`.
    let msg = submit_err(RFC_VALKEY, &[WRAPPER_ENV_TEMPLATE], &[]);
    assert_eq!(
        msg,
        format!(
            "Model validation error: 1 validation error for Submission\n\
             EnvironmentTemplate[0] -> environment:\n\t{}",
            wrapper_message(
                "QueueContainer",
                "EnvironmentTemplate[0]",
                "Service 'Cache' (JobTemplate -> jobServices[0])"
            )
        )
    );
}

#[test]
fn queue_wrapper_attached_to_a_job_declaring_only_step_services_is_rejected() {
    // A Job Environment is entered by every Service Session, including a
    // Step Service's.
    let msg = submit_err(RFC_COORDINATOR, &[WRAPPER_ENV_TEMPLATE], &[]);
    assert_eq!(
        msg,
        format!(
            "Model validation error: 1 validation error for Submission\n\
             EnvironmentTemplate[0] -> environment:\n\t{}",
            wrapper_message(
                "QueueContainer",
                "EnvironmentTemplate[0]",
                "Service 'Coordinator' (JobTemplate -> steps[0] -> stepServices[0])"
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
    // A Job Service's Session enters only `jobEnvironments`, so a wrapping
    // Step Environment is never entered for an external (Job-scoped)
    // Service and the submission is accepted. Only a Step Service of that
    // Step would put the wrapper in a Service Session.
    let (_job, applied) = submit_ok(STEP_WRAPPER_JOB, &[RFC_QUEUE_CACHE], &[]);
    assert_eq!(applied.external_services.len(), 1);
}

#[test]
fn wrapper_declaring_service_with_the_four_hooks_is_accepted() {
    let (job, applied) = submit_ok(RFC_VALKEY, &[SERVICE_AWARE_WRAPPER_ENV_TEMPLATE], &[]);
    assert_eq!(applied.environments.len(), 1);
    assert!(applied.environments[0]
        .script
        .as_ref()
        .unwrap()
        .actions
        .has_any_service_wrap_hook());
    assert_eq!(
        service_names(
            applied
                .into_combined_job(job)
                .job_services
                .as_deref()
                .unwrap()
        ),
        ["Cache"]
    );
}

#[test]
fn wrapper_declaring_service_with_task_run_scope_is_accepted() {
    let (_, applied) = submit_ok(RFC_COORDINATOR, &[TASK_ONLY_WRAPPER_ENV_TEMPLATE], &[]);
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
        service_names(combined.job_services.as_deref().unwrap()),
        ["Cache"]
    );
}

#[test]
fn collision_and_wrapper_violations_are_reported_together() {
    // One result, both checks: the wrapper attachment and a colliding
    // `Cache` attachment on the Valkey Job.
    let msg = submit_err(
        RFC_VALKEY,
        &[WRAPPER_ENV_TEMPLATE, &service_only_template("Cache")],
        &[],
    );
    assert!(
        msg.starts_with("Model validation error: 2 validation errors for Submission\n"),
        "{msg}"
    );
    assert!(
        msg.contains("EnvironmentTemplate[1] -> services[0]:\n\texternal Service 'Cache'"),
        "{msg}"
    );
    assert!(
        msg.contains(
            "EnvironmentTemplate[0] -> environment:\n\twrapping Environment 'QueueContainer'"
        ),
        "{msg}"
    );
}

// ════════════════════════════════════════════════════════════════════
// 4. Scope
// ════════════════════════════════════════════════════════════════════

#[test]
fn external_service_sees_earlier_services_of_its_own_document() {
    // `Second` references `First` from the same `services` list; the
    // reference is carried forward and its symbol is seeded for the re-check.
    const T: &str = r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, EXPR]
services:
  - name: First
    ports: [{ name: main }]
    script:
      actions:
        onRun: { command: serve }
  - name: Second
    ports: [{ name: main }]
    script:
      actions:
        onRun: { command: proxy, args: ["{{ Service.First.main.connectAddress }}:{{ Service.First.main.port }}"] }
"#;
    let (_, applied) = submit_ok(PLAIN_JOB, &[T], &[]);
    assert_eq!(
        service_names(&applied.external_services),
        ["First", "Second"]
    );
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
    // Two attachments of 10 Services each plus a Job Template with 10 Job
    // Services: 30 Services in the combined list, each document at the cap.
    let a = many_services_template("A", 10);
    let b = many_services_template("B", 10);
    let (job, applied) = submit_ok(&many_job_services_job("J", 10), &[&a, &b], &[]);
    assert_eq!(applied.external_services.len(), 20);
    let combined = applied.into_combined_job(job);
    let names = service_names(combined.job_services.as_deref().unwrap());
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
