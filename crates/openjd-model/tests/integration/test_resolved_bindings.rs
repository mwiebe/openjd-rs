// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Tests for resolved_symtab serialization/deserialization on job::Step,
//! verifying compatibility with the Python library's JSON transport format.

use openjd_expr::path_mapping::PathFormat;
use openjd_model::CallerLimits;
use openjd_model::{create_job, decode_job_template, preprocess_job_parameters};

struct TestDirs {
    _root: tempfile::TempDir,
    dir: std::path::PathBuf,
}
impl TestDirs {
    fn new() -> Self {
        let root = tempfile::TempDir::new().unwrap();
        let dir = root.path().to_path_buf();
        Self { _root: root, dir }
    }
    fn path(&self) -> &str {
        self.dir.to_str().unwrap()
    }
}

fn yaml_val(s: &str) -> serde_json::Value {
    serde_saphyr::from_str(s).unwrap()
}

/// A step with EXPR let bindings should serialize resolvedSymTab
/// in the Python-compatible format: [{"name": str, "value": str_or_list, "type": str}]
#[test]
fn test_resolved_symtab_serialize_with_let_bindings() {
    let td = TestDirs::new();
    let template = yaml_val(
        r#"{
        "specificationVersion": "jobtemplate-2023-09",
        "name": "TestJob",
        "extensions": ["EXPR"],
        "parameterDefinitions": [
            {"name": "Foo", "type": "INT", "default": 42}
        ],
        "steps": [{
            "name": "MyStep",
            "let": ["x = 10", "greeting = 'hello'"],
            "script": {"actions": {"onRun": {"command": "echo", "args": ["{{x}}", "{{greeting}}", "{{Param.Foo}}", "{{Job.Name}}", "{{Step.Name}}"]}}}
        }]
    }"#,
    );

    let jt = decode_job_template(template, Some(&["EXPR"]), &CallerLimits::default()).unwrap();
    let params = preprocess_job_parameters(
        &jt,
        &Default::default(),
        &[],
        &openjd_model::PathParameterOptions {
            job_template_dir: td.path(),
            current_working_dir: td.path(),
            allow_template_dir_walk_up: true,
            path_format: PathFormat::host(),
            allow_uri_path_values: true,
        },
    )
    .unwrap();
    let job = create_job(&jt, &params, &jt.default_validation_context()).unwrap();

    let step = &job.steps[0];
    assert!(step.resolved_symtab.is_some());

    // Serialize to JSON and verify the resolvedSymTab format
    let json = serde_json::to_value(step).unwrap();
    let rb = json
        .get("resolvedSymTab")
        .expect("missing resolvedSymTab in JSON");
    let arr = rb.as_array().expect("resolvedSymTab should be array");

    // Find the "x" binding
    let x_binding = arr
        .iter()
        .find(|v| v["name"] == "x")
        .expect("missing x binding");
    assert_eq!(x_binding["value"], "10");
    assert_eq!(x_binding["type"], "int");

    // Find the "greeting" binding
    let greeting = arr
        .iter()
        .find(|v| v["name"] == "greeting")
        .expect("missing greeting");
    assert_eq!(greeting["value"], "hello");
    assert_eq!(greeting["type"], "string");

    // Find Job.Name
    let job_name = arr
        .iter()
        .find(|v| v["name"] == "Job.Name")
        .expect("missing Job.Name");
    assert_eq!(job_name["value"], "TestJob");
    assert_eq!(job_name["type"], "string");

    // Find Step.Name
    let step_name = arr
        .iter()
        .find(|v| v["name"] == "Step.Name")
        .expect("missing Step.Name");
    assert_eq!(step_name["value"], "MyStep");
    assert_eq!(step_name["type"], "string");
}

