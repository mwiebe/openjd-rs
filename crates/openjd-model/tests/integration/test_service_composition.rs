// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Integration tests for the composition of RFC 0009 (`SERVICE`) with RFC
//! 0010 (`ENV_PARAMS`): which Job Environments a Service's Sessions enter
//! and with what values, a Service's `environment:` entries and the scopes
//! they read, the dependency graph's edges from entering an Environment, a
//! Service's scope through `ON_REQUEST` Environments, a Service's combined
//! host requirements, and RFC 0010 `<StepDependency>` constraint 7 for a
//! Service. Each is stated in RFC 0009: `<Service>` items 3–5,
//! `<StepDependency>` item 3 and constraint 3, `<Environment>`
//! *dependencies*, Service scope, and "Services run inside Environments".
//!
//! Wrap hook rule 1 per kind of Session is in `test_service_environments.rs`;
//! the attached-Environment cycles are in `test_service_external.rs`.
//!
//! Error assertions follow the repo convention of asserting on the full
//! Pydantic-style error path + message.

use openjd_model::job::{self, ServiceScope};
use openjd_model::template::JobTemplate;
use openjd_model::{
    check_environment_templates, create_job_with_environment_templates,
    decode_environment_template, decode_job_template, preprocess_job_parameters,
    AttachedEnvironmentTemplate, CallerLimits, JobParameterInputValues, PathParameterOptions,
};

const EXTS: &[&str] = &[
    "EXPR",
    "ENV_PARAMS",
    "SERVICE",
    "WRAP_ACTIONS",
    "FEATURE_BUNDLE_1",
];
const JOB_ERR: &str = "Model validation error: 1 validation error for JobTemplate\n";
const SUBMISSION_ERR: &str = "Model validation error: 1 validation error for Submission\n";

fn yaml_val(s: &str) -> serde_json::Value {
    serde_saphyr::from_str(s).unwrap()
}

fn decode(template: &str) -> Result<JobTemplate, String> {
    decode_job_template(yaml_val(template), Some(EXTS), &CallerLimits::default())
        .map_err(|e| e.to_string())
}

/// Validate `template`, then create its Job with `attachments` attached.
fn create(template: &str, attachments: &[&str]) -> Result<job::Job, String> {
    let jt = decode(template)?;
    let ets: Vec<_> = attachments
        .iter()
        .map(|t| {
            decode_environment_template(yaml_val(t), Some(EXTS), &CallerLimits::default())
                .expect("environment template should validate")
        })
        .collect();
    let td = tempfile::TempDir::new().unwrap();
    let dir = td.path().to_str().unwrap();
    let processed = preprocess_job_parameters(
        &jt,
        &JobParameterInputValues::default(),
        &ets,
        &PathParameterOptions::new(dir, dir),
    )
    .map_err(|e| e.to_string())?;
    let attached: Vec<AttachedEnvironmentTemplate<'_>> = ets.iter().map(Into::into).collect();
    create_job_with_environment_templates(
        &jt,
        &attached,
        &processed,
        &jt.default_validation_context(),
    )
    .map(|c| c.job)
    .map_err(|e| e.to_string())
}

/// A Job Template with Job Parameters `P` (STRING) and `D` (PATH), the
/// `jobEnvironments` list body `envs`, one Service `X` whose `let` binds
/// `l` and `n` (from `Job.EnvNames`) followed by `svc_extra` (indented four
/// spaces), and Steps `A`, which lists `X` and then `step_deps` (a flow-list
/// tail), and `B`, with `b_extra` before its script.
fn job(envs: &str, svc_extra: &str, step_deps: &str, b_extra: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR, WRAP_ACTIONS, FEATURE_BUNDLE_1]
name: J
parameterDefinitions:
  - {{name: P, type: STRING, default: p}}
  - {{name: D, type: PATH, default: dir}}
jobEnvironments:
{envs}services:
  - name: X
    let: [l = 'L' + Job.Name, n = len(Job.EnvNames)]
{svc_extra}    ports: [{{name: main}}]
    healthCheck: {{type: STDOUT}}
    script: {{actions: {{onRun: {{command: run}}}}}}
steps:
  - name: A
    dependencies: [{{service: X}}{step_deps}]
    script: {{actions: {{onRun: {{command: run}}}}}}
  - name: B
{b_extra}    script: {{actions: {{onRun: {{command: run}}}}}}
"
    )
}

