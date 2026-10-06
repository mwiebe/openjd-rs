// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! Port of Python openjd-cli unit tests to Rust integration tests.
//! Tests the `openjd` binary via `Command` invocations.

use std::path::PathBuf;
use std::process::Command;
use std::sync::LazyLock;

fn openjd_bin() -> PathBuf {
    // Use cargo to find the binary
    let mut path = PathBuf::from(env!("CARGO_BIN_EXE_openjd"));
    if !path.exists() {
        // Fallback
        path = PathBuf::from("target/debug/openjd");
    }
    path
}

fn templates_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/templates")
}

/// Resolve a shim directory to prepend to `PATH` so every template's
/// `command: python` works regardless of what the host happens to
/// provide. The resolution order is:
///
/// 1. If `OPENJD_TEST_PYTHON` is set in the environment, use it as the
///    target interpreter. This lets CI or developers override the
///    interpreter explicitly (e.g. a specific venv's python).
/// 2. Otherwise, check the host `PATH` for the first of `python`,
///    `python3`, `python3.13`, `python3.12`, `python3.11`, `python3.10`,
///    `python3.9` that exists as an executable. The probe order favors
///    the canonical name `python` first so no shim is created when the
///    host already provides it.
///
/// If a shim is needed, a persistent temp directory is created (leaked
/// via `Box::leak` — tests run once per process, the OS reclaims the
/// dir when the process exits) and a `python` symlink or wrapper script
/// is placed inside pointing at the resolved interpreter. Returns
/// `None` when `python` is already directly available on `PATH` so the
/// caller can short-circuit.
///
/// This keeps the YAML fixtures portable (`command: python`) while
/// surviving hosts that only install `python3`.
fn python_shim_dir() -> Option<PathBuf> {
    static SHIM: LazyLock<Option<PathBuf>> = LazyLock::new(|| {
        // Fail fast with a clear message: a Python interpreter is required for
        // the CLI tests, so a failure to find one should not be silently
        // swallowed into a confusing downstream test failure.
        let target = resolve_python_interpreter().expect("Failed to find a Python interpreter");
        // If the interpreter is literally named `python` we don't need a shim —
        // its parent directory is already on `PATH` (that's how `which` found
        // it, transitively). Detect this by comparing the file-name component.
        let fname = target.file_name().and_then(|n| n.to_str()).unwrap_or("");
        // On Windows, `python.exe` is the canonical name.
        let is_canonical = if cfg!(windows) {
            fname.eq_ignore_ascii_case("python.exe")
        } else {
            fname == "python"
        };
        if is_canonical {
            return None;
        }
        let shim_dir =
            std::env::temp_dir().join(format!("openjd-cli-tests-python-{}", std::process::id()));
        // A shim was needed but we failed to set it up — fail fast with a
        // descriptive message rather than returning `None`, which would make
        // the downstream test fail in a way that's hard to diagnose.
        std::fs::create_dir_all(&shim_dir)
            .unwrap_or_else(|e| panic!("Failed to create shim_dir {shim_dir:?}: {e}"));
        let shim_path = shim_dir.join(if cfg!(windows) {
            "python.exe"
        } else {
            "python"
        });

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Write a small shell script rather than a symlink — a symlink to
            // `python3` breaks when that binary itself inspects `argv[0]` to
            // decide behavior (rare, but some venv wrappers do this).
            let script = format!("#!/bin/sh\nexec {:?} \"$@\"\n", target);
            std::fs::write(&shim_path, script)
                .unwrap_or_else(|e| panic!("Failed to write python shim {shim_path:?}: {e}"));
            let mut perms = std::fs::metadata(&shim_path)
                .unwrap_or_else(|e| panic!("Failed to stat python shim {shim_path:?}: {e}"))
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&shim_path, perms).unwrap_or_else(|e| {
                panic!("Failed to set permissions on python shim {shim_path:?}: {e}")
            });
        }
        #[cfg(windows)]
        {
            // On Windows, hard-link the real interpreter as `python.exe` so
            // it resolves correctly under PATHEXT semantics without the
            // argument-mangling issues of a .cmd/.bat wrapper. Hard link
            // avoids a ~5 MB copy and works as long as source and dest are on
            // the same volume (both are under %TEMP% / %LOCALAPPDATA%).
            std::fs::hard_link(&target, &shim_path).unwrap_or_else(|_| {
                std::fs::copy(&target, &shim_path).unwrap_or_else(|e| {
                    panic!("Failed to copy python interpreter to {shim_path:?}: {e}")
                });
            });
        }
        Some(shim_dir)
    });
    SHIM.clone()
}

/// Locate a usable Python interpreter on the host `PATH`, or return the
/// value of `OPENJD_TEST_PYTHON` if set. Returns `None` if none of the
/// candidate names exists — the caller should let the underlying test
/// fail with a clear message in that case.
fn resolve_python_interpreter() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("OPENJD_TEST_PYTHON") {
        let p = PathBuf::from(explicit);
        if p.exists() {
            return Some(p);
        }
    }
    // Candidate names in priority order. `python` first so we early-exit
    // without creating a shim on hosts that already provide it.
    let candidates: &[&str] = &[
        "python",
        "python3",
        "python3.13",
        "python3.12",
        "python3.11",
        "python3.10",
        "python3.9",
    ];
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        for name in candidates {
            let exe = if cfg!(windows) {
                dir.join(format!("{name}.exe"))
            } else {
                dir.join(name)
            };
            if exe.is_file() {
                return Some(exe);
            }
        }
    }
    None
}

fn cli_command(args: &[&str]) -> Command {
    let mut cmd = Command::new(openjd_bin());
    cmd.args(args).env("RUSTUP_TOOLCHAIN", "1.96.0");
    if let Some(shim) = python_shim_dir() {
        // Only rewrite PATH when it's actually defined. If PATH is unset
        // (extremely rare), skip — prepending the shim to an empty PATH would
        // add an empty entry that searches the current directory for
        // executables, which we don't want.
        if let Some(existing) = std::env::var_os("PATH") {
            let joined =
                std::env::join_paths(std::iter::once(shim).chain(std::env::split_paths(&existing)))
                    .expect("join_paths");
            cmd.env("PATH", joined);
        }
    }
    cmd
}

fn run_cli(args: &[&str]) -> (i32, String, String) {
    let output = cli_command(args)
        .output()
        .expect("failed to execute openjd");
    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (exit_code, stdout, stderr)
}

// ============================================================
// Group 1: CLI Entrypoint / Argument Parsing (test_main.py)
// ============================================================

mod cli_entrypoint {
    use super::*;

    #[test]
    fn test_cli_check_success() {
        let template = templates_dir().join("basic.yaml");
        let (code, stdout, _stderr) = run_cli(&["check", template.to_str().unwrap()]);
        assert_eq!(code, 0, "check should succeed");
        assert!(
            stdout.contains("passes validation checks"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_cli_run_success_base() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "BareStep",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "base run should succeed. stderr: {stderr}");
        assert!(
            stdout.contains("All actions completed successfully!"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_cli_run_success_with_params() {
        let tdir = templates_dir();
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            tdir.join("basic.yaml").to_str().unwrap(),
            "--step",
            "First",
            "-p",
            "J=value1",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "run with params should succeed. stderr: {stderr}");
    }

    #[test]
    fn test_cli_run_success_with_multiple_params() {
        let tdir = templates_dir();
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            tdir.join("basic.yaml").to_str().unwrap(),
            "--step",
            "First",
            "-p",
            "J=value1",
            "--run-dependencies",
            "--extensions",
            "",
        ]);
        assert_eq!(
            code, 0,
            "run with multiple options should succeed. stderr: {stderr}"
        );
    }

    #[test]
    fn test_cli_argument_errors_no_command() {
        let (code, _stdout, _stderr) = run_cli(&[]);
        assert_ne!(code, 0, "no command should fail");
    }

    #[test]
    fn test_cli_argument_errors_nonexistent_command() {
        let (code, _stdout, _stderr) = run_cli(&["notarealcommand"]);
        assert_ne!(code, 0, "nonexistent command should fail");
    }

    #[test]
    fn test_cli_argument_errors_check_no_args() {
        let (code, _stdout, _stderr) = run_cli(&["check"]);
        assert_ne!(code, 0, "check with no args should fail");
    }

    #[test]
    fn test_cli_argument_errors_run_no_step_arg_value() {
        let tdir = templates_dir();
        let (code, _stdout, _stderr) =
            run_cli(&["run", tdir.join("basic.yaml").to_str().unwrap(), "--step"]);
        assert_ne!(code, 0, "missing step value should fail");
    }
}

// ============================================================
// Group 2: Check Command (test_check_command.py)
// ============================================================

mod check_command {
    use super::*;
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};

