// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Integration tests for applying Environment Templates to a Job under RFC
//! 0009 (Template Schemas §1.2.2 "Services from Environment Templates" and
//! §9.7's submission-time check) via `create_job_with_environment_templates`:
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
//! 3. **§1.2.2 item 4** — the wrapping-Environment rule: a SERVICE-less
//!    wrapper attachment with a Job that declares Services, or with a later
//!    attachment that defines one, is rejected; a Job Template Job-level
//!    wrapper is not entered by an attachment's Service (an external Service
//!    never enters the Job Template's `jobEnvironments`), so it is not
//!    subject to the rule; its non-application to Step Environments, the
//!    non-rejection when the wrapper declares `SERVICE` with the four hooks
//!    or is `ON_REQUEST` and requested by no Service, the rejection of a
//!    `SERVICE`-declaring three-hook wrapper a Service enters, and the
//!    non-rejection when nothing places a Service in scope.
//! 4. **Scope** — an external Service's scope is computed over the combined
//!    Job by the §9.1 rules (its own document's Environment, a bound
//!    `externalServices` entry, or another Service of its document that lists
//!    it); one nothing lists has an empty scope, is accepted, and is never
//!    started. It sees another Service of its own document only when it
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

use crate::service_common::{self, Applied};
use openjd_expr::ExprValue;
use openjd_model::job::{self, CompletedTasksPolicy, ServiceScope};
use openjd_model::template::EnvironmentTemplate;
use openjd_model::{
    create_job, decode_environment_template, decode_job_template, AttachedEnvironmentTemplate,
    CallerLimits, JobParameterInputValues, ModelError,
};

