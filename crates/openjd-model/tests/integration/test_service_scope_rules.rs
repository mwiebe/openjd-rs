// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Integration tests for the restructured RFC 0009 Service scope
//! (Template Schemas §9.1), the reference-cycle rule (§1.1 item 8.4, §9.9
//! item 9), `<Service>.dependencies` (§9 item 4, §9.9 item 10), and the
//! reference-dependent `runScope` default (§4 item 3), at template
//! validation and at job creation.
//!
//! Scope is computed from `Service.X.*` references:
//!
//! 1. a Step whose `script` or `stepEnvironments` references `X` is in
//!    `X`'s scope;
//! 2. a `jobEnvironments` entry referencing `X` puts every Step in it;
//! 3. a Service `Y` referencing `X` puts `Y`'s scope inside `X`'s,
//!    transitively;
//! 4. a Service nothing references has every Step in its scope.
//!
//! Error assertions follow the repo convention of asserting on the full
//! Pydantic-style error path + message.

use openjd_expr::ExprValue;
use openjd_model::job::{self, RunScope};
use openjd_model::template::{compute_service_scopes, JobTemplate, ServiceScope};
use openjd_model::{
    apply_environment_templates, create_job, decode_environment_template, decode_job_template,
    AttachedEnvironmentTemplate, CallerLimits, JobParameterInputValues, ModelError,
};

const EXTS: &[&str] = &["EXPR", "SERVICE", "FEATURE_BUNDLE_1", "WRAP_ACTIONS"];

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

/// One Service `name` with port `main` whose `variables` hold `vars` (a
/// flow-mapping body, e.g. `UP: "{{ Service.B.main.port }}"`; empty for
/// none). Indented as a `services` list item.
fn svc(name: &str, vars: &str) -> String {
    let vars = if vars.is_empty() {
        String::new()
    } else {
        format!("    variables: {{ {vars} }}\n")
    };
    format!(
        "  - name: {name}\n    ports: [{{ name: main }}]\n{vars}    script:\n      actions:\n        onRun: {{ command: serve }}\n"
    )
}

/// One Step `name` whose `onRun` args are `args` (a flow sequence) with
/// `extra` (already-indented YAML lines) after its name.
fn step(name: &str, extra: &str, args: &str) -> String {
    format!(
        "  - name: {name}\n{extra}    script:\n      actions:\n        onRun:\n          command: run\n          args: {args}\n"
    )
}