    #[test]
    fn test_do_check_file_success_json() {
        let mut f = NamedTempFile::with_suffix(".template.json").unwrap();
        write!(
            f,
            r#"{{
            "specificationVersion": "jobtemplate-2023-09",
            "name": "test",
            "steps": [{{"name": "s1", "script": {{"actions": {{"onRun": {{"command": "echo"}}}}}}}}]
        }}"#
        )
        .unwrap();
        let (code, stdout, stderr) = run_cli(&["check", f.path().to_str().unwrap()]);
        assert_eq!(code, 0, "JSON check should succeed. stderr: {stderr}");
        assert!(
            stdout.contains("passes validation checks"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_do_check_file_success_yaml() {
        let mut f = NamedTempFile::with_suffix(".template.yaml").unwrap();
        write!(
            f,
            r#"specificationVersion: "jobtemplate-2023-09"
name: test
steps:
  - name: s1
    script:
      actions:
        onRun:
          command: echo
"#
        )
        .unwrap();
        let (code, stdout, stderr) = run_cli(&["check", f.path().to_str().unwrap()]);
        assert_eq!(code, 0, "YAML check should succeed. stderr: {stderr}");
        assert!(
            stdout.contains("passes validation checks"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_do_check_file_error_nonexistent() {
        let (code, _stdout, stderr) = run_cli(&["check", "error-file.json"]);
        assert_ne!(code, 0, "nonexistent file should fail");
        assert!(stderr.contains("does not exist"), "stderr: {stderr}");
    }

    #[test]
    fn test_do_check_bundle_error_directory() {
        let dir = TempDir::new().unwrap();
        let (code, _stdout, stderr) = run_cli(&["check", dir.path().to_str().unwrap()]);
        assert_ne!(code, 0, "directory should fail");
        assert!(stderr.contains("not a file"), "stderr: {stderr}");
    }

    #[test]
    fn test_do_check_file_success_ojdt() {
        let mut f = NamedTempFile::with_suffix(".ojdt").unwrap();
        write!(
            f,
            r#"specificationVersion: "jobtemplate-2023-09"
name: test
steps:
  - name: s1
    script:
      actions:
        onRun:
          command: echo
"#
        )
        .unwrap();
        let (code, stdout, stderr) = run_cli(&["check", f.path().to_str().unwrap()]);
        assert_eq!(
            code, 0,
            ".ojdt check should succeed (parsed as YAML). stderr: {stderr}"
        );
        assert!(
            stdout.contains("passes validation checks"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_do_run_file_success_ojdt() {
        let mut f = NamedTempFile::with_suffix(".ojdt").unwrap();
        write!(
            f,
            r#"specificationVersion: "jobtemplate-2023-09"
name: test
steps:
  - name: s1
    script:
      actions:
        onRun:
          command: echo
          args: ["ojdt works"]
"#
        )
        .unwrap();
        let (code, stdout, stderr) = run_cli(&["run", f.path().to_str().unwrap()]);
        assert_eq!(code, 0, ".ojdt run should succeed. stderr: {stderr}");
        assert!(stdout.contains("ojdt works"), "stdout: {stdout}");
    }

    #[test]
    fn test_do_summary_file_success_ojdt() {
        let mut f = NamedTempFile::with_suffix(".ojdt").unwrap();
        write!(
            f,
            r#"specificationVersion: "jobtemplate-2023-09"
name: OjdtJob
steps:
  - name: s1
    script:
      actions:
        onRun:
          command: echo
"#
        )
        .unwrap();
        let (code, stdout, stderr) = run_cli(&["summary", f.path().to_str().unwrap()]);
        assert_eq!(code, 0, ".ojdt summary should succeed. stderr: {stderr}");
        assert!(stdout.contains("OjdtJob"), "stdout: {stdout}");
    }
}

// ============================================================
// Group 3: Common Utilities — Parameter Parsing (test_common.py)
// ============================================================

mod common_params {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    // Test parameter parsing via the run command (since Rust doesn't expose internal functions directly)
    // We test that parameters are correctly parsed by running a template that echoes them.

    #[test]
    fn test_params_from_key_value_pair() {
        let template = templates_dir().join("simple_with_j_param.yaml");
        let (code, stdout, stderr) =
            run_cli(&["run", template.to_str().unwrap(), "-p", "J=TestValue"]);
        assert_eq!(code, 0, "should succeed. stderr: {stderr}");
        assert!(stdout.contains("DoTask TestValue"), "stdout: {stdout}");
    }

    #[test]
    fn test_params_from_file() {
        let mut f = NamedTempFile::with_suffix(".json").unwrap();
        write!(f, r#"{{"J": "FromFile"}}"#).unwrap();
        let file_arg = format!("file://{}", f.path().display());
        let template = templates_dir().join("simple_with_j_param.yaml");
        let (code, stdout, stderr) = run_cli(&["run", template.to_str().unwrap(), "-p", &file_arg]);
        assert_eq!(code, 0, "should succeed. stderr: {stderr}");
        assert!(stdout.contains("DoTask FromFile"), "stdout: {stdout}");
    }

    #[test]
    fn test_params_value_with_equals() {
        // Create a template that accepts a STRING param and echoes it
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            f,
            r#"specificationVersion: "jobtemplate-2023-09"
name: test
parameterDefinitions:
  - name: MyParam
    type: STRING
steps:
  - name: s1
    script:
      actions:
        onRun:
          command: echo
          args: ["{{{{Param.MyParam}}}}"]
"#
        )
        .unwrap();
        let (code, stdout, stderr) =
            run_cli(&["run", f.path().to_str().unwrap(), "-p", "MyParam=One=Two"]);
        assert_eq!(code, 0, "should succeed. stderr: {stderr}");
        assert!(stdout.contains("One=Two"), "stdout: {stdout}");
    }

    #[test]
    fn test_run_error_nonexistent_template() {
        let (code, _stdout, stderr) = run_cli(&["run", "some-file.json"]);
        assert_ne!(code, 0, "nonexistent file should fail");
        assert!(stderr.contains("does not exist"), "stderr: {stderr}");
    }

    #[test]
    fn test_params_yaml_file() {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        writeln!(f, "J: YamlValue").unwrap();
        let file_arg = format!("file://{}", f.path().display());
        let template = templates_dir().join("simple_with_j_param.yaml");
        let (code, stdout, stderr) = run_cli(&["run", template.to_str().unwrap(), "-p", &file_arg]);
        assert_eq!(code, 0, "should succeed. stderr: {stderr}");
        assert!(stdout.contains("YamlValue"), "stdout: {stdout}");
    }

    #[test]
    fn test_params_combination_kvp_and_file() {
        let mut f = NamedTempFile::with_suffix(".json").unwrap();
        write!(f, r#"{{"J": "FromFile"}}"#).unwrap();
        let file_arg = format!("file://{}", f.path().display());
        // Use a template with two params: J and an extra one
        let mut tmpl = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            tmpl,
            r#"specificationVersion: "jobtemplate-2023-09"
name: test
parameterDefinitions:
  - name: J
    type: STRING
  - name: Extra
    type: STRING
steps:
  - name: s1
    script:
      actions:
        onRun:
          command: echo
          args: ["{{{{Param.J}}}} {{{{Param.Extra}}}}"]
"#
        )
        .unwrap();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tmpl.path().to_str().unwrap(),
            "-p",
            &file_arg,
            "-p",
            "Extra=ExtraVal",
        ]);
        assert_eq!(code, 0, "should succeed. stderr: {stderr}");
        assert!(stdout.contains("FromFile"), "stdout: {stdout}");
        assert!(stdout.contains("ExtraVal"), "stdout: {stdout}");
    }

    #[test]
    fn test_params_bad_json_file() {
        let mut f = NamedTempFile::with_suffix(".json").unwrap();
        write!(f, "{{bad json}}").unwrap();
        let file_arg = format!("file://{}", f.path().display());
        let template = templates_dir().join("simple_with_j_param.yaml");
        let (code, _stdout, stderr) =
            run_cli(&["run", template.to_str().unwrap(), "-p", &file_arg]);
        assert_ne!(code, 0, "bad JSON should fail");
        assert!(!stderr.is_empty(), "stderr: {stderr}");
    }

    #[test]
    fn test_params_non_dict_json_file() {
        let mut f = NamedTempFile::with_suffix(".json").unwrap();
        write!(f, r#"["not", "a", "dict"]"#).unwrap();
        let file_arg = format!("file://{}", f.path().display());
        let template = templates_dir().join("simple_with_j_param.yaml");
        let (code, _stdout, stderr) =
            run_cli(&["run", template.to_str().unwrap(), "-p", &file_arg]);
        assert_ne!(code, 0, "non-dict JSON should fail");
        assert!(stderr.contains("dictionary"), "stderr: {stderr}");
    }

    #[test]
    fn test_params_directory_as_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let file_arg = format!("file://{}", dir.path().display());
        let template = templates_dir().join("simple_with_j_param.yaml");
        let (code, _stdout, stderr) =
            run_cli(&["run", template.to_str().unwrap(), "-p", &file_arg]);
        assert_ne!(code, 0, "directory as param file should fail");
        assert!(!stderr.is_empty(), "stderr: {stderr}");
    }

    #[test]
    fn test_params_not_json_string() {
        let template = templates_dir().join("simple_with_j_param.yaml");
        let (code, _stdout, stderr) =
            run_cli(&["run", template.to_str().unwrap(), "-p", "- not json -"]);
        assert_ne!(code, 0, "non-json non-kvp should fail");
        assert!(
            stderr.contains("not formatted correctly") || stderr.contains("format"),
            "stderr: {stderr}"
        );
    }

    #[test]
    fn test_params_json_array_string() {
        let template = templates_dir().join("simple_with_j_param.yaml");
        let (code, _stdout, stderr) =
            run_cli(&["run", template.to_str().unwrap(), "-p", r#"["a", "b"]"#]);
        assert_ne!(code, 0, "JSON array string should fail");
        assert!(
            stderr.contains("not formatted correctly") || stderr.contains("format"),
            "stderr: {stderr}"
        );
    }
}

// ============================================================
// Group 9: Run with Environment Templates (test_run_with_env.py)
// ============================================================

mod run_with_env {
    use super::*;

    #[test]
    fn test_run_job_with_env_default_params() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("simple_with_j_param.yaml").to_str().unwrap(),
            "-p",
            "J=Jvalue",
            "--env",
            tdir.join("env_with_param.yaml").to_str().unwrap(),
        ]);
        assert_eq!(code, 0, "should succeed. stderr: {stderr}");
        assert!(
            stdout.contains("EnvWithParam Enter DefaultForEnvParam"),
            "stdout: {stdout}"
        );
        assert!(stdout.contains("DoTask Jvalue"), "stdout: {stdout}");
        assert!(
            stdout.contains("EnvWithParam Exit DefaultForEnvParam"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_run_job_with_env_provide_env_param() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("simple_with_j_param.yaml").to_str().unwrap(),
            "-p",
            "J=Jvalue",
            "-p",
            "EnvParam=EnvParamValue",
            "--env",
            tdir.join("env_with_param.yaml").to_str().unwrap(),
        ]);
        assert_eq!(code, 0, "should succeed. stderr: {stderr}");
        assert!(
            stdout.contains("EnvWithParam Enter EnvParamValue"),
            "stdout: {stdout}"
        );
        assert!(stdout.contains("DoTask Jvalue"), "stdout: {stdout}");
        assert!(
            stdout.contains("EnvWithParam Exit EnvParamValue"),
            "stdout: {stdout}"
        );
    }
}

// ============================================================
// Group 10: Feature Bundle 1 (test_feature_bundle_1.py)
// ============================================================

mod feature_bundle_1 {
    use super::*;

    #[test]
    fn test_python_syntax_sugar() {
        let template = templates_dir().join("feature_bundle_1_python.yaml");
        let (code, stdout, stderr) = run_cli(&["run", template.to_str().unwrap()]);
        assert_eq!(code, 0, "should succeed. stderr: {stderr}");
        assert!(stdout.contains("Hello from Python!"), "stdout: {stdout}");
    }

    #[test]
    fn test_bash_syntax_sugar() {
        let template = templates_dir().join("feature_bundle_1_bash.yaml");
        let (code, stdout, stderr) = run_cli(&["run", template.to_str().unwrap()]);
        assert_eq!(code, 0, "should succeed. stderr: {stderr}");
        assert!(stdout.contains("Hello from Bash!"), "stdout: {stdout}");
    }

    #[test]
    fn test_format_string_timeout() {
        let template = templates_dir().join("feature_bundle_1_timeout.yaml");
        let (code, stdout, stderr) = run_cli(&["run", template.to_str().unwrap()]);
        assert_eq!(code, 0, "should succeed. stderr: {stderr}");
        assert!(
            stdout.contains("Running with timeout 5s"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_format_string_amount_minmax() {
        let template = templates_dir().join("feature_bundle_1_amount_minmax.yaml");
        let (code, stdout, stderr) = run_cli(&["run", template.to_str().unwrap()]);
        assert_eq!(code, 0, "should succeed. stderr: {stderr}");
        assert!(stdout.contains("Amount min/max works!"), "stdout: {stdout}");
    }

    #[test]
    fn test_format_string_notify_period() {
        let template = templates_dir().join("feature_bundle_1_notify_period.yaml");
        let (code, stdout, stderr) = run_cli(&["run", template.to_str().unwrap()]);
        assert_eq!(code, 0, "should succeed. stderr: {stderr}");
        assert!(stdout.contains("Notify period works!"), "stdout: {stdout}");
    }

    #[test]
    fn test_extended_step_name() {
        let template = templates_dir().join("feature_bundle_1_long_name.yaml");
        let (code, stdout, stderr) = run_cli(&["run", template.to_str().unwrap()]);
        assert_eq!(code, 0, "should succeed. stderr: {stderr}");
        assert!(stdout.contains("Long step name works!"), "stdout: {stdout}");
    }

    #[test]
    fn test_end_of_line_lf() {
        let template = templates_dir().join("feature_bundle_1_eol_lf.yaml");
        let (code, stdout, stderr) = run_cli(&["run", template.to_str().unwrap()]);
        assert_eq!(code, 0, "should succeed. stderr: {stderr}");
        assert!(
            stdout.contains("310a 6c69 6e65 320a 6c69"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_end_of_line_crlf() {
        let template = templates_dir().join("feature_bundle_1_eol_crlf.yaml");
        let (code, stdout, stderr) = run_cli(&["run", template.to_str().unwrap()]);
        assert_eq!(code, 0, "should succeed. stderr: {stderr}");
        assert!(
            stdout.contains("310d 0a6c 696e 6532 0d0a"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_end_of_line_auto() {
        let template = templates_dir().join("feature_bundle_1_eol_auto.yaml");
        let (code, stdout, stderr) = run_cli(&["run", template.to_str().unwrap()]);
        assert_eq!(code, 0, "should succeed. stderr: {stderr}");
        if cfg!(windows) {
            // On Windows, AUTO should produce CRLF
            assert!(
                stdout.contains("310d 0a6c 696e 6532 0d0a"),
                "stdout: {stdout}"
            );
        } else {
            // On Linux/macOS, AUTO should produce LF
            assert!(
                stdout.contains("310a 6c69 6e65 320a 6c69"),
                "stdout: {stdout}"
            );
        }
    }

    #[test]
    fn test_check_validates_extension() {
        let template = templates_dir().join("feature_bundle_1_python.yaml");
        let (code, stdout, _stderr) = run_cli(&["check", template.to_str().unwrap()]);
        assert_eq!(code, 0, "check should succeed");
        assert!(
            stdout.contains("passes validation checks"),
            "stdout: {stdout}"
        );
    }
}

// ============================================================
// Group 12: Redacted Environment Variables (test_redacted_env.py)
// ============================================================

mod redacted_env {
    use super::*;

    #[test]
    fn test_run_job_with_redacted_env() {
        let template = templates_dir().join("redacted_env.yaml");
        let (code, stdout, stderr) = run_cli(&["run", template.to_str().unwrap()]);
        assert_eq!(code, 0, "should succeed. stderr: {stderr}");
        assert!(stdout.contains("Setting redacted vars"), "stdout: {stdout}");
        // Verify the openjd_redacted_env protocol lines are not leaked
        assert!(
            !stdout.contains("openjd_redacted_env: SECRETVAR=SECRETVAL"),
            "should not leak protocol. stdout: {stdout}"
        );
        assert!(
            !stdout.contains("openjd_redacted_env: KEYSPACE =SECRETVAL"),
            "should not leak protocol. stdout: {stdout}"
        );
        assert!(
            !stdout.contains("openjd_redacted_env: VALSPACE= SPACEVAL"),
            "should not leak protocol. stdout: {stdout}"
        );
    }
}

// ============================================================
// Group 6: Run Command (test_run_command.py)
// ============================================================

mod run_command {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    // --- test_do_run_success parametrized cases ---

    #[test]
    fn test_run_first_step() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("basic.yaml").to_str().unwrap(),
            "--step",
            "First",
            "-p",
            "J=Jvalue",
            "--run-dependencies",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("J1 Enter"), "stdout: {stdout}");
        assert!(stdout.contains("J2 Enter"), "stdout: {stdout}");
        assert!(stdout.contains("J=Jvalue"), "stdout: {stdout}");
        assert!(stdout.contains("Foo=1. Bar=Bar1"), "stdout: {stdout}");
        assert!(stdout.contains("Foo=1. Bar=Bar2"), "stdout: {stdout}");
    }

    #[test]
    fn test_run_second_step_with_dep() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("basic_dependency_job.yaml").to_str().unwrap(),
            "--step",
            "Second",
            "-p",
            "J=Jvalue",
            "--run-dependencies",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("J=Jvalue Fuz=1"), "stdout: {stdout}");
        assert!(stdout.contains("J=Jvalue Fuz=2"), "stdout: {stdout}");
        // Should also run First step (dependency)
        assert!(stdout.contains("Foo=1. Bar=Bar1"), "stdout: {stdout}");
    }

    #[test]
    fn test_run_second_step_no_dep() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("basic_dependency_job.yaml").to_str().unwrap(),
            "--step",
            "Second",
            "-p",
            "J=Jvalue",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("J=Jvalue Fuz=1"), "stdout: {stdout}");
        // Should NOT run First step
        assert!(
            !stdout.contains("Foo=1. Bar=Bar1"),
            "should not run dep. stdout: {stdout}"
        );
    }

    #[test]
    fn test_run_with_one_env() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("basic.yaml").to_str().unwrap(),
            "--step",
            "First",
            "-p",
            "J=Jvalue",
            "--run-dependencies",
            "--environment",
            tdir.join("env_1.yaml").to_str().unwrap(),
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("Env1 Enter"), "stdout: {stdout}");
        assert!(stdout.contains("Env1 Exit"), "stdout: {stdout}");
        assert!(stdout.contains("J=Jvalue"), "stdout: {stdout}");
    }

    #[test]
    fn test_run_with_two_envs() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("basic.yaml").to_str().unwrap(),
            "--step",
            "First",
            "-p",
            "J=Jvalue",
            "--run-dependencies",
            "--environment",
            tdir.join("env_1.yaml").to_str().unwrap(),
            "--environment",
            tdir.join("env_2.yaml").to_str().unwrap(),
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("Env1 Enter"), "stdout: {stdout}");
        assert!(stdout.contains("Env2 Enter"), "stdout: {stdout}");
        assert!(stdout.contains("Env2 Exit"), "stdout: {stdout}");
        assert!(stdout.contains("Env1 Exit"), "stdout: {stdout}");
    }

    #[test]
    fn test_run_error_nonexistent_template() {
        let (code, _stdout, stderr) = run_cli(&["run", "some-file.json"]);
        assert_ne!(code, 0);
        assert!(stderr.contains("does not exist"), "stderr: {stderr}");
    }

    #[test]
    fn test_run_nonexistent_step() {
        let tdir = templates_dir();
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            tdir.join("basic.yaml").to_str().unwrap(),
            "--step",
            "FakeStep",
            "-p",
            "J=Jvalue",
            "--extensions",
            "",
        ]);
        assert_ne!(code, 0);
        assert!(
            stderr.contains("No Step with name 'FakeStep'"),
            "stderr: {stderr}"
        );
    }

    #[test]
    fn test_preserve_option() {
        let mut f = NamedTempFile::with_suffix(".json").unwrap();
        write!(
            f,
            r#"{{
            "specificationVersion": "jobtemplate-2023-09",
            "name": "TestJob",
            "steps": [{{
                "name": "TestStep",
                "script": {{"actions": {{"onRun": {{"command": "echo", "args": ["hello"]}}}}}}
            }}]
        }}"#
        )
        .unwrap();
        let (code, stdout, stderr) = run_cli(&["run", f.path().to_str().unwrap(), "--preserve"]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(
            stdout.contains("Working directory preserved at"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_run_path_mapping_rules() {
        let mut template_f = NamedTempFile::with_suffix(".json").unwrap();
        let (source_format, source_path, dest_path, param_value, expected) = if cfg!(windows) {
            (
                "WINDOWS",
                r"D:\\home\\work",
                r"E:\\mnt\\work",
                r"D:\home\work",
                r"Mapped:E:\mnt\work",
            )
        } else {
            (
                "POSIX",
                "/home/test",
                "/mnt/test",
                "/home/test",
                "Mapped:/mnt/test",
            )
        };
        write!(template_f, r#"{{
            "specificationVersion": "jobtemplate-2023-09",
            "name": "Job",
            "parameterDefinitions": [{{"name": "TestPath", "type": "PATH"}}],
            "steps": [{{
                "name": "TestStep",
                "script": {{"actions": {{"onRun": {{"command": "python", "args": ["-c", "print('Mapped:{{{{Param.TestPath}}}}')"]}}}}}}
            }}]
        }}"#).unwrap();

        let mut rules_f = NamedTempFile::with_suffix(".rules.json").unwrap();
        write!(
            rules_f,
            r#"{{
            "version": "pathmapping-1.0",
            "path_mapping_rules": [{{
                "source_path_format": "{source_format}",
                "source_path": "{source_path}",
                "destination_path": "{dest_path}"
            }}]
        }}"#
        )
        .unwrap();

        let rules_arg = format!("file://{}", rules_f.path().display());
        let param_arg = format!("TestPath={param_value}");
        let (code, stdout, stderr) = run_cli(&[
            "run",
            template_f.path().to_str().unwrap(),
            "-p",
            &param_arg,
            "--path-mapping-rules",
            &rules_arg,
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains(expected), "stdout: {stdout}");
    }

    // --- Run local session tests ---

    #[test]
    fn test_run_bare_step() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "BareStep",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(
            stdout.contains("All actions completed successfully!"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_run_normal_step() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "NormalStep",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("Hello, world!"), "stdout: {stdout}");
        assert!(
            stdout.contains("All actions completed successfully!"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_run_dependent_step_with_deps() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "DependentStep",
            "--run-dependencies",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(
            stdout.contains("Running step 'BareStep'"),
            "stdout: {stdout}"
        );
        assert!(
            stdout.contains("Running step 'DependentStep'"),
            "stdout: {stdout}"
        );
        assert!(
            stdout.contains("All actions completed successfully!"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_run_dependent_step_no_deps() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "DependentStep",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(
            !stdout.contains("Running step 'BareStep'"),
            "should not run dep. stdout: {stdout}"
        );
        assert!(stdout.contains("I am dependent!"), "stdout: {stdout}");
    }

    #[test]
    fn test_run_extra_dependent_step() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "ExtraDependentStep",
            "--run-dependencies",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(
            stdout.contains("Running step 'BareStep'"),
            "stdout: {stdout}"
        );
        assert!(
            stdout.contains("Running step 'DependentStep'"),
            "stdout: {stdout}"
        );
        assert!(
            stdout.contains("Running step 'TaskParamStep'"),
            "stdout: {stdout}"
        );
        assert!(
            stdout.contains("Running step 'ExtraDependentStep'"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_run_task_param_step() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "TaskParamStep",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("1.Hi!"), "stdout: {stdout}");
        assert!(
            stdout.contains("All actions completed successfully!"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_run_bad_command() {
        let tdir = templates_dir();
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "BadCommand",
            "--extensions",
            "",
        ]);
        assert_ne!(code, 0, "bad command should fail");
        assert!(
            !stderr.is_empty() || !_stdout.is_empty(),
            "should have error output"
        );
    }

    #[test]
    fn test_step_dep_has_step_env() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "StepDepHasStepEnv",
            "--run-dependencies",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(
            stdout.contains("Running step 'NormalStep'"),
            "stdout: {stdout}"
        );
        assert!(
            stdout.contains("Running step 'StepDepHasStepEnv'"),
            "stdout: {stdout}"
        );
    }

    // --- Env/task failure tests (from test_do_run_success parametrized cases) ---

    #[test]
    fn test_enter_env_fails() {
        let tdir = templates_dir();
        let (code, stdout, _stderr) = run_cli(&[
            "run",
            tdir.join("simple_with_j_param.yaml").to_str().unwrap(),
            "-p",
            "J=Jvalue",
            "--environment",
            tdir.join("env_fails_enter.yaml").to_str().unwrap(),
        ]);
        assert_ne!(code, 0, "should fail when env enter fails");
        assert!(stdout.contains("EnvEnterFail Enter"), "stdout: {stdout}");
        // Should not run the task
        assert!(
            !stdout.contains("DoTask"),
            "should not run task. stdout: {stdout}"
        );
    }

    #[test]
    fn test_task_fails_still_exits_env() {
        let tdir = templates_dir();
        let (code, stdout, _stderr) = run_cli(&[
            "run",
            tdir.join("simple_with_j_param_exit_1.yaml")
                .to_str()
                .unwrap(),
            "-p",
            "J=Jvalue",
            "--environment",
            tdir.join("env_1.yaml").to_str().unwrap(),
        ]);
        assert_ne!(code, 0, "should fail when task exits 1");
        assert!(stdout.contains("Env1 Enter"), "stdout: {stdout}");
        assert!(stdout.contains("DoTask"), "stdout: {stdout}");
        assert!(stdout.contains("Env1 Exit"), "stdout: {stdout}");
    }

    #[test]
    fn test_env_exit_fails() {
        let tdir = templates_dir();
        let (code, stdout, _stderr) = run_cli(&[
            "run",
            tdir.join("simple_with_j_param.yaml").to_str().unwrap(),
            "-p",
            "J=Jvalue",
            "--environment",
            tdir.join("env_fails_exit.yaml").to_str().unwrap(),
        ]);
        // Task should still run even if env exit fails
        assert!(stdout.contains("EnvExitFail Enter"), "stdout: {stdout}");
        assert!(stdout.contains("DoTask"), "stdout: {stdout}");
        let _ = code; // exit code may vary
    }

    // --- Step let bindings ---

    #[test]
    fn test_step_let_bindings_in_step_env() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("step_let_in_step_env.yaml").to_str().unwrap(),
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("ENTER_VAL:21"), "stdout: {stdout}");
        assert!(stdout.contains("ENTER_LABEL:item_21"), "stdout: {stdout}");
        assert!(stdout.contains("ENV_VAL:21"), "stdout: {stdout}");
        assert!(stdout.contains("ENV_LABEL:item_21"), "stdout: {stdout}");
        assert!(stdout.contains("TASK_VAL:21"), "stdout: {stdout}");
        assert!(stdout.contains("EXIT_VAL:21"), "stdout: {stdout}");
        assert!(stdout.contains("EXIT_LABEL:item_21"), "stdout: {stdout}");
    }

    #[test]
    fn test_task_timeout() {
        // Action-level timeout is enforced: the task should be killed before
        // printing EXIT_NORMAL.
        let tdir = templates_dir();
        let (code, stdout, _stderr) = run_cli(&[
            "run",
            tdir.join("job_sleep_exit_normal.yaml").to_str().unwrap(),
            "--step",
            "Timeout",
            "-p",
            "J=x",
        ]);
        assert!(
            stdout.contains("SLEEP"),
            "should print SLEEP. stdout: {stdout}"
        );
        assert!(
            !stdout.contains("EXIT_NORMAL"),
            "should NOT print EXIT_NORMAL (killed by timeout). stdout: {stdout}"
        );
        assert_ne!(code, 0, "task should fail due to timeout");
    }

    /// Regression test: NaN / Infinity values for a FLOAT task parameter must
    /// produce a clean error rather than panicking inside `Float64::new`.
    /// See the 2026-05-06 security review ("[LOW] Potential Panic via unwrap()
    /// on Float64::new() with NaN Input").
    #[test]
    fn test_run_task_param_float_rejects_nan_and_infinity() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            f,
            r#"specificationVersion: "jobtemplate-2023-09"
name: float-task-param
steps:
  - name: s1
    parameterSpace:
      taskParameterDefinitions:
        - name: MyFloat
          type: FLOAT
          range: [1.0, 2.0, 3.0]
    script:
      actions:
        onRun:
          command: python
          args: ["-c", "print('{{{{Task.Param.MyFloat}}}}')"]
"#
        )
        .unwrap();
        let path = f.path().to_str().unwrap();

        for bad in ["NaN", "inf", "-inf", "infinity"] {
            let tasks = format!(r#"[{{"MyFloat":"{bad}"}}]"#);
            let (code, _stdout, stderr) =
                run_cli(&["run", path, "--step", "s1", "--tasks", &tasks]);
            assert_ne!(
                code, 0,
                "expected non-zero exit for '{bad}', stderr: {stderr}"
            );
            // Must not be a Rust panic — check for our friendly error message.
            assert!(
                !stderr.contains("panicked"),
                "must not panic on '{bad}'; stderr: {stderr}"
            );
            assert!(
                stderr.contains("must be finite"),
                "expected 'must be finite' error for '{bad}'; stderr: {stderr}"
            );
        }
    }
}

// ============================================================
// Group 3 continued: Common Utilities — Error Cases
// ============================================================

mod common_errors {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_check_job_template_parsing_error_json() {
        let mut f = NamedTempFile::with_suffix(".template.json").unwrap();
        write!(f, r#"{{ "specificationVersion": "jobtemplate-2023-09" }}"#).unwrap();
        let (code, _stdout, stderr) = run_cli(&["check", f.path().to_str().unwrap()]);
        assert_ne!(code, 0, "should fail validation");
        assert!(
            stderr.contains("validation") || stderr.contains("missing") || stderr.contains("error"),
            "stderr: {stderr}"
        );
    }

    #[test]
    fn test_check_job_template_parsing_error_yaml() {
        let mut f = NamedTempFile::with_suffix(".template.yaml").unwrap();
        writeln!(f, "specificationVersion: \"jobtemplate-2023-09\"").unwrap();
        let (code, _stdout, stderr) = run_cli(&["check", f.path().to_str().unwrap()]);
        assert_ne!(code, 0, "should fail validation");
        assert!(
            stderr.contains("validation") || stderr.contains("missing") || stderr.contains("error"),
            "stderr: {stderr}"
        );
    }

    #[test]
    fn test_check_env_template_parsing_error() {
        let mut f = NamedTempFile::with_suffix(".template.yaml").unwrap();
        writeln!(f, "specificationVersion: \"environment-2023-09\"").unwrap();
        let (code, _stdout, stderr) = run_cli(&["check", f.path().to_str().unwrap()]);
        assert_ne!(code, 0, "should fail validation");
        assert!(
            stderr.contains("validation") || stderr.contains("missing") || stderr.contains("error"),
            "stderr: {stderr}"
        );
    }

    #[test]
    fn test_run_extra_params_error() {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            f,
            r#"specificationVersion: "jobtemplate-2023-09"
name: test
steps:
  - name: s1
    script:
      actions:
        onRun:
          command: echo
"#
        )
        .unwrap();
        let (code, _stdout, stderr) =
            run_cli(&["run", f.path().to_str().unwrap(), "-p", "ExtraParam=value"]);
        assert_ne!(code, 0, "extra params should fail");
        assert!(
            stderr.contains("not defined") || stderr.contains("parameter"),
            "stderr: {stderr}"
        );
    }

    #[test]
    fn test_run_missing_required_params_error() {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            f,
            r#"specificationVersion: "jobtemplate-2023-09"
name: test
parameterDefinitions:
  - name: Required
    type: INT
    minValue: 3
    maxValue: 8
steps:
  - name: s1
    script:
      actions:
        onRun:
          command: echo
"#
        )
        .unwrap();
        let (code, _stdout, stderr) = run_cli(&["run", f.path().to_str().unwrap()]);
        assert_ne!(code, 0, "missing params should fail");
        assert!(
            stderr.contains("missing") || stderr.contains("required") || stderr.contains("Values"),
            "stderr: {stderr}"
        );
    }

    #[test]
    fn test_run_invalid_param_type_error() {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            f,
            r#"specificationVersion: "jobtemplate-2023-09"
name: test
parameterDefinitions:
  - name: Count
    type: INT
steps:
  - name: s1
    script:
      actions:
        onRun:
          command: echo
"#
        )
        .unwrap();
        let (code, _stdout, stderr) =
            run_cli(&["run", f.path().to_str().unwrap(), "-p", "Count=notanumber"]);
        assert_ne!(code, 0, "invalid type should fail");
        assert!(
            stderr.contains("integer") || stderr.contains("INT") || stderr.contains("error"),
            "stderr: {stderr}"
        );
    }

    #[test]
    fn test_run_param_constraint_violation() {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            f,
            r#"specificationVersion: "jobtemplate-2023-09"
name: test
parameterDefinitions:
  - name: Title
    type: STRING
    minLength: 3
  - name: Required
    type: INT
    minValue: 3
    maxValue: 8
steps:
  - name: s1
    script:
      actions:
        onRun:
          command: echo
"#
        )
        .unwrap();
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            f.path().to_str().unwrap(),
            "-p",
            "Title=a",
            "-p",
            "Required=5",
        ]);
        assert_ne!(code, 0, "constraint violation should fail");
        assert!(
            stderr.contains("length")
                || stderr.contains("characters")
                || stderr.contains("at least"),
            "stderr: {stderr}"
        );
    }

    #[test]
    fn test_run_param_file_nonexistent() {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            f,
            r#"specificationVersion: "jobtemplate-2023-09"
name: test
parameterDefinitions:
  - name: P
    type: STRING
steps:
  - name: s1
    script:
      actions:
        onRun:
          command: echo
"#
        )
        .unwrap();
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            f.path().to_str().unwrap(),
            "-p",
            "file:///nonexistent/params.json",
        ]);
        assert_ne!(code, 0, "nonexistent param file should fail");
        assert!(
            stderr.to_lowercase().contains("no such file")
                || stderr.to_lowercase().contains("cannot read")
                || stderr.to_lowercase().contains("not found")
                || stderr.to_lowercase().contains("does not exist"),
            "stderr: {stderr}"
        );
    }

    #[test]
    fn test_run_invalid_param_format() {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            f,
            r#"specificationVersion: "jobtemplate-2023-09"
name: test
steps:
  - name: s1
    script:
      actions:
        onRun:
          command: echo
"#
        )
        .unwrap();
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            f.path().to_str().unwrap(),
            "-p",
            "bad format no equals",
        ]);
        assert_ne!(code, 0, "badly formatted param should fail");
        assert!(
            stderr.contains("Invalid parameter format") || stderr.contains("not formatted"),
            "stderr: {stderr}"
        );
    }

    /// Regression test: `file://` parameter files larger than
    /// `MAX_FILE_INPUT_SIZE` must be rejected with a clear error rather
    /// than being fully read into memory.
    /// See the 2026-05-06 security review ("[LOW] Unbounded File Read via
    /// `file://` Parameter Paths").
    #[test]
    fn test_run_job_param_file_size_limit() {
        // 11 MiB sparse file — exceeds the 10 MiB cap. Uses set_len so the
        // test doesn't actually write gigabytes of data.
        let oversized = NamedTempFile::with_suffix(".json").unwrap();
        oversized
            .as_file()
            .set_len(11 * 1024 * 1024)
            .expect("set_len should succeed");
        let file_arg = format!("file://{}", oversized.path().display());

        let mut tpl = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            tpl,
            r#"specificationVersion: "jobtemplate-2023-09"
name: test
steps:
  - name: s1
    script:
      actions:
        onRun:
          command: echo
"#
        )
        .unwrap();

        let (code, _stdout, stderr) =
            run_cli(&["run", tpl.path().to_str().unwrap(), "-p", &file_arg]);
        assert_ne!(code, 0, "oversized param file should be rejected");
        assert!(stderr.contains("exceeds maximum size"), "stderr: {stderr}");
    }

    /// Regression test: oversized `file://` tasks files are also rejected.
    #[test]
    fn test_run_tasks_file_size_limit() {
        let oversized = NamedTempFile::with_suffix(".json").unwrap();
        oversized
            .as_file()
            .set_len(11 * 1024 * 1024)
            .expect("set_len should succeed");
        let tasks_arg = format!("file://{}", oversized.path().display());

        let tdir = templates_dir();
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "TaskParamStep",
            "--tasks",
            &tasks_arg,
            "--extensions",
            "",
        ]);
        assert_ne!(code, 0, "oversized tasks file should be rejected");
        assert!(stderr.contains("exceeds maximum size"), "stderr: {stderr}");
    }

    /// Regression test: oversized `file://` path-mapping-rules files are
    /// also rejected.
    #[test]
    fn test_run_path_mapping_rules_file_size_limit() {
        let oversized = NamedTempFile::with_suffix(".json").unwrap();
        oversized
            .as_file()
            .set_len(11 * 1024 * 1024)
            .expect("set_len should succeed");
        let rules_arg = format!("file://{}", oversized.path().display());

        let mut tpl = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            tpl,
            r#"specificationVersion: "jobtemplate-2023-09"
name: test
steps:
  - name: s1
    script:
      actions:
        onRun:
          command: echo
"#
        )
        .unwrap();

        let (code, _stdout, stderr) = run_cli(&[
            "run",
            tpl.path().to_str().unwrap(),
            "--path-mapping-rules",
            &rules_arg,
        ]);
        assert_ne!(code, 0, "oversized path-mapping file should be rejected");
        assert!(stderr.contains("exceeds maximum size"), "stderr: {stderr}");
    }
}