const EXTS: &[&str] = &[
    "EXPR",
    "ENV_PARAMS",
    "SERVICE",
    "FEATURE_BUNDLE_1",
    "WRAP_ACTIONS",
];

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
    // Match without the newline so a trailing comment on the entry still counts.
    if template.contains(entry.trim()) {
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

/// `create_job` for the Job Template alone, then the submission through
/// `create_job_with_environment_templates` with the attachments in order.
/// Returns the Job Template's own Job and the application.
fn submit(
    job_template: &str,
    attachments: &[&str],
    params: &[(&str, &str)],
) -> Result<(job::Job, Applied), ModelError> {
    let jt = decode_job(job_template);
    let ets: Vec<EnvironmentTemplate> = attachments.iter().map(|t| decode_env(t)).collect();
    submit_decoded(&jt, &ets, params)
}

fn submit_decoded(
    jt: &openjd_model::template::JobTemplate,
    ets: &[EnvironmentTemplate],
    params: &[(&str, &str)],
) -> Result<(job::Job, Applied), ModelError> {
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
    let applied = service_common::apply(jt, &attached, &processed)?;
    Ok((job, applied))
}

fn submit_ok(
    job_template: &str,
    attachments: &[&str],
    params: &[(&str, &str)],
) -> (job::Job, Applied) {
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

/// A Job Template whose one Step requests the attached Environment
/// `Client` with an `environment:` entry (RFC 0010). The entry is not
/// `required`, so the Job Template is also creatable on its own, as
/// [`submit`] does first.
const REQUESTING_JOB: &str = r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [ENV_PARAMS, EXPR]
name: Requesting
steps:
  - name: Render
    dependencies: [{ environment: Client, required: false }]
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
extensions: [WRAP_ACTIONS, ENV_PARAMS, EXPR, SERVICE]
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
extensions: [WRAP_ACTIONS, ENV_PARAMS, EXPR, SERVICE]
environment:
  name: QueueContainer
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
extensions: [SERVICE, ENV_PARAMS, EXPR]
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
        "specificationVersion: \"environment-2023-09\"\nextensions: [SERVICE, ENV_PARAMS, EXPR]\nservices:\n{}\n",
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
        "specificationVersion: \"jobtemplate-2023-09\"\nname: Many\nextensions: [SERVICE, ENV_PARAMS, EXPR]\nservices:\n{}\nsteps:\n  - name: S\n    dependencies:\n{dependencies}    script:\n      actions:\n        onRun:\n          command: echo\n",
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
    // The attachment's own Environment lists `Cache` (§9.1 rule 3), so every
    // Step is in its scope; it is stamped with its document.
    assert_eq!(cache.scope, ServiceScope::AllSteps);
    assert_eq!(cache.document, job::Document::environment_template(0, None));
    assert!(applied.requirement_bindings.is_empty());
    assert_eq!(
        cache.description.as_deref(),
        Some("A per-Job Valkey store shared by all of the Job's Tasks.")
    );
    assert_eq!(cache.ports.len(), 1);
    assert_eq!(cache.ports[0].name, "main");
    assert_eq!(cache.restart_policy.max_restarts, 3);
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

    // The attachment's Environment is converted; it lists the Service, so
    // the Service's Sessions do not enter it (RFC 0009 "Services run inside
    // Environments").
    assert_eq!(applied.environments.len(), 1);
    let client = &applied.environments[0];
    assert_eq!(client.name, "CacheClient");
    assert!(client.depends_on_service("Cache"));
    assert!(!cache.enters_environment(&client_document(), "CacheClient"));

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
    // The RFC's queue-cache example has a literal `min`; this variant
    // computes it from the attachment's parameter (a format string there
    // needs FEATURE_BUNDLE_1), so the value shows which one was used.
    let template = RFC_QUEUE_CACHE
        .replace("min: 8192", "min: \"{{ Param.CacheMemoryMiB }}\"")
        .replace(
            "extensions: [SERVICE, ENV_PARAMS, EXPR]",
            "extensions: [SERVICE, ENV_PARAMS, EXPR, FEATURE_BUNDLE_1]",
        );
    assert_ne!(template, RFC_QUEUE_CACHE);
    let (_, applied) = submit_ok(RFC_CONSUMER, &[&template], &[("CacheMemoryMiB", "1024")]);
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
extensions: [SERVICE, ENV_PARAMS, EXPR]
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
    // §1.2.2 item 3: the coordinator example's inline `Coordinator` and an
    // attached `Coordinator` are two Services; the Job Template's references
    // mean its own, and the two are kept distinct by their documents. The attached one precedes the
    // inline one in the combined Job. Nothing lists the attached one — the
    // Step's `service: Coordinator` names the inline Service — so its scope
    // is empty and it is never started (§9.1).
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
    assert_eq!(
        services[0].scope,
        ServiceScope::steps::<[&str; 0], &str>([])
    );
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
    let attached =
        [AttachedEnvironmentTemplate::new(&et).with_label("queue/cache.environment.yaml")];
    let applied = service_common::apply(&jt, &attached, &processed)
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
extensions: [SERVICE, ENV_PARAMS, EXPR]
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
extensions: [SERVICE, ENV_PARAMS, EXPR]
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

/// The §1.2.2 item 4 (wrap hook rule 1) message for wrapping Environment
/// `env` of document `doc` (`the Job Template`, or an attachment's name),
/// entered by the Sessions of `who` (`Service 'X'`, or `Service 'X'
/// (<doc>)` for an external one), when `doc` does not declare `SERVICE`.
fn wrapper_message(env: &str, doc: &str, who: &str) -> String {
    format!(
        "the Sessions of {who} enter wrapping Environment '{env}', defined by {doc}, which \
         therefore must define onWrapEnvEnter, onWrapEnvExit, onWrapServiceEnter, \
         onWrapServiceRun, onWrapServiceHealthCheck, and onWrapServiceExit; missing: \
         onWrapServiceEnter, onWrapServiceRun, onWrapServiceHealthCheck, onWrapServiceExit (RFC \
         0009 wrap hook rule 1, Template Schemas §1.2.2 item 4). Its document, {doc}, does not \
         declare the SERVICE extension, so it cannot define the onWrapService* hooks: declare SERVICE and \
         define them, or declare ENV_PARAMS and EXPR and either make the Environment \
         ON_REQUEST, so that only the Steps that request it enter it, or leave it BY_DEFAULT, so \
         that the Service can decline it with a disabled 'environment:' dependency."
    )
}

/// The document of the first attachment.
fn client_document() -> job::Document {
    job::Document::environment_template(0, None)
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
             EnvironmentTemplate[0] -> environment -> script -> actions:\n\t{}",
            wrapper_message(
                "QueueContainer",
                "EnvironmentTemplate[0]",
                "Service 'Cache'"
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
             EnvironmentTemplate[0] -> environment -> script -> actions:\n\t{}",
            wrapper_message(
                "QueueContainer",
                "EnvironmentTemplate[0]",
                "Service 'Coordinator'"
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
             EnvironmentTemplate[0] -> environment -> script -> actions:\n\t{}",
            wrapper_message(
                "QueueContainer",
                "EnvironmentTemplate[0]",
                "Service 'Cache' (EnvironmentTemplate[1])"
            )
        )
    );
}

#[test]
fn job_template_wrapper_is_not_entered_by_an_attached_service() {
    // Direction 2: a Job Template with an RFC 0008 wrapper Job Environment,
    // a queue that attaches a Service. An external Service enters only the
    // Environments of its own document and of earlier attachments, never the
    // Job Template's `jobEnvironments`, so the SERVICE-less wrapper is not
    // subject to §1.2.2 item 4 and the submission is accepted.
    let (job, applied) = submit_ok(WRAPPER_JOB, &[RFC_QUEUE_CACHE], &[]);
    let cache = &applied.external_services[0];
    assert_eq!(cache.name, "Cache");
    assert!(!cache.enters_environment(&job::Document::JobTemplate, "Container"));
    // The Job's own Step still enters the wrapper.
    let combined = applied.into_combined_job(job);
    assert!(combined.steps[0].enters_environment(&job::Document::JobTemplate, "Container"));
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
fn wrapper_declaring_service_with_three_hooks_is_rejected_when_a_service_enters_it() {
    // §1.2.2 item 4: declaring SERVICE is not enough; a Service Session
    // that enters the wrapper needs the four hooks.
    let msg = submit_err(&RFC_COORDINATOR, &[TASK_ONLY_WRAPPER_ENV_TEMPLATE], &[]);
    assert_eq!(
        msg,
        "Model validation error: 1 validation error for Submission\n\
         EnvironmentTemplate[0] -> environment -> script -> actions:\n\tthe Sessions of Service \
         'Coordinator' enter wrapping Environment 'QueueContainer', defined by \
         EnvironmentTemplate[0], which therefore must define onWrapEnvEnter, onWrapEnvExit, \
         onWrapServiceEnter, onWrapServiceRun, onWrapServiceHealthCheck, and onWrapServiceExit; \
         missing: onWrapServiceEnter, onWrapServiceRun, onWrapServiceHealthCheck, \
         onWrapServiceExit (RFC 0009 wrap hook rule 1, Template Schemas §1.2.2 item 4)."
    );
}

#[test]
fn on_request_three_hook_wrapper_no_service_requests_is_accepted() {
    // The remedy: an ON_REQUEST wrapper that only Steps request is never
    // entered by a Service Session.
    let template = TASK_ONLY_WRAPPER_ENV_TEMPLATE.replace(
        "  name: QueueContainer\n",
        "  name: QueueContainer\n  activation: ON_REQUEST\n",
    );
    let (_, applied) = submit_ok(&RFC_COORDINATOR, &[&template], &[]);
    assert_eq!(applied.environments.len(), 1);
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
fn job_template_three_hook_wrapper_is_not_entered_by_an_external_service() {
    // A Job Template that declares SERVICE with a three-hook ALWAYS
    // wrapper validates on its own (no Service of its own enters it), and
    // the queue's Service does not enter it either: an external Service
    // never enters the Job Template's `jobEnvironments`, so §1.2.2 item 4
    // does not apply and the submission is accepted.
    const T: &str = r#"
specificationVersion: "jobtemplate-2023-09"
name: Wrapped
extensions: [WRAP_ACTIONS, ENV_PARAMS, EXPR, SERVICE]
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
    let (job, applied) = submit_ok(T, &[RFC_QUEUE_CACHE], &[]);
    let cache = &applied.external_services[0];
    assert_eq!(cache.name, "Cache");
    assert!(!cache.enters_environment(&job::Document::JobTemplate, "Container"));
    let combined = applied.into_combined_job(job);
    assert!(combined.steps[0].enters_environment(&job::Document::JobTemplate, "Container"));
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
            "EnvironmentTemplate[0] -> environment -> script -> actions:\n\tthe Sessions of \
             Service 'Cache' enter wrapping Environment 'QueueContainer', defined by \
             EnvironmentTemplate[0]"
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
    // listed Services' symbols seeded for the re-check. The document's
    // Environment lists `Second` only and is `ON_REQUEST`, requested by the
    // Job's one Step; by §9.1 rules 2 and 3 every Step is then in the scope
    // of all three. (Were it `ALWAYS`, `First`'s and `Third`'s Sessions would
    // enter it and so wait for `Second`, which waits for them: see
    // `attached_always_client_environment_closes_a_cycle_at_submission`.)
    const T: &str = r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, ENV_PARAMS, EXPR]
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
environment:
  name: Client
  activation: ON_REQUEST
  dependencies: [{ service: "Second" }]
  variables: { PROXY: "{{ Service.Second.main.port }}" }
"#;
    let (_, applied) = submit_ok(REQUESTING_JOB, &[T], &[]);
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
extensions: [SERVICE, ENV_PARAMS, EXPR]
environment:
  name: UsesCache
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
extensions: [SERVICE, ENV_PARAMS, EXPR]
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
extensions: [SERVICE, ENV_PARAMS, EXPR]
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
fn attached_service_nothing_lists_has_an_empty_scope_and_is_accepted() {
    // §1.2.2, §9.1: an attached Service is scoped by the same rules as an
    // inline one over the combined Job. Nothing here lists `Metrics` — no
    // Step through `externalServices`, and the attachment has no
    // Environment — so its scope is empty. That is not an error (the queue
    // cannot know which Jobs will use it); the runtime never starts it.
    let (_, applied) = submit_ok(&RFC_COORDINATOR, &[&service_only_template("Metrics")], &[]);
    let metrics = &applied.external_services[0];
    assert_eq!(metrics.scope, ServiceScope::steps::<[&str; 0], &str>([]));
    assert!(metrics.dependencies.is_none());
}

#[test]
fn attached_service_listed_by_its_own_environment_has_every_step_in_scope() {
    // §9.1 rule 3 over the combined Job: the attachment's Environment is a
    // Job Environment of the Job, so a Service it lists has every Step. The
    // Environment is BY_DEFAULT and `Metrics` declines it: entering it would
    // be the cycle `Metrics -> Metrics` (RFC 0009 `<StepDependency>`
    // constraint 3).
    const T: &str = r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, ENV_PARAMS, EXPR]
services:
  - name: Metrics
    dependencies: [{ environment: Client, enableIf: false }]
    ports: [{ name: main }]
    script:
      actions:
        onRun: { command: serve }
environment:
  name: Client
  activation: BY_DEFAULT
  dependencies: [{ service: "Metrics" }]
  variables: { METRICS: "{{ Service.Metrics.main.port }}" }
"#;
    let (_, applied) = submit_ok(&RFC_COORDINATOR, &[T], &[]);
    assert_eq!(applied.external_services[0].scope, ServiceScope::AllSteps);
}

#[test]
fn attached_service_chain_declared_in_reverse_reaches_a_fixed_point() {
    // §9.1 rule 2 within an attached document, with the Services declared
    // in reverse dependency order so that one pass over the list is not
    // enough: `Third` is listed by `First`, which is listed by `Second`,
    // which the `ON_REQUEST` Environment the Step requests lists. All three
    // must end up with every Step.
    const T: &str = r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, ENV_PARAMS, EXPR]
services:
  - name: Third
    ports: [{ name: main }]
    script:
      actions:
        onRun: { command: serve }
  - name: First
    dependencies: [{ service: "Third" }]
    ports: [{ name: main }]
    script:
      actions:
        onRun: { command: serve }
  - name: Second
    dependencies: [{ service: "First" }]
    ports: [{ name: main }]
    script:
      actions:
        onRun: { command: proxy }
environment:
  name: Client
  activation: ON_REQUEST
  dependencies: [{ service: "Second" }]
  variables: { PROXY: "{{ Service.Second.main.port }}" }
"#;
    let (_, applied) = submit_ok(REQUESTING_JOB, &[T], &[]);
    assert_eq!(
        service_names(&applied.external_services),
        ["Third", "First", "Second"]
    );
    for svc in &applied.external_services {
        assert_eq!(svc.scope, ServiceScope::AllSteps, "{}", svc.name);
    }
}

/// An Environment Template with one Service `Cache` and an `ALWAYS`
/// Environment `<env>` that lists it.
fn cache_with_always_client(env: &str) -> String {
    format!(
        r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, ENV_PARAMS, EXPR]
services:
  - name: Cache
    ports: [{{ name: main }}]
    script: {{ actions: {{ onRun: {{ command: serve }} }} }}
environment:
  name: {env}
  dependencies: [{{ service: "Cache" }}]
  variables: {{ PORT: "{{{{ Service.Cache.main.port }}}}" }}
"#
    )
}

/// RFC 0009 `<StepDependency>` constraint 3 and Environment Template
/// *services* constraint 5: an `ALWAYS` client Environment that lists its
/// own document's `Cache` is entered by `Cache`'s Sessions, the cycle
/// `Cache -> Cache`, rejected when the Environment Template is validated.
/// Made `BY_DEFAULT` and declined by `Cache`, two such attachments combine
/// without a cycle: an external Service enters the Environments of its own
/// document and of earlier attachments only, so the second `Cache` waits for
/// the first (it enters `FirstClient`) but the first never enters
/// `SecondClient`, and no cycle can close across the attachments.
#[test]
fn attached_client_environments_cannot_close_a_cycle_across_attachments() {
    let first = cache_with_always_client("FirstClient");
    let err = decode_environment_template(yaml_val(&first), Some(EXTS), &CallerLimits::default())
        .expect_err("an ALWAYS Environment entered by the Service it lists is a cycle")
        .to_string();
    assert_eq!(
        err,
        "Model validation error: 1 validation error for EnvironmentTemplate\nservices:\n\t\
         dependencies contain a cycle: Service Cache -> Service Cache (the Sessions of Service \
         Cache enter Environment 'FirstClient', which lists Service 'Cache', so it would wait \
         for itself to be READY. There is no exception for an Environment that lists the \
         Service: keep the Service's Sessions out of it: drop any 'environment:' dependency of the Service that requests it, and decline a BY_DEFAULT one with a disabled 'environment:' dependency of the Service, or make it ON_REQUEST and request it only from the Steps that need it; Template Schemas §3.2 \
         constraint 3)."
    );
    let declined = |env: &str| {
        cache_with_always_client(env)
            .replace(
                &format!("  name: {env}\n"),
                &format!("  name: {env}\n  activation: BY_DEFAULT\n"),
            )
            .replace(
                "    ports: [{ name: main }]\n",
                &format!(
                    "    dependencies: [{{ environment: {env}, enableIf: false }}]\n    \
                     ports: [{{ name: main }}]\n"
                ),
            )
    };
    let (job, applied) = submit_ok(
        PLAIN_JOB,
        &[&declined("FirstClient"), &declined("SecondClient")],
        &[],
    );
    let first_doc = job::Document::environment_template(0, None);
    let second_doc = job::Document::environment_template(1, None);
    let [first_cache, second_cache] = applied.external_services.as_slice() else {
        panic!("two external Services: {:?}", applied.external_services);
    };
    assert_eq!(first_cache.document, first_doc);
    assert!(!first_cache.enters_environment(&first_doc, "FirstClient"));
    assert!(!first_cache.enters_environment(&second_doc, "SecondClient"));
    assert_eq!(second_cache.document, second_doc);
    assert!(second_cache.enters_environment(&first_doc, "FirstClient"));
    assert!(!second_cache.enters_environment(&second_doc, "SecondClient"));
    // The Step enters both client Environments.
    let combined = applied.into_combined_job(job);
    assert!(combined.steps[0].enters_environment(&first_doc, "FirstClient"));
    assert!(combined.steps[0].enters_environment(&second_doc, "SecondClient"));
}

#[test]
fn inline_service_listing_a_requirement_passes_its_scope_to_the_attached_service() {
    // §9.1 rule 2 across the requirement: inline `Proxy` lists the
    // `externalServices` entry `Metrics`, and only Step A lists `Proxy`, so
    // the attached `Metrics` is in scope for A alone, computed over the
    // combined Job.
    const JOB: &str = r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [SERVICE, ENV_PARAMS, EXPR]
name: ViaInline
externalServices:
  - name: Metrics
    ports: [{ name: main }]
services:
  - name: Proxy
    dependencies: [{ service: "Metrics" }]
    ports: [{ name: http }]
    script: { actions: { onRun: { command: proxy, args: ["{{ Service.Metrics.main.port }}"] } } }
steps:
  - name: A
    dependencies: [{ service: "Proxy" }]
    script: { actions: { onRun: { command: run } } }
  - name: B
    script: { actions: { onRun: { command: run } } }
"#;
    let (_, applied) = submit_ok(JOB, &[&service_only_template("Metrics")], &[]);
    assert_eq!(
        applied.external_services[0].scope,
        ServiceScope::steps(["A"])
    );
}

#[test]
fn every_step_inline_service_listing_a_requirement_gives_the_attached_service_every_step() {
    // §9.1 rules 2–3 across the requirement: a Job Environment that every
    // Step requests lists the inline `Proxy` (every Step), and `Proxy` lists
    // the `externalServices` entry `Metrics`, so the attached `Metrics` has
    // every Step. The Environment is `ON_REQUEST`: were it `ALWAYS`,
    // `Metrics`'s Sessions would enter it and wait for `Proxy`, which waits
    // for `Metrics` (RFC 0009 `<StepDependency>` constraint 3).
    const JOB: &str = r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [SERVICE, ENV_PARAMS, EXPR]
name: ViaInlineAllSteps
externalServices:
  - name: Metrics
    ports: [{ name: main }]
services:
  - name: Proxy
    dependencies: [{ service: "Metrics" }]
    ports: [{ name: http }]
    script: { actions: { onRun: { command: proxy, args: ["{{ Service.Metrics.main.port }}"] } } }
jobEnvironments:
  - name: E
    activation: ON_REQUEST
    dependencies: [{ service: "Proxy" }]
    variables: { P: "{{ Service.Proxy.http.port }}" }
steps:
  - name: A
    dependencies: [{ environment: E }]
    script: { actions: { onRun: { command: run } } }
  - name: B
    dependencies: [{ environment: E }]
    script: { actions: { onRun: { command: run } } }
"#;
    let (_, applied) = submit_ok(JOB, &[&service_only_template("Metrics")], &[]);
    assert_eq!(applied.external_services[0].scope, ServiceScope::AllSteps);
}

#[test]
fn attached_service_listed_by_two_step_scoped_siblings_takes_the_union_of_their_scopes() {
    // §9.1 rule 2 within the attached document, over the combined Job:
    // `Store` is listed by `Front1` and `Front2`, bound to requirements that
    // Steps A and B list respectively; `Store`'s scope is the union {A, B}.
    const JOB: &str = r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [SERVICE, ENV_PARAMS, EXPR]
name: Union
externalServices:
  - name: Front1
    ports: [{ name: main }]
  - name: Front2
    ports: [{ name: main }]
steps:
  - name: A
    dependencies: [{ service: "Front1" }]
    script: { actions: { onRun: { command: run, args: ["{{ Service.Front1.main.port }}"] } } }
  - name: B
    dependencies: [{ service: "Front2" }]
    script: { actions: { onRun: { command: run, args: ["{{ Service.Front2.main.port }}"] } } }
  - name: C
    script: { actions: { onRun: { command: run } } }
"#;
    const T: &str = r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, ENV_PARAMS, EXPR]
services:
  - name: Store
    ports: [{ name: main }]
    script: { actions: { onRun: { command: serve } } }
  - name: Front1
    dependencies: [{ service: "Store" }]
    ports: [{ name: main }]
    script: { actions: { onRun: { command: front, args: ["{{ Service.Store.main.port }}"] } } }
  - name: Front2
    dependencies: [{ service: "Store" }]
    ports: [{ name: main }]
    script: { actions: { onRun: { command: front, args: ["{{ Service.Store.main.port }}"] } } }
"#;
    let (_, applied) = submit_ok(JOB, &[T], &[]);
    let scope_of = |name: &str| {
        applied
            .external_services
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no {name}"))
            .scope
            .clone()
    };
    assert_eq!(scope_of("Front1"), ServiceScope::steps(["A"]));
    assert_eq!(scope_of("Front2"), ServiceScope::steps(["B"]));
    assert_eq!(scope_of("Store"), ServiceScope::steps(["A", "B"]));
}

