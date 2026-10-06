// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Integration tests for RFC 0009 `requiresServices` (Template Schemas §1.1
//! item 9, §1.2.2 items 2–4, §9 scope lists, §9.8, §9.9 items 1, 5 and 11):
//!
//! 1. **Template validation** — schema, list and name constraints, the
//!    `Service.R.*` scope a requirement opens, and the diagnostics for an
//!    undeclared port and for `bindAddress`.
//! 2. **Job creation** — `job::Job::requires_services` and its JSON.
//! 3. **Submission** (`apply_environment_templates`) — matching each
//!    requirement to exactly one attached Service, the bindings recorded,
//!    and every rejection; plus the §1.2.2 item 4 wrapper rule as it
//!    concerns external Services.
//!
//! A required Service's `Service.R.*` values follow the same rule as an
//! inline Service's: a Step or Service sees them only when it lists
//! `service:R` in its `dependencies` (`with_step_deps`), and a Job
//! Environment sees them with no dependency. An inline Service in these
//! fixtures is made used by having Step `S` list it, since an unused inline
//! Service is rejected; an unused requirement is not.
//!
//! Error assertions follow the repo convention of asserting on the full
//! Pydantic-style error path + message.

use openjd_expr::ExprValue;
use openjd_model::job::{self, ServicePortProtocol, ServiceScope};
use openjd_model::template::{compute_service_scopes, EnvironmentTemplate, JobTemplate};
use openjd_model::{
    apply_environment_templates, create_job, decode_environment_template, decode_job_template,
    AppliedEnvironmentTemplates, AttachedEnvironmentTemplate, CallerLimits,
    JobParameterInputValues, ModelError, RequirementBinding,
};

const EXTS: &[&str] = &["EXPR", "SERVICE", "FEATURE_BUNDLE_1", "WRAP_ACTIONS"];

const RFC_REQUIRED: &str = include_str!("../fixtures/rfc0009/required-queue-cache.job.yaml");
const RFC_QUEUE_CACHE: &str = include_str!("../fixtures/rfc0009/queue-cache.environment.yaml");

fn yaml_val(s: &str) -> serde_json::Value {
    serde_saphyr::from_str(s).unwrap()
}

fn decode_job(template: &str) -> JobTemplate {
    decode_job_template(yaml_val(template), Some(EXTS), &CallerLimits::default())
        .expect("job template should validate")
}

fn job_err(template: &str) -> String {
    decode_job_template(yaml_val(template), Some(EXTS), &CallerLimits::default())
        .expect_err("job template should fail validation")
        .to_string()
}

fn decode_env(template: &str) -> EnvironmentTemplate {
    decode_environment_template(yaml_val(template), Some(EXTS), &CallerLimits::default())
        .expect("environment template should validate")
}

/// Asserts that `err` has exactly `count` errors and contains each of
/// `lines` (each a full `path:\n\tmessage` entry or a distinctive part).
fn assert_errs(err: &str, count: usize, lines: &[&str]) {
    let header = if count == 1 {
        "Model validation error: 1 validation error for JobTemplate\n".to_string()
    } else {
        format!("Model validation error: {count} validation errors for JobTemplate\n")
    };
    assert!(err.starts_with(&header), "expected {header:?} in:\n{err}");
    for line in lines {
        assert!(err.contains(line), "missing {line:?} in:\n{err}");
    }
}

/// A Job Template with `requiresServices` body `reqs`, `services` body
/// `services` (omitted when empty), Job Environments `job_envs` (a
/// complete block or empty), and one Step `S` whose `onRun` args are
/// `args`.
fn job(reqs: &str, services: &str, job_envs: &str, args: &str) -> String {
    let services = if services.is_empty() {
        String::new()
    } else {
        format!("services:\n{services}")
    };
    format!(
        "specificationVersion: \"jobtemplate-2023-09\"\nextensions: [SERVICE, EXPR]\nname: Req\n\
         {job_envs}requiresServices:\n{reqs}{services}steps:\n  - name: S\n    script:\n      \
         actions:\n        onRun:\n          command: run\n          args: {args}\n"
    )
}

/// `template` with Step `S` listing `service:<Name>` for each of `names`.
fn with_step_deps(template: &str, names: &[&str]) -> String {
    let deps = names
        .iter()
        .map(|n| format!("{{ dependsOn: \"service:{n}\" }}"))
        .collect::<Vec<_>>()
        .join(", ");
    template.replacen(
        "  - name: S\n",
        &format!("  - name: S\n    dependencies: [{deps}]\n"),
        1,
    )
}

const CACHE_REQ: &str = "  - name: Cache\n    ports: [{ name: main }]\n";

fn n_reqs(n: usize) -> String {
    (0..n)
        .map(|i| format!("  - name: R{i}\n    ports: [{{ name: main }}]\n"))
        .collect()
}