// ============================================================
// Group 11: Chunked Job (test_chunked_job.py)
// ============================================================

mod chunked_job {
    use super::*;

    #[test]
    fn test_check_chunked_job_default() {
        let template = templates_dir().join("chunked_job.yaml");
        let (code, stdout, _stderr) = run_cli(&["check", template.to_str().unwrap()]);
        assert_eq!(code, 0, "check should succeed with default extensions");
        assert!(
            stdout.contains("passes validation checks"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_check_chunked_job_no_extensions() {
        let template = templates_dir().join("chunked_job.yaml");
        let (code, _stdout, stderr) =
            run_cli(&["check", template.to_str().unwrap(), "--extensions", ""]);
        assert_ne!(code, 0, "should fail without TASK_CHUNKING extension");
        assert!(
            stderr.contains("TASK_CHUNKING")
                || stderr.contains("extension")
                || stderr.contains("Unsupported"),
            "stderr: {stderr}"
        );
    }

    #[test]
    fn test_check_chunked_job_with_extension() {
        let template = templates_dir().join("chunked_job.yaml");
        let (code, stdout, _stderr) = run_cli(&[
            "check",
            template.to_str().unwrap(),
            "--extensions",
            "TASK_CHUNKING",
        ]);
        assert_eq!(code, 0, "check should succeed with TASK_CHUNKING");
        assert!(
            stdout.contains("passes validation checks"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_run_chunked_job_default_options() {
        let template = templates_dir().join("chunked_job.yaml");
        let (code, stdout, stderr) =
            run_cli(&["run", template.to_str().unwrap(), "--step", "Chunked Step"]);
        assert_eq!(code, 0, "stderr: {stderr}");
        // Should run 4 chunks of 10 items each (1-10, 11-20, 21-30, 31-40)
        assert!(stdout.contains("1-10"), "stdout: {stdout}");
        assert!(stdout.contains("11-20"), "stdout: {stdout}");
        assert!(stdout.contains("21-30"), "stdout: {stdout}");
        assert!(stdout.contains("31-40"), "stdout: {stdout}");
    }

    #[test]
    fn test_run_chunked_job_maximum_task_count() {
        let template = templates_dir().join("chunked_job.yaml");
        let (code, stdout, stderr) = run_cli(&[
            "run",
            template.to_str().unwrap(),
            "--step",
            "Chunked Step",
            "-p",
            "ChunkSize=3",
            "-p",
            "TargetRuntime=0",
            "--maximum-tasks",
            "3",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        // Should only run 3 chunks
        assert!(stdout.contains("Chunks run: 3"), "stdout: {stdout}");
    }

    #[test]
    fn test_run_chunked_job_adaptive_chunking() {
        let template = templates_dir().join("chunked_job.yaml");
        let (code, stdout, stderr) = run_cli(&[
            "run",
            template.to_str().unwrap(),
            "--step",
            "Chunked Step",
            "-p",
            "ChunkSize=1",
            "-p",
            "TargetRuntime=10000",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        // With TargetRuntime very high and ChunkSize=1, adaptive chunking should
        // produce first chunk of 1 item, then remainder in second chunk
        assert!(stdout.contains("1"), "stdout: {stdout}");
        assert!(stdout.contains("2-40"), "stdout: {stdout}");
    }

    #[test]
    fn test_chunked_job_bad_task_param_out_of_range() {
        // Item=0 is outside the 1-40 range — the CLI now validates task param
        // values against the parameter space range.
        let template = templates_dir().join("chunked_job.yaml");
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            template.to_str().unwrap(),
            "--step",
            "Chunked Step",
            "-t",
            "Item=0",
        ]);
        assert_ne!(code, 0, "should reject out-of-range task param value");
        assert!(
            stderr.contains("not in the parameter space") || stderr.contains("not a valid chunk"),
            "stderr: {stderr}"
        );
    }

    #[test]
    fn test_chunked_job_bad_task_param_not_range_expr() {
        // "1;2" is not a valid range expression — the CLI rejects it at parse time
        let template = templates_dir().join("chunked_job.yaml");
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            template.to_str().unwrap(),
            "--step",
            "Chunked Step",
            "-t",
            "Item=1;2",
        ]);
        assert_ne!(code, 0, "task should fail with invalid range expression");
        assert!(
            stderr.contains("invalid range expression"),
            "stderr: {stderr}"
        );
    }

    #[test]
    fn test_chunked_job_bad_task_param_interval_out_of_range() {
        // Item=30-41 extends beyond the 1-40 range — the CLI now validates
        // task param values against the parameter space range.
        let template = templates_dir().join("chunked_job.yaml");
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            template.to_str().unwrap(),
            "--step",
            "Chunked Step",
            "-t",
            "Item=30-41",
        ]);
        assert_ne!(
            code, 0,
            "should reject out-of-range interval task param values"
        );
        assert!(
            stderr.contains("not in the parameter space")
                || stderr.contains("not a valid chunk")
                || stderr.contains("not a subset"),
            "stderr: {stderr}"
        );
    }
}

// ============================================================
// Group 7: Context-Aware Help (test_help_formatter.py + TestContextAwareHelp)
// ============================================================

mod context_aware_help {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_help_with_yaml_template() {
        let tdir = templates_dir();
        let (code, stdout, _stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "-h",
        ]);
        assert_eq!(code, 0);
        assert!(stdout.contains("Job: my-job"), "stdout: {stdout}");
        assert!(
            stdout.contains("Job Parameters (-p/--job-param PARAM_NAME=VALUE):"),
            "stdout: {stdout}"
        );
        assert!(stdout.contains("Message (STRING)"), "stdout: {stdout}");
        assert!(
            stdout.contains("[default: 'Hello, world!']"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_help_with_long_flag() {
        let tdir = templates_dir();
        let (code, stdout, _stderr) =
            run_cli(&["run", tdir.join("basic.yaml").to_str().unwrap(), "--help"]);
        assert_eq!(code, 0);
        assert!(stdout.contains("Job: Job"), "stdout: {stdout}");
        assert!(stdout.contains("J (STRING)"), "stdout: {stdout}");
        assert!(stdout.contains("[required]"), "stdout: {stdout}");
    }

    #[test]
    fn test_help_with_description() {
        let mut f = NamedTempFile::with_suffix(".json").unwrap();
        write!(
            f,
            r#"{{
            "specificationVersion": "jobtemplate-2023-09",
            "name": "TestJob",
            "description": "This is a test job with a description",
            "steps": [{{
                "name": "TestStep",
                "script": {{"actions": {{"onRun": {{"command": "echo"}}}}}}
            }}]
        }}"#
        )
        .unwrap();
        let (code, stdout, _stderr) = run_cli(&["run", f.path().to_str().unwrap(), "-h"]);
        assert_eq!(code, 0);
        assert!(stdout.contains("Job: TestJob"), "stdout: {stdout}");
        assert!(
            stdout.contains("This is a test job with a description"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_help_with_multiple_parameters() {
        let mut f = NamedTempFile::with_suffix(".json").unwrap();
        write!(f, r#"{{
            "specificationVersion": "jobtemplate-2023-09",
            "name": "MultiParamJob",
            "parameterDefinitions": [
                {{"name": "StringParam", "type": "STRING", "default": "hello", "description": "A string parameter"}},
                {{"name": "IntParam", "type": "INT", "minValue": 1, "maxValue": 10}},
                {{"name": "FloatParam", "type": "FLOAT", "default": 3.14}},
                {{"name": "PathParam", "type": "PATH"}}
            ],
            "steps": [{{
                "name": "TestStep",
                "script": {{"actions": {{"onRun": {{"command": "echo"}}}}}}
            }}]
        }}"#).unwrap();
        let (code, stdout, _stderr) = run_cli(&["run", f.path().to_str().unwrap(), "--help"]);
        assert_eq!(code, 0);
        assert!(
            stdout.contains("StringParam (STRING) [default: 'hello']"),
            "stdout: {stdout}"
        );
        assert!(stdout.contains("A string parameter"), "stdout: {stdout}");
        assert!(
            stdout.contains("IntParam (INT) [required]"),
            "stdout: {stdout}"
        );
        assert!(stdout.contains("range: 1 to 10"), "stdout: {stdout}");
        assert!(
            stdout.contains("FloatParam (FLOAT) [default: 3.14]"),
            "stdout: {stdout}"
        );
        assert!(
            stdout.contains("PathParam (PATH) [required]"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_help_includes_standard_options() {
        let tdir = templates_dir();
        let (code, stdout, _stderr) =
            run_cli(&["run", tdir.join("basic.yaml").to_str().unwrap(), "-h"]);
        assert_eq!(code, 0);
        assert!(stdout.contains("Standard Options:"), "stdout: {stdout}");
        assert!(stdout.contains("--job-param"), "stdout: {stdout}");
        assert!(stdout.contains("--environment"), "stdout: {stdout}");
        assert!(stdout.contains("--verbose"), "stdout: {stdout}");
        assert!(
            stdout.contains("leave out template to list all options"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_help_without_template_shows_standard_help() {
        let (code, stdout, _stderr) = run_cli(&["run", "--help"]);
        assert_eq!(code, 0);
        assert!(
            !stdout.contains("Job:"),
            "should not show job info. stdout: {stdout}"
        );
        assert!(
            !stdout.contains("Job Parameters"),
            "should not show params. stdout: {stdout}"
        );
    }

    #[test]
    fn test_help_with_nonexistent_template() {
        let (code, _stdout, stderr) = run_cli(&["run", "nonexistent_template.json", "-h"]);
        assert_ne!(code, 0);
        assert!(stderr.contains("Error:"), "stderr: {stderr}");
        assert!(
            stderr.contains("not found") || stderr.contains("does not exist"),
            "stderr: {stderr}"
        );
    }

    #[test]
    fn test_help_with_invalid_json_template() {
        let mut f = NamedTempFile::with_suffix(".json").unwrap();
        write!(f, "{{invalid json content").unwrap();
        let (code, _stdout, stderr) = run_cli(&["run", f.path().to_str().unwrap(), "-h"]);
        assert_ne!(code, 0);
        assert!(stderr.contains("Error:"), "stderr: {stderr}");
    }

    #[test]
    fn test_help_with_schema_validation_failure() {
        let mut f = NamedTempFile::with_suffix(".json").unwrap();
        write!(f, r#"{{ "specificationVersion": "jobtemplate-2023-09" }}"#).unwrap();
        let (code, _stdout, stderr) = run_cli(&["run", f.path().to_str().unwrap(), "-h"]);
        assert_ne!(code, 0);
        assert!(stderr.contains("Error:"), "stderr: {stderr}");
        assert!(
            stderr.contains("Invalid job template") || stderr.contains("validation"),
            "stderr: {stderr}"
        );
    }

    #[test]
    fn test_help_error_messages_are_user_friendly() {
        let (code, _stdout, stderr) = run_cli(&["run", "does_not_exist.json", "--help"]);
        assert_ne!(code, 0);
        assert!(stderr.contains("Error:"), "stderr: {stderr}");
        assert!(
            !stderr.contains("Traceback"),
            "should not show traceback. stderr: {stderr}"
        );
        assert!(
            !stderr.contains("panicked"),
            "should not show panic. stderr: {stderr}"
        );
    }

    #[test]
    fn test_help_with_constraints() {
        let mut f = NamedTempFile::with_suffix(".json").unwrap();
        write!(f, r#"{{
            "specificationVersion": "jobtemplate-2023-09",
            "name": "constraint-job",
            "description": "Job with various constraints",
            "parameterDefinitions": [
                {{"name": "Environment", "type": "STRING", "default": "dev", "allowedValues": ["dev", "staging", "prod"]}},
                {{"name": "Ratio", "type": "FLOAT", "default": 0.5, "minValue": 0.0, "maxValue": 1.0}},
                {{"name": "Username", "type": "STRING", "default": "user", "minLength": 3, "maxLength": 20}}
            ],
            "steps": [{{
                "name": "Step1",
                "script": {{"actions": {{"onRun": {{"command": "echo"}}}}}}
            }}]
        }}"#).unwrap();
        let (code, stdout, _stderr) = run_cli(&["run", f.path().to_str().unwrap(), "-h"]);
        assert_eq!(code, 0);
        assert!(
            stdout.contains("Environment (STRING) [default: 'dev']"),
            "stdout: {stdout}"
        );
        assert!(
            stdout.contains("allowed: 'dev', 'staging', 'prod'"),
            "stdout: {stdout}"
        );
        assert!(
            stdout.contains("Ratio (FLOAT) [default: 0.5]"),
            "stdout: {stdout}"
        );
        assert!(stdout.contains("range: 0 to 1"), "stdout: {stdout}");
        assert!(
            stdout.contains("Username (STRING) [default: 'user']"),
            "stdout: {stdout}"
        );
        assert!(
            stdout.contains("length: 3 to 20 characters"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_normal_execution_unaffected() {
        let tdir = templates_dir();
        let (code, stdout, _stderr) = run_cli(&[
            "run",
            tdir.join("simple_with_j_param.yaml").to_str().unwrap(),
            "-p",
            "J=TestValue",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0);
        assert!(stdout.contains("DoTask"), "stdout: {stdout}");
        assert!(
            !stdout.contains("Job Parameters"),
            "should not show help. stdout: {stdout}"
        );
    }

    #[test]
    fn test_no_params_template_no_params_section() {
        let mut f = NamedTempFile::with_suffix(".json").unwrap();
        write!(
            f,
            r#"{{
            "specificationVersion": "jobtemplate-2023-09",
            "name": "no-params-job",
            "description": "A job without parameters",
            "steps": [{{
                "name": "Step1",
                "script": {{"actions": {{"onRun": {{"command": "echo"}}}}}}
            }}]
        }}"#
        )
        .unwrap();
        let (code, stdout, _stderr) = run_cli(&["run", f.path().to_str().unwrap(), "-h"]);
        assert_eq!(code, 0);
        assert!(stdout.contains("Job: no-params-job"), "stdout: {stdout}");
        assert!(
            stdout.contains("A job without parameters"),
            "stdout: {stdout}"
        );
        assert!(
            !stdout.contains("Job Parameters"),
            "should not show params section. stdout: {stdout}"
        );
        assert!(stdout.contains("Standard Options:"), "stdout: {stdout}");
    }

    #[test]
    fn test_job_info_before_standard_options() {
        let tdir = templates_dir();
        let (code, stdout, _stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "-h",
        ]);
        assert_eq!(code, 0);
        let job_idx = stdout.find("Job: my-job").unwrap();
        let params_idx = stdout.find("Job Parameters").unwrap();
        let options_idx = stdout.find("Standard Options:").unwrap();
        assert!(job_idx < params_idx, "job name should come before params");
        assert!(
            params_idx < options_idx,
            "params should come before standard options"
        );
    }

    #[test]
    fn test_help_with_invalid_yaml_template() {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(f, "invalid: yaml: content: [unclosed").unwrap();
        let (code, _stdout, stderr) = run_cli(&["run", f.path().to_str().unwrap(), "-h"]);
        assert_ne!(code, 0);
        assert!(stderr.contains("Error"), "stderr: {stderr}");
    }

    #[test]
    fn test_required_vs_optional_parameters() {
        let mut f = NamedTempFile::with_suffix(".json").unwrap();
        write!(
            f,
            r#"{{
            "specificationVersion": "jobtemplate-2023-09",
            "name": "ReqOptTest",
            "parameterDefinitions": [
                {{"name": "RequiredParam", "type": "STRING"}},
                {{"name": "OptionalParam", "type": "STRING", "default": "default_value"}},
                {{"name": "Count", "type": "INT", "default": 0}},
                {{"name": "Message", "type": "STRING", "default": ""}}
            ],
            "steps": [{{"name": "S1", "script": {{"actions": {{"onRun": {{"command": "echo"}}}}}}}}]
        }}"#
        )
        .unwrap();
        let (code, stdout, _stderr) = run_cli(&["run", f.path().to_str().unwrap(), "-h"]);
        assert_eq!(code, 0);
        assert!(stdout.contains("RequiredParam"), "stdout: {stdout}");
        assert!(stdout.contains("[required]"), "stdout: {stdout}");
        assert!(stdout.contains("OptionalParam"), "stdout: {stdout}");
        assert!(stdout.contains("default_value"), "stdout: {stdout}");
        assert!(stdout.contains("Count"), "stdout: {stdout}");
        assert!(
            stdout.contains("[default: 0]"),
            "Count with default=0 should show as default. stdout: {stdout}"
        );
        assert!(stdout.contains("Message"), "stdout: {stdout}");
        assert!(
            stdout.contains("[default: '']"),
            "Message with default='' should show as default. stdout: {stdout}"
        );
    }

    #[test]
    fn test_string_parameter_with_multiline_default() {
        let mut f = NamedTempFile::with_suffix(".json").unwrap();
        write!(f, r#"{{
            "specificationVersion": "jobtemplate-2023-09",
            "name": "MultilineTest",
            "parameterDefinitions": [
                {{"name": "Script", "type": "STRING", "default": "echo 'Hello'\necho 'World'\nls -la", "description": "A bash script to run"}}
            ],
            "steps": [{{"name": "S1", "script": {{"actions": {{"onRun": {{"command": "echo"}}}}}}}}]
        }}"#).unwrap();
        let (code, stdout, _stderr) = run_cli(&["run", f.path().to_str().unwrap(), "-h"]);
        assert_eq!(code, 0);
        assert!(stdout.contains("Script"), "stdout: {stdout}");
        assert!(stdout.contains("A bash script to run"), "stdout: {stdout}");
        assert!(stdout.contains("echo 'Hello'"), "stdout: {stdout}");
        assert!(stdout.contains("echo 'World'"), "stdout: {stdout}");
        assert!(stdout.contains("ls -la"), "stdout: {stdout}");
    }

    #[test]
    fn test_path_parameter_with_multiline_default() {
        let mut f = NamedTempFile::with_suffix(".json").unwrap();
        write!(f, r#"{{
            "specificationVersion": "jobtemplate-2023-09",
            "name": "PathMultilineTest",
            "parameterDefinitions": [
                {{"name": "ConfigFile", "type": "PATH", "default": "/path/to/file1\n/path/to/file2"}}
            ],
            "steps": [{{"name": "S1", "script": {{"actions": {{"onRun": {{"command": "echo"}}}}}}}}]
        }}"#).unwrap();
        let (code, stdout, _stderr) = run_cli(&["run", f.path().to_str().unwrap(), "-h"]);
        assert_eq!(code, 0);
        assert!(stdout.contains("ConfigFile"), "stdout: {stdout}");
        assert!(stdout.contains("/path/to/file1"), "stdout: {stdout}");
        assert!(stdout.contains("/path/to/file2"), "stdout: {stdout}");
    }

    #[test]
    fn test_missing_parameters_shows_help() {
        let mut f = NamedTempFile::with_suffix(".json").unwrap();
        write!(f, r#"{{
            "specificationVersion": "jobtemplate-2023-09",
            "name": "TestJob",
            "description": "A test job with required parameters",
            "parameterDefinitions": [
                {{"name": "RequiredParam1", "type": "STRING", "description": "First required parameter"}},
                {{"name": "RequiredParam2", "type": "INT", "description": "Second required parameter"}}
            ],
            "steps": [{{"name": "S", "script": {{"actions": {{"onRun": {{"command": "echo"}}}}}}}}]
        }}"#).unwrap();
        let (code, stdout, stderr) = run_cli(&["run", f.path().to_str().unwrap()]);
        assert_ne!(code, 0, "should fail with missing params");
        let output = format!("{stdout}{stderr}");
        // Should show the missing params error
        assert!(
            output.contains("missing") || output.contains("Missing"),
            "should mention missing params. output: {output}"
        );
        // Should also show context-aware help with job info
        assert!(
            output.contains("Job: TestJob"),
            "should show job name. output: {output}"
        );
        assert!(
            output.contains("A test job with required parameters"),
            "should show job description. output: {output}"
        );
        assert!(
            output.contains("Job Parameters"),
            "should show parameters section. output: {output}"
        );
        assert!(
            output.contains("RequiredParam1")
                && output.contains("STRING")
                && output.contains("[required]"),
            "should show RequiredParam1 info. output: {output}"
        );
        assert!(
            output.contains("First required parameter"),
            "should show param1 description. output: {output}"
        );
        assert!(
            output.contains("RequiredParam2")
                && output.contains("INT")
                && output.contains("[required]"),
            "should show RequiredParam2 info. output: {output}"
        );
        assert!(
            output.contains("Second required parameter"),
            "should show param2 description. output: {output}"
        );
    }
}

// ============================================================
// Group 9 continued: EXPR extension in env
// ============================================================

mod run_with_env_expr {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_run_job_with_env_expr_extension() {
        let mut job_f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            job_f,
            r#"specificationVersion: jobtemplate-2023-09
name: TestJob
extensions:
  - FEATURE_BUNDLE_1
steps:
  - name: TestStep
    bash:
      script: echo "Task ran"
"#
        )
        .unwrap();

        let mut env_f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            env_f,
            r#"specificationVersion: environment-2023-09
extensions:
  - EXPR
environment:
  name: ExprEnv
  script:
    actions:
      onEnter:
        command: echo
        args:
          - "Enter {{{{ 1 + 2 }}}}"
      onExit:
        command: echo
        args:
          - "Exit"
"#
        )
        .unwrap();

        let (code, stdout, stderr) = run_cli(&[
            "run",
            job_f.path().to_str().unwrap(),
            "--environment",
            env_f.path().to_str().unwrap(),
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(
            stdout.contains("Enter 3"),
            "EXPR should evaluate. stdout: {stdout}"
        );
    }
}

mod new_cli_options {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_run_task_param_explicit() {
        // --task-param should run a single task with explicit values
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "TaskParamStep",
            "--task-param",
            "TaskNumber=1",
            "--task-param",
            "TaskMessage=Hi!",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("Running Task"), "stdout: {stdout}");
    }

    #[test]
    fn test_run_tasks_inline_json() {
        // --tasks with inline JSON array
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "TaskParamStep",
            "--tasks",
            r#"[{"TaskNumber": "1", "TaskMessage": "Hi!"}]"#,
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("Running Task"), "stdout: {stdout}");
    }

    #[test]
    fn test_run_verbose_flag() {
        // --verbose should be accepted without error
        let tdir = templates_dir();
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "BareStep",
            "--verbose",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
    }

    #[test]
    fn test_run_timestamp_format_utc() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "BareStep",
            "--timestamp-format",
            "utc",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        // UTC timestamps contain 'T' and 'Z'
        assert!(
            stdout.contains('T') && stdout.contains('Z'),
            "Expected UTC timestamps, stdout: {stdout}"
        );
    }

    #[test]
    fn test_run_timestamp_format_local() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "BareStep",
            "--timestamp-format",
            "local",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        // Local timestamps contain 'T' but not 'Z'
        assert!(
            stdout.contains('T'),
            "Expected local timestamps, stdout: {stdout}"
        );
    }

    #[test]
    fn test_run_output_json() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "BareStep",
            "--output",
            "json",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(
            stdout.contains(r#""status": "success""#),
            "Expected JSON output, stdout: {stdout}"
        );
    }

    #[test]
    fn test_run_output_yaml() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "BareStep",
            "--output",
            "yaml",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("status: success"), "stdout: {stdout}");
        assert!(stdout.contains("job_name:"), "stdout: {stdout}");
        assert!(stdout.contains("duration:"), "stdout: {stdout}");
        assert!(stdout.contains("chunks_run: 1"), "stdout: {stdout}");
    }

    #[test]
    fn test_run_auto_select_single_step() {
        // Job with one step should auto-select it without --step
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            f,
            r#"specificationVersion: "jobtemplate-2023-09"
name: SingleStepJob
steps:
  - name: OnlyStep
    script:
      actions:
        onRun:
          command: echo
          args: ["auto-selected"]
"#
        )
        .unwrap();
        let (code, stdout, stderr) = run_cli(&["run", f.path().to_str().unwrap()]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("auto-selected"), "stdout: {stdout}");
    }

    #[test]
    fn test_run_task_param_conflicts_with_maximum_tasks() {
        let tdir = templates_dir();
        let (code, _stdout, _stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "TaskParamStep",
            "--task-param",
            "TaskNumber=1",
            "--maximum-tasks",
            "5",
            "--extensions",
            "",
        ]);
        assert_ne!(code, 0, "conflicting options should fail");
    }