#[test]
fn attached_service_bound_to_a_requirement_takes_the_requirement_scope() {
    // §9.1 rules 1–2 over the combined Job: a Step that lists the
    // `externalServices` entry is in the matched attached Service's scope,
    // and only that Step.
    const JOB: &str = r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [SERVICE, ENV_PARAMS, EXPR]
name: Req
externalServices:
  - name: Metrics
    ports: [{ name: main }]
steps:
  - name: A
    dependencies: [{ service: "Metrics" }]
    script: { actions: { onRun: { command: run, args: ["{{ Service.Metrics.main.port }}"] } } }
  - name: B
    script: { actions: { onRun: { command: run } } }
"#;
    let (_, applied) = submit_ok(JOB, &[&service_only_template("Metrics")], &[]);
    assert_eq!(applied.requirement_bindings.len(), 1);
    assert_eq!(
        applied.external_services[0].scope,
        ServiceScope::steps(["A"])
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
extensions: [SERVICE, ENV_PARAMS, EXPR]
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
    // carried-forward re-check must seed that symbol (the Environment lists
    // Cache), or the re-check would fail. Covered by the success of the
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
extensions: [SERVICE, ENV_PARAMS, EXPR]
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
extensions: [SERVICE, ENV_PARAMS, EXPR]
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
    assert_eq!(
        msg,
        "Model validation error: 1 validation error for Submission\n\
         EnvironmentTemplate[0] -> services[0] -> let[0]:\n\
         \tInvalid expression in let binding 'share': Division by zero\n\
         \x20 share = 100 // Param.Divisor\n\
         \x20         ~~~~^~~~~~~~~~~~~~~~"
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

/// Check failures across every attached document — a Service's and an
/// Environment's — are reported together in one `Submission` error, each
/// under its document's path, as `create_job` aggregates the Job
/// Template's; the first failing document does not abort alone.
#[test]
fn attached_document_check_failures_aggregate_across_documents() {
    const SVC: &str = r#"
specificationVersion: "environment-2023-09"
extensions: [SERVICE, ENV_PARAMS, EXPR]
parameterDefinitions:
  - name: Port
    type: INT
    default: 70000
  - name: Divisor
    type: INT
    default: 0
services:
  - name: Svc
    ports: [{ name: main, port: "{{ Param.Port }}" }]
    script:
      let: ["share = 100 // Param.Divisor"]
      actions:
        onRun: { command: serve, args: ["{{ share }}"] }
"#;
    const ENV: &str = r#"
specificationVersion: "environment-2023-09"
parameterDefinitions:
  - name: Big
    type: STRING
environment:
  name: Big
  variables:
    BIG: "{{ Param.Big }}"
"#;
    let long = "x".repeat(2049);
    let msg = submit_err(PLAIN_JOB, &[SVC, ENV], &[("Big", &long)]);
    assert_eq!(
        msg,
        "Model validation error: 3 validation errors for Submission\n\
         EnvironmentTemplate[0] -> services[0] -> ports[0] -> port:\n\
         \tmust be between 1 and 65535.\n\
         EnvironmentTemplate[0] -> services[0] -> script -> let[0]:\n\
         \tInvalid expression in let binding 'share': Division by zero\n\
         \x20 share = 100 // Param.Divisor\n\
         \x20         ~~~~^~~~~~~~~~~~~~~~\n\
         EnvironmentTemplate[1] -> environment -> variables -> BIG:\n\
         \tresolves to at least 2049 characters, exceeding the maximum of 2048."
    );
}

#[test]
fn attached_environment_template_profile_reflects_its_own_extensions() {
    let et = decode_env(RFC_QUEUE_CACHE);
    let profile = et.profile();
    assert!(profile.has_extension(openjd_model::ModelExtension::Service));
    assert!(profile.has_extension(openjd_model::ModelExtension::Expr));
    // The RFC's example lists [SERVICE, ENV_PARAMS, EXPR] and nothing else.
    assert!(profile.has_extension(openjd_model::ModelExtension::EnvParams));
    assert!(!profile.has_extension(openjd_model::ModelExtension::FeatureBundle1));
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

// ════════════════════════════════════════════════════════════════════
// 7. Attached Services and the Environments of the attachments
// ════════════════════════════════════════════════════════════════════

/// Decodes each attachment, as [`submit`] does.
fn decode_envs(attachments: &[&str]) -> Vec<EnvironmentTemplate> {
    attachments.iter().map(|t| decode_env(t)).collect()
}

/// `check_environment_templates` for `job_template` with `attachments`:
/// what `openjd check --environment` reports.
fn check_ets(job_template: &str, attachments: &[&str]) -> Result<(), String> {
    let jt = decode_job(job_template);
    let ets = decode_envs(attachments);
    let attached: Vec<AttachedEnvironmentTemplate<'_>> = ets.iter().map(Into::into).collect();
    openjd_model::check_environment_templates(&jt, &attached, &jt.default_validation_context())
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// The whole `CreatedJob` of the submission of `job_template` with
/// `attachments`, with default parameter values.
fn create_ets(
    job_template: &str,
    attachments: &[&str],
) -> Result<openjd_model::CreatedJob, String> {
    let jt = decode_job(job_template);
    let ets = decode_envs(attachments);
    let td = tempfile::TempDir::new().unwrap();
    let dir = td.path().to_str().unwrap();
    let processed = openjd_model::preprocess_job_parameters(
        &jt,
        &JobParameterInputValues::default(),
        &ets,
        &openjd_model::PathParameterOptions::new(dir, dir),
    )
    .map_err(|e| e.to_string())?;
    let attached: Vec<AttachedEnvironmentTemplate<'_>> = ets.iter().map(Into::into).collect();
    openjd_model::create_job_with_environment_templates(
        &jt,
        &attached,
        &processed,
        &jt.default_validation_context(),
    )
    .map_err(|e| e.to_string())
}

/// `check_environment_templates` and job creation give the same result for
/// `job_template` with `attachments`; returns the error, if any.
fn agree_ets(job_template: &str, attachments: &[&str]) -> Option<String> {
    let checked = check_ets(job_template, attachments);
    assert_eq!(
        checked,
        create_ets(job_template, attachments).map(|_| ()),
        "check and job creation disagree"
    );
    checked.err()
}

/// The decisions on the Environments of the combined Job's Service `k`, as
/// `name@document: status[ by name@document]`.
fn statuses_of(created: &openjd_model::CreatedJob, k: usize) -> Vec<String> {
    use openjd_model::EnvironmentStatus;
    created.service_environment_statuses[k]
        .environments
        .iter()
        .map(|s| {
            let status = match s.status {
                EnvironmentStatus::Entered => "entered",
                EnvironmentStatus::Declined => "declined",
                EnvironmentStatus::Disabled => "disabled",
                EnvironmentStatus::Superseded => "superseded",
            };
            let by = s
                .superseded_by
                .as_ref()
                .map_or(String::new(), |w| format!(" by {}@{}", w.name, w.document));
            format!("{}@{}: {status}{by}", s.name, s.document)
        })
        .collect()
}

/// A Job Template whose one Step lists the `externalServices` entry
/// `name`, so the attached Service it matches is used.
fn requiring(name: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
name: J
externalServices:
  - name: {name}
    ports: [{{name: main}}]
steps:
  - name: A
    dependencies: [{{service: {name}}}]
    script: {{actions: {{onRun: {{command: run}}}}}}
"
    )
}

/// An Environment Template with only the Environment `name`, with
/// `extra` (indented two spaces) after the name.
fn env_only(name: &str, extra: &str) -> String {
    format!(
        "specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: {name}
{extra}  variables: {{K: v}}
"
    )
}

/// An Environment Template with Service `S` (`svc_extra` indented four
/// spaces) and, when `own` is not empty, the Environment whose body `own`
/// is (indented two spaces).
fn s_with(svc_extra: &str, own: &str) -> String {
    let own = if own.is_empty() {
        String::new()
    } else {
        format!("environment:\n{own}")
    };
    format!(
        "specificationVersion: environment-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
services:
  - name: S
{svc_extra}    ports: [{{name: main}}]
    script: {{actions: {{onRun: {{command: run}}}}}}
{own}"
    )
}

/// Job creation's error for the cycle `S -> S` of the only attachment's
/// Service `S` entering its own Environment `env`, which lists it.
fn own_cycle(env: &str) -> String {
    format!(
        "Model validation error: 1 validation error for Submission\n\
         EnvironmentTemplate[0] -> services:\n\
         \tdependencies contain a cycle: Service S (EnvironmentTemplate[0]) -> Service S \
         (EnvironmentTemplate[0]) (the Sessions of Service S (EnvironmentTemplate[0]) enter Job \
         Environment '{env}' (EnvironmentTemplate[0]), which lists Service 'S', so it would wait \
         for itself to be READY. There is no exception for an Environment that lists the \
         Service: keep the Service's Sessions out of it: drop any 'environment:' dependency of \
         the Service that requests it, and decline a BY_DEFAULT one with a disabled \
         'environment:' dependency of the Service, or make it ON_REQUEST and request it only \
         from the Steps that need it; Template Schemas §3.2 constraint 3)."
    )
}

/// `Own`, in `exclusiveGroup: G`, listing `S`.
const OWN_IN_G: &str = "  name: Own
  exclusiveGroup: G
  dependencies: [{service: S}]
  variables: {P: \"{{Service.S.main.port}}\"}
";

/// An Environment Template whose own Environment is in an
/// `exclusiveGroup` and lists its own Service is not a cycle when the
/// Environment Template is validated, since an earlier attachment's member
/// of the group may supersede it. Job creation decides: superseded by an
/// earlier attachment's `A`, the Service does not enter `Own` and there is
/// no cycle; attached alone, the Service enters `Own`, which waits for it,
/// and the cycle is reported at the Environment Template's `services`.
/// Its nodes name the Service with its document; the Service that the
/// Environment lists is quoted by its bare name.
#[test]
fn an_own_environment_in_an_exclusive_group_listing_its_service_is_decided_at_job_creation() {
    let et1 = env_only("A", "  exclusiveGroup: G\n");
    let et2 = s_with("", OWN_IN_G);
    // (a)
    decode_environment_template(yaml_val(&et2), Some(EXTS), &CallerLimits::default())
        .expect("a grouped Environment listing its Service is decided at job creation");
    // (b)
    let job = requiring("S");
    assert_eq!(agree_ets(&job, &[&et1, &et2]), None);
    let created = create_ets(&job, &[&et1, &et2]).unwrap();
    assert_eq!(
        statuses_of(&created, 0),
        [
            "A@EnvironmentTemplate[0]: entered",
            "Own@EnvironmentTemplate[1]: superseded by A@EnvironmentTemplate[0]",
        ]
    );
    // (c)
    assert_eq!(create_ets(&job, &[&et2]).unwrap_err(), own_cycle("Own"));
}

/// An Environment Template's own Environment whose `enableIf` is an
/// expression and lists its own Service is decided at job creation; when it
/// is enabled, the Service's Sessions enter it, and the cycle is reported at
/// the Environment Template's `services`, not at the Job Template.
#[test]
fn an_own_environment_with_an_enable_if_expression_listing_its_service_cycles_at_its_services() {
    let et = format!(
        "parameterDefinitions:\n  - {{name: EB, type: BOOL, default: true}}\n{}",
        s_with(
            "",
            "  name: Own
  enableIf: \"Param.EB\"
  dependencies: [{service: S}]
  variables: {P: \"{{Service.S.main.port}}\"}
"
        )
    );
    assert_eq!(
        create_ets(&requiring("S"), &[&et]).unwrap_err(),
        own_cycle("Own")
    );
}

/// An attached Service's `environment:` entry errors are reported
/// whatever its scope, also when nothing uses it, by both
/// `check_environment_templates` and job creation: declining an earlier
/// attachment's `ALWAYS` Environment, also when a second earlier
/// attachment has the same name.
#[test]
fn an_unused_attached_services_entry_errors_are_reported_by_check_and_job_creation() {
    let w = env_only("W", "  activation: ALWAYS\n");
    let declines_w = s_with(
        "    dependencies: [{environment: W, enableIf: false}]\n",
        "",
    );
    // (a)
    assert_eq!(
        agree_ets(PLAIN_JOB, &[&w, &declines_w]).unwrap(),
        "Model validation error: 1 validation error for Submission\n\
         EnvironmentTemplate[1] -> services[0] -> dependencies[0] -> enableIf:\n\
         \tis false, so Service 'S' would decline Environment 'W' (EnvironmentTemplate[0]), \
         whose activation is ALWAYS: no Service can leave out an ALWAYS Environment (Template \
         Schemas §3.2 item 3.2)."
    );
    // (b) The name refers to both: it declines the `BY_DEFAULT` one, and
    // the `ALWAYS` one is the only error (§3.2.1 item 3).
    let by_default_w = env_only("W", "  activation: BY_DEFAULT\n");
    assert_eq!(
        agree_ets(PLAIN_JOB, &[&by_default_w, &w, &declines_w]).unwrap(),
        "Model validation error: 1 validation error for Submission\n\
         EnvironmentTemplate[2] -> services[0] -> dependencies[0] -> enableIf:\n\
         \tis false, so Service 'S' would decline Environment 'W' (EnvironmentTemplate[1]), \
         whose activation is ALWAYS: no Service can leave out an ALWAYS Environment (Template \
         Schemas §3.2 item 3.2)."
    );
}

/// An attached Service's error for declining an earlier attachment's
/// `ALWAYS` Environment is a §7.4.1 step 4 error, filed after every Step's
/// step 4 errors, in `check_environment_templates` as in job creation:
/// Step `A`'s decline of the attached `ALWAYS` `Z` comes first.
#[test]
fn an_attached_services_always_decline_is_a_step_4_error_after_the_steps() {
    let job = "specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
name: J
steps:
  - name: A
    dependencies: [{environment: Z, enableIf: false}]
    script: {actions: {onRun: {command: run}}}
";
    let w = env_only("W", "  activation: ALWAYS\n");
    let z = env_only("Z", "  activation: ALWAYS\n");
    let declines_w = s_with(
        "    dependencies: [{environment: W, enableIf: false}]\n",
        "",
    );
    assert_eq!(
        agree_ets(job, &[&w, &z, &declines_w]).unwrap(),
        "Model validation error: 2 validation errors for Submission\n\
         JobTemplate -> steps[0] -> dependencies[0] -> enableIf:\n\
         \tis false, so Step 'A' would decline Environment 'Z' (EnvironmentTemplate[1]), whose \
         activation is ALWAYS: no Step can leave out an ALWAYS Environment (Template Schemas \
         §3.2 item 3.2).\n\
         EnvironmentTemplate[2] -> services[0] -> dependencies[0] -> enableIf:\n\
         \tis false, so Service 'S' would decline Environment 'W' (EnvironmentTemplate[0]), \
         whose activation is ALWAYS: no Service can leave out an ALWAYS Environment (Template \
         Schemas §3.2 item 3.2)."
    );
}

/// An attached Service's disabled entry whose name two earlier
/// attachments define is not an error, as for a Step (Template Schemas
/// §3.2.1 item 3, §1.2 services constraint 5): it refers to each of them,
/// declining each `BY_DEFAULT` one and having no effect on one from a
/// document that does not list `ENV_PARAMS`. `check_environment_templates`
/// and job creation agree, used or not.
#[test]
fn an_attached_services_disabled_entry_with_an_ambiguous_name_refers_to_each_match() {
    const PRE: &str = "specificationVersion: environment-2023-09
environment:
  name: W
  variables: {K: v}
";
    let by_default_w = env_only("W", "  activation: BY_DEFAULT\n");
    let on_request_w = env_only("W", "  activation: ON_REQUEST\n");
    let declines_w = s_with(
        "    dependencies: [{environment: W, enableIf: false}]\n",
        "",
    );
    let job = requiring("S");
    for (attachments, expected) in [
        (
            [&by_default_w as &str, &by_default_w],
            [
                "W@EnvironmentTemplate[0]: declined",
                "W@EnvironmentTemplate[1]: declined",
            ],
        ),
        (
            [PRE, PRE],
            [
                "W@EnvironmentTemplate[0]: entered",
                "W@EnvironmentTemplate[1]: entered",
            ],
        ),
        (
            [&by_default_w, PRE],
            [
                "W@EnvironmentTemplate[0]: declined",
                "W@EnvironmentTemplate[1]: entered",
            ],
        ),
    ] {
        let all = [attachments[0], attachments[1], &declines_w];
        assert_eq!(agree_ets(PLAIN_JOB, &all), None, "{expected:?}");
        assert_eq!(agree_ets(&job, &all), None, "{expected:?}");
        let created = create_ets(&job, &all).unwrap();
        assert_eq!(statuses_of(&created, 0), expected);
    }
    // An `ON_REQUEST` match is not entered and the entry has no effect on
    // it; only the `BY_DEFAULT` one is declined.
    let all = [&on_request_w as &str, &by_default_w, &declines_w];
    assert_eq!(agree_ets(&job, &all), None);
    let created = create_ets(&job, &all).unwrap();
    assert_eq!(
        statuses_of(&created, 0),
        ["W@EnvironmentTemplate[1]: declined"]
    );
}

/// An attached Service's disabled entry for its own document's
/// Environment refers to that one only, also when earlier attachments
/// that list `ENV_PARAMS` have an Environment of the same name (§1.2
/// services constraint 5): an earlier `BY_DEFAULT` one is still entered,
/// and declining the name is no error for an earlier `ALWAYS` one.
/// `check_environment_templates` and job creation agree.
#[test]
fn an_attached_services_own_environment_hides_earlier_same_named_ones() {
    let declines_own_w = s_with(
        "    dependencies: [{environment: W, enableIf: false}]\n",
        "  name: W\n  variables: {K: own}\n",
    );
    let job = requiring("S");
    for activation in ["BY_DEFAULT", "ALWAYS"] {
        let earlier = env_only("W", &format!("  activation: {activation}\n"));
        let all = [&earlier as &str, &declines_own_w];
        assert_eq!(agree_ets(&job, &all), None, "{activation}");
        let created = create_ets(&job, &all).unwrap();
        assert_eq!(
            statuses_of(&created, 0),
            [
                "W@EnvironmentTemplate[0]: entered",
                "W@EnvironmentTemplate[1]: declined",
            ],
            "{activation}"
        );
    }
}

/// An attached Service's disabled entry whose name two earlier
/// attachments define refers to every one of them, whatever their order,
/// in `check_environment_templates` as in job creation: with the `ALWAYS`
/// match first, declining it is the error; and with two `BY_DEFAULT`
/// matches that have a parameter without a `default`, the Service declines
/// both, so constraint 7 holds for it, and only the Step, which declines
/// neither, makes the attachments' parameters an error.
#[test]
fn an_attached_services_ambiguous_disabled_entry_resolves_every_match_in_check() {
    let declines_w = s_with(
        "    dependencies: [{environment: W, enableIf: false}]\n",
        "",
    );
    let always_w = env_only("W", "  activation: ALWAYS\n");
    let by_default_w = env_only("W", "  activation: BY_DEFAULT\n");
    assert_eq!(
        agree_ets(PLAIN_JOB, &[&always_w, &by_default_w, &declines_w]).unwrap(),
        "Model validation error: 1 validation error for Submission\n\
         EnvironmentTemplate[2] -> services[0] -> dependencies[0] -> enableIf:\n\
         \tis false, so Service 'S' would decline Environment 'W' (EnvironmentTemplate[0]), \
         whose activation is ALWAYS: no Service can leave out an ALWAYS Environment (Template \
         Schemas §3.2 item 3.2)."
    );
    let needs_r = env_only(
        "W",
        "  activation: BY_DEFAULT\n  envParameterDefinitions: [{name: R, type: STRING}]\n",
    );
    assert_eq!(
        agree_ets(&requiring("S"), &[&needs_r, &needs_r, &declines_w]).unwrap(),
        "Model validation error: 2 validation errors for Submission\n\
         EnvironmentTemplate[0] -> environment -> envParameterDefinitions:\n\
         \tEnvironment 'W' is defined by 2 attached Environment Templates \
         (EnvironmentTemplate[0], EnvironmentTemplate[1]), so no enabled 'environment:' \
         dependency can reach it (Template Schemas §3.2.1 item 3), and it has parameters \
         without a default that no Step or Service can supply: R. Its activation is \
         BY_DEFAULT, so it applies to every Step and Service that does not decline it with a \
         disabled entry, and at least one Step or Service does not decline it, and the \
         submission is rejected (§3.2.1).\n\
         EnvironmentTemplate[1] -> environment -> envParameterDefinitions:\n\
         \tEnvironment 'W' is defined by 2 attached Environment Templates \
         (EnvironmentTemplate[0], EnvironmentTemplate[1]), so no enabled 'environment:' \
         dependency can reach it (Template Schemas §3.2.1 item 3), and it has parameters \
         without a default that no Step or Service can supply: R. Its activation is \
         BY_DEFAULT, so it applies to every Step and Service that does not decline it with a \
         disabled entry, and at least one Step or Service does not decline it, and the \
         submission is rejected (§3.2.1)."
    );
}

/// RFC 0010 S2 (Template Schemas §4.5 item 1): an Environment parameter
/// `default` may read `Job.Name`, also for the Environments an attached
/// Service's Sessions enter: here its own document's `W`.
#[test]
fn an_attached_services_environment_default_reads_job_name() {
    let et = s_with(
        "",
        "  name: W
  envParameterDefinitions: [{name: T, type: STRING, default: \"job-{{Job.Name}}\"}]
  variables: {K: \"{{Env.Param.T}}\"}
",
    );
    let job = requiring("S");
    assert_eq!(agree_ets(&job, &[&et]), None);
    let created = create_ets(&job, &[&et]).unwrap();
    let s = &created.job.services.as_ref().unwrap()[0];
    let values: Vec<String> = s
        .environment_parameters
        .iter()
        .flatten()
        .map(|v| format!("{}: {}", v.environment, v.values.as_value()))
        .collect();
    assert_eq!(
        values,
        [r#"W: [{"name":"T","type":"string","value":"job-J"}]"#]
    );
}

/// Declining an earlier attachment's Environment from a
/// document that does not list `ENV_PARAMS` (whose Environment behaves as
/// `ALWAYS`) has no effect (Template Schemas §4 item 4): no error from
/// `check_environment_templates` or job creation, and the Service still
/// enters it.
#[test]
fn declining_a_pre_extension_attachments_environment_has_no_effect() {
    const PRE: &str = "specificationVersion: environment-2023-09
environment:
  name: W
  variables: {K: v}
";
    let declines_w = s_with(
        "    dependencies: [{environment: W, enableIf: false}]\n",
        "",
    );
    let job = requiring("S");
    assert_eq!(agree_ets(&job, &[PRE, &declines_w]), None);
    let created = create_ets(&job, &[PRE, &declines_w]).unwrap();
    assert_eq!(
        statuses_of(&created, 0),
        ["W@EnvironmentTemplate[0]: entered"]
    );
}

/// An attached Service's own Environment in an
/// `exclusiveGroup` is superseded, for the Service, by an earlier
/// attachment's member of the group; a member that is disabled (literal
/// `enableIf: false`) takes no part, and the later one is entered.
#[test]
fn an_attached_services_own_grouped_environment_is_superseded_by_an_earlier_attachments() {
    let c = "specificationVersion: environment-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
services:
  - name: C
    ports: [{name: main}]
    script: {actions: {onRun: {command: run}}}
environment:
  name: G1
  exclusiveGroup: g
  variables: {K: v}
";
    let job = requiring("C");
    let g0 = env_only("G0", "  exclusiveGroup: g\n");
    let created = create_ets(&job, &[&g0, c]).unwrap();
    assert_eq!(
        statuses_of(&created, 0),
        [
            "G0@EnvironmentTemplate[0]: entered",
            "G1@EnvironmentTemplate[1]: superseded by G0@EnvironmentTemplate[0]",
        ]
    );
    let g0_off = env_only("G0", "  exclusiveGroup: g\n  enableIf: false\n");
    let created = create_ets(&job, &[&g0_off, c]).unwrap();
    assert_eq!(
        statuses_of(&created, 0),
        [
            "G0@EnvironmentTemplate[0]: disabled",
            "G1@EnvironmentTemplate[1]: entered",
        ]
    );
}

/// `check_environment_templates` reports a cycle inside one attachment,
/// every node a Service of that Environment Template, at the attachment's
/// `services`, as job creation does: the attachment's own `Own`, in an
/// `exclusiveGroup` that nothing earlier supersedes, lists its Service
/// `S`, which a Step uses, so `S`'s Sessions enter `Own`, which waits for
/// `S`.
#[test]
fn check_reports_a_cycle_inside_one_attachment_at_its_services() {
    let et = s_with("", OWN_IN_G);
    assert_eq!(
        agree_ets(&requiring("S"), &[&et]).unwrap(),
        own_cycle("Own")
    );
}

/// §1.2 *services* constraint 5 with §3.2 item 3.2: only a disabled entry
/// that names the document's own `environment` declines it. A Service whose
/// own Environment `Own` is `ALWAYS` may decline an earlier attachment's
/// `BY_DEFAULT` `W`: the Environment Template decodes, both
/// `check_environment_templates` and job creation accept it, and the
/// Service declines `W` and enters `Own`.
#[test]
fn declining_an_earlier_attachments_environment_beside_an_always_own_is_accepted() {
    let w = env_only("W", "  activation: BY_DEFAULT\n");
    let et = s_with(
        "    dependencies: [{environment: W, enableIf: false}]\n",
        "  name: Own\n  activation: ALWAYS\n  variables: {K: v}\n",
    );
    decode_environment_template(yaml_val(&et), Some(EXTS), &CallerLimits::default())
        .expect("declining another Environment beside an ALWAYS own one is valid");
    let job = requiring("S");
    assert_eq!(agree_ets(&job, &[&w, &et]), None);
    let created = create_ets(&job, &[&w, &et]).unwrap();
    assert_eq!(
        statuses_of(&created, 0),
        [
            "W@EnvironmentTemplate[0]: declined",
            "Own@EnvironmentTemplate[1]: entered",
        ]
    );
}

/// §3.2.1 item 3: an attached Service's `environment:` entry matches only
/// its own document's Environment or one of an Environment Template
/// attached before it. A disabled entry naming `W`, the `ALWAYS`
/// Environment of a later attachment, matches nothing and has no effect:
/// no error from `check_environment_templates` or job creation, and the
/// Service's Sessions do not consider `W`.
#[test]
fn an_entry_naming_a_later_attachments_environment_has_no_effect() {
    let declines_w = s_with(
        "    dependencies: [{environment: W, enableIf: false}]\n",
        "",
    );
    let w = env_only("W", "  activation: ALWAYS\n");
    let job = requiring("S");
    assert_eq!(agree_ets(&job, &[&declines_w, &w]), None);
    let created = create_ets(&job, &[&declines_w, &w]).unwrap();
    assert_eq!(statuses_of(&created, 0), Vec::<String>::new());
}