/// A Job Template with `job_envs` (a complete `jobEnvironments:` block, or
/// empty), `services` and `steps` (list bodies).
fn job(job_envs: &str, services: &str, steps: &str) -> String {
    format!(
        "specificationVersion: \"jobtemplate-2023-09\"\nextensions: [SERVICE, EXPR, WRAP_ACTIONS]\nname: Test\n{job_envs}services:\n{services}steps:\n{steps}"
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

// ════════════════════════════════════════════════════════════════════
// §9.1 item 1 — a Step referencing X is in X's scope
// ════════════════════════════════════════════════════════════════════

#[test]
fn step_script_reference_puts_only_that_step_in_scope() {
    let t = job(
        "",
        &svc("X", ""),
        &format!(
            "{}{}",
            step("A", "", r#"["{{ Service.X.main.port }}"]"#),
            step("B", "", "[b]")
        ),
    );
    let jt = decode(&t);
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
    assert_eq!(x.referencing_steps, vec!["A".to_string()]);
    assert!(!x.referenced_by_job_environment);
    assert!(x.references.is_empty());
    assert_eq!(scopes.scope_of("X"), ServiceScope::steps(["A"]));

    // Job creation records the same scope on the job::Service.
    let job = create_ok(&t);
    let svc = &job.services.as_ref().unwrap()[0];
    assert_eq!(svc.scope, ServiceScope::steps(["A"]));
    assert!(svc.references.is_empty());
}

#[test]
fn step_environment_and_script_let_and_embedded_file_references_count() {
    // §9.1 item 1: the script's actions, embedded files and `let`, and any
    // `stepEnvironments` entry (variables, actions, embedded files, let).
    let t = job(
        "",
        &format!(
            "{}{}{}{}",
            svc("ByEnvVar", ""),
            svc("ByEnvLet", ""),
            svc("ByScriptLet", ""),
            svc("ByFile", "")
        ),
        &format!(
            "{}{}{}",
            step(
                "Env",
                "    stepEnvironments:\n      - name: E\n        variables: { P: \"{{ Service.ByEnvVar.main.port }}\" }\n      - name: F\n        script:\n          let: [\"u = Service.ByEnvLet.main.connectAddress\"]\n          actions:\n            onEnter: { command: echo, args: [\"{{ u }}\"] }\n",
                "[x]"
            ),
            "  - name: Let\n    script:\n      let: [\"p = Service.ByScriptLet.main.port\"]\n      actions:\n        onRun: { command: run, args: [\"{{ p }}\"] }\n",
            "  - name: File\n    script:\n      actions:\n        onRun: { command: run, args: [\"{{ Task.File.F }}\"] }\n      embeddedFiles:\n        - name: F\n          type: TEXT\n          data: \"{{ Service.ByFile.main.connectAddress }}\"\n",
        ),
    );
    let jt = decode(&t);
    assert_eq!(scope_of(&jt, "ByEnvVar"), ServiceScope::steps(["Env"]));
    assert_eq!(scope_of(&jt, "ByEnvLet"), ServiceScope::steps(["Env"]));
    assert_eq!(scope_of(&jt, "ByScriptLet"), ServiceScope::steps(["Let"]));
    assert_eq!(scope_of(&jt, "ByFile"), ServiceScope::steps(["File"]));
}

#[test]
fn two_referencing_steps_are_both_in_scope() {
    let t = job(
        "",
        &svc("X", ""),
        &format!(
            "{}{}{}",
            step("A", "", r#"["{{ Service.X.main.port }}"]"#),
            step("B", "", "[b]"),
            step("C", "", r#"["{{ Service.X.main.connectAddress }}"]"#)
        ),
    );
    let jt = decode(&t);
    let scopes = compute_service_scopes(&jt).unwrap();
    let x = scopes.get("X").unwrap();
    assert_eq!(x.scope, ServiceScope::steps(["A", "C"]));
    assert_eq!(x.scope.to_string(), "Steps A, C");
    assert_eq!(x.referencing_steps, vec!["A".to_string(), "C".to_string()]);
    let job = create_ok(&t);
    let json = serde_json::to_value(&job.services.as_ref().unwrap()[0]).unwrap();
    assert_eq!(
        json["scope"],
        serde_json::json!({"kind": "steps", "steps": ["A", "C"]})
    );
}

// ════════════════════════════════════════════════════════════════════
// §9.1 items 2 and 4 — every Step
// ════════════════════════════════════════════════════════════════════

#[test]
fn job_environment_reference_puts_every_step_in_scope() {
    let t = job(
        "jobEnvironments:\n  - name: Client\n    variables: { H: \"{{ Service.X.main.connectAddress }}\" }\n",
        &svc("X", ""),
        &format!(
            "{}{}",
            step("A", "", r#"["{{ Service.X.main.port }}"]"#),
            step("B", "", "[b]")
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
    // The Step reference is still recorded.
    assert_eq!(x.referencing_steps, vec!["A".to_string()]);

    let job = create_ok(&t);
    let svc = &job.services.as_ref().unwrap()[0];
    assert_eq!(svc.scope, ServiceScope::AllSteps);
    let json = serde_json::to_value(svc).unwrap();
    assert_eq!(json["scope"], serde_json::json!({"kind": "allSteps"}));
    let back: job::Service = serde_json::from_value(json).unwrap();
    assert_eq!(&back, svc);
}

#[test]
fn unreferenced_service_has_every_step_in_scope() {
    let t = job("", &svc("X", ""), &step("A", "", "[a]"));
    let jt = decode(&t);
    let scopes = compute_service_scopes(&jt).unwrap();
    let x = scopes.get("X").unwrap();
    assert_eq!(x.scope, ServiceScope::AllSteps);
    assert!(x.referencing_steps.is_empty());
    assert!(!x.referenced_by_job_environment);
    assert_eq!(scopes.iter().count(), 1);
    assert!(scopes.get("Nope").is_none());
    assert_eq!(
        create_ok(&t).services.unwrap()[0].scope,
        ServiceScope::AllSteps
    );
}

#[test]
fn own_references_do_not_change_scope_or_references() {
    // The Service's own bindAddress / port references are not edges.
    let t = job(
        "",
        &svc(
            "X",
            r#"BIND: "{{ Service.X.main.bindAddress }}", PORT: "{{ Service.X.main.port }}""#,
        ),
        &step("A", "", r#"["{{ Service.X.main.port }}"]"#),
    );
    let jt = decode(&t);
    let scopes = compute_service_scopes(&jt).unwrap();
    assert!(scopes.get("X").unwrap().references.is_empty());
    assert_eq!(scopes.get("X").unwrap().scope, ServiceScope::steps(["A"]));
    assert!(create_ok(&t).services.unwrap()[0].references.is_empty());
}

// ════════════════════════════════════════════════════════════════════
// §9.1 item 3 — Service-to-Service references, transitively
// ════════════════════════════════════════════════════════════════════

/// `Front` (referenced by Step A) references `Mid`, which references
/// `Back`; Step B references `Mid` directly. List order is the reverse of
/// the reference order.
const CHAIN: &str = "specificationVersion: \"jobtemplate-2023-09\"
extensions: [SERVICE, EXPR]
name: Chain
services:
  - name: Back
    ports: [{ name: main }]
    script: { actions: { onRun: { command: back } } }
  - name: Front
    ports: [{ name: main }]
    variables: { UP: \"{{ Service.Mid.main.connectAddress }}\" }
    script: { actions: { onRun: { command: front } } }
  - name: Mid
    ports: [{ name: main }]
    variables: { UP: \"{{ Service.Back.main.port }}\" }
    script: { actions: { onRun: { command: mid } } }
steps:
  - name: A
    script: { actions: { onRun: { command: a, args: [\"{{ Service.Front.main.port }}\"] } } }
  - name: B
    script: { actions: { onRun: { command: b, args: [\"{{ Service.Mid.main.port }}\"] } } }
  - name: C
    script: { actions: { onRun: { command: c } } }
";

#[test]
fn referenced_service_scope_contains_the_referencing_services_transitively() {
    let jt = decode(CHAIN);
    let scopes = compute_service_scopes(&jt).unwrap();
    let front = scopes.get("Front").unwrap();
    let mid = scopes.get("Mid").unwrap();
    let back = scopes.get("Back").unwrap();
    assert_eq!(front.scope, ServiceScope::steps(["A"]));
    // scope(Mid) ⊇ scope(Front) ∪ {B}.
    assert_eq!(mid.scope, ServiceScope::steps(["A", "B"]));
    // scope(Back) ⊇ scope(Mid), though no Step references Back directly.
    assert_eq!(back.scope, ServiceScope::steps(["A", "B"]));
    assert!(back.referencing_steps.is_empty());
    // `references` are the direct edges only.
    assert_eq!(front.references.iter().collect::<Vec<_>>(), ["Mid"]);
    assert_eq!(mid.references.iter().collect::<Vec<_>>(), ["Back"]);
    assert!(back.references.is_empty());

    let job = create_ok(CHAIN);
    let services = job.services.as_ref().unwrap();
    let by_name = |n: &str| services.iter().find(|s| s.name == n).unwrap();
    assert_eq!(by_name("Front").references, vec!["Mid".to_string()]);
    assert_eq!(by_name("Mid").references, vec!["Back".to_string()]);
    assert!(by_name("Back").references.is_empty());
    assert_eq!(by_name("Back").scope, ServiceScope::steps(["A", "B"]));
    let json = serde_json::to_value(by_name("Front")).unwrap();
    assert_eq!(json["references"], serde_json::json!(["Mid"]));
    assert!(serde_json::to_value(by_name("Back"))
        .unwrap()
        .get("references")
        .is_none());
}

#[test]
fn unreferenced_service_referencing_another_gives_it_every_step() {
    // `Sidecar` is referenced by nothing (every Step) and references `X`,
    // so X's scope is every Step even though only Step A references it.
    let t = job(
        "",
        &format!(
            "{}{}",
            svc("Sidecar", r#"UP: "{{ Service.X.main.port }}""#),
            svc("X", "")
        ),
        &format!(
            "{}{}",
            step("A", "", r#"["{{ Service.X.main.port }}"]"#),
            step("B", "", "[b]")
        ),
    );
    let jt = decode(&t);
    assert_eq!(scope_of(&jt, "Sidecar"), ServiceScope::AllSteps);
    assert_eq!(scope_of(&jt, "X"), ServiceScope::AllSteps);
}

#[test]
fn service_referenced_through_a_job_environment_referenced_service() {
    // Rule 2 then rule 3: the Job Environment references Up, Up references X.
    // (Not named `Y`: YAML reads a bare `Y` as a boolean.)
    let t = job(
        "jobEnvironments:\n  - name: Client\n    variables: { H: \"{{ Service.Up.main.connectAddress }}\" }\n",
        &format!(
            "{}{}",
            svc("X", ""),
            svc("Up", r#"UP: "{{ Service.X.main.port }}""#)
        ),
        &step("A", "", r#"["{{ Service.X.main.port }}"]"#),
    );
    let jt = decode(&t);
    let scopes = compute_service_scopes(&jt).unwrap();
    assert!(scopes.get("Up").unwrap().referenced_by_job_environment);
    assert!(!scopes.get("X").unwrap().referenced_by_job_environment);
    assert_eq!(scopes.get("X").unwrap().scope, ServiceScope::AllSteps);
}

// ════════════════════════════════════════════════════════════════════
// §1.1 item 8.4 / §9.9 item 9 — the reference graph must be acyclic
// ════════════════════════════════════════════════════════════════════

fn cycle_message(path: &str) -> String {
    format!(
        "the Service.* references among the Services form a cycle: {path}; a Service may not \
         reference a Service that (transitively) references it."
    )
}

#[test]
fn two_service_reference_cycle_is_rejected() {
    let t = job(
        "",
        &format!(
            "{}{}",
            svc("A", r#"UP: "{{ Service.B.main.port }}""#),
            svc("B", r#"UP: "{{ Service.A.main.port }}""#)
        ),
        &step("S", "", "[x]"),
    );
    assert_eq!(
        job_err(&t),
        format!(
            "Model validation error: 1 validation error for JobTemplate\nservices:\n\t{}",
            cycle_message("A -> B -> A")
        )
    );
}

#[test]
fn three_service_reference_cycle_is_rejected() {
    let t = job(
        "",
        &format!(
            "{}{}{}",
            svc("A", r#"UP: "{{ Service.B.main.port }}""#),
            svc("B", r#"UP: "{{ Service.C.main.port }}""#),
            svc("C", r#"UP: "{{ Service.A.main.port }}""#)
        ),
        &step("S", "", "[x]"),
    );
    assert_eq!(
        job_err(&t),
        format!(
            "Model validation error: 1 validation error for JobTemplate\nservices:\n\t{}",
            cycle_message("A -> B -> C -> A")
        )
    );
}

#[test]
fn reference_cycle_in_an_environment_template_is_rejected() {
    let t = format!(
        "specificationVersion: \"environment-2023-09\"\nextensions: [SERVICE, EXPR]\nservices:\n{}{}",
        svc("A", r#"UP: "{{ Service.B.main.port }}""#),
        svc("B", r#"UP: "{{ Service.A.main.port }}""#)
    );
    assert_eq!(
        env_err(&t),
        format!(
            "Model validation error: 1 validation error for EnvironmentTemplate\nservices:\n\t{}",
            cycle_message("A -> B -> A")
        )
    );
}

#[test]
fn a_diamond_is_not_a_cycle() {
    // Top references Left and Right, both of which reference Bottom.
    let t = job(
        "",
        &format!(
            "{}{}{}{}",
            svc(
                "Top",
                r#"L: "{{ Service.Left.main.port }}", R: "{{ Service.Right.main.port }}""#
            ),
            svc("Left", r#"B: "{{ Service.Bottom.main.port }}""#),
            svc("Right", r#"B: "{{ Service.Bottom.main.port }}""#),
            svc("Bottom", "")
        ),
        &step("S", "", r#"["{{ Service.Top.main.port }}"]"#),
    );
    let jt = decode(&t);
    let scopes = compute_service_scopes(&jt).unwrap();
    assert_eq!(
        scopes
            .get("Top")
            .unwrap()
            .references
            .iter()
            .collect::<Vec<_>>(),
        ["Left", "Right"]
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
// §9 item 4 / §9.9 item 10 — <Service>.dependencies
// ════════════════════════════════════════════════════════════════════

/// Service `X` with `dependencies` body `deps` (indented YAML lines).
fn svc_with_deps(deps: &str) -> String {
    format!(
        "  - name: X\n    dependencies:\n{deps}    ports: [{{ name: main }}]\n    script:\n      actions:\n        onRun: {{ command: serve }}\n"
    )
}

const PREP_AND_RENDER: &str = "  - name: Prep\n    script: { actions: { onRun: { command: prep } } }\n  - name: Render\n    dependencies: [{ dependsOn: Prep }]\n    script: { actions: { onRun: { command: render, args: [\"{{ Service.X.main.port }}\"] } } }\n";

#[test]
fn dependencies_on_a_step_outside_the_scope_are_accepted_and_carried_into_the_job() {
    let t = job(
        "",
        &svc_with_deps("      - dependsOn: Prep\n"),
        PREP_AND_RENDER,
    );
    let jt = decode(&t);
    let deps = jt.services.as_ref().unwrap()[0]
        .dependencies
        .as_ref()
        .unwrap();
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].depends_on, "Prep");
    assert_eq!(scope_of(&jt, "X"), ServiceScope::steps(["Render"]));

    let job = create_ok(&t);
    let svc = &job.services.as_ref().unwrap()[0];
    assert_eq!(svc.scope, ServiceScope::steps(["Render"]));
    let deps = svc.dependencies.as_ref().expect("dependencies");
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].depends_on, "Prep");
    let json = serde_json::to_value(svc).unwrap();
    assert_eq!(
        json["dependencies"],
        serde_json::json!([{ "dependsOn": "Prep" }])
    );
    let back: job::Service = serde_json::from_value(json).unwrap();
    assert_eq!(&back, svc);
}

#[test]
fn dependencies_must_not_be_empty() {
    let t = job("", &svc_with_deps("      []\n"), PREP_AND_RENDER);
    // An empty flow list on its own line.
    let t = t.replace("    dependencies:\n      []\n", "    dependencies: []\n");
    assert_eq!(
        job_err(&t),
        "Model validation error: 1 validation error for JobTemplate\n\
         services[0] -> dependencies:\n\tmust not be empty."
    );
}

#[test]
fn dependencies_must_name_a_step_of_the_template() {
    let t = job(
        "",
        &svc_with_deps("      - dependsOn: Prep\n      - dependsOn: Nope\n"),
        PREP_AND_RENDER,
    );
    assert_eq!(
        job_err(&t),
        "Model validation error: 1 validation error for JobTemplate\n\
         services[0] -> dependencies[1] -> dependsOn:\n\treferences unknown Step 'Nope'."
    );
}

fn own_scope_message(step: &str, why: &str) -> String {
    format!(
        "Step '{step}' is in the scope of Service 'X' ({why}); a Service cannot depend on a Step \
         in its own scope, which could not run until the Service was READY."
    )
}

#[test]
fn dependency_on_a_step_in_scope_because_nothing_references_the_service() {
    let t = job(
        "",
        &svc_with_deps("      - dependsOn: Prep\n"),
        "  - name: Prep\n    script: { actions: { onRun: { command: prep } } }\n",
    );
    assert_eq!(
        job_err(&t),
        format!(
            "Model validation error: 1 validation error for JobTemplate\n\
             services[0] -> dependencies[0] -> dependsOn:\n\t{}",
            own_scope_message(
                "Prep",
                "nothing references the Service, so every Step is in its scope"
            )
        )
    );
}

#[test]
fn dependency_on_a_step_in_scope_because_a_job_environment_references_the_service() {
    let t = job(
        "jobEnvironments:\n  - name: Client\n    variables: { H: \"{{ Service.X.main.connectAddress }}\" }\n",
        &svc_with_deps("      - dependsOn: Prep\n"),
        PREP_AND_RENDER,
    );
    assert_eq!(
        job_err(&t),
        format!(
            "Model validation error: 1 validation error for JobTemplate\n\
             services[0] -> dependencies[0] -> dependsOn:\n\t{}",
            own_scope_message(
                "Prep",
                "a Job Environment references the Service, so every Step is in its scope"
            )
        )
    );
}

#[test]
fn dependency_on_a_step_that_references_the_service() {
    let t = job(
        "",
        &svc_with_deps("      - dependsOn: Render\n"),
        PREP_AND_RENDER,
    );
    assert_eq!(
        job_err(&t),
        format!(
            "Model validation error: 1 validation error for JobTemplate\n\
             services[0] -> dependencies[0] -> dependsOn:\n\t{}",
            own_scope_message("Render", "Step 'Render' references the Service")
        )
    );
}

#[test]
fn dependency_on_a_step_that_references_a_service_that_references_it() {
    // Render references Up; Up references X; X depends on Render.
    let t = job(
        "",
        &format!(
            "{}{}",
            svc_with_deps("      - dependsOn: Render\n"),
            svc("Up", r#"UP: "{{ Service.X.main.port }}""#)
        ),
        &PREP_AND_RENDER.replace("Service.X.main.port", "Service.Up.main.port"),
    );
    assert_eq!(
        job_err(&t),
        format!(
            "Model validation error: 1 validation error for JobTemplate\n\
             services[0] -> dependencies[0] -> dependsOn:\n\t{}",
            own_scope_message(
                "Render",
                "Step 'Render' references a Service that references 'X'"
            )
        )
    );
}

#[test]
fn dependencies_not_permitted_in_an_environment_template() {
    let t = format!(
        "specificationVersion: \"environment-2023-09\"\nextensions: [SERVICE, EXPR]\nservices:\n{}",
        svc_with_deps("      - dependsOn: Prep\n")
    );
    assert_eq!(
        env_err(&t),
        "Model validation error: 1 validation error for EnvironmentTemplate\n\
         services[0] -> dependencies:\n\tdependencies is not permitted on a Service in an \
         Environment Template: the document has no Steps."
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
    assert!(
        err.starts_with("Model validation error: 1 validation error for JobTemplate\n"),
        "{err}"
    );
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
/// of `onWrapTaskRun`, defining `hooks`.
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
    assert!(
        err.starts_with("Model validation error: 1 validation error for JobTemplate\n"),
        "{err}"
    );
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