    #[test]
    fn test_run_inline_json_params() {
        // -p with inline JSON
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            f,
            r#"specificationVersion: "jobtemplate-2023-09"
name: test
parameterDefinitions:
  - name: Greeting
    type: STRING
steps:
  - name: s1
    script:
      actions:
        onRun:
          command: echo
          args: ["{{{{Param.Greeting}}}}"]
"#
        )
        .unwrap();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            f.path().to_str().unwrap(),
            "-p",
            r#"{"Greeting": "HelloJSON"}"#,
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("HelloJSON"), "stdout: {stdout}");
    }

    #[test]
    fn test_run_preserve_shows_working_dir() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "BareStep",
            "--preserve",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(
            stdout.contains("Working directory preserved at:"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_run_task_params_require_step_for_multistep() {
        // --task-param without --step on a multi-step job should error
        let tdir = templates_dir();
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--task-param",
            "TaskNumber=1",
            "--extensions",
            "",
        ]);
        assert_ne!(code, 0, "should fail without --step on multi-step job");
        assert!(
            stderr.contains("requires a specified step") || stderr.contains("single step"),
            "stderr: {stderr}"
        );
    }

    #[test]
    fn test_task_param_value_with_equals() {
        // TaskParamStep has TaskNumber(INT) and TaskMessage(STRING)
        // Pass a value containing '=' to verify split_once behavior
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "TaskParamStep",
            "-t",
            "TaskNumber=1",
            "-t",
            "TaskMessage=One=Two",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("One=Two"), "stdout: {stdout}");
    }

    #[test]
    fn test_task_param_bad_format() {
        let tdir = templates_dir();
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "TaskParamStep",
            "-t",
            "NoEquals",
            "--extensions",
            "",
        ]);
        assert_ne!(code, 0, "bad format should fail");
        assert!(
            stderr.contains("defined incorrectly") || stderr.contains("format"),
            "stderr: {stderr}"
        );
    }

    #[test]
    fn test_task_param_duplicate() {
        let tdir = templates_dir();
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "TaskParamStep",
            "-t",
            "TaskNumber=1",
            "-t",
            "TaskNumber=2",
            "--extensions",
            "",
        ]);
        assert_ne!(code, 0, "duplicate should fail");
        assert!(
            stderr.contains("more than once") || stderr.contains("duplicate"),
            "stderr: {stderr}"
        );
    }

    #[test]
    fn test_tasks_from_json_file() {
        let mut f = NamedTempFile::with_suffix(".json").unwrap();
        write!(f, r#"[{{"TaskNumber": "1", "TaskMessage": "Hi!"}}]"#).unwrap();
        let tasks_arg = format!("file://{}", f.path().display());
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "TaskParamStep",
            "--tasks",
            &tasks_arg,
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("Running Task"), "stdout: {stdout}");
    }

    #[test]
    fn test_tasks_from_yaml_file() {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(f, "- TaskNumber: \"1\"\n  TaskMessage: \"Hi!\"\n").unwrap();
        let tasks_arg = format!("file://{}", f.path().display());
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "TaskParamStep",
            "--tasks",
            &tasks_arg,
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("Running Task"), "stdout: {stdout}");
    }

    #[test]
    fn test_tasks_not_a_list() {
        let tdir = templates_dir();
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "TaskParamStep",
            "--tasks",
            r#"{"TaskNumber": "1"}"#,
            "--extensions",
            "",
        ]);
        assert_ne!(code, 0, "non-list should fail");
        assert!(
            stderr.contains("list") || stderr.contains("must be"),
            "stderr: {stderr}"
        );
    }

    #[test]
    fn test_tasks_not_list_of_dicts() {
        let tdir = templates_dir();
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "TaskParamStep",
            "--tasks",
            "[1, 2, 3]",
            "--extensions",
            "",
        ]);
        assert_ne!(code, 0, "non-dict items should fail");
        assert!(
            stderr.contains("maps") || stderr.contains("must be"),
            "stderr: {stderr}"
        );
    }

    #[test]
    fn test_tasks_numeric_values_coerced() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "TaskParamStep",
            "--tasks",
            r#"[{"TaskNumber": 1, "TaskMessage": "Hi!"}]"#,
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "numeric coercion should succeed. stderr: {stderr}");
        assert!(stdout.contains("Running Task"), "stdout: {stdout}");
    }
}

mod summary_command {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_summary_basic_job() {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            f,
            r#"specificationVersion: "jobtemplate-2023-09"
name: TestJob
steps:
  - name: Step1
    script:
      actions:
        onRun:
          command: echo
          args: ["hello"]
"#
        )
        .unwrap();
        let (code, stdout, stderr) = run_cli(&["summary", f.path().to_str().unwrap()]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("Summary for 'TestJob'"), "stdout: {stdout}");
        assert!(stdout.contains("Total steps: 1"), "stdout: {stdout}");
        assert!(stdout.contains("Total tasks: 1"), "stdout: {stdout}");
        assert!(
            stdout.contains("'Step1' (1 total Tasks)"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_summary_with_parameters() {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            f,
            r#"specificationVersion: "jobtemplate-2023-09"
name: ParamJob
parameterDefinitions:
  - name: Greeting
    type: STRING
    default: Hello
steps:
  - name: S1
    script:
      actions:
        onRun:
          command: echo
"#
        )
        .unwrap();
        let (code, stdout, stderr) = run_cli(&["summary", f.path().to_str().unwrap()]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(
            stdout.contains("Greeting (STRING): Hello"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_summary_with_task_params() {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            f,
            r#"specificationVersion: "jobtemplate-2023-09"
name: TaskParamJob
steps:
  - name: Render
    parameterSpace:
      taskParameterDefinitions:
        - name: Frame
          type: INT
          range: "1-10"
    script:
      actions:
        onRun:
          command: echo
"#
        )
        .unwrap();
        let (code, stdout, stderr) = run_cli(&["summary", f.path().to_str().unwrap()]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("10 total Tasks"), "stdout: {stdout}");
        assert!(stdout.contains("Frame (INT)"), "stdout: {stdout}");
    }

    #[test]
    fn test_summary_step_filter() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "summary",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "TaskParamStep",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(
            stdout.contains("Summary for Step 'TaskParamStep'"),
            "stdout: {stdout}"
        );
        assert!(stdout.contains("Total tasks:"), "stdout: {stdout}");
    }

    #[test]
    fn test_summary_nonexistent_step() {
        let tdir = templates_dir();
        let (code, _stdout, stderr) = run_cli(&[
            "summary",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "NoSuchStep",
            "--extensions",
            "",
        ]);
        assert_ne!(code, 0);
        assert!(stderr.contains("does not exist"), "stderr: {stderr}");
    }

    #[test]
    fn test_summary_json_output() {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            f,
            r#"specificationVersion: "jobtemplate-2023-09"
name: JsonJob
steps:
  - name: S1
    script:
      actions:
        onRun:
          command: echo
"#
        )
        .unwrap();
        let (code, stdout, stderr) =
            run_cli(&["summary", f.path().to_str().unwrap(), "--output", "json"]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(
            stdout.contains(r#""status": "success""#),
            "stdout: {stdout}"
        );
        assert!(stdout.contains(r#""name": "JsonJob""#), "stdout: {stdout}");
    }

    #[test]
    fn test_summary_yaml_output() {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            f,
            r#"specificationVersion: "jobtemplate-2023-09"
name: YamlJob
parameterDefinitions:
  - name: Greeting
    type: STRING
    default: Hello
steps:
  - name: S1
    script:
      actions:
        onRun:
          command: echo
"#
        )
        .unwrap();
        let (code, stdout, stderr) =
            run_cli(&["summary", f.path().to_str().unwrap(), "--output", "yaml"]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("status: success"), "stdout: {stdout}");
        assert!(stdout.contains("name: YamlJob"), "stdout: {stdout}");
        assert!(stdout.contains("total_steps: 1"), "stdout: {stdout}");
        assert!(stdout.contains("total_tasks: 1"), "stdout: {stdout}");
        // Should include structured step data (not just top-level keys)
        assert!(
            stdout.contains("- name: S1"),
            "steps should be structured. stdout: {stdout}"
        );
    }

    #[test]
    fn test_summary_with_dependencies() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "summary",
            tdir.join("basic_dependency_job.yaml").to_str().unwrap(),
            "-p",
            "J=hello",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("dependencies"), "stdout: {stdout}");
    }

    #[test]
    fn test_summary_with_environments() {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            f,
            r#"specificationVersion: "jobtemplate-2023-09"
name: EnvJob
steps:
  - name: S1
    stepEnvironments:
      - name: MyEnv
        description: "A test environment"
        script:
          actions:
            onEnter:
              command: echo
              args: ["entering"]
    script:
      actions:
        onRun:
          command: echo
"#
        )
        .unwrap();
        let (code, stdout, stderr) = run_cli(&["summary", f.path().to_str().unwrap()]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("Total environments: 1"), "stdout: {stdout}");
        assert!(stdout.contains("MyEnv (from 'S1')"), "stdout: {stdout}");
        assert!(stdout.contains("A test environment"), "stdout: {stdout}");
    }

    #[test]
    fn test_summary_missing_required_param() {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            f,
            r#"specificationVersion: "jobtemplate-2023-09"
name: test
parameterDefinitions:
  - name: Required
    type: STRING
steps:
  - name: S1
    script:
      actions:
        onRun:
          command: echo
"#
        )
        .unwrap();
        let (code, _stdout, stderr) = run_cli(&["summary", f.path().to_str().unwrap()]);
        assert_ne!(code, 0);
        assert!(
            stderr.contains("missing") || stderr.contains("Required"),
            "stderr: {stderr}"
        );
    }

    #[test]
    fn test_summary_step_with_params() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "summary",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "TaskParamStep",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("Total tasks: 9"), "stdout: {stdout}");
        assert!(
            stdout.contains("Total task parameters: 2"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_summary_step_bare() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "summary",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "BareStep",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("Total tasks: 1"), "stdout: {stdout}");
        assert!(stdout.contains("Total environments: 0"), "stdout: {stdout}");
    }

    #[test]
    fn test_summary_with_combination_expr() {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            f,
            r#"specificationVersion: "jobtemplate-2023-09"
name: ComboJob
steps:
  - name: ComboStep
    parameterSpace:
      taskParameterDefinitions:
        - name: A
          type: INT
          range: "1-3"
        - name: B
          type: INT
          range: "1-3"
        - name: C
          type: INT
          range: "1-2"
      combination: "(A, B) * C"
    script:
      actions:
        onRun:
          command: echo
"#
        )
        .unwrap();
        let (code, stdout, stderr) = run_cli(&["summary", f.path().to_str().unwrap()]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("ComboStep"), "stdout: {stdout}");
        assert!(stdout.contains("A (INT)"), "stdout: {stdout}");
        assert!(stdout.contains("B (INT)"), "stdout: {stdout}");
        assert!(stdout.contains("C (INT)"), "stdout: {stdout}");
    }

    #[test]
    fn test_summary_job_with_root_envs() {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            f,
            r#"specificationVersion: "jobtemplate-2023-09"
name: RootEnvJob
jobEnvironments:
  - name: GlobalEnv
    variables:
      FOO: bar
steps:
  - name: S1
    script:
      actions:
        onRun:
          command: echo
"#
        )
        .unwrap();
        let (code, stdout, stderr) = run_cli(&["summary", f.path().to_str().unwrap()]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("Total environments: 1"), "stdout: {stdout}");
        assert!(stdout.contains("GlobalEnv"), "stdout: {stdout}");
    }

    #[test]
    fn test_summary_step_and_params() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "summary",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "-p",
            "Message=CustomMsg",
            "--step",
            "TaskParamStep",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("TaskParamStep"), "stdout: {stdout}");
        assert!(stdout.contains("Total tasks: 9"), "stdout: {stdout}");
    }
}

// ============================================================
// Group: All-steps topological sort
// ============================================================

mod all_steps_topo_sort {
    use super::*;

    #[test]
    fn test_all_steps_respects_dependency_order() {
        // Template defines StepC (depends on StepB), StepB (depends on StepA), StepA
        // Without topo sort, they'd run in definition order: C, B, A (wrong).
        // With topo sort, they must run: A, B, C.
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("reverse_dependency_order.yaml").to_str().unwrap(),
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        let a_pos = stdout
            .find("Running step 'StepA'")
            .expect("StepA should run");
        let b_pos = stdout
            .find("Running step 'StepB'")
            .expect("StepB should run");
        let c_pos = stdout
            .find("Running step 'StepC'")
            .expect("StepC should run");
        assert!(
            a_pos < b_pos,
            "StepA must run before StepB. stdout: {stdout}"
        );
        assert!(
            b_pos < c_pos,
            "StepB must run before StepC. stdout: {stdout}"
        );
    }
}

// ============================================================
// Python CLI Compatibility Tests
// ============================================================

mod python_compat {
    use super::*;

    #[test]
    fn test_job_param_long_flag_accepted() {
        // Python uses --job-param; Rust should accept it too
        let tdir = templates_dir();
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            tdir.join("basic.yaml").to_str().unwrap(),
            "--step",
            "First",
            "--job-param",
            "J=value1",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
    }

    #[test]
    fn test_job_param_short_flag_p() {
        // Both Python and Rust use -p
        let tdir = templates_dir();
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            tdir.join("basic.yaml").to_str().unwrap(),
            "--step",
            "First",
            "-p",
            "J=value1",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
    }

    #[test]
    fn test_summary_job_param_long_flag() {
        let tdir = templates_dir();
        let (code, _stdout, stderr) = run_cli(&[
            "summary",
            tdir.join("basic.yaml").to_str().unwrap(),
            "--job-param",
            "J=value1",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
    }

    #[test]
    fn test_task_param_tp_short_flag() {
        // Python uses -tp; Rust should recognize it as --task-param before
        // rejecting the selected step's missing parameter space.
        let tdir = templates_dir();
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "BareStep",
            "-tp",
            "Foo=bar",
            "--extensions",
            "",
        ]);
        assert_ne!(code, 0);
        assert_eq!(
            stderr.trim(),
            "ERROR: Step 'BareStep' does not define a parameterSpace; --task-param cannot be used."
        );
    }