fn n_ports(n: usize) -> String {
    let ports: Vec<String> = (0..n).map(|i| format!("{{ name: p{i} }}")).collect();
    format!("  - name: Cache\n    ports: [{}]\n", ports.join(", "))
}

// ════════════════════════════════════════════════════════════════════
// 1. Template validation
// ════════════════════════════════════════════════════════════════════

#[test]
fn requirement_opens_port_and_connect_address_where_listed() {
    // In a Step script and Step Environment (the Step lists `service:Cache`),
    // an inline Service that lists it, and a Job Environment (default
    // runScope: [TASK]), which needs no dependency (§9 scope rules 2–4, §9.8
    // item 2). The inline `Proxy` must likewise be listed by the Step that
    // references it.
    let t = with_step_deps(&job(
        "  - name: Cache\n    ports:\n      - name: main\n      - name: stats\n        protocol: UDP\n",
        "  - name: Proxy\n    dependencies: [{ dependsOn: \"service:Cache\" }]\n    ports: [{ name: main }]\n    variables: { UP: \"{{ Service.Cache.main.connectAddress }}:{{ Service.Cache.main.port }}\" }\n    script: { actions: { onRun: { command: proxy } } }\n",
        "jobEnvironments:\n  - name: Client\n    variables:\n      HOST: \"{{ Service.Cache.main.connectAddress }}\"\n      STATS: \"{{ Service.Cache.stats.port }}\"\n",
        r#"["{{ Service.Cache.main.port }}", "{{ join_host_port(Service.Cache.main.connectAddress, Service.Cache.main.port) }}", "{{ Service.Proxy.main.port }}"]"#,
    ), &["Proxy", "Cache"])
    .replace(
        "  - name: S\n",
        "  - name: S\n    stepEnvironments:\n      - name: StepClient\n        variables: { P: \"{{ Service.Cache.stats.connectAddress }}\" }\n",
    );
    let jt = decode_job(&t);
    let reqs = jt.requires_services();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].name, "Cache");
    assert_eq!(reqs[0].port_names().collect::<Vec<_>>(), ["main", "stats"]);
    assert_eq!(reqs[0].ports[0].protocol, ServicePortProtocol::Tcp);
    assert_eq!(reqs[0].ports[1].protocol, ServicePortProtocol::Udp);
    // The required Service is not a same-document Service: no scope is
    // computed for it, and listing it is not an edge among the inline
    // Services.
    let scopes = compute_service_scopes(&jt).unwrap();
    assert!(scopes.get("Cache").is_none());
    let proxy = scopes.get("Proxy").unwrap();
    assert!(proxy.depends_on_services.is_empty());
    assert_eq!(proxy.scope, ServiceScope::steps(["S"]));
    // The Job Environment's default runScope is [TASK] (it references
    // Service.*).
    assert!(jt.job_environments.as_ref().unwrap()[0].default_run_scope_is_task_only());
}

#[test]
fn step_reference_to_a_required_service_without_dependency_is_rejected() {
    // §9 scope rule 3, §9.8 item 2, §9.9 item 1: the requirement alone does
    // not put the Service in scope for the Step; the message names the fix,
    // exactly as for an inline Service.
    let err = job_err(&job(
        CACHE_REQ,
        "",
        "",
        r#"["{{ Service.Cache.main.connectAddress }}", "{{ Service.Cache.main.port }}"]"#,
    ));
    assert_errs(
        &err,
        2,
        &[
            "steps[0] -> script -> actions -> onRun -> args[0]:\n\tFailed to parse interpolation expression at [",
            "Step 'S' references Service.Cache.main.connectAddress but does not list service:Cache in dependencies.",
            "steps[0] -> script -> actions -> onRun -> args[1]:\n\tFailed to parse interpolation expression at [",
            "Step 'S' references Service.Cache.main.port but does not list service:Cache in dependencies.",
        ],
    );
    assert!(!err.contains("Undefined variable"), "{err}");
}

#[test]
fn step_environment_reference_to_a_required_service_without_dependency_is_rejected() {
    // A Step Environment follows its Step's dependencies (§9 scope rule 3).
    let t = job(CACHE_REQ, "", "", "[x]").replace(
        "  - name: S\n",
        "  - name: S\n    stepEnvironments:\n      - name: Tools\n        variables: { P: \"{{ Service.Cache.main.port }}\" }\n",
    );
    assert_errs(
        &job_err(&t),
        1,
        &[
            "steps[0] -> stepEnvironments[0] -> variables -> P:\n\tFailed to parse interpolation expression at [",
            "Step 'S' references Service.Cache.main.port in stepEnvironments 'Tools' but does not list service:Cache in dependencies.",
        ],
    );
}