/// Deserialization should round-trip: serialize then deserialize back.
#[test]
fn test_resolved_symtab_round_trip() {
    let td = TestDirs::new();
    let template = yaml_val(
        r#"{
        "specificationVersion": "jobtemplate-2023-09",
        "name": "RoundTrip",
        "extensions": ["EXPR"],
        "steps": [{
            "name": "Step1",
            "let": ["count = 5", "flag = true"],
            "script": {"actions": {"onRun": {"command": "echo", "args": ["{{count}}", "{{flag}}"]}}}
        }]
    }"#,
    );

    let jt = decode_job_template(template, Some(&["EXPR"]), &CallerLimits::default()).unwrap();
    let params = preprocess_job_parameters(
        &jt,
        &Default::default(),
        &[],
        &openjd_model::PathParameterOptions {
            job_template_dir: td.path(),
            current_working_dir: td.path(),
            allow_template_dir_walk_up: true,
            path_format: PathFormat::host(),
            allow_uri_path_values: true,
        },
    )
    .unwrap();
    let job = create_job(&jt, &params, &jt.default_validation_context()).unwrap();

    let step = &job.steps[0];
    let original_symtab = step.resolved_symtab.as_ref().unwrap();

    // Serialize step to JSON, then deserialize the resolvedSymTab portion
    let json = serde_json::to_value(step).unwrap();
    let rb_json = json.get("resolvedSymTab").unwrap();
    let deserialized: openjd_model::SymbolTable = serde_json::from_value(rb_json.clone()).unwrap();

    // The round-tripped symtab should contain the serialized entries
    assert_eq!(
        deserialized.get_value("count"),
        Some(&openjd_expr::ExprValue::Int(5))
    );
    assert_eq!(
        deserialized.get_value("flag"),
        Some(&openjd_expr::ExprValue::Bool(true))
    );
    let original_symtab = original_symtab
        .to_symtab(openjd_expr::PathFormat::Posix)
        .unwrap();
    assert_eq!(
        deserialized.get_value("Job.Name"),
        original_symtab.get_value("Job.Name")
    );
    assert_eq!(
        deserialized.get_value("Step.Name"),
        original_symtab.get_value("Step.Name")
    );
}

/// List-typed let bindings should serialize value as a JSON array.
#[test]
fn test_resolved_symtab_list_value() {
    let td = TestDirs::new();
    let template = yaml_val(
        r#"{
        "specificationVersion": "jobtemplate-2023-09",
        "name": "ListTest",
        "extensions": ["EXPR"],
        "parameterDefinitions": [
            {"name": "Nums", "type": "LIST[INT]", "default": [1, 2, 3]}
        ],
        "steps": [{
            "name": "Step1",
            "let": ["my_list = Param.Nums"],
            "script": {"actions": {"onRun": {"command": "echo", "args": ["{{my_list}}"]}}}
        }]
    }"#,
    );

    let jt = decode_job_template(template, Some(&["EXPR"]), &CallerLimits::default()).unwrap();
    let params = preprocess_job_parameters(
        &jt,
        &Default::default(),
        &[],
        &openjd_model::PathParameterOptions {
            job_template_dir: td.path(),
            current_working_dir: td.path(),
            allow_template_dir_walk_up: true,
            path_format: PathFormat::host(),
            allow_uri_path_values: true,
        },
    )
    .unwrap();
    let job = create_job(&jt, &params, &jt.default_validation_context()).unwrap();

    let json = serde_json::to_value(&job.steps[0]).unwrap();
    let rb = json.get("resolvedSymTab").unwrap().as_array().unwrap();

    let my_list = rb
        .iter()
        .find(|v| v["name"] == "my_list")
        .expect("missing my_list");
    assert_eq!(my_list["type"], "list[int]");
    // Value should be a JSON array of strings (matching Python's transport format)
    let val = my_list["value"]
        .as_array()
        .expect("list value should be array");
    assert_eq!(
        val,
        &[
            serde_json::json!("1"),
            serde_json::json!("2"),
            serde_json::json!("3")
        ]
    );
}

/// A step without EXPR still has a resolvedSymTab with Param/RawParam entries.
#[test]
fn test_resolved_symtab_serialized_without_expr() {
    let td = TestDirs::new();
    let template = yaml_val(
        r#"{
        "specificationVersion": "jobtemplate-2023-09",
        "name": "Simple",
        "parameterDefinitions": [{"name": "X", "type": "INT", "default": 1}],
        "steps": [{
            "name": "Step1",
            "script": {"actions": {"onRun": {"command": "echo", "args": ["{{Param.X}}", "{{RawParam.X}}"]}}}
        }]
    }"#,
    );

    let jt = decode_job_template(template, Some(&["EXPR"]), &CallerLimits::default()).unwrap();
    let params = preprocess_job_parameters(
        &jt,
        &Default::default(),
        &[],
        &openjd_model::PathParameterOptions {
            job_template_dir: td.path(),
            current_working_dir: td.path(),
            allow_template_dir_walk_up: true,
            path_format: PathFormat::host(),
            allow_uri_path_values: true,
        },
    )
    .unwrap();
    let job = create_job(&jt, &params, &jt.default_validation_context()).unwrap();

    let json = serde_json::to_value(&job.steps[0]).unwrap();
    let rb = json
        .get("resolvedSymTab")
        .expect("should have resolvedSymTab");
    let arr = rb.as_array().unwrap();
    // Should contain Param.X and RawParam.X
    assert!(
        arr.iter().any(|v| v["name"] == "Param.X"),
        "missing Param.X"
    );
    assert!(
        arr.iter().any(|v| v["name"] == "RawParam.X"),
        "missing RawParam.X"
    );
}

