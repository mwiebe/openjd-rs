// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Integration tests for job creation of RFC 0009 Services: the `job::Service`
//! types `create_job` produces, resolution of the `<Service>.let` bindings
//! and the numeric `@fmtstring` fields (Template Schemas §9.2 "resolved at
//! job creation ... target type `int?`"), Service `hostRequirements`, the
//! carried-forward re-checks, `resolved_symtab`, and the job-side
//! `Environment.run_scope` / `onWrapService*` fields. Also the runtime-facing
//! `service_symbols` API that a Session uses to bind `Service.*`.
//!
//! Every Job Template here declares the dependency on each Service it uses:
//! a Step or Service that references `Service.X.*` lists `service:X` in its
//! `dependencies` (Template Schemas §9.1), and every inline Service is listed
//! by at least one Step or Service, since an unused one is rejected. The
//! scope and the job-side `depends_on_services()` follow those declarations.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::LazyLock;

use openjd_expr::{ExprValue, FormatString, SymbolTable};
use openjd_model::job::service_symbols::{
    add_wrapped_service_symbols, build_service_symbol_table, ServiceEndpoint, ServiceEndpoints,
};
use openjd_model::job::{
    self, CompletedTasksPolicy, RunScope, ServiceHealthCheck, ServicePortProtocol, ServiceScope,
};
use openjd_model::{
    create_job, decode_job_template, CallerLimits, JobParameterInputValues, ModelError,
};

const EXTS: &[&str] = &["EXPR", "SERVICE", "FEATURE_BUNDLE_1", "WRAP_ACTIONS"];

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