#[test]
fn step_reference_to_a_required_service_with_dependency_is_accepted() {
    // Listing `service:Cache` grants access to its values and changes
    // nothing else: the required Service has no computed scope, and the
    // Step's Task Sessions see `port` and `connectAddress`.
    let t = with_step_deps(
        &job(
            CACHE_REQ,
            "",
            "",
            r#"["{{ Service.Cache.main.connectAddress }}", "{{ Service.Cache.main.port }}"]"#,
        ),
        &["Cache"],
    )
    .replace(
        "  - name: S\n",
        "  - name: S\n    stepEnvironments:\n      - name: Tools\n        variables: { P: \"{{ Service.Cache.main.port }}\" }\n",
    );
    let jt = decode_job(&t);
    assert!(compute_service_scopes(&jt).unwrap().get("Cache").is_none());
    let (job, _) = create(&jt, &[]).unwrap();
    assert!(job.services.is_none());
    assert_eq!(job.requires_services.as_ref().unwrap()[0].name, "Cache");
}

#[test]
fn service_reference_to_a_required_service_without_dependency_is_rejected() {
    // §9 scope rule 2, §9.8 item 2: an inline Service lists a required one
    // as it would another inline Service. `S` lists `Proxy` so that `Proxy`
    // is used.
    let err = job_err(&with_step_deps(
        &job(
            CACHE_REQ,
            "  - name: Proxy\n    ports: [{ name: main }]\n    variables: { UP: \"{{ Service.Cache.main.connectAddress }}\" }\n    script: { actions: { onRun: { command: proxy, args: [\"{{ Service.Cache.main.port }}\"] } } }\n",
            "",
            "[x]",
        ),
        &["Proxy"],
    ));
    assert_errs(
        &err,
        2,
        &[
            "services[0] -> variables -> UP:\n\tFailed to parse interpolation expression at [",
            "Service 'Proxy' references Service.Cache.main.connectAddress but does not list service:Cache in dependencies.",
            "services[0] -> script -> actions -> onRun -> args[0]:\n\tFailed to parse interpolation expression at [",
            "Service 'Proxy' references Service.Cache.main.port but does not list service:Cache in dependencies.",
        ],
    );
}

#[test]
fn job_environment_reference_to_a_required_service_needs_no_dependency() {
    // §9 scope rule 4: the one exception — a Job Environment has no
    // `dependencies`. Its runScope defaults to [TASK]. The Step neither
    // lists nor references the Service.
    let jt = decode_job(&job(
        CACHE_REQ,
        "",
        "jobEnvironments:\n  - name: Client\n    variables:\n      HOST: \"{{ Service.Cache.main.connectAddress }}\"\n      PORT: \"{{ Service.Cache.main.port }}\"\n",
        "[x]",
    ));
    assert!(jt.job_environments.as_ref().unwrap()[0].default_run_scope_is_task_only());
    assert!(compute_service_scopes(&jt).unwrap().get("Cache").is_none());
}

#[test]
fn unused_requirement_is_accepted() {
    // §9.8: a requirement nothing lists or references is not an error,
    // unlike an unused inline Service (§9.1 rule 4); it is still matched at
    // submission.
    let t = job(CACHE_REQ, "", "", "[x]");
    let jt = decode_job(&t);
    assert_eq!(jt.requires_services()[0].name, "Cache");
    let (created, _) = create(&jt, &[]).unwrap();
    assert_eq!(created.requires_services.as_ref().unwrap().len(), 1);
    let et = decode_env(&provider("Cache", "[{ name: main }]"));
    let (_, applied) = submit(&jt, std::slice::from_ref(&et), &[]).unwrap();
    assert_eq!(applied.requirement_bindings.len(), 1);
    assert_eq!(
        submit_err(&t, &[], &[]),
        submission_err(
            0,
            "required Service 'Cache' is not provided: no Environment Template is attached (Template Schemas §1.2.2 item 2)."
        )
    );
}

#[test]
fn rfc_required_queue_cache_example_validates() {
    let jt = decode_job(RFC_REQUIRED);
    assert!(jt.services.is_none());
    assert_eq!(jt.requires_services()[0].name, "Cache");
}

#[test]
fn requires_services_must_not_be_empty() {
    assert_eq!(
        job_err(&job("  []\n", "", "", "[x]").replace("requiresServices:\n  []\n", "requiresServices: []\n")),
        "Model validation error: 1 validation error for JobTemplate\nrequiresServices:\n\tmust not be empty."
    );
}

#[test]
fn requires_services_at_most_ten() {
    decode_job(&job(&n_reqs(10), "", "", "[x]"));
    assert_eq!(
        job_err(&job(&n_reqs(11), "", "", "[x]")),
        "Model validation error: 1 validation error for JobTemplate\nrequiresServices:\n\tmust not contain more than 10 elements."
    );
}