/// A script-level let binding that calls apply_path_mapping should not
/// produce a concrete path in resolved_symtab — script let bindings are
/// type-checked but their results stay out of the template-scope symtab.
#[test]
fn test_script_let_apply_path_mapping_not_in_resolved_symtab() {
    let td = TestDirs::new();
    let template = yaml_val(
        r#"{
        "specificationVersion": "jobtemplate-2023-09",
        "name": "PathMapTest",
        "extensions": ["EXPR"],
        "parameterDefinitions": [
            {"name": "SrcPath", "type": "PATH", "default": "/src/file.txt"}
        ],
        "steps": [{
            "name": "Step1",
            "script": {
                "let": ["mapped = apply_path_mapping(RawParam.SrcPath)"],
                "actions": {"onRun": {"command": "echo"}}
            }
        }]
    }"#,
    );

    let jt = decode_job_template(template, Some(&["EXPR"]), &CallerLimits::default()).unwrap();
    let params = preprocess_job_parameters(
        &jt,
        &Default::default(),
        &[],
        &openjd_model::PathParameterOptions {
            job_template_dir: td.path(),
            current_working_dir: td.path(),
            allow_template_dir_walk_up: true,
            path_format: PathFormat::host(),
            allow_uri_path_values: true,
        },
    )
    .unwrap();
    let job = create_job(&jt, &params, &jt.default_validation_context()).unwrap();

    let symtab = job.steps[0]
        .resolved_symtab
        .as_ref()
        .unwrap()
        .to_symtab(openjd_expr::PathFormat::Posix)
        .unwrap();
    // Script-level let bindings are type-checked but not stored in resolved_symtab
    assert!(
        symtab.get_value("mapped").is_none(),
        "script-level let binding should not be in resolved_symtab"
    );
}

/// Script-level let bindings are type-checked during template validation.
/// A type error like adding a path and an int should fail at decode time.
#[test]
fn test_script_let_type_error_caught_at_validation() {
    let template = yaml_val(
        r#"{
        "specificationVersion": "jobtemplate-2023-09",
        "name": "TypeErrTest",
        "extensions": ["EXPR"],
        "steps": [{
            "name": "Step1",
            "script": {
                "let": ["bad = apply_path_mapping('/some/path') + 3"],
                "actions": {"onRun": {"command": "echo"}}
            }
        }]
    }"#,
    );

    let result = decode_job_template(template, Some(&["EXPR"]), &CallerLimits::default());
    assert!(result.is_err(), "should fail: can't add path + int");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("path") && err.contains("int"),
        "error should mention the type mismatch, got: {err}"
    );
}