    #[test]
    fn test_tasks_rejected_for_step_without_parameter_space() {
        let tdir = templates_dir();
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "BareStep",
            "--tasks",
            "[]",
            "--extensions",
            "",
        ]);
        assert_ne!(code, 0);
        assert_eq!(
            stderr.trim(),
            "ERROR: Step 'BareStep' does not define a parameterSpace; --tasks cannot be used."
        );
    }

    #[test]
    fn test_run_json_output_uses_chunks_run() {
        // Python uses "chunks_run" in JSON output; Rust should match
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "BareStep",
            "--output",
            "json",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(
            stdout.contains("\"chunks_run\""),
            "JSON output should use 'chunks_run' to match Python CLI. stdout: {stdout}"
        );
    }

    #[test]
    fn test_run_yaml_output_uses_chunks_run() {
        // Python uses "chunks_run" in YAML output; Rust should match
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "BareStep",
            "--output",
            "yaml",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(
            stdout.contains("chunks_run:"),
            "YAML output should use 'chunks_run' to match Python CLI. stdout: {stdout}"
        );
    }

    #[test]
    fn test_run_human_output_uses_chunks_run() {
        // Python uses "Chunks run:" in human output; Rust should match
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "BareStep",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(
            stdout.contains("Chunks run:"),
            "Human output should say 'Chunks run:' to match Python CLI. stdout: {stdout}"
        );
    }

    #[test]
    fn test_context_help_shows_job_param() {
        // Context-aware help should show --job-param, not --parameter
        let tdir = templates_dir();
        let (code, stdout, _stderr) = run_cli(&[
            "run",
            tdir.join("basic.yaml").to_str().unwrap(),
            "--extensions",
            "",
            "-h",
        ]);
        assert_eq!(code, 0);
        assert!(
            stdout.contains("--job-param"),
            "Context help should show --job-param. stdout: {stdout}"
        );
    }
}

// ============================================================
// Group: Interruption (SIGINT / Ctrl+Break) handling
// ============================================================

/// Deliver an interruption to the CLI process only (not the whole
/// test-process group).
///
/// Unix: SIGINT via `kill`. Windows: CTRL_BREAK_EVENT via
/// `GenerateConsoleCtrlEvent` — the CLI is spawned with
/// CREATE_NEW_PROCESS_GROUP so the event reaches only its group, and
/// because that flag disables Ctrl+C for the child, Ctrl+Break is the
/// only console event that can interrupt it.
fn send_interrupt(child: &std::process::Child) -> bool {
    #[cfg(unix)]
    {
        Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        use windows::Win32::System::Console::{GenerateConsoleCtrlEvent, CTRL_BREAK_EVENT};
        unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.id()).is_ok() }
    }
}

/// Spawn `openjd run <template> <args…>` with piped stdout, in its own
/// process group on Windows so a console control event reaches it alone.
fn spawn_run_piped(template: &std::path::Path, extra: &[&str]) -> std::process::Child {
    let mut args = vec!["run", template.to_str().unwrap()];
    args.extend_from_slice(extra);
    let mut cmd = cli_command(&args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn openjd")
}

mod interruption {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::Duration;

    /// End-to-end coverage for the RESULTS-phase rule "treat any observed
    /// interruption as a failed run" (specs/cli/run.md): an interrupted run
    /// must cancel the in-flight task, print the interruption message and
    /// failure summary, and exit with code 1.
    #[test]
    fn test_interrupt_during_task_fails_run() {
        let tdir = templates_dir();
        let template = tdir.join("job_interruptible_sleep.yaml");
        let mut cmd = cli_command(&["run", template.to_str().unwrap()]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
        }
        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn openjd");

        // Stream stdout from a thread so the wait below can time out instead
        // of blocking forever if the interruption is never acted on.
        let stdout = child.stdout.take().expect("stdout is piped");
        let (tx, rx) = mpsc::channel::<String>();
        let reader_thread = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });

        /// Collect lines until the marker appears (`Marker`), the sender
        /// disconnects because the CLI exited and closed stdout (`Eof`),
        /// or 60s pass without a line (`TimedOut`).
        #[derive(PartialEq, Debug)]
        enum RecvEnd {
            Marker,
            Eof,
            TimedOut,
        }
        fn recv_until(
            rx: &mpsc::Receiver<String>,
            collected: &mut String,
            marker: Option<&str>,
        ) -> RecvEnd {
            loop {
                match rx.recv_timeout(Duration::from_secs(60)) {
                    Ok(line) => {
                        collected.push_str(&line);
                        collected.push('\n');
                        if marker.is_some_and(|m| line.contains(m)) {
                            return RecvEnd::Marker;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return RecvEnd::Eof,
                    Err(mpsc::RecvTimeoutError::Timeout) => return RecvEnd::TimedOut,
                }
            }
        }

        // Interrupt only once the task subprocess is demonstrably running:
        // the signal handler is installed before any action starts.
        let mut collected = String::new();
        assert_eq!(
            recv_until(&rx, &mut collected, Some("READY_FOR_SIGNAL")),
            RecvEnd::Marker,
            "task never signaled readiness. output so far:\n{collected}"
        );
        if !send_interrupt(&child) {
            let _ = child.kill();
            let _ = child.wait();
            // On Windows, console ctrl events cannot be generated without a
            // console (e.g. some service contexts). Skip rather than fail.
            #[cfg(windows)]
            {
                eprintln!("SKIPPED: no console available to deliver CTRL_BREAK_EVENT");
                return;
            }
            #[cfg(not(windows))]
            panic!("failed to deliver SIGINT to the CLI process");
        }

        // Drain the remaining output; EOF means the process closed stdout.
        if recv_until(&rx, &mut collected, None) != RecvEnd::Eof {
            let _ = child.kill();
            let _ = child.wait();
            panic!("CLI did not exit within 60s of interruption. output:\n{collected}");
        }
        reader_thread.join().expect("reader thread panicked");
        let status = child.wait().expect("failed to wait for openjd");

        assert_eq!(
            status.code(),
            Some(1),
            "an interrupted run must exit with code 1. output:\n{collected}"
        );
        assert!(
            collected.contains("Interruption signal received."),
            "missing interruption message. output:\n{collected}"
        );
        assert!(
            collected.contains("Session ended with errors."),
            "an interrupted run must report failure. output:\n{collected}"
        );
        assert!(
            !collected.contains("EXIT_NORMAL"),
            "the task must be canceled, not run to completion. output:\n{collected}"
        );
    }
}

// ============================================================
// Group: RFC 0008 wrap actions (session-level rules + env params)
// ============================================================

mod wrap_actions {
    use super::*;

    /// RFC 0008 review finding F11: a wrap environment template's own
    /// `parameterDefinitions` must resolve inside its wrap hooks. The CLI
    /// converts env-template environments with the preprocessed parameter
    /// symtab so the frozen `resolved_symtab` carries `Param.*` values the
    /// hooks reference; `Session::run_task`'s step-filtered symbol table
    /// cannot supply them.
    #[test]
    fn test_run_wrap_env_template_parameters_resolve_in_hooks() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("wrap_simple_job.yaml").to_str().unwrap(),
            "--env",
            tdir.join("wrap_env_with_param.yaml").to_str().unwrap(),
        ]);
        assert_eq!(code, 0, "should succeed. stderr: {stderr}");
        assert!(
            stdout.contains("WrapParam Task FROM_ENV_PARAM"),
            "wrap hook must resolve the env template's own parameter. stdout: {stdout}"
        );
        assert!(
            !stdout.contains("TaskBody Ran"),
            "the wrapped task body must be replaced by the wrap hook. stdout: {stdout}"
        );
    }

    /// RFC 0008 review finding F8: the single-layer rule must hold across
    /// separately-supplied environment templates. The model validator can
    /// only see one template at a time, so the session rejects the second
    /// wrap-defining environment at enter time, before running anything
    /// under it.
    #[test]
    fn test_run_two_wrap_env_templates_rejected() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("wrap_simple_job.yaml").to_str().unwrap(),
            "--env",
            tdir.join("wrap_env_with_param.yaml").to_str().unwrap(),
            "--env",
            tdir.join("wrap_env_second.yaml").to_str().unwrap(),
        ]);
        assert_ne!(code, 0, "two wrap env templates must fail the run");
        let combined = format!("{stdout}\n{stderr}");
        assert!(
            combined.contains("at most one Environment defining wrap hooks"),
            "expected the RFC 0008 single-layer rejection; got:\n{combined}"
        );
        assert!(
            !stdout.contains("WrapB Task") && !stdout.contains("TaskBody Ran"),
            "no task may run in a session with two wrap layers. stdout: {stdout}"
        );
    }

    /// RFC 0008 "Lifecycle and cleanup guarantees", scenario row 1: a
    /// failed `onWrapEnvEnter` for an inner environment still runs that
    /// environment's `onWrapEnvExit` and the wrapping environment's own
    /// `onExit` before the run ends, and the failing hook's exit code is
    /// the one surfaced for the failure.
    #[test]
    fn test_run_failed_wrap_enter_still_runs_wrap_exit_and_own_exit() {
        let tdir = templates_dir();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            tdir.join("wrap_job_with_inner_env.yaml").to_str().unwrap(),
            "--env",
            tdir.join("wrap_env_failing_enter.yaml").to_str().unwrap(),
        ]);
        assert_ne!(code, 0, "the failed enter must fail the run");
        let combined = format!("{stdout}\n{stderr}");
        for expected in [
            "WrapEnv Own Enter",
            "Wrap Enter Failing",
            "Process exited with code: 7",
            "Wrap Exit Ran for InnerEnv",
            "WrapEnv Own Exit",
        ] {
            assert!(
                combined.contains(expected),
                "expected '{expected}' in output; got:\n{combined}"
            );
        }
        for forbidden in ["TaskBody Ran", "Inner Enter Body", "Inner Exit Body"] {
            assert!(
                !combined.contains(forbidden),
                "'{forbidden}' must not run after the failed enter; got:\n{combined}"
            );
        }
    }
}

/// The CLI's opinionated resolved-argument cap: `openjd check` rejects a
/// template whose argument is guaranteed to exceed the CLI's uniform 32K
/// default (see `common::DEFAULT_MAX_ARG_LEN`), while the library
/// default applies no cap.
mod check_resolved_arg_cap {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Mirror of `common::DEFAULT_MAX_ARG_LEN` (the binary crate's
    /// internals are not linkable from an integration test).
    const DEFAULT_MAX_ARG_LEN: usize = 32 * 1024;

    fn template_with_arg(arg: &str) -> NamedTempFile {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            f,
            r#"specificationVersion: "jobtemplate-2023-09"
extensions: [EXPR]
name: test
steps:
  - name: s1
    script:
      actions:
        onRun:
          command: echo
          args:
            - "{arg}"
"#
        )
        .unwrap();
        f
    }

    #[test]
    fn check_rejects_arg_over_default_cap() {
        // 2,000,000 characters is far over the CLI's uniform 32K default.
        let f = template_with_arg("{{ 'A' * 2000000 }}");
        let (code, _stdout, stderr) = run_cli(&["check", f.path().to_str().unwrap()]);
        assert_ne!(code, 0, "over-cap arg must fail check");
        let expected = format!(
            "steps[0] -> script -> actions -> onRun -> args[0]:\n\tresolves to at least 2000000 characters, exceeding the maximum of {DEFAULT_MAX_ARG_LEN}."
        );
        assert!(
            stderr.contains(&expected),
            "expected {expected:?} in stderr:\n{stderr}"
        );
    }

    #[test]
    fn check_accepts_arg_under_default_cap() {
        let f = template_with_arg("{{ 'A' * 1000 }}");
        let (code, stdout, stderr) = run_cli(&["check", f.path().to_str().unwrap()]);
        assert_eq!(code, 0, "under-cap arg must pass check. stderr: {stderr}");
        assert!(
            stdout.contains("passes validation checks"),
            "stdout: {stdout}"
        );
    }
}

// ============================================================
// Group: RFC 0009 Services (openjd run orchestrates Service Sessions)
// ============================================================

mod services {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::sync::mpsc;
    use std::time::Duration;
    use tempfile::TempDir;

    fn run_service_template(name: &str, extra: &[&str]) -> (i32, String, String) {
        let template = templates_dir().join(name);
        let mut args = vec!["run", template.to_str().unwrap()];
        args.extend_from_slice(extra);
        run_cli(&args)
    }

    fn read_trace(path: &std::path::Path) -> Vec<String> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Byte offset of `needle` in `haystack`, panicking with the output when
    /// absent — used to assert the relative order of log lines.
    fn pos(haystack: &str, needle: &str) -> usize {
        haystack
            .find(needle)
            .unwrap_or_else(|| panic!("missing {needle:?} in output:\n{haystack}"))
    }