/// An `ON_REQUEST` Job Environment `E` with one STRING parameter `V`.
const ON_REQUEST_V: &str = "  - name: E
    activation: ON_REQUEST
    envParameterDefinitions: [{name: V, type: STRING, default: d}]
    variables: {K: \"{{Env.Param.V}}\"}
";

/// A Job Environment `E` (with `activation`, indented four spaces) that
/// lists `X`.
fn listing_x(activation: &str) -> String {
    format!(
        "  - name: E\n{activation}    dependencies: [{{service: X}}]\n    variables: {{K: \"{{{{Service.X.main.port}}}}\"}}\n"
    )
}

fn values(svc: &job::Service) -> Vec<(String, String)> {
    svc.environment_parameters
        .iter()
        .flatten()
        .flat_map(|v| {
            v.values
                .as_value()
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|e| {
                    (
                        format!("{}.{}", v.environment, e["name"].as_str().unwrap()),
                        e["value"].as_str().unwrap().to_string(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

// ════════════════════════════════════════════════════════════════════
// <Service> item 4: the scopes an environment: entry reads
// ════════════════════════════════════════════════════════════════════

/// `parameters` values and `enableIf` may read `RawParam.*`, non-PATH
/// `Param.*`, `Job.Name`, `Job.EnvNames`, and the Service's own `let`
/// (which may itself read `Job.EnvNames`, item 3); the Service's Session
/// enters the Environment it requests with those values.
#[test]
fn a_services_environment_entry_reads_the_service_scopes() {
    let t = job(
        ON_REQUEST_V,
        "    dependencies: [{environment: E, parameters: {V: \"{{RawParam.P}}{{Param.P}}{{Job.Name}}{{l}}{{n}}{{Job.EnvNames[0]}}\"}, enableIf: \"len(Job.EnvNames) > 0 and l != ''\"}]\n",
        "",
        "",
    );
    let job = create(&t, &[]).unwrap();
    let x = &job.services.as_ref().unwrap()[0];
    assert_eq!(values(x), [("E.V".to_string(), "ppJLJ1E".to_string())]);
    assert!(x.enters_environment(&job::Document::JobTemplate, "E"));
    // E is requested by X alone.
    assert!(!job.steps[0].enters_environment(&job::Document::JobTemplate, "E"));
}

#[test]
fn a_services_environment_entry_cannot_read_step_name() {
    assert_eq!(
        decode(&job(
            ON_REQUEST_V,
            "    dependencies: [{environment: E, parameters: {V: \"{{Step.Name}}\"}}]\n",
            "",
            ""
        ))
        .unwrap_err(),
        format!(
            "{JOB_ERR}services[0] -> dependencies[0] -> parameters -> V:\n\tFailed to parse \
             interpolation expression at [0, 13]. Step.* is not available within a Service.\n  \
             Step.Name\n  ~~~~~^~~~"
        )
    );
    assert_eq!(
        decode(&job(
            ON_REQUEST_V,
            "    dependencies: [{environment: E, enableIf: \"Step.Name == 'x'\"}]\n",
            "",
            ""
        ))
        .unwrap_err(),
        format!(
            "{JOB_ERR}services[0] -> dependencies[0] -> enableIf:\n\tInvalid expression: Step.* \
             is not available within a Service.\n  Step.Name == 'x'\n  ~~~~~^~~~"
        )
    );
}

#[test]
fn a_services_environment_entry_cannot_read_a_path_parameter() {
    // As for a Step (RFC 0010 `<StepDependency>` item 6): a PATH value is
    // host-specific, and a Service is placed on its own host.
    assert_eq!(
        decode(&job(
            ON_REQUEST_V,
            "    dependencies: [{environment: E, parameters: {V: \"{{Param.D}}\"}}]\n",
            "",
            ""
        ))
        .unwrap_err(),
        format!(
            "{JOB_ERR}services[0] -> dependencies[0] -> parameters -> V:\n\tFailed to parse \
             interpolation expression at [0, 11]. Undefined variable: 'Param.D'. Did you mean: \
             Param.P\n  Param.D\n  ~~~~~~^"
        )
    );
}

// ════════════════════════════════════════════════════════════════════
// <StepDependency> constraint 3: a Service's own entry for an Environment
// that lists it is the cycle X -> X, whatever the activation
// ════════════════════════════════════════════════════════════════════

/// The cycle `service -> service` that entering `environment` (as messages
/// name it, such as `Job Environment 'E'`), which lists the Service, makes.
fn self_cycle(service: &str, environment: &str) -> String {
    format!(
        "dependencies contain a cycle: Service {service} -> Service {service} (the Sessions of \
         Service {service} enter {environment}, which lists Service '{service}', so it would \
         wait for itself to be READY. There is no exception for an Environment that lists the \
         Service: keep the Service's Sessions out of it: drop any 'environment:' dependency of the Service that requests it, and decline a BY_DEFAULT one with a disabled 'environment:' dependency of the Service, or make it ON_REQUEST and request it only from the Steps that need it; Template Schemas §3.2 \
         constraint 3)."
    )
}

/// [`self_cycle`] for `X` and the Job Environment `E`.
fn own_entry_cycle() -> String {
    self_cycle("X", "Job Environment 'E'")
}

#[test]
fn a_services_own_entry_for_a_by_default_environment_that_lists_it_is_a_cycle() {
    // There is no exception for an Environment that lists the Service, with
    // or without an entry; an enabled entry cannot decline it.
    assert_eq!(
        decode(&job(
            &listing_x(""),
            "    dependencies: [{environment: E}]\n",
            "",
            ""
        ))
        .unwrap_err(),
        format!("{JOB_ERR}JobTemplate: {}", own_entry_cycle())
    );
}

#[test]
fn a_services_own_entry_for_an_on_request_environment_that_lists_it_is_a_cycle() {
    assert_eq!(
        decode(&job(
            &listing_x("    activation: ON_REQUEST\n"),
            "    dependencies: [{environment: E}]\n",
            ", {environment: E}",
            ""
        ))
        .unwrap_err(),
        format!("{JOB_ERR}JobTemplate: {}", own_entry_cycle())
    );
}

#[test]
fn a_cycle_through_an_expression_enable_if_is_reported_at_job_creation() {
    // The edge depends on an `enableIf` expression, so template validation
    // leaves it to job creation, which reports it once decided.
    let t = job(
        &listing_x(""),
        "    dependencies: [{environment: E, enableIf: \"Job.Name == 'J'\"}]\n",
        "",
        "",
    );
    decode(&t).unwrap();
    assert_eq!(
        create(&t, &[]).unwrap_err(),
        format!("{JOB_ERR}JobTemplate: {}", own_entry_cycle())
    );
    // Decided false: no edge, and the Job is created.
    let t = t.replace("Job.Name == 'J'", "Job.Name == 'K'");
    let job = create(&t, &[]).unwrap();
    assert_eq!(job.services.as_ref().unwrap()[0].environments, Some(vec![]));
}

/// `<StepDependency>` constraint 3, cross-Service: Front lists Back, and a
/// `BY_DEFAULT` client Environment lists Front, so Back's Sessions enter it and
/// Back waits for Front. Making the client Environment `ON_REQUEST`,
/// requested by the Steps, is the remedy.
#[test]
fn a_by_default_client_environment_of_a_dependent_service_is_a_cycle_and_on_request_breaks_it() {
    let t = |activation: &str, step_deps: &str| {
        format!(
            "specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
name: J
jobEnvironments:
  - name: FrontClient
{activation}    dependencies: [{{service: Front}}]
    variables: {{F: \"{{{{Service.Front.main.port}}}}\"}}
services:
  - name: Front
    dependencies: [{{service: Back}}]
    ports: [{{name: main}}]
    script: {{actions: {{onRun: {{command: run, args: [\"{{{{Service.Back.main.port}}}}\"]}}}}}}
  - name: Back
    ports: [{{name: main}}]
    script: {{actions: {{onRun: {{command: run}}}}}}
steps:
  - name: A
{step_deps}    script: {{actions: {{onRun: {{command: run}}}}}}
"
        )
    };
    assert_eq!(
        decode(&t("", "")).unwrap_err(),
        format!(
            "{JOB_ERR}JobTemplate: dependencies contain a cycle: Service Front -> Service Back -> \
             Service Front (the Sessions of Service Back enter Job Environment 'FrontClient', \
             which lists Service 'Front', so Service Back depends on it; Template Schemas §3.2 \
             constraint 3)."
        )
    );
    let remedy = t(
        "    activation: ON_REQUEST\n",
        "    dependencies: [{environment: FrontClient}]\n",
    );
    let job = create(&remedy, &[]).unwrap();
    let services = job.services.as_ref().unwrap();
    for svc in services {
        assert_eq!(svc.scope, ServiceScope::AllSteps, "{}", svc.name);
        assert_eq!(svc.environments, Some(vec![]), "{}", svc.name);
    }
    assert!(job.steps[0].enters_environment(&job::Document::JobTemplate, "FrontClient"));
}

// ════════════════════════════════════════════════════════════════════
// Service scope rules 3–4 through ON_REQUEST Environments
// ════════════════════════════════════════════════════════════════════

#[test]
fn an_on_request_environment_puts_the_requesting_steps_in_a_listed_services_scope() {
    // E lists X and only Step A requests it; neither Step lists X.
    let t = job(&listing_x("    activation: ON_REQUEST\n"), "", "", "").replace(
        "    dependencies: [{service: X}]\n    script",
        "    dependencies: [{environment: E}]\n    script",
    );
    let jt = decode(&t).unwrap();
    let scopes = openjd_model::template::compute_service_scopes(&jt).unwrap();
    assert_eq!(scopes.get("X").unwrap().scope, ServiceScope::steps(["A"]));
    let job = create(&t, &[]).unwrap();
    assert_eq!(
        job.services.as_ref().unwrap()[0].scope,
        ServiceScope::steps(["A"])
    );
}

#[test]
fn an_inline_service_listed_only_by_an_unrequested_on_request_environment_is_unused() {
    let t = job(&listing_x("    activation: ON_REQUEST\n"), "", "", "")
        .replace("    dependencies: [{service: X}]\n    script", "    script");
    assert_eq!(
        decode(&t).unwrap_err(),
        format!(
            "{JOB_ERR}services[0]:\n\tService 'X' is unused: no Step is in its scope (Job \
             Environment 'E' lists it, but no Step's Sessions enter it)."
        )
    );
}

#[test]
fn a_scope_that_depends_on_an_enable_if_expression_is_decided_at_job_creation() {
    // Step A requests E through an entry whose enableIf is an expression:
    // validation keeps X (it may be used); job creation decides.
    let t = job(&listing_x("    activation: ON_REQUEST\n"), "", "", "").replace(
        "    dependencies: [{service: X}]\n    script",
        "    dependencies: [{environment: E, enableIf: \"Job.Name == 'J'\"}]\n    script",
    );
    let jt = decode(&t).unwrap();
    let scopes = openjd_model::template::compute_service_scopes(&jt).unwrap();
    let x = scopes.get("X").unwrap();
    assert!(!x.is_unused());
    assert_eq!(x.scope, ServiceScope::steps::<[&str; 0], &str>([]));
    let job = create(&t, &[]).unwrap();
    assert_eq!(
        job.services.as_ref().unwrap()[0].scope,
        ServiceScope::steps(["A"])
    );
}

// ════════════════════════════════════════════════════════════════════
// "Services run inside Environments": own values, enableIf
// ════════════════════════════════════════════════════════════════════

#[test]
fn a_service_session_enters_a_by_default_environment_with_its_own_values() {
    let env = "  - name: E
    envParameterDefinitions: [{name: V, type: STRING, default: d}]
    variables: {K: \"{{Env.Param.V}}\"}
";
    let created = create(
        &job(
            env,
            "    dependencies: [{environment: E, parameters: {V: \"{{l}}\"}}]\n",
            ", {environment: E, parameters: {V: a}}",
            "",
        ),
        &[],
    )
    .unwrap();
    let x = &created.services.as_ref().unwrap()[0];
    assert_eq!(values(x), [("E.V".to_string(), "LJ".to_string())]);
    // A Service that supplies nothing enters it with the default.
    let created = create(&job(env, "", "", ""), &[]).unwrap();
    let x = &created.services.as_ref().unwrap()[0];
    assert_eq!(values(x), [("E.V".to_string(), "d".to_string())]);
}

#[test]
fn an_environments_enable_if_excludes_a_service_by_the_services_values() {
    let env = "  - name: E
    envParameterDefinitions: [{name: Flag, type: STRING, default: up}]
    enableIf: \"Env.Param.Flag == 'up'\"
    variables: {K: v}
";
    let job = create(
        &job(
            env,
            "    dependencies: [{environment: E, parameters: {Flag: down}}]\n",
            "",
            "",
        ),
        &[],
    )
    .unwrap();
    let x = &job.services.as_ref().unwrap()[0];
    assert_eq!(x.environments, Some(vec![]));
    assert!(x.environment_parameters.is_none());
    // The Steps enter it with the default.
    for step in &job.steps {
        assert!(step.enters_environment(&job::Document::JobTemplate, "E"));
    }
}

// ════════════════════════════════════════════════════════════════════
// <Service> item 4: RFC 0010 constraint 7 for a Service
// ════════════════════════════════════════════════════════════════════

/// The constraint 7 error for the Job Template's Service `X` (`services[0]`)
/// over the Job Environments, without attachments, listing `missing`.
fn job_service_must_supply(missing: &str) -> String {
    format!(
        "{JOB_ERR}services[0]:\n\tmust supply, in an 'environment:' dependency, every parameter \
         without a default of each Environment that applies to it (every jobEnvironments entry \
         with activation ALWAYS, each one with activation BY_DEFAULT that the Service does not \
         decline, and each one with activation ON_REQUEST that the Service requests; Template \
         Schemas §3.2 constraint 7, RFC 0009 <Service> item 4), whatever that Environment's own \
         enableIf is; missing: {missing}."
    )
}

#[test]
fn a_service_supplies_every_required_parameter_of_each_environment_it_enters() {
    let env = "  - name: E
    envParameterDefinitions: [{name: R, type: STRING}]
    variables: {K: \"{{Env.Param.R}}\"}
";
    let b = "    dependencies: [{environment: E, parameters: {R: b}}]\n";
    assert_eq!(
        decode(&job(env, "", ", {environment: E, parameters: {R: s}}", b)).unwrap_err(),
        job_service_must_supply("'R' of Environment 'E'")
    );
    let created = create(
        &job(
            env,
            "    dependencies: [{environment: E, parameters: {R: x}}]\n",
            ", {environment: E, parameters: {R: s}}",
            b,
        ),
        &[],
    )
    .unwrap();
    assert_eq!(
        values(&created.services.as_ref().unwrap()[0]),
        [("E.R".to_string(), "x".to_string())]
    );
    // A BY_DEFAULT Environment that lists the Service must be declined by it
    // (entering it is the cycle X -> X), and then the Service need not
    // supply its parameters.
    let listing = "  - name: E
    envParameterDefinitions: [{name: R, type: STRING}]
    dependencies: [{service: X}]
    variables: {K: \"{{Env.Param.R}}\"}
";
    decode(&job(
        listing,
        "    dependencies: [{environment: E, enableIf: false}]\n",
        ", {environment: E, parameters: {R: s}}",
        b,
    ))
    .unwrap();
}

// ════════════════════════════════════════════════════════════════════
// <Service> item 5: combined host requirements
// ════════════════════════════════════════════════════════════════════

#[test]
fn a_services_combined_host_requirements_use_its_own_values() {
    let env = "  - name: E
    envParameterDefinitions: [{name: M, type: INT, default: 1024}]
    hostRequirements: {amounts: [{name: amount.worker.memory, min: \"{{Env.Param.M}}\"}]}
    variables: {K: v}
";
    let job = create(
        &job(
            env,
            "    dependencies: [{environment: E, parameters: {M: 2048}}]\n    hostRequirements: {attributes: [{name: attr.worker.preemptible, anyOf: [\"false\"]}]}\n",
            "",
            "",
        ),
        &[],
    )
    .unwrap();
    let hr = job.services.as_ref().unwrap()[0]
        .host_requirements
        .as_ref()
        .unwrap();
    let amounts = hr.amounts.as_ref().unwrap();
    assert_eq!(amounts.len(), 1);
    assert_eq!(amounts[0].name, "amount.worker.memory");
    assert_eq!(amounts[0].min, Some(2048.0));
    assert_eq!(
        hr.attributes.as_ref().unwrap()[0].name,
        "attr.worker.preemptible"
    );
    // The Steps' own combination uses their own values (the default here).
    let step_hr = job.steps[0].host_requirements.as_ref().unwrap();
    assert_eq!(step_hr.amounts.as_ref().unwrap()[0].min, Some(1024.0));
}

#[test]
fn a_services_combined_host_requirement_conflict_rejects_the_submission_naming_it() {
    let env = "  - name: E
    hostRequirements: {amounts: [{name: amount.worker.memory, max: 4096}]}
    variables: {K: v}
";
    assert_eq!(
        create(
            &job(
                env,
                "    hostRequirements: {amounts: [{name: amount.worker.memory, min: 8192}]}\n",
                "",
                ""
            ),
            &[]
        )
        .unwrap_err(),
        format!(
            "{JOB_ERR}services[0]:\n\tService 'X': no host can satisfy the combined host \
             requirements for amount 'amount.worker.memory': services[0] -> hostRequirements \
             requires min 8192, and jobEnvironments[0] -> hostRequirements (Environment 'E') \
             requires max 4096. Template Schemas §3.3.3."
        )
    );
}

// ════════════════════════════════════════════════════════════════════
// Wrap hook rule 1 at job creation, and Environment Template item 4
// ════════════════════════════════════════════════════════════════════

const THREE_HOOK_WRAPPER: &str = "    script:
      actions:
        onWrapEnvEnter: {command: c}
        onWrapTaskRun: {command: c}
        onWrapEnvExit: {command: c}
";

const SERVICE_HOOKS_MISSING: &str = "which therefore must define onWrapEnvEnter, onWrapEnvExit, \
     onWrapServiceEnter, onWrapServiceRun, onWrapServiceHealthCheck, and onWrapServiceExit; \
     missing: onWrapServiceEnter, onWrapServiceRun, onWrapServiceHealthCheck, onWrapServiceExit \
     (RFC 0009 wrap hook rule 1, Template Schemas §1.2.2 item 4).";

#[test]
fn a_wrapper_a_service_requests_through_an_enable_if_expression_is_checked_at_job_creation() {
    let env = format!("  - name: W\n    activation: ON_REQUEST\n{THREE_HOOK_WRAPPER}");
    let t = job(
        &env,
        "    dependencies: [{environment: W, enableIf: \"Job.Name == 'J'\"}]\n",
        "",
        "",
    );
    decode(&t).unwrap();
    assert_eq!(
        create(&t, &[]).unwrap_err(),
        format!(
            "{JOB_ERR}jobEnvironments[0] -> script -> actions:\n\tthe Sessions of Service 'X' \
             enter wrapping Environment 'W', defined by the Job Template, {SERVICE_HOOKS_MISSING}"
        )
    );
}

/// Environment Template item 4: an attached wrapping Environment whose
/// document declares `SERVICE` but which defines only RFC 0008's three
/// hooks validates on its own, and is rejected at submission when a
/// Service with a non-empty scope enters it, naming the Environment, its
/// document, and the Service.
#[test]
fn an_attached_service_declaring_three_hook_wrapper_is_rejected_at_submission() {
    let wrapper = format!(
        "specificationVersion: environment-2023-09\nextensions: [SERVICE, ENV_PARAMS, EXPR, WRAP_ACTIONS]\nenvironment:\n  name: W\n{}",
        THREE_HOOK_WRAPPER
            .lines()
            .map(|l| format!("{}\n", &l[2..]))
            .collect::<String>()
    );
    decode_environment_template(yaml_val(&wrapper), Some(EXTS), &CallerLimits::default()).unwrap();
    let t = job("  - name: E\n    variables: {K: v}\n", "", "", "");
    assert_eq!(
        create(&t, &[&wrapper]).unwrap_err(),
        format!(
            "{SUBMISSION_ERR}EnvironmentTemplate[0] -> environment -> script -> actions:\n\tthe \
             Sessions of Service 'X' enter wrapping Environment 'W', defined by \
             EnvironmentTemplate[0], {SERVICE_HOOKS_MISSING}"
        )
    );
    // ON_REQUEST, requested by the Steps only: no Service Session enters it.
    let on_request = wrapper.replace("  name: W\n", "  name: W\n  activation: ON_REQUEST\n");
    let t = job(
        "  - name: E\n    variables: {K: v}\n",
        "",
        ", {environment: W}",
        "",
    );
    let job = create(&t, &[&on_request]).unwrap();
    let x = &job.services.as_ref().unwrap()[0];
    assert!(!x.enters_environment(&job::Document::environment_template(0, None), "W"));
}

// ════════════════════════════════════════════════════════════════════
// <StepDependency> item 3: where environment: entries are not permitted
// ════════════════════════════════════════════════════════════════════

/// An Environment does not request another Environment, so an
/// `environment:` entry in an `<Environment>`'s `dependencies` is rejected.
/// (`X` declines `E`, which lists it.)
#[test]
fn an_environment_entry_in_an_environments_dependencies_is_rejected() {
    let t = job(
        "  - name: A\n    variables: {K: v}\n  - name: E\n    dependencies: [{service: X}, {environment: A}]\n    variables: {K: \"{{Service.X.main.port}}\"}\n",
        "    dependencies: [{environment: E, enableIf: false}]\n",
        "",
        "",
    );
    assert_eq!(
        decode(&t).unwrap_err(),
        format!(
            "{JOB_ERR}jobEnvironments[1] -> dependencies[1]:\n\tdependency 'environment: A' names \
             an Environment; an Environment does not request another Environment (RFC 0009 \
             <StepDependency> item 3)."
        )
    );
}

/// A Service in an Environment Template's `services` may have only
/// disabled `environment:` entries (Environment Template *services*
/// constraint 5); its document's author writes the `environment` too. (`E`
/// lists no Service, so `C` entering it is no cycle.)
#[test]
fn an_environment_entry_on_an_environment_template_service_is_rejected() {
    let et = "specificationVersion: environment-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions: [{name: V, type: STRING, default: d}]
  variables: {K: \"{{Env.Param.V}}\"}
services:
  - name: C
    dependencies: [{environment: E, parameters: {V: x}}]
    ports: [{name: main}]
    healthCheck: {type: STDOUT}
    script: {actions: {onRun: {command: run}}}
";
    assert_eq!(
        decode_environment_template(yaml_val(et), Some(EXTS), &CallerLimits::default())
            .unwrap_err()
            .to_string(),
        "Model validation error: 1 validation error for EnvironmentTemplate\nservices[0] -> \
         dependencies[0]:\n\tdependency 'environment: E' must be disabled: a Service in an \
         Environment Template may have only 'environment:' entries whose enableIf is the literal \
         false and that have no parameters, which decline the Environment. The document's author \
         writes its environment too, and can give it the values directly (Template Schemas §1.2 \
         services constraint 5)."
    );
}

/// An unresolved `environment:` entry of a Service names the Service, not
/// a Step, in its remedy (RFC 0009 <StepDependency> item 3: read RFC 0010's
/// "Step" as "Service").
#[test]
fn an_unresolved_service_environment_entry_names_the_service() {
    let t = job(
        &listing_x(""),
        "    dependencies: [{environment: Nope}, {environment: E, enableIf: false}]\n",
        "",
        "",
    );
    assert_eq!(
        create(&t, &[]).unwrap_err(),
        format!(
            "{JOB_ERR}services[0] -> dependencies[0] -> environment:\n\t\
             names Environment 'Nope', which is not a jobEnvironments entry of the Job Template, \
             and no Environment Template is attached. The entry is required; set 'required: \
             false' if the Service can run without the Environment (Template Schemas §3.2.1 \
             item 2)."
        )
    );
}

// ════════════════════════════════════════════════════════════════════
// RFC 0010 §3.2 last paragraph: the pre-submission check covers Services
// ════════════════════════════════════════════════════════════════════

/// `check_environment_templates` for `template` with `attachments`: what
/// `openjd check --environment` reports.
fn check(template: &str, attachments: &[&str]) -> Result<(), String> {
    let jt = decode(template)?;
    let ets: Vec<_> = attachments
        .iter()
        .map(|t| {
            decode_environment_template(yaml_val(t), Some(EXTS), &CallerLimits::default())
                .expect("environment template should validate")
        })
        .collect();
    let attached: Vec<AttachedEnvironmentTemplate<'_>> = ets.iter().map(Into::into).collect();
    check_environment_templates(&jt, &attached, &jt.default_validation_context())
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// A Job Template with one Service `X` (`svc_extra` indented four spaces)
/// and one Step `A` that lists it, followed by `step_deps` (a flow-list
/// tail), with `jobEnvironments` list body `envs` when not empty.
fn service_job(envs: &str, svc_extra: &str, step_deps: &str) -> String {
    let envs = if envs.is_empty() {
        String::new()
    } else {
        format!("jobEnvironments:\n{envs}")
    };
    format!(
        "specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
name: J
{envs}services:
  - name: X
{svc_extra}    ports: [{{name: main}}]
    healthCheck: {{type: STDOUT}}
    script: {{actions: {{onRun: {{command: run}}}}}}
steps:
  - name: A
    dependencies: [{{service: X}}{step_deps}]
    script: {{actions: {{onRun: {{command: run}}}}}}
"
    )
}

/// An attached Environment `Q` (with `activation`, indented two spaces)
/// with a STRING parameter `R` without a default.
fn q_requiring_r(activation: &str) -> String {
    format!(
        "specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: Q
{activation}  envParameterDefinitions: [{{name: R, type: STRING}}]
  variables: {{K: \"{{{{Env.Param.R}}}}\"}}
"
    )
}

/// `check` and job creation both reject `template` with `attachments`,
/// with the same `expected` message.
fn check_and_create_err(template: &str, attachments: &[&str], expected: &str) {
    assert_eq!(check(template, attachments).unwrap_err(), expected);
    assert_eq!(create(template, attachments).unwrap_err(), expected);
}

const SERVICE_MUST_SUPPLY: &str = "must supply, in an 'environment:' dependency, every \
     parameter without a default of each Environment that applies to it (every attached \
     Environment with activation ALWAYS, each one with activation BY_DEFAULT that the Service \
     does not decline, and each one with activation ON_REQUEST that the Service requests; \
     Template Schemas §3.2.1, RFC 0009 <StepDependency> item 3), whatever that Environment's own \
     enableIf is; missing: 'R' of Environment 'Q' (EnvironmentTemplate[0]).";

/// The constraint 7 error for Step `A` (`steps[0]`) over the attached `Q`.
const STEP_MUST_SUPPLY: &str = "JobTemplate -> steps[0]:\n\tmust supply, in an 'environment:' \
     dependency, every parameter without a default of each Environment that applies to it \
     (every attached Environment with activation ALWAYS, each one with activation BY_DEFAULT \
     that the Step does not decline, and each one with activation ON_REQUEST that the Step \
     requests; Template Schemas §3.2.1), whatever that Environment's own enableIf is; missing: \
     'R' of Environment 'Q' (EnvironmentTemplate[0]).";

/// A Service's unresolved required entry (§3.2.1 item 2) and the Step's
/// constraint 7 over an attached `BY_DEFAULT` Environment are found before
/// submission, with job creation's messages and order: §7.4.1 step 3 (the
/// entry) before step 5 (constraint 7). Job creation takes no step after
/// step 4 for the Service, which has a step 3 error, so it gives the Service
/// no constraint 7 error. The check before submission reports every
/// violation it is certain of, as it does for a Step, so it also reports
/// the Service's constraint 7, after the Steps'.
#[test]
fn check_resolves_a_services_entries_and_applies_constraint_7_to_it() {
    let t = service_job("", "    dependencies: [{environment: Missing}]\n", "");
    let entry = "JobTemplate -> services[0] -> dependencies[0] -> environment:\n\tnames \
         Environment 'Missing', which is not a jobEnvironments entry of the Job Template, and \
         none of the attached Environment Templates (EnvironmentTemplate[0]) defines it. The \
         entry is required; set 'required: false' if the Service can run without the \
         Environment (Template Schemas §3.2.1 item 2).";
    assert_eq!(
        create(&t, &[&q_requiring_r("")]).unwrap_err(),
        format!(
            "Model validation error: 2 validation errors for Submission\n{entry}\n\
             {STEP_MUST_SUPPLY}"
        )
    );
    assert_eq!(
        check(&t, &[&q_requiring_r("")]).unwrap_err(),
        format!(
            "Model validation error: 3 validation errors for Submission\n{entry}\n\
             {STEP_MUST_SUPPLY}\nJobTemplate -> services[0]:\n\t{SERVICE_MUST_SUPPLY}"
        )
    );
}

/// An `ON_REQUEST` attachment the Service requests is entered by its
/// Sessions, so the Service must supply its parameters even when a Step
/// does (constraint 7 per entity).
#[test]
fn check_applies_constraint_7_to_an_attachment_a_service_requests() {
    let t = service_job(
        "",
        "    dependencies: [{environment: Q}]\n",
        ", {environment: Q, parameters: {R: s}}",
    );
    let q = q_requiring_r("  activation: ON_REQUEST\n");
    check_and_create_err(
        &t,
        &[&q],
        &format!("{SUBMISSION_ERR}JobTemplate -> services[0]:\n\t{SERVICE_MUST_SUPPLY}"),
    );
    // Supplied by the Service: both pass.
    let t = service_job(
        "",
        "    dependencies: [{environment: Q, parameters: {R: x}}]\n",
        ", {environment: Q, parameters: {R: s}}",
    );
    check(&t, &[&q]).unwrap();
    create(&t, &[&q]).unwrap();
}

/// Constraint 6 for a literal value a Service supplies to an attachment.
#[test]
fn check_applies_constraint_6_to_a_services_literal_value() {
    let q = "specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: Q
  activation: ON_REQUEST
  envParameterDefinitions: [{name: C, type: INT, default: 1, minValue: 1}]
  variables: {K: \"{{Env.Param.C}}\"}
";
    let t = service_job(
        "",
        "    dependencies: [{environment: Q, parameters: {C: 0}}]\n",
        "",
    );
    let err = check(&t, &[q]).unwrap_err();
    assert_eq!(err, create(&t, &[q]).unwrap_err());
    assert_eq!(
        err,
        format!(
            "{SUBMISSION_ERR}JobTemplate -> services[0] -> dependencies[0] -> parameters -> C:\n\t\
             Parameter 'C': value 0 is less than minimum 1"
        )
    );
}

const H_LINUX: &str = "specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: H
  hostRequirements: {attributes: [{name: attr.worker.os.family, anyOf: [linux]}]}
  variables: {K: v}
";

const X_WINDOWS: &str =
    "    hostRequirements: {attributes: [{name: attr.worker.os.family, anyOf: [windows]}]}\n";

/// A Service's combined literal host requirements (§3.3.3, RFC 0009
/// `<Service>` item 5) include the attached Environments its Sessions
/// enter, and a conflict is found before submission.
#[test]
fn check_combines_a_services_literal_host_requirements_with_attachments() {
    let t = service_job("", X_WINDOWS, "");
    check_and_create_err(
        &t,
        &[H_LINUX],
        &format!(
            "{SUBMISSION_ERR}JobTemplate -> services[0]:\n\tService 'X': no host can satisfy \
             the combined host requirements for attribute 'attr.worker.os.family', of which a \
             host has exactly one value: the anyOf and allOf values of JobTemplate -> \
             services[0] -> hostRequirements and EnvironmentTemplate[0] -> environment -> \
             hostRequirements (Environment 'H') have no value in common. Template Schemas \
             §3.3.3."
        ),
    );
}

/// The same for a Job Environment: a `BY_DEFAULT` one is combined, and
/// one that lists the Service and that the Service declines (as it must,
/// RFC 0009 "Services run inside Environments") is not entered by its
/// Sessions, so it is not.
#[test]
fn check_combines_a_services_host_requirements_with_the_job_environments_it_enters() {
    let env = "  - name: E\n    hostRequirements: {attributes: [{name: attr.worker.os.family, anyOf: [linux]}]}\n    variables: {K: v}\n";
    let t = service_job(env, X_WINDOWS, "");
    let err = check(&t, &[]).unwrap_err();
    assert_eq!(err, create(&t, &[]).unwrap_err());
    assert_eq!(
        err,
        format!(
            "{JOB_ERR}services[0]:\n\tService 'X': no host can satisfy the combined host \
             requirements for attribute 'attr.worker.os.family', of which a host has exactly one \
             value: the anyOf and allOf values of services[0] -> hostRequirements and \
             jobEnvironments[0] -> hostRequirements (Environment 'E') have no value in common. \
             Template Schemas §3.3.3."
        )
    );
    let listing = "  - name: E\n    dependencies: [{service: X}]\n    hostRequirements: {attributes: [{name: attr.worker.os.family, anyOf: [linux]}]}\n    variables: {K: \"{{Service.X.main.port}}\"}\n";
    let declines = format!("    dependencies: [{{environment: E, enableIf: false}}]\n{X_WINDOWS}");
    let t = service_job(listing, &declines, "");
    check(&t, &[]).unwrap();
    create(&t, &[]).unwrap();
}

// ════════════════════════════════════════════════════════════════════
// The pre-submission check over the Service Sessions: wrap hook rule 1,
// the single-layer rule, and the attached Services
// ════════════════════════════════════════════════════════════════════

/// The seven hooks of a wrapper that both kinds of Session may enter.
const SEVEN_HOOKS: &str = "    script:
      actions:
        onWrapEnvEnter: {command: c}
        onWrapTaskRun: {command: c}
        onWrapEnvExit: {command: c}
        onWrapServiceEnter: {command: c}
        onWrapServiceRun: {command: c}
        onWrapServiceHealthCheck: {command: c}
        onWrapServiceExit: {command: c}
";

/// An attached document declaring `SERVICE` whose Environment `name`
/// (`activation` indented two spaces) carries `hooks` (indented four
/// spaces, as for a `jobEnvironments` entry).
fn attached_wrapper(name: &str, activation: &str, hooks: &str) -> String {
    format!(
        "specificationVersion: environment-2023-09\nextensions: [SERVICE, ENV_PARAMS, EXPR, WRAP_ACTIONS]\nenvironment:\n  name: {name}\n{activation}{}",
        hooks
            .lines()
            .map(|l| format!("{}\n", &l[2..]))
            .collect::<String>()
    )
}

/// An attachment with Service `Cache` (`svc_extra` indented four spaces)
/// and a `BY_DEFAULT` Environment `CacheClient` that lists it, so every
/// Step is in its scope. `Cache` declines `CacheClient`, since entering it
/// would be the cycle `Cache -> Cache`.
fn cache_attachment(svc_extra: &str) -> String {
    format!(
        "specificationVersion: environment-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
services:
  - name: Cache
    dependencies: [{{environment: CacheClient, enableIf: false}}]
{svc_extra}    ports: [{{name: main}}]
    healthCheck: {{type: STDOUT}}
    script: {{actions: {{onRun: {{command: run}}}}}}
environment:
  name: CacheClient
  dependencies: [{{service: Cache}}]
  variables: {{CACHE_PORT: \"{{{{Service.Cache.main.port}}}}\"}}
"
    )
}

/// A-F1 (a): an attached `BY_DEFAULT` wrapper with RFC 0008's three hooks
/// that a Job Template Service's Sessions enter is found before
/// submission, with job creation's message.
#[test]
fn check_applies_wrap_hook_rule_1_to_a_service_entering_an_attached_wrapper() {
    let wrapper = attached_wrapper("QWrap", "", THREE_HOOK_WRAPPER);
    let t = service_job("", "", "");
    check_and_create_err(
        &t,
        &[&wrapper],
        &format!(
            "{SUBMISSION_ERR}EnvironmentTemplate[0] -> environment -> script -> actions:\n\tthe \
             Sessions of Service 'X' enter wrapping Environment 'QWrap', defined by \
             EnvironmentTemplate[0], {SERVICE_HOOKS_MISSING}"
        ),
    );
    // Requested by the Service through an entry whose enableIf is an
    // expression: not certain before submission, so check passes.
    let wrapper = attached_wrapper("QWrap", "  activation: ON_REQUEST\n", THREE_HOOK_WRAPPER);
    let t = service_job(
        "",
        "    dependencies: [{environment: QWrap, enableIf: \"Job.Name == 'J'\"}]\n",
        "",
    );
    check(&t, &[&wrapper]).unwrap();
    assert!(create(&t, &[&wrapper]).is_err());
    // A wrapper with only the Service-Session hooks, in a document that
    // declares SERVICE: the Steps' Sessions enter it, so it must define
    // onWrapTaskRun.
    let service_only = SEVEN_HOOKS.replace("        onWrapTaskRun: {command: c}\n", "");
    let wrapper = attached_wrapper("QWrap", "", &service_only);
    check_and_create_err(
        &service_job("", "", ""),
        &[&wrapper],
        &format!(
            "{SUBMISSION_ERR}EnvironmentTemplate[0] -> environment -> script -> actions:\n\tthe \
             Sessions of Step 'A' enter wrapping Environment 'QWrap', defined by \
             EnvironmentTemplate[0], which therefore must define onWrapEnvEnter, onWrapTaskRun, \
             and onWrapEnvExit; missing: onWrapTaskRun (RFC 0009 wrap hook rule 1)."
        ),
    );
}

/// A-F1 (b): an attached Service whose Sessions enter an RFC 0008 wrapper
/// attached before it, from a document that declares neither `SERVICE` nor
/// `ENV_PARAMS` (so the wrapper behaves as `ALWAYS`). An attached Service
/// never enters the Job Template's Environments (RFC 0009 "Services run
/// inside Environments"), so the same wrapper in a Job Template that does
/// not declare `SERVICE` is no error.
#[test]
fn check_applies_wrap_hook_rule_1_to_an_attached_service_entering_an_earlier_attachments_wrapper() {
    let t = format!(
        "specificationVersion: jobtemplate-2023-09
extensions: [WRAP_ACTIONS, EXPR]
name: J
jobEnvironments:
  - name: JobWrapper
{THREE_HOOK_WRAPPER}steps:
  - name: Use
    script: {{actions: {{onRun: {{command: run}}}}}}
"
    );
    assert_eq!(agree(&t, &[&cache_attachment("")]), None);
    let wrapper = format!(
        "specificationVersion: environment-2023-09\nextensions: [WRAP_ACTIONS, EXPR]\nenvironment:\n  name: W\n{}",
        THREE_HOOK_WRAPPER
            .lines()
            .map(|l| format!("{}\n", &l[2..]))
            .collect::<String>()
    );
    check_and_create_err(
        &one_step("", ""),
        &[&wrapper, &cache_attachment("")],
        &format!(
            "{SUBMISSION_ERR}EnvironmentTemplate[0] -> environment -> script -> actions:\n\tthe \
             Sessions of Service 'Cache' (EnvironmentTemplate[1]) enter wrapping Environment \
             'W', defined by EnvironmentTemplate[0], {SERVICE_HOOKS_MISSING} Its document, \
             EnvironmentTemplate[0], does not declare the SERVICE extension, so it cannot define \
             the onWrapService* hooks: declare SERVICE and define them, or declare ENV_PARAMS and \
             EXPR and either make the Environment ON_REQUEST, so that only the Steps that request \
             it enter it, or leave it BY_DEFAULT, so that the Service can decline it with a \
             disabled 'environment:' dependency."
        ),
    );
}

/// A-F2 and A-F4: the single-layer rule for a Job Template Service whose
/// Sessions enter a `BY_DEFAULT` Job Environment and an attached wrapper it
/// requests; reported at the Service, before and at submission.
#[test]
fn check_applies_the_single_layer_rule_to_a_services_sessions() {
    let jwrap = format!("  - name: JWrap\n{SEVEN_HOOKS}");
    let qwrap = attached_wrapper("QWrap", "  activation: ON_REQUEST\n", SEVEN_HOOKS);
    let t = job(&jwrap, "    dependencies: [{environment: QWrap}]\n", "", "");
    check_and_create_err(
        &t,
        &[&qwrap],
        &format!(
            "{SUBMISSION_ERR}JobTemplate -> services[0]:\n\tService 'X': its Sessions would enter \
             2 Environments that define wrap hooks (onWrapEnvEnter, onWrapEnvExit, \
             onWrapService*): 'QWrap' (EnvironmentTemplate[0]), 'JWrap'. A Session may have at \
             most one Environment defining wrap hooks (RFC 0008)."
        ),
    );
}

/// The same for an attached Service, entering the `BY_DEFAULT` wrappers of
/// two attachments before its own; the Steps enter both too. (An attached
/// Service never enters the Job Template's Environments.)
#[test]
fn check_applies_the_single_layer_rule_to_an_attached_services_sessions() {
    let layers = |hooks: &str| {
        format!(
            "2 Environments that define wrap hooks ({hooks}): 'JWrap' (EnvironmentTemplate[0]), \
             'AWrap' (EnvironmentTemplate[1]). A Session may have at most one Environment \
             defining wrap hooks (RFC 0008)."
        )
    };
    let (task, service) = (
        layers("onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit"),
        layers("onWrapEnvEnter, onWrapEnvExit, onWrapService*"),
    );
    check_and_create_err(
        &one_step("", ""),
        &[
            &jwrap_attachment(),
            &attached_wrapper("AWrap", "", SEVEN_HOOKS),
            &cache_attachment(""),
        ],
        &format!(
            "Model validation error: 2 validation errors for Submission\nJobTemplate -> \
             steps[0]:\n\tits Sessions would enter {task}\nEnvironmentTemplate[2] -> \
             services[0]:\n\tService 'Cache' (EnvironmentTemplate[2]): its Sessions would enter \
             {service}"
        ),
    );
}

/// A Job Template whose Step `A` supplies parameter `A` of the attached
/// Environment `Alw`.
const SUPPLIES_ALW: &str = "specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
  - name: A
    dependencies: [{environment: Alw, parameters: {A: x}}]
    script: {actions: {onRun: {command: run}}}
";

/// An attached `BY_DEFAULT` Environment `Alw` with STRING parameter `A`
/// (`param_extra` inside its definition) and `env_extra` (indented two
/// spaces).
fn alw(param_extra: &str, env_extra: &str) -> String {
    format!(
        "specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: Alw
  envParameterDefinitions: [{{name: A, type: STRING{param_extra}}}]
{env_extra}  variables: {{K: \"{{{{Env.Param.A}}}}\"}}
"
    )
}

/// A-F3 (a): an attached Service with a non-empty scope must have a value
/// for every parameter of each Environment that applies to it, here the
/// `BY_DEFAULT` `Alw` of an earlier attachment, which it can get only from
/// a `default` (constraint 7, RFC 0009 "Services run inside
/// Environments").
#[test]
fn check_applies_constraint_7_to_an_attached_service() {
    let svc = cache_attachment(
        "    hostRequirements: {amounts: [{name: amount.worker.vcpu, min: 64}]}\n",
    );
    check_and_create_err(
        SUPPLIES_ALW,
        &[&alw("", ""), &svc],
        &format!(
            "{SUBMISSION_ERR}EnvironmentTemplate[1] -> services[0]:\n\tmust supply, in an \
             'environment:' dependency, every parameter without a default of each Environment \
             that applies to it (every Environment of its own document and of the Environment \
             Templates attached before it with activation ALWAYS, and each one with activation \
             BY_DEFAULT that it does not decline; a Service in an Environment Template has only \
             disabled 'environment:' dependencies, so it enters each with every parameter at \
             its default; Template Schemas §3.2 constraint 7, RFC 0009 \"Services run inside \
             Environments\"), whatever that Environment's own enableIf is; missing: 'A' of \
             Environment 'Alw' (EnvironmentTemplate[0])."
        ),
    );
}

/// A-F3 (b): an attached Service's combined host requirements include the
/// Environments of earlier attachments it enters, here the `BY_DEFAULT`
/// `Alw`.
#[test]
fn check_combines_an_attached_services_host_requirements() {
    let svc = cache_attachment(
        "    hostRequirements: {amounts: [{name: amount.worker.vcpu, min: 64}]}\n",
    );
    let alw = alw(
        ", default: d",
        "  hostRequirements: {amounts: [{name: amount.worker.vcpu, max: 8}]}\n",
    );
    check_and_create_err(
        SUPPLIES_ALW,
        &[&alw, &svc],
        &format!(
            "{SUBMISSION_ERR}EnvironmentTemplate[1] -> services[0]:\n\tService 'Cache' \
             (EnvironmentTemplate[1]): no host can satisfy the combined host requirements for \
             amount 'amount.worker.vcpu': EnvironmentTemplate[1] -> services[0] -> \
             hostRequirements requires min 64, and EnvironmentTemplate[0] -> environment -> \
             hostRequirements (Environment 'Alw') requires max 8. Template Schemas §3.3.3."
        ),
    );
    // A Service nothing uses is never started: its Environment is
    // ON_REQUEST and no Step requests it, so neither reports.
    let unused = cache_attachment(
        "    hostRequirements: {amounts: [{name: amount.worker.vcpu, min: 64}]}\n",
    )
    .replace(
        "  name: CacheClient\n",
        "  name: CacheClient\n  activation: ON_REQUEST\n",
    );
    check(SUPPLIES_ALW, &[&alw, &unused]).unwrap();
    create(SUPPLIES_ALW, &[&alw, &unused]).unwrap();
}

// ════════════════════════════════════════════════════════════════════
// Service scope rule 2 through an entering edge
// ════════════════════════════════════════════════════════════════════

/// Services `Front` and `Back`; Job Environment `E` (`activation` indented
/// four spaces) lists `Back`, and `Front` requests `E`; Step `A` lists
/// `Front`, and Step `B` lists nothing.
fn front_enters_e_listing_back(activation: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
name: J
jobEnvironments:
  - name: E
{activation}    dependencies: [{{service: Back}}]
    variables: {{BACK_PORT: \"{{{{Service.Back.main.port}}}}\"}}
services:
  - name: Front
    dependencies: [{{environment: E}}]
    ports: [{{name: main}}]
    script: {{actions: {{onRun: {{command: run}}}}}}
  - name: Back
    ports: [{{name: main}}]
    script: {{actions: {{onRun: {{command: run}}}}}}
steps:
  - name: A
    dependencies: [{{service: Front}}]
    script: {{actions: {{onRun: {{command: run}}}}}}
  - name: B
    script: {{actions: {{onRun: {{command: run}}}}}}
"
    )
}

/// Scope rule 2 (RFC 0009 Service scope): a Service whose Sessions enter
/// an `ON_REQUEST` Job Environment that lists another Service depends on
/// it, so the other's scope holds the first's. Only `Front` enters `E`, so
/// `Back`'s scope is `Front`'s, `[A]`, at template validation and in the
/// created Job.
#[test]
fn a_service_entering_an_environment_that_lists_another_joins_its_scope() {
    let t = front_enters_e_listing_back("    activation: ON_REQUEST\n");
    let jt = decode(&t).unwrap();
    let scopes = openjd_model::template::compute_service_scopes(&jt).unwrap();
    assert_eq!(
        scopes.get("Front").unwrap().scope,
        ServiceScope::steps(["A"])
    );
    assert_eq!(
        scopes.get("Back").unwrap().scope,
        ServiceScope::steps(["A"])
    );
    let job = create(&t, &[]).unwrap();
    let services = job.services.as_ref().unwrap();
    assert_eq!(
        services
            .iter()
            .map(|s| (s.name.as_str(), s.scope.clone()))
            .collect::<Vec<_>>(),
        [
            ("Front", ServiceScope::steps(["A"])),
            ("Back", ServiceScope::steps(["A"]))
        ]
    );
}

/// With `E` `BY_DEFAULT`, every Step enters it, so `Back` is a Service of
/// every Step directly (rule 3), and `Front`'s scope stays `[A]`. `Back`
/// declines `E`, since entering it would be the cycle `Back -> Back`.
#[test]
fn a_by_default_environment_listing_a_service_puts_every_step_in_its_scope() {
    let t = front_enters_e_listing_back("").replace(
        "  - name: Back\n",
        "  - name: Back\n    dependencies: [{environment: E, enableIf: false}]\n",
    );
    let jt = decode(&t).unwrap();
    let scopes = openjd_model::template::compute_service_scopes(&jt).unwrap();
    assert_eq!(
        scopes.get("Front").unwrap().scope,
        ServiceScope::steps(["A"])
    );
    assert_eq!(scopes.get("Back").unwrap().scope, ServiceScope::AllSteps);
    let job = create(&t, &[]).unwrap();
    let services = job.services.as_ref().unwrap();
    assert_eq!(services[0].scope, ServiceScope::steps(["A"]));
    assert!(
        services[1].scope.contains("A") && services[1].scope.contains("B"),
        "{:?}",
        services[1].scope
    );
}

// ════════════════════════════════════════════════════════════════════
// ENV_PARAMS `ALWAYS` / `BY_DEFAULT` split: every SERVICE rule written
// against the old `ALWAYS` applies to both values
// ════════════════════════════════════════════════════════════════════

/// Create `template`'s Job, returning the whole `CreatedJob`.
fn create_details(template: &str) -> openjd_model::CreatedJob {
    create_details_with(template, &[])
}

/// [`create_details`] with `attachments` attached.
fn create_details_with(template: &str, attachments: &[&str]) -> openjd_model::CreatedJob {
    let jt = decode(template).unwrap();
    let ets: Vec<_> = attachments
        .iter()
        .map(|t| {
            decode_environment_template(yaml_val(t), Some(EXTS), &CallerLimits::default())
                .expect("environment template should validate")
        })
        .collect();
    let td = tempfile::TempDir::new().unwrap();
    let dir = td.path().to_str().unwrap();
    let processed = preprocess_job_parameters(
        &jt,
        &JobParameterInputValues::default(),
        &ets,
        &PathParameterOptions::new(dir, dir),
    )
    .unwrap();
    let attached: Vec<AttachedEnvironmentTemplate<'_>> = ets.iter().map(Into::into).collect();
    create_job_with_environment_templates(
        &jt,
        &attached,
        &processed,
        &jt.default_validation_context(),
    )
    .unwrap()
}

/// A strict `ALWAYS` Job Environment that does not list `X` is entered by
/// its Sessions, as a `BY_DEFAULT` one is.
#[test]
fn a_service_enters_a_strict_always_and_a_by_default_environment() {
    let envs = "  - name: S\n    activation: ALWAYS\n    variables: {K: v}\n  - name: D\n    variables: {K: v}\n";
    let job = create(&job(envs, "", "", ""), &[]).unwrap();
    let x = &job.services.as_ref().unwrap()[0];
    assert!(x.enters_environment(&job::Document::JobTemplate, "S"));
    assert!(x.enters_environment(&job::Document::JobTemplate, "D"));
}

/// A strict `ALWAYS` Job Environment that lists `X` is entered by its
/// Sessions, as every `ALWAYS` one is: there is no exception for an
/// Environment that lists the Service, so it is the cycle `X -> X`, with or
/// without an own entry for it, as for a `BY_DEFAULT` one.
#[test]
fn a_strict_always_environment_listing_a_service_is_a_cycle() {
    assert_eq!(
        decode(&job(&listing_x("    activation: ALWAYS\n"), "", "", "")).unwrap_err(),
        format!("{JOB_ERR}JobTemplate: {}", own_entry_cycle())
    );
    assert_eq!(
        decode(&job(
            &listing_x("    activation: ALWAYS\n"),
            "    dependencies: [{environment: E}]\n",
            "",
            ""
        ))
        .unwrap_err(),
        format!("{JOB_ERR}JobTemplate: {}", own_entry_cycle())
    );
}

/// A strict `ALWAYS` Environment cannot be disabled, for a Service as for
/// a Step: an `enableIf` expression that is `false` for the Service is an
/// error naming it (RFC 0010 §3.2.2 item 6).
#[test]
fn a_strict_always_environment_disabled_for_a_service_is_an_error_naming_it() {
    let envs = "  - name: S\n    activation: ALWAYS\n    envParameterDefinitions: [{name: V, type: STRING, default: yes1}]\n    enableIf: \"Env.Param.V == 'yes1'\"\n    variables: {K: v}\n";
    let t = job(
        envs,
        "    dependencies: [{environment: S, parameters: {V: no1}}]\n",
        "",
        "",
    );
    assert_eq!(
        create(&t, &[]).unwrap_err(),
        format!(
            "{JOB_ERR}jobEnvironments[0] -> enableIf:\n\tEnvironment 'S' has activation \
             ALWAYS, so it cannot be disabled, but its enableIf is false for Service 'X' \
             (Template Schemas §4 item 4)."
        )
    );
}

/// A Service's own requirements under RFC 0010 §3.3: an amount whose
/// bounds both resolve to null, and an attribute whose one-expression
/// `anyOf` resolves to null, are discarded and reported for the Service.
#[test]
fn a_services_own_host_requirements_that_resolve_to_nothing_are_discarded() {
    let hr = "    hostRequirements:\n      amounts: [{name: amount.worker.gpu, min: \"{{ None }}\"}]\n      attributes: [{name: attr.worker.os.family, anyOf: \"{{ None }}\"}]\n";
    let created = create_details(&job("", hr, "", ""));
    let x = &created.job.services.as_ref().unwrap()[0];
    assert_eq!(x.host_requirements, None);
    assert_eq!(
        created.discarded_service_host_requirements,
        vec![vec![
            openjd_model::DiscardedHostRequirement {
                environment: None,
                kind: openjd_model::HostRequirementKind::Amount,
                name: "amount.worker.gpu".into(),
            },
            openjd_model::DiscardedHostRequirement {
                environment: None,
                kind: openjd_model::HostRequirementKind::Attribute,
                name: "attr.worker.os.family".into(),
            },
        ]]
    );
    // The Steps' list is unaffected, index for index with the Steps.
    assert_eq!(created.discarded_host_requirements, vec![vec![], vec![]]);
}

/// An entered Environment's requirement that resolves to nothing for the
/// Service is discarded for it, named with the Environment, and the
/// Service's own requirement survives the combination.
#[test]
fn an_entered_environments_discarded_requirement_is_reported_for_the_service() {
    let envs = "  - name: E\n    envParameterDefinitions: [{name: G, type: INT, default: 1}]\n    hostRequirements:\n      amounts: [{name: amount.worker.gpu, min: \"{{ Env.Param.G if Env.Param.G > 0 else None }}\"}]\n    variables: {K: v}\n";
    let t = job(
        envs,
        "    dependencies: [{environment: E, parameters: {G: 0}}]\n    hostRequirements: {amounts: [{name: amount.worker.vcpu, min: 2}]}\n",
        "",
        "",
    );
    let created = create_details(&t);
    let x = &created.job.services.as_ref().unwrap()[0];
    let hr = x.host_requirements.as_ref().unwrap();
    let names: Vec<&str> = hr
        .amounts
        .iter()
        .flatten()
        .map(|a| a.name.as_str())
        .collect();
    assert_eq!(names, ["amount.worker.vcpu"]);
    assert_eq!(
        created.discarded_service_host_requirements,
        vec![vec![openjd_model::DiscardedHostRequirement {
            environment: Some(job::EnvironmentRef {
                document: job::Document::JobTemplate,
                environment: "E".into(),
            }),
            kind: openjd_model::HostRequirementKind::Amount,
            name: "amount.worker.gpu".into(),
        }]]
    );
    // The Steps enter E with G = 1, so their requirement is kept.
    let a = &created.job.steps[0];
    assert!(a
        .host_requirements
        .as_ref()
        .unwrap()
        .amounts
        .iter()
        .flatten()
        .any(|r| r.name == "amount.worker.gpu"));
    assert_eq!(created.discarded_host_requirements, vec![vec![], vec![]]);
}

/// A Service's one-expression `anyOf` that resolves to an empty list is an
/// error naming the Service, in the form a Step's own gets.
#[test]
fn a_services_one_expression_any_of_that_resolves_to_an_empty_list_is_an_error() {
    let hr = "    hostRequirements:\n      attributes: [{name: \"custom:attr.a\", anyOf: \"{{ [] if Param.P == 'p' else ['x'] }}\"}]\n";
    assert_eq!(
        create(&job("", hr, "", ""), &[]).unwrap_err(),
        "Validation error: services[0] -> hostRequirements -> attributes[0] -> anyOf: attribute \
         'custom:attr.a' of Service 'X' has no anyOf values after resolution. An empty list is \
         not allowed (Template Schemas §3.3.2); an expression that resolves to null omits the \
         field instead"
    );
}

// ════════════════════════════════════════════════════════════════════
// The pre-submission check agrees with job creation: rejected values,
// cycles through attachments, requirement binding, and which Environments
// an attached Service enters
// ════════════════════════════════════════════════════════════════════

/// `check` and job creation agree on `template` with `attachments`: both
/// pass, or both fail with the same message, which is returned.
fn agree(template: &str, attachments: &[&str]) -> Option<String> {
    let checked = check(template, attachments);
    assert_eq!(
        checked,
        create(template, attachments).map(|_| ()),
        "check and job creation disagree"
    );
    checked.err()
}

const MIN_8: &str = "    hostRequirements: {amounts: [{name: amount.worker.vcpu, min: 8}]}\n";

/// An attached `BY_DEFAULT` Environment `E` with INT parameter `Need`
/// (`param_extra` inside its definition) and the literal requirement
/// `amount.worker.vcpu` at most 4.
fn need_env(param_extra: &str) -> String {
    need_env_of("INT", param_extra)
}

/// [`need_env`] with `Need` of type `ty`.
fn need_env_of(ty: &str, param_extra: &str) -> String {
    let k = if ty.starts_with("LIST") {
        "len(Env.Param.Need)"
    } else {
        "Env.Param.Need"
    };
    format!(
        "specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions: [{{name: Need, type: '{ty}'{param_extra}}}]
  hostRequirements: {{amounts: [{{name: amount.worker.vcpu, max: 4}}]}}
  variables: {{K: \"{{{{{k}}}}}\"}}
"
    )
}

/// A Job Template whose Step `A` has the `dependencies` flow-list body
/// `deps` (none when empty) and `step_extra` (indented four spaces).
fn one_step(deps: &str, step_extra: &str) -> String {
    let deps = if deps.is_empty() {
        String::new()
    } else {
        format!("    dependencies: [{deps}]\n")
    };
    format!(
        "specificationVersion: jobtemplate-2023-09
extensions: [ENV_PARAMS, EXPR]
name: J
steps:
  - name: A
{deps}{step_extra}    script: {{actions: {{onRun: {{command: run}}}}}}
"
    )
}

/// A-F1 (round 4), Step: job creation has no values of `E` for a Step whose
/// value for it fails constraint 6 or which lacks one (constraint 7), so it
/// skips `E`'s host requirements for that Step; check reports only the
/// value error too.
#[test]
fn check_skips_the_host_requirements_of_an_environment_whose_values_a_step_failed() {
    for deps in ["{environment: E, parameters: {Need: 0}}", ""] {
        let t = one_step(deps, MIN_8);
        let err = agree(&t, &[&need_env(", minValue: 1")]).unwrap();
        assert!(err.starts_with(SUBMISSION_ERR), "{err}");
        assert!(!err.contains("no host can satisfy"), "{err}");
    }
    // A valid value: the conflict is reported by both.
    let t = one_step("{environment: E, parameters: {Need: 1}}", MIN_8);
    let err = agree(&t, &[&need_env(", minValue: 1")]).unwrap();
    assert!(err.contains("Step 'A': no host can satisfy"), "{err}");
}

/// Step `A` supplies `E`'s `Need` as the format string `{{Job.Name}}`.
const NEED_JOB_NAME: &str = "{environment: E, parameters: {Need: \"{{Job.Name}}\"}}";

const A_CONFLICT: &str = "JobTemplate -> steps[0]:\n\tStep 'A': no host can satisfy the combined \
     host requirements for amount 'amount.worker.vcpu': JobTemplate -> steps[0] -> \
     hostRequirements requires min 8, and EnvironmentTemplate[0] -> environment -> \
     hostRequirements (Environment 'E') requires max 4. Template Schemas §3.3.3.";

/// [`need_env`] whose `Need` defaults to `{{Param.S}}`, the Environment
/// Template's own STRING parameter `S` (default `many`), which does not
/// convert to INT.
fn need_env_param_default() -> String {
    need_env(", default: \"{{Param.S}}\"").replace(
        "environment:\n",
        "parameterDefinitions: [{name: S, type: STRING, default: many}]\nenvironment:\n",
    )
}

/// The resolution error of [`need_env_param_default`]'s `default` for
/// `who`, at the attachment `doc`.
fn param_default_err(doc: &str, who: &str) -> String {
    format!(
        "{SUBMISSION_ERR}{doc} -> environment -> envParameterDefinitions[0] -> default:\n\tfor \
         {who}, which does not supply this parameter: could not be resolved as a value of type \
         INT: Cannot convert 'many' to int: invalid digit found in string\n  Param.S\n  ~~~~~~^"
    )
}

/// a/F1 (round 5): only job creation resolves a format-string value, and
/// it skips `E`'s host requirements for the Step when the value fails, so
/// check leaves `E` out of the Step's combination and reports nothing.
/// For an INT `Need`, job creation reports only the value error. For a
/// STRING `Need` the value resolves and job creation reports the conflict,
/// which check does not: the gap specs/cli/check.md discloses.
#[test]
fn check_leaves_out_an_environment_a_step_supplies_a_format_string_value_for() {
    let t = one_step(NEED_JOB_NAME, MIN_8);
    assert_eq!(check(&t, &[&need_env("")]), Ok(()));
    assert_eq!(
        create(&t, &[&need_env("")]).map(|_| ()).unwrap_err(),
        format!(
            "{SUBMISSION_ERR}JobTemplate -> steps[0] -> dependencies[0] -> parameters -> Need:\n\t\
             could not be resolved as a value of type INT: Cannot convert 'J' to int: invalid \
             digit found in string\n  Job.Name\n  ~~~~^~~~"
        )
    );
    let string_env = need_env_of("STRING", "");
    assert_eq!(check(&t, &[&string_env]), Ok(()));
    assert_eq!(
        create(&t, &[&string_env]).map(|_| ()).unwrap_err(),
        format!("{SUBMISSION_ERR}{A_CONFLICT}")
    );
}

/// The same for a `default` the Step falls back to: a format-string one
/// is left out by check (job creation reports its failure), and a valid
/// literal one is combined, as job creation does.
#[test]
fn check_combines_an_environment_a_step_falls_back_to_a_default_for_only_when_it_is_a_valid_literal(
) {
    let t = one_step("", MIN_8);
    let env = need_env_param_default();
    assert_eq!(check(&t, &[&env]), Ok(()));
    assert_eq!(
        create(&t, &[&env]).map(|_| ()).unwrap_err(),
        param_default_err("EnvironmentTemplate[0]", "Step 'A'")
    );
    assert_eq!(
        agree(&t, &[&need_env(", default: 1")]).unwrap(),
        format!("{SUBMISSION_ERR}{A_CONFLICT}")
    );
    // A PATH value is used as given, so a relative literal `default` is
    // valid and combined.
    assert_eq!(
        agree(&t, &[&need_env_of("PATH", ", default: rel/p")]).unwrap(),
        format!("{SUBMISSION_ERR}{A_CONFLICT}")
    );
}

/// b/F1 (round 5): a format-string value that fails the single-expression
/// rule for a list-typed parameter is reported alone by both, without
/// `E`'s host conflict.
#[test]
fn check_skips_the_host_requirements_of_an_environment_whose_format_string_value_failed() {
    let t = one_step(
        "{environment: E, parameters: {Need: \"a{{Job.Name}}\"}}",
        MIN_8,
    );
    assert_eq!(
        agree(&t, &[&need_env_of("LIST[INT]", "")]).unwrap(),
        format!(
            "{SUBMISSION_ERR}JobTemplate -> steps[0] -> dependencies[0] -> parameters -> Need:\n\t\
             a LIST[INT] parameter has no string form, so a format-string value for it must be \
             a single '{{{{ }}}}' expression."
        )
    );
}

/// a/F1 (round 5) for a Job Template Service, which falls back to a
/// format-string `default` or supplies a format-string value, and for an
/// attached Service, which enters `E`, attached before it, with its
/// format-string `default`:
/// check reports nothing, job creation the value error.
#[test]
fn check_leaves_out_an_environment_a_services_values_for_are_not_literals() {
    let t = service_job("", MIN_8, ", {environment: E, parameters: {Need: 1}}");
    let env = need_env_param_default();
    assert_eq!(check(&t, &[&env]), Ok(()));
    assert_eq!(
        create(&t, &[&env]).map(|_| ()).unwrap_err(),
        param_default_err("EnvironmentTemplate[0]", "Service 'X'")
    );

    let t = service_job(
        "",
        &format!("    dependencies: [{NEED_JOB_NAME}]\n{MIN_8}"),
        ", {environment: E, parameters: {Need: 1}}",
    );
    assert_eq!(check(&t, &[&need_env("")]), Ok(()));
    assert_eq!(
        create(&t, &[&need_env("")]).map(|_| ()).unwrap_err(),
        format!(
            "{SUBMISSION_ERR}JobTemplate -> services[0] -> dependencies[0] -> parameters -> \
             Need:\n\tcould not be resolved as a value of type INT: Cannot convert 'J' to int: \
             invalid digit found in string\n  Job.Name\n  ~~~~^~~~"
        )
    );

    let t = one_step("{environment: E, parameters: {Need: 1}}", "");
    let attachments = [env, cache_attachment(MIN_8)];
    let attachments: Vec<&str> = attachments.iter().map(String::as_str).collect();
    assert_eq!(check(&t, &attachments), Ok(()));
    assert_eq!(
        create(&t, &attachments).map(|_| ()).unwrap_err(),
        param_default_err(
            "EnvironmentTemplate[0]",
            "Service 'Cache' (EnvironmentTemplate[1])"
        )
    );
}

/// The same for a Job Template Service, which lacks `Need` (the Step
/// supplies it).
#[test]
fn check_skips_the_host_requirements_of_an_environment_whose_values_a_service_lacks() {
    let t = service_job("", MIN_8, ", {environment: E, parameters: {Need: 1}}");
    let err = agree(&t, &[&need_env("")]).unwrap();
    assert!(err.starts_with(SUBMISSION_ERR), "{err}");
    assert!(
        err.contains("JobTemplate -> services[0]:\n\tmust supply"),
        "{err}"
    );
    let err = agree(&t, &[&need_env(", default: 1")]).unwrap();
    assert!(err.contains("Service 'X': no host can satisfy"), "{err}");
}

/// The same for an attached Service, which enters `E`, attached before
/// it, with every parameter at its `default` and so lacks `Need`.
#[test]
fn check_skips_the_host_requirements_of_an_environment_whose_values_an_attached_service_lacks() {
    let t = one_step("{environment: E, parameters: {Need: 1}}", "");
    let err = agree(&t, &[&need_env(""), &cache_attachment(MIN_8)]).unwrap();
    assert!(err.starts_with(SUBMISSION_ERR), "{err}");
    assert!(
        err.contains("EnvironmentTemplate[1] -> services[0]:\n\tmust supply"),
        "{err}"
    );
    let err = agree(&t, &[&need_env(", default: 1"), &cache_attachment(MIN_8)]).unwrap();
    assert!(
        err.contains("Service 'Cache' (EnvironmentTemplate[1]): no host can satisfy"),
        "{err}"
    );
}

/// A Job Template with Service `service` and a `BY_DEFAULT` Job
/// Environment `JW` (`jw_extra` indented four spaces) that lists it and
/// that it declines.
fn jw_listing(service: &str, jw_extra: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR, WRAP_ACTIONS]
name: J
jobEnvironments:
  - name: JW
{jw_extra}    dependencies: [{{service: {service}}}]
    variables: {{K: \"{{{{Service.{service}.main.port}}}}\"}}
services:
  - name: {service}
    dependencies: [{{environment: JW, enableIf: false}}]
    ports: [{{name: main}}]
    script: {{actions: {{onRun: {{command: run}}}}}}
steps:
  - name: A
    script: {{actions: {{onRun: {{command: run}}}}}}
"
    )
}

/// An attachment with Service `service` and a `BY_DEFAULT` Environment
/// `CC` (`cc_extra` indented two spaces) that lists it and that it
/// declines.
fn cc_listing(service: &str, cc_extra: &str) -> String {
    format!(
        "specificationVersion: environment-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR, WRAP_ACTIONS]
services:
  - name: {service}
    dependencies: [{{environment: CC, enableIf: false}}]
    ports: [{{name: main}}]
    script: {{actions: {{onRun: {{command: run}}}}}}
environment:
  name: CC
  dependencies: [{{service: {service}}}]
  variables: {{C: \"{{{{Service.{service}.main.port}}}}\"}}
{cc_extra}"
    )
}

/// A-F2 (round 4), repro s10c, under the earlier-documents rule: the Job
/// Template's Service `S` enters the attachment's `CC`, which lists the
/// attached `S`, but the attached `S` never enters the Job Template's
/// `JW`, which lists the Job Template's `S` (RFC 0009 "Services run inside
/// Environments"). No edge points from an attachment to the Job Template,
/// so there is no cycle, and check agrees with job creation.
#[test]
fn check_finds_no_cycle_through_an_attached_service() {
    assert_eq!(agree(&jw_listing("S", ""), &[&cc_listing("S", "")]), None);
}

/// Repro s10d: the same shape with two Services, both Environments
/// wrappers. With no cycle, Step `A`, which enters both wrappers, breaks
/// the single-layer rule, and check reports it as job creation does; the
/// Services each enter at most one wrapper.
#[test]
fn check_finds_no_cycle_through_attached_wrappers() {
    let t = jw_listing("S1", SEVEN_HOOKS);
    let hooks: String = SEVEN_HOOKS
        .lines()
        .map(|l| format!("{}\n", &l[2..]))
        .collect();
    assert_eq!(
        agree(&t, &[&cc_listing("S2", &hooks)]).unwrap(),
        format!(
            "{SUBMISSION_ERR}JobTemplate -> steps[0]:\n\tits Sessions would enter 2 Environments \
             that define wrap hooks (onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit): 'CC' \
             (EnvironmentTemplate[0]), 'JW'. A Session may have at most one Environment defining \
             wrap hooks (RFC 0008)."
        )
    );
}

/// A services-only attachment with Service `Cache` (`svc_extra` indented
/// four spaces).
fn cache_only(svc_extra: &str) -> String {
    format!(
        "specificationVersion: environment-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
services:
  - name: Cache
{svc_extra}    ports: [{{name: main}}]
    script: {{actions: {{onRun: {{command: run}}}}}}
"
    )
}

/// A Job Template requiring the external Service `Cache`, with a
/// `BY_DEFAULT` Job Environment `JX` that lists its Service `X`, which
/// declines it and lists the `dependencies` flow-list tail `x_deps`; X is
/// preceded by `own_before` (a `services` list body), and Step `A` has the
/// `dependencies` flow-list body `step_deps`. The attached `Cache` never
/// enters `JX`.
fn jx_lists_x(own_before: &str, x_deps: &str, step_deps: &str) -> String {
    format!(
        "specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
name: J
externalServices:
  - name: Cache
    ports: [{{name: main}}]
jobEnvironments:
  - name: JX
    dependencies: [{{service: X}}]
    variables: {{K: \"{{{{Service.X.main.port}}}}\"}}
services:
{own_before}  - name: X
    dependencies: [{{environment: JX, enableIf: false}}{x_deps}]
    ports: [{{name: main}}]
    script: {{actions: {{onRun: {{command: run}}}}}}
steps:
  - name: A
    dependencies: [{step_deps}]
    script: {{actions: {{onRun: {{command: run}}}}}}
"
    )
}

/// b/F2 (round 5), under the earlier-documents rule: a Job Template
/// Service's `service:` entry naming the requirement bound to an attached
/// Service is an edge to that Service, but `Cache` never enters `JX`, which
/// lists `X`, so there is no cycle `X -> Cache -> X`, and check agrees with
/// job creation. An own Service declared before `X` (used by `A`) moves
/// `X`'s node.
#[test]
fn check_finds_no_cycle_through_a_service_entry_naming_a_bound_requirement() {
    let x_needs_cache = ", {service: Cache}";
    let t = jx_lists_x("", x_needs_cache, "{service: X}");
    assert_eq!(agree(&t, &[&cache_only("")]), None);
    let x0 =
        "  - name: X0\n    ports: [{name: main}]\n    script: {actions: {onRun: {command: run}}}\n";
    let t = jx_lists_x(x0, x_needs_cache, "{service: X0}, {service: X}");
    assert_eq!(agree(&t, &[&cache_only("")]), None);
}

/// b/F2 (round 5), under the earlier-documents rule: `X`'s edge to `Cache`
/// from entering the attachment's `BY_DEFAULT` `CC`, which lists `Cache`,
/// cannot close a cycle, since `Cache` never enters `JX`; Step `A`'s
/// `service:` entries (`X`, then the bound `Cache`) add no cycle either.
#[test]
fn check_finds_no_cycle_through_an_attached_environment() {
    let t = jx_lists_x("", "", "{service: X}, {service: Cache}");
    assert_eq!(agree(&t, &[&cc_listing("Cache", "")]), None);
}

/// A Job Template requiring the external Service `Cache`, which Step `A`
/// lists followed by `step_deps` (a flow-list tail), with `jobEnvironments`
/// list body `envs` when not empty.
fn requires_cache(envs: &str, step_deps: &str) -> String {
    let envs = if envs.is_empty() {
        String::new()
    } else {
        format!("jobEnvironments:\n{envs}")
    };
    format!(
        "specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
name: J
externalServices:
  - name: Cache
    ports: [{{name: main}}]
{envs}steps:
  - name: A
    dependencies: [{{service: Cache}}{step_deps}]
    script: {{actions: {{onRun: {{command: run}}}}}}
"
    )
}

/// An attachment whose `BY_DEFAULT` Environment `H` allows at most 4 vCPUs.
const H_MAX_4: &str = "specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: H
  hostRequirements: {amounts: [{name: amount.worker.vcpu, max: 4}]}
  variables: {K: v}
";

/// B-F2 (round 4): two attachments define `Cache`, so the requirement is
/// ambiguous (§1.2.2 item 2). Job creation reports that alone, and so does
/// check. With one, the requirement binds it, and its Sessions enter the
/// `BY_DEFAULT` `H`, attached before it, which does not list it (B-F1:
/// kills `bound || lists`), so the host conflict is found.
#[test]
fn check_reports_an_ambiguous_requirement_alone_and_applies_a_unique_binding() {
    let t = requires_cache("", "");
    let err = agree(&t, &[H_MAX_4, &cache_only(MIN_8), &cache_only(MIN_8)]).unwrap();
    assert_eq!(
        err,
        format!(
            "{SUBMISSION_ERR}JobTemplate -> externalServices[0]:\n\trequired Service 'Cache' is \
             ambiguous: 2 attached Environment Templates define a Service with that name \
             (EnvironmentTemplate[1], EnvironmentTemplate[2]); a requirement must match exactly \
             one (Template Schemas §1.2.2 item 2)."
        )
    );
    let err = agree(&t, &[H_MAX_4, &cache_only(MIN_8)]).unwrap();
    assert!(
        err.starts_with(&format!(
            "{SUBMISSION_ERR}EnvironmentTemplate[1] -> services[0]:\n\tService 'Cache' \
             (EnvironmentTemplate[1]): no host can satisfy"
        )),
        "{err}"
    );
}

/// B-F1 (round 4): a Job Environment that lists the requirement bound to an
/// attached Service is not entered by its Sessions (an attached Service
/// enters none of the Job Template's Environments), so its host
/// requirements are not combined with the Service's.
#[test]
fn an_attached_service_does_not_enter_a_job_environment_listing_its_requirement() {
    let listing = "  - name: H\n    dependencies: [{service: Cache}]\n    hostRequirements: {amounts: [{name: amount.worker.vcpu, max: 4}]}\n    variables: {K: \"{{Service.Cache.main.port}}\"}\n";
    assert_eq!(
        agree(&requires_cache(listing, ""), &[&cache_only(MIN_8)]),
        None
    );
}

/// An attachment with Services `Cache` and `Other` and a `BY_DEFAULT`
/// Environment `CacheClient` with a STRING parameter `R` without a default,
/// listing `listed`, which declines it (entering it would be a cycle).
fn client_requiring_r(listed: &str) -> String {
    let decline = |name: &str| {
        if name == listed {
            "    dependencies: [{environment: CacheClient, enableIf: false}]\n"
        } else {
            ""
        }
    };
    let (cache_deps, other_deps) = (decline("Cache"), decline("Other"));
    format!(
        "specificationVersion: environment-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
services:
  - name: Cache
{cache_deps}    ports: [{{name: main}}]
    script: {{actions: {{onRun: {{command: run}}}}}}
  - name: Other
{other_deps}    ports: [{{name: main}}]
    script: {{actions: {{onRun: {{command: run}}}}}}
environment:
  name: CacheClient
  dependencies: [{{service: {listed}}}]
  envParameterDefinitions: [{{name: R, type: STRING}}]
  variables: {{K: \"{{{{Service.{listed}.main.port}}}} {{{{Env.Param.R}}}}\"}}
"
    )
}

/// B-F1 (round 4): an attached Service does not enter its own document's
/// `BY_DEFAULT` Environment when it declines it, as it must when that lists
/// it, so constraint 7 does not hold it to that Environment's parameters
/// (kills `lists` `!=`, the document test `!=`, and `<` `>` in the attached
/// branch); it does enter one that lists only another Service of its
/// document, and lacks `R` there (kills `env_docs[e] == d || lists`).
#[test]
fn an_attached_service_enters_its_documents_environment_only_when_it_does_not_decline_it() {
    let supplies = ", {environment: CacheClient, parameters: {R: x}}";
    assert_eq!(
        agree(
            &requires_cache("", supplies),
            &[&client_requiring_r("Cache")]
        ),
        None
    );
    let err = agree(
        &requires_cache("", supplies),
        &[&client_requiring_r("Other")],
    )
    .unwrap();
    assert!(
        err.contains(
            "EnvironmentTemplate[0] -> services[0]:\n\tmust supply, in an 'environment:' \
             dependency"
        ) && err.contains("missing: 'R' of Environment 'CacheClient'"),
        "{err}"
    );
}

/// B-F1 (round 4): an attached Service does not enter a wrapper attached
/// before it that is `ON_REQUEST`, though the Steps request it, nor a
/// `BY_DEFAULT` one whose `enableIf` is the scalar `false`; the wrapper has
/// only RFC 0008's three hooks, so entering either would break wrap hook
/// rule 1 (kills `ALWAYS || ...` and `... || !lists_it`).
#[test]
fn an_attached_service_does_not_enter_an_unrequested_or_disabled_wrapper() {
    let on_request = attached_wrapper("QWrap", "  activation: ON_REQUEST\n", THREE_HOOK_WRAPPER);
    assert_eq!(
        agree(
            &one_step("{environment: QWrap}", ""),
            &[&on_request, &cache_attachment("")]
        ),
        None
    );
    let disabled = attached_wrapper("QWrap", "  enableIf: false\n", THREE_HOOK_WRAPPER);
    assert_eq!(
        agree(&one_step("", ""), &[&disabled, &cache_attachment("")]),
        None
    );
}

/// An attachment whose `BY_DEFAULT` Environment `JWrap` is a seven-hook
/// wrapper.
fn jwrap_attachment() -> String {
    attached_wrapper("JWrap", "", SEVEN_HOOKS)
}

/// B-F4 (round 4): the single-layer rule counts only the wrappers an
/// attached Service's Sessions enter, among those attached before it: not
/// an `ON_REQUEST` wrapper nothing requests, nor a non-wrapping Environment
/// it enters (kills `entered || wraps`).
#[test]
fn the_single_layer_rule_counts_only_the_wrappers_an_attached_service_enters() {
    let t = one_step("", "");
    let awrap = attached_wrapper("AWrap", "  activation: ON_REQUEST\n", SEVEN_HOOKS);
    assert_eq!(
        agree(&t, &[&jwrap_attachment(), &awrap, &cache_attachment("")]),
        None
    );
    assert_eq!(
        agree(&t, &[&jwrap_attachment(), H_LINUX, &cache_attachment("")]),
        None
    );
}

/// B-F5 (round 4): which Environments a Job Template Service enters, by
/// kind: an attached `ON_REQUEST` one it does not request is not entered,
/// and nor is a `BY_DEFAULT` Job Environment that lists it and that it
/// declines (as it must), here the second Job Environment (so its index
/// exceeds the number of attached ones), whose host requirements conflict
/// with the Service's (kills `e < n_attached_envs` `>`).
#[test]
fn a_service_enters_attached_and_job_environments_by_their_own_rules() {
    let envs = "  - name: E0\n    variables: {K: v}\n  - name: E1\n    dependencies: [{service: X}]\n    hostRequirements: {amounts: [{name: amount.worker.vcpu, max: 4}]}\n    variables: {K: \"{{Service.X.main.port}}\"}\n";
    let q = "specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: Q
  activation: ON_REQUEST
  variables: {K: v}
";
    let x_extra = format!("    dependencies: [{{environment: E1, enableIf: false}}]\n{MIN_8}");
    assert_eq!(agree(&service_job(envs, &x_extra, ""), &[q]), None);
}

/// An attachment with Services `A`, which lists `a_lists`, `B`
/// (`b_extra` indented four spaces), and `C`, and an `ON_REQUEST`
/// Environment `Client` that lists `A`. (With `BY_DEFAULT`, `B`'s Sessions
/// would enter `Client`, a cycle when `A` lists `B`.)
fn a_b_c(a_lists: &str, b_extra: &str) -> String {
    format!(
        "specificationVersion: environment-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
services:
  - name: A
    dependencies: [{{service: {a_lists}}}]
    ports: [{{name: main}}]
    script: {{actions: {{onRun: {{command: run, args: [\"{{{{Service.{a_lists}.main.port}}}}\"]}}}}}}
  - name: B
{b_extra}    ports: [{{name: main}}]
    script: {{actions: {{onRun: {{command: run}}}}}}
  - name: C
    ports: [{{name: main}}]
    script: {{actions: {{onRun: {{command: run}}}}}}
environment:
  name: Client
  activation: ON_REQUEST
  dependencies: [{{service: A}}]
  variables: {{K: \"{{{{Service.A.main.port}}}}\"}}
"
    )
}

const H_MAX_8: &str = "specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: H
  hostRequirements: {amounts: [{name: amount.worker.vcpu, max: 8}]}
  variables: {K: v}
";

const B_MIN_64: &str = "    hostRequirements: {amounts: [{name: amount.worker.vcpu, min: 64}]}\n";

/// B-F3 (round 4), scope rule 2 between attached Services: `A` is used
/// (the Step requests `Client`, which lists it) and lists `B`, so `B` is
/// used and its host requirements are combined with those of `H`, attached
/// before them (kills `from.doc !=
/// target.doc` and `own_env && lists`). When nothing requests `Client`,
/// neither is used and nothing is reported (kills `used || ...`). When `A`
/// lists `C` instead, `B` is unused (kills `same document || lists` and
/// `l.name != target`).
#[test]
fn scope_rule_2_between_attached_services_decides_which_are_checked() {
    let t = one_step("{environment: Client}", "");
    let err = agree(&t, &[H_MAX_8, &a_b_c("B", B_MIN_64)]).unwrap();
    assert!(
        err.starts_with(&format!(
            "{SUBMISSION_ERR}EnvironmentTemplate[1] -> services[1]:\n\tService 'B' \
             (EnvironmentTemplate[1]): no host can satisfy"
        )),
        "{err}"
    );
    assert_eq!(
        agree(&one_step("", ""), &[H_MAX_8, &a_b_c("B", B_MIN_64)]),
        None
    );
    assert_eq!(agree(&t, &[H_MAX_8, &a_b_c("C", B_MIN_64)]), None);
}

/// An Environment whose own `enableIf` fails for a Service is an error
/// whose note names the Service, the only entity that requests it.
/// (Constraint 7 holds whatever the `enableIf`, so the Service supplies
/// `R`.)
#[test]
fn a_failed_enable_if_note_names_the_service() {
    let envs = "  - name: E\n    activation: ON_REQUEST\n    envParameterDefinitions:\n      - {name: S, type: STRING, default: abc}\n      - {name: R, type: STRING}\n    enableIf: \"int(Env.Param.S) > 0\"\n    variables: {K: v}\n";
    let t = job(
        envs,
        "    dependencies: [{environment: E, parameters: {R: r}}]\n",
        "",
        "",
    );
    assert_eq!(
        create(&t, &[]).unwrap_err(),
        format!(
            "{JOB_ERR}jobEnvironments[0] -> enableIf:\n\tcould not be evaluated: Cannot convert \
             'abc' to int. Resolved for Service 'X'.\n  int(Env.Param.S) > 0\n  \
             ^~~~~~~~~~~~~~~~"
        )
    );
}

/// A strict `ALWAYS` Environment's `enableIf` expression does not leave the
/// entrance of `X`'s Sessions undecided at template validation, since it
/// cannot disable the Environment: one that lists `X` is the cycle `X ->
/// X`, found at template validation. (This pins the case without an entry;
/// the one with an entry is the next test.)
#[test]
fn a_strict_always_environments_enable_if_expression_does_not_defer_its_cycle() {
    assert_eq!(
        decode(&job(
            &listing_x("    activation: ALWAYS\n    enableIf: \"Job.Name == 'J'\"\n"),
            "",
            "",
            "",
        ))
        .unwrap_err(),
        format!("{JOB_ERR}JobTemplate: {}", own_entry_cycle())
    );
}

/// The same Environment with `X`'s own entry for it is the cycle X -> X,
/// found at template validation.
#[test]
fn a_services_entry_for_a_strict_always_environment_with_an_enable_if_is_a_cycle() {
    assert_eq!(
        decode(&job(
            &listing_x("    activation: ALWAYS\n    enableIf: \"Job.Name == 'J'\"\n"),
            "    dependencies: [{environment: E}]\n",
            "",
            ""
        ))
        .unwrap_err(),
        format!("{JOB_ERR}JobTemplate: {}", own_entry_cycle())
    );
}

/// `discarded_service_host_requirements` is index for index with
/// `job.services`: the external Services first, in attachment order, then
/// the Job Template's, each with its own discards.
#[test]
fn discarded_service_host_requirements_follow_the_order_of_the_services() {
    let et = "specificationVersion: environment-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
services:
  - name: C
    dependencies: [{environment: Client, enableIf: false}]
    hostRequirements: {attributes: [{name: \"custom:attr.c\", anyOf: \"{{ None }}\"}]}
    ports: [{name: main}]
    script: {actions: {onRun: {command: run}}}
  - name: D
    dependencies: [{environment: Client, enableIf: false}]
    hostRequirements: {attributes: [{name: \"custom:attr.d\", anyOf: \"{{ None }}\"}]}
    ports: [{name: main}]
    script: {actions: {onRun: {command: run}}}
environment:
  name: Client
  dependencies: [{service: C}, {service: D}]
  variables: {K: v}
";
    let hr =
        "    hostRequirements: {attributes: [{name: \"custom:attr.x\", anyOf: \"{{ None }}\"}]}\n";
    let created = create_details_with(&job("", hr, "", ""), &[et]);
    let names: Vec<&str> = created
        .job
        .services
        .iter()
        .flatten()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, ["C", "D", "X"]);
    let discarded: Vec<Vec<&str>> = created
        .discarded_service_host_requirements
        .iter()
        .map(|d| d.iter().map(|r| r.name.as_str()).collect())
        .collect();
    assert_eq!(
        discarded,
        [
            vec!["custom:attr.c"],
            vec!["custom:attr.d"],
            vec!["custom:attr.x"]
        ]
    );
}

/// An Environment Template with Service `X` (Windows only) and an
/// Environment `Client` that lists it and that `X` declines, so a Step
/// entering `Client` uses it.
const EXT_X_WINDOWS: &str = "specificationVersion: environment-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
services:
  - name: X
    dependencies: [{environment: Client, enableIf: false}]
    hostRequirements: {attributes: [{name: attr.worker.os.family, anyOf: [windows]}]}
    ports: [{name: main}]
    script: {actions: {onRun: {command: run}}}
environment:
  name: Client
  dependencies: [{service: X}]
  variables: {K: v}
";

/// An attached Environment `H` (Linux only) with `activation_lines`.
fn h_linux(activation_lines: &str) -> String {
    format!(
        "specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: H
{activation_lines}  hostRequirements: {{attributes: [{{name: attr.worker.os.family, anyOf: [linux]}}]}}
  variables: {{K: v}}
"
    )
}

/// An external Service enters every `ALWAYS` and `BY_DEFAULT` Environment
/// attached before its document that it does not decline, and `check
/// --environment` finds the conflict with its own requirements as job
/// creation does; it does not enter an `ON_REQUEST` one, nor a
/// `BY_DEFAULT` one its literal `enableIf: false` disables, while a strict
/// `ALWAYS` one is entered whatever its `enableIf`.
#[test]
fn check_enters_attachments_for_an_external_service_by_activation() {
    let t = one_step("", "");
    let conflict = |h: &str| agree(&t, &[h, EXT_X_WINDOWS]);
    let expected = format!(
        "{SUBMISSION_ERR}EnvironmentTemplate[1] -> services[0]:\n\tService 'X' \
         (EnvironmentTemplate[1]): no host can satisfy the combined host requirements for \
         attribute 'attr.worker.os.family'"
    );
    for h in [
        h_linux(""),
        h_linux("  activation: BY_DEFAULT\n"),
        h_linux("  activation: ALWAYS\n"),
        h_linux("  activation: ALWAYS\n  enableIf: \"Job.Name == 'J'\"\n"),
    ] {
        let err = conflict(&h).unwrap_or_else(|| panic!("no conflict for {h}"));
        assert!(err.starts_with(&expected), "{h}\n{err}");
    }
    for h in [
        h_linux("  activation: ON_REQUEST\n"),
        h_linux("  enableIf: false\n"),
    ] {
        assert_eq!(conflict(&h), None, "{h}");
    }
}

/// At job creation, an attached Environment whose `enableIf` fails for an
/// external Service, which enters it with every parameter at its
/// `default`, is an error whose note names the Service; the Step supplies
/// a value that resolves.
#[test]
fn an_external_services_failed_enable_if_note_names_the_service() {
    let e = "specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions:
    - {name: S, type: STRING, default: abc}
    - {name: R, type: STRING, default: r}
  enableIf: \"int(Env.Param.S) > 0\"
  variables: {K: v}
";
    let t = one_step("{environment: E, parameters: {S: '1'}}", "");
    assert_eq!(
        create(&t, &[e, EXT_X_WINDOWS]).unwrap_err(),
        format!(
            "{SUBMISSION_ERR}EnvironmentTemplate[0] -> environment -> enableIf:\n\tcould not be \
             evaluated: Cannot convert 'abc' to int. Resolved for Service 'X' \
             (EnvironmentTemplate[1]).\n  int(Env.Param.S) > 0\n  ^~~~~~~~~~~~~~~~"
        )
    );
}

/// An attachment with a parameter without a default that an entry with an
/// `enableIf` expression names: where the entry is disabled it declines
/// the `BY_DEFAULT` Environment (§3.2 item 3.2), so job creation does not
/// combine its host requirements, and `check` leaves it out of the
/// combination whatever the entry resolves to, never reporting a conflict
/// job creation does not.
#[test]
fn check_leaves_out_an_attachment_an_enable_if_expression_entry_names() {
    for (cond, enabled) in [("Job.Name == 'J'", true), ("Job.Name == 'K'", false)] {
        let t = one_step(
            &format!("{{environment: E, enableIf: \"{cond}\", parameters: {{Need: 3}}}}"),
            MIN_8,
        );
        let created = create(&t, &[&need_env("")]).map(|_| ());
        if enabled {
            assert_eq!(
                created.unwrap_err(),
                format!(
                    "{SUBMISSION_ERR}JobTemplate -> steps[0]:\n\tStep 'A': no host can satisfy \
                     the combined host requirements for amount 'amount.worker.vcpu': JobTemplate \
                     -> steps[0] -> hostRequirements requires min 8, and EnvironmentTemplate[0] \
                     -> environment -> hostRequirements (Environment 'E') requires max 4. \
                     Template Schemas §3.3.3."
                )
            );
        } else {
            assert_eq!(created, Ok(()), "{cond}");
        }
        assert_eq!(check(&t, &[&need_env("")]), Ok(()), "{cond}");
    }
}

/// [`need_env`] with `activation: ALWAYS`.
fn always_need_env(param_extra: &str) -> String {
    need_env(param_extra).replace("  name: E\n", "  name: E\n  activation: ALWAYS\n")
}

/// `check --environment` matches an `externalServices` requirement's ports
/// against the attached Service (Template Schemas §1.2.2 item 2), as job
/// creation does: a missing port and a protocol mismatch are both found.
#[test]
fn check_matches_a_requirements_ports_against_the_attached_service() {
    let provider = |ports: &str| {
        format!(
            "specificationVersion: environment-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
services:
  - name: Cache
    ports: {ports}
    healthCheck: {{type: STDOUT}}
    script: {{actions: {{onRun: {{command: run}}}}}}
"
        )
    };
    let t = requires_cache("", "");
    assert_eq!(agree(&t, &[&provider("[{name: main}]")]), None);
    let missing = agree(&t, &[&provider("[{name: other}]")]).unwrap();
    assert!(
        missing.contains(
            "required Service 'Cache' is provided by EnvironmentTemplate[0], which is missing \
             port 'main'; its ports: other"
        ),
        "{missing}"
    );
    let udp = agree(&t, &[&provider("[{name: main, protocol: UDP}]")]).unwrap();
    assert!(udp.contains("protocol"), "{udp}");
}

/// A Job Template with a STRING Job Parameter `Mode` (default `off`) and a
/// Step `A` that requires 8 vCPUs and has an entry for `E` with `parameters`
/// (a flow-map body) and `enableIf: "Param.Mode == 'on'"`.
fn mode_entry(parameters: &str) -> String {
    one_step(
        &format!(
            "{{environment: E, enableIf: \"Param.Mode == 'on'\", parameters: {{{parameters}}}}}"
        ),
        MIN_8,
    )
    .replace(
        "name: J\n",
        "name: J\nparameterDefinitions: [{name: Mode, type: STRING, default: \"off\"}]\n",
    )
}

/// Job creation's error for `template` with `Mode` set to `mode`.
fn create_with_mode(template: &str, attachments: &[&str], mode: &str) -> Result<(), String> {
    let jt = decode(template)?;
    let ets: Vec<_> = attachments
        .iter()
        .map(|t| {
            decode_environment_template(yaml_val(t), Some(EXTS), &CallerLimits::default())
                .expect("environment template should validate")
        })
        .collect();
    let td = tempfile::TempDir::new().unwrap();
    let dir = td.path().to_str().unwrap();
    let input: JobParameterInputValues = [(
        "Mode".to_string(),
        openjd_expr::ExprValue::String(mode.to_string()),
    )]
    .into_iter()
    .collect();
    let processed =
        preprocess_job_parameters(&jt, &input, &ets, &PathParameterOptions::new(dir, dir))
            .map_err(|e| e.to_string())?;
    let attached: Vec<AttachedEnvironmentTemplate<'_>> = ets.iter().map(Into::into).collect();
    create_job_with_environment_templates(
        &jt,
        &attached,
        &processed,
        &jt.default_validation_context(),
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// An entry with an `enableIf` expression whose name two attachments
/// define: the name is an error only where the entry is enabled (§3.2.1
/// item 3), which is known only when the Job is created (§7.6 rule 5), so
/// check does not report it. Job creation then has no values for either
/// attachment, so check leaves them out of the combination and reports
/// nothing.
#[test]
fn check_leaves_out_attachments_an_ambiguous_enable_if_expression_entry_names() {
    let t = mode_entry("Need: 3");
    let att = always_need_env(", default: 1");
    assert_eq!(
        create_with_mode(&t, &[&att, &att], "on").unwrap_err(),
        format!(
            "{SUBMISSION_ERR}JobTemplate -> steps[0] -> dependencies[0] -> environment:\n\tnames \
             Environment 'E', which 2 attached Environment Templates define \
             (EnvironmentTemplate[0], EnvironmentTemplate[1]); an entry must match exactly one \
             (Template Schemas §3.2.1 item 3)."
        )
    );
    assert_eq!(check(&t, &[&att, &att]), Ok(()));
}

/// A Job Template Service's entry whose `enableIf` is an expression is
/// decided only when the Job is created (§7.6 rule 5), as a Step's is: an
/// entry that would decline an `ALWAYS` attachment, and one whose name two
/// `BY_DEFAULT` attachments define, pass `check` and are rejected by job
/// creation only for the `Mode` that makes them wrong. Where the entry is
/// enabled, job creation has no values for two ambiguous attachments, so
/// check leaves them out of the Service's host requirements.
#[test]
fn check_leaves_a_services_entry_with_an_enable_if_expression_to_job_creation() {
    let t = service_job(
        "",
        "    dependencies: [{environment: E, enableIf: \"Param.Mode == 'on'\"}]\n",
        "",
    )
    .replace(
        "name: J\n",
        "name: J\nparameterDefinitions: [{name: Mode, type: STRING, default: \"off\"}]\n",
    );
    let e = "specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  activation: ALWAYS
  variables: {K: v}
";
    // One `ALWAYS` E: declining it is wrong only when `Mode` is `off`.
    assert_eq!(check(&t, &[e]), Ok(()));
    assert_eq!(create_with_mode(&t, &[e], "on"), Ok(()));
    assert_eq!(
        create_with_mode(&t, &[e], "off").unwrap_err(),
        format!(
            "{SUBMISSION_ERR}JobTemplate -> services[0] -> dependencies[0] -> enableIf:\n\tis \
             false, so Service 'X' would decline Environment 'E' (EnvironmentTemplate[0]), whose \
             activation is ALWAYS: no Service can leave out an ALWAYS Environment (Template \
             Schemas §3.2 item 3.2)."
        )
    );
    // Two `BY_DEFAULT` E: the name is an error only where the entry is
    // enabled; disabled, it declines both.
    let by_default_e = e.replace("ALWAYS", "BY_DEFAULT");
    let e2 = by_default_e.as_str();
    assert_eq!(check(&t, &[e2, e2]), Ok(()));
    assert_eq!(create_with_mode(&t, &[e2, e2], "off"), Ok(()));
    assert_eq!(
        create_with_mode(&t, &[e2, e2], "on").unwrap_err(),
        format!(
            "{SUBMISSION_ERR}JobTemplate -> services[0] -> dependencies[0] -> environment:\n\tnames \
             Environment 'E', which 2 attached Environment Templates define \
             (EnvironmentTemplate[0], EnvironmentTemplate[1]); an entry must match exactly one \
             (Template Schemas §3.2.1 item 3)."
        )
    );
    // Where it is, job creation has no values for either E for the
    // Service, so check leaves both out of the Service's host requirements:
    // their `max: 4` vCPUs would otherwise conflict with its `min: 8`.
    let hosted = service_job(
        "",
        &format!(
            "    dependencies: [{{environment: E, enableIf: \"Param.Mode == 'on'\", parameters: \
             {{Need: 3}}}}]\n{MIN_8}"
        ),
        "",
    )
    .replace(
        "name: J\n",
        "name: J\nparameterDefinitions: [{name: Mode, type: STRING, default: \"off\"}]\n",
    );
    let att = always_need_env(", default: 1");
    assert_eq!(check(&hosted, &[&att, &att]), Ok(()));
}

/// A Job Template Service's decline of an attached `ALWAYS` Environment
/// is a §7.4.1 step 4 error, filed after every Step's step 4 errors, by
/// `check` and job creation alike (§3.2 item 3.2).
#[test]
fn a_services_always_decline_is_a_step_4_error_after_the_steps() {
    let always = |name: &str| {
        format!(
            "specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: {name}
  activation: ALWAYS
  variables: {{K: v}}
"
        )
    };
    let t = service_job(
        "",
        "    dependencies: [{environment: W, enableIf: false}]\n",
        ", {environment: Z, enableIf: false}",
    );
    check_and_create_err(
        &t,
        &[&always("W"), &always("Z")],
        "Model validation error: 2 validation errors for Submission\n\
         JobTemplate -> steps[0] -> dependencies[1] -> enableIf:\n\tis false, so Step 'A' would \
         decline Environment 'Z' (EnvironmentTemplate[1]), whose activation is ALWAYS: no Step \
         can leave out an ALWAYS Environment (Template Schemas §3.2 item 3.2).\n\
         JobTemplate -> services[0] -> dependencies[0] -> enableIf:\n\tis false, so Service 'X' \
         would decline Environment 'W' (EnvironmentTemplate[0]), whose activation is ALWAYS: no \
         Service can leave out an ALWAYS Environment (Template Schemas §3.2 item 3.2).",
    );
}

/// A Job Template Service's disabled entry for an attachment from a
/// document that does not list `ENV_PARAMS` has no effect and is not an
/// error (§4 item 4): the Service still enters it, in `check` and in job
/// creation.
#[test]
fn a_services_disabled_entry_for_a_pre_extension_attachment_has_no_effect() {
    let pre = "specificationVersion: environment-2023-09
environment:
  name: E
  variables: {K: v}
";
    let t = service_job(
        "",
        "    dependencies: [{environment: E, enableIf: false}]\n",
        "",
    );
    assert_eq!(check(&t, &[pre]), Ok(()));
    let created = create_details_with(&t, &[pre]);
    let x = &created.job.services.as_ref().unwrap()[0];
    assert!(x.enters_environment(&job::Document::environment_template(0, None), "E"));
}

/// `check` reports a Job Template Service's constraint 7 error for an
/// `ALWAYS` attachment its disabled entry would decline, with the note
/// naming that entry, after the decline error; job creation stops the
/// Service after §7.4.1 step 4, so it reports the decline only.
#[test]
fn check_names_a_services_disabled_entry_in_its_missing_parameters_message() {
    let t = service_job(
        "",
        "    dependencies: [{environment: Q, enableIf: false}]\n",
        ", {environment: Q, parameters: {R: r}}",
    );
    let q = q_requiring_r("  activation: ALWAYS\n");
    let decline = "JobTemplate -> services[0] -> dependencies[0] -> enableIf:\n\tis false, so \
                   Service 'X' would decline Environment 'Q' (EnvironmentTemplate[0]), whose \
                   activation is ALWAYS: no Service can leave out an ALWAYS Environment (Template \
                   Schemas §3.2 item 3.2).";
    assert_eq!(
        check(&t, &[&q]).unwrap_err(),
        format!(
            "Model validation error: 2 validation errors for Submission\n{decline}\n\
             JobTemplate -> services[0]:\n\t{} (the Service's 'environment:' dependency for it \
             is disabled by its enableIf, so it supplies nothing; Template Schemas §3.2 item \
             3.2).",
            SERVICE_MUST_SUPPLY.trim_end_matches('.')
        )
    );
    assert_eq!(
        create(&t, &[&q]).unwrap_err(),
        format!("{SUBMISSION_ERR}{decline}")
    );
}

/// A Job Template Service's entry whose `enableIf` is an expression may
/// request or decline the attachment it names, which is decided only when
/// the Job is created (§7.6 rule 5), so `check` neither holds the Service
/// to that attachment's parameters nor combines its host requirements or
/// its values, as for a Step.
#[test]
fn check_decides_nothing_for_a_services_entry_with_an_enable_if_expression() {
    let mode = |t: String| {
        t.replace(
            "name: J\n",
            "name: J\nparameterDefinitions: [{name: Mode, type: STRING, default: \"off\"}]\n",
        )
    };
    // A `BY_DEFAULT` Q with a parameter without a default: the Service
    // supplies nothing, so only an enabled entry leaves it to supply R.
    let t = mode(service_job(
        "",
        "    dependencies: [{environment: Q, enableIf: \"Param.Mode == 'on'\"}]\n",
        ", {environment: Q, parameters: {R: r}}",
    ));
    let q = q_requiring_r("");
    assert_eq!(check(&t, &[&q]), Ok(()));
    assert_eq!(create_with_mode(&t, &[&q], "off"), Ok(()));
    assert_eq!(
        create_with_mode(&t, &[&q], "on").unwrap_err(),
        format!("{SUBMISSION_ERR}JobTemplate -> services[0]:\n\t{SERVICE_MUST_SUPPLY}")
    );
    // A `BY_DEFAULT` E whose `max: 4` vCPUs conflicts with the Service's
    // `min: 8`: combined only where the entry is enabled.
    let hosted = mode(service_job(
        "",
        &format!(
            "    dependencies: [{{environment: E, enableIf: \"Param.Mode == 'on'\", parameters: \
             {{Need: 3}}}}]\n{MIN_8}"
        ),
        "",
    ));
    let e = need_env(", default: 1");
    assert_eq!(check(&hosted, &[&e]), Ok(()));
    assert_eq!(create_with_mode(&hosted, &[&e], "off"), Ok(()));
    assert_eq!(
        create_with_mode(&hosted, &[&e], "on").unwrap_err(),
        format!(
            "{SUBMISSION_ERR}JobTemplate -> services[0]:\n\tService 'X': no host can satisfy the \
             combined host requirements for amount 'amount.worker.vcpu': JobTemplate -> \
             services[0] -> hostRequirements requires min 8, and EnvironmentTemplate[0] -> \
             environment -> hostRequirements (Environment 'E') requires max 4. Template Schemas \
             §3.3.3."
        )
    );
    // An `ALWAYS` E whose entry value fails constraint 6 where the `default`
    // is valid: job creation has no values for it where the entry is
    // enabled, so check leaves it out of the combination.
    let e = always_need_env(", default: 6, minValue: 5");
    assert_eq!(check(&hosted, &[&e]), Ok(()));
    assert_eq!(
        create_with_mode(&hosted, &[&e], "on").unwrap_err(),
        format!(
            "{SUBMISSION_ERR}JobTemplate -> services[0] -> dependencies[0] -> parameters -> \
             Need:\n\tParameter 'Need': value 3 is less than minimum 5"
        )
    );
}

/// An attached `BY_DEFAULT` Environment `E` (with `extra`, indented two
/// spaces) with a STRING parameter `R` without a default.
fn e_requiring_r(extra: &str) -> String {
    format!(
        "specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  activation: BY_DEFAULT
{extra}  envParameterDefinitions: [{{name: R, type: STRING}}]
  variables: {{K: \"{{{{Env.Param.R}}}}\"}}
"
    )
}

/// [`service_job`] with the STRING Job Parameter `Mode` (default `off`).
fn with_mode(t: String) -> String {
    t.replace(
        "name: J\n",
        "name: J\nparameterDefinitions: [{name: Mode, type: STRING, default: \"off\"}]\n",
    )
}

/// Two attachments define `BY_DEFAULT` `E` with a parameter without a
/// default, which no enabled entry can supply (§3.2.1 item 3). The error
/// at each attachment is reported where some Step or Job Template Service
/// that is not stopped does not decline `E`: here only the Service, since
/// the Step declines it. With an `enableIf` on `E`, the message says it
/// applies whatever that `enableIf` is. `check` and job creation agree.
#[test]
fn an_ambiguous_attachment_is_an_error_when_only_a_service_does_not_decline_it() {
    let t = service_job("", "", ", {environment: E, enableIf: false}");
    let unreachable = |doc: &str, applies: &str| {
        format!(
            "{doc} -> environment -> envParameterDefinitions:\n\tEnvironment 'E' is defined by \
             2 attached Environment Templates (EnvironmentTemplate[0], EnvironmentTemplate[1]), \
             so no enabled 'environment:' dependency can reach it (Template Schemas §3.2.1 item \
             3), and it has parameters without a default that no Step or Service can supply: R. \
             Its activation is BY_DEFAULT, so it applies to every Step and Service that does not \
             decline it with a disabled entry, {applies}and at least one Step or Service does not \
             decline it, and the submission is rejected (§3.2.1)."
        )
    };
    let both = |applies: &str| {
        format!(
            "Model validation error: 2 validation errors for Submission\n{}\n{}",
            unreachable("EnvironmentTemplate[0]", applies),
            unreachable("EnvironmentTemplate[1]", applies)
        )
    };
    let e = e_requiring_r("");
    check_and_create_err(&t, &[&e, &e], &both(""));
    let e = e_requiring_r("  enableIf: \"true\"\n");
    check_and_create_err(
        &t,
        &[&e, &e],
        &both("whatever its enableIf and exclusiveGroup (§3.2 constraint 7), "),
    );
    // When the Service declines `E` too, nothing applies it.
    let t = service_job(
        "",
        "    dependencies: [{environment: E, enableIf: false}]\n",
        ", {environment: E, enableIf: false}",
    );
    let e = e_requiring_r("");
    assert_eq!(check(&t, &[&e, &e]), Ok(()));
    assert_eq!(create(&t, &[&e, &e]).map(|_| ()), Ok(()));
}

/// A Job Template Service's entry whose `enableIf` is an expression may
/// decline the two `BY_DEFAULT` `E` that the Step declines, so `check`
/// does not hold them to constraint 7 for it; job creation decides by
/// `Mode`.
#[test]
fn check_leaves_ambiguous_attachments_a_services_expression_entry_may_decline() {
    let t = with_mode(service_job(
        "",
        "    dependencies: [{environment: E, enableIf: \"Param.Mode == 'on'\"}]\n",
        ", {environment: E, enableIf: false}",
    ));
    let e = e_requiring_r("");
    assert_eq!(check(&t, &[&e, &e]), Ok(()));
    assert_eq!(create_with_mode(&t, &[&e, &e], "off"), Ok(()));
    assert_eq!(
        create_with_mode(&t, &[&e, &e], "on").unwrap_err(),
        format!(
            "{SUBMISSION_ERR}JobTemplate -> services[0] -> dependencies[0] -> environment:\n\tnames \
             Environment 'E', which 2 attached Environment Templates define \
             (EnvironmentTemplate[0], EnvironmentTemplate[1]); an entry must match exactly one \
             (Template Schemas §3.2.1 item 3)."
        )
    );
}

/// The `exclusiveGroup` rule at `check` for a Job Template Service's entry
/// whose `enableIf` is an expression and whose name two attachments in the
/// group define: whether either is entered is decided at job creation, so
/// a later member of the group the Service requests, with a value that
/// fails, may be superseded and is not checked, as for a Step.
#[test]
fn a_services_expression_entry_for_ambiguous_grouped_attachments_leaves_the_group_undetermined() {
    let grouped = |name: &str, params: &str| {
        format!(
            "specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: {name}
  activation: ON_REQUEST
  exclusiveGroup: G
{params}  variables: {{K: v}}
"
        )
    };
    let e = grouped("E", "");
    let l = grouped(
        "L",
        "  envParameterDefinitions: [{name: Q, type: STRING, allowedValues: [good]}]\n",
    );
    let t = with_mode(service_job(
        "",
        "    dependencies: [{environment: E, enableIf: \"Param.Mode == 'on'\"}, {environment: L, \
         parameters: {Q: bad}}]\n",
        "",
    ));
    assert_eq!(check(&t, &[&e, &e, &l]), Ok(()));
    assert_eq!(
        create_with_mode(&t, &[&e, &e, &l], "off").unwrap_err(),
        format!(
            "{SUBMISSION_ERR}JobTemplate -> services[0] -> dependencies[1] -> parameters -> \
             Q:\n\tParameter 'Q': value 'bad' is not in allowed values"
        )
    );
}

/// RFC 0010 S2 (Template Schemas §4.5 item 1): an Environment parameter
/// `default` may read `Job.Name`, also for the Environments a Job Template
/// Service's Sessions enter. The Step declines `W`, so only the Service
/// resolves its `default`.
#[test]
fn a_services_environment_default_reads_job_name() {
    let w = "specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: W
  envParameterDefinitions: [{name: T, type: STRING, default: \"job-{{Job.Name}}\"}]
  variables: {K: \"{{Env.Param.T}}\"}
";
    let t = service_job("", "", ", {environment: W, enableIf: false}");
    assert_eq!(check(&t, &[w]), Ok(()));
    let created = create_details_with(&t, &[w]);
    assert_eq!(
        values(&created.job.services.as_ref().unwrap()[0]),
        [("W.T".to_string(), "job-J".to_string())]
    );
}

/// The §3.2 item 3.2 error for Step `A`'s entry when `Mode` is `off`: it
/// would decline the `ALWAYS` `E`.
const A_DECLINES_ALWAYS_E: &str = "JobTemplate -> steps[0] -> dependencies[0] -> enableIf:\n\tis \
     false, so Step 'A' would decline Environment 'E' (EnvironmentTemplate[0]), whose activation \
     is ALWAYS: no Step can leave out an ALWAYS Environment (Template Schemas §3.2 item 3.2).";

/// Constraint 5 alone (an undeclared key in the entry) does not make the
/// values uncertain: job creation combines the `ALWAYS` attachment when the
/// entry is enabled, so check still finds the conflict, as job creation
/// does. Disabled, the entry would decline the `ALWAYS` attachment, which
/// is an error of its own.
#[test]
fn check_keeps_an_attachment_an_enable_if_expression_entry_names_with_an_extra_key() {
    let t = mode_entry("Need: 3, Bogus: 1");
    let att = always_need_env(", default: 1");
    let err = create_with_mode(&t, &[&att], "on").unwrap_err();
    assert!(err.contains("no host can satisfy"), "{err}");
    assert_eq!(
        create_with_mode(&t, &[&att], "off").unwrap_err(),
        format!("{SUBMISSION_ERR}{A_DECLINES_ALWAYS_E}")
    );
    assert!(check(&t, &[&att])
        .unwrap_err()
        .contains("no host can satisfy"));
}

/// An entry value that fails constraint 6 where the `default` is valid: job
/// creation has no values for the `ALWAYS` attachment when the entry is
/// enabled, so check leaves it out of the combination. (Disabled, the entry
/// would decline the `ALWAYS` attachment, an error of its own.)
#[test]
fn check_leaves_out_an_attachment_whose_enable_if_expression_entry_value_fails() {
    let t = mode_entry("Need: 3");
    let att = always_need_env(", default: 6, minValue: 5");
    assert_eq!(
        create_with_mode(&t, &[&att], "on").unwrap_err(),
        format!(
            "{SUBMISSION_ERR}JobTemplate -> steps[0] -> dependencies[0] -> parameters -> Need:\n\t\
             Parameter 'Need': value 3 is less than minimum 5"
        )
    );
    assert_eq!(
        create_with_mode(&t, &[&att], "off").unwrap_err(),
        format!("{SUBMISSION_ERR}{A_DECLINES_ALWAYS_E}")
    );
    assert_eq!(check(&t, &[&att]), Ok(()));
}

// ════════════════════════════════════════════════════════════════════
// ENV_PARAMS §3.2 item 3.2 with SERVICE: a disabled `environment:` entry
// declines a `BY_DEFAULT` Environment, for a Step and for a Service alike
// (RFC 0009 `<StepDependency>` item 3), and a declining Step is not in the
// scope of a Service that Environment lists (Service scope rule 3)
// ════════════════════════════════════════════════════════════════════

/// [`job`] with `E` `BY_DEFAULT` and listing `X`, which declines it, and
/// Step `B`'s dependencies the flow-list body `b_deps`.
fn b_entry_for_listing_e(b_deps: &str) -> String {
    job(
        &listing_x(""),
        "    dependencies: [{environment: E, enableIf: false}]\n",
        "",
        &format!("    dependencies: [{b_deps}]\n"),
    )
}

/// Step `B` declines `E`, which lists `X`, so `B`'s Sessions do not enter
/// it and `B` is not in `X`'s scope, at template validation and at job
/// creation. Without the decline, every Step enters a `BY_DEFAULT`
/// Environment, and `X`'s scope is every Step.
#[test]
fn a_step_that_declines_a_by_default_environment_listing_a_service_is_not_in_its_scope() {
    let t = b_entry_for_listing_e("{environment: E, enableIf: false}");
    let jt = decode(&t).unwrap();
    let scopes = openjd_model::template::compute_service_scopes(&jt).unwrap();
    assert_eq!(scopes.get("X").unwrap().scope, ServiceScope::steps(["A"]));
    let job = create(&t, &[]).unwrap();
    assert_eq!(
        job.services.as_ref().unwrap()[0].scope,
        ServiceScope::steps(["A"])
    );
    assert!(job.steps[0].enters_environment(&job::Document::JobTemplate, "E"));
    assert!(!job.steps[1].enters_environment(&job::Document::JobTemplate, "E"));
}

/// The same with an `enableIf` expression on `B`'s entry: template
/// validation cannot tell whether `B` declines `E`, so it leaves `B` out
/// of the scope it knows; job creation decides it.
#[test]
fn a_step_whose_expression_entry_may_decline_a_listing_environment_is_decided_at_job_creation() {
    for (cond, b_in_scope) in [("Job.Name == 'J'", true), ("Job.Name == 'K'", false)] {
        let t = b_entry_for_listing_e(&format!("{{environment: E, enableIf: \"{cond}\"}}"));
        let jt = decode(&t).unwrap();
        let scopes = openjd_model::template::compute_service_scopes(&jt).unwrap();
        assert_eq!(
            scopes.get("X").unwrap().scope,
            ServiceScope::steps(["A"]),
            "{cond}"
        );
        let job = create(&t, &[]).unwrap();
        let scope = &job.services.as_ref().unwrap()[0].scope;
        assert!(scope.contains("A"), "{cond}: {scope:?}");
        assert_eq!(scope.contains("B"), b_in_scope, "{cond}: {scope:?}");
    }
}

/// A `BY_DEFAULT` Job Environment `E` with a STRING parameter `V`
/// (default `d`) that lists no Service.
const BY_DEFAULT_V: &str = "  - name: E
    envParameterDefinitions: [{name: V, type: STRING, default: d}]
    variables: {K: \"{{Env.Param.V}}\"}
";

/// A Service's disabled entry declines a `BY_DEFAULT` Job Environment as
/// a Step's does: its Sessions do not enter it and get no values for it,
/// while the Steps still enter it.
#[test]
fn a_services_disabled_entry_declines_a_by_default_job_environment() {
    let created = create(
        &job(
            BY_DEFAULT_V,
            "    dependencies: [{environment: E, enableIf: false}]\n",
            "",
            "",
        ),
        &[],
    )
    .unwrap();
    let x = &created.services.as_ref().unwrap()[0];
    assert_eq!(x.environments, Some(vec![]));
    assert!(x.environment_parameters.is_none());
    for step in &created.steps {
        assert!(step.enters_environment(&job::Document::JobTemplate, "E"));
    }
}

/// A Service's disabled entry declines a `BY_DEFAULT` Job Environment, so
/// constraint 7 does not apply to the Service for it at template
/// validation; a Step without an entry still must supply it.
#[test]
fn constraint_7_does_not_apply_to_a_job_environment_a_service_declines() {
    let env = "  - name: E
    envParameterDefinitions: [{name: R, type: STRING}]
    variables: {K: \"{{Env.Param.R}}\"}
";
    let t = service_job(
        env,
        "    dependencies: [{environment: E, enableIf: false}]\n",
        ", {environment: E, parameters: {R: r}}",
    );
    assert!(decode(&t).is_ok(), "{:?}", decode(&t).err());
    let undeclined = service_job(env, "", ", {environment: E, parameters: {R: r}}");
    assert_eq!(
        decode(&undeclined).unwrap_err(),
        job_service_must_supply("'R' of Environment 'E'")
    );
}

/// A Service's disabled entry declines a `BY_DEFAULT` attachment, at
/// `check` and at job creation: constraint 7 does not apply to it for the
/// Service, while the Step, which has no entry, must still supply it.
#[test]
fn a_services_disabled_entry_declines_a_by_default_attachment() {
    let t = service_job(
        "",
        "    dependencies: [{environment: Q, enableIf: false}]\n",
        "",
    );
    check_and_create_err(
        &t,
        &[&q_requiring_r("")],
        &format!("{SUBMISSION_ERR}{STEP_MUST_SUPPLY}"),
    );
    let created = create_details_with(
        &service_job(
            "",
            "    dependencies: [{environment: Q, enableIf: false}]\n",
            ", {environment: Q, parameters: {R: r}}",
        ),
        &[&q_requiring_r("")],
    );
    let x = &created.job.services.as_ref().unwrap()[0];
    assert!(!x.enters_environment(&job::Document::environment_template(0, None), "Q"));
}

/// A Job Template Service's disabled entry whose name two attachments
/// define is not an error, as for a Step (§3.2.1 item 3): it refers to
/// each of them, declining each `BY_DEFAULT` one, and is an error for
/// each `ALWAYS` one. `check` and job creation agree.
#[test]
fn a_services_disabled_entry_with_an_ambiguous_name_refers_to_each_attachment() {
    let q = |activation: &str, var: &str| {
        format!(
            "specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: Q
  activation: {activation}
  variables: {{{var}: v}}
"
        )
    };
    let t = service_job(
        "",
        "    dependencies: [{environment: Q, enableIf: false}]\n",
        "",
    );
    let (first, second) = (q("BY_DEFAULT", "FIRST"), q("BY_DEFAULT", "SECOND"));
    assert_eq!(check(&t, &[&first, &second]), Ok(()));
    let created = create_details_with(&t, &[&first, &second]);
    let x = &created.job.services.as_ref().unwrap()[0];
    let a = &created.job.steps[0];
    for d in 0..2 {
        let doc = job::Document::environment_template(d, None);
        assert!(!x.enters_environment(&doc, "Q"), "{d}");
        assert!(a.enters_environment(&doc, "Q"), "{d}");
    }
    // With the `ALWAYS` match first or second, declining it is the error.
    check_and_create_err(
        &t,
        &[&q("ALWAYS", "FIRST"), &second],
        &format!(
            "{SUBMISSION_ERR}JobTemplate -> services[0] -> dependencies[0] -> enableIf:\n\tis \
             false, so Service 'X' would decline Environment 'Q' (EnvironmentTemplate[0]), whose \
             activation is ALWAYS: no Service can leave out an ALWAYS Environment (Template \
             Schemas §3.2 item 3.2)."
        ),
    );
    let always = q("ALWAYS", "SECOND");
    check_and_create_err(
        &t,
        &[&first, &always],
        &format!(
            "{SUBMISSION_ERR}JobTemplate -> services[0] -> dependencies[0] -> enableIf:\n\tis \
             false, so Service 'X' would decline Environment 'Q' (EnvironmentTemplate[1]), whose \
             activation is ALWAYS: no Service can leave out an ALWAYS Environment (Template \
             Schemas §3.2 item 3.2)."
        ),
    );
}

/// A Service's disabled entry for a strict `ALWAYS` Environment is an
/// error naming the Service, as for a Step (§3.2 item 3.2): at template
/// validation for a Job Environment, and at `check` and job creation for
/// an attachment.
#[test]
fn a_services_disabled_entry_for_an_always_environment_is_an_error_naming_the_service() {
    let env = "  - name: E
    activation: ALWAYS
    variables: {K: v}
";
    assert_eq!(
        decode(&service_job(
            env,
            "    dependencies: [{environment: E, enableIf: false}]\n",
            ""
        ))
        .unwrap_err(),
        format!(
            "{JOB_ERR}services[0] -> dependencies[0] -> enableIf:\n\tis false, so Service 'X' \
             would decline Environment 'E', whose activation is ALWAYS: no Service can leave out \
             an ALWAYS Environment (Template Schemas §3.2 item 3.2)."
        )
    );
    let attached = "specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: Q
  activation: ALWAYS
  variables: {K: v}
";
    check_and_create_err(
        &service_job(
            "",
            "    dependencies: [{environment: Q, enableIf: false}]\n",
            "",
        ),
        &[attached],
        &format!(
            "{SUBMISSION_ERR}JobTemplate -> services[0] -> dependencies[0] -> enableIf:\n\tis \
             false, so Service 'X' would decline Environment 'Q' (EnvironmentTemplate[0]), whose \
             activation is ALWAYS: no Service can leave out an ALWAYS Environment (Template \
             Schemas §3.2 item 3.2)."
        ),
    );
}

/// When every Step that would enter `E`, which lists `X`, declines it (as
/// `X` does), no Step is in `X`'s scope and `X` is unused (scope rules 3
/// and 4).
#[test]
fn a_service_listed_only_by_an_environment_every_step_declines_is_unused() {
    let t = job(
        &listing_x(""),
        "    dependencies: [{environment: E, enableIf: false}]\n",
        "",
        "",
    )
    .replace(
        "    dependencies: [{service: X}]\n    script",
        "    dependencies: [{environment: E, enableIf: false}]\n    script",
    )
    .replace(
        "  - name: B\n",
        "  - name: B\n    dependencies: [{environment: E, enableIf: false}]\n",
    );
    assert_eq!(
        decode(&t).unwrap_err(),
        format!(
            "{JOB_ERR}services[0]:\n\tService 'X' is unused: no Step is in its scope (Job \
             Environment 'E' lists it, but no Step's Sessions enter it)."
        )
    );
}

/// An `ON_REQUEST` Environment that lists `X` and has an `enableIf`
/// expression, which no Step requests, puts no Step in `X`'s scope: the
/// expression cannot make a Step that does not request it enter it.
#[test]
fn an_unrequested_on_request_environment_with_an_enable_if_expression_leaves_its_service_unused() {
    let t = job(
        &listing_x("    activation: ON_REQUEST\n    enableIf: \"Job.Name == 'J'\"\n"),
        "",
        "",
        "",
    )
    .replace("    dependencies: [{service: X}]\n    script", "    script");
    assert_eq!(
        decode(&t).unwrap_err(),
        format!(
            "{JOB_ERR}services[0]:\n\tService 'X' is unused: no Step is in its scope (Job \
             Environment 'E' lists it, but no Step's Sessions enter it)."
        )
    );
}

/// A Service's entry for a strict `ALWAYS` Job Environment whose `enableIf`
/// expression resolves to `false` is the §3.2 item 3.2 error at job creation,
/// naming the Service; one that resolves to `true` is accepted.
#[test]
fn a_services_expression_entry_that_resolves_false_for_an_always_environment_is_an_error() {
    let env = "  - name: E
    activation: ALWAYS
    variables: {K: v}
";
    let entry = |cond: &str| {
        service_job(
            env,
            &format!("    dependencies: [{{environment: E, enableIf: \"{cond}\"}}]\n"),
            "",
        )
    };
    assert!(create(&entry("Job.Name == 'J'"), &[]).is_ok());
    assert_eq!(
        create(&entry("Job.Name == 'K'"), &[]).unwrap_err(),
        format!(
            "{JOB_ERR}services[0] -> dependencies[0] -> enableIf:\n\tis false, so Service 'X' \
             would decline Environment 'E', whose activation is ALWAYS: no Service can leave out \
             an ALWAYS Environment (Template Schemas §3.2 item 3.2)."
        )
    );
}

/// Constraint 7 for a Service that declines one `BY_DEFAULT` Job
/// Environment and enters another: only the entered one's parameters are
/// listed as missing.
#[test]
fn a_services_constraint_7_message_lists_only_the_environments_it_enters() {
    let envs = "  - name: E
    envParameterDefinitions: [{name: R, type: STRING}]
    variables: {K: \"{{Env.Param.R}}\"}
  - name: F
    envParameterDefinitions: [{name: S, type: STRING}]
    variables: {K: \"{{Env.Param.S}}\"}
";
    let t = service_job(
        envs,
        "    dependencies: [{environment: E, enableIf: false}]\n",
        ", {environment: E, parameters: {R: r}}, {environment: F, parameters: {S: s}}",
    );
    assert_eq!(
        decode(&t).unwrap_err(),
        job_service_must_supply("'S' of Environment 'F'")
    );
}

/// A Service that declines a `BY_DEFAULT` attachment does not combine its
/// host requirements, at `check` and at job creation alike, so a conflict
/// the undeclined attachment causes
/// (`check_combines_a_services_literal_host_requirements_with_attachments`)
/// is not reported.
#[test]
fn a_services_declined_attachment_is_left_out_of_its_host_requirements() {
    let t = service_job(
        "",
        &format!("{X_WINDOWS}    dependencies: [{{environment: H, enableIf: false}}]\n"),
        "",
    );
    assert_eq!(check(&t, &[H_LINUX]), Ok(()));
    assert!(create(&t, &[H_LINUX]).is_ok());
}

/// `X` lists `Back`, and the `BY_DEFAULT` Job Environment `E` lists `X`,
/// which declines it. `Back`'s Sessions would enter `E` and so depend on
/// `X`, a cycle; `back_entry` (a flow-list body, empty for none) may
/// decline it.
fn back_declines_e(back_entry: &str) -> String {
    let back_deps = if back_entry.is_empty() {
        String::new()
    } else {
        format!("    dependencies: [{back_entry}]\n")
    };
    format!(
        "specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
name: J
jobEnvironments:
  - name: E
    dependencies: [{{service: X}}]
    variables: {{K: \"{{{{Service.X.main.port}}}}\"}}
services:
  - name: X
    dependencies: [{{service: Back}}, {{environment: E, enableIf: false}}]
    ports: [{{name: main}}]
    healthCheck: {{type: STDOUT}}
    script: {{actions: {{onRun: {{command: run}}}}}}
  - name: Back
{back_deps}    ports: [{{name: main}}]
    healthCheck: {{type: STDOUT}}
    script: {{actions: {{onRun: {{command: run}}}}}}
steps:
  - name: A
    dependencies: [{{service: X}}]
    script: {{actions: {{onRun: {{command: run}}}}}}
"
    )
}

/// A Service that declines a `BY_DEFAULT` Job Environment listing another
/// Service does not depend on that Service through it, at template
/// validation and at job creation: `Back` declining `E` breaks the cycle
/// `X -> Back -> X` that entering `E` makes.
#[test]
fn a_service_that_declines_a_listing_environment_does_not_depend_on_its_service() {
    assert_eq!(
        decode(&back_declines_e("")).unwrap_err(),
        format!(
            "{JOB_ERR}JobTemplate: dependencies contain a cycle: Service X -> Service Back -> \
             Service X (the Sessions of Service Back enter Job Environment 'E', which lists \
             Service 'X', so Service Back depends on it; Template Schemas §3.2 constraint 3)."
        )
    );
    let declined = back_declines_e("{environment: E, enableIf: false}");
    assert!(decode(&declined).is_ok());
    assert!(create(&declined, &[]).is_ok());
}

/// The single-layer rule at template validation counts only the wrapping
/// Job Environments a Service's Sessions certainly enter: one it declines
/// is not a layer for it. (Every Step still enters both, which is
/// reported at `jobEnvironments`.)
#[test]
fn a_declined_wrapper_is_not_a_layer_for_a_service_at_template_validation() {
    let wraps = format!("  - name: W1\n{SEVEN_HOOKS}  - name: W2\n{SEVEN_HOOKS}");
    const LAYER: &str = "only one environment in the session stack may define any of \
                         onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit (RFC 0008).";
    assert_eq!(
        decode(&job(&wraps, "", "", "")).unwrap_err(),
        format!(
            "Model validation error: 2 validation errors for JobTemplate\njobEnvironments:\n\t\
             {LAYER}\nservices[0]:\n\t{LAYER}"
        )
    );
    assert_eq!(
        decode(&job(
            &wraps,
            "    dependencies: [{environment: W1, enableIf: false}]\n",
            "",
            ""
        ))
        .unwrap_err(),
        format!("{JOB_ERR}jobEnvironments:\n\t{LAYER}")
    );
}

/// A Service's disabled entry for a strict `ALWAYS` Job Environment that
/// lists the Service is the §3.2 item 3.2 error, and since it cannot
/// decline the Environment its Sessions still enter it: the cycle `X -> X`
/// too, both at template validation for a literal `false`. For an
/// expression the Sessions enter the Environment whatever it resolves to,
/// so template validation reports the cycle alone, without waiting for job
/// creation.
#[test]
fn a_services_disabled_entry_for_an_always_environment_that_lists_it_is_an_error_and_a_cycle() {
    let env = "  - name: E
    activation: ALWAYS
    dependencies: [{service: X}]
    variables: {K: \"{{Service.X.main.port}}\"}
";
    assert_eq!(
        decode(&service_job(
            env,
            "    dependencies: [{environment: E, enableIf: false}]\n",
            ""
        ))
        .unwrap_err(),
        format!(
            "Model validation error: 2 validation errors for JobTemplate\nservices[0] -> \
             dependencies[0] -> enableIf:\n\tis false, so Service 'X' would decline Environment \
             'E', whose activation is ALWAYS: no Service can leave out an ALWAYS Environment \
             (Template Schemas §3.2 item 3.2).\nJobTemplate: {}",
            own_entry_cycle()
        )
    );
    assert_eq!(
        decode(&service_job(
            env,
            "    dependencies: [{environment: E, enableIf: \"Job.Name == 'K'\"}]\n",
            ""
        ))
        .unwrap_err(),
        format!("{JOB_ERR}JobTemplate: {}", own_entry_cycle())
    );
}

/// A Service's enabled entry for a Job Environment that lists it is the
/// cycle `X -> X` only where the Environment is entered: one whose own
/// `enableIf` is `false` is entered by no Session, so template validation
/// and `check_environment_templates` accept the template.
#[test]
fn a_services_entry_for_a_disabled_environment_that_lists_it_is_not_a_cycle() {
    let env = "  - name: E
    enableIf: false
    dependencies: [{service: X}]
    variables: {K: \"{{Service.X.main.port}}\"}
";
    let t = service_job(env, "    dependencies: [{environment: E}]\n", "");
    assert!(decode(&t).is_ok(), "{:?}", decode(&t).err());
    assert_eq!(check(&t, &[]), Ok(()));
    assert_eq!(check(&t, &[H_LINUX]), Ok(()));
}

/// The wrap hooks a wrapper needs for Service Sessions only (RFC 0009
/// wrap hook rule 1), indented as for a `jobEnvironments` entry.
const SERVICE_ONLY_HOOKS: &str = "    script:
      actions:
        onWrapEnvEnter: {command: c}
        onWrapEnvExit: {command: c}
        onWrapServiceEnter: {command: c}
        onWrapServiceRun: {command: c}
        onWrapServiceHealthCheck: {command: c}
        onWrapServiceExit: {command: c}
";

/// Wrap hook rule 1 at template validation applies only to a kind of
/// Session known to enter the wrapper: a Service that declines a
/// `BY_DEFAULT` wrapper defining only the Task-Session hooks does not need
/// the Service hooks, and job creation agrees.
#[test]
fn wrap_hook_rule_1_skips_a_service_that_declines_the_wrapper() {
    let w = format!("  - name: W\n{THREE_HOOK_WRAPPER}");
    assert_eq!(
        decode(&job(&w, "", "", "")).unwrap_err(),
        format!(
            "{JOB_ERR}jobEnvironments[0] -> script -> actions:\n\tthe Sessions of Service 'X' \
             enter this wrapping environment, which therefore must define onWrapEnvEnter, \
             onWrapEnvExit, onWrapServiceEnter, onWrapServiceRun, onWrapServiceHealthCheck, and \
             onWrapServiceExit; missing: onWrapServiceEnter, onWrapServiceRun, \
             onWrapServiceHealthCheck, onWrapServiceExit (RFC 0009 wrap hook rule 1)."
        )
    );
    let t = job(
        &w,
        "    dependencies: [{environment: W, enableIf: false}]\n",
        "",
        "",
    );
    assert!(decode(&t).is_ok(), "{:?}", decode(&t).err());
    assert!(create(&t, &[]).is_ok());
}

/// The same for Steps: when every Step declines a `BY_DEFAULT` wrapper
/// defining only the Service-Session hooks, it needs no `onWrapTaskRun`.
#[test]
fn wrap_hook_rule_1_skips_steps_that_decline_the_wrapper() {
    let w = format!("  - name: W\n{SERVICE_ONLY_HOOKS}");
    assert_eq!(
        decode(&job(&w, "", "", "")).unwrap_err(),
        format!(
            "{JOB_ERR}jobEnvironments[0] -> script -> actions:\n\tthe Sessions of Step 'A' enter \
             this wrapping environment, which therefore must define onWrapEnvEnter, \
             onWrapTaskRun, and onWrapEnvExit; missing: onWrapTaskRun (RFC 0009 wrap hook rule \
             1)."
        )
    );
    let t = job(
        &w,
        "",
        ", {environment: W, enableIf: false}",
        "    dependencies: [{environment: W, enableIf: false}]\n",
    );
    assert!(decode(&t).is_ok(), "{:?}", decode(&t).err());
    assert!(create(&t, &[]).is_ok());
}

/// A Job Template with `externalServices` entry `Cache`, a `BY_DEFAULT`
/// Job Environment `H` that lists it, an inline Service `X`
/// (`x_deps`, a flow-list body), and Steps `A` (which lists `X`, then
/// `a_tail`) and `B` (`b_deps`, a flow-list body).
fn h_lists_cache(x_deps: &str, a_tail: &str, b_deps: &str) -> String {
    let deps = |d: &str| {
        if d.is_empty() {
            String::new()
        } else {
            format!("    dependencies: [{d}]\n")
        }
    };
    let (x_deps, b_deps) = (deps(x_deps), deps(b_deps));
    format!(
        "specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
name: J
externalServices:
  - name: Cache
    ports: [{{name: main}}]
jobEnvironments:
  - name: H
    dependencies: [{{service: Cache}}]
    variables: {{K: \"{{{{Service.Cache.main.port}}}}\"}}
services:
  - name: X
{x_deps}    ports: [{{name: main}}]
    healthCheck: {{type: STDOUT}}
    script: {{actions: {{onRun: {{command: run}}}}}}
steps:
  - name: A
    dependencies: [{{service: X}}{a_tail}]
    script: {{actions: {{onRun: {{command: run}}}}}}
  - name: B
{b_deps}    script: {{actions: {{onRun: {{command: run}}}}}}
"
    )
}

const DECLINE_H: &str = "{environment: H, enableIf: false}";

/// The scope of an `externalServices` entry at template validation follows
/// the same rules: a Step that declines the `BY_DEFAULT` Job Environment
/// listing it is not in its scope, and when every Step (and every Service
/// that would bring a Step in) declines it, the entry is unused.
#[test]
fn a_declining_step_or_service_is_not_in_an_external_service_requirements_scope() {
    let scope_of = |t: &str| {
        let jt = decode(t).unwrap();
        openjd_model::template::compute_service_scopes(&jt)
            .unwrap()
            .required_service_scopes()
            .get("Cache")
            .cloned()
    };
    // X declines H; A declines it too, so only B enters H, at template
    // validation and, for the attached Service bound to the entry, at job
    // creation.
    let t = h_lists_cache(DECLINE_H, &format!(", {DECLINE_H}"), "");
    assert_eq!(scope_of(&t), Some(ServiceScope::steps(["B"])));
    let provider = "specificationVersion: environment-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
services:
  - name: Cache
    ports: [{name: main}]
    healthCheck: {type: STDOUT}
    script: {actions: {onRun: {command: run}}}
";
    let created = create(&t, &[provider]).unwrap();
    let cache = created
        .services
        .as_ref()
        .unwrap()
        .iter()
        .find(|s| s.name == "Cache")
        .unwrap();
    assert_eq!(cache.scope, ServiceScope::steps(["B"]));
    // Only X brings A in: X enters H, so A, in X's scope, is in Cache's.
    assert_eq!(
        scope_of(&h_lists_cache("", &format!(", {DECLINE_H}"), DECLINE_H)),
        Some(ServiceScope::steps(["A"]))
    );
    // Every Step and X decline H: no Step is in Cache's scope.
    assert_eq!(
        decode(&h_lists_cache(
            DECLINE_H,
            &format!(", {DECLINE_H}"),
            DECLINE_H
        ))
        .unwrap_err(),
        format!(
            "{JOB_ERR}externalServices[0]:\n\texternal Service 'Cache' is unused: no Step, \
             Service, or Job Environment lists it."
        )
    );
}

/// A Job Template with Service `X`, whose `dependencies` hold `n` optional
/// `environment:` entries, and a Step that lists it.
fn service_with_entries(n: usize) -> String {
    let entries: Vec<String> = (0..n)
        .map(|i| format!("{{environment: E{i}, required: false}}"))
        .collect();
    format!(
        "specificationVersion: jobtemplate-2023-09
extensions: [EXPR, ENV_PARAMS, SERVICE]
name: J
services:
  - name: X
    dependencies: [{}]
    ports: [{{name: main}}]
    healthCheck: {{type: STDOUT}}
    script: {{actions: {{onRun: {{command: run}}}}}}
steps:
  - name: S
    dependencies: [{{service: X}}]
    script: {{actions: {{onRun: {{command: echo}}}}}}
",
        entries.join(", ")
    )
}

fn entry_limit(max: usize) -> CallerLimits {
    CallerLimits {
        max_step_environment_entries: Some(max),
        ..Default::default()
    }
}

/// The caller limit on `environment:` entries applies to each Service's
/// `dependencies` as to a Step's, counted as written and reported at the
/// Service's `dependencies`.
#[test]
fn the_environment_entry_limit_applies_to_a_job_template_service() {
    let at = decode_job_template(
        yaml_val(&service_with_entries(2)),
        Some(EXTS),
        &entry_limit(2),
    );
    assert!(at.is_ok(), "2 entries at a limit of 2: {:?}", at.err());
    let over = decode_job_template(
        yaml_val(&service_with_entries(3)),
        Some(EXTS),
        &entry_limit(2),
    )
    .unwrap_err();
    assert_eq!(
        over.to_string(),
        format!(
            "{JOB_ERR}services[0] -> dependencies:\n\texceeds caller limit of 2 'environment' \
             entries (3 entries)."
        )
    );
}

/// The limit applies to an Environment Template's Services too, whose
/// entries can only decline.
#[test]
fn the_environment_entry_limit_applies_to_an_environment_template_service() {
    let et = |n: usize| {
        let entries: Vec<String> = (0..n)
            .map(|i| format!("{{environment: E{i}, enableIf: false}}"))
            .collect();
        format!(
            "specificationVersion: environment-2023-09
extensions: [EXPR, ENV_PARAMS, SERVICE]
services:
  - name: C
    dependencies: [{}]
    ports: [{{name: main}}]
    healthCheck: {{type: STDOUT}}
    script: {{actions: {{onRun: {{command: run}}}}}}
",
            entries.join(", ")
        )
    };
    let at = decode_environment_template(yaml_val(&et(2)), Some(EXTS), &entry_limit(2));
    assert!(at.is_ok(), "2 entries at a limit of 2: {:?}", at.err());
    let over =
        decode_environment_template(yaml_val(&et(3)), Some(EXTS), &entry_limit(2)).unwrap_err();
    assert_eq!(
        over.to_string(),
        "Model validation error: 1 validation error for EnvironmentTemplate\n\
         services[0] -> dependencies:\n\texceeds caller limit of 2 'environment' entries (3 \
         entries)."
    );
}

/// Service `k`'s Environment statuses as `name@document: status`, with
/// ` by name@document` after a superseded one's.
fn service_statuses(created: &openjd_model::CreatedJob, k: usize) -> Vec<String> {
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

/// `CreatedJob::service_environment_statuses` gives a Job Template
/// Service's decisions as `environment_statuses` gives a Step's: a
/// declined `BY_DEFAULT` Environment, one whose `enableIf` is false for the
/// Service, the later member of an `exclusiveGroup`, a requested
/// `ON_REQUEST` one, an unrequested one left out, and an optional entry
/// that named nothing.
#[test]
fn service_environment_statuses_give_a_services_decisions() {
    let t = "specificationVersion: jobtemplate-2023-09
extensions: [EXPR, ENV_PARAMS, SERVICE]
name: J
jobEnvironments:
  - name: D
    variables: {K: v}
  - name: Skip
    enableIf: \"Job.Name == 'other'\"
    variables: {K: v}
  - name: G1
    exclusiveGroup: g
    variables: {K: v}
  - name: G2
    exclusiveGroup: g
    variables: {K: v}
  - name: R
    activation: ON_REQUEST
    variables: {K: v}
  - name: U
    activation: ON_REQUEST
    variables: {K: v}
services:
  - name: X
    dependencies:
      - {environment: D, enableIf: false}
      - {environment: R}
      - {environment: Missing, required: false}
    ports: [{name: main}]
    healthCheck: {type: STDOUT}
    script: {actions: {onRun: {command: run}}}
steps:
  - name: S
    dependencies: [{service: X}]
    script: {actions: {onRun: {command: echo}}}
";
    let created = create_details_with(t, &[]);
    assert_eq!(created.service_environment_statuses.len(), 1);
    assert_eq!(
        service_statuses(&created, 0),
        [
            "D@JobTemplate: declined",
            "Skip@JobTemplate: disabled",
            "G1@JobTemplate: entered",
            "G2@JobTemplate: superseded by G1@JobTemplate",
            "R@JobTemplate: entered",
        ]
    );
    let nf = &created.service_environment_statuses[0].not_found;
    assert_eq!(nf.len(), 1);
    assert_eq!(
        (nf[0].dependency_index, nf[0].environment.as_str()),
        (2, "Missing")
    );
    let x = &created.job.services.as_ref().unwrap()[0];
    let entered: Vec<&str> = x
        .environments
        .as_ref()
        .unwrap()
        .iter()
        .map(|e| e.environment.as_str())
        .collect();
    assert_eq!(entered, ["G1", "R"]);
}

/// An attached Service's statuses cover only its own document's
/// Environments and the earlier attachments', with its decline recorded;
/// a later attachment's Environment is not considered.
#[test]
fn an_attached_services_statuses_cover_only_its_own_and_earlier_documents() {
    let earlier = "specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: Early
  variables: {K: v}
";
    let later = "specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: Late
  variables: {K: v}
";
    let created = create_details_with(
        &requires_cache("", ", {environment: CacheClient, parameters: {R: x}}"),
        &[earlier, &client_requiring_r("Cache"), later],
    );
    let names: Vec<&str> = created
        .job
        .services
        .as_ref()
        .unwrap()
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    let cache = names.iter().position(|n| *n == "Cache").unwrap();
    assert_eq!(
        service_statuses(&created, cache),
        [
            "Early@EnvironmentTemplate[0]: entered",
            "CacheClient@EnvironmentTemplate[1]: declined",
        ]
    );
    assert!(created.service_environment_statuses[cache]
        .not_found
        .is_empty());
}

// ════════════════════════════════════════════════════════════════════
// §7.4.1 error order and stopping across Steps, the Job Template's
// Services, and attached Services
// ════════════════════════════════════════════════════════════════════

/// `template` with the STRING Job Parameter `P` (default `p`).
fn with_p(template: &str) -> String {
    template.replacen(
        "name: J\n",
        "name: J\nparameterDefinitions:\n  - {name: P, type: STRING, default: p}\n",
        1,
    )
}

/// An attachment whose Environment `Aw` has activation `ALWAYS`.
const ALWAYS_AW: &str = "specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: Aw
  activation: ALWAYS
  variables: {K: v}
";

/// An attachment defining the Service `Cache`, which declines `Aw`.
fn cache_declining_aw() -> String {
    cache_only("    dependencies: [{environment: Aw, enableIf: false}]\n")
}

/// The step 4 error of `Cache` attached at `EnvironmentTemplate[1]` for
/// declining `Aw` (`EnvironmentTemplate[0]`).
const CACHE_DECLINES_AW: &str = "EnvironmentTemplate[1] -> services[0] -> dependencies[0] -> \
     enableIf:\n\tis false, so Service 'Cache' would decline Environment 'Aw' \
     (EnvironmentTemplate[0]), whose activation is ALWAYS: no Service can leave out an ALWAYS \
     Environment (Template Schemas §3.2 item 3.2).";

/// [`self_cycle`] for `X` and `E` as a submission error.
fn submission_own_entry_cycle() -> String {
    format!("{SUBMISSION_ERR}JobTemplate:\n\t{}", own_entry_cycle())
}

/// D1-1: a Job Template Service's errors are filed after every Step's.
/// Service `X`'s `enableIf` fails to evaluate (§7.4.1 step 2), which stops
/// `X`, and Step `A` lacks `R` of the attached `BY_DEFAULT` `Q` (step 5):
/// both are reported, step 2 first, and `X` gets no constraint 7 error. Were
/// `X` filed under Step `A`'s index, `X`'s error would stop `A` instead.
#[test]
fn a_services_step_2_error_stops_the_service_and_not_the_step_filed_before_it() {
    let t = with_p(&service_job(
        "",
        "    dependencies: [{environment: Q, parameters: {R: x}, enableIf: \"int(Param.P) == 1\"}]\n",
        "",
    ));
    assert_eq!(
        create(&t, &[&q_requiring_r("")]).unwrap_err(),
        format!(
            "Model validation error: 2 validation errors for Submission\nJobTemplate -> \
             services[0] -> dependencies[0] -> enableIf:\n\tcould not be evaluated: Cannot \
             convert 'p' to int\n  int(Param.P) == 1\n  ^~~~~~~~~~~~\n{STEP_MUST_SUPPLY}"
        )
    );
}

/// D1-2: an attached Service's errors are filed after the Job Template's
/// Services'. Attached `Cache` declines the earlier attachment's `ALWAYS`
/// `Aw` (§7.4.1 step 4), and the Job Template's `X` lacks `R` of the
/// attached `BY_DEFAULT` `Q` (step 5): both are reported, step 4 first.
/// Were `Cache` filed under `X`'s index, its error would stop `X`.
#[test]
fn an_attached_services_step_4_error_does_not_stop_a_job_template_service() {
    let t = service_job("", "", ", {environment: Q, parameters: {R: s}}");
    assert_eq!(
        create(&t, &[ALWAYS_AW, &cache_declining_aw(), &q_requiring_r("")]).unwrap_err(),
        format!(
            "Model validation error: 2 validation errors for Submission\n{CACHE_DECLINES_AW}\n\
             JobTemplate -> services[0]:\n\t{}",
            SERVICE_MUST_SUPPLY.replace("EnvironmentTemplate[0]", "EnvironmentTemplate[2]")
        )
    );
}

/// D1-2 (`service_indices`): each Service's §3.3.3 conflict is filed under
/// its own index, the Job Template's `X` before the attached `Cache`, so
/// job creation reports `X`'s first although `job.services` lists `Cache`
/// first. Both enter the attached `H` (at most 4 vCPUs) and need 8. (Check
/// reports the two conflicts in `job.services` order instead.)
#[test]
fn job_creation_reports_a_job_template_services_host_conflict_before_an_attached_ones() {
    let t = requires_cache("", ", {service: X}").replacen(
        "steps:\n",
        &format!(
            "services:\n  - name: X\n{MIN_8}    ports: [{{name: main}}]\n    healthCheck: \
             {{type: STDOUT}}\n    script: {{actions: {{onRun: {{command: run}}}}}}\nsteps:\n"
        ),
        1,
    );
    let conflict = |who: &str, path: &str| {
        format!(
            "{path}:\n\t{who}: no host can satisfy the combined host requirements for amount \
             'amount.worker.vcpu': {path} -> hostRequirements requires min 8, and \
             EnvironmentTemplate[0] -> environment -> hostRequirements (Environment 'H') \
             requires max 4. Template Schemas §3.3.3."
        )
    };
    let x = conflict("Service 'X'", "JobTemplate -> services[0]");
    let cache = conflict(
        "Service 'Cache' (EnvironmentTemplate[1])",
        "EnvironmentTemplate[1] -> services[0]",
    );
    let attachments: &[&str] = &[H_MAX_4, &cache_only(MIN_8)];
    assert_eq!(
        create(&t, attachments).unwrap_err(),
        format!("Model validation error: 2 validation errors for Submission\n{x}\n{cache}")
    );
    assert_eq!(
        check(&t, attachments).unwrap_err(),
        format!("Model validation error: 2 validation errors for Submission\n{cache}\n{x}")
    );
}

/// D1-3: the combined dependency-cycle check runs only when no Step and no
/// Service is stopped. `X`'s entry for `E`, which lists it, is enabled at
/// job creation (the cycle `X -> X`), but Service `Z`'s required entry
/// names nothing (§7.4.1 step 3), so only `Z`'s error is reported. Without
/// `Z`, job creation reports the cycle.
#[test]
fn a_stopped_job_template_service_blocks_the_cycle_check() {
    let t = job(
        &listing_x(""),
        "    dependencies: [{environment: E, enableIf: \"Param.P == 'p'\"}]\n",
        "",
        "",
    );
    assert_eq!(
        create(&t, &[]).unwrap_err(),
        format!("{JOB_ERR}JobTemplate: {}", own_entry_cycle())
    );
    let t = t
        .replacen(
            "steps:\n",
            "  - name: Z\n    dependencies: [{environment: NopeZ}]\n    ports: [{name: main}]\n    \
             healthCheck: {type: STDOUT}\n    script: {actions: {onRun: {command: \
             run}}}\nsteps:\n",
            1,
        )
        .replacen(
            "  - name: B\n",
            "  - name: B\n    dependencies: [{service: Z}]\n",
            1,
        );
    decode(&t).unwrap();
    assert_eq!(
        create(&t, &[]).unwrap_err(),
        format!(
            "{JOB_ERR}services[1] -> dependencies[0] -> environment:\n\tnames Environment \
             'NopeZ', which is not a jobEnvironments entry of the Job Template, and no \
             Environment Template is attached. The entry is required; set 'required: false' if \
             the Service can run without the Environment (Template Schemas §3.2.1 item 2)."
        )
    );
}

/// A Job Template requiring the external Service `Cache`, with one Service
/// `X` (`svc_extra` indented four spaces) and `jobEnvironments` list body
/// `envs` when not empty; Step `A` lists `Cache` and `X`.
fn requires_cache_and_x(envs: &str, svc_extra: &str) -> String {
    let envs = if envs.is_empty() {
        String::new()
    } else {
        format!("jobEnvironments:\n{envs}")
    };
    format!(
        "specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
name: J
externalServices:
  - name: Cache
    ports: [{{name: main}}]
{envs}services:
  - name: X
{svc_extra}    ports: [{{name: main}}]
    healthCheck: {{type: STDOUT}}
    script: {{actions: {{onRun: {{command: run}}}}}}
steps:
  - name: A
    dependencies: [{{service: Cache}}, {{service: X}}]
    script: {{actions: {{onRun: {{command: run}}}}}}
"
    )
}

/// D1-6 (a): an attached Service a Step lists, stopped by its step 4 error
/// (declining the earlier `ALWAYS` `Aw`), and the Job Template's `X`,
/// stopped by its step 3 error (a required entry naming nothing), are
/// both reported, step 3 first; check agrees.
#[test]
fn a_stopped_attached_service_and_a_stopped_job_template_service_are_both_reported() {
    let t = requires_cache_and_x("", "    dependencies: [{environment: Nope}]\n");
    assert_eq!(
        agree(&t, &[ALWAYS_AW, &cache_declining_aw()]).unwrap(),
        format!(
            "Model validation error: 2 validation errors for Submission\nJobTemplate -> \
             services[0] -> dependencies[0] -> environment:\n\tnames Environment 'Nope', which \
             is not a jobEnvironments entry of the Job Template, and none of the attached \
             Environment Templates (EnvironmentTemplate[0], EnvironmentTemplate[1]) defines it. \
             The entry is required; set 'required: false' if the Service can run without the \
             Environment (Template Schemas §3.2.1 item 2).\n{CACHE_DECLINES_AW}"
        )
    );
}

/// D1-6 (b): a stopped attached Service blocks job creation's cycle check:
/// `X`'s Sessions enter `E`, which lists `X` (an `exclusiveGroup` member, so
/// left to job creation), and `Cache`'s step 4 error is reported alone.
/// Check, which reports every violation it is certain of, reports the
/// cycle too; without `Cache`'s error, both report the cycle.
#[test]
fn a_stopped_attached_service_blocks_the_cycle_check_at_job_creation() {
    let e = "  - name: E\n    exclusiveGroup: g\n    dependencies: [{service: X}]\n    variables: {K: \"{{Service.X.main.port}}\"}\n";
    let t = requires_cache_and_x(e, "");
    decode(&t).unwrap();
    let attachments: &[&str] = &[ALWAYS_AW, &cache_declining_aw()];
    assert_eq!(
        create(&t, attachments).unwrap_err(),
        format!("{SUBMISSION_ERR}{CACHE_DECLINES_AW}")
    );
    assert_eq!(
        check(&t, attachments).unwrap_err(),
        format!(
            "Model validation error: 2 validation errors for Submission\n{CACHE_DECLINES_AW}\n\
             JobTemplate:\n\t{}",
            own_entry_cycle()
        )
    );
    assert_eq!(
        agree(&t, &[ALWAYS_AW, &cache_only("")]).unwrap(),
        submission_own_entry_cycle()
    );
}

// ════════════════════════════════════════════════════════════════════
// Conflict findings: warnings, wrapping Step Environments, exclusive
// groups
// ════════════════════════════════════════════════════════════════════

/// §3.2.1 item 1: a Service's `required: false` entry naming an attachment
/// that declares no parameters has its `parameters` ignored, with a
/// warning saying the Service's Sessions get no values from it, from job
/// creation and from check alike.
#[test]
fn a_services_ignored_parameters_warning_names_the_services_sessions() {
    let t = service_job(
        "",
        "    dependencies: [{environment: Aw, required: false, parameters: {Z: z}}]\n",
        "",
    );
    let expected = "JobTemplate -> services[0] -> dependencies[0] -> parameters: Environment \
         'Aw' (EnvironmentTemplate[0]) declares no parameters, and this entry has 'required: \
         false', so its parameters are ignored: Z. The Service's Sessions get no values from it \
         (Template Schemas §3.2.1 item 1).";
    let created = create_details_with(&t, &[ALWAYS_AW]);
    let warnings: Vec<String> = created.warnings.iter().map(ToString::to_string).collect();
    assert_eq!(warnings, [expected]);
    let jt = decode(&t).unwrap();
    let et = decode_environment_template(yaml_val(ALWAYS_AW), Some(EXTS), &CallerLimits::default())
        .unwrap();
    let attached: Vec<AttachedEnvironmentTemplate<'_>> = vec![(&et).into()];
    let warnings: Vec<String> =
        check_environment_templates(&jt, &attached, &jt.default_validation_context())
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect();
    assert_eq!(warnings, [expected]);
}

/// Wrap hook rule 1 for a wrapping Step Environment in a `SERVICE` Job:
/// template validation checks only one whose `enableIf` is known to be
/// `true`, so one whose `enableIf` expression is true at job creation is
/// checked there, and lacking `onWrapTaskRun` is reported.
#[test]
fn a_wrapping_step_environment_enabled_at_job_creation_is_checked_for_wrap_hook_rule_1() {
    let t = "specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR, WRAP_ACTIONS]
name: J
parameterDefinitions:
  - {name: P, type: STRING, default: p}
services:
  - name: X
    ports: [{name: main}]
    healthCheck: {type: STDOUT}
    script: {actions: {onRun: {command: run}}}
steps:
  - name: A
    dependencies: [{service: X}]
    stepEnvironments:
      - name: W
        enableIf: \"Param.P == 'p'\"
        script:
          actions:
            onWrapEnvEnter: {command: c}
            onWrapEnvExit: {command: c}
    script: {actions: {onRun: {command: run}}}
";
    decode(t).unwrap();
    assert_eq!(
        create(t, &[]).unwrap_err(),
        format!(
            "{JOB_ERR}steps[0] -> stepEnvironments[0] -> script -> actions:\n\tthe Sessions of \
             Step 'A' enter wrapping Environment 'W', defined by the Job Template, which \
             therefore must define onWrapEnvEnter, onWrapTaskRun, and onWrapEnvExit; missing: \
             onWrapTaskRun (RFC 0009 wrap hook rule 1)."
        )
    );
}

/// `<StepDependency>` constraint 3 with an `exclusiveGroup`: a grouped Job
/// Environment `E` that lists `X`, which does not decline it, is not known
/// to be entered at template validation (an attachment's group member may
/// supersede it), so the Job Template is accepted. With an attachment
/// outside the group, nothing supersedes `E`, and check and job creation
/// both report the cycle `X -> X`.
#[test]
fn a_grouped_environment_listing_its_service_is_a_cycle_decided_at_submission() {
    let t = job(
        "  - name: E\n    exclusiveGroup: g\n    dependencies: [{service: X}]\n    variables: {K: \"{{Service.X.main.port}}\"}\n",
        "",
        "",
        "",
    );
    decode(&t).unwrap();
    assert_eq!(agree(&t, &[H_MAX_4]).unwrap(), submission_own_entry_cycle());
}

// ════════════════════════════════════════════════════════════════════
// The wrapping-Environment rules at job creation while another Step or
// Service is stopped
// ════════════════════════════════════════════════════════════════════

/// Two `ON_REQUEST` seven-hook wrappers `W1` and `W2` that `X` requests
/// through entries whose `enableIf` is an expression, so only job creation
/// knows `X`'s Sessions enter both; Step `A` lists `X` and then
/// `step_deps`, and Step `B` has `b_extra`.
fn x_requests_two_wrappers(step_deps: &str, b_extra: &str) -> String {
    let envs = format!(
        "  - name: W1\n    activation: ON_REQUEST\n{SEVEN_HOOKS}  - name: W2\n    activation: ON_REQUEST\n{SEVEN_HOOKS}"
    );
    job(
        &envs,
        "    dependencies:\n      - {environment: W1, enableIf: \"Param.P == 'p'\"}\n      - {environment: W2, enableIf: \"Param.P == 'p'\"}\n",
        step_deps,
        b_extra,
    )
}

/// `X`'s single-layer error for [`x_requests_two_wrappers`].
const X_TWO_LAYERS: &str = "services[0]:\n\tService 'X': its Sessions would enter 2 Environments \
     that define wrap hooks (onWrapEnvEnter, onWrapEnvExit, onWrapService*): 'W1', 'W2'. A \
     Session may have at most one Environment defining wrap hooks (RFC 0008).";

/// The RFC 0008 single-layer rule for a Job Template Service's Sessions,
/// decided only at job creation, is checked there also when another
/// entity is stopped by a §7.4.1 checks 1–4 error: a Step's step 3 error
/// (a required entry naming nothing), or an attached Service's step 4
/// error (declining an earlier attachment's `ALWAYS` Environment). Both
/// errors are reported.
#[test]
fn a_services_single_layer_error_is_kept_when_another_entity_is_stopped() {
    let t = x_requests_two_wrappers("", "");
    decode(&t).unwrap();
    assert_eq!(
        create(&t, &[]).unwrap_err(),
        format!("{JOB_ERR}{X_TWO_LAYERS}")
    );
    // Step B is stopped by its step 3 error.
    let t = x_requests_two_wrappers("", "    dependencies: [{environment: Nope}]\n");
    assert_eq!(
        create(&t, &[]).unwrap_err(),
        format!(
            "Model validation error: 2 validation errors for JobTemplate\nsteps[1] -> \
             dependencies[0] -> environment:\n\tnames Environment 'Nope', which is not a \
             jobEnvironments entry of the Job Template, and no Environment Template is attached. \
             The entry is required; set 'required: false' if the Step can run without the \
             Environment (Template Schemas §3.2.1 item 2).\n{X_TWO_LAYERS}"
        )
    );
    // The attached Service `Cache`, which Step A lists, is stopped by its
    // step 4 error.
    let t = x_requests_two_wrappers(", {service: Cache}", "").replace(
        "jobEnvironments:\n",
        "externalServices:\n  - name: Cache\n    ports: [{name: main}]\njobEnvironments:\n",
    );
    assert_eq!(
        create(&t, &[ALWAYS_AW, &cache_declining_aw()]).unwrap_err(),
        format!(
            "Model validation error: 2 validation errors for Submission\n{CACHE_DECLINES_AW}\n\
             JobTemplate -> {X_TWO_LAYERS}"
        )
    );
}

/// RFC 0009 wrap hook rule 1 for a Step's Task Sessions, decided only at
/// job creation, is checked there also when a Job Template Service is
/// stopped by its step 3 error: Step `A` requests the wrapper `W`, which
/// lacks `onWrapTaskRun`, through an entry whose `enableIf` is an
/// expression, and lists the stopped `Z`. Both errors are reported.
#[test]
fn a_steps_wrap_hook_rule_1_error_is_kept_when_a_service_is_stopped() {
    let t = "specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR, WRAP_ACTIONS]
name: J
parameterDefinitions:
  - {name: P, type: STRING, default: p}
jobEnvironments:
  - name: W
    activation: ON_REQUEST
    script:
      actions:
        onWrapEnvEnter: {command: c}
        onWrapEnvExit: {command: c}
services:
  - name: Z
    dependencies: [{environment: NopeZ}]
    ports: [{name: main}]
    healthCheck: {type: STDOUT}
    script: {actions: {onRun: {command: run}}}
steps:
  - name: A
    dependencies: [{service: Z}, {environment: W, enableIf: \"Param.P == 'p'\"}]
    script: {actions: {onRun: {command: run}}}
";
    decode(t).unwrap();
    assert_eq!(
        create(t, &[]).unwrap_err(),
        "Model validation error: 2 validation errors for JobTemplate\nservices[0] -> \
         dependencies[0] -> environment:\n\tnames Environment 'NopeZ', which is not a \
         jobEnvironments entry of the Job Template, and no Environment Template is attached. The \
         entry is required; set 'required: false' if the Service can run without the Environment \
         (Template Schemas §3.2.1 item 2).\njobEnvironments[0] -> script -> actions:\n\tthe \
         Sessions of Step 'A' enter wrapping Environment 'W', defined by the Job Template, which \
         therefore must define onWrapEnvEnter, onWrapTaskRun, and onWrapEnvExit; missing: \
         onWrapTaskRun (RFC 0009 wrap hook rule 1)."
    );
}

/// A Job Template Service's job creation errors are filed after every
/// Step's, within the same §7.4.1 step: the combined host requirement
/// conflicts with the `BY_DEFAULT` `H` of Step `B` (`steps[1]`) and of
/// Service `X` (`services[0]`) are reported `B` first, though `X` is the
/// first Service and `B` the second Step.
#[test]
fn a_services_host_requirement_conflict_is_filed_after_every_steps() {
    let t = "specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
name: J
jobEnvironments:
  - name: H
    hostRequirements: {amounts: [{name: amount.worker.vcpu, min: 4}]}
    variables: {K: v}
services:
  - name: X
    hostRequirements: {amounts: [{name: amount.worker.vcpu, max: 2}]}
    ports: [{name: main}]
    healthCheck: {type: STDOUT}
    script: {actions: {onRun: {command: run}}}
steps:
  - name: A
    dependencies: [{service: X}]
    script: {actions: {onRun: {command: run}}}
  - name: B
    hostRequirements: {amounts: [{name: amount.worker.vcpu, max: 1}]}
    script: {actions: {onRun: {command: run}}}
";
    decode(t).unwrap();
    assert_eq!(
        create(t, &[]).unwrap_err(),
        "Model validation error: 2 validation errors for JobTemplate\nsteps[1]:\n\tStep 'B': no \
         host can satisfy the combined host requirements for amount 'amount.worker.vcpu': \
         jobEnvironments[0] -> hostRequirements (Environment 'H') requires min 4, and steps[1] \
         -> hostRequirements requires max 1. Template Schemas §3.3.3.\nservices[0]:\n\tService \
         'X': no host can satisfy the combined host requirements for amount \
         'amount.worker.vcpu': jobEnvironments[0] -> hostRequirements (Environment 'H') requires \
         min 4, and services[0] -> hostRequirements requires max 2. Template Schemas §3.3.3."
    );
}

/// Wrap hook rule 1 for a wrapping Step Environment `W` enabled at job
/// creation is reported at `W`'s own place in `stepEnvironments`, not at
/// an earlier non-wrapping Step Environment whose `enableIf` is also an
/// expression.
#[test]
fn a_wrapping_step_environment_after_another_enabled_at_job_creation_is_reported_at_its_place() {
    let t = "specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR, WRAP_ACTIONS]
name: J
parameterDefinitions:
  - {name: P, type: STRING, default: p}
services:
  - name: X
    ports: [{name: main}]
    healthCheck: {type: STDOUT}
    script: {actions: {onRun: {command: run}}}
steps:
  - name: A
    dependencies: [{service: X}]
    stepEnvironments:
      - name: Plain
        enableIf: \"Param.P == 'p'\"
        variables: {K: v}
      - name: W
        enableIf: \"Param.P == 'p'\"
        script:
          actions:
            onWrapEnvEnter: {command: c}
            onWrapEnvExit: {command: c}
    script: {actions: {onRun: {command: run}}}
";
    decode(t).unwrap();
    assert_eq!(
        create(t, &[]).unwrap_err(),
        format!(
            "{JOB_ERR}steps[0] -> stepEnvironments[1] -> script -> actions:\n\tthe Sessions of \
             Step 'A' enter wrapping Environment 'W', defined by the Job Template, which \
             therefore must define onWrapEnvEnter, onWrapTaskRun, and onWrapEnvExit; missing: \
             onWrapTaskRun (RFC 0009 wrap hook rule 1)."
        )
    );
}

// ════════════════════════════════════════════════════════════════════
// The wrapping-Environment rules and the cycle check at job creation
// ════════════════════════════════════════════════════════════════════

/// The script of a wrapper defining only `onWrapEnvEnter` and
/// `onWrapEnvExit` (indented four spaces, as for a `jobEnvironments`
/// entry).
const TWO_HOOKS: &str = "    script:
      actions:
        onWrapEnvEnter: {command: c}
        onWrapEnvExit: {command: c}
";

/// An `ON_REQUEST` Job Environment `name` with `hooks`.
fn on_request_wrapper(name: &str, hooks: &str) -> String {
    format!("  - name: {name}\n    activation: ON_REQUEST\n{hooks}")
}

/// The wrap-hook rules are checked at job creation also when the cycle
/// check finds a cycle: Service `X` requests the `ON_REQUEST` Job
/// Environment `E`, which lists `X` (the cycle `X -> X`), and the
/// seven-hook wrappers `W1` and `W2`, and Step `A` requests the two-hook
/// wrapper `W3`, each through an entry whose `enableIf` is an expression.
/// `X`'s single-layer error is filed under `X`, then the cycle, then
/// `W3`'s wrap hook rule 1 error.
#[test]
fn the_wrap_hook_rules_are_checked_at_job_creation_when_a_cycle_is_found() {
    let envs = format!(
        "{}{}{}{}",
        listing_x("    activation: ON_REQUEST\n"),
        on_request_wrapper("W1", SEVEN_HOOKS),
        on_request_wrapper("W2", SEVEN_HOOKS),
        on_request_wrapper("W3", TWO_HOOKS),
    );
    let t = job(
        &envs,
        "    dependencies:\n      - {environment: E, enableIf: \"Param.P == 'p'\"}\n      - {environment: W1, enableIf: \"Param.P == 'p'\"}\n      - {environment: W2, enableIf: \"Param.P == 'p'\"}\n",
        ", {environment: W3, enableIf: \"Param.P == 'p'\"}",
        "",
    );
    decode(&t).unwrap();
    assert_eq!(
        create(&t, &[]).unwrap_err(),
        format!(
            "Model validation error: 3 validation errors for JobTemplate\n{X_TWO_LAYERS}\n\
             JobTemplate: {}\njobEnvironments[3] -> script -> actions:\n\tthe Sessions of Step \
             'A' enter wrapping Environment 'W3', defined by the Job Template, which therefore \
             must define onWrapEnvEnter, onWrapTaskRun, and onWrapEnvExit; missing: \
             onWrapTaskRun (RFC 0009 wrap hook rule 1).",
            own_entry_cycle()
        )
    );
}

/// Within §7.4.1 step 7 at job creation, each entity's own single-layer
/// error is filed under the entity, Steps first, then the Job Template's
/// Services, and wrap hook rule 1 errors after them: Step `A` and Service
/// `X` both enter the attached `BY_DEFAULT` wrappers `W1` (seven hooks)
/// and `W2` (RFC 0008's three hooks), which lacks the Service-Session hooks
/// `X` needs.
#[test]
fn job_creation_files_single_layer_errors_by_entity_before_wrap_hook_rule_1_errors() {
    let w1 = attached_wrapper("W1", "", SEVEN_HOOKS);
    let w2 = attached_wrapper("W2", "", THREE_HOOK_WRAPPER);
    let layers = |hooks: &str| {
        format!(
            "2 Environments that define wrap hooks ({hooks}): 'W1' (EnvironmentTemplate[0]), \
             'W2' (EnvironmentTemplate[1]). A Session may have at most one Environment defining \
             wrap hooks (RFC 0008)."
        )
    };
    assert_eq!(
        create(&service_job("", "", ""), &[&w1, &w2]).unwrap_err(),
        format!(
            "Model validation error: 3 validation errors for Submission\nJobTemplate -> \
             steps[0]:\n\tits Sessions would enter {}\nJobTemplate -> services[0]:\n\tService \
             'X': its Sessions would enter {}\nEnvironmentTemplate[1] -> environment -> script \
             -> actions:\n\tthe Sessions of Service 'X' enter wrapping Environment 'W2', defined \
             by EnvironmentTemplate[1], {SERVICE_HOOKS_MISSING}",
            layers("onWrapEnvEnter, onWrapTaskRun, onWrapEnvExit"),
            layers("onWrapEnvEnter, onWrapEnvExit, onWrapService*"),
        )
    );
}

/// A Job Template whose Step `A` lists `X` and then `a_tail` (a flow-list
/// tail), with the two-hook Step Environment `W`, whose `enableIf` is an
/// expression.
fn step_with_two_hook_w(a_tail: &str) -> String {
    let hooks: String = TWO_HOOKS.lines().map(|l| format!("    {l}\n")).collect();
    format!(
        "specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR, WRAP_ACTIONS]
name: J
parameterDefinitions:
  - {{name: P, type: STRING, default: p}}
services:
  - name: X
    ports: [{{name: main}}]
    healthCheck: {{type: STDOUT}}
    script: {{actions: {{onRun: {{command: run}}}}}}
steps:
  - name: A
    dependencies: [{{service: X}}{a_tail}]
    stepEnvironments:
      - name: W
        enableIf: \"Param.P == 'p'\"
{hooks}    script: {{actions: {{onRun: {{command: run}}}}}}
"
    )
}

/// A Step stopped by a §7.4.1 checks 1–4 error gets no wrap hook rule 1
/// error for its Step Environments at job creation (steps 5–8 are not
/// taken for it): with a required entry naming nothing, only that error is
/// reported; without it, the enabled two-hook `W` lacks `onWrapTaskRun`.
#[test]
fn a_stopped_steps_step_environment_is_not_checked_for_wrap_hook_rule_1() {
    let t = step_with_two_hook_w(", {environment: Nope}");
    decode(&t).unwrap();
    assert_eq!(
        create(&t, &[]).unwrap_err(),
        format!(
            "{JOB_ERR}steps[0] -> dependencies[1] -> environment:\n\tnames Environment 'Nope', \
             which is not a jobEnvironments entry of the Job Template, and no Environment \
             Template is attached. The entry is required; set 'required: false' if the Step can \
             run without the Environment (Template Schemas §3.2.1 item 2)."
        )
    );
    assert_eq!(
        create(&step_with_two_hook_w(""), &[]).unwrap_err(),
        format!(
            "{JOB_ERR}steps[0] -> stepEnvironments[0] -> script -> actions:\n\tthe Sessions of \
             Step 'A' enter wrapping Environment 'W', defined by the Job Template, which \
             therefore must define onWrapEnvEnter, onWrapTaskRun, and onWrapEnvExit; missing: \
             onWrapTaskRun (RFC 0009 wrap hook rule 1)."
        )
    );
}

/// Job creation's cycle check follows a Step's `step:` entries and a Job
/// Template Service's `step:` entries: Step `A` requests the `ON_REQUEST`
/// `Ej`, which lists `X`, through an entry whose `enableIf` is an
/// expression, so template validation and check cannot see the edge
/// `A -> X`; `X` lists Step `B`, and `B` lists Step `A`.
#[test]
fn job_creation_finds_a_cycle_through_step_entries_of_a_step_and_a_service() {
    let t = "specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
name: J
parameterDefinitions:
  - {name: P, type: STRING, default: p}
jobEnvironments:
  - name: Ej
    activation: ON_REQUEST
    dependencies: [{service: X}]
    variables: {K: \"{{Service.X.main.port}}\"}
services:
  - name: X
    dependencies: [{step: B}]
    ports: [{name: main}]
    script: {actions: {onRun: {command: run}}}
steps:
  - name: A
    dependencies: [{environment: Ej, enableIf: \"Param.P == 'p'\"}]
    script: {actions: {onRun: {command: run}}}
  - name: B
    dependencies: [{step: A}]
    script: {actions: {onRun: {command: run}}}
";
    decode(t).unwrap();
    check(t, &[]).unwrap();
    assert_eq!(
        create(t, &[]).unwrap_err(),
        format!(
            "{JOB_ERR}JobTemplate: dependencies contain a cycle: Step A -> Service X -> Step B -> \
             Step A (the Sessions of Step A enter Job Environment 'Ej', which lists Service 'X', \
             so Step A depends on it; Template Schemas §3.2 constraint 3)."
        )
    );
}

/// A Job Template with the `BY_DEFAULT` Job Environment `Ej` in
/// `exclusiveGroup` `g`, which lists `X`, the Services `services` and the
/// Steps `steps`, each a name and a `dependencies` flow-list body (none
/// when empty).
fn grouped_ej(services: &[(&str, &str)], steps: &[(&str, &str)]) -> String {
    let deps = |d: &str| {
        if d.is_empty() {
            String::new()
        } else {
            format!("    dependencies: [{d}]\n")
        }
    };
    let services: String = services
        .iter()
        .map(|(name, d)| {
            format!(
                "  - name: {name}\n{}    ports: [{{name: main}}]\n    script: {{actions: {{onRun: \
                 {{command: run}}}}}}\n",
                deps(d)
            )
        })
        .collect();
    let steps: String = steps
        .iter()
        .map(|(name, d)| {
            format!(
                "  - name: {name}\n{}    script: {{actions: {{onRun: {{command: run}}}}}}\n",
                deps(d)
            )
        })
        .collect();
    format!(
        "specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
name: J
jobEnvironments:
  - name: Ej
    exclusiveGroup: g
    dependencies: [{{service: X}}]
    variables: {{K: \"{{{{Service.X.main.port}}}}\"}}
services:
{services}steps:
{steps}"
    )
}

/// A disabled entry declining `Ej`.
const NO_EJ: &str = "{environment: Ej, enableIf: false}";

/// The cycle error for `path`, closed by `who` entering `Ej`.
fn ej_cycle(path: &str, who: &str) -> String {
    format!(
        "{SUBMISSION_ERR}JobTemplate:\n\tdependencies contain a cycle: {path} (the Sessions of \
         {who} enter Job Environment 'Ej', which lists Service 'X', so {who} depends on it; \
         Template Schemas §3.2 constraint 3)."
    )
}

/// `<StepDependency>` constraint 3 before submission: check finds the
/// cycles through the grouped `Ej`, which nothing supersedes once an
/// attachment outside the group is attached, along each kind of certain
/// edge, as job creation does. The others decline `Ej`, so each cycle has
/// one edge from entering it and its other edges of the kind under test:
/// a Step's `step:` entry and a Job Template Service's `step:` entry; a
/// Step's `service:` entry; and a Service's `service:` entry (naming a
/// Service declared after it).
#[test]
fn check_finds_a_cycle_through_each_kind_of_certain_edge() {
    let cases = [
        // A enters Ej; X lists Step B; B lists Step A.
        (
            grouped_ej(
                &[("X", &format!("{{step: B}}, {NO_EJ}"))],
                &[("A", ""), ("B", &format!("{{step: A}}, {NO_EJ}"))],
            ),
            ej_cycle("Step A -> Service X -> Step B -> Step A", "Step A"),
        ),
        // A lists Z; Z enters Ej; X lists Step A.
        (
            grouped_ej(
                &[("X", &format!("{{step: A}}, {NO_EJ}")), ("Z", "")],
                &[("A", &format!("{{service: Z}}, {NO_EJ}"))],
            ),
            ej_cycle("Step A -> Service Z -> Service X -> Step A", "Service Z"),
        ),
        // X lists Z; Z enters Ej.
        (
            grouped_ej(
                &[("X", &format!("{{service: Z}}, {NO_EJ}")), ("Z", "")],
                &[("A", &format!("{{service: X}}, {NO_EJ}"))],
            ),
            ej_cycle("Service X -> Service Z -> Service X", "Service Z"),
        ),
    ];
    for (t, expected) in cases {
        decode(&t).unwrap();
        assert_eq!(agree(&t, &[H_MAX_4]).unwrap(), expected);
    }
}

/// A Job Template whose `BY_DEFAULT` Job Environment `Ej`, in
/// `exclusiveGroup` `g`, lists Service `X`; `X` lists Step `B` and declines
/// `Ej`; Step `A` enters `Ej`; Step `B` declines `Ej`, requires at least 8
/// vCPUs, and lists Step `A` when `b_lists_a`, which closes the cycle
/// `A -> X -> B -> A`.
fn grouped_ej_with_b_min_8(b_lists_a: bool) -> String {
    let b_deps = if b_lists_a { "{step: A}, " } else { "" };
    format!(
        "specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
name: J
jobEnvironments:
  - name: Ej
    exclusiveGroup: g
    dependencies: [{{service: X}}]
    variables: {{K: \"{{{{Service.X.main.port}}}}\"}}
services:
  - name: X
    dependencies: [{{step: B}}, {NO_EJ}]
    ports: [{{name: main}}]
    script: {{actions: {{onRun: {{command: run}}}}}}
steps:
  - name: A
    script: {{actions: {{onRun: {{command: run}}}}}}
  - name: B
    dependencies: [{b_deps}{NO_EJ}]
{MIN_8}    script: {{actions: {{onRun: {{command: run}}}}}}
"
    )
}

/// `<StepDependency>` constraint 3 before submission: once check finds a
/// certain cycle it reports it with the errors found so far and makes none
/// of the checks after it. Step `B`'s host requirement conflicts with the
/// attached `H`, and check reports that conflict when there is no cycle;
/// with the cycle through the grouped `Ej`, check reports the cycle alone.
#[test]
fn check_environment_templates_stops_after_a_certain_cycle() {
    let b_conflict = format!(
        "{SUBMISSION_ERR}JobTemplate -> steps[1]:\n\tStep 'B': no host can satisfy the combined \
         host requirements for amount 'amount.worker.vcpu': JobTemplate -> steps[1] -> \
         hostRequirements requires min 8, and EnvironmentTemplate[0] -> environment -> \
         hostRequirements (Environment 'H') requires max 4. Template Schemas §3.3.3."
    );
    assert_eq!(
        check(&grouped_ej_with_b_min_8(false), &[H_MAX_4]).unwrap_err(),
        b_conflict
    );
    assert_eq!(
        check(&grouped_ej_with_b_min_8(true), &[H_MAX_4]).unwrap_err(),
        ej_cycle("Step A -> Service X -> Step B -> Step A", "Step A")
    );
}

/// `<StepDependency>` constraint 3 before submission: a Step's `service:`
/// entry naming an `externalServices` requirement is an edge to the
/// attached Service the requirement binds, as at job creation. Step `A`
/// lists the requirement `S`, bound to the attachment's `S`, and enters
/// the grouped `Ej`, which lists `X`, which lists Step `B`, which lists
/// `A`; the attached `S` enters its document's grouped `BY_DEFAULT`
/// `Client`, which lists it (`A` declines `Client`). Job creation reports
/// the first cycle its graph walk reaches from `A`, `S -> S` through the
/// edge `A -> S`, and check names the same one.
#[test]
fn check_names_the_cycle_job_creation_does_through_a_bound_requirement() {
    let t = format!(
        "specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
name: J
externalServices:
  - name: S
    ports: [{{name: main}}]
jobEnvironments:
  - name: Ej
    exclusiveGroup: g
    dependencies: [{{service: X}}]
    variables: {{K: \"{{{{Service.X.main.port}}}}\"}}
services:
  - name: X
    dependencies: [{{step: B}}, {NO_EJ}]
    ports: [{{name: main}}]
    script: {{actions: {{onRun: {{command: run}}}}}}
steps:
  - name: A
    dependencies: [{{service: S}}, {{environment: Client, enableIf: false}}]
    script: {{actions: {{onRun: {{command: run}}}}}}
  - name: B
    dependencies: [{{step: A}}, {NO_EJ}]
    script: {{actions: {{onRun: {{command: run}}}}}}
"
    );
    let client = "specificationVersion: environment-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
services:
  - name: S
    ports: [{name: main}]
    script: {actions: {onRun: {command: run}}}
environment:
  name: Client
  exclusiveGroup: c
  dependencies: [{service: S}]
  variables: {K: \"{{Service.S.main.port}}\"}
";
    decode(&t).unwrap();
    assert_eq!(
        agree(&t, &[client]).unwrap(),
        format!(
            "{SUBMISSION_ERR}EnvironmentTemplate[0] -> services:\n\tdependencies contain a \
             cycle: Service S (EnvironmentTemplate[0]) -> Service S (EnvironmentTemplate[0]) \
             (the Sessions of Service S (EnvironmentTemplate[0]) enter Job Environment 'Client' \
             (EnvironmentTemplate[0]), which lists Service 'S', so it would wait for itself to \
             be READY. There is no exception for an Environment that lists the Service: keep \
             the Service's Sessions out of it: drop any 'environment:' dependency of the Service \
             that requests it, and decline a BY_DEFAULT one with a disabled 'environment:' \
             dependency of the Service, or make it ON_REQUEST and request it only from the \
             Steps that need it; Template Schemas §3.2 constraint 3)."
        )
    );
}

/// A Job Template listing `SERVICE`, with the Job Environment `E` and a
/// Service `X` that Step `A` lists.
const SERVICE_JOB_WITH_E: &str = "specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, ENV_PARAMS, EXPR]
name: J
jobEnvironments:
  - {name: E, variables: {V: x}}
services:
  - name: X
    ports: [{name: main}]
    script: {actions: {onRun: {command: run}}}
steps:
  - name: A
    dependencies: [{service: X}]
    script: {actions: {onRun: {command: run}}}
";

/// An attached `BY_DEFAULT` Environment `E` (`extra` indented two spaces)
/// with a STRING parameter `Need` without a default.
fn by_default_e_needing(extra: &str) -> String {
    format!(
        "specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
{extra}  envParameterDefinitions: [{{name: Need, type: STRING}}]
  variables: {{V: x}}
"
    )
}

/// Template Schemas §3.2.1 item 4 under `SERVICE`: an attachment named like
/// a Job Environment cannot be reached by any `environment:` entry, so a
/// `BY_DEFAULT` one with a parameter without a default rejects the
/// submission. When the Job Template lists `SERVICE` its Services have
/// entries too, so the message says no Step or Service can supply the
/// parameter and, with an `enableIf` or `exclusiveGroup` on the
/// attachment, that it applies to every Step and Service (§3.2 constraint
/// 7). Check and job creation agree.
#[test]
fn an_unreachable_attachment_names_steps_and_services_when_the_job_template_lists_service() {
    let unreachable = |applies: &str| {
        format!(
            "{SUBMISSION_ERR}EnvironmentTemplate[0] -> environment -> envParameterDefinitions:\
             \n\tEnvironment 'E' has the same name as a jobEnvironments entry of the Job \
             Template, so no 'environment:' dependency can reach it (Template Schemas §3.2.1 \
             item 4), and it has parameters without a default that no Step or Service can \
             supply: Need. Its activation is BY_DEFAULT, so {applies}, and the submission is \
             rejected (§3.2.1)."
        )
    };
    assert_eq!(
        agree(SERVICE_JOB_WITH_E, &[&by_default_e_needing("")]).unwrap(),
        unreachable("every Session enters it")
    );
    let applies_to_all = "it applies to every Step and Service whatever its enableIf and \
                          exclusiveGroup (§3.2 constraint 7)";
    assert_eq!(
        agree(
            SERVICE_JOB_WITH_E,
            &[&by_default_e_needing("  exclusiveGroup: g\n")]
        )
        .unwrap(),
        unreachable(applies_to_all)
    );
    assert_eq!(
        agree(
            SERVICE_JOB_WITH_E,
            &[&by_default_e_needing("  enableIf: \"true\"\n")]
        )
        .unwrap(),
        unreachable(applies_to_all)
    );
}

/// Template Schemas §3.2.1 item 3 under `SERVICE`: an enabled entry naming
/// an Environment that two attachments define is rejected, so a
/// `BY_DEFAULT` one with a parameter without a default that some Step or
/// Service does not decline rejects the submission, once per attachment. When the Job Template lists `SERVICE` its Services have
/// entries too, so the message says no Step or Service can supply the
/// parameter. Check and job creation agree.
#[test]
fn an_ambiguous_attachment_names_steps_and_services_when_the_job_template_lists_service() {
    let t = SERVICE_JOB_WITH_E.replace("{name: E, variables", "{name: F, variables");
    let e = by_default_e_needing("");
    let ambiguous = |doc: &str| {
        format!(
            "{doc} -> environment -> envParameterDefinitions:\n\tEnvironment 'E' is defined by \
             2 attached Environment Templates (EnvironmentTemplate[0], EnvironmentTemplate[1]), \
             so no enabled 'environment:' dependency can reach it (Template Schemas §3.2.1 item \
             3), and it has parameters without a default that no Step or Service can supply: \
             Need. Its activation is BY_DEFAULT, so it applies to every Step and Service that \
             does not decline it with a disabled entry, and at least one Step or Service does \
             not decline it, and the submission is rejected (§3.2.1)."
        )
    };
    assert_eq!(
        agree(&t, &[&e, &e]).unwrap(),
        format!(
            "Model validation error: 2 validation errors for Submission\n{}\n{}",
            ambiguous("EnvironmentTemplate[0]"),
            ambiguous("EnvironmentTemplate[1]")
        )
    );
}

/// A Job Template Service with a job-creation error in one of its own
/// fields (here a `restartPolicy.maxRestarts` format string that resolves
/// below 0) is stopped: job creation reports that error alone, and not the
/// host conflict between the Service's `hostRequirements` and those of the
/// `BY_DEFAULT` Job Environment its Sessions enter, which it reports when
/// the field resolves.
#[test]
fn a_job_template_services_field_error_stops_the_service() {
    let t = service_job(
        "  - name: E\n    hostRequirements: {amounts: [{name: amount.worker.vcpu, min: 8}]}\n    \
         variables: {V: x}\n",
        "    restartPolicy: {maxRestarts: \"{{Param.Num - 1}}\"}\n    hostRequirements: \
         {amounts: [{name: amount.worker.vcpu, max: 4}]}\n",
        "",
    )
    .replace(
        "name: J\n",
        "name: J\nparameterDefinitions: [{name: Num, type: INT, default: 0}]\n",
    );
    assert_eq!(
        create(&t, &[]).map(|_| ()).unwrap_err(),
        format!("{JOB_ERR}services[0] -> restartPolicy -> maxRestarts:\n\tmust be >= 0.")
    );
    // With a value that resolves to 0, the Service is not stopped and its
    // host conflict is reported.
    assert_eq!(
        create(&t.replace("Param.Num - 1", "Param.Num"), &[])
            .map(|_| ())
            .unwrap_err(),
        format!(
            "{JOB_ERR}services[0]:\n\tService 'X': no host can satisfy the combined host \
             requirements for amount 'amount.worker.vcpu': jobEnvironments[0] -> \
             hostRequirements (Environment 'E') requires min 8, and services[0] -> \
             hostRequirements requires max 4. Template Schemas §3.3.3."
        )
    );
}

/// An attached `BY_DEFAULT` Environment `E` requiring at least 8 vCPUs,
/// whose INT parameter `Need` has `maxValue: 5` and the literal `default`
/// 9, set on the decoded template because template validation rejects it
/// as written.
fn e_with_failing_literal_default() -> openjd_model::template::EnvironmentTemplate {
    let dec = |def: &str| {
        decode_environment_template(
            yaml_val(&format!(
                "specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: E
  envParameterDefinitions: [{{name: Need, type: INT, {def}}}]
  hostRequirements: {{amounts: [{{name: amount.worker.vcpu, min: 8}}]}}
  variables: {{K: \"{{{{Env.Param.Need}}}}\"}}
"
            )),
            Some(EXTS),
            &CallerLimits::default(),
        )
        .unwrap()
    };
    let mut e = dec("maxValue: 5, default: 5");
    e.environment
        .as_mut()
        .unwrap()
        .env_parameter_definitions
        .as_mut()
        .unwrap()[0]
        .default = dec("default: 9")
        .environment
        .unwrap()
        .env_parameter_definitions
        .unwrap()[0]
        .default
        .clone();
    e
}

/// A Service that falls back to an attachment's literal `default` that
/// fails its definition gets no values of that Environment at job
/// creation, which skips the Environment's host requirements for it; check
/// leaves the Environment out of the Service's static combination too, so
/// both report only the `default` error and no host conflict.
#[test]
fn check_leaves_out_an_environment_a_service_falls_back_to_a_failing_literal_default_for() {
    let jt = decode(&service_job(
        "",
        "    hostRequirements: {amounts: [{name: amount.worker.vcpu, max: 4}]}\n",
        "",
    ))
    .unwrap();
    let e = e_with_failing_literal_default();
    let attached = [AttachedEnvironmentTemplate::from(&e)];
    let checked = check_environment_templates(&jt, &attached, &jt.default_validation_context())
        .map(|_| ())
        .map_err(|e| e.to_string());
    let td = tempfile::TempDir::new().unwrap();
    let dir = td.path().to_str().unwrap();
    let processed = preprocess_job_parameters(
        &jt,
        &JobParameterInputValues::default(),
        std::slice::from_ref(&e),
        &PathParameterOptions::new(dir, dir),
    )
    .unwrap();
    let created = create_job_with_environment_templates(
        &jt,
        &attached,
        &processed,
        &jt.default_validation_context(),
    )
    .map(|_| ())
    .map_err(|e| e.to_string());
    let expected = format!(
        "{SUBMISSION_ERR}EnvironmentTemplate[0] -> environment -> envParameterDefinitions[0] -> \
         default:\n\tParameter 'Need': value 9 exceeds maximum 5"
    );
    assert_eq!(checked.unwrap_err(), expected);
    assert_eq!(created.unwrap_err(), expected);
}

/// Template Schemas §9.1 rule 4 at job creation through `exclusiveGroup`:
/// a Job Template Service listed only by a `BY_DEFAULT` Job Environment in
/// group `g` is used as far as template validation can tell, but an
/// attachment whose Environment is in `g` supersedes that Job Environment
/// for every Step, so no Step is in the Service's scope and job creation
/// rejects it as unused.
#[test]
fn an_inline_service_left_unused_by_an_exclusive_group_is_rejected_at_job_creation() {
    let t = job(
        "  - name: E2\n    exclusiveGroup: g\n    dependencies: [{service: X}]\n    \
         variables: {K: \"{{Service.X.main.port}}\"}\n",
        "",
        "",
        "",
    )
    .replace("    dependencies: [{service: X}]\n    script", "    script");
    let g0 = "specificationVersion: environment-2023-09
extensions: [ENV_PARAMS, EXPR]
environment:
  name: G0
  exclusiveGroup: g
  variables: {K: v}
";
    decode(&t).unwrap();
    assert_eq!(
        create(&t, &[g0]).map(|_| ()).unwrap_err(),
        format!(
            "{SUBMISSION_ERR}JobTemplate -> services[0]:\n\tService 'X' is unused: no Step is in \
             its scope once every enableIf and exclusiveGroup is decided (Template Schemas §9.1 \
             rule 4)."
        )
    );
}