/// Step resolved_symtab only contains symbols referenced by the step's format strings.
/// Unreferenced parameters and EXPR symbols like Job.Name should be excluded.
#[test]
fn test_step_resolved_symtab_excludes_unreferenced_symbols() {
    let td = TestDirs::new();
    let template = yaml_val(
        r#"{
        "specificationVersion": "jobtemplate-2023-09",
        "name": "FilterTest",
        "extensions": ["EXPR"],
        "parameterDefinitions": [
            {"name": "Used", "type": "STRING", "default": "yes"},
            {"name": "Unused", "type": "STRING", "default": "no"}
        ],
        "steps": [{
            "name": "Step1",
            "let": ["referenced = 1", "unreferenced = 2"],
            "script": {"actions": {"onRun": {"command": "echo", "args": ["{{Param.Used}}", "{{referenced}}"]}}}
        }]
    }"#,
    );

    let jt = decode_job_template(template, Some(&["EXPR"]), &CallerLimits::default()).unwrap();
    let params = preprocess_job_parameters(
        &jt,
        &Default::default(),
        &[],
        &openjd_model::PathParameterOptions {
            job_template_dir: td.path(),
            current_working_dir: td.path(),
            allow_template_dir_walk_up: true,
            path_format: PathFormat::host(),
            allow_uri_path_values: true,
        },
    )
    .unwrap();
    let job = create_job(&jt, &params, &jt.default_validation_context()).unwrap();

    let st = job.steps[0]
        .resolved_symtab
        .as_ref()
        .unwrap()
        .to_symtab(openjd_expr::PathFormat::Posix)
        .unwrap();

    // Referenced symbols are present
    assert!(
        st.get_value("Param.Used").is_some(),
        "Param.Used should be in resolved_symtab"
    );
    assert!(
        st.get_value("referenced").is_some(),
        "referenced let binding should be in resolved_symtab"
    );

    // Unreferenced symbols are excluded
    assert!(
        st.get_value("Param.Unused").is_none(),
        "Param.Unused should be excluded"
    );
    assert!(
        st.get_value("RawParam.Unused").is_none(),
        "RawParam.Unused should be excluded"
    );
    assert!(
        st.get_value("unreferenced").is_none(),
        "unreferenced let binding should be excluded"
    );
    assert!(
        st.get_value("Job.Name").is_none(),
        "Job.Name should be excluded when unreferenced"
    );
    assert!(
        st.get_value("Step.Name").is_none(),
        "Step.Name should be excluded when unreferenced"
    );
}

/// Job environment resolved_symtab only contains symbols referenced by that
/// environment's format strings. Symbols used only by the step are excluded.
#[test]
fn test_job_env_resolved_symtab_excludes_unreferenced_symbols() {
    let td = TestDirs::new();
    let template = yaml_val(
        r#"{
        "specificationVersion": "jobtemplate-2023-09",
        "name": "EnvFilterTest",
        "extensions": ["EXPR"],
        "parameterDefinitions": [
            {"name": "EnvUsed", "type": "STRING", "default": "env_val"},
            {"name": "StepOnly", "type": "STRING", "default": "step_val"}
        ],
        "jobEnvironments": [{
            "name": "TestEnv",
            "script": {
                "actions": {
                    "onEnter": {"command": "echo", "args": ["{{Param.EnvUsed}}"]},
                    "onExit": {"command": "echo", "args": ["done"]}
                }
            }
        }],
        "steps": [{
            "name": "Step1",
            "script": {"actions": {"onRun": {"command": "echo", "args": ["{{Param.StepOnly}}"]}}}
        }]
    }"#,
    );

    let jt = decode_job_template(template, Some(&["EXPR"]), &CallerLimits::default()).unwrap();
    let params = preprocess_job_parameters(
        &jt,
        &Default::default(),
        &[],
        &openjd_model::PathParameterOptions {
            job_template_dir: td.path(),
            current_working_dir: td.path(),
            allow_template_dir_walk_up: true,
            path_format: PathFormat::host(),
            allow_uri_path_values: true,
        },
    )
    .unwrap();
    let job = create_job(&jt, &params, &jt.default_validation_context()).unwrap();

    // Check the job environment's resolved_symtab
    let env = &job.job_environments.as_ref().unwrap()[0];
    let env_st = env
        .resolved_symtab
        .as_ref()
        .expect("job env should have resolved_symtab")
        .to_symtab(openjd_expr::PathFormat::Posix)
        .unwrap();

    assert!(
        env_st.get_value("Param.EnvUsed").is_some(),
        "Param.EnvUsed should be in env resolved_symtab"
    );
    assert!(
        env_st.get_value("Param.StepOnly").is_none(),
        "Param.StepOnly should be excluded from env resolved_symtab"
    );
    assert!(
        env_st.get_value("Job.Name").is_none(),
        "Job.Name should be excluded when unreferenced by env"
    );

    // Check the step's resolved_symtab has the opposite
    let step_st = job.steps[0]
        .resolved_symtab
        .as_ref()
        .unwrap()
        .to_symtab(openjd_expr::PathFormat::Posix)
        .unwrap();

    assert!(
        step_st.get_value("Param.StepOnly").is_some(),
        "Param.StepOnly should be in step resolved_symtab"
    );
    assert!(
        step_st.get_value("Param.EnvUsed").is_none(),
        "Param.EnvUsed should be excluded from step resolved_symtab"
    );
}

