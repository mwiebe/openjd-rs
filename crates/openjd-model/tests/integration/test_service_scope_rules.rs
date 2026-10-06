// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Integration tests for the RFC 0009 declared Service dependencies: the
//! Service scope (Template Schemas §9.1), the combined dependency graph
//! (§3.2, §9.9), `dependsOn: "service:<name>"` on Steps and Services
//! (§3.1 constraint 4, §3.2, §9 item 4), and the reference-dependent
//! `runScope` default (§4 item 3), at template validation and at job
//! creation.
//!
//! Scope is computed from the `dependencies` lists, never from references:
//!
//! 1. a Step that lists `service:X` is in `X`'s scope;
//! 2. a Service `Y` listing `service:X` puts `Y`'s scope inside `X`'s,
//!    transitively;
//! 3. a `jobEnvironments` entry referencing `Service.X.*` puts every Step in
//!    `X`'s scope;
//! 4. a Job Template Service with no Step in its scope is rejected as
//!    unused (Environment Template Services are exempt).
//!
//! `Service.X.*` is visible to a Step or Service only if it lists
//! `service:X`; required Services are visible everywhere. The Step-to-Step,
//! Step-to-Service, Service-to-Step and Service-to-Service edges form one
//! graph that must be acyclic. Under `SERVICE` a Step name may not contain
//! `:`.
//!
//! Error assertions follow the repo convention of asserting on the full
//! Pydantic-style error path + message.

use openjd_expr::ExprValue;
use openjd_model::job::{self, RunScope};
use openjd_model::template::{compute_service_scopes, DependencyTarget, JobTemplate, ServiceScope};
use openjd_model::{
    apply_environment_templates, create_job, decode_environment_template, decode_job_template,
    AttachedEnvironmentTemplate, CallerLimits, JobParameterInputValues, ModelError,
};

const EXTS: &[&str] = &["EXPR", "SERVICE", "FEATURE_BUNDLE_1", "WRAP_ACTIONS"];

const JOB_ERR: &str = "Model validation error: 1 validation error for JobTemplate\n";
const ENV_ERR: &str = "Model validation error: 1 validation error for EnvironmentTemplate\n";

fn yaml_val(s: &str) -> serde_json::Value {
    serde_saphyr::from_str(s).unwrap()
}

fn decode(template: &str) -> JobTemplate {
    decode_job_template(yaml_val(template), Some(EXTS), &CallerLimits::default())
        .expect("template should validate")
}

/// The full error text of a Job Template that fails validation.
fn job_err(template: &str) -> String {
    decode_job_template(yaml_val(template), Some(EXTS), &CallerLimits::default())
        .expect_err("template should fail validation")
        .to_string()
}

fn env_err(template: &str) -> String {
    decode_environment_template(yaml_val(template), Some(EXTS), &CallerLimits::default())
        .expect_err("environment template should fail validation")
        .to_string()
}

fn create(template: &str) -> Result<job::Job, ModelError> {
    let jt = decode(template);
    let td = tempfile::TempDir::new().unwrap();
    let dir = td.path().to_str().unwrap();
    let processed = openjd_model::preprocess_job_parameters(
        &jt,
        &JobParameterInputValues::default(),
        &[],
        &openjd_model::PathParameterOptions::new(dir, dir),
    )
    .unwrap();
    create_job(&jt, &processed, &jt.default_validation_context())
}

fn create_ok(template: &str) -> job::Job {
    create(template).expect("job creation should succeed")
}

/// A `dependencies:` line listing `deps` (empty for none), indented for a
/// list item's body.
fn deps(deps: &[&str]) -> String {
    if deps.is_empty() {
        return String::new();
    }
    let items: Vec<String> = deps
        .iter()
        .map(|d| format!("{{ dependsOn: \"{d}\" }}"))
        .collect();
    format!("    dependencies: [{}]\n", items.join(", "))
}

/// One Service `name` with port `main`, `dependencies` `ds`, and
/// `variables` holding `vars` (a flow-mapping body, e.g.
/// `UP: "{{ Service.B.main.port }}"`; empty for none). Indented as a
/// `services` list item.
fn svc(name: &str, ds: &[&str], vars: &str) -> String {
    let vars = if vars.is_empty() {
        String::new()
    } else {
        format!("    variables: {{ {vars} }}\n")
    };
    format!(
        "  - name: {name}\n{}    ports: [{{ name: main }}]\n{vars}    script:\n      actions:\n        onRun: {{ command: serve }}\n",
        deps(ds)
    )
}

/// One Step `name` with `dependencies` `ds` whose `onRun` args are `args`
/// (a flow sequence).
fn step(name: &str, ds: &[&str], args: &str) -> String {
    format!(
        "  - name: {name}\n{}    script:\n      actions:\n        onRun:\n          command: run\n          args: {args}\n",
        deps(ds)
    )
}

/// A Job Template with `job_envs` (a complete `jobEnvironments:` block, or
/// empty), `services` and `steps` (list bodies).
fn job(job_envs: &str, services: &str, steps: &str) -> String {
    format!(
        "specificationVersion: \"jobtemplate-2023-09\"\nextensions: [SERVICE, EXPR, WRAP_ACTIONS]\nname: Test\n{job_envs}services:\n{services}steps:\n{steps}"
    )
}

/// An Environment Template whose `services` list body is `services`.
fn env_template(services: &str) -> String {
    format!(
        "specificationVersion: \"environment-2023-09\"\nextensions: [SERVICE, EXPR]\nservices:\n{services}"
    )
}

fn scope_of(jt: &JobTemplate, name: &str) -> ServiceScope {
    compute_service_scopes(jt)
        .expect("acyclic")
        .get(name)
        .unwrap_or_else(|| panic!("no scope for {name}"))
        .scope
        .clone()
}

const CLIENT_JOB_ENV: &str =
    "jobEnvironments:\n  - name: Client\n    variables: { H: \"{{ Service.X.main.connectAddress }}\" }\n";

// ════════════════════════════════════════════════════════════════════
// §9.1 rule 1 — a Step listing service:X is in X's scope
// ════════════════════════════════════════════════════════════════════