/// Adds `dependsOn: service:<service>` to the `dependencies` of the Step
/// named `step` (a top-level `  - name: <step>` entry), creating the list
/// when the Step has none. Unchanged when the template already lists it.
fn with_service_dependency(template: &str, step: &str, service: &str) -> String {
    let entry = format!("      - dependsOn: service:{service}\n");
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

fn decode(template: &str) -> openjd_model::template::JobTemplate {
    decode_job_template(yaml_val(template), Some(EXTS), &CallerLimits::default())
        .expect("template should validate")
}

fn create(template: &str, params: &[(&str, &str)]) -> Result<job::Job, ModelError> {
    let jt = decode(template);
    let td = tempfile::TempDir::new().unwrap();
    let dir = td.path().to_str().unwrap();
    let input: JobParameterInputValues = params
        .iter()
        .map(|(k, v)| (k.to_string(), ExprValue::String(v.to_string())))
        .collect();
    let processed = openjd_model::preprocess_job_parameters(
        &jt,
        &input,
        &[],
        &openjd_model::PathParameterOptions::new(dir, dir),
    )
    .unwrap();
    create_job(&jt, &processed, &jt.default_validation_context())
}

fn create_ok(template: &str, params: &[(&str, &str)]) -> job::Job {
    create(template, params).expect("job creation should succeed")
}

fn create_err(template: &str, params: &[(&str, &str)]) -> String {
    create(template, params)
        .expect_err("job creation should fail")
        .to_string()
}

fn resolve(fs: &FormatString, st: &SymbolTable) -> Result<String, String> {
    // Resolve with the library a SERVICE template's profile yields, since
    // the host and port functions exist only under that extension.
    let lib = openjd_expr::FunctionLibrary::for_profile(
        &openjd_expr::ExprProfile::current().with_extensions(std::collections::HashSet::from([
            openjd_expr::ExprExtension::Service,
        ])),
    );
    fs.resolve_string_with(
        st,
        &openjd_expr::FormatStringOptions::new().with_library(&*lib),
    )
    .map_err(|e| e.to_string())
}

fn symtab_of(st: &openjd_expr::SerializedSymbolTable) -> SymbolTable {
    st.to_symtab(openjd_expr::path_mapping::PathFormat::Posix)
        .unwrap()
}

// ════════════════════════════════════════════════════════════════════
// RFC "Basic Examples"
// ════════════════════════════════════════════════════════════════════

#[test]
fn valkey_example_creates_a_job_service_with_defaults_applied() {
    let job = create_ok(&RFC_VALKEY, &[("FrameEnd", "10")]);
    let services = job.services.as_ref().expect("services");
    assert_eq!(services.len(), 1);
    let cache = &services[0];
    assert_eq!(cache.name, "Cache");
    assert_eq!(
        cache.description.as_deref(),
        Some("A Valkey store the Job's Tasks share to cache and coordinate.")
    );
    // Ports: declared without a number — the runtime allocates.
    assert_eq!(cache.port_names().collect::<Vec<_>>(), vec!["main"]);
    assert_eq!(cache.ports[0].port, None);
    // Health check: TCP_CONNECT with no `ports` probes every declared
    // port; readinessIntervalSeconds takes its TCP_CONNECT default of 1.
    assert_eq!(
        cache.health_check,
        ServiceHealthCheck::TcpConnect {
            ports: vec!["main".to_string()],
            readiness_interval_seconds: 1,
            readiness_timeout_seconds: 60,
            health_interval_seconds: 10,
            failure_threshold: 3,
        }
    );
    assert_eq!(cache.health_check.readiness_interval_seconds(), Some(1));
    assert_eq!(cache.health_check.readiness_timeout_seconds(), 60);
    assert_eq!(cache.health_check.health_interval_seconds(), Some(10));
    assert_eq!(cache.health_check.failure_threshold(), 3);
    assert!(cache.health_check.monitors_health());
    assert_eq!(cache.restart_policy.max_attempts, 3);
    assert_eq!(
        cache.restart_policy.completed_tasks,
        CompletedTasksPolicy::Keep
    );
    // Host requirements are resolved.
    let hr = cache.host_requirements.as_ref().unwrap();
    let attrs = hr.attributes.as_ref().unwrap();
    assert_eq!(attrs[0].name, "attr.worker.preemptible");
    assert_eq!(attrs[0].any_of.as_deref(), Some(&["false".to_string()][..]));
    let amounts = hr.amounts.as_ref().unwrap();
    assert_eq!(amounts[0].name, "amount.worker.memory");
    assert_eq!(amounts[0].min, Some(8192.0));
    // The script is carried forward unresolved (host scope).
    let args = cache.script.actions.on_run.args.as_ref().unwrap();
    assert_eq!(args[1].raw(), "{{ Service.Cache.main.port }}");
    assert_eq!(args[3].raw(), "{{ Service.Cache.main.bindAddress }}");
    assert!(cache.variables.is_none());
    // The RFC's example provisions Valkey from Conda in onEnter (the
    // rejected `serviceEnvironments` list once held this).
    let on_enter = cache.script.actions.on_enter.as_ref().expect("onEnter");
    assert_eq!(on_enter.command.raw(), "bash");
    assert!(on_enter.args.as_ref().unwrap()[1]
        .raw()
        .starts_with("conda create -y -p ./valkey-env"));
    assert!(cache.script.actions.on_health_check.is_none());
    assert_eq!(
        cache
            .script
            .actions
            .iter_named()
            .map(|(n, _)| n)
            .collect::<Vec<_>>(),
        vec!["onEnter", "onRun"]
    );
    // The only Step lists `service:Cache`, so its scope is that Step (§9.1
    // item 1); the Service itself lists no dependencies.
    assert_eq!(cache.scope, ServiceScope::steps(["ProcessFrames"]));
    assert!(cache.depends_on_services().next().is_none());
    assert!(cache.dependencies.is_none());
    // The step script still references the Service, unresolved.
    let data = job.steps[0].script.embedded_files.as_ref().unwrap()[0]
        .data
        .as_ref()
        .unwrap();
    assert!(data
        .raw()
        .contains("{{ Service.Cache.main.connectAddress }}"));
}

#[test]
fn coordinator_example_creates_a_service_scoped_to_one_step() {
    let job = create_ok(&RFC_COORDINATOR, &[]);
    let services = job.services.as_ref().expect("services");
    assert_eq!(services.len(), 1);
    let c = &services[0];
    assert_eq!(c.name, "Coordinator");
    // Only RenderTiles lists `service:Coordinator` (§9.1 item 1);
    // PrepareScene, which RenderTiles also depends on, is not in its scope.
    assert_eq!(c.scope, ServiceScope::steps(["RenderTiles"]));
    assert!(!c.scope.contains("PrepareScene"));
    assert!(c.depends_on_services().next().is_none());
    // RenderTiles' one list names both a Step and the Service.
    assert!(job.service_active());
    let render = &job.steps[1];
    assert_eq!(render.name, "RenderTiles");
    let deps = render.dependencies.as_deref().unwrap();
    assert_eq!(
        deps.iter()
            .map(|d| (d.target(true).step(), d.target(true).service()))
            .collect::<Vec<_>>(),
        vec![(Some("PrepareScene"), None), (None, Some("Coordinator"))]
    );
    assert_eq!(c.port_names().collect::<Vec<_>>(), vec!["api", "metrics"]);
    // STDOUT without healthIntervalSeconds: no heartbeat; failureThreshold
    // takes its default but has nothing to count.
    assert_eq!(
        c.health_check,
        ServiceHealthCheck::Stdout {
            readiness_timeout_seconds: 120,
            health_interval_seconds: None,
            failure_threshold: 3,
        }
    );
    assert_eq!(c.health_check.type_name(), "STDOUT");
    assert_eq!(c.health_check.readiness_interval_seconds(), None);
    assert_eq!(c.health_check.readiness_timeout_seconds(), 120);
    assert_eq!(c.health_check.health_interval_seconds(), None);
    assert!(!c.health_check.monitors_health());
    assert_eq!(c.restart_policy.max_attempts, 1);
    assert_eq!(
        c.restart_policy.completed_tasks,
        CompletedTasksPolicy::Rerun
    );
    assert!(c.host_requirements.is_none());
    assert_eq!(
        c.script
            .actions
            .iter_named()
            .map(|(n, _)| n)
            .collect::<Vec<_>>(),
        vec!["onEnter", "onRun", "onExit"]
    );
    assert_eq!(c.script.actions.iter_actions().count(), 3);
    let on_run_args = c.script.actions.on_run.args.as_ref().unwrap();
    assert_eq!(
        on_run_args[4].raw(),
        "{{ join_host_port(Service.Coordinator.api.bindAddress, Service.Coordinator.api.port) }}"
    );
    // The resolved symtab carries nothing for this Service: every reference
    // is host-scope (Session.*, Service.*), bound by the Service Session.
    let st = symtab_of(c.resolved_symtab.as_ref().unwrap());
    assert!(
        st.keys().next().is_none(),
        "got {:?}",
        st.keys().collect::<Vec<_>>()
    );
}

// ════════════════════════════════════════════════════════════════════
// Resolution of job-creation-stage fields
// ════════════════════════════════════════════════════════════════════

const NUMERIC: &str = r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [SERVICE, EXPR, FEATURE_BUNDLE_1]
name: Test
parameterDefinitions:
  - { name: Port, type: INT, default: 6379 }
  - { name: Attempts, type: INT, default: 2 }
  - { name: Dir, type: PATH, default: tmp }
  - { name: Text, type: STRING, default: "6" }
  - { name: Amount, type: STRING, default: amount.worker.vcpu }
services:
  - name: Cache
    let:
      - metrics = Param.Port + 1000
      - label = Job.Name + '-cache'
    hostRequirements:
      amounts:
        - name: amount.worker.memory
          min: "{{ metrics }}"
        - name: "{{ Param.Amount }}"
          min: 1
    ports:
      - name: main
        port: "{{ Param.Port }}"
      - name: metrics
        port: "{{ metrics }}"
      - name: padded
        port: " 0080 "
      - name: unset
        port: "{{ null }}"
    healthCheck:
      type: COMMAND
      readinessIntervalSeconds: "{{ 2 * 3 }}"
      readinessTimeoutSeconds: "{{ null }}"
      healthIntervalSeconds: "{{ Param.Port // 1000 }}"
      failureThreshold: "{{ null }}"
    restartPolicy:
      maxAttempts: "{{ Param.Attempts }}"
    variables:
      LABEL: "{{ label }}"
      DIR: "{{ Param.Dir }}"
    script:
      let:
        - listen = join_host_port(Service.Cache.main.bindAddress, Service.Cache.main.port)
      actions:
        onRun:
          command: valkey-server
          args: ["{{ listen }}", "{{ label }}"]
          timeout: "{{ metrics }}"
        onHealthCheck:
          command: probe
  - name: Side
    dependencies:
      - dependsOn: service:Cache
    let:
      - tag = Job.Name + string(2)
    ports:
      - name: p
    restartPolicy:
      completedTasks: KEEP
    variables:
      TAG: "{{ tag }}"
      UP: "{{ Service.Cache.main.connectAddress }}"
    script:
      actions:
        onRun:
          command: side
steps:
  - name: S
    dependencies:
      - dependsOn: service:Side
    let:
      - n = 2
    script:
      actions:
        onRun:
          command: run
          args: ["{{ Service.Side.p.port }}"]
"#;

#[test]
fn numeric_fields_resolve_in_the_service_let_scope() {
    let job = create_ok(NUMERIC, &[("Port", "7000")]);
    let cache = &job.services.as_ref().unwrap()[0];
    let ports: Vec<(&str, Option<u16>)> = cache
        .ports
        .iter()
        .map(|p| (p.name.as_str(), p.port))
        .collect();
    assert_eq!(
        ports,
        vec![
            ("main", Some(7000)),
            ("metrics", Some(8000)),
            ("padded", Some(80)),
            ("unset", None),
        ]
    );
    // A `null` whole-field expression means "not provided": the §9.3
    // default applies (readinessTimeoutSeconds 300, failureThreshold 3).
    assert_eq!(
        cache.health_check,
        ServiceHealthCheck::Command {
            readiness_interval_seconds: 6,
            readiness_timeout_seconds: 300,
            health_interval_seconds: 7,
            failure_threshold: 3,
        }
    );
    assert_eq!(cache.restart_policy.max_attempts, 2);
    assert_eq!(
        cache.restart_policy.completed_tasks,
        CompletedTasksPolicy::Rerun
    );
    let amounts = cache
        .host_requirements
        .as_ref()
        .unwrap()
        .amounts
        .as_ref()
        .unwrap();
    assert_eq!(amounts[0].min, Some(8000.0));
    assert_eq!(amounts[1].name, "amount.worker.vcpu");
    // Host-scope fields are carried forward verbatim.
    assert_eq!(
        cache.variables.as_ref().unwrap()["LABEL"].raw(),
        "{{ label }}"
    );
    assert_eq!(
        cache.script.let_bindings.as_deref(),
        Some(
            &[
                "listen = join_host_port(Service.Cache.main.bindAddress, Service.Cache.main.port)"
                    .to_string()
            ][..]
        )
    );
    // `timeout` stays a format string for the runtime, like a Step's.
    assert_eq!(
        cache.script.actions.on_run.timeout.as_ref().unwrap().raw(),
        "{{ metrics }}"
    );
}

#[test]
fn service_resolved_symtab_carries_let_values_and_raw_param_fallbacks() {
    let job = create_ok(NUMERIC, &[]);
    let cache = &job.services.as_ref().unwrap()[0];
    let st = symtab_of(cache.resolved_symtab.as_ref().unwrap());
    // `label` and `metrics` are referenced by host-resolved fields.
    assert_eq!(
        st.get_value("label"),
        Some(&ExprValue::String("Test-cache".to_string()))
    );
    assert_eq!(st.get_value("metrics"), Some(&ExprValue::Int(7379)));
    // PATH Param.Dir is host-only: its RawParam is transported instead.
    assert!(st.get_value("Param.Dir").is_none());
    assert!(st.get_value("RawParam.Dir").is_some());
    // Unreferenced symbols are filtered out.
    assert!(st.get_value("Param.Port").is_none());
    assert!(st.get_value("Job.Name").is_none());

    let side = &job.services.as_ref().unwrap()[1];
    let st = symtab_of(side.resolved_symtab.as_ref().unwrap());
    assert_eq!(
        st.get_value("tag"),
        Some(&ExprValue::String("Test2".to_string()))
    );
    assert_eq!(side.restart_policy.max_attempts, 0);
    assert_eq!(
        side.restart_policy.completed_tasks,
        CompletedTasksPolicy::Keep
    );
    assert_eq!(
        side.health_check,
        ServiceHealthCheck::TcpConnect {
            ports: vec!["p".to_string()],
            readiness_interval_seconds: 1,
            readiness_timeout_seconds: 300,
            health_interval_seconds: 30,
            failure_threshold: 3,
        }
    );
}

#[test]
fn numeric_fields_are_range_checked_at_job_creation() {
    let err = create_err(NUMERIC, &[("Port", "70000")]);
    assert_eq!(
        err,
        "Model validation error: 1 validation error for JobTemplate\nservices[0] -> ports[0] -> port:\n\tmust be between 1 and 65535."
    );
    let err = create_err(NUMERIC, &[("Attempts", "-1")]);
    assert_eq!(
        err,
        "Model validation error: 1 validation error for JobTemplate\nservices[0] -> restartPolicy -> maxAttempts:\n\tmust be >= 0."
    );
}

#[test]
fn numeric_field_that_does_not_resolve_to_an_integer_fails() {
    // Multi-segment: concatenates to text and must parse.
    let tmpl = NUMERIC.replace(
        "readinessIntervalSeconds: \"{{ 2 * 3 }}\"",
        "readinessIntervalSeconds: \"{{ Param.Text }}x\"",
    );
    let err = create_err(&tmpl, &[]);
    assert_eq!(
        err,
        "Model validation error: 1 validation error for JobTemplate\nservices[0] -> healthCheck -> readinessIntervalSeconds:\n\tmust be an integer."
    );
    // Whole-field non-int against the `int?` target is a resolution error.
    let tmpl = NUMERIC.replace(
        "readinessIntervalSeconds: \"{{ 2 * 3 }}\"",
        "readinessIntervalSeconds: \"{{ Param.Text }}\"",
    );
    let err = create_err(&tmpl, &[("Text", "soon")]);
    assert!(
        err.starts_with(
            "Format string error: services[0] -> healthCheck -> readinessIntervalSeconds: "
        ),
        "got: {err}"
    );
}

/// §9.3 (RFC 0009): all four numeric `<ServiceHealthCheck>` fields are
/// `@fmtstring`, resolved in the Service's job-creation scope (`Param.*` and
/// `<Service>.let`) and range-checked (`<posinteger>`) there — the
/// `9.3--health-numeric-fields-from-param` fixture, per form.
#[test]
fn health_check_numeric_fields_resolve_from_params_and_lets() {
    fn template(health: &str, extra_actions: &str) -> String {
        format!(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [SERVICE, EXPR]
name: T
parameterDefinitions:
  - name: Patience
    type: INT
    default: 60
services:
  - name: Store
    let:
      - strikes = 3 if Param.Patience > 30 else 1
    ports:
      - name: main
    healthCheck:
{health}    script:
      actions:
        onRun:
          command: run
{extra_actions}steps:
  - name: S
    dependencies:
      - dependsOn: service:Store
    script:
      actions:
        onRun:
          command: run
"#
        )
    }
    const ON_HEALTH_CHECK: &str = "        onHealthCheck:\n          command: probe\n";
    const FOUR: &str = "      readinessIntervalSeconds: \"{{ Param.Patience // 10 }}\"\n      \
                        readinessTimeoutSeconds: \"{{ Param.Patience }}\"\n      \
                        healthIntervalSeconds: \"{{ Param.Patience // 2 }}\"\n      \
                        failureThreshold: \"{{ strikes }}\"\n";
    const THREE: &str = "      readinessTimeoutSeconds: \"{{ Param.Patience }}\"\n      \
                         healthIntervalSeconds: \"{{ Param.Patience // 2 }}\"\n      \
                         failureThreshold: \"{{ strikes }}\"\n";

    let job = create_ok(
        &template(&format!("      type: COMMAND\n{FOUR}"), ON_HEALTH_CHECK),
        &[],
    );
    assert_eq!(
        job.services.as_ref().unwrap()[0].health_check,
        ServiceHealthCheck::Command {
            readiness_interval_seconds: 6,
            readiness_timeout_seconds: 60,
            health_interval_seconds: 30,
            failure_threshold: 3,
        }
    );
    // With Patience 20: strikes resolves to 1.
    let job = create_ok(
        &template(&format!("      type: TCP_CONNECT\n{FOUR}"), ""),
        &[("Patience", "20")],
    );
    assert_eq!(
        job.services.as_ref().unwrap()[0].health_check,
        ServiceHealthCheck::TcpConnect {
            ports: vec!["main".to_string()],
            readiness_interval_seconds: 2,
            readiness_timeout_seconds: 20,
            health_interval_seconds: 10,
            failure_threshold: 1,
        }
    );
    let job = create_ok(&template(&format!("      type: STDOUT\n{THREE}"), ""), &[]);
    assert_eq!(
        job.services.as_ref().unwrap()[0].health_check,
        ServiceHealthCheck::Stdout {
            readiness_timeout_seconds: 60,
            health_interval_seconds: Some(30),
            failure_threshold: 3,
        }
    );
    // A STDOUT heartbeat interval that resolves to null means no heartbeat.
    let job = create_ok(
        &template(
            "      type: STDOUT\n      healthIntervalSeconds: \"{{ null }}\"\n",
            "",
        ),
        &[],
    );
    assert_eq!(
        job.services.as_ref().unwrap()[0].health_check,
        ServiceHealthCheck::Stdout {
            readiness_timeout_seconds: 300,
            health_interval_seconds: None,
            failure_threshold: 3,
        }
    );
    // Each field is range-checked once resolved (Patience 5: 5 // 10 is 0).
    for (field, bad) in [
        ("readinessIntervalSeconds", "{{ Param.Patience // 10 }}"),
        ("readinessTimeoutSeconds", "{{ Param.Patience - 5 }}"),
        ("healthIntervalSeconds", "{{ -Param.Patience }}"),
        ("failureThreshold", "{{ Param.Patience // 10 }}"),
    ] {
        let err = create_err(
            &template(
                &format!("      type: COMMAND\n      {field}: \"{bad}\"\n"),
                ON_HEALTH_CHECK,
            ),
            &[("Patience", "5")],
        );
        assert_eq!(
            err,
            format!(
                "Model validation error: 1 validation error for JobTemplate\nservices[0] -> \
                 healthCheck -> {field}:\n\tmust be > 0."
            )
        );
    }
    let err = create_err(
        &template(
            "      type: STDOUT\n      healthIntervalSeconds: \"{{ Param.Patience - 60 }}\"\n",
            "",
        ),
        &[],
    );
    assert_eq!(
        err,
        "Model validation error: 1 validation error for JobTemplate\nservices[0] -> \
         healthCheck -> healthIntervalSeconds:\n\tmust be > 0."
    );
}

#[test]
fn service_let_failure_names_the_binding() {
    let tmpl = NUMERIC.replace(
        "- metrics = Param.Port + 1000",
        "- metrics = 1 // Param.Attempts",
    );
    let err = create_err(&tmpl, &[("Attempts", "0")]);
    assert!(
        err.starts_with("Expression error: service let binding 'metrics': "),
        "got: {err}"
    );
}

#[test]
fn service_host_requirements_errors_carry_the_service_path() {
    let err = create_err(NUMERIC, &[("Amount", "amount.worker.memory")]);
    assert_eq!(
        err,
        "Model validation error: 1 validation error for JobTemplate\nservices[0] -> hostRequirements -> amounts[1]:\n\tduplicate amount name 'amount.worker.memory'."
    );
    let tmpl = NUMERIC.replace(
        "        - name: \"{{ Param.Amount }}\"\n          min: 1",
        "      attributes:\n        - name: attr.worker.os.family\n          anyOf: [\"{{ Param.Text }}\"]",
    );
    let err = create_err(&tmpl, &[("Text", "beos")]);
    assert!(
        err.starts_with(
            "Model validation error: 1 validation error for JobTemplate\nservices[0] -> hostRequirements -> attributes[0] -> anyOf[0]:\n\t"
        ),
        "got: {err}"
    );
}

#[test]
fn second_service_errors_carry_its_index() {
    let tmpl = NUMERIC.replace(
        "    ports:\n      - name: p\n",
        "    ports:\n      - name: p\n        port: \"{{ Param.Port - 6379 }}\"\n",
    );
    let err = create_err(&tmpl, &[]);
    assert_eq!(
        err,
        "Model validation error: 1 validation error for JobTemplate\nservices[1] -> ports[0] -> port:\n\tmust be between 1 and 65535."
    );
}

/// The NUMERIC template's scopes: Step `S` lists `service:Side`, so it is in
/// `Side`'s scope (§9.1 item 1); `Side` lists `service:Cache`, so `Side`'s
/// scope is inside `Cache`'s (§9.1 item 2) and both are scoped to `S`. The
/// job-side `Service` keeps the declared edge: `Side` depends on `Cache`, and
/// neither depends on a Step.
#[test]
fn numeric_template_scopes_follow_the_declared_dependencies() {
    let job = create_ok(NUMERIC, &[]);
    let services = job.services.as_ref().unwrap();
    assert_eq!(services[0].name, "Cache");
    assert_eq!(services[0].scope, ServiceScope::steps(["S"]));
    assert!(services[0].depends_on_services().next().is_none());
    assert!(services[0].depends_on_steps().next().is_none());
    assert!(services[0].dependencies.is_none());
    assert_eq!(services[1].name, "Side");
    assert_eq!(services[1].scope, ServiceScope::steps(["S"]));
    assert_eq!(
        services[1].depends_on_services().collect::<Vec<_>>(),
        vec!["Cache"]
    );
    assert!(services[1].depends_on_service("Cache"));
    assert!(!services[1].depends_on_service("Side"));
    assert!(services[1].depends_on_steps().next().is_none());
    // The Step's own dependency is carried as written.
    assert_eq!(
        job.steps[0]
            .dependencies
            .as_deref()
            .unwrap()
            .iter()
            .map(|d| d.depends_on.as_str())
            .collect::<Vec<_>>(),
        vec!["service:Side"]
    );
}

/// A Service may list a Step and a Service in one `dependencies` list
/// (§9 item 4). `Use` lists `service:Front`, so `Use` is in `Front`'s scope;
/// `Front` lists `service:Back`, so `Back`'s scope contains `Front`'s
/// (§9.1 item 2). `Front` also lists the Step `Prepare`, which is not in
/// either scope: it must complete before `Front` starts.
#[test]
fn service_dependencies_on_steps_and_services_are_carried_into_the_job() {
    let tmpl = r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [SERVICE, EXPR]
name: Test
services:
  - name: Back
    ports: [{ name: main }]
    script: { actions: { onRun: { command: back } } }
  - name: Front
    dependencies:
      - dependsOn: Prepare
      - dependsOn: service:Back
    ports: [{ name: main }]
    script:
      actions:
        onRun: { command: front, args: ["{{ Service.Back.main.connectAddress }}"] }
steps:
  - name: Prepare
    script: { actions: { onRun: { command: prepare } } }
  - name: Use
    dependencies:
      - dependsOn: service:Front
    script:
      actions:
        onRun: { command: use, args: ["{{ Service.Front.main.port }}"] }
"#;
    let job = create_ok(tmpl, &[]);
    let services = job.services.as_ref().unwrap();
    let (back, front) = (&services[0], &services[1]);
    assert_eq!(back.scope, ServiceScope::steps(["Use"]));
    assert_eq!(front.scope, ServiceScope::steps(["Use"]));
    assert!(!front.scope.contains("Prepare"));
    assert_eq!(
        front.depends_on_steps().collect::<Vec<_>>(),
        vec!["Prepare"]
    );
    assert_eq!(
        front.depends_on_services().collect::<Vec<_>>(),
        vec!["Back"]
    );
    assert!(back.dependencies.is_none());
    let json = serde_json::to_value(front).unwrap();
    assert_eq!(
        json["dependencies"],
        serde_json::json!([{ "dependsOn": "Prepare" }, { "dependsOn": "service:Back" }])
    );
    let back_again: job::Service = serde_json::from_value(json).unwrap();
    assert_eq!(&back_again, front);
}

#[test]
fn step_host_requirements_errors_keep_their_step_path() {
    // The shared resolver now takes an owner path; the Step path is unchanged.
    let tmpl = NUMERIC.replace(
        "  - name: S\n    dependencies:",
        "  - name: S\n    hostRequirements:\n      amounts:\n        - name: amount.worker.vcpu\n          min: \"{{ null }}\"\n    dependencies:",
    );
    let err = create_err(&tmpl, &[]);
    assert_eq!(
        err,
        "Model validation error: 1 validation error for JobTemplate\nsteps[0] -> hostRequirements -> amounts[0]:\n\tmust have at least one of min or max after resolution."
    );
}

// ════════════════════════════════════════════════════════════════════
// Carried-forward re-checks with parameters bound
// ════════════════════════════════════════════════════════════════════

#[test]
fn service_variables_rechecked_against_env_var_limit_at_job_creation() {
    let tmpl = r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [SERVICE, EXPR]
name: Test
parameterDefinitions:
  - { name: Count, type: INT, default: 1 }
services:
  - name: A
    ports: [{ name: p }]
    variables:
      BIG: "{{ 'x' * Param.Count }}"
    script:
      actions:
        onRun:
          command: a
steps:
  - name: S
    dependencies:
      - dependsOn: service:A
    script:
      actions:
        onRun:
          command: run
"#;
    create_ok(tmpl, &[("Count", "2048")]);
    let err = create_err(tmpl, &[("Count", "2049")]);
    assert_eq!(
        err,
        "Model validation error: 1 validation error for JobTemplate\nservices[0] -> variables -> BIG:\n\tresolves to at least 2049 characters, exceeding the maximum of 2048."
    );
}

#[test]
fn service_script_let_evaluated_with_parameters_bound() {
    let tmpl = r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [SERVICE, EXPR]
name: Test
parameterDefinitions:
  - { name: D, type: INT, default: 1 }
services:
  - name: A
    ports: [{ name: p }]
    script:
      let:
        - x = 10 // Param.D
      actions:
        onRun:
          command: a
          args: ["{{ x }}"]
steps:
  - name: S
    dependencies:
      - dependsOn: service:A
    script:
      actions:
        onRun:
          command: run
"#;
    create_ok(tmpl, &[("D", "2")]);
    let err = create_err(tmpl, &[("D", "0")]);
    assert!(
        err.starts_with("Expression error: script let binding 'x': "),
        "got: {err}"
    );
}

// ════════════════════════════════════════════════════════════════════
// Environments: run_scope and Service hooks on the job side
// ════════════════════════════════════════════════════════════════════

#[test]
fn environment_run_scope_and_service_hooks_are_carried_into_the_job() {
    let tmpl = r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [SERVICE, EXPR, WRAP_ACTIONS]
name: Test
jobEnvironments:
  - name: Client
    dependencies: [{ dependsOn: "service:A" }]
    runScope: [TASK]
    variables:
      HOST: "{{ Service.A.p.connectAddress }}"
  - name: Wrapper
    script:
      actions:
        onEnter: { command: echo }
        onWrapEnvEnter: { command: echo }
        onWrapTaskRun: { command: echo }
        onWrapEnvExit: { command: echo }
        onWrapServiceEnter: { command: echo, args: ["{{ WrappedService.Name }}"] }
        onWrapServiceRun: { command: echo }
        onWrapServiceHealthCheck: { command: echo }
        onWrapServiceExit: { command: echo }
services:
  - name: A
    ports: [{ name: p }]
    script:
      actions:
        onRun:
          command: a
steps:
  - name: S
    stepEnvironments:
      - name: SvcOnly
        runScope: [SERVICE]
        variables: { K: v }
    script:
      actions:
        onRun:
          command: run
"#;
    let job = create_ok(tmpl, &[]);
    let envs = job.job_environments.as_ref().unwrap();
    assert_eq!(envs[0].run_scope, Some(vec![RunScope::Task]));
    assert!(envs[0].runs_in(RunScope::Task));
    assert!(!envs[0].runs_in(RunScope::Service));
    assert_eq!(envs[0].depends_on_services().collect::<Vec<_>>(), ["A"]);
    assert_eq!(envs[1].run_scope, None);
    assert!(envs[1].dependencies.is_none());
    assert!(envs[1].runs_in(RunScope::Service));
    let actions = &envs[1].script.as_ref().unwrap().actions;
    assert!(actions.has_any_service_wrap_hook());
    assert!(actions.has_any_wrap_hook());
    assert_eq!(actions.named_slots().len(), 9);
    assert_eq!(actions.wrap_hooks().len(), 7);
    assert_eq!(
        actions
            .service_wrap_hooks()
            .iter()
            .map(|(n, _)| *n)
            .collect::<Vec<_>>(),
        vec![
            "onWrapServiceEnter",
            "onWrapServiceRun",
            "onWrapServiceHealthCheck",
            "onWrapServiceExit"
        ]
    );
    assert_eq!(
        actions
            .on_wrap_service_enter
            .as_ref()
            .unwrap()
            .args
            .as_ref()
            .unwrap()[0]
            .raw(),
        "{{ WrappedService.Name }}"
    );
    let step_env = &job.steps[0].step_environments.as_ref().unwrap()[0];
    assert_eq!(step_env.run_scope, Some(vec![RunScope::Service]));

    // Round-trips through the job wire format.
    let json = serde_json::to_value(&envs[1]).unwrap();
    assert_eq!(json.get("runScope"), None);
    assert!(json["script"]["actions"]["onWrapServiceRun"].is_object());
    let back: job::Environment = serde_json::from_value(json).unwrap();
    assert_eq!(&back, &envs[1]);
    let json = serde_json::to_value(&envs[0]).unwrap();
    assert_eq!(json["runScope"], serde_json::json!(["TASK"]));
    let back: job::Environment = serde_json::from_value(json).unwrap();
    assert_eq!(&back, &envs[0]);
}

#[test]
fn environment_without_service_fields_serializes_as_before() {
    let actions = job::EnvironmentActions {
        on_enter: None,
        on_wrap_env_enter: None,
        on_wrap_task_run: None,
        on_wrap_env_exit: None,
        on_wrap_service_enter: None,
        on_wrap_service_run: None,
        on_wrap_service_health_check: None,
        on_wrap_service_exit: None,
        on_exit: None,
    };
    let json = serde_json::to_value(&actions).unwrap();
    assert_eq!(json, serde_json::json!({ "onEnter": null, "onExit": null }));
    // Older documents without the new keys deserialize.
    let back: job::EnvironmentActions =
        serde_json::from_value(serde_json::json!({ "onEnter": null, "onExit": null })).unwrap();
    assert_eq!(back, actions);
    let env: job::Environment = serde_json::from_value(serde_json::json!({
        "name": "E", "description": null, "script": null, "variables": null
    }))
    .unwrap();
    assert_eq!(env.run_scope, None);
}

// ════════════════════════════════════════════════════════════════════
// job::Service serde, equality and hashing
// ════════════════════════════════════════════════════════════════════

fn hash_of<T: Hash>(v: &T) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

#[test]
fn job_with_services_round_trips_eq_and_hash() {
    let job = create_ok(NUMERIC, &[]);
    let json = serde_json::to_value(&job).unwrap();
    let svc = &json["services"][0];
    assert_eq!(svc["name"], "Cache");
    assert_eq!(
        svc["ports"][0],
        serde_json::json!({ "name": "main", "port": 6379 })
    );
    assert_eq!(
        svc["ports"][3],
        serde_json::json!({ "name": "unset", "port": null })
    );
    assert_eq!(
        svc["healthCheck"],
        serde_json::json!({
            "type": "COMMAND",
            "readinessIntervalSeconds": 6,
            "readinessTimeoutSeconds": 300,
            "healthIntervalSeconds": 6,
            "failureThreshold": 3
        })
    );
    assert_eq!(
        svc["restartPolicy"],
        serde_json::json!({ "maxAttempts": 2, "completedTasks": "RERUN" })
    );
    assert_eq!(
        svc["script"]["let"][0],
        "listen = join_host_port(Service.Cache.main.bindAddress, Service.Cache.main.port)"
    );
    assert!(svc["resolvedSymTab"].is_array());
    let back: job::Service = serde_json::from_value(svc.clone()).unwrap();
    assert_eq!(&back, &job.services.as_ref().unwrap()[0]);
    assert_eq!(hash_of(&back), hash_of(&job.services.as_ref().unwrap()[0]));
    assert_eq!(
        svc["scope"],
        serde_json::json!({ "kind": "steps", "steps": ["S"] })
    );
    // `Cache` lists no dependencies: the key is omitted. There is no
    // `references` key at all any more; a Service's dependencies are the
    // ones it declares.
    assert!(svc.get("references").is_none(), "got {svc}");
    assert!(svc.get("dependencies").is_none(), "got {svc}");
    let side_json = &json["services"][1];
    assert!(side_json.get("references").is_none(), "got {side_json}");
    assert_eq!(
        side_json["dependencies"],
        serde_json::json!([{ "dependsOn": "service:Cache" }])
    );
    let side_back: job::Service = serde_json::from_value(side_json.clone()).unwrap();
    assert_eq!(&side_back, &job.services.as_ref().unwrap()[1]);

    // Services created twice from the same inputs are equal and hash
    // equal; a different parameter value makes them differ. (The PATH
    // parameter resolves against a fresh directory per call, so the `Side`
    // Service — which does not reference it — is the stable comparison.)
    let again = create_ok(NUMERIC, &[]);
    let side = |j: &job::Job| j.services.as_ref().unwrap()[1].clone();
    assert_eq!(side(&job), side(&again));
    assert_eq!(hash_of(&side(&job)), hash_of(&side(&again)));
    let other = create_ok(NUMERIC, &[("Port", "7000")]);
    assert_ne!(
        job.services.as_ref().unwrap()[0].ports,
        other.services.as_ref().unwrap()[0].ports
    );

    // `variables` equality is order-insensitive.
    let svc = &job.services.as_ref().unwrap()[0];
    let mut reordered = svc.clone();
    let mut pairs: Vec<(String, FormatString)> = svc
        .variables
        .as_ref()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    pairs.reverse();
    let vars: HashMap<String, FormatString> = pairs.into_iter().collect();
    reordered.variables = Some(vars);
    assert_eq!(&reordered, svc);
    assert_eq!(hash_of(&reordered), hash_of(svc));
}

#[test]
fn job_without_services_omits_the_keys() {
    let tmpl = r#"
specificationVersion: "jobtemplate-2023-09"
name: Test
steps:
  - name: S
    script:
      actions:
        onRun:
          command: run
"#;
    let job = create_ok(tmpl, &[]);
    assert!(job.services.is_none());
    let json = serde_json::to_value(&job).unwrap();
    assert!(json.get("services").is_none());
    assert!(json.get("requiresServices").is_none());
    let step: job::Step = serde_json::from_value(json["steps"][0].clone()).unwrap();
    assert_eq!(step, job.steps[0]);
}

// ════════════════════════════════════════════════════════════════════
// <ServicePort>.protocol — §9.2 item 3, §9 items 6.4 and 7, §9.7 item 8
// ════════════════════════════════════════════════════════════════════

/// A metrics sink with a UDP `ingest` port and a TCP `api` port, both
/// numbered by Job Parameters, with the health check `health` (a YAML
/// body indented four spaces; empty = omitted).
fn protocol_template(health: &str) -> String {
    format!(
        r#"
specificationVersion: "jobtemplate-2023-09"
extensions: [SERVICE, EXPR]
name: Test
parameterDefinitions:
  - {{ name: Ingest, type: INT, default: 8125 }}
  - {{ name: Api, type: INT, default: 8080 }}
services:
  - name: Metrics
    ports:
      - name: ingest
        port: "{{{{ Param.Ingest }}}}"
        protocol: UDP
      - name: api
        port: "{{{{ Param.Api }}}}"
{health}    script:
      actions:
        onRun:
          command: sink
steps:
  - name: S
    dependencies:
      - dependsOn: service:Metrics
    script:
      actions:
        onRun:
          command: run
"#
    )
}

#[test]
fn port_protocol_is_carried_into_the_job_and_tcp_is_omitted_from_json() {
    let job = create_ok(&protocol_template(""), &[]);
    let svc = &job.services.as_ref().unwrap()[0];
    assert_eq!(
        svc.ports,
        vec![
            job::ServicePort {
                name: "ingest".into(),
                port: Some(8125),
                protocol: ServicePortProtocol::Udp,
            },
            job::ServicePort {
                name: "api".into(),
                port: Some(8080),
                protocol: ServicePortProtocol::Tcp,
            },
        ]
    );
    let json = serde_json::to_value(svc).unwrap();
    assert_eq!(json["ports"][0]["protocol"], "UDP");
    assert!(
        json["ports"][1].get("protocol").is_none(),
        "TCP is the default and is not serialized: {json}"
    );
    // A Job serialized before `protocol` existed still deserializes.
    let legacy: job::ServicePort = serde_json::from_str(r#"{"name": "main", "port": 80}"#).unwrap();
    assert_eq!(legacy.protocol, ServicePortProtocol::Tcp);
    let back: job::Service = serde_json::from_value(json).unwrap();
    assert_eq!(&back, svc);
}

#[test]
fn default_tcp_connect_probes_only_the_tcp_ports() {
    let job = create_ok(&protocol_template(""), &[]);
    let svc = &job.services.as_ref().unwrap()[0];
    assert_eq!(
        svc.health_check,
        ServiceHealthCheck::TcpConnect {
            ports: vec!["api".to_string()],
            readiness_interval_seconds: 1,
            readiness_timeout_seconds: 300,
            health_interval_seconds: 30,
            failure_threshold: 3,
        }
    );
    // An explicit TCP_CONNECT without `ports` defaults the same way.
    let job = create_ok(
        &protocol_template(
            "    healthCheck:\n      type: TCP_CONNECT\n      readinessTimeoutSeconds: 7\n",
        ),
        &[],
    );
    assert_eq!(
        job.services.as_ref().unwrap()[0].health_check,
        ServiceHealthCheck::TcpConnect {
            ports: vec!["api".to_string()],
            readiness_interval_seconds: 1,
            readiness_timeout_seconds: 7,
            health_interval_seconds: 30,
            failure_threshold: 3,
        }
    );
}

#[test]
fn format_string_port_numbers_are_checked_for_duplicates_at_job_creation() {
    // Same number across protocols is allowed (DNS-style).
    let job = create_ok(
        &protocol_template(""),
        &[("Ingest", "5353"), ("Api", "5353")],
    );
    let ports: Vec<Option<u16>> = job.services.as_ref().unwrap()[0]
        .ports
        .iter()
        .map(|p| p.port)
        .collect();
    assert_eq!(ports, vec![Some(5353), Some(5353)]);

    // Same number in one protocol's space is rejected once resolved.
    let tmpl = protocol_template("").replace("        protocol: UDP\n", "");
    let err = create_err(&tmpl, &[("Ingest", "6379"), ("Api", "6379")]);
    assert_eq!(
        err,
        "Model validation error: 1 validation error for JobTemplate\nservices[0] -> ports[1] -> port:\n\tTCP port 6379 is also used by port 'ingest'; two ports with the same protocol must not have the same port number."
    );
    // One literal and one format string are compared too.
    let tmpl = protocol_template("    healthCheck:\n      type: STDOUT\n").replace(
        "port: \"{{ Param.Ingest }}\"\n        protocol: UDP",
        "port: 9000\n        protocol: UDP",
    );
    let tmpl = tmpl.replace(
        "port: \"{{ Param.Api }}\"",
        "port: \"{{ Param.Api }}\"\n        protocol: UDP",
    );
    let err = create_err(&tmpl, &[("Api", "9000")]);
    assert_eq!(
        err,
        "Model validation error: 1 validation error for JobTemplate\nservices[0] -> ports[1] -> port:\n\tUDP port 9000 is also used by port 'ingest'; two ports with the same protocol must not have the same port number."
    );
}

// ════════════════════════════════════════════════════════════════════
// Runtime-facing symbol table construction
// ════════════════════════════════════════════════════════════════════

#[test]
fn service_symbol_table_resolves_the_rfc_examples_format_strings() {
    let job = create_ok(&RFC_VALKEY, &[("FrameEnd", "1")]);
    let cache = &job.services.as_ref().unwrap()[0];
    let endpoints = ServiceEndpoints::new(
        cache.name.clone(),
        cache
            .port_names()
            .map(|p| {
                (
                    p.to_string(),
                    ServiceEndpoint {
                        port: 6379,
                        protocol: ServicePortProtocol::Tcp,
                        bind_address: "0.0.0.0".to_string(),
                        connect_address: "cache.farm.example".to_string(),
                    },
                )
            })
            .collect(),
    );
    // The Service's own Session: bindAddress in scope.
    let own = build_service_symbol_table(&[], Some(&endpoints)).unwrap();
    let args = cache.script.actions.on_run.args.as_ref().unwrap();
    assert_eq!(resolve(&args[1], &own).unwrap(), "6379");
    assert_eq!(resolve(&args[3], &own).unwrap(), "0.0.0.0");
    // A Task Session of the Job: port and connectAddress only.
    let task = build_service_symbol_table(std::slice::from_ref(&endpoints), None).unwrap();
    let data = job.steps[0].script.embedded_files.as_ref().unwrap()[0]
        .data
        .as_ref()
        .unwrap();
    let mut st = task.clone();
    st.set("Task.Param.Frame", ExprValue::Int(1)).unwrap();
    let text = resolve(data, &st).unwrap();
    assert!(text.contains("--valkey-host 'cache.farm.example'"));
    assert!(text.contains("--valkey-port 6379"));
    assert!(
        resolve(&args[3], &task).is_err(),
        "bindAddress must be out of scope"
    );

    // The WrappedService.* group for a wrap hook in the Service's Session.
    let mut wrapped = SymbolTable::new();
    add_wrapped_service_symbols(&mut wrapped, &endpoints).unwrap();
    let fs = FormatString::new(
        "{{ flatten([['-p', string(p) + ':' + string(p)] for p in WrappedService.Ports]) }}",
    )
    .unwrap();
    assert_eq!(resolve(&fs, &wrapped).unwrap(), r#"["-p", "6379:6379"]"#);
    let fs = FormatString::new(
        "{{ join_host_port(WrappedService.BindAddresses[0], WrappedService.Ports[0]) }}",
    )
    .unwrap();
    assert_eq!(resolve(&fs, &wrapped).unwrap(), "0.0.0.0:6379");
    assert_eq!(
        resolve(
            &FormatString::new("{{ WrappedService.Name }}/{{ WrappedService.PortNames[0] }}")
                .unwrap(),
            &wrapped
        )
        .unwrap(),
        "Cache/main"
    );
    // RFC 0009 §4.3.1: the Docker `-p` list with the protocol of each port.
    let fs = FormatString::new(
        "{{ flatten([['-p', string(WrappedService.Ports[i]) + ':' + string(WrappedService.Ports[i]) + '/' + lower(WrappedService.Protocols[i])] for i in range(len(WrappedService.Ports))]) }}",
    )
    .unwrap();
    assert_eq!(
        resolve(&fs, &wrapped).unwrap(),
        r#"["-p", "6379:6379/tcp"]"#
    );
}