/// Job environment resolved_symtab includes symbols from embedded file data
/// and let bindings, not just action args.
#[test]
fn test_job_env_resolved_symtab_includes_embedded_file_refs() {
    let td = TestDirs::new();
    let template = yaml_val(
        r#"{
        "specificationVersion": "jobtemplate-2023-09",
        "name": "EmbedTest",
        "extensions": ["EXPR"],
        "parameterDefinitions": [
            {"name": "Greeting", "type": "STRING", "default": "hello"},
            {"name": "Unused", "type": "STRING", "default": "nope"}
        ],
        "jobEnvironments": [{
            "name": "TestEnv",
            "script": {
                "embeddedFiles": [{"name": "cfg", "type": "TEXT", "data": "msg={{Param.Greeting}}"}],
                "let": ["cfg_path = Env.File.cfg"],
                "actions": {
                    "onEnter": {"command": "cat", "args": ["{{cfg_path}}"]}
                }
            }
        }],
        "steps": [{
            "name": "Step1",
            "script": {"actions": {"onRun": {"command": "echo"}}}
        }]
    }"#,
    );

    let jt = decode_job_template(template, Some(&["EXPR"]), &CallerLimits::default()).unwrap();
    let params = preprocess_job_parameters(
        &jt,
        &Default::default(),
        &[],
        &openjd_model::PathParameterOptions {
            job_template_dir: td.path(),
            current_working_dir: td.path(),
            allow_template_dir_walk_up: true,
            path_format: PathFormat::host(),
            allow_uri_path_values: true,
        },
    )
    .unwrap();
    let job = create_job(&jt, &params, &jt.default_validation_context()).unwrap();

    let env = &job.job_environments.as_ref().unwrap()[0];
    let env_st = env
        .resolved_symtab
        .as_ref()
        .unwrap()
        .to_symtab(openjd_expr::PathFormat::Posix)
        .unwrap();

    // Param.Greeting is referenced in embedded file data
    assert!(
        env_st.get_value("Param.Greeting").is_some(),
        "Param.Greeting should be included (referenced in embedded file data)"
    );
    // Unused is not referenced anywhere in this environment
    assert!(
        env_st.get_value("Param.Unused").is_none(),
        "Param.Unused should be excluded from env resolved_symtab"
    );
}

/// When a job environment references a PATH-typed parameter via `Param.X`,
/// the environment's resolved_symtab should include `RawParam.X` (since
/// `Param.X` is not available at template scope for PATH types).
/// Uses `Param.OutDir.name` (property access) to verify the fallback
/// correctly extracts the parameter name from dotted symbol references.
#[test]
fn test_job_env_resolved_symtab_includes_raw_param_for_path() {
    let td = TestDirs::new();
    let template = yaml_val(
        r#"{
        "specificationVersion": "jobtemplate-2023-09",
        "name": "EnvPathTest",
        "extensions": ["EXPR"],
        "parameterDefinitions": [
            {"name": "OutDir", "type": "PATH", "default": "/out/renders"}
        ],
        "jobEnvironments": [{
            "name": "PathEnv",
            "script": {
                "actions": {
                    "onEnter": {"command": "echo", "args": ["{{Param.OutDir.name}}"]}
                }
            }
        }],
        "steps": [{
            "name": "Step1",
            "script": {"actions": {"onRun": {"command": "echo"}}}
        }]
    }"#,
    );

    let jt = decode_job_template(template, Some(&["EXPR"]), &CallerLimits::default()).unwrap();
    let params = preprocess_job_parameters(
        &jt,
        &Default::default(),
        &[],
        &openjd_model::PathParameterOptions {
            job_template_dir: td.path(),
            current_working_dir: td.path(),
            allow_template_dir_walk_up: true,
            path_format: PathFormat::host(),
            allow_uri_path_values: true,
        },
    )
    .unwrap();
    let job = create_job(&jt, &params, &jt.default_validation_context()).unwrap();

    let env = &job.job_environments.as_ref().unwrap()[0];
    let env_st = env
        .resolved_symtab
        .as_ref()
        .expect("env should have resolved_symtab")
        .to_symtab(openjd_expr::PathFormat::Posix)
        .unwrap();

    // Param.OutDir is PATH type — not in template-scope symtab
    // RawParam.OutDir should be included so the session can path-map it
    assert!(
        env_st.get_value("RawParam.OutDir").is_some(),
        "RawParam.OutDir should be in env resolved_symtab for PATH param"
    );
}