#[test]
fn requirement_names_must_be_unique() {
    assert_eq!(
        job_err(&job(&format!("{CACHE_REQ}{CACHE_REQ}"), "", "", "[x]")),
        "Model validation error: 1 validation error for JobTemplate\nrequiresServices[1]:\n\tduplicate service requirement name: 'Cache'"
    );
}

#[test]
fn requirement_name_must_not_also_be_an_inline_service() {
    assert_eq!(
        job_err(&with_step_deps(
            &job(
                CACHE_REQ,
                "  - name: Cache\n    ports: [{ name: main }]\n    script: { actions: { onRun: { command: serve } } }\n",
                "",
                "[x]"
            ),
            &["Cache"]
        )),
        "Model validation error: 1 validation error for JobTemplate\nrequiresServices[0] -> name:\n\t'Cache' is also declared in services; a Service is either declared or required, not both."
    );
}

#[test]
fn requirement_name_must_be_an_identifier_and_not_file() {
    assert_eq!(
        job_err(&job("  - name: File\n    ports: [{ name: main }]\n", "", "", "[x]")),
        "Model validation error: 1 validation error for JobTemplate\nrequiresServices[0] -> name:\n\tmust not be 'File'; it is reserved for Service.File.* references."
    );
    assert_eq!(
        job_err(&job("  - name: \"my-cache\"\n    ports: [{ name: main }]\n", "", "", "[x]")),
        "Model validation error: 1 validation error for JobTemplate\nrequiresServices[0] -> name:\n\t'my-cache' is not a valid identifier."
    );
}

#[test]
fn requirement_ports_must_not_be_empty_or_exceed_ten() {
    assert_eq!(
        job_err(&job("  - name: Cache\n    ports: []\n", "", "", "[x]")),
        "Model validation error: 1 validation error for JobTemplate\nrequiresServices[0] -> ports:\n\tmust not be empty."
    );
    decode_job(&job(&n_ports(10), "", "", "[x]"));
    assert_eq!(
        job_err(&job(&n_ports(11), "", "", "[x]")),
        "Model validation error: 1 validation error for JobTemplate\nrequiresServices[0] -> ports:\n\tmust not contain more than 10 elements."
    );
}

#[test]
fn requirement_port_names_must_be_unique_identifiers_and_not_file() {
    assert_eq!(
        job_err(&job("  - name: Cache\n    ports: [{ name: main }, { name: main, protocol: UDP }]\n", "", "", "[x]")),
        "Model validation error: 1 validation error for JobTemplate\nrequiresServices[0] -> ports[1]:\n\tduplicate port name 'main'."
    );
    assert_eq!(
        job_err(&job("  - name: Cache\n    ports: [{ name: File }]\n", "", "", "[x]")),
        "Model validation error: 1 validation error for JobTemplate\nrequiresServices[0] -> ports[0] -> name:\n\tmust not be 'File'; it is reserved for Service.File.* references."
    );
    assert_eq!(
        job_err(&job("  - name: Cache\n    ports: [{ name: \"9main\" }]\n", "", "", "[x]")),
        "Model validation error: 1 validation error for JobTemplate\nrequiresServices[0] -> ports[0] -> name:\n\t'9main' is not a valid identifier."
    );
}

#[test]
fn requirement_rejects_unknown_fields_and_protocols() {
    let err = job_err(&job(
        "  - name: Cache\n    ports: [{ name: main }]\n    optional: true\n",
        "",
        "",
        "[x]",
    ));
    assert!(err.contains("unknown field `optional`"), "{err}");
    let err = job_err(&job(
        "  - name: Cache\n    ports: [{ name: main, protocol: SCTP }]\n",
        "",
        "",
        "[x]",
    ));
    assert!(err.contains("unknown variant `SCTP`"), "{err}");
}