#[test]
fn step_listing_the_service_puts_only_that_step_in_scope() {
    let t = job(
        "",
        &svc("X", &[], ""),
        &format!(
            "{}{}",
            step("A", &["service:X"], r#"["{{ Service.X.main.port }}"]"#),
            step("B", &[], "[b]")
        ),
    );
    let jt = decode(&t);
    let dep = &jt.steps[0].dependencies.as_ref().unwrap()[0];
    assert_eq!(dep.target(true), DependencyTarget::Service("X"));
    assert_eq!(dep.target(false), DependencyTarget::Step("service:X"));

    let scopes = compute_service_scopes(&jt).unwrap();
    let x = scopes.get("X").unwrap();
    assert_eq!(x.name, "X");
    assert_eq!(x.scope, ServiceScope::steps(["A"]));
    assert!(x.scope.contains("A"));
    assert!(!x.scope.contains("B"));
    assert!(!x.scope.is_all_steps());
    assert_eq!(
        x.scope.step_names().unwrap().iter().collect::<Vec<_>>(),
        ["A"]
    );
    assert_eq!(x.scope.to_string(), "Step A");
    assert_eq!(x.dependent_steps, vec!["A".to_string()]);
    assert!(x.dependent_services.is_empty());
    assert!(x.depends_on_services.is_empty());
    assert!(x.depends_on_steps.is_empty());
    assert!(!x.referenced_by_job_environment);
    assert!(!x.is_unused());
    assert_eq!(scopes.scope_of("X"), ServiceScope::steps(["A"]));
    assert_eq!(scopes.iter().count(), 1);
    assert!(scopes.get("Nope").is_none());

    // Job creation records the scope and carries the Step's dependency.
    let job = create_ok(&t);
    assert!(job.service_active());
    let svc = &job.services.as_ref().unwrap()[0];
    assert_eq!(svc.scope, ServiceScope::steps(["A"]));
    assert!(svc.depends_on_services().next().is_none());
    assert!(svc.depends_on_steps().next().is_none());
    assert!(svc.dependencies.is_none());
    let a_deps = job.steps[0].dependencies.as_ref().expect("dependencies");
    assert_eq!(a_deps.len(), 1);
    assert_eq!(a_deps[0].depends_on, "service:X");
    assert_eq!(
        a_deps[0].target(job.service_active()),
        DependencyTarget::Service("X")
    );
    assert!(job.steps[1].dependencies.is_none());
}

#[test]
fn a_listing_step_need_not_reference_the_service() {
    // The dependency alone places the Step in scope (no Service.* reference).
    let t = job(
        "",
        &svc("X", &[], ""),
        &format!(
            "{}{}",
            step("A", &[], "[a]"),
            step("B", &["service:X"], "[b]")
        ),
    );
    let jt = decode(&t);
    assert_eq!(scope_of(&jt, "X"), ServiceScope::steps(["B"]));
}

#[test]
fn step_environment_script_let_and_embedded_file_references_need_the_dependency() {
    // With the dependency listed, every place a Step may reference Service.*
    // (stepEnvironments variables/let, script let, embedded files) is valid.
    let t = job(
        "",
        &format!(
            "{}{}{}{}",
            svc("ByEnvVar", &[], ""),
            svc("ByEnvLet", &[], ""),
            svc("ByScriptLet", &[], ""),
            svc("ByFile", &[], "")
        ),
        &format!(
            "{}{}{}",
            "  - name: Env\n    dependencies: [{ dependsOn: \"service:ByEnvVar\" }, { dependsOn: \"service:ByEnvLet\" }]\n    stepEnvironments:\n      - name: E\n        variables: { P: \"{{ Service.ByEnvVar.main.port }}\" }\n      - name: F\n        script:\n          let: [\"u = Service.ByEnvLet.main.connectAddress\"]\n          actions:\n            onEnter: { command: echo, args: [\"{{ u }}\"] }\n    script:\n      actions:\n        onRun: { command: run, args: [x] }\n",
            "  - name: Let\n    dependencies: [{ dependsOn: \"service:ByScriptLet\" }]\n    script:\n      let: [\"p = Service.ByScriptLet.main.port\"]\n      actions:\n        onRun: { command: run, args: [\"{{ p }}\"] }\n",
            "  - name: File\n    dependencies: [{ dependsOn: \"service:ByFile\" }]\n    script:\n      actions:\n        onRun: { command: run, args: [\"{{ Task.File.F }}\"] }\n      embeddedFiles:\n        - name: F\n          type: TEXT\n          data: \"{{ Service.ByFile.main.connectAddress }}\"\n",
        ),
    );
    let jt = decode(&t);
    assert_eq!(scope_of(&jt, "ByEnvVar"), ServiceScope::steps(["Env"]));
    assert_eq!(scope_of(&jt, "ByEnvLet"), ServiceScope::steps(["Env"]));
    assert_eq!(scope_of(&jt, "ByScriptLet"), ServiceScope::steps(["Let"]));
    assert_eq!(scope_of(&jt, "ByFile"), ServiceScope::steps(["File"]));
}

#[test]
fn two_listing_steps_are_both_in_scope() {
    let t = job(
        "",
        &svc("X", &[], ""),
        &format!(
            "{}{}{}",
            step("A", &["service:X"], r#"["{{ Service.X.main.port }}"]"#),
            step("B", &[], "[b]"),
            step(
                "C",
                &["service:X"],
                r#"["{{ Service.X.main.connectAddress }}"]"#
            )
        ),
    );
    let jt = decode(&t);
    let scopes = compute_service_scopes(&jt).unwrap();
    let x = scopes.get("X").unwrap();
    assert_eq!(x.scope, ServiceScope::steps(["A", "C"]));
    assert_eq!(x.scope.to_string(), "Steps A, C");
    assert_eq!(x.dependent_steps, vec!["A".to_string(), "C".to_string()]);
    let job = create_ok(&t);
    let json = serde_json::to_value(&job.services.as_ref().unwrap()[0]).unwrap();
    assert_eq!(
        json["scope"],
        serde_json::json!({"kind": "steps", "steps": ["A", "C"]})
    );
}

#[test]
fn own_references_need_no_dependency_and_are_not_edges() {
    // The Service's own bindAddress / port references are always visible.
    let t = job(
        "",
        &svc(
            "X",
            &[],
            r#"BIND: "{{ Service.X.main.bindAddress }}", PORT: "{{ Service.X.main.port }}""#,
        ),
        &step("A", &["service:X"], "[a]"),
    );
    let jt = decode(&t);
    let scopes = compute_service_scopes(&jt).unwrap();
    assert!(scopes.get("X").unwrap().depends_on_services.is_empty());
    assert_eq!(scopes.get("X").unwrap().scope, ServiceScope::steps(["A"]));
    assert!(create_ok(&t).services.unwrap()[0]
        .depends_on_services()
        .next()
        .is_none());
}

// ════════════════════════════════════════════════════════════════════
// Visibility — a reference without the dependency is rejected
// ════════════════════════════════════════════════════════════════════

#[test]
fn step_script_reference_without_dependency_is_rejected() {
    let t = job(
        "",
        &svc("Cache", &[], ""),
        &format!(
            "{}{}",
            step("Render", &[], r#"["{{ Service.Cache.main.port }}"]"#),
            step("Use", &["service:Cache"], "[u]")
        ),
    );
    let err = job_err(&t);
    assert!(err.starts_with(JOB_ERR), "{err}");
    assert!(
        err.contains(
            "steps[0] -> script -> actions -> onRun -> args[0]:\n\tFailed to parse interpolation expression at ["
        ),
        "{err}"
    );
    assert!(
        err.contains(
            "Step 'Render' references Service.Cache.main.port but does not list service:Cache in dependencies."
        ),
        "{err}"
    );
    assert!(!err.contains("Undefined variable"), "{err}");
}

#[test]
fn step_environment_reference_without_dependency_is_rejected() {
    let t = job(
        "",
        &svc("Cache", &[], ""),
        &format!(
            "{}{}",
            "  - name: Render\n    stepEnvironments:\n      - name: Tools\n        variables: { P: \"{{ Service.Cache.main.port }}\" }\n    script:\n      actions:\n        onRun: { command: run }\n",
            step("Use", &["service:Cache"], "[u]")
        ),
    );
    let err = job_err(&t);
    assert!(err.starts_with(JOB_ERR), "{err}");
    assert!(
        err.contains(
            "steps[0] -> stepEnvironments[0] -> variables -> P:\n\tFailed to parse interpolation expression at ["
        ),
        "{err}"
    );
    assert!(
        err.contains(
            "Step 'Render' references Service.Cache.main.port in stepEnvironments 'Tools' but does not list service:Cache in dependencies."
        ),
        "{err}"
    );
}

#[test]
fn service_reference_without_dependency_is_rejected() {
    let t = job(
        "",
        &format!(
            "{}{}",
            svc("Front", &[], r#"UP: "{{ Service.Back.main.port }}""#),
            svc("Back", &[], "")
        ),
        &step("S", &["service:Front", "service:Back"], "[s]"),
    );
    let err = job_err(&t);
    assert!(err.starts_with(JOB_ERR), "{err}");
    assert!(
        err.contains(
            "services[0] -> variables -> UP:\n\tFailed to parse interpolation expression at ["
        ),
        "{err}"
    );
    assert!(
        err.contains(
            "Service 'Front' references Service.Back.main.port but does not list service:Back in dependencies."
        ),
        "{err}"
    );
}

// ════════════════════════════════════════════════════════════════════
// §9.1 rule 3 — a Job Environment reference puts every Step in scope
// ════════════════════════════════════════════════════════════════════

#[test]
fn job_environment_reference_puts_every_step_in_scope() {
    let t = job(
        CLIENT_JOB_ENV,
        &svc("X", &[], ""),
        &format!(
            "{}{}",
            step("A", &["service:X"], r#"["{{ Service.X.main.port }}"]"#),
            step("B", &[], "[b]")
        ),
    );
    let jt = decode(&t);
    let scopes = compute_service_scopes(&jt).unwrap();
    let x = scopes.get("X").unwrap();
    assert_eq!(x.scope, ServiceScope::AllSteps);
    assert!(x.scope.is_all_steps());
    assert!(x.scope.contains("B"));
    assert_eq!(x.scope.step_names(), None);
    assert_eq!(x.scope.to_string(), "every Step");
    assert!(x.referenced_by_job_environment);
    assert!(!x.is_unused());
    // The Step's dependency is still recorded.
    assert_eq!(x.dependent_steps, vec!["A".to_string()]);

    let job = create_ok(&t);
    let svc = &job.services.as_ref().unwrap()[0];
    assert_eq!(svc.scope, ServiceScope::AllSteps);
    let json = serde_json::to_value(svc).unwrap();
    assert_eq!(json["scope"], serde_json::json!({"kind": "allSteps"}));
    let back: job::Service = serde_json::from_value(json).unwrap();
    assert_eq!(&back, svc);
}

#[test]
fn job_environment_reference_alone_keeps_the_service_used() {
    // No Step lists the Service; the Job Environment reference suffices.
    let t = job(CLIENT_JOB_ENV, &svc("X", &[], ""), &step("A", &[], "[a]"));
    let jt = decode(&t);
    let scopes = compute_service_scopes(&jt).unwrap();
    let x = scopes.get("X").unwrap();
    assert_eq!(x.scope, ServiceScope::AllSteps);
    assert!(x.dependent_steps.is_empty());
    assert!(x.referenced_by_job_environment);
}

#[test]
fn service_listed_by_a_job_environment_referenced_service_has_every_step() {
    // Rule 3 then rule 2. (Not named `Y`: YAML reads a bare `Y` as a boolean.)
    let t = job(
        "jobEnvironments:\n  - name: Client\n    variables: { H: \"{{ Service.Up.main.connectAddress }}\" }\n",
        &format!(
            "{}{}",
            svc("X", &[], ""),
            svc("Up", &["service:X"], r#"UP: "{{ Service.X.main.port }}""#)
        ),
        &step("A", &[], "[a]"),
    );
    let jt = decode(&t);
    let scopes = compute_service_scopes(&jt).unwrap();
    assert!(scopes.get("Up").unwrap().referenced_by_job_environment);
    assert!(!scopes.get("X").unwrap().referenced_by_job_environment);
    assert_eq!(scopes.get("X").unwrap().scope, ServiceScope::AllSteps);
    assert_eq!(
        scopes.get("X").unwrap().dependent_services,
        vec!["Up".to_string()]
    );
}

// ════════════════════════════════════════════════════════════════════
// §9.1 rule 4 — an unused Service is rejected
// ════════════════════════════════════════════════════════════════════

fn unused_message(name: &str) -> String {
    format!(
        "Service '{name}' is unused: no Step or Service lists 'service:{name}' in its \
         dependencies and no Job Environment references it, so no Step is in its scope."
    )
}

#[test]
fn unused_service_is_rejected() {
    let t = job(
        "",
        &format!("{}{}", svc("X", &[], ""), svc("Cache", &[], "")),
        &step("A", &["service:X"], "[a]"),
    );
    assert_eq!(
        job_err(&t),
        format!("{JOB_ERR}services[1]:\n\t{}", unused_message("Cache"))
    );
}

#[test]
fn computed_scope_of_an_unused_service_is_empty() {
    // compute_service_scopes reports what validation then rejects.
    let mut jt = decode(&job(
        "",
        &svc("X", &[], ""),
        &step("A", &["service:X"], "[a]"),
    ));
    jt.steps[0].dependencies = None;
    let scopes = compute_service_scopes(&jt).unwrap();
    let x = scopes.get("X").unwrap();
    assert!(x.is_unused());
    assert_eq!(x.scope, ServiceScope::steps(Vec::<String>::new()));
    assert_eq!(x.scope.to_string(), "no Step");
}

#[test]
fn service_listed_only_by_an_unused_service_is_unused_too() {
    let t = job(
        "",
        &format!(
            "{}{}{}",
            svc("Used", &[], ""),
            svc("Sidecar", &["service:Back"], ""),
            svc("Back", &[], "")
        ),
        &step("A", &["service:Used"], "[a]"),
    );
    let err = job_err(&t);
    assert!(
        err.starts_with("Model validation error: 2 validation errors for JobTemplate\n"),
        "{err}"
    );
    assert!(
        err.contains(&format!("services[1]:\n\t{}", unused_message("Sidecar"))),
        "{err}"
    );
    assert!(
        err.contains(&format!("services[2]:\n\t{}", unused_message("Back"))),
        "{err}"
    );
}

// ════════════════════════════════════════════════════════════════════
// §9.1 rule 2 — Service-to-Service dependencies, transitively
// ════════════════════════════════════════════════════════════════════

/// `Front` (listed by Step A) lists `Mid`, which lists `Back`; Step B lists
/// `Mid` directly. List order is the reverse of the dependency order.
const CHAIN: &str = "specificationVersion: \"jobtemplate-2023-09\"
extensions: [SERVICE, EXPR]
name: Chain
services:
  - name: Back
    ports: [{ name: main }]
    script: { actions: { onRun: { command: back } } }
  - name: Front
    dependencies: [{ dependsOn: \"service:Mid\" }]
    ports: [{ name: main }]
    variables: { UP: \"{{ Service.Mid.main.connectAddress }}\" }
    script: { actions: { onRun: { command: front } } }
  - name: Mid
    dependencies: [{ dependsOn: \"service:Back\" }]
    ports: [{ name: main }]
    variables: { UP: \"{{ Service.Back.main.port }}\" }
    script: { actions: { onRun: { command: mid } } }
steps:
  - name: A
    dependencies: [{ dependsOn: \"service:Front\" }]
    script: { actions: { onRun: { command: a, args: [\"{{ Service.Front.main.port }}\"] } } }
  - name: B
    dependencies: [{ dependsOn: \"service:Mid\" }]
    script: { actions: { onRun: { command: b, args: [\"{{ Service.Mid.main.port }}\"] } } }
  - name: C
    script: { actions: { onRun: { command: c } } }
";

#[test]
fn listed_service_scope_contains_the_listing_services_transitively() {
    let jt = decode(CHAIN);
    let scopes = compute_service_scopes(&jt).unwrap();
    let front = scopes.get("Front").unwrap();
    let mid = scopes.get("Mid").unwrap();
    let back = scopes.get("Back").unwrap();
    assert_eq!(front.scope, ServiceScope::steps(["A"]));
    // scope(Mid) ⊇ scope(Front) ∪ {B}.
    assert_eq!(mid.scope, ServiceScope::steps(["A", "B"]));
    // scope(Back) ⊇ scope(Mid), though no Step lists Back directly.
    assert_eq!(back.scope, ServiceScope::steps(["A", "B"]));
    assert!(back.dependent_steps.is_empty());
    assert_eq!(back.dependent_services, vec!["Mid".to_string()]);
    assert_eq!(mid.dependent_services, vec!["Front".to_string()]);
    assert_eq!(mid.dependent_steps, vec!["B".to_string()]);
    assert!(front.dependent_services.is_empty());
    assert_eq!(front.dependent_steps, vec!["A".to_string()]);
    // depends_on_services are the direct edges only.
    assert_eq!(
        front.depends_on_services.iter().collect::<Vec<_>>(),
        ["Mid"]
    );
    assert_eq!(mid.depends_on_services.iter().collect::<Vec<_>>(), ["Back"]);
    assert!(back.depends_on_services.is_empty());
    assert!(front.depends_on_steps.is_empty());

    let job = create_ok(CHAIN);
    let services = job.services.as_ref().unwrap();
    let by_name = |n: &str| services.iter().find(|s| s.name == n).unwrap();
    assert_eq!(
        by_name("Front").depends_on_services().collect::<Vec<_>>(),
        vec!["Mid"]
    );
    assert!(by_name("Front").depends_on_service("Mid"));
    assert!(!by_name("Front").depends_on_service("Back"));
    assert_eq!(
        by_name("Mid").depends_on_services().collect::<Vec<_>>(),
        vec!["Back"]
    );
    assert!(by_name("Back").depends_on_services().next().is_none());
    assert_eq!(by_name("Back").scope, ServiceScope::steps(["A", "B"]));
    let json = serde_json::to_value(by_name("Front")).unwrap();
    assert!(json.get("references").is_none(), "{json}");
    assert_eq!(
        json["dependencies"],
        serde_json::json!([{ "dependsOn": "service:Mid" }])
    );
    assert!(serde_json::to_value(by_name("Back"))
        .unwrap()
        .get("dependencies")
        .is_none());
}

#[test]
fn a_diamond_is_not_a_cycle() {
    // Top lists Left and Right, both of which list Bottom.
    let t = job(
        "",
        &format!(
            "{}{}{}{}",
            svc("Top", &["service:Left", "service:Right"], ""),
            svc("Left", &["service:Bottom"], ""),
            svc("Right", &["service:Bottom"], ""),
            svc("Bottom", &[], "")
        ),
        &step("S", &["service:Top"], "[s]"),
    );
    let jt = decode(&t);
    let scopes = compute_service_scopes(&jt).unwrap();
    assert_eq!(
        scopes
            .get("Top")
            .unwrap()
            .depends_on_services
            .iter()
            .collect::<Vec<_>>(),
        ["Left", "Right"]
    );
    assert_eq!(
        scopes.get("Bottom").unwrap().dependent_services,
        vec!["Left".to_string(), "Right".to_string()]
    );
    for name in ["Top", "Left", "Right", "Bottom"] {
        assert_eq!(
            scopes.get(name).unwrap().scope,
            ServiceScope::steps(["S"]),
            "{name}"
        );
    }
}

// ════════════════════════════════════════════════════════════════════
// Combined graph acyclicity (§3.2 constraint 3, §9.9)
// ════════════════════════════════════════════════════════════════════

#[test]
fn step_service_step_cycle_is_rejected() {
    let t = job(
        "",
        &svc("Indexer", &["Use"], ""),
        &step("Use", &["service:Indexer"], "[u]"),
    );
    assert_eq!(
        job_err(&t),
        format!(
            "{JOB_ERR}JobTemplate: dependencies contain a cycle: Use -> service:Indexer -> Use."
        )
    );
}

#[test]
fn service_service_cycle_is_rejected() {
    let t = job(
        "",
        &format!(
            "{}{}",
            svc("A", &["service:B"], ""),
            svc("B", &["service:A"], "")
        ),
        &step("S", &["service:A"], "[s]"),
    );
    assert_eq!(
        job_err(&t),
        format!("{JOB_ERR}JobTemplate: dependencies contain a cycle: service:A -> service:B -> service:A.")
    );
}

#[test]
fn three_service_cycle_is_rejected() {
    let t = job(
        "",
        &format!(
            "{}{}{}",
            svc("A", &["service:B"], ""),
            svc("B", &["service:C"], ""),
            svc("C", &["service:A"], "")
        ),
        &step("S", &["service:A"], "[s]"),
    );
    assert_eq!(
        job_err(&t),
        format!(
            "{JOB_ERR}JobTemplate: dependencies contain a cycle: service:A -> service:B -> service:C -> service:A."
        )
    );
}

#[test]
fn step_cycle_under_service_uses_the_combined_message() {
    let t = job(
        "",
        &svc("X", &[], ""),
        &format!(
            "{}{}",
            step("A", &["B", "service:X"], "[a]"),
            step("B", &["A"], "[b]")
        ),
    );
    assert_eq!(
        job_err(&t),
        format!("{JOB_ERR}JobTemplate: dependencies contain a cycle: A -> B -> A.")
    );
}

#[test]
fn service_depending_on_a_step_in_its_scope_is_a_cycle() {
    // Render lists service:X and X lists Render.
    let t = job(
        "",
        &svc("X", &["Render"], ""),
        &format!(
            "{}{}",
            step("Prep", &[], "[p]"),
            step("Render", &["Prep", "service:X"], "[r]")
        ),
    );
    assert_eq!(
        job_err(&t),
        format!(
            "{JOB_ERR}JobTemplate: dependencies contain a cycle: Render -> service:X -> Render."
        )
    );
}

#[test]
fn service_depending_on_a_step_in_its_transitive_scope_is_a_cycle() {
    // Render lists Up; Up lists X; X lists Render.
    let t = job(
        "",
        &format!(
            "{}{}",
            svc("X", &["Render"], ""),
            svc("Up", &["service:X"], "")
        ),
        &step("Render", &["service:Up"], "[r]"),
    );
    assert_eq!(
        job_err(&t),
        format!(
            "{JOB_ERR}JobTemplate: dependencies contain a cycle: Render -> service:Up -> service:X -> Render."
        )
    );
}

#[test]
fn cycle_in_an_environment_template_is_rejected() {
    let t = env_template(&format!(
        "{}{}",
        svc("A", &["service:B"], ""),
        svc("B", &["service:A"], "")
    ));
    assert_eq!(
        env_err(&t),
        format!(
            "{ENV_ERR}services:\n\tdependencies contain a cycle: service:A -> service:B -> service:A."
        )
    );
}

// ════════════════════════════════════════════════════════════════════
// §9 item 4 — <Service>.dependencies lists Steps and Services
// ════════════════════════════════════════════════════════════════════

#[test]
fn service_dependencies_on_steps_and_services_are_carried_into_the_job() {
    let t = job(
        "",
        &format!(
            "{}{}",
            svc(
                "X",
                &["Prep", "service:Db"],
                r#"DB: "{{ Service.Db.main.port }}""#
            ),
            svc("Db", &[], "")
        ),
        &format!(
            "{}{}",
            step("Prep", &[], "[p]"),
            step(
                "Render",
                &["Prep", "service:X"],
                r#"["{{ Service.X.main.port }}"]"#
            )
        ),
    );
    let jt = decode(&t);
    let deps = jt.services.as_ref().unwrap()[0]
        .dependencies
        .as_ref()
        .unwrap();
    assert_eq!(deps.len(), 2);
    assert_eq!(deps[0].depends_on, "Prep");
    assert_eq!(deps[1].depends_on, "service:Db");
    let scopes = compute_service_scopes(&jt).unwrap();
    let x = scopes.get("X").unwrap();
    assert_eq!(x.scope, ServiceScope::steps(["Render"]));
    assert_eq!(x.depends_on_steps, vec!["Prep".to_string()]);
    assert_eq!(x.depends_on_services.iter().collect::<Vec<_>>(), ["Db"]);
    let db = scopes.get("Db").unwrap();
    assert_eq!(db.scope, ServiceScope::steps(["Render"]));
    assert_eq!(db.dependent_services, vec!["X".to_string()]);

    let job = create_ok(&t);
    let svc = &job.services.as_ref().unwrap()[0];
    assert_eq!(svc.scope, ServiceScope::steps(["Render"]));
    assert_eq!(svc.depends_on_steps().collect::<Vec<_>>(), vec!["Prep"]);
    assert_eq!(svc.depends_on_services().collect::<Vec<_>>(), vec!["Db"]);
    let json = serde_json::to_value(svc).unwrap();
    assert_eq!(
        json["dependencies"],
        serde_json::json!([{ "dependsOn": "Prep" }, { "dependsOn": "service:Db" }])
    );
    let back: job::Service = serde_json::from_value(json).unwrap();
    assert_eq!(&back, svc);
}

#[test]
fn dependencies_must_not_be_empty() {
    let t = job(
        "",
        &svc("X", &[], "").replace("    ports:", "    dependencies: []\n    ports:"),
        &step("S", &["service:X"], "[s]"),
    );
    assert_eq!(
        job_err(&t),
        format!("{JOB_ERR}services[0] -> dependencies:\n\tmust not be empty.")
    );
}

#[test]
fn service_dependency_on_an_unknown_step_is_rejected() {
    let t = job(
        "",
        &svc("X", &["Prep", "Nope"], ""),
        &format!(
            "{}{}",
            step("Prep", &[], "[p]"),
            step("S", &["service:X"], "[s]")
        ),
    );
    assert_eq!(
        job_err(&t),
        format!("{JOB_ERR}services[0] -> dependencies[1]:\n\tdependency 'Nope' not found.")
    );
}

fn unknown_service_message() -> &'static str {
    "dependency 'service:Nope' not found: no Service of that name in services or requiresServices."
}

#[test]
fn step_dependency_on_an_unknown_service_is_rejected() {
    let t = job(
        "",
        &svc("X", &[], ""),
        &step("S", &["service:X", "service:Nope"], "[s]"),
    );
    assert_eq!(
        job_err(&t),
        format!(
            "{JOB_ERR}steps[0] -> dependencies[1]:\n\t{}",
            unknown_service_message()
        )
    );
}

#[test]
fn service_dependency_on_an_unknown_service_is_rejected() {
    let t = job(
        "",
        &svc("X", &["service:Nope"], ""),
        &step("S", &["service:X"], "[s]"),
    );
    assert_eq!(
        job_err(&t),
        format!(
            "{JOB_ERR}services[0] -> dependencies[0]:\n\t{}",
            unknown_service_message()
        )
    );
}

#[test]
fn duplicate_service_dependency_is_rejected() {
    let t = job(
        "",
        &svc("X", &[], ""),
        &step("S", &["service:X", "service:X"], "[s]"),
    );
    assert_eq!(
        job_err(&t),
        format!("{JOB_ERR}steps[0] -> dependencies[1]:\n\tduplicate dependency 'service:X'.")
    );
}

#[test]
fn service_self_dependency_is_rejected() {
    let t = job(
        "",
        &svc("X", &["service:X"], ""),
        &step("S", &["service:X"], "[s]"),
    );
    assert_eq!(
        job_err(&t),
        format!("{JOB_ERR}services[0] -> dependencies[0]:\n\tcannot depend on itself.")
    );
}

#[test]
fn job_environment_referenced_service_listing_a_step_is_rejected() {
    let t = job(
        CLIENT_JOB_ENV,
        &svc("X", &["Prep"], ""),
        &format!("{}{}", step("Prep", &[], "[p]"), step("S", &[], "[s]")),
    );
    assert_eq!(
        job_err(&t),
        format!(
            "{JOB_ERR}services[0] -> dependencies[0]:\n\tStep 'Prep' is in the scope of Service \
             'X' (a Job Environment references the Service, so every Step is in its scope); a \
             Service cannot depend on a Step in its own scope, which could not run until the \
             Service was READY."
        )
    );
}

// ════════════════════════════════════════════════════════════════════
// Required Services (requiresServices)
// ════════════════════════════════════════════════════════════════════

const REQUIRED: &str = "specificationVersion: \"jobtemplate-2023-09\"
extensions: [SERVICE, EXPR]
name: Required
requiresServices:
  - name: R
    ports: [{ name: main }]
services:
  - name: X
    dependencies: [{ dependsOn: \"service:R\" }]
    ports: [{ name: main }]
    variables: { R: \"{{ Service.R.main.port }}\" }
    script: { actions: { onRun: { command: serve } } }
steps:
  - name: A
    dependencies: [{ dependsOn: \"service:R\" }, { dependsOn: \"service:X\" }]
    script: { actions: { onRun: { command: a, args: [\"{{ Service.R.main.port }}\"] } } }
  - name: B
    script: { actions: { onRun: { command: b, args: [\"{{ Service.R.main.connectAddress }}\"] } } }
";

#[test]
fn depending_on_a_required_service_is_valid_and_a_no_op_for_scope() {
    let jt = decode(REQUIRED);
    let scopes = compute_service_scopes(&jt).unwrap();
    // R is external: no computed entry, and every Step is in its scope.
    assert!(scopes.get("R").is_none());
    assert_eq!(scopes.scope_of("R"), ServiceScope::AllSteps);
    let x = scopes.get("X").unwrap();
    assert_eq!(x.scope, ServiceScope::steps(["A"]));
    // A required Service is not an edge among the inline Services.
    assert!(x.depends_on_services.is_empty());

    let job = create_ok(REQUIRED);
    let svc = &job.services.as_ref().unwrap()[0];
    assert_eq!(svc.scope, ServiceScope::steps(["A"]));
    // The job::Service still lists the required Service it waits for.
    assert_eq!(svc.depends_on_services().collect::<Vec<_>>(), vec!["R"]);
}

// ════════════════════════════════════════════════════════════════════
// Environment Templates — `service:` only
// ════════════════════════════════════════════════════════════════════

#[test]
fn environment_template_service_may_list_a_service_of_the_same_document() {
    let t = env_template(&format!(
        "{}{}",
        svc("A", &[], ""),
        svc("B", &["service:A"], r#"UP: "{{ Service.A.main.port }}""#)
    ));
    let et = decode_environment_template(yaml_val(&t), Some(EXTS), &CallerLimits::default())
        .expect("environment template should validate");
    let b = &et.services.as_ref().unwrap()[1];
    assert_eq!(
        b.dependencies.as_ref().unwrap()[0].target(true),
        DependencyTarget::Service("A")
    );
}

#[test]
fn environment_template_service_listing_a_step_is_rejected() {
    let t = env_template(&svc("A", &["Prepare"], ""));
    assert_eq!(
        env_err(&t),
        format!(
            "{ENV_ERR}services[0] -> dependencies[0]:\n\tdependency 'Prepare' names a Step, but \
             an Environment Template has no Steps; a Service here may depend only on a Service of \
             the same document, as 'service:<name>'."
        )
    );
}

#[test]
fn environment_template_unknown_service_dependency_is_rejected() {
    let t = env_template(&svc("A", &["service:Nope"], ""));
    assert_eq!(
        env_err(&t),
        format!(
            "{ENV_ERR}services[0] -> dependencies[0]:\n\tdependency 'service:Nope' not found: no \
             Service of that name in this document's services."
        )
    );
}

// ════════════════════════════════════════════════════════════════════
// §3.1 constraint 4 — `:` in a Step name
// ════════════════════════════════════════════════════════════════════

#[test]
fn step_name_with_colon_is_rejected_under_service() {
    let t = job("", &svc("X", &[], ""), &step("a:b", &["service:X"], "[a]"));
    assert_eq!(
        job_err(&t),
        format!(
            "{JOB_ERR}steps[0] -> name:\n\tmust not contain ':' when the SERVICE extension is \
             used, so that a dependsOn value beginning 'service:' can only name a Service \
             (Template Schemas §3.1 constraint 4)."
        )
    );
}

#[test]
fn step_name_with_colon_is_accepted_without_service() {
    // Without SERVICE, `service:Prep` is an ordinary Step name.
    let t = "specificationVersion: \"jobtemplate-2023-09\"
name: Colons
steps:
  - name: \"service:Prep\"
    script: { actions: { onRun: { command: prep } } }
  - name: Use
    dependencies: [{ dependsOn: \"service:Prep\" }]
    script: { actions: { onRun: { command: use } } }
";
    let jt = decode(t);
    let dep = &jt.steps[1].dependencies.as_ref().unwrap()[0];
    assert_eq!(dep.target(false), DependencyTarget::Step("service:Prep"));
    let job = create_ok(t);
    assert!(!job.service_active());
    let dep = &job.steps[1].dependencies.as_ref().unwrap()[0];
    assert_eq!(
        dep.target(job.service_active()),
        DependencyTarget::Step("service:Prep")
    );
}

// ════════════════════════════════════════════════════════════════════
// §4 item 3 — the runScope default follows the Environment's references
// ════════════════════════════════════════════════════════════════════

const RUN_SCOPE_DEFAULTS: &str = "specificationVersion: \"jobtemplate-2023-09\"
extensions: [SERVICE, EXPR]
name: RunScopes
jobEnvironments:
  - name: Client
    variables: { HOST: \"{{ Service.X.main.connectAddress }}\" }
  - name: Plain
    variables: { K: v }
services:
  - name: X
    ports: [{ name: main }]
    script: { actions: { onRun: { command: serve } } }
steps:
  - name: S
    dependencies: [{ dependsOn: \"service:X\" }]
    stepEnvironments:
      - name: StepClient
        script:
          actions:
            onEnter: { command: echo, args: [\"{{ Service.X.main.port }}\"] }
      - name: StepPlain
        variables: { K: v }
    script: { actions: { onRun: { command: run } } }
";

#[test]
fn environment_referencing_a_service_defaults_to_task_only() {
    let jt = decode(RUN_SCOPE_DEFAULTS);
    let envs = jt.job_environments.as_ref().unwrap();
    let client = &envs[0];
    assert!(client.run_scope.is_none());
    assert!(client.references_service());
    assert!(client.default_run_scope_is_task_only());
    assert!(client.runs_in(RunScope::Task));
    assert!(!client.runs_in(RunScope::Service));
    assert_eq!(
        client.effective_run_scope().collect::<Vec<_>>(),
        [RunScope::Task]
    );
    let plain = &envs[1];
    assert!(!plain.references_service());
    assert!(!plain.default_run_scope_is_task_only());
    assert!(plain.runs_in(RunScope::Task));
    assert!(plain.runs_in(RunScope::Service));
    let step_envs = jt.steps[0].step_environments.as_ref().unwrap();
    assert!(step_envs[0].default_run_scope_is_task_only());
    assert!(!step_envs[0].runs_in(RunScope::Service));
    assert!(step_envs[1].runs_in(RunScope::Service));

    // Job creation materializes the default for the referencing ones only.
    let job = create_ok(RUN_SCOPE_DEFAULTS);
    let envs = job.job_environments.as_ref().unwrap();
    assert_eq!(envs[0].run_scope, Some(vec![RunScope::Task]));
    assert!(!envs[0].runs_in(RunScope::Service));
    assert_eq!(envs[1].run_scope, None);
    assert!(envs[1].runs_in(RunScope::Service));
    let step_envs = job.steps[0].step_environments.as_ref().unwrap();
    assert_eq!(step_envs[0].run_scope, Some(vec![RunScope::Task]));
    assert_eq!(step_envs[1].run_scope, None);
    let json = serde_json::to_value(&envs[0]).unwrap();
    assert_eq!(json["runScope"], serde_json::json!(["TASK"]));
}

#[test]
fn explicit_service_run_scope_with_a_service_reference_is_still_rejected() {
    let t = RUN_SCOPE_DEFAULTS.replace(
        "  - name: Client\n    variables:",
        "  - name: Client\n    runScope: [SERVICE]\n    variables:",
    );
    let err = job_err(&t);
    assert!(err.starts_with(JOB_ERR), "{err}");
    assert!(
        err.contains(
            "jobEnvironments[0] -> variables -> HOST:\n\tFailed to parse interpolation expression at ["
        ),
        "{err}"
    );
    assert!(
        err.contains(
            "Environment 'Client' is entered in Service Sessions (its runScope includes SERVICE) \
             and may not reference Service.*; declare runScope: [TASK] if it configures Tasks."
        ),
        "{err}"
    );
}

/// A Job Environment wrapper that references `Service.X.*` in `hook_args`
/// of `onWrapTaskRun`, defining `hooks`. The reference keeps `X` used.
fn referencing_wrapper(hooks: &[&str]) -> String {
    let actions: String = hooks
        .iter()
        .map(|h| {
            if *h == "onWrapTaskRun" {
                format!(
                    "        {h}: {{ command: wrap, args: [\"{{{{ Service.X.main.port }}}}\"] }}\n"
                )
            } else {
                format!("        {h}: {{ command: wrap }}\n")
            }
        })
        .collect();
    format!(
        "specificationVersion: \"jobtemplate-2023-09\"
extensions: [SERVICE, EXPR, WRAP_ACTIONS]
name: Wrapped
jobEnvironments:
  - name: Wrapper
    script:
      actions:
{actions}services:
  - name: X
    ports: [{{ name: main }}]
    script: {{ actions: {{ onRun: {{ command: serve }} }} }}
steps:
  - name: S
    dependencies: [{{ dependsOn: \"service:X\" }}]
    script: {{ actions: {{ onRun: {{ command: run }} }} }}
"
    )
}

#[test]
fn wrap_hooks_follow_the_default_task_run_scope_of_a_referencing_environment() {
    // §4.3 WRAP_ACTIONS constraint 6 with the effective runScope [TASK]:
    // exactly the three RFC 0008 hooks.
    let jt = decode(&referencing_wrapper(&[
        "onWrapEnvEnter",
        "onWrapTaskRun",
        "onWrapEnvExit",
    ]));
    let env = &jt.job_environments.as_ref().unwrap()[0];
    assert!(env.default_run_scope_is_task_only());
}

#[test]
fn service_wrap_hooks_rejected_on_a_referencing_environment_with_default_run_scope() {
    // The four Service hooks are not permitted.
    let err = job_err(&referencing_wrapper(&[
        "onWrapEnvEnter",
        "onWrapTaskRun",
        "onWrapEnvExit",
        "onWrapServiceEnter",
        "onWrapServiceRun",
        "onWrapServiceHealthCheck",
        "onWrapServiceExit",
    ]));
    assert!(
        err.starts_with("Model validation error: 4 validation errors for JobTemplate\n"),
        "{err}"
    );
    for hook in [
        "onWrapServiceEnter",
        "onWrapServiceRun",
        "onWrapServiceHealthCheck",
        "onWrapServiceExit",
    ] {
        // The parenthetical names the effective runScope. It must not call
        // it "every kind of Session": this Environment references
        // Service.*, so its default is [TASK] (§4 item 3).
        let head = format!(
            "jobEnvironments[0] -> script -> actions -> {hook}:\n\t{hook} must not be defined: \
             this environment's runScope ("
        );
        let start = err
            .find(&head)
            .unwrap_or_else(|| panic!("missing {head:?} in:\n{err}"))
            + head.len();
        let rest = &err[start..];
        let end = rest
            .find(") excludes SERVICE (RFC 0009).")
            .unwrap_or_else(|| panic!("missing tail for {hook} in:\n{err}"));
        let described = &rest[..end];
        assert!(
            !described.contains("every kind of Session"),
            "{hook}: the runScope is described as {described:?}, but the default for an \
             Environment that references Service.* is [TASK]:\n{err}"
        );
    }
}

#[test]
fn task_wrap_hook_required_on_a_referencing_environment_with_default_run_scope() {
    let err = job_err(
        &referencing_wrapper(&["onWrapEnvEnter", "onWrapTaskRun", "onWrapEnvExit"])
            .replace(
                "        onWrapEnvExit: { command: wrap }\n",
                "        onWrapEnvExit: { command: wrap, args: [\"{{ Service.X.main.port }}\"] }\n",
            )
            .replace(
                "        onWrapTaskRun: { command: wrap, args: [\"{{ Service.X.main.port }}\"] }\n",
                "",
            ),
    );
    assert!(err.starts_with(JOB_ERR), "{err}");
    let head = "jobEnvironments[0] -> script -> actions:\n\ta wrapping environment whose \
                runScope includes TASK (";
    let tail = ") must define onWrapTaskRun; missing: onWrapTaskRun (RFC 0009).";
    let start = err.find(head).unwrap_or_else(|| panic!("{err}")) + head.len();
    let end = err[start..].find(tail).unwrap_or_else(|| panic!("{err}"));
    let described = &err[start..start + end];
    // As above: the default for this Environment is [TASK], not every kind.
    assert!(
        !described.contains("every kind of Session"),
        "the runScope is described as {described:?}:\n{err}"
    );
}

#[test]
fn attached_environment_referencing_its_service_defaults_to_task_only_in_the_job() {
    // The RFC's queue-cache Environment Template without its explicit
    // `runScope: [TASK]`: the default is the same, and the converted Job
    // Environment carries it.
    let et_text = include_str!("../fixtures/rfc0009/queue-cache.environment.yaml")
        .replace("  runScope: [TASK]\n", "");
    assert!(!et_text.contains("runScope"));
    let et = decode_environment_template(yaml_val(&et_text), Some(EXTS), &CallerLimits::default())
        .expect("environment template should validate");
    assert!(et
        .environment
        .as_ref()
        .unwrap()
        .default_run_scope_is_task_only());
    let jt = decode(include_str!(
        "../fixtures/rfc0009/queue-cache-consumer.job.yaml"
    ));
    let td = tempfile::TempDir::new().unwrap();
    let dir = td.path().to_str().unwrap();
    let mut input = JobParameterInputValues::default();
    input.insert(
        "CacheMemoryMiB".to_string(),
        ExprValue::String("1024".to_string()),
    );
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
    assert_eq!(
        applied.environments[0].run_scope,
        Some(vec![RunScope::Task])
    );
    assert!(!applied.environments[0].runs_in(RunScope::Service));
}