/// Create a job from `template` with default parameter values and return the
/// first step's resolved_symtab.
fn first_step_resolved_symtab(template: &str) -> openjd_model::SymbolTable {
    let td = TestDirs::new();
    let jt = decode_job_template(
        yaml_val(template),
        Some(&["EXPR"]),
        &CallerLimits::default(),
    )
    .unwrap();
    let params = preprocess_job_parameters(
        &jt,
        &Default::default(),
        &[],
        &openjd_model::PathParameterOptions {
            job_template_dir: td.path(),
            current_working_dir: td.path(),
            allow_template_dir_walk_up: true,
            path_format: PathFormat::host(),
            allow_uri_path_values: true,
        },
    )
    .unwrap();
    let job = create_job(&jt, &params, &jt.default_validation_context()).unwrap();
    job.steps[0]
        .resolved_symtab
        .as_ref()
        .expect("step should have resolved_symtab")
        .to_symtab(openjd_expr::PathFormat::Posix)
        .unwrap()
}

/// A step environment's script `let` bindings are resolved on the worker
/// from the step's resolved_symtab, so the job parameters they reference
/// must be in it: directly, through a chain of bindings, and inside a
/// comprehension. Parameters nothing references stay out.
#[test]
fn test_step_env_let_refs_in_step_resolved_symtab() {
    let st = first_step_resolved_symtab(
        r#"{
        "specificationVersion": "jobtemplate-2023-09",
        "name": "StepEnvLet",
        "extensions": ["EXPR"],
        "parameterDefinitions": [
            {"name": "Base", "type": "INT", "default": 7},
            {"name": "Offset", "type": "INT", "default": 1},
            {"name": "Flag", "type": "BOOL", "default": true},
            {"name": "Unused", "type": "INT", "default": 3}
        ],
        "steps": [{
            "name": "Step1",
            "stepEnvironments": [{
                "name": "StepEnv",
                "script": {
                    "let": [
                        "x = Param.Base * 2",
                        "y = x + Param.Offset",
                        "b = [v for v in [1] if Param.Flag]"
                    ],
                    "actions": {"onEnter": {"command": "echo", "args": ["{{ y }}", "{{ b }}"]}}
                }
            }],
            "script": {"actions": {"onRun": {"command": "echo"}}}
        }]
    }"#,
    );

    assert_eq!(
        st.get_value("Param.Base"),
        Some(&openjd_expr::ExprValue::Int(7))
    );
    assert_eq!(
        st.get_value("Param.Offset"),
        Some(&openjd_expr::ExprValue::Int(1))
    );
    assert_eq!(
        st.get_value("Param.Flag"),
        Some(&openjd_expr::ExprValue::Bool(true))
    );
    assert!(
        st.get_value("Param.Unused").is_none(),
        "Param.Unused should be excluded"
    );
    // Names bound inside the environment's let chain and comprehension loop
    // variables are not job-creation symbols, so they don't appear.
    for name in ["x", "y", "b", "v"] {
        assert!(st.get_value(name).is_none(), "{name} should be excluded");
    }
}

/// A step environment's script `let` that references a PATH-typed job
/// parameter gets the `RawParam.*` fallback in the step's resolved_symtab,
/// as a job environment's does.
#[test]
fn test_step_env_let_path_param_includes_raw_param() {
    let st = first_step_resolved_symtab(
        r#"{
        "specificationVersion": "jobtemplate-2023-09",
        "name": "StepEnvLetPath",
        "extensions": ["EXPR"],
        "parameterDefinitions": [
            {"name": "OutDir", "type": "PATH", "default": "/out/renders"},
            {"name": "OtherDir", "type": "PATH", "default": "/other"}
        ],
        "steps": [{
            "name": "Step1",
            "stepEnvironments": [{
                "name": "StepEnv",
                "script": {
                    "let": ["leaf = Param.OutDir.name"],
                    "actions": {"onEnter": {"command": "echo", "args": ["{{ leaf }}"]}}
                }
            }],
            "script": {"actions": {"onRun": {"command": "echo"}}}
        }]
    }"#,
    );

    assert!(
        st.get_value("RawParam.OutDir").is_some(),
        "RawParam.OutDir should be in step resolved_symtab for PATH param"
    );
    assert!(
        st.get_value("RawParam.OtherDir").is_none(),
        "RawParam.OtherDir should be excluded"
    );
}