#[test]
fn undeclared_port_of_a_required_service_is_rejected() {
    // The Step lists `service:Cache`, so the port is what is wrong.
    let err = job_err(&with_step_deps(
        &job(CACHE_REQ, "", "", r#"["{{ Service.Cache.stats.port }}"]"#),
        &["Cache"],
    ));
    assert_errs(
        &err,
        1,
        &[
            "steps[0] -> script -> actions -> onRun -> args[0]:\n\tFailed to parse interpolation expression at [",
            "required Service 'Cache' has no port 'stats'; declared ports: main.",
        ],
    );
}

#[test]
fn bind_address_of_a_required_service_is_rejected_everywhere() {
    // In a Step script and in an inline Service alike, both of which list
    // `service:Cache`; `S` lists `Proxy` so that `Proxy` is used.
    let err = job_err(&with_step_deps(
        &job(
            CACHE_REQ,
            "  - name: Proxy\n    dependencies: [{ dependsOn: \"service:Cache\" }]\n    ports: [{ name: main }]\n    variables: { B: \"{{ Service.Cache.main.bindAddress }}\" }\n    script: { actions: { onRun: { command: proxy } } }\n",
            "",
            r#"["{{ Service.Cache.main.bindAddress }}"]"#,
        ),
        &["Proxy", "Cache"],
    ));
    assert_errs(
        &err,
        2,
        &[
            "services[0] -> variables -> B:\n\tFailed to parse interpolation expression at [",
            "steps[0] -> script -> actions -> onRun -> args[0]:\n\tFailed to parse interpolation expression at [",
            "bindAddress of required Service 'Cache' is not available; use connectAddress to reach it.",
        ],
    );
}

const LET_MSG: &str = "steps[0] -> let[0]:\n\tInvalid expression in let binding 'p': Service.* is \
    not available in a let binding: it is resolved at job creation, before any Service has an \
    endpoint.";
const HOST_REQ_PATH: &str =
    "steps[0] -> hostRequirements -> attributes[0] -> anyOf[0]:\n\tFailed to parse interpolation expression at [";
const HOST_REQ_MSG: &str = "Service.* is not available in hostRequirements: it is resolved at job \
    creation, before any Service has an endpoint.";
const STEP_LET: &str = "    let: [\"p = Service.Cache.main.port\"]\n";
const STEP_HOST_REQ: &str = "    hostRequirements:\n      attributes:\n        - name: attr.worker.os.family\n          anyOf: [\"{{ Service.Cache.main.connectAddress }}\"]\n";

fn with_step_fields(fields: &str) -> String {
    job(CACHE_REQ, "", "", "[x]").replace("  - name: S\n", &format!("  - name: S\n{fields}"))
}

#[test]
fn required_service_values_are_not_in_a_step_let() {
    // §9.9 item 2: never in a <StepTemplate>.let.
    assert_errs(&job_err(&with_step_fields(STEP_LET)), 1, &[LET_MSG]);
}

#[test]
fn required_service_values_are_not_in_step_host_requirements() {
    // §9.9 item 2: never in hostRequirements.
    assert_errs(
        &job_err(&with_step_fields(STEP_HOST_REQ)),
        1,
        &[HOST_REQ_PATH, HOST_REQ_MSG],
    );
}

#[test]
fn step_let_and_host_requirements_errors_are_each_reported_once() {
    // Both at once: two errors, not three (the let error must not be
    // reported twice). Not specific to Service.*: any failing step `let`
    // is reported a second time when the Step has `hostRequirements`.
    let err = job_err(&with_step_fields(&format!("{STEP_LET}{STEP_HOST_REQ}")));
    assert_errs(&err, 2, &[LET_MSG, HOST_REQ_PATH, HOST_REQ_MSG]);
    assert_eq!(err.matches(LET_MSG).count(), 1, "{err}");
}

#[test]
fn required_service_values_are_not_in_an_environment_entered_in_service_sessions() {
    let err = job_err(&job(
        CACHE_REQ,
        "",
        "jobEnvironments:\n  - name: Client\n    runScope: [TASK, SERVICE]\n    variables: { H: \"{{ Service.Cache.main.connectAddress }}\" }\n",
        "[x]",
    ));
    assert_errs(
        &err,
        1,
        &[
            "jobEnvironments[0] -> variables -> H:\n\tFailed to parse interpolation expression at [",
            "Environment 'Client' is entered in Service Sessions (its runScope includes SERVICE) and may not reference Service.*; declare runScope: [TASK] if it configures Tasks.",
        ],
    );
}

#[test]
fn requires_services_rejected_in_an_environment_template() {
    let err = decode_environment_template(
        yaml_val(
            "specificationVersion: \"environment-2023-09\"\nextensions: [SERVICE, EXPR]\nrequiresServices:\n  - name: Cache\n    ports: [{ name: main }]\nenvironment:\n  name: E\n  variables: { K: v }\n",
        ),
        Some(EXTS),
        &CallerLimits::default(),
    )
    .expect_err("requiresServices is permitted only in a Job Template")
    .to_string();
    assert!(
        err.starts_with("Validation error: 'environment-2023-09' failed checks: unknown field `requiresServices`, expected one of "),
        "{err}"
    );
}

#[test]
fn requires_services_without_the_service_extension_is_rejected() {
    let t = job(CACHE_REQ, "", "", "[x]")
        .replace("extensions: [SERVICE, EXPR]\n", "extensions: [EXPR]\n");
    assert_eq!(
        job_err(&t),
        "Model validation error: 1 validation error for JobTemplate\nrequiresServices:\n\trequiresServices requires the SERVICE extension."
    );
}

// ════════════════════════════════════════════════════════════════════
// 2. Job creation
// ════════════════════════════════════════════════════════════════════

fn create(
    jt: &JobTemplate,
    ets: &[EnvironmentTemplate],
) -> Result<(job::Job, openjd_model::JobParameterValues), ModelError> {
    let td = tempfile::TempDir::new().unwrap();
    let dir = td.path().to_str().unwrap();
    let processed = openjd_model::preprocess_job_parameters(
        jt,
        &JobParameterInputValues::default(),
        ets,
        &openjd_model::PathParameterOptions::new(dir, dir),
    )?;
    let job = create_job(jt, &processed, &jt.default_validation_context())?;
    Ok((job, processed))
}

#[test]
fn job_carries_the_requirements_and_serializes_them() {
    let t = with_step_deps(
        &job(
            "  - name: Cache\n    ports:\n      - name: main\n      - name: stats\n        protocol: UDP\n",
            "",
            "",
            r#"["{{ Service.Cache.main.port }}"]"#,
        ),
        &["Cache"],
    );
    let jt = decode_job(&t);
    let (job, _) = create(&jt, &[]).unwrap();
    let reqs = job.requires_services.as_ref().expect("requires_services");
    assert_eq!(
        reqs,
        &vec![job::ServiceRequirement {
            name: "Cache".into(),
            ports: vec![
                job::ServiceRequirementPort {
                    name: "main".into(),
                    protocol: ServicePortProtocol::Tcp,
                },
                job::ServiceRequirementPort {
                    name: "stats".into(),
                    protocol: ServicePortProtocol::Udp,
                },
            ],
        }]
    );
    assert!(job.services.is_none());
    let json = serde_json::to_value(&job).unwrap();
    assert_eq!(json["requiresServices"][0]["name"], "Cache");
    assert_eq!(json["requiresServices"][0]["ports"][0]["name"], "main");
    assert_eq!(json["requiresServices"][0]["ports"][1]["protocol"], "UDP");
    let back: Vec<job::ServiceRequirement> =
        serde_json::from_value(json["requiresServices"].clone()).unwrap();
    assert_eq!(Some(back), job.requires_services);
}

// ════════════════════════════════════════════════════════════════════
// 3. Submission — matching requirements to attached Services
// ════════════════════════════════════════════════════════════════════

/// Submits `jt` with `ets` attached, labelling attachment `i` with
/// `labels[i]` when given.
fn submit(
    jt: &JobTemplate,
    ets: &[EnvironmentTemplate],
    labels: &[&str],
) -> Result<(job::Job, AppliedEnvironmentTemplates), ModelError> {
    let (job, processed) = create(jt, ets)?;
    let attached: Vec<AttachedEnvironmentTemplate<'_>> = ets
        .iter()
        .enumerate()
        .map(|(i, et)| match labels.get(i) {
            Some(label) => AttachedEnvironmentTemplate::new(et).with_label(label),
            None => AttachedEnvironmentTemplate::new(et),
        })
        .collect();
    let applied =
        apply_environment_templates(&job, &attached, &processed, &CallerLimits::default())?;
    Ok((job, applied))
}

fn submit_err(jt: &str, ets: &[&str], labels: &[&str]) -> String {
    let jt = decode_job(jt);
    let ets: Vec<EnvironmentTemplate> = ets.iter().map(|t| decode_env(t)).collect();
    submit(&jt, &ets, labels)
        .expect_err("submission should be rejected")
        .to_string()
}

/// A services-only attachment defining `name` with `ports` (a flow list).
fn provider(name: &str, ports: &str) -> String {
    format!(
        "specificationVersion: \"environment-2023-09\"\nextensions: [SERVICE, EXPR]\nservices:\n  - name: {name}\n    ports: {ports}\n    healthCheck: {{ type: STDOUT }}\n    script:\n      actions:\n        onRun: {{ command: serve }}\n"
    )
}

fn submission_err(index: usize, message: &str) -> String {
    format!(
        "Model validation error: 1 validation error for Submission\nJobTemplate -> requiresServices[{index}]:\n\t{message}"
    )
}

#[test]
fn requirement_matched_to_the_rfc_queue_cache_is_bound() {
    let jt = decode_job(RFC_REQUIRED);
    let et = decode_env(RFC_QUEUE_CACHE);
    let (job, applied) = submit(&jt, std::slice::from_ref(&et), &["queue/cache.yaml"]).unwrap();
    assert_eq!(
        applied.requirement_bindings,
        vec![RequirementBinding {
            requirement: "Cache".into(),
            document: job::Document::environment_template(0, Some("queue/cache.yaml")),
            service: "Cache".into(),
        }]
    );
    let cache = &applied.external_services[0];
    assert_eq!(cache.scope, ServiceScope::AllSteps);
    assert_eq!(
        cache.document,
        job::Document::environment_template(0, Some("queue/cache.yaml"))
    );
    let combined = applied.into_combined_job(job);
    assert_eq!(combined.services.as_ref().unwrap().len(), 1);
    assert!(combined.requires_services.is_some());
}

#[test]
fn requirement_binds_to_the_one_attachment_that_provides_it() {
    // Two attachments, only the second provides `Cache`; extra ports on the
    // provider are fine.
    let jt = decode_job(&with_step_deps(
        &job(CACHE_REQ, "", "", r#"["{{ Service.Cache.main.port }}"]"#),
        &["Cache"],
    ));
    let ets = [
        decode_env(&provider("Other", "[{ name: main }]")),
        decode_env(&provider("Cache", "[{ name: admin }, { name: main }]")),
    ];
    let (_, applied) = submit(&jt, &ets, &[]).unwrap();
    assert_eq!(
        applied.requirement_bindings,
        vec![RequirementBinding {
            requirement: "Cache".into(),
            document: job::Document::environment_template(1, None),
            service: "Cache".into(),
        }]
    );
}

#[test]
fn requirement_with_no_attachment_is_rejected() {
    assert_eq!(
        submit_err(RFC_REQUIRED, &[], &[]),
        submission_err(
            0,
            "required Service 'Cache' is not provided: no Environment Template is attached (Template Schemas §1.2.2 item 2)."
        )
    );
}

#[test]
fn requirement_no_attachment_defines_is_rejected() {
    assert_eq!(
        submit_err(
            RFC_REQUIRED,
            &[&provider("Other", "[{ name: main }]"), &provider("Metrics", "[{ name: main }]")],
            &["queue/other.yaml"],
        ),
        submission_err(
            0,
            "required Service 'Cache' is not provided: none of the attached Environment Templates (queue/other.yaml, EnvironmentTemplate[1]) defines a Service named 'Cache' (Template Schemas §1.2.2 item 2)."
        )
    );
}

#[test]
fn requirement_two_attachments_define_is_ambiguous() {
    assert_eq!(
        submit_err(
            RFC_REQUIRED,
            &[RFC_QUEUE_CACHE, &provider("Cache", "[{ name: main }]")],
            &["queue/a.yaml", "queue/b.yaml"],
        ),
        submission_err(
            0,
            "required Service 'Cache' is ambiguous: 2 attached Environment Templates define a Service with that name (queue/a.yaml, queue/b.yaml); a requirement must match exactly one (Template Schemas §1.2.2 item 2)."
        )
    );
}

#[test]
fn requirement_port_missing_on_the_provider_is_rejected() {
    let jt = job(
        "  - name: Cache\n    ports: [{ name: main }, { name: stats }]\n",
        "",
        "",
        "[x]",
    );
    assert_eq!(
        submit_err(&jt, &[&provider("Cache", "[{ name: main }, { name: other }]")], &[]),
        submission_err(
            0,
            "required Service 'Cache' is provided by EnvironmentTemplate[0], which is missing port 'stats'; its ports: main, other (Template Schemas §1.2.2 item 2)."
        )
    );
}

#[test]
fn requirement_port_protocol_mismatch_is_rejected() {
    // The requirement defaults to TCP; the provider's port is UDP.
    assert_eq!(
        submit_err(
            &job(CACHE_REQ, "", "", "[x]"),
            &[&provider("Cache", "[{ name: main, protocol: UDP }]")],
            &["queue/udp.yaml"],
        ),
        submission_err(
            0,
            "required Service 'Cache' is provided by queue/udp.yaml, whose port 'main' has protocol UDP but the requirement declares TCP (Template Schemas §1.2.2 item 2)."
        )
    );
}

#[test]
fn each_requirement_is_reported_at_its_own_index() {
    let jt = job(
        "  - name: Cache\n    ports: [{ name: main }]\n  - name: Metrics\n    ports: [{ name: main }]\n",
        "",
        "",
        "[x]",
    );
    let err = submit_err(&jt, &[&provider("Cache", "[{ name: main }]")], &[]);
    assert_eq!(
        err,
        submission_err(
            1,
            "required Service 'Metrics' is not provided: none of the attached Environment Templates (EnvironmentTemplate[0]) defines a Service named 'Metrics' (Template Schemas §1.2.2 item 2)."
        )
    );
}

#[test]
fn same_named_attachments_are_accepted_when_not_required() {
    // §1.2.2 item 3: two attached Services with the same name are an error
    // only when a requirement names it.
    let plain = "specificationVersion: \"jobtemplate-2023-09\"\nname: Plain\nsteps:\n  - name: S\n    script: { actions: { onRun: { command: run } } }\n";
    let jt = decode_job(plain);
    let ets = [
        decode_env(&provider("Cache", "[{ name: main }]")),
        decode_env(&provider("Cache", "[{ name: main }]")),
    ];
    let (_, applied) = submit(&jt, &ets, &[]).unwrap();
    assert_eq!(applied.external_services.len(), 2);
    assert!(applied.requirement_bindings.is_empty());
}

#[test]
fn inline_service_shadowing_an_attached_one_is_accepted() {
    // §1.2.2 item 3: the inline `Cache` is what the Job Template's
    // references resolve to (the Step lists `service:Cache`, which names
    // the inline one); the attached one still runs.
    let fixture = include_str!("../fixtures/rfc0009/valkey-shared-store.job.yaml");
    let fixture = if fixture.contains("service:Cache") {
        fixture.to_string()
    } else {
        fixture.replacen(
            "  - name: ProcessFrames\n",
            "  - name: ProcessFrames\n    dependencies: [{ dependsOn: \"service:Cache\" }]\n",
            1,
        )
    };
    let jt = decode_job(&fixture);
    let et = decode_env(RFC_QUEUE_CACHE);
    let (job, applied) = submit(&jt, std::slice::from_ref(&et), &[]).unwrap();
    assert!(applied.requirement_bindings.is_empty());
    let combined = applied.into_combined_job(job);
    let services = combined.services.as_ref().unwrap();
    assert_eq!(services.len(), 2);
    assert_eq!(
        services[0].document,
        job::Document::environment_template(0, None)
    );
    assert_eq!(services[0].scope, ServiceScope::AllSteps);
    assert_eq!(services[1].document, job::Document::JobTemplate);
    assert_eq!(services[1].scope, ServiceScope::steps(["ProcessFrames"]));
}

#[test]
fn service_less_wrapper_attachment_rejected_when_a_required_service_is_attached() {
    // §1.2.2 item 4: the combined Job has a Service (the required, attached
    // `Cache`), and the SERVICE-less wrapper is in the combined
    // jobEnvironments.
    const WRAPPER: &str = "specificationVersion: \"environment-2023-09\"\nextensions: [WRAP_ACTIONS, EXPR]\nenvironment:\n  name: QueueContainer\n  script:\n    actions:\n      onWrapEnvEnter: { command: c }\n      onWrapTaskRun: { command: c }\n      onWrapEnvExit: { command: c }\n";
    let err = submit_err(RFC_REQUIRED, &[WRAPPER, RFC_QUEUE_CACHE], &[]);
    assert_eq!(
        err,
        "Model validation error: 1 validation error for Submission\nEnvironmentTemplate[0] -> environment:\n\t\
         wrapping Environment 'QueueContainer' is defined by EnvironmentTemplate[0], which does not declare the \
         SERVICE extension, so it has the default runScope (every kind of Session) and cannot define the \
         onWrapService* hooks; but the combined Job places external Service 'Cache' (EnvironmentTemplate[1] -> \
         services[0]) in its scope, and the Service would run in a Session the Environment enters but cannot \
         wrap. Declare SERVICE in EnvironmentTemplate[0] and either define onWrapServiceEnter, onWrapServiceRun, \
         onWrapServiceHealthCheck, and onWrapServiceExit, or declare a runScope that excludes SERVICE (RFC \
         0009, Template Schemas §1.2.2 item 4)."
    );
}

#[test]
fn job_template_task_session_values_for_a_bound_requirement() {
    // The parameter-less consumer of the RFC resolves against the bound
    // Service's name: the requirement name equals the attached Service's.
    let jt = decode_job(RFC_REQUIRED);
    let mut input = JobParameterInputValues::default();
    input.insert("CacheMemoryMiB".into(), ExprValue::String("2048".into()));
    let et = decode_env(RFC_QUEUE_CACHE);
    let td = tempfile::TempDir::new().unwrap();
    let dir = td.path().to_str().unwrap();
    let processed = openjd_model::preprocess_job_parameters(
        &jt,
        &input,
        std::slice::from_ref(&et),
        &openjd_model::PathParameterOptions::new(dir, dir),
    )
    .unwrap();
    let job = create_job(&jt, &processed, &jt.default_validation_context()).unwrap();
    let applied = apply_environment_templates(
        &job,
        &[AttachedEnvironmentTemplate::new(&et)],
        &processed,
        &CallerLimits::default(),
    )
    .unwrap();
    let binding = &applied.requirement_bindings[0];
    let bound = applied
        .external_services
        .iter()
        .find(|s| s.name == binding.service && s.document == binding.document)
        .expect("the bound Service is among the external Services");
    assert_eq!(bound.port_names().collect::<Vec<_>>(), ["main"]);
    assert_eq!(
        bound
            .host_requirements
            .as_ref()
            .unwrap()
            .amounts
            .as_ref()
            .unwrap()[0]
            .min,
        Some(2048.0)
    );
}
