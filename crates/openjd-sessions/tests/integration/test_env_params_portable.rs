// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `ENV_PARAMS` Session tests that start no process, so they run on every
//! platform (the end-to-end tests in `test_env_params_session.rs` are
//! Unix-only). Values are observed through an Environment's `variables`,
//! which resolve during `enter_environment_with_parameters`.

use std::collections::HashMap;

use openjd_expr::format_string::FormatString;
use openjd_expr::SerializedSymbolTable;
use openjd_model::job::{Document, Environment, EnvironmentParameterValues};
use openjd_sessions::session::Session;
use tempfile::TempDir;

/// An Environment with no script whose `variables` are `vars`.
fn env_with_variables(vars: &[(&str, &str)]) -> Environment {
    Environment {
        name: "E".to_string(),
        description: None,
        script: None,
        variables: Some(
            vars.iter()
                .map(|(k, v)| (k.to_string(), FormatString::new(v).unwrap()))
                .collect(),
        ),
        resolved_symtab: None,
    }
}

/// Enter `E` with `table` as its values and return the session's
/// variables named in `vars`.
async fn variables(table: serde_json::Value, vars: &[(&str, &str)]) -> HashMap<String, String> {
    let tmp = TempDir::new().unwrap();
    let mut session = Session::new_for_test(tmp.path().to_path_buf());
    let values = EnvironmentParameterValues {
        document: Document::JobTemplate,
        environment: "E".to_string(),
        values: SerializedSymbolTable::from_value(table),
    };
    session
        .enter_environment_with_parameters(
            &env_with_variables(vars),
            None,
            Some(&values),
            None,
            None,
        )
        .await
        .unwrap();
    let all = session.evaluate_env_vars(None);
    vars.iter()
        .map(|(k, _)| (k.to_string(), all[*k].clone().unwrap()))
        .collect()
}

/// `Env.Param` of a PATH is a path in the host format, so path operations
/// follow the host's separators; `Env.RawParam` is the string as given.
#[tokio::test]
async fn env_param_paths_use_the_host_path_format() {
    let (value, name, parent) = if cfg!(windows) {
        (r"C:\data\scene.txt", "scene.txt", r"C:\data")
    } else {
        ("/data/scene.txt", "scene.txt", "/data")
    };
    let got = variables(
        serde_json::json!([{"name": "P", "type": "path", "value": value}]),
        &[
            ("NAME", "{{ Env.Param.P.name }}"),
            ("PARENT", "{{ Env.Param.P.parent }}"),
            ("RAW", "{{ Env.RawParam.P }}"),
        ],
    )
    .await;
    assert_eq!(got["NAME"], name);
    assert_eq!(got["PARENT"], parent);
    assert_eq!(got["RAW"], value);
}

/// An empty LIST[PATH] value keeps its element types: `Env.RawParam` is a
/// `list[string]` (which `path()` joins) and `Env.Param` a `list[path]`
/// (which `path()` does not accept).
#[tokio::test]
async fn an_empty_list_path_keeps_its_element_types() {
    let table = serde_json::json!([{"name": "Dirs", "type": "list[path]", "value": []}]);
    let got = variables(table.clone(), &[("RAW", "{{ path(Env.RawParam.Dirs) }}")]).await;
    assert_eq!(got["RAW"], ".");

    let tmp = TempDir::new().unwrap();
    let mut session = Session::new_for_test(tmp.path().to_path_buf());
    let values = EnvironmentParameterValues {
        document: Document::JobTemplate,
        environment: "E".to_string(),
        values: SerializedSymbolTable::from_value(table),
    };
    let err = session
        .enter_environment_with_parameters(
            &env_with_variables(&[("MAPPED", "{{ path(Env.Param.Dirs) }}")]),
            None,
            Some(&values),
            None,
            None,
        )
        .await
        .expect_err("path() takes no list[path]");
    assert_eq!(
        err.to_string(),
        "Failed to resolve env var 'MAPPED': No matching signature for path(list[path])\n  \
         path(Env.Param.Dirs)\n  ^~~~~~~~~~~~~~~~~~~~"
    );
}