/// Environment `variables` that reference job parameters put them in the
/// resolved_symtab the environment is entered with: the step's for a step
/// environment, and the environment's own for a job environment. A PATH
/// parameter arrives as its `RawParam.*` value.
#[test]
fn test_env_variables_refs_in_resolved_symtab() {
    let td = TestDirs::new();
    let template = yaml_val(
        r#"{
        "specificationVersion": "jobtemplate-2023-09",
        "name": "EnvVars",
        "extensions": ["EXPR"],
        "parameterDefinitions": [
            {"name": "Q", "type": "INT", "default": 5},
            {"name": "PP", "type": "PATH", "default": "/a/f.txt"},
            {"name": "J", "type": "STRING", "default": "jv"},
            {"name": "Unused", "type": "INT", "default": 3}
        ],
        "jobEnvironments": [{
            "name": "JobEnv",
            "variables": {"JV": "{{ Param.J }}"}
        }],
        "steps": [{
            "name": "Step1",
            "stepEnvironments": [{
                "name": "StepEnv",
                "variables": {"SV": "{{ Param.Q * 2 }}|{{ Param.PP.name }}"}
            }],
            "script": {"actions": {"onRun": {"command": "echo"}}}
        }]
    }"#,
    );
    let jt = decode_job_template(template, Some(&["EXPR"]), &CallerLimits::default()).unwrap();
    let params = preprocess_job_parameters(
        &jt,
        &Default::default(),
        &[],
        &openjd_model::PathParameterOptions {
            job_template_dir: td.path(),
            current_working_dir: td.path(),
            allow_template_dir_walk_up: true,
            path_format: PathFormat::host(),
            allow_uri_path_values: true,
        },
    )
    .unwrap();
    let job = create_job(&jt, &params, &jt.default_validation_context()).unwrap();

    let step_st = job.steps[0]
        .resolved_symtab
        .as_ref()
        .unwrap()
        .to_symtab(openjd_expr::PathFormat::Posix)
        .unwrap();
    assert_eq!(
        step_st.get_value("Param.Q"),
        Some(&openjd_expr::ExprValue::Int(5))
    );
    assert!(
        step_st.get_value("RawParam.PP").is_some(),
        "RawParam.PP should be in step resolved_symtab for PATH param"
    );
    assert!(step_st.get_value("Param.J").is_none());
    assert!(step_st.get_value("Param.Unused").is_none());

    let env_st = job.job_environments.as_ref().unwrap()[0]
        .resolved_symtab
        .as_ref()
        .unwrap()
        .to_symtab(openjd_expr::PathFormat::Posix)
        .unwrap();
    assert_eq!(
        env_st.get_value("Param.J"),
        Some(&openjd_expr::ExprValue::String("jv".into()))
    );
    assert!(env_st.get_value("Param.Q").is_none());
}

/// A step script `let` that references a PATH-typed job parameter gets the
/// `RawParam.*` fallback in the step's resolved_symtab.
#[test]
fn test_step_script_let_path_param_includes_raw_param() {
    let st = first_step_resolved_symtab(
        r#"{
        "specificationVersion": "jobtemplate-2023-09",
        "name": "StepScriptLetPath",
        "extensions": ["EXPR"],
        "parameterDefinitions": [
            {"name": "P2", "type": "PATH", "default": "/a/two.txt"},
            {"name": "Other", "type": "PATH", "default": "/other"}
        ],
        "steps": [{
            "name": "Step1",
            "script": {
                "let": ["b = Param.P2.name"],
                "actions": {"onRun": {"command": "echo", "args": ["{{ b }}"]}}
            }
        }]
    }"#,
    );
    assert!(
        st.get_value("RawParam.P2").is_some(),
        "RawParam.P2 should be in step resolved_symtab for PATH param"
    );
    assert!(st.get_value("RawParam.Other").is_none());
}