    /// RFC 0009 example 1: a Service listed by both Steps (scope Steps
    /// First, Second, §9.1) with a TCP_CONNECT health check is READY before
    /// the first Task, is reached by Tasks of two Steps through
    /// `Service.Store.main.connectAddress` / `.port`, and is stopped after
    /// the last Step (constraints 1, 3, 6).
    #[test]
    fn test_job_service_tcp_consumed_by_two_steps() {
        let dir = TempDir::new().unwrap();
        let trace = dir.path().join("trace.txt");
        let (code, stdout, stderr) = run_service_template(
            "service_job_tcp.yaml",
            &["-p", &format!("TraceFile={}", trace.display())],
        );
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            stdout.contains(
                "Service 'Store' (scope: Steps First, Second) endpoints: main -> 127.0.0.1:"
            ),
            "{stdout}"
        );
        assert!(stdout.contains("Service 'Store' is READY"), "{stdout}");
        assert!(stdout.contains("TASK_REPLY ECHO:First-1"), "{stdout}");
        assert!(stdout.contains("TASK_REPLY ECHO:First-2"), "{stdout}");
        assert!(stdout.contains("TASK_REPLY ECHO:Second"), "{stdout}");
        assert!(
            pos(&stdout, "Running step 'First'") < pos(&stdout, "Starting Service: Store")
                && pos(&stdout, "Service 'Store' is READY") < pos(&stdout, "TASK_REPLY"),
            "the Service starts when Step First is about to run and is READY before any \
             Task:\n{stdout}"
        );
        assert!(
            pos(&stdout, "Running step 'Second'") < pos(&stdout, "Stopping Service: Store"),
            "the Service must outlive every Step in its scope:\n{stdout}"
        );
        assert!(stdout.contains("Service 'Store' stopped"), "{stdout}");
        assert!(stdout.contains("Chunks run: 3"), "{stdout}");
        assert_eq!(
            read_trace(&trace),
            vec!["task First-1 ECHO:First-1", "task First-2 ECHO:First-2"]
        );
    }

    /// RFC 0009 §9.2 item 3 / §9 item 6 (conformance
    /// `service-udp-port-echo`): a Service whose only port is UDP is
    /// allocated a UDP port on loopback, reported with a `/udp` suffix in
    /// the endpoints line, becomes READY through its STDOUT check, and is
    /// reached by the Tasks with datagrams to `connectAddress`:`port`.
    #[test]
    fn test_job_service_udp_echo() {
        let (code, stdout, stderr) = run_service_template("service_job_udp.yaml", &[]);
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        let line = stdout
            .lines()
            .find(|l| l.contains("Service 'Udp' (scope: Step Ping) endpoints: "))
            .unwrap_or_else(|| panic!("no endpoints line in:\n{stdout}"));
        assert!(
            line.contains("dgram -> 127.0.0.1:") && line.trim_end().ends_with("/udp"),
            "the UDP port is suffixed /udp: {line}"
        );
        assert!(stdout.contains("UDP_BOUND"), "{stdout}");
        assert!(stdout.contains("Service 'Udp' is READY"), "{stdout}");
        assert!(stdout.contains("TASK_REPLY udp-echo:Ping-1"), "{stdout}");
        assert!(stdout.contains("TASK_REPLY udp-echo:Ping-2"), "{stdout}");
        assert!(
            pos(&stdout, "Service 'Udp' is READY") < pos(&stdout, "TASK_REPLY"),
            "{stdout}"
        );
        assert!(stdout.contains("Service 'Udp' stopped"), "{stdout}");
        assert!(stdout.contains("Chunks run: 2"), "{stdout}");
    }

    /// RFC 0009 "A metrics sink with a UDP ingest port": a Service with
    /// a UDP port and a TCP port and no `healthCheck` becomes READY
    /// through the default TCP_CONNECT, which probes the TCP port only
    /// (§9 item 6, §9.3 item 2). The endpoints line suffixes only the UDP
    /// port; the Task reaches both ports.
    #[test]
    fn test_job_service_mixed_tcp_udp_default_health_check_probes_tcp_only() {
        let (code, stdout, stderr) = run_service_template("service_job_tcp_udp.yaml", &[]);
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        let line = stdout
            .lines()
            .find(|l| l.contains("Service 'Metrics' (scope: Step Report) endpoints: "))
            .unwrap_or_else(|| panic!("no endpoints line in:\n{stdout}"));
        let (ingest, api) = line
            .split_once("endpoints: ")
            .unwrap()
            .1
            .split_once(", ")
            .unwrap();
        assert!(
            ingest.starts_with("ingest -> 127.0.0.1:") && ingest.trim_end().ends_with("/udp"),
            "{line}"
        );
        assert!(
            api.starts_with("api -> 127.0.0.1:") && !api.contains("/udp"),
            "{line}"
        );
        assert!(stdout.contains("SINK_LISTENING"), "{stdout}");
        assert!(stdout.contains("Service 'Metrics' is READY"), "{stdout}");
        assert!(
            stdout.contains("TASK_REPLY COUNT=2 frame-1,frame-2"),
            "{stdout}"
        );
        assert!(stdout.contains("Chunks run: 1"), "{stdout}");
    }

    /// RFC 0009 example 2: a Service scoped to one Step with a STDOUT health check starts
    /// (onEnter, onRun) before the Step's first Task and stops (onExit) once
    /// the Step's three Tasks are done, before the next Step runs
    /// (constraints 3, 6, 7).
    #[test]
    fn test_step_service_stdout_lifecycle_brackets_the_step() {
        let dir = TempDir::new().unwrap();
        let trace = dir.path().join("trace.txt");
        let (code, stdout, stderr) = run_service_template(
            "service_step_stdout.yaml",
            &["-p", &format!("TraceFile={}", trace.display())],
        );
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            stdout.contains("Service 'Coordinator' is READY: coordinator listening"),
            "{stdout}"
        );
        assert_eq!(
            read_trace(&trace),
            vec![
                "service enter",
                "service run",
                "task 1 assignment 1",
                "task 2 assignment 2",
                "task 3 assignment 3",
                "service exit",
                "after task",
            ]
        );
        assert!(
            pos(&stdout, "Stopping Service: Coordinator") < pos(&stdout, "Running step 'After'"),
            "{stdout}"
        );
        assert!(stdout.contains("Chunks run: 4"), "{stdout}");
    }

    /// COMMAND health check, phase 1: `onHealthCheck` runs concurrently with
    /// `onRun` (tagged `[onHealthCheck]`), fails while the connection is
    /// refused, and makes the Service READY when it exits 0.
    #[test]
    fn test_command_health_check() {
        let (code, stdout, stderr) = run_service_template("service_command_health_check.yaml", &[]);
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            stdout.contains(
                "health check: COMMAND (readinessTimeoutSeconds 60, readinessIntervalSeconds 1, \
                 healthIntervalSeconds 30, failureThreshold 3)"
            ),
            "{stdout}"
        );
        // Every line of the Service Session carries the Service tag; the
        // concurrent check's lines carry its action tag after it (RFC 0009
        // rule 3), while onRun's carry the Service tag alone.
        assert!(
            stdout.contains("[Service Slow] [onHealthCheck] CHECK_OK"),
            "{stdout}"
        );
        assert!(
            stdout.contains("\t[Service Slow] SLOW_LISTENING"),
            "{stdout}"
        );
        assert!(stdout.contains("Service 'Slow' is READY"), "{stdout}");
        assert!(
            pos(&stdout, "SLOW_LISTENING") < pos(&stdout, "Service 'Slow' is READY"),
            "{stdout}"
        );
        assert!(stdout.contains("TASK_GOT hello"), "{stdout}");
    }

    /// RFC 0009 `<ServiceHealthCheck>` after READY, constraint 11, "Failure
    /// and restart": a COMMAND probe fails `failureThreshold` times after
    /// Task 1 makes the service sick; the instance is UNHEALTHY, `onRun` is
    /// canceled, and with `maxAttempts: 1` / `KEEP` a second instance is
    /// relaunched in the same Service Session and serves Tasks 2 and 3.
    /// Task 1 completes against instance 1 (conformance
    /// `service-health-command-fails-after-ready-relaunches`).
    #[test]
    fn test_health_command_fails_after_ready_relaunches() {
        let dir = TempDir::new().unwrap();
        let (code, stdout, stderr) = run_service_template(
            "service_health_command_fails.yaml",
            &["-p", &format!("MarkerDir={}", dir.path().display())],
        );
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        let unhealthy =
            "Service 'Store' (scope: Step Work) is UNHEALTHY: 2 consecutive health probes \
                         failed (failureThreshold: 2); last probe: onHealthCheck exit code: 1";
        assert!(stdout.contains(unhealthy), "{stdout}");
        assert!(
            stdout.contains(
                "Service 'Store' (scope: Step Work) is UNREADY: instance UNHEALTHY: 2 consecutive \
                 health probes failed (failureThreshold: 2); last probe: onHealthCheck exit \
                 code: 1 (completedTasks: KEEP)"
            ),
            "{stdout}"
        );
        assert!(
            stdout.contains(
                "Relaunching Service 'Store' onRun in its Service Session (relaunch 1 of 1): \
                 instance UNHEALTHY"
            ),
            "{stdout}"
        );
        // The Session's own lines: the probe failures and the forced cancel.
        assert!(
            stdout.contains("[Service Store] [onHealthCheck] CHECK_UNHEALTHY"),
            "{stdout}"
        );
        assert!(!stdout.contains("Canceling the running Task"), "{stdout}");
        for needle in [
            "INSTANCE 1 LISTENING",
            "TASK 1 GOT INSTANCE 1",
            "TASK 1 INSTANCE 1 DONE",
            "INSTANCE 2 LISTENING",
            "TASK 2 INSTANCE 2 DONE",
            "TASK 3 INSTANCE 2 DONE",
        ] {
            assert!(stdout.contains(needle), "missing {needle}: {stdout}");
        }
        assert!(!stdout.contains("TASK 2 GOT INSTANCE 1"), "{stdout}");
        assert!(
            pos(&stdout, unhealthy) < pos(&stdout, "INSTANCE 2 LISTENING"),
            "{stdout}"
        );
        assert!(stdout.contains("Chunks run: 3"), "{stdout}");
    }

    /// RFC 0009 `<ServiceHealthCheck>` item 6: two failures below a
    /// `failureThreshold` of 3, then a success, leave the instance READY;
    /// nothing is relaunched and every Task is served by instance 1
    /// (conformance `service-health-threshold-tolerates-blips`).
    #[test]
    fn test_health_threshold_tolerates_blips() {
        let dir = TempDir::new().unwrap();
        let (code, stdout, stderr) = run_service_template(
            "service_health_threshold_blips.yaml",
            &["-p", &format!("MarkerDir={}", dir.path().display())],
        );
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        for needle in [
            "[Service Store] [onHealthCheck] CHECK_BLIP 1",
            "[Service Store] [onHealthCheck] CHECK_BLIP 2",
            "[Service Store] [onHealthCheck] CHECK_OK 3",
            "TASK 1 INSTANCE 1 DONE",
            "TASK 2 INSTANCE 1 DONE",
            "TASK 3 INSTANCE 1 DONE",
        ] {
            assert!(stdout.contains(needle), "missing {needle}: {stdout}");
        }
        assert!(!stdout.contains("INSTANCE 2"), "{stdout}");
        assert!(!stdout.contains("UNHEALTHY"), "{stdout}");
        assert!(!stdout.contains("Relaunching"), "{stdout}");
        assert!(stdout.contains("Chunks run: 3"), "{stdout}");
    }

    /// TCP_CONNECT after READY: the service closes its listener and hangs.
    /// Two refused connections make the instance UNHEALTHY; `onRun` is
    /// canceled; with `maxAttempts` 0 the Service is FAILED and the Job
    /// fails before Task 2; `onExit` still runs (conformance
    /// `service-health-tcp-port-closed-while-process-hangs`).
    #[test]
    fn test_health_tcp_port_closed_while_process_hangs_fails_the_job() {
        let (code, stdout, stderr) =
            run_service_template("service_health_tcp_port_closed.yaml", &[]);
        assert_eq!(code, 1, "stdout:\n{stdout}\nstderr:\n{stderr}");
        for needle in [
            "STORE_LISTENING",
            "TASK 1 GOT hello",
            "STORE_LISTENER_CLOSED",
            "STORE_EXIT_RAN",
        ] {
            assert!(stdout.contains(needle), "missing {needle}: {stdout}");
        }
        assert!(!stdout.contains("TASK 2 GOT"), "{stdout}");
        assert!(
            stdout.contains(
                "Service 'Store' (scope: Step Work) is UNHEALTHY: 2 consecutive health probes failed \
                 (failureThreshold: 2); last probe: TCP connect to port 'main' (127.0.0.1:"
            ),
            "{stdout}"
        );
        assert!(
            stdout.contains(
                "Service 'Store' (scope: Step Work) is FAILED: instance UNHEALTHY: 2 consecutive"
            ),
            "{stdout}"
        );
        assert!(
            stdout.contains("0 of 0 relaunch(es) used (restartPolicy.maxAttempts)"),
            "{stdout}"
        );
        assert!(
            stderr.contains("ERROR: Service 'Store' (scope: Step Work) failed: instance UNHEALTHY"),
            "{stderr}"
        );
        assert!(stdout.contains("Session ended with errors."), "{stdout}");
    }

    /// STDOUT with `healthIntervalSeconds`: the heartbeat stops after Task
    /// 1; two silent intervals make the instance UNHEALTHY and, with
    /// `maxAttempts` 0, fail the Job (conformance
    /// `service-health-stdout-heartbeat-missed`).
    #[test]
    fn test_health_stdout_heartbeat_missed_fails_the_job() {
        let (code, stdout, stderr) =
            run_service_template("service_health_stdout_heartbeat_missed.yaml", &[]);
        assert_eq!(code, 1, "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            stdout.contains(
                "health check: STDOUT (readinessTimeoutSeconds 60, healthIntervalSeconds 1, \
                 failureThreshold 2)"
            ),
            "{stdout}"
        );
        assert!(
            stdout.contains("Service 'Store' is READY: beat"),
            "{stdout}"
        );
        for needle in [
            "TASK 1 GOT hello",
            "STORE_HEARTBEAT_STOPPED",
            "STORE_EXIT_RAN",
        ] {
            assert!(stdout.contains(needle), "missing {needle}: {stdout}");
        }
        assert!(!stdout.contains("TASK 2 GOT"), "{stdout}");
        assert!(
            stdout.contains(
                "Service 'Store' (scope: Step Work) is UNHEALTHY: 2 consecutive health probes failed \
                 (failureThreshold: 2); last probe: no openjd_service_ready line within 1s"
            ),
            "{stdout}"
        );
        assert!(
            stdout.contains("Failed Service: Store (scope: Step Work): instance UNHEALTHY"),
            "{stdout}"
        );
    }

    /// STDOUT without `healthIntervalSeconds`: no heartbeat is expected, so
    /// a service that prints `openjd_service_ready` once and is then silent
    /// stays READY for every Task (conformance
    /// `service-health-stdout-no-heartbeat-configured`).
    #[test]
    fn test_health_stdout_no_heartbeat_configured_runs_every_task() {
        let (code, stdout, stderr) =
            run_service_template("service_health_stdout_no_heartbeat.yaml", &[]);
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            stdout.contains(
                "health check: STDOUT (readinessTimeoutSeconds 60, no heartbeat after READY)"
            ),
            "{stdout}"
        );
        for needle in [
            "STORE_READY_ONCE",
            "TASK 1 SERVED AS 1",
            "TASK 2 SERVED AS 2",
            "TASK 3 SERVED AS 3",
        ] {
            assert!(stdout.contains(needle), "missing {needle}: {stdout}");
        }
        assert!(!stdout.contains("UNHEALTHY"), "{stdout}");
        assert!(stdout.contains("Chunks run: 3"), "{stdout}");
    }

    /// Constraint 2: a Service that lists `service:Back` and reads Back's
    /// endpoint starts only once Back is READY, and sees its endpoint.
    #[test]
    fn test_service_depending_on_an_earlier_service_starts_after_it() {
        let (code, stdout, stderr) = run_service_template("service_reference_chain.yaml", &[]);
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            pos(&stdout, "Service 'Back' is READY") < pos(&stdout, "Starting Service: Front"),
            "Front must not start before Back is READY:\n{stdout}"
        );
        assert!(
            stdout.contains("FRONT_GOT_FROM_BACK BACK_VALUE"),
            "{stdout}"
        );
        assert!(stdout.contains("TASK_GOT front:BACK_VALUE"), "{stdout}");
        // Constraint 4: stopped in reverse start order.
        assert!(
            pos(&stdout, "Stopping Service: Front") < pos(&stdout, "Stopping Service: Back"),
            "{stdout}"
        );
    }

    /// Template Schemas §9.1 rule 1 / constraints 3 and 6: an opaque
    /// consumer. Step Use lists `service:Sidecar` and never references
    /// `Service.Sidecar.*`; the dependency alone puts it in the scope. The
    /// Sidecar is READY before Use's Task runs and is stopped (onExit run)
    /// once Use completes, before Unrelated runs.
    #[test]
    fn test_step_depending_on_a_service_without_referencing_it_is_in_its_scope() {
        let dir = TempDir::new().unwrap();
        let marker = dir.path().join("sidecar.up");
        let (code, stdout, stderr) = run_service_template(
            "service_dependency_without_reference.yaml",
            &["-p", &format!("MarkerFile={}", marker.display())],
        );
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            stdout.contains("Service 'Sidecar' (scope: Step Use) endpoints: main -> "),
            "{stdout}"
        );
        assert!(
            pos(&stdout, "Service 'Sidecar' is READY") < pos(&stdout, "TASK_SEES_SIDECAR True"),
            "{stdout}"
        );
        assert!(
            pos(&stdout, "TASK_SEES_SIDECAR True") < pos(&stdout, "Stopping Service: Sidecar")
                && pos(&stdout, "[Service Sidecar] SIDECAR_EXIT")
                    < pos(&stdout, "Service 'Sidecar' stopped")
                && pos(&stdout, "Service 'Sidecar' stopped")
                    < pos(&stdout, "Running step 'Unrelated'"),
            "the Sidecar stops once Use completes, before Unrelated:\n{stdout}"
        );
        assert!(stdout.contains("UNRELATED_SEES_SIDECAR False"), "{stdout}");
        assert!(!marker.exists(), "onExit removed the marker");
    }

    /// §9 item 4 / constraints 2 and 4 / §9.1 rule 2: a Service -> Service
    /// dependency chain declared with no `Service.*` reference. Front lists
    /// service:Mid, Mid lists service:Back, Use lists service:Front. Scope
    /// flows through the chain (every Service is scoped to Step Use), Back
    /// is READY before Mid starts and Mid before Front, and they stop in
    /// reverse: Front, Mid, Back.
    #[test]
    fn test_service_dependency_chain_without_references_orders_start_and_stop() {
        let dir = TempDir::new().unwrap();
        let trace = dir.path().join("trace.txt");
        let (code, stdout, stderr) = run_service_template(
            "service_dependency_chain_no_reference.yaml",
            &["-p", &format!("TraceFile={}", trace.display())],
        );
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        for svc in ["Back", "Mid", "Front"] {
            assert!(
                stdout.contains(&format!(
                    "Service '{svc}' (scope: Step Use) endpoints: main -> "
                )),
                "{svc} is scoped to Use through the chain:\n{stdout}"
            );
        }
        assert!(
            pos(&stdout, "Service 'Back' is READY") < pos(&stdout, "Starting Service: Mid")
                && pos(&stdout, "Service 'Mid' is READY") < pos(&stdout, "Starting Service: Front")
                && pos(&stdout, "Service 'Front' is READY") < pos(&stdout, "TASK_RAN"),
            "{stdout}"
        );
        assert!(
            pos(&stdout, "Stopping Service: Front") < pos(&stdout, "Stopping Service: Mid")
                && pos(&stdout, "Stopping Service: Mid") < pos(&stdout, "Stopping Service: Back"),
            "{stdout}"
        );
        assert_eq!(
            read_trace(&trace),
            [
                "back launch",
                "mid launch",
                "front launch",
                "task",
                "front exit",
                "mid exit",
                "back exit",
            ]
        );
    }

    /// Template Schemas §3.1 constraint 4 (RFC 0009): the `:` constraint on
    /// a Step's name is gated on SERVICE. Without the extension,
    /// `service:Store` is an ordinary Step name and `dependsOn: service:Store`
    /// names that Step, which runs first.
    #[test]
    fn test_colon_in_step_name_is_a_step_name_without_service() {
        let (code, stdout, stderr) =
            run_service_template("service_colon_step_name_without_service.yaml", &[]);
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            pos(&stdout, "STORE_STEP_RAN") < pos(&stdout, "USE_STEP_RAN"),
            "{stdout}"
        );
        assert!(stdout.contains("Running step 'service:Store'"), "{stdout}");
        // `check` and `summary` agree.
        let template = templates_dir().join("service_colon_step_name_without_service.yaml");
        let (code, _stdout, stderr) = run_cli(&["check", template.to_str().unwrap()]);
        assert_eq!(code, 0, "{stderr}");
    }

    /// Template Schemas §3.1 constraint 4 / §9.9 item 12: with SERVICE
    /// declared a Step's name may not contain `:`.
    #[test]
    fn test_colon_in_step_name_is_rejected_with_service() {
        let template = templates_dir().join("service_colon_step_name_invalid.yaml");
        let (code, stdout, stderr) = run_cli(&["check", template.to_str().unwrap()]);
        assert_ne!(code, 0, "stdout:\n{stdout}");
        assert!(
            stderr.contains(
                "steps[0] -> name:\n\tmust not contain ':' when the SERVICE extension is used, so \
                 that a dependsOn value beginning 'service:' can only name a Service (Template \
                 Schemas §3.1 constraint 4)."
            ),
            "{stderr}"
        );
    }

    /// Template Schemas §9 scope rule 3 / §9.9 item 1: a Step that references
    /// a Service it does not list in `dependencies` is rejected, and the
    /// message names the missing entry.
    #[test]
    fn test_reference_without_dependency_is_rejected_naming_the_missing_entry() {
        let template = templates_dir().join("service_reference_without_dependency_invalid.yaml");
        let (code, stdout, stderr) = run_cli(&["check", template.to_str().unwrap()]);
        assert_ne!(code, 0, "stdout:\n{stdout}");
        assert!(
            stderr.contains(
                "steps[1] -> script -> actions -> onRun -> args[2]:\n\tFailed to parse \
                 interpolation expression at [0, 29]. Step 'Render' references \
                 Service.Cache.main.port but does not list service:Cache in dependencies."
            ),
            "{stderr}"
        );
    }

    /// Constraint 2: Services neither of which depends on the other start
    /// concurrently — each takes ~2 s to become READY and both launch before
    /// either is READY.
    #[test]
    fn test_independent_services_start_concurrently() {
        let dir = TempDir::new().unwrap();
        let trace = dir.path().join("trace.txt");
        let (code, stdout, stderr) = run_service_template(
            "service_concurrent_start.yaml",
            &["-p", &format!("TraceFile={}", trace.display())],
        );
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        let lines = read_trace(&trace);
        let stamp = |event: &str| -> f64 {
            lines
                .iter()
                .find(|l| l.starts_with(event))
                .and_then(|l| l.rsplit(' ').next())
                .and_then(|t| t.parse().ok())
                .unwrap_or_else(|| panic!("missing {event:?} in trace {lines:?}"))
        };
        assert!(
            stamp("Beta launch") < stamp("Alpha ready")
                && stamp("Alpha launch") < stamp("Beta ready"),
            "launches must overlap: {lines:?}"
        );
        assert!(stdout.contains("TASK_RAN"), "{stdout}");
    }

    /// RFC 0009 example 3: an external Service from an Environment Template
    /// attached with `--environment`, published to Tasks of a Job Template
    /// that does not use SERVICE through a TASK-scoped Environment's
    /// variables; the template's own parameter reaches the Service.
    #[test]
    fn test_external_service_from_environment_template() {
        let tdir = templates_dir();
        let env_path = tdir.join("service_external_cache.yaml");
        let env_path = env_path.to_str().unwrap();
        let (code, stdout, stderr) = run_service_template(
            "service_external_consumer.yaml",
            &[
                "--environment",
                env_path,
                "-p",
                "CachePayload=from-the-queue",
            ],
        );
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        // An external Service is logged with its document.
        assert!(
            pos(
                &stdout,
                &format!("Service 'Cache' (from {env_path}) is READY")
            ) < pos(&stdout, "Entering Environment: CacheClient"),
            "{stdout}"
        );
        assert!(
            stdout.contains("FRAME 1 CACHE_SAYS from-the-queue"),
            "{stdout}"
        );
        // An external Service's lines are tagged with its document too.
        assert!(
            stdout.contains(&format!(
                "\t[Service Cache (from {env_path})] CACHE_LISTENING"
            )),
            "{stdout}"
        );
        assert!(
            stdout.contains("FRAME 2 CACHE_SAYS from-the-queue"),
            "{stdout}"
        );
        assert!(
            pos(&stdout, "Exiting Environment: CacheClient")
                < pos(
                    &stdout,
                    &format!("Stopping Service: Cache (from {env_path})")
                ),
            "{stdout}"
        );
    }

    /// Exploratory report bug B1 / Template Schemas §1.2 item 3: an attached
    /// Environment Template's strings are evaluated under *its* extensions.
    /// The queue's client Environment composes `KV_ADDR` with
    /// `join_host_port` (a `SERVICE` function) in `variables`, `onEnter`,
    /// and `onExit`; the Job Template declares no extensions at all. Before
    /// the fix the run failed with `Failed to resolve env var 'KV_ADDR':
    /// Unknown function: 'join_host_port'`.
    #[test]
    fn test_attached_environment_uses_its_own_extensions_not_the_jobs() {
        let tdir = templates_dir();
        let env_path = tdir.join("service_external_kv_join.yaml");
        let env_path = env_path.to_str().unwrap();
        let (code, stdout, stderr) = run_service_template(
            "service_external_kv_consumer.yaml",
            &["--environment", env_path],
        );
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            !stderr.contains("Unknown function: 'join_host_port'"),
            "stderr:\n{stderr}"
        );
        // variables, onEnter, and onExit of the attached Environment all
        // resolved `join_host_port`, and the Task read the composed value.
        let port_of = |needle: &str| -> String {
            let at = pos(&stdout, needle) + needle.len();
            stdout[at..]
                .trim_start()
                .split(['\n', '\r'])
                .next()
                .unwrap()
                .rsplit(':')
                .next()
                .unwrap()
                .to_string()
        };
        let enter_port = port_of("KVCLIENT_ENTER 127.0.0.1:");
        let exit_port = port_of("KVCLIENT_EXIT 127.0.0.1:");
        assert_eq!(enter_port, exit_port, "{stdout}");
        assert!(
            enter_port.parse::<u16>().is_ok(),
            "port in {enter_port:?}:\n{stdout}"
        );
        assert!(stdout.contains("TASK_GOT KV_SAYS hello"), "{stdout}");
        assert!(
            pos(&stdout, &format!("Service 'Kv' (from {env_path}) is READY"))
                < pos(&stdout, "Entering Environment: KvClient"),
            "{stdout}"
        );
        assert!(stdout.contains("Chunks run: 1"), "{stdout}");
    }

    /// The inverse of the previous test: a Job Template that declares
    /// `SERVICE` attaches a plain `[EXPR]`-only Environment Template. The
    /// attached Environment is evaluated under its own profile (its `upper()`
    /// works), the Job's Service and Tasks under the Job's, and the Service
    /// Session enters the attached Job Environment too (default `runScope`).
    #[test]
    fn test_plain_environment_template_attached_to_a_service_job() {
        let dir = TempDir::new().unwrap();
        let trace = dir.path().join("trace.txt");
        let tdir = templates_dir();
        let env_path = tdir.join("env_plain_expr_greeting.yaml");
        let env_path = env_path.to_str().unwrap();
        let (code, stdout, stderr) = run_service_template(
            "service_job_tcp.yaml",
            &[
                "--environment",
                env_path,
                "-p",
                &format!("TraceFile={}", trace.display()),
            ],
        );
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        // Entered once in the Service Session (before the Service is READY)
        // and once in the Task Session. Store's scope is Steps First, Second,
        // so its Service Session opens once Step First is about to run —
        // after the Task Session entered the Job Environments.
        assert_eq!(
            stdout
                .matches("GREETING_ENTER HELLO FROM THE QUEUE")
                .count(),
            2,
            "{stdout}"
        );
        assert!(
            pos(&stdout, "Starting Service: Store")
                < pos(&stdout, "[Service Store] GREETING_ENTER")
                && pos(&stdout, "[Service Store] GREETING_ENTER")
                    < pos(&stdout, "Service 'Store' is READY"),
            "{stdout}"
        );
        assert!(stdout.contains("TASK_REPLY ECHO:Second"), "{stdout}");
        assert!(stdout.contains("Chunks run: 3"), "{stdout}");
    }

    /// Exploratory report stumble S6: a Service's `onEnter` prints
    /// `openjd_redacted_env: SECRET=hunter2` but the Job Template declares no
    /// `REDACTED_ENV_VARS`. The directive is dropped, and the run log says
    /// so with one warning in the Service Session's output stream (tagged
    /// `[Service Vault]`, naming `SECRET` and never `hunter2`), the Service's
    /// own `onRun` sees `SECRET` unset while the plain `openjd_env` next to
    /// it is honored, and the Task — which never sees a Service's variables —
    /// sees it unset too.
    #[test]
    fn test_service_on_enter_redacted_env_without_the_extension_warns() {
        let (code, stdout, stderr) =
            run_service_template("service_redacted_env_no_extension.yaml", &[]);
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        let warning = "[Service Vault] Received openjd_redacted_env for 'SECRET' but the \
                       REDACTED_ENV_VARS extension is not declared; the variable is not set.";
        assert_eq!(stdout.matches(warning).count(), 1, "{stdout}");
        // The warning comes with the redacted directive line, before onRun.
        assert!(
            pos(&stdout, warning)
                < pos(
                    &stdout,
                    "[Service Vault] openjd_redacted_env: SECRET=********"
                )
                && pos(&stdout, warning) < pos(&stdout, "VAULT_SECRET UNSET"),
            "{stdout}"
        );
        assert!(!stdout.contains("hunter2"), "{stdout}");
        assert!(!stderr.contains("hunter2"), "{stderr}");
        assert!(
            stdout.contains("[Service Vault] VAULT_SECRET UNSET"),
            "{stdout}"
        );
        assert!(
            stdout.contains("[Service Vault] VAULT_NOTE set-in-onEnter"),
            "{stdout}"
        );
        assert!(stdout.contains("TASK_SECRET UNSET"), "{stdout}");
        assert!(stdout.contains("Chunks run: 1"), "{stdout}");
    }

    /// Template Schemas §1.2.2 item 2: Service names are scoped to their
    /// document. The RFC's Valkey Job Template (inline Service `Cache`) is
    /// submitted with the RFC's queue-cache attachment (external Service
    /// `Cache`): both start, on different ports, and each consumer reaches
    /// its own document's Service — the Task through `Service.Cache.*`
    /// (the Job Template's), the attached client Environment's
    /// `VALKEY_HOST` / `VALKEY_PORT` through the external one.
    #[test]
    fn test_same_named_services_in_two_documents_stay_distinct() {
        let tdir = templates_dir();
        let env_path = tdir.join("service_same_name_queue_cache.yaml");
        let env_path = env_path.to_str().unwrap();
        let (code, stdout, stderr) =
            run_service_template("service_same_name_job.yaml", &["--environment", env_path]);
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");

        // Two Services named Cache, each labeled by its document in the log.
        // Neither depends on the other, so they start concurrently and the
        // order of their endpoint lines is not fixed.
        let external = format!("Service 'Cache' (from {env_path})");
        assert!(
            stdout.contains(&format!(
                "{external} (scope: every Step) endpoints: main -> 127.0.0.1:"
            )),
            "{stdout}"
        );
        assert!(
            stdout.contains(
                "Service 'Cache' (scope: Step ProcessFrames) endpoints: main -> 127.0.0.1:"
            ),
            "{stdout}"
        );
        assert!(stdout.contains(&format!("{external} is READY")), "{stdout}");
        assert!(stdout.contains("\tService 'Cache' is READY"), "{stdout}");
        assert_eq!(
            stdout.matches("is READY").count(),
            2,
            "exactly two Services became READY: {stdout}"
        );
        assert!(
            stdout.contains(&format!("Starting Service: Cache (from {env_path})")),
            "{stdout}"
        );
        assert!(stdout.contains("Starting Service: Cache\n"), "{stdout}");

        // Each listener announced its own port; the Task reached each one
        // through its own document's Service and the ports differ.
        let port_after = |marker: &str| -> u16 {
            let at = pos(&stdout, marker);
            stdout[at + marker.len()..]
                .split_whitespace()
                .next()
                .unwrap()
                .parse()
                .unwrap()
        };
        let job_port = port_after("JOB_CACHE_LISTENING ");
        let queue_port = port_after("QUEUE_CACHE_LISTENING ");
        assert_ne!(job_port, queue_port, "{stdout}");
        for frame in 1..=2 {
            assert!(
                stdout.contains(&format!(
                    "FRAME {frame} JOB_TEMPLATE_CACHE {job_port} SAYS JOB_CACHE"
                )),
                "{stdout}"
            );
            assert!(
                stdout.contains(&format!(
                    "FRAME {frame} QUEUE_CACHE {queue_port} SAYS QUEUE_CACHE"
                )),
                "{stdout}"
            );
            assert!(
                stdout.contains(&format!("FRAME {frame} PORTS_DIFFER True")),
                "{stdout}"
            );
        }
        // The client Environment resolved the external Cache's port, not
        // the Job Template's.
        assert!(
            pos(&stdout, &format!("{external} is READY"))
                < pos(&stdout, "Entering Environment: CacheClient"),
            "{stdout}"
        );
        // Both stopped, in reverse start order: the Job Template's first.
        assert!(
            pos(&stdout, "Stopping Service: Cache\n")
                < pos(
                    &stdout,
                    &format!("Stopping Service: Cache (from {env_path})")
                ),
            "{stdout}"
        );
        assert!(
            stdout.contains("All actions completed successfully!"),
            "{stdout}"
        );
    }

    /// As above, with the Job Template's same-named inline Service scoped to
    /// Step Work (the only Step that lists it): the Task's
    /// `Service.Cache.*` is the inline `Cache` (inline shadows), the attached
    /// Environment's variables the queue's, and the inline `Cache` stops
    /// before Step Done, which is outside its scope.
    #[test]
    fn test_step_scoped_service_named_like_an_external_service() {
        let tdir = templates_dir();
        let env_path = tdir.join("service_same_name_queue_cache.yaml");
        let env_path = env_path.to_str().unwrap();
        let (code, stdout, stderr) = run_service_template(
            "service_same_name_step_service.yaml",
            &[
                "--environment",
                env_path,
                "-p",
                "QueueCachePayload=FROM_QUEUE",
            ],
        );
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        let external = format!("Service 'Cache' (from {env_path})");
        assert!(
            stdout.contains(&format!("{external} (scope: every Step) endpoints:")),
            "{stdout}"
        );
        assert!(
            stdout.contains("Service 'Cache' (scope: Step Work) endpoints:"),
            "{stdout}"
        );
        assert!(
            stdout.contains("Service 'Cache' is READY: step cache on"),
            "{stdout}"
        );
        let port_after = |marker: &str| -> u16 {
            let at = pos(&stdout, marker);
            stdout[at + marker.len()..]
                .split_whitespace()
                .next()
                .unwrap()
                .parse()
                .unwrap()
        };
        let step_port = port_after("Service 'Cache' is READY: step cache on ");
        let queue_port = port_after("QUEUE_CACHE_LISTENING ");
        assert_ne!(step_port, queue_port, "{stdout}");
        assert!(
            stdout.contains(&format!("STEP_CACHE {step_port} SAYS STEP_CACHE")),
            "{stdout}"
        );
        assert!(
            stdout.contains(&format!("QUEUE_CACHE {queue_port} SAYS FROM_QUEUE")),
            "{stdout}"
        );
        assert!(stdout.contains("PORTS_DIFFER True"), "{stdout}");
        // The inline Cache stops with its scope, before Step Done runs and
        // before the every-Step external one.
        assert!(
            pos(&stdout, "Stopping Service: Cache\n") < pos(&stdout, "Running step 'Done'"),
            "{stdout}"
        );
        assert!(stdout.contains("DONE_STEP_RAN"), "{stdout}");
        assert!(
            pos(&stdout, "Stopping Service: Cache\n")
                < pos(
                    &stdout,
                    &format!("Stopping Service: Cache (from {env_path})")
                ),
            "{stdout}"
        );
    }

    /// "Failure and restart" with `completedTasks: RERUN`: the running Task
    /// is canceled (not a Task failure), `onRun` is relaunched in the same
    /// Service Session, and every completed Task of the Step runs again in
    /// a new Task Session.
    #[test]
    fn test_rerun_relaunch_requeues_completed_tasks() {
        let dir = TempDir::new().unwrap();
        let trace = dir.path().join("trace.txt");
        let marker = dir.path().join("marker");
        let (code, stdout, stderr) = run_service_template(
            "service_rerun_relaunch.yaml",
            &[
                "-p",
                &format!("TraceFile={}", trace.display()),
                "-p",
                &format!("MarkerFile={}", marker.display()),
            ],
        );
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            stdout.contains(
                "Service 'Flaky' (scope: Step Work) is UNREADY: onRun exited while the scope \
                 still had work (exit code: 1) (completedTasks: RERUN)"
            ),
            "{stdout}"
        );
        assert!(
            stdout.contains(
                "Canceling the running Task of Step 'Work': a Service with completedTasks: RERUN \
                 is UNREADY; the Task returns to the queue"
            ),
            "{stdout}"
        );
        assert!(
            stdout.contains(
                "Relaunching Service 'Flaky' onRun in its Service Session (relaunch 1 of 1)"
            ),
            "{stdout}"
        );
        assert!(
            stdout.contains(
                "Returning every completed Task of Step(s) 'Work' to the queue: a Service with \
                 completedTasks: RERUN (scope: Step Work) was relaunched\n"
            ),
            "{stdout}"
        );
        assert!(
            stdout.contains("New Task Session for the requeued Tasks"),
            "{stdout}"
        );
        assert!(
            stdout.contains("All actions completed successfully!"),
            "{stdout}"
        );
        // Task 1 completed, Task 2 was canceled, then Tasks 1-3 ran again.
        assert!(stdout.contains("Chunks run: 4"), "{stdout}");
        let lines = read_trace(&trace);
        assert_eq!(
            lines.iter().filter(|l| *l == "service launch").count(),
            2,
            "{lines:?}"
        );
        assert_eq!(
            lines.iter().filter(|l| *l == "task 1 done").count(),
            2,
            "{lines:?}"
        );
        assert_eq!(
            lines.iter().filter(|l| *l == "task 2 done").count(),
            1,
            "{lines:?}"
        );
        assert_eq!(
            lines.iter().filter(|l| *l == "task 3 done").count(),
            1,
            "{lines:?}"
        );
        let crash = lines.iter().position(|l| l == "service crash").unwrap();
        assert!(
            lines[..crash].contains(&"task 2 start".to_string())
                && !lines[..crash].contains(&"task 2 done".to_string()),
            "the crash must land while Task 2 runs: {lines:?}"
        );
    }

    /// `RERUN` on a Service whose scope is every Step (a Job Environment
    /// references it) returns every Step to pending ("RERUN and Step
    /// dependencies"): Step A's completed Task runs again. Helper is scoped
    /// to Step B, whose Task was running, so its scope never completed: its
    /// Service Session continues across the RERUN (constraint 6) and is not
    /// started again (constraint 9 applies only to a Session that ended).
    #[test]
    fn test_rerun_on_every_step_service_returns_every_step_to_pending() {
        let dir = TempDir::new().unwrap();
        let trace = dir.path().join("trace.txt");
        let marker = dir.path().join("marker");
        let (code, stdout, stderr) = run_service_template(
            "service_rerun_job_scope.yaml",
            &[
                "-p",
                &format!("TraceFile={}", trace.display()),
                "-p",
                &format!("MarkerFile={}", marker.display()),
            ],
        );
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            stdout.contains(
                "Returning every completed Task of Step(s) 'A', 'B' to the queue: a Service with \
                 completedTasks: RERUN (scope: every Step) was relaunched\n"
            ),
            "{stdout}"
        );
        assert_eq!(stdout.matches("Running step 'A'").count(), 2, "{stdout}");
        // The CLI's own banner (`\t--------- …`), one per Service Session
        // opened; the Session's tagged `[Service Helper] --------- Starting
        // Service: Helper` line follows each.
        assert_eq!(
            stdout
                .matches("\t--------- Starting Service: Helper")
                .count(),
            1,
            "{stdout}"
        );
        assert_eq!(
            stdout
                .matches("\t--------- Starting Service: Shared")
                .count(),
            1,
            "{stdout}"
        );
        // A1 ran, B1 was canceled, then A1, B1, B2 ran.
        assert!(stdout.contains("Chunks run: 4"), "{stdout}");
        let lines = read_trace(&trace);
        // Shared's relaunch runs in the background while the Task Session
        // is torn down, so the two "shared launch" lines are checked by
        // count rather than by position.
        assert_eq!(
            lines.iter().filter(|l| *l == "shared launch").count(),
            2,
            "{lines:?}"
        );
        let without_launches: Vec<&String> =
            lines.iter().filter(|l| *l != "shared launch").collect();
        assert_eq!(
            without_launches,
            vec![
                "task A1 start",
                "task A1 done",
                "helper launch",
                "task B1 start",
                "shared crash",
                "task A1 start",
                "task A1 done",
                "task B1 start",
                "task B1 done",
                "task B2 start",
                "task B2 done",
                "helper exit",
            ]
        );
    }

    /// "Failure and restart" with `completedTasks: KEEP`: the running Task
    /// continues and completes, completed Tasks stand, the Service is
    /// relaunched before the next Task is gated on it.
    #[test]
    fn test_keep_crash_lets_tasks_continue() {
        let dir = TempDir::new().unwrap();
        let trace = dir.path().join("trace.txt");
        let marker = dir.path().join("marker");
        let (code, stdout, stderr) = run_service_template(
            "service_keep_crash.yaml",
            &[
                "-p",
                &format!("TraceFile={}", trace.display()),
                "-p",
                &format!("MarkerFile={}", marker.display()),
            ],
        );
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            stdout.contains("(completedTasks: KEEP)")
                && stdout.contains("Relaunching Service 'Flaky' onRun in its Service Session"),
            "{stdout}"
        );
        assert!(!stdout.contains("Canceling the running Task"), "{stdout}");
        assert!(stdout.contains("Chunks run: 3"), "{stdout}");
        let lines = read_trace(&trace);
        assert_eq!(
            lines,
            vec![
                "service launch",
                "task 1 start",
                "task 1 done",
                "task 2 start",
                "service crash",
                "service launch",
                "task 2 done",
                "task 3 start",
                "task 3 done",
            ]
        );
    }

    /// A relaunch that begins a new Service Session changes the Service's
    /// endpoints: a Service that depends on it is restarted with the new
    /// value (no attempt consumed), a TASK-scoped Environment that captured
    /// it is re-entered, and later Tasks resolve the new port. With `KEEP`,
    /// the Task running during the relaunch completes.
    #[test]
    fn test_new_session_restarts_dependents_and_reenters_environments() {
        let dir = TempDir::new().unwrap();
        let trace = dir.path().join("trace.txt");
        let counter = dir.path().join("counter");
        let (code, stdout, stderr) = run_service_template(
            "service_new_session_dependents.yaml",
            &[
                "-p",
                &format!("TraceFile={}", trace.display()),
                "-p",
                &format!("CounterFile={}", counter.display()),
            ],
        );
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            stdout.contains(
                "Relaunching Service 'Back' onRun in its Service Session (relaunch 1 of 2)"
            ),
            "{stdout}"
        );
        assert!(
            stdout.contains(
                "Relaunching Service 'Back' in a new Service Session (relaunch 2 of 2): onRun \
                 exited before becoming READY (exit code: 7)"
            ),
            "{stdout}"
        );
        assert!(
            stdout.contains(
                "Service 'Front' depends on a Service that began a new Service Session; \
                 restarting it with the new endpoints"
            ),
            "{stdout}"
        );
        assert!(
            stdout.contains(
                "Re-entering 1 Environment(s): a Service they may reference has new endpoints"
            ),
            "{stdout}"
        );
        assert!(stdout.contains("Chunks run: 3"), "{stdout}");

        let lines = read_trace(&trace);
        let port_of = |prefix: &str| -> String {
            lines
                .iter()
                .find(|l| l.starts_with(prefix))
                .and_then(|l| l.rsplit(' ').next())
                .unwrap_or_else(|| panic!("missing {prefix:?} in {lines:?}"))
                .to_string()
        };
        let old_port = port_of("back launch 1 port ");
        let new_port = port_of("back launch 3 port ");
        assert_ne!(old_port, new_port, "{lines:?}");
        assert_eq!(port_of("back launch 2 port "), old_port, "{lines:?}");
        // Back is Job-wide (the Client Environment lists it) and so is
        // READY before Client is entered; Front is scoped to Step Work (which
        // lists it) and starts when Work is about to run, after Client.
        let expected: Vec<String> = [
            format!("back launch 1 port {old_port}"),
            format!("env enter backport {old_port}"),
            format!("front launch backport {old_port}"),
            format!("task 1 start backport {old_port}"),
            "task 1 done".to_string(),
            format!("task 2 start backport {old_port}"),
            "back crash".to_string(),
            format!("back launch 2 port {old_port}"),
            format!("back launch 3 port {new_port}"),
            "task 2 done".to_string(),
            format!("front launch backport {new_port}"),
            format!("env enter backport {new_port}"),
            format!("task 3 start backport {new_port}"),
            "task 3 done".to_string(),
        ]
        .into_iter()
        .collect();
        assert_eq!(lines, expected);
    }

    /// `maxAttempts` exhausted: every relaunch after an exit-before-READY
    /// begins a new Service Session (new ports, onExit each time), then the
    /// Service is FAILED, the Job fails with a non-zero exit code, no Task
    /// runs, and the result names the Service.
    #[test]
    fn test_max_attempts_exhausted_fails_the_job() {
        let dir = TempDir::new().unwrap();
        let trace = dir.path().join("trace.txt");
        let (code, stdout, stderr) = run_service_template(
            "service_fail_exhausted.yaml",
            &["-p", &format!("TraceFile={}", trace.display())],
        );
        assert_eq!(code, 1, "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(!stdout.contains("TASK_MUST_NOT_RUN"), "{stdout}");
        assert!(
            stdout.contains(
                "Relaunching Service 'Broken' in a new Service Session (relaunch 2 of 2): onRun \
                 exited before becoming READY (exit code: 3)"
            ),
            "{stdout}"
        );
        let reason = "onRun exited before becoming READY (exit code: 3); 2 of 2 relaunch(es) \
                      used (restartPolicy.maxAttempts)";
        assert!(
            stdout.contains(&format!(
                "Service 'Broken' (scope: Step Never) is FAILED: {reason}"
            )),
            "{stdout}"
        );
        assert!(
            stdout.contains(&format!(
                "Service 'Broken' (scope: Step Never) failed: {reason}"
            )),
            "{stdout}"
        );
        assert!(
            stderr.contains("ERROR: Service 'Broken' (scope: Step Never) failed:"),
            "{stderr}"
        );
        assert!(stdout.contains("Session ended with errors."), "{stdout}");
        assert!(
            stdout.contains(&format!(
                "Failed Service: Broken (scope: Step Never): {reason}"
            )),
            "{stdout}"
        );
        let lines = read_trace(&trace);
        let ports: std::collections::BTreeSet<&str> = lines
            .iter()
            .filter_map(|l| l.strip_prefix("launch port "))
            .collect();
        assert_eq!(ports.len(), 3, "each new Session gets new ports: {lines:?}");
        assert_eq!(
            lines.iter().filter(|l| *l == "exit").count(),
            3,
            "{lines:?}"
        );
    }

    /// Exploratory report stumble S10 (`06b`): a `RERUN` Service whose
    /// relaunch budget is exhausted while a Task runs. The Task is canceled
    /// and returns to the queue, the Service is FAILED, and the Job fails —
    /// without announcing a requeue that never happens or opening a new Task
    /// Session for it.
    #[test]
    fn test_rerun_failed_service_does_not_announce_a_requeue() {
        let dir = TempDir::new().unwrap();
        let trace = dir.path().join("trace.txt");
        let (code, stdout, stderr) = run_service_template(
            "service_rerun_exhausted_mid_task.yaml",
            &["-p", &format!("TraceFile={}", trace.display())],
        );
        assert_eq!(code, 1, "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            stdout.contains(
                "Canceling the running Task of Step 'Work': a Service with completedTasks: RERUN \
                 is UNREADY; the Task returns to the queue"
            ),
            "{stdout}"
        );
        assert!(
            stdout.contains("Task canceled; it returns to the queue (not a Task failure)"),
            "{stdout}"
        );
        let reason = "onRun exited while the scope still had work (exit code: 1); 0 of 0 \
                      relaunch(es) used (restartPolicy.maxAttempts)";
        assert!(
            stdout.contains(&format!(
                "Service 'Flaky' (scope: Step Work) is FAILED: {reason}"
            )),
            "{stdout}"
        );
        assert!(
            !stdout.contains("Returning every completed Task"),
            "no requeue is announced for a FAILED Service:\n{stdout}"
        );
        assert!(
            !stdout.contains("New Task Session for the requeued Tasks"),
            "{stdout}"
        );
        assert!(!stdout.contains("Relaunching"), "{stdout}");
        assert!(
            stdout.contains(&format!(
                "Failed Service: Flaky (scope: Step Work): {reason}"
            )),
            "{stdout}"
        );
        // Task 1 completed; Task 2 was canceled; Task 3 never ran.
        assert!(stdout.contains("Chunks run: 1"), "{stdout}");
        let lines = read_trace(&trace);
        assert_eq!(
            lines,
            vec![
                "flaky launch",
                "task 1 start",
                "task 1 done",
                "task 2 start",
                "flaky crash"
            ],
            "{lines:?}"
        );
    }

    /// The structured result carries the FAILED Service.
    #[test]
    fn test_failed_service_in_json_result() {
        let dir = TempDir::new().unwrap();
        let trace = dir.path().join("trace.txt");
        let (code, stdout, _stderr) = run_service_template(
            "service_fail_exhausted.yaml",
            &[
                "-p",
                &format!("TraceFile={}", trace.display()),
                "--output",
                "json",
            ],
        );
        assert_eq!(code, 1, "{stdout}");
        let json_start = stdout.find('{').expect("json output");
        let value: serde_json::Value = serde_json::from_str(&stdout[json_start..]).unwrap();
        assert_eq!(value["status"], "error");
        assert_eq!(value["failed_services"][0]["name"], "Broken");
        assert_eq!(value["failed_services"][0]["scope"], "Step Never");
        assert_eq!(
            value["failed_services"][0]["reason"],
            "onRun exited before becoming READY (exit code: 3); 2 of 2 relaunch(es) used \
             (restartPolicy.maxAttempts)"
        );
        assert!(
            value["message"]
                .as_str()
                .unwrap()
                .starts_with("Service 'Broken' (scope: Step Never) failed:"),
            "{value}"
        );
    }

    /// Constraint 10: a Step whose selection schedules no Task does not
    /// start the Services scoped to it.
    #[test]
    fn test_step_with_no_tasks_does_not_start_its_service() {
        let dir = TempDir::new().unwrap();
        let marker = dir.path().join("marker");
        let (code, stdout, stderr) = run_service_template(
            "service_step_no_tasks.yaml",
            &[
                "-p",
                &format!("MarkerFile={}", marker.display()),
                "--tasks",
                "[]",
            ],
        );
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            stdout.contains(
                "Not starting Service 'Unused' for Step 'Work': no Task of this Step will run"
            ),
            "{stdout}"
        );
        assert!(!stdout.contains("Starting Service"), "{stdout}");
        assert!(!marker.exists(), "the Service must not have started");
        assert!(stdout.contains("Chunks run: 0"), "{stdout}");
    }

    /// An interruption cancels the running Task and stops every Service:
    /// `onRun` is canceled and `onExit` still runs (constraint 7).
    #[test]
    fn test_interrupt_stops_services() {
        let dir = TempDir::new().unwrap();
        let trace = dir.path().join("trace.txt");
        let template = templates_dir().join("service_interruptible.yaml");
        let mut child = spawn_run_piped(
            &template,
            &["-p", &format!("TraceFile={}", trace.display())],
        );
        let stdout = child.stdout.take().expect("stdout is piped");
        let (tx, rx) = mpsc::channel::<String>();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut collected = String::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(60)) {
                Ok(line) => {
                    let hit = line.contains("READY_FOR_SIGNAL");
                    collected.push_str(&line);
                    collected.push('\n');
                    if hit {
                        break;
                    }
                }
                Err(_) => {
                    let _ = child.kill();
                    panic!("task never signaled readiness. output so far:\n{collected}");
                }
            }
        }
        if !send_interrupt(&child) {
            let _ = child.kill();
            let _ = child.wait();
            #[cfg(windows)]
            {
                eprintln!("SKIPPED: no console available to deliver CTRL_BREAK_EVENT");
                return;
            }
            #[cfg(not(windows))]
            panic!("failed to deliver SIGINT to the CLI process");
        }
        loop {
            match rx.recv_timeout(Duration::from_secs(60)) {
                Ok(line) => {
                    collected.push_str(&line);
                    collected.push('\n');
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let _ = child.kill();
                    panic!("CLI did not exit within 60s of interruption. output:\n{collected}");
                }
            }
        }
        reader.join().unwrap();
        let status = child.wait().unwrap();
        assert_eq!(status.code(), Some(1), "{collected}");
        assert!(
            collected.contains("Interruption signal received."),
            "{collected}"
        );
        assert!(collected.contains("Stopping Service: Store"), "{collected}");
        assert!(collected.contains("Service 'Store' stopped"), "{collected}");
        assert!(!collected.contains("EXIT_NORMAL"), "{collected}");
        assert_eq!(read_trace(&trace), vec!["service exit"]);
    }

    /// The RFC's Valkey example adapted: the Service's `onEnter` puts a fake
    /// `valkey-server` on PATH with `openjd_env` (the RFC's `conda create`),
    /// the Service's bare `valkey-server` `onRun` resolves through it, and
    /// the Tasks consume the endpoint without the fake binary on their own
    /// PATH.
    #[test]
    fn test_valkey_example_on_enter_provisions_the_binary() {
        let dir = TempDir::new().unwrap();
        let trace = dir.path().join("trace.txt");
        let (code, stdout, stderr) = run_service_template(
            "service_valkey_conda.yaml",
            &["-p", &format!("TraceFile={}", trace.display())],
        );
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        // onEnter's PATH export, then the fake binary found through it by
        // onRun.
        assert!(
            stdout.contains("\t[Service Cache] openjd_env: PATH="),
            "{stdout}"
        );
        assert!(
            pos(
                &stdout,
                "\t[Service Cache] --------- Service onEnter: Cache"
            ) < pos(
                &stdout,
                "\t[Service Cache] --------- Service onRun: Cache (launch 1)"
            ),
            "{stdout}"
        );
        assert!(
            stdout.contains("FAKE_VALKEY_LISTENING 127.0.0.1 "),
            "{stdout}"
        );
        assert!(stdout.contains("Service 'Cache' is READY"), "{stdout}");
        for frame in 1..=3 {
            assert!(
                stdout.contains(&format!("FRAME {frame} CACHED done")),
                "{stdout}"
            );
        }
        assert!(stdout.contains("Chunks run: 3"), "{stdout}");
        let mut lines = read_trace(&trace);
        lines.sort();
        assert_eq!(
            lines,
            vec![
                "frame 1 valkey-server-on-task-path=False",
                "frame 2 valkey-server-on-task-path=False",
                "frame 3 valkey-server-on-task-path=False",
            ]
        );
    }

    /// Template Schemas §9.1 rule 1 / RFC 0009 lifecycle constraint 6:
    /// scope by dependency. Coord is listed by Scatter and Gather, not by
    /// Unrelated: it starts when Scatter is about to run, one instance
    /// persists across both Steps (Gather reads what Scatter's three Tasks
    /// stored), and it is stopped — onExit run, `stopped` logged — before
    /// Unrelated's Task runs, which finds its endpoint refused.
    #[test]
    fn test_scope_by_dependency_two_of_three_steps() {
        let dir = TempDir::new().unwrap();
        let trace = dir.path().join("trace.txt");
        let (code, stdout, stderr) = run_service_template(
            "service_scope_two_of_three.yaml",
            &["-p", &format!("TraceFile={}", trace.display())],
        );
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            stdout.contains("Service 'Coord' (scope: Steps Gather, Scatter) endpoints: main -> "),
            "{stdout}"
        );
        assert_eq!(
            stdout
                .matches("\t--------- Starting Service: Coord")
                .count(),
            1,
            "one Service Session across both Steps:\n{stdout}"
        );
        assert!(
            pos(&stdout, "Running step 'Scatter'") < pos(&stdout, "Starting Service: Coord")
                && pos(&stdout, "Service 'Coord' is READY") < pos(&stdout, "SCATTER 1 OK"),
            "{stdout}"
        );
        assert!(stdout.contains("GATHERED part1,part2,part3"), "{stdout}");
        assert!(
            pos(&stdout, "GATHERED") < pos(&stdout, "Stopping Service: Coord")
                && pos(&stdout, "[Service Coord] COORD_EXIT")
                    < pos(&stdout, "Service 'Coord' stopped")
                && pos(&stdout, "Service 'Coord' stopped")
                    < pos(&stdout, "Running step 'Unrelated'"),
            "Coord stops once its scope completes, before Unrelated:\n{stdout}"
        );
        assert!(stdout.contains("UNRELATED_RAN COORD_GONE True"), "{stdout}");
        assert!(stdout.contains("Chunks run: 5"), "{stdout}");
        let lines = read_trace(&trace);
        let lines: Vec<&str> = lines
            .iter()
            .map(String::as_str)
            .filter(|l| !l.starts_with("endpoint "))
            .collect();
        assert_eq!(
            lines,
            vec![
                "coord launch",
                "scatter 1",
                "scatter 2",
                "scatter 3",
                "gather part1,part2,part3",
                "coord exit",
                "unrelated coord_gone=True",
            ]
        );
    }

    /// Template Schemas §9 item 4 / How Jobs Are Run constraint 2: a Service
    /// with `dependencies: [{dependsOn: Prepare}]` starts only after
    /// Prepare's Task completed. Use — listed first, listing only
    /// `service:Indexer` and no Step of its own — runs after Prepare through
    /// the Step edge the Service's dependencies imply.
    #[test]
    fn test_service_dependencies_start_after_the_listed_step() {
        let dir = TempDir::new().unwrap();
        let trace = dir.path().join("trace.txt");
        let (code, stdout, stderr) = run_service_template(
            "service_dependencies_after_step.yaml",
            &["-p", &format!("TraceFile={}", trace.display())],
        );
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            stdout.contains("INDEXER_UP_DURING_PREPARE False"),
            "{stdout}"
        );
        assert!(
            stdout.contains("Service 'Indexer' (scope: Step Use) endpoints: main -> "),
            "{stdout}"
        );
        assert!(
            pos(&stdout, "Running step 'Prepare'") < pos(&stdout, "INDEXER_UP_DURING_PREPARE")
                && pos(&stdout, "INDEXER_UP_DURING_PREPARE")
                    < pos(&stdout, "Starting Service: Indexer")
                && pos(&stdout, "Running step 'Prepare'") < pos(&stdout, "Running step 'Use'"),
            "the Service starts after its dependency Step's Task completed:\n{stdout}"
        );
        assert!(
            stdout.contains("[Service Indexer] INDEXER_READ prepared-content"),
            "{stdout}"
        );
        assert!(stdout.contains("USE_GOT prepared-content"), "{stdout}");
        assert!(stdout.contains("Chunks run: 2"), "{stdout}");
    }

    /// Template Schemas §1.1 item 9, §1.2.2 item 2, §9.8: `requiresServices`
    /// satisfied by an attached Environment Template. `Service.R.main.*`
    /// resolves to the attached Service's endpoint both in the Task's
    /// arguments and in a Job Environment's variable; the attached Service
    /// is logged with its document and has scope every Step.
    #[test]
    fn test_requires_services_satisfied_by_environment() {
        let env_path = templates_dir().join("service_required_provider.yaml");
        let env_path = env_path.to_str().unwrap();
        let (code, stdout, stderr) = run_service_template(
            "service_required_consumer.yaml",
            &["--environment", env_path],
        );
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        let marker = format!(
            "Service 'R' (from {env_path}) (scope: every Step) endpoints: main -> 127.0.0.1:"
        );
        let at = pos(&stdout, &marker) + marker.len();
        let port: u16 = stdout[at..]
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            stdout.contains(&format!("TASK_DIRECT {port} R_SAYS_HI")),
            "{stdout}"
        );
        assert!(
            stdout.contains(&format!("TASK_VIA_ENV {port} R_SAYS_HI")),
            "{stdout}"
        );
        assert!(stdout.contains("SAME_ENDPOINT True"), "{stdout}");
        assert!(
            pos(&stdout, &format!("Service 'R' (from {env_path}) is READY"))
                < pos(&stdout, "Entering Environment: RClient"),
            "{stdout}"
        );
        assert!(stdout.contains("Chunks run: 1"), "{stdout}");
    }

    /// A `requiresServices` entry that the attachments do not satisfy is
    /// rejected at submission (Template Schemas §1.2.2 item 2): non-zero
    /// exit, no Session started, and the model error at
    /// `JobTemplate -> requiresServices[0]`.
    fn assert_requirement_rejected(environments: &[&str], message: &str) {
        let mut args = Vec::new();
        let paths: Vec<String> = environments
            .iter()
            .map(|e| templates_dir().join(e).to_str().unwrap().to_string())
            .collect();
        for p in &paths {
            args.push("--environment");
            args.push(p.as_str());
        }
        let (code, stdout, stderr) = run_service_template("service_required_consumer.yaml", &args);
        assert_ne!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(!stdout.contains("Session start"), "{stdout}");
        let mut message = message.to_string();
        for (name, p) in environments.iter().zip(&paths) {
            message = message.replace(&format!("<{name}>"), p);
        }
        assert!(
            stderr.contains(&format!(
                "1 validation error for Submission\nJobTemplate -> requiresServices[0]:\n\t{message}\n"
            )),
            "expected {message:?} in stderr:\n{stderr}"
        );
    }

    #[test]
    fn test_requires_services_unmet_without_attachment() {
        assert_requirement_rejected(
            &[],
            "required Service 'R' is not provided: no Environment Template is attached \
             (Template Schemas §1.2.2 item 2).",
        );
    }

    #[test]
    fn test_requires_services_unmet_by_attachment_without_the_service() {
        assert_requirement_rejected(
            &["env_plain_expr_greeting.yaml"],
            "required Service 'R' is not provided: none of the attached Environment Templates \
             (<env_plain_expr_greeting.yaml>) defines a Service named 'R' (Template Schemas \
             §1.2.2 item 2).",
        );
    }

    #[test]
    fn test_requires_services_ambiguous_two_attachments() {
        assert_requirement_rejected(
            &[
                "service_required_provider.yaml",
                "service_required_provider_missing_port.yaml",
            ],
            "required Service 'R' is ambiguous: 2 attached Environment Templates define a \
             Service with that name (<service_required_provider.yaml>, \
             <service_required_provider_missing_port.yaml>); a requirement must match exactly \
             one (Template Schemas §1.2.2 item 2).",
        );
    }

    #[test]
    fn test_requires_services_provider_missing_port() {
        assert_requirement_rejected(
            &["service_required_provider_missing_port.yaml"],
            "required Service 'R' is provided by <service_required_provider_missing_port.yaml>, \
             which is missing port 'main'; its ports: other (Template Schemas §1.2.2 item 2).",
        );
    }

    #[test]
    fn test_requires_services_provider_protocol_mismatch() {
        assert_requirement_rejected(
            &["service_required_provider_udp.yaml"],
            "required Service 'R' is provided by <service_required_provider_udp.yaml>, whose \
             port 'main' has protocol UDP but the requirement declares TCP (Template Schemas \
             §1.2.2 item 2).",
        );
    }

    /// Template Schemas §4 items 3–4: a Job Environment that lists a Service
    /// in `dependencies` and declares no `runScope` runs in `[TASK]` only.
    /// XClient is entered in the Task Session (its variable, resolved from
    /// the Service it lists, reaches the Task) and not in X's Service Session
    /// (X's onRun sees it unset); Plain, which lists no Service, is entered
    /// in both.
    #[test]
    fn test_run_scope_default_is_task_for_an_environment_depending_on_a_service() {
        let (code, stdout, stderr) = run_service_template("service_run_scope_default.yaml", &[]);
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            stdout.contains("Service 'X' (scope: every Step) endpoints: main -> "),
            "{stdout}"
        );
        assert!(
            stdout.contains("[Service X] --------- Entering Environment: Plain"),
            "{stdout}"
        );
        assert!(
            !stdout.contains("[Service X] --------- Entering Environment: XClient"),
            "{stdout}"
        );
        assert!(
            stdout.contains("[Service X] SERVICE_SEES_X_ENDPOINT UNSET"),
            "{stdout}"
        );
        assert!(
            stdout.contains("[Service X] SERVICE_SEES_PLAIN plain-value"),
            "{stdout}"
        );
        assert!(
            stdout.contains("\t--------- Entering Environment: XClient"),
            "{stdout}"
        );
        assert!(stdout.contains("X_SAYS_HI PLAIN plain-value"), "{stdout}");
    }

    /// As above: the Service Session's skip of XClient is logged under the
    /// Service's tag (specs/sessions/service-session.md "log `Skipping
    /// Environment '<name>': its runScope does not include SERVICE`"), as
    /// the Task Session's skip of a SERVICE-only Environment is.
    #[test]
    fn test_run_scope_default_skip_is_logged_in_the_service_session() {
        let (code, stdout, stderr) = run_service_template("service_run_scope_default.yaml", &[]);
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            stdout.contains(
                "[Service X] Skipping Environment 'XClient': its runScope does not include SERVICE"
            ),
            "{stdout}"
        );
    }

    /// Template Schemas §4 item 3 / §9.1 rule 3: a Job Environment that lists
    /// a Service it never references — an opaque consumer that reaches the
    /// Service by other means — puts every Step in the Service's scope. No
    /// Step lists or references the Service either, yet it is up Job-wide:
    /// READY before the Environment is entered (its endpoint file exists
    /// when the first Task reads it), still up for a second Step, and
    /// stopped once at the end. The Environment is Task-only by default, so
    /// the Service Session never sees its variable.
    #[test]
    fn test_job_environment_dependency_without_reference_keeps_the_service_up_job_wide() {
        let dir = TempDir::new().unwrap();
        let endpoint = dir.path().join("endpoint.txt");
        let (code, stdout, stderr) = run_service_template(
            "service_job_environment_dependency_opaque.yaml",
            &["-p", &format!("EndpointFile={}", endpoint.display())],
        );
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            stdout.contains("Service 'Store' (scope: every Step) endpoints: main -> "),
            "{stdout}"
        );
        // Entered in the Task Session only, after the Service is READY and
        // before the first Task; skipped in the Service Session.
        assert!(stdout.contains("OPAQUE_ENTER"), "{stdout}");
        assert!(
            stdout.contains(
                "[Service Store] Skipping Environment 'Opaque': its runScope does not include \
                 SERVICE"
            ),
            "{stdout}"
        );
        assert!(
            stdout.contains("[Service Store] SERVICE_SEES_OPAQUE UNSET"),
            "{stdout}"
        );
        assert!(
            pos(&stdout, "openjd_service_ready: STORE") < pos(&stdout, "OPAQUE_ENTER"),
            "{stdout}"
        );
        // Both Tasks reach the Service through the file, with no Step
        // listing it and the Environment's variable set.
        assert!(stdout.contains("FIRST_GOT STORE OPAQUE yes"), "{stdout}");
        assert!(stdout.contains("SECOND_GOT STORE"), "{stdout}");
        assert!(
            stdout.contains("[Service Store] STORE_EXIT False"),
            "{stdout}"
        );
        assert_eq!(stdout.matches("STORE_EXIT").count(), 1, "{stdout}");
        assert!(
            pos(&stdout, "SECOND_GOT STORE") < pos(&stdout, "[Service Store] STORE_EXIT"),
            "{stdout}"
        );
    }

    /// RFC 0009 "RERUN and Step dependencies": Flaky (RERUN) has scope
    /// Steps A, B; C depends on A and is outside the scope; B depends on C.
    /// Flaky exits while B's Task runs: A's and B's completed Tasks return to
    /// the queue and C, a dependent of the returned Step A, returns to
    /// pending, so C's Task runs twice.
    #[test]
    fn test_rerun_returns_dependent_step_outside_the_scope_to_pending() {
        let dir = TempDir::new().unwrap();
        let trace = dir.path().join("trace.txt");
        let marker = dir.path().join("marker");
        let (code, stdout, stderr) = run_service_template(
            "service_rerun_dependent_step.yaml",
            &[
                "-p",
                &format!("TraceFile={}", trace.display()),
                "-p",
                &format!("MarkerFile={}", marker.display()),
            ],
        );
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            stdout.contains(
                "Returning every completed Task of Step(s) 'A', 'B' to the queue: a Service with \
                 completedTasks: RERUN (scope: Steps A, B) was relaunched; dependent Step(s) 'C' \
                 return to pending\n"
            ),
            "{stdout}"
        );
        assert!(
            stdout.contains(
                "Canceling the running Task of Step 'B': a Service with completedTasks: RERUN \
                 is UNREADY; the Task returns to the queue"
            ),
            "{stdout}"
        );
        for step in ["A", "C", "B"] {
            assert_eq!(
                stdout.matches(&format!("Running step '{step}'")).count(),
                2,
                "{stdout}"
            );
        }
        assert!(stdout.contains("Chunks run: 5"), "{stdout}");
        let lines = read_trace(&trace);
        assert_eq!(
            lines,
            vec![
                "flaky launch",
                "task A start",
                "task A done",
                "task C start",
                "task C done",
                "task B start",
                "flaky crash",
                "flaky launch",
                "task A start",
                "task A done",
                "task C start",
                "task C done",
                "task B start",
                "task B done",
            ]
        );
    }

    /// RFC 0009 "RERUN and Step dependencies" / lifecycle constraint 9: Solo
    /// (scope Step A) is stopped once A completes. Flaky (RERUN, scope Steps
    /// A, B) exits while B runs and returns A to pending, so Solo is started
    /// again in a new Service Session — new port, onExit run again — when A
    /// next becomes schedulable.
    #[test]
    fn test_rerun_restarts_a_service_whose_scope_had_completed() {
        let dir = TempDir::new().unwrap();
        let trace = dir.path().join("trace.txt");
        let marker = dir.path().join("marker");
        let (code, stdout, stderr) = run_service_template(
            "service_rerun_restarts_completed_scope.yaml",
            &[
                "-p",
                &format!("TraceFile={}", trace.display()),
                "-p",
                &format!("MarkerFile={}", marker.display()),
            ],
        );
        assert_eq!(code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            stdout.contains("Service 'Solo' (scope: Step A) endpoints: main -> "),
            "{stdout}"
        );
        assert_eq!(
            stdout.matches("\t--------- Starting Service: Solo").count(),
            2,
            "{stdout}"
        );
        assert_eq!(
            stdout
                .matches("\t--------- Starting Service: Flaky")
                .count(),
            1,
            "{stdout}"
        );
        assert!(
            pos(&stdout, "Service 'Solo' stopped") < pos(&stdout, "Running step 'B'"),
            "Solo stops once its scope completes:\n{stdout}"
        );
        let rerun = "Returning every completed Task of Step(s) 'A', 'B' to the queue: a Service \
                     with completedTasks: RERUN (scope: Steps A, B) was relaunched\n";
        let at = pos(&stdout, rerun);
        assert!(
            stdout[at..].contains("\t--------- Starting Service: Solo"),
            "Solo starts again after the RERUN:\n{stdout}"
        );
        assert!(stdout.contains("Chunks run: 3"), "{stdout}");
        let lines = read_trace(&trace);
        let solo_ports: Vec<&str> = lines
            .iter()
            .filter_map(|l| l.strip_prefix("solo launch "))
            .collect();
        assert_eq!(solo_ports.len(), 2, "{lines:?}");
        assert_ne!(
            solo_ports[0], solo_ports[1],
            "a new Service Session gets new ports: {lines:?}"
        );
        let shape: Vec<String> = lines
            .iter()
            .map(|l| {
                if l.starts_with("solo launch ") {
                    "solo launch".to_string()
                } else if l.starts_with("task A ") {
                    "task A".to_string()
                } else {
                    l.clone()
                }
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                "solo launch",
                "task A",
                "solo exit",
                "task B start",
                "flaky crash",
                "solo launch",
                "task A",
                "solo exit",
                "task B start",
                "task B done",
            ]
        );
        // Each run of A saw its own Solo Session's port.
        let task_a_ports: Vec<&str> = lines
            .iter()
            .filter_map(|l| l.strip_prefix("task A "))
            .map(|rest| rest.split(' ').next().unwrap())
            .collect();
        assert_eq!(task_a_ports, solo_ports, "{lines:?}");
    }
}
