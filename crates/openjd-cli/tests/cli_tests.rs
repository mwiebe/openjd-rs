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
    cmd.args(args).env("RUSTUP_TOOLCHAIN", "1.97.0");
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

    /// A path mapping rule with a relative source path is rejected when the
    /// rules are loaded, naming the rule. The template's required parameter
    /// has no value, so a later check would report that instead.
    #[test]
    fn test_run_path_mapping_rules_relative_source_rejected() {
        let mut tpl = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            tpl,
            r#"specificationVersion: "jobtemplate-2023-09"
name: test
parameterDefinitions:
  - name: Required
    type: PATH
steps:
  - name: s1
    script:
      actions:
        onRun:
          command: echo
"#
        )
        .unwrap();
        let rules = r#"{"version": "pathmapping-1.0", "path_mapping_rules": [
            {"source_path_format": "POSIX", "source_path": "/ok", "destination_path": "/a"},
            {"source_path_format": "POSIX", "source_path": "home/user", "destination_path": "/b"}
        ]}"#;

        let (code, stdout, stderr) = run_cli(&[
            "run",
            tpl.path().to_str().unwrap(),
            "--path-mapping-rules",
            rules,
        ]);
        assert_ne!(code, 0, "relative source path should be rejected");
        assert!(
            stderr.contains(
                "Invalid path mapping rules: path mapping rule 1: source_path 'home/user' \
                 must be an absolute POSIX path, starting with '/'"
            ),
            "stderr: {stderr}"
        );
        assert!(!stdout.contains("Session start"), "stdout: {stdout}");
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

    /// A one-step template with one CHUNK[INT] parameter `Item`, whose task
    /// does nothing; `chunks` is the YAML body of its `chunks` field.
    fn write_chunk_template(dir: &std::path::Path, range: &str, chunks: &str) -> PathBuf {
        let template = dir.join("chunks.yaml");
        std::fs::write(
            &template,
            format!(
                r#"specificationVersion: 'jobtemplate-2023-09'
extensions: [TASK_CHUNKING]
name: Chunks
steps:
- name: S
  parameterSpace:
    taskParameterDefinitions:
    - name: Item
      type: CHUNK[INT]
      range: "{range}"
      chunks: {{{chunks}}}
  script:
    actions:
      onRun:
        command: python
        args: ["-c", "pass"]
"#
            ),
        )
        .unwrap();
        template
    }

    /// `run_cli`, failing if the CLI has not exited within `limit`. A CLI that
    /// lists the values of a huge range runs out of memory or never finishes;
    /// the limit turns the second into a failure.
    fn run_cli_within(args: &[&str], limit: std::time::Duration) -> (i32, String, String) {
        let mut child = cli_command(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("failed to execute openjd");
        // Drain both pipes while waiting, so a CLI that writes more than a
        // pipe buffer holds is not mistaken for one that does not finish.
        let drain = |pipe: Option<Box<dyn std::io::Read + Send>>| {
            std::thread::spawn(move || {
                let mut text = String::new();
                if let Some(mut pipe) = pipe {
                    let _ = std::io::Read::read_to_string(&mut pipe, &mut text);
                }
                text
            })
        };
        let stdout = drain(child.stdout.take().map(|p| Box::new(p) as _));
        let stderr = drain(child.stderr.take().map(|p| Box::new(p) as _));
        let started = std::time::Instant::now();
        while child.try_wait().expect("try_wait").is_none() {
            if started.elapsed() > limit {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "openjd {args:?} did not finish within {limit:?}: is it expanding the values?"
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let status = child.wait().expect("wait");
        (
            status.code().unwrap_or(-1),
            stdout.join().expect("stdout reader"),
            stderr.join().expect("stderr reader"),
        )
    }

    #[test]
    fn test_run_adaptive_chunk_size_beyond_usize_takes_the_rest() {
        // A huge targetRuntimeSeconds over a quick task makes the adaptive
        // chunk size saturate at usize::MAX, which the next chunk's end used
        // to overflow on. The next chunk is all the values that remain.
        let dir = tempfile::TempDir::new().unwrap();
        let template = write_chunk_template(
            dir.path(),
            "1-10",
            "defaultTaskCount: 1, targetRuntimeSeconds: 9223372036854775807, \
             rangeConstraint: NONCONTIGUOUS",
        );
        let (code, stdout, stderr) = run_cli_within(
            &["run", template.to_str().unwrap()],
            std::time::Duration::from_secs(120),
        );
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(
            stdout.contains("Item(CHUNK[INT]) = 1\n"),
            "stdout: {stdout}"
        );
        assert!(
            stdout.contains("Item(CHUNK[INT]) = 2-10\n"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn test_chunks_of_huge_ranges_are_built_without_listing_values() {
        // Each case lists a trillion values (or walks half a trillion
        // intervals) if any chunking path expands the range.
        let dir = tempfile::TempDir::new().unwrap();
        let limit = std::time::Duration::from_secs(60);
        for (range, chunks, extra, want) in [
            (
                "1-1000000000000",
                "defaultTaskCount: 500000000000, rangeConstraint: NONCONTIGUOUS",
                vec!["--maximum-tasks", "1"],
                "Item(CHUNK[INT]) = 1-500000000000\n",
            ),
            (
                "1-1000000000000",
                "defaultTaskCount: 2, targetRuntimeSeconds: 1, rangeConstraint: NONCONTIGUOUS",
                vec!["--maximum-tasks", "1"],
                "Item(CHUNK[INT]) = 1,2\n",
            ),
            (
                "1-1000000000000",
                "defaultTaskCount: 2, targetRuntimeSeconds: 1, rangeConstraint: CONTIGUOUS",
                vec!["--maximum-tasks", "1"],
                "Item(CHUNK[INT]) = 1-2\n",
            ),
            (
                "1-1000000000000:2",
                "defaultTaskCount: 1, rangeConstraint: CONTIGUOUS",
                vec!["--maximum-tasks", "1"],
                "Item(CHUNK[INT]) = 1-1\n",
            ),
            (
                "1-1000000000000:2",
                "defaultTaskCount: 1, rangeConstraint: CONTIGUOUS",
                vec!["-t", "Item=999999999999-999999999999"],
                "Item = 999999999999-999999999999\n",
            ),
        ] {
            let template = write_chunk_template(dir.path(), range, chunks);
            let mut args = vec!["run", template.to_str().unwrap()];
            args.extend(extra);
            let (code, stdout, stderr) = run_cli_within(&args, limit);
            assert_eq!(
                code, 0,
                "{range} {chunks}\nstdout: {stdout}\nstderr: {stderr}"
            );
            assert!(
                stdout.contains(want),
                "{range} {chunks}: want {want:?}, stdout: {stdout}"
            );
        }

        // `summary` counts a stepped range's contiguous chunks: one per value.
        let template = write_chunk_template(
            dir.path(),
            "1-1000000000000:2",
            "defaultTaskCount: 1, rangeConstraint: CONTIGUOUS",
        );
        let (code, stdout, stderr) =
            run_cli_within(&["summary", template.to_str().unwrap()], limit);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(stdout.contains("500000000000"), "stdout: {stdout}");
    }

    #[test]
    fn test_contiguous_chunk_task_param_written_n_dash_m() {
        // A CONTIGUOUS chunk is always written <N>-<M> (Template Schemas
        // §3.4.1.5), including one given with --task-param; a
        // NONCONTIGUOUS one is written in canonical form.
        let dir = tempfile::TempDir::new().unwrap();
        for (constraint, value, want) in [
            ("CONTIGUOUS", "5-6", "P:[5-6] R:[5-6]"),
            ("NONCONTIGUOUS", "5-6", "P:[5,6] R:[5,6]"),
        ] {
            let template = dir.path().join(format!("{constraint}.yaml"));
            std::fs::write(
                &template,
                format!(
                    r#"specificationVersion: 'jobtemplate-2023-09'
extensions: [TASK_CHUNKING]
name: Chunk Form
steps:
- name: S
  parameterSpace:
    taskParameterDefinitions:
    - name: Frame
      type: CHUNK[INT]
      range: "1-10"
      chunks:
        defaultTaskCount: 2
        rangeConstraint: {constraint}
  script:
    actions:
      onRun:
        command: python
        args: ["-c", "print('P:[{{{{Task.Param.Frame}}}}] R:[{{{{Task.RawParam.Frame}}}}]')"]
"#
                ),
            )
            .unwrap();
            let task_param = format!("Frame={value}");
            let (code, stdout, stderr) = run_cli(&[
                "run",
                template.to_str().unwrap(),
                "--step",
                "S",
                "-t",
                &task_param,
            ]);
            assert_eq!(code, 0, "{constraint} {value} stderr: {stderr}");
            assert!(
                stdout.contains(want),
                "{constraint} {value}: want {want}, stdout: {stdout}"
            );
        }
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

    /// Template help resolves `--extensions` as `check` and `run` do, so a
    /// template using a newer extension (here WRAP_ACTIONS and CONTROL_FLOW)
    /// gets help rather than "Unsupported extension names".
    #[test]
    fn test_help_enables_every_supported_extension_by_default() {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            f,
            "specificationVersion: jobtemplate-2023-09
extensions: [EXPR, WRAP_ACTIONS, CONTROL_FLOW]
name: CfJob
steps:
  - name: S
    outputVariables: [{{name: Count, type: INT}}]
    script: {{actions: {{onRun: {{command: echo}}}}}}
"
        )
        .unwrap();
        let (code, stdout, stderr) = run_cli(&["run", f.path().to_str().unwrap(), "--help"]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("Job: CfJob"), "stdout: {stdout}");
    }

    #[test]
    fn test_help_honors_the_extensions_option() {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        write!(
            f,
            "specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: CfJob
steps:
  - name: S
    script: {{actions: {{onRun: {{command: echo}}}}}}
"
        )
        .unwrap();
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            f.path().to_str().unwrap(),
            "--extensions",
            "EXPR",
            "--help",
        ]);
        assert_ne!(code, 0);
        assert!(
            stderr.contains("Unsupported extension names: CONTROL_FLOW"),
            "stderr: {stderr}"
        );
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            f.path().to_str().unwrap(),
            "--extensions=EXPR",
            "--help",
        ]);
        assert_ne!(code, 0);
        assert!(
            stderr.contains("Unsupported extension names: CONTROL_FLOW"),
            "stderr: {stderr}"
        );
        // `--extensions ""` disables every extension.
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            f.path().to_str().unwrap(),
            "--extensions",
            "",
            "--help",
        ]);
        assert_ne!(code, 0);
        assert!(
            stderr.contains("Unsupported extension names: CONTROL_FLOW, EXPR"),
            "stderr: {stderr}"
        );
        // Names are case-insensitive, as for `check`.
        let (code, stdout, stderr) = run_cli(&[
            "run",
            f.path().to_str().unwrap(),
            "--extensions",
            "expr,control_flow",
            "--help",
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("Job: CfJob"), "stdout: {stdout}");
        // A trailing `--extensions` with no value keeps the defaults.
        let (code, stdout, stderr) =
            run_cli(&["run", f.path().to_str().unwrap(), "--help", "--extensions"]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(stdout.contains("Job: CfJob"), "stdout: {stdout}");
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            f.path().to_str().unwrap(),
            "--extensions",
            "NOT_AN_EXTENSION",
            "--help",
        ]);
        assert_ne!(code, 0);
        assert!(
            stderr.contains("Unsupported Open Job Description extension(s): NOT_AN_EXTENSION"),
            "stderr: {stderr}"
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
        assert!(
            _stderr.starts_with(
                "error: the argument '--task-param <TASK_PARAMS>' cannot be used with '--maximum-tasks <MAXIMUM_TASKS>'\n"
            ),
            "{_stderr}"
        );
    }

    /// `--maximum-tasks` is an `Option<i64>`, and still conflicts with
    /// `--tasks`, even at `-1`.
    #[test]
    fn test_run_tasks_conflicts_with_maximum_tasks() {
        let tdir = templates_dir();
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            tdir.join("job_with_test_steps.yaml").to_str().unwrap(),
            "--step",
            "TaskParamStep",
            "--tasks",
            r#"[{"TaskNumber": "1"}]"#,
            "--maximum-tasks",
            "-1",
            "--extensions",
            "",
        ]);
        assert_eq!(code, 2);
        assert!(
            stderr.starts_with(
                "error: the argument '--tasks <TASKS>' cannot be used with '--maximum-tasks <MAXIMUM_TASKS>'\n"
            ),
            "{stderr}"
        );
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

mod interruption {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::Duration;

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
// CONTROL_FLOW: openjd_output_var treatments in `openjd run`
// ============================================================

mod control_flow_output_vars {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    const JOB: &str = r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: CF
steps:
- name: P
  outputVariables:
  - name: N
    type: INT
  script:
    actions:
      onRun:
        command: python
        args: ['-c', 'print("openjd_output_var: N=1")']
"#;

    /// A declared output with no value fails the Task, and the reason is in
    /// the output, not only in the session's status.
    #[test]
    fn missing_output_fails_and_is_reported() {
        let job = write(&JOB.replace("print(\"openjd_output_var: N=1\")", "print(\"hi\")"));
        let (code, stdout, stderr) = run_cli(&["run", job.path().to_str().unwrap()]);
        assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
        assert!(
            stdout.contains(
                "\tERROR: Output variable 'N': no value was reported (collect mode ONCE)\n"
            ),
            "stdout: {stdout}"
        );
    }

    /// An Environment Template has not opted in to CONTROL_FLOW, so a
    /// directive in its action is ordinary output, on enter and on exit.
    #[test]
    fn environment_template_directive_is_ordinary_output() {
        let job = write(JOB);
        let env = write(
            r#"specificationVersion: environment-2023-09
environment:
  name: EnvT
  script:
    actions:
      onEnter:
        command: python
        args: ['-c', 'print("openjd_output_var: N=1")']
      onExit:
        command: python
        args: ['-c', 'print("openjd_output_var: N=2")']
"#,
        );
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--environment",
            env.path().to_str().unwrap(),
        ]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(!stdout.contains("-- ERROR:"), "stdout: {stdout}");
    }

    /// A Job Environment is defined by the Job Template, so a directive in
    /// its action fails it.
    #[test]
    fn job_environment_directive_fails() {
        let job = write(&JOB.replace(
            "steps:\n",
            r#"jobEnvironments:
- name: JobEnv
  script:
    actions:
      onEnter:
        command: python
        args: ['-c', 'print("openjd_output_var: N=1")']
steps:
"#,
        ));
        let (code, stdout, _stderr) = run_cli(&["run", job.path().to_str().unwrap()]);
        assert_ne!(code, 0, "stdout: {stdout}");
        assert!(
            stdout.contains(
                "openjd_output_var: N=1 -- ERROR: Invalid openjd_output_var directive: this \
                 Action can't report output values; only the onRun action of a Step that \
                 declares outputVariables can, in 'openjd_output_var: N=1'"
            ),
            "stdout: {stdout}"
        );
    }
}

// ============================================================
// CONTROL_FLOW: `openjd run` as a scheduler simulator, and `summary`
// ============================================================

mod control_flow_run {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    /// The log lines, without their timestamps.
    fn lines(stdout: &str) -> Vec<&str> {
        stdout
            .lines()
            .map(|l| l.split_once('\t').map_or(l, |(_, rest)| rest))
            .collect()
    }

    /// The position of the first log line equal to `line`.
    fn at(stdout: &str, line: &str) -> usize {
        lines(stdout)
            .iter()
            .position(|l| *l == line)
            .unwrap_or_else(|| panic!("no line {line:?} in:\n{stdout}"))
    }

    /// P reports N; C fans out over range(N), chunked; job outputs read
    /// C's TaskParam (concrete once its Tasks are known) and its outputs.
    const FAN_OUT: &str = r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW, TASK_CHUNKING]
name: Fan
jobOutputDefinitions:
- name: Frames
  type: LIST[INT]
  value: "{{Steps.C.TaskParam.F}}"
- name: Total
  type: INT
  unit: px
  value: "{{sum(Steps.C.Output.Done)}}"
steps:
- name: P
  outputVariables:
  - {name: N, type: INT, unit: frames}
  script:
    actions:
      onRun:
        command: python
        args:
        - -c
        - |-
          print('openjd_output_var: N=3')
- name: C
  dependencies: [{dependsOn: P}]
  parameterSpace:
    taskParameterDefinitions:
    - name: F
      type: CHUNK[INT]
      range: "{{[2, 0, 1][:Steps.P.Output.N]}}"
      chunks: {defaultTaskCount: 2, rangeConstraint: NONCONTIGUOUS}
  outputVariables:
  - {name: Done, type: INT}
  script:
    actions:
      onRun:
        command: python
        args:
        - -c
        - |-
          from openjd_range import r
        - "{{Task.Param.F}}"
"#;

    /// FAN_OUT with C's script reporting Done[f] = 10 * f for each f of its
    /// chunk.
    fn fan_out() -> String {
        FAN_OUT.replace(
            "          from openjd_range import r\n",
            "          import sys\n          for p in sys.argv[1].split(','):\n            a, _, b = p.partition('-')\n            for f in range(int(a), int(b or a) + 1): print(f'openjd_output_var: Done[{f}]={10 * f}')\n",
        )
    }

    #[test]
    fn run_advances_pending_steps_and_evaluates_job_outputs() {
        let job = write(&fan_out());
        let (code, stdout, stderr) = run_cli(&["run", job.path().to_str().unwrap()]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        let pending = at(&stdout, "Step 'C' is pending inputs:");
        assert_eq!(
            lines(&stdout)[pending + 1..pending + 4],
            [
                "  Enabled: yes",
                "  Tasks: determined at run time from Steps.P.Output.N",
                "  Host requirements: known",
            ]
        );
        let output = at(&stdout, "Output N = 3 frames");
        let known = at(&stdout, "Step 'C': Tasks known (3 tasks)");
        let ready = at(&stdout, "Step 'C' is ready");
        // Frames reads only C's TaskParam, so it is evaluated before C runs.
        let frames = at(&stdout, "Job output Frames = [2, 0, 1]");
        let runs = at(&stdout, "Running step 'C'");
        assert!(output < known && known < ready && ready < frames && frames < runs);
        // Chunks are {0, 2} and {1}; each value's output is printed.
        for line in [
            "Output Done[0] = 0",
            "Output Done[2] = 20",
            "Output Done[1] = 10",
        ] {
            assert!(at(&stdout, line) > runs);
        }
        assert!(at(&stdout, "Job output Total = 30 px") > at(&stdout, "Output Done[1] = 10"));
        assert!(
            stdout.contains("Job outputs:\n  Frames = [2, 0, 1]\n  Total = 30 px"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn json_output_has_steps_and_job_outputs() {
        let job = write(&fan_out());
        let (code, stdout, stderr) =
            run_cli(&["run", job.path().to_str().unwrap(), "--output", "json"]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        let json_start = stdout.find("{\n").expect("JSON result");
        let result: serde_json::Value = serde_json::from_str(&stdout[json_start..]).unwrap();
        assert_eq!(
            result["steps"],
            serde_json::json!([
                {"name": "P", "state": "SUCCEEDED", "task_count": 1, "outputs": [{"N": 3}]},
                {"name": "C", "state": "SUCCEEDED", "task_count": 3,
                 "outputs": [{"Done": 20}, {"Done": 0}, {"Done": 10}]},
            ])
        );
        assert_eq!(
            result["job_outputs"],
            serde_json::json!({"Frames": [2, 0, 1], "Total": 30})
        );
        assert_eq!(result["job_outputs_not_evaluated"], serde_json::json!([]));
    }

    #[test]
    fn json_output_of_a_template_without_control_flow_is_unchanged() {
        let template = templates_dir().join("job_with_test_steps.yaml");
        let (code, stdout, _) = run_cli(&[
            "run",
            template.to_str().unwrap(),
            "--step",
            "BareStep",
            "--output",
            "json",
        ]);
        assert_eq!(code, 0, "stdout: {stdout}");
        let json_start = stdout.find("{\n").expect("JSON result");
        let result: serde_json::Value = serde_json::from_str(&stdout[json_start..]).unwrap();
        let keys: Vec<&String> = result.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            [
                "chunks_run",
                "duration",
                "job_name",
                "message",
                "status",
                "step_name"
            ]
        );
    }

    /// RFC 0011 §How a Step is bound item 3: C's range fails as soon as P,
    /// which it reads, finishes, before Q, which only its script reads, runs.
    #[test]
    fn early_group_failure_stops_the_run() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Early
steps:
- name: P
  outputVariables: [{name: N, type: INT}]
  script:
    actions:
      onRun:
        command: python
        args: [-c, "print('openjd_output_var: N=0')"]
- name: Q
  dependencies: [{dependsOn: P}]
  outputVariables: [{name: M, type: INT}]
  script:
    actions:
      onRun:
        command: python
        args: [-c, "print('Q_' + 'RAN'); print('openjd_output_var: M=1')"]
- name: C
  dependencies: [{dependsOn: P}, {dependsOn: Q}]
  parameterSpace:
    taskParameterDefinitions:
    - name: F
      type: INT
      range: "{{range(Steps.P.Output.N) if Steps.P.Output.N > 0 else fail('no frames')}}"
  script:
    actions:
      onRun:
        command: python
        args: [-c, "print({{Steps.Q.Output.M}})"]
"#,
        );
        let (code, stdout, stderr) = run_cli(&["run", job.path().to_str().unwrap()]);
        assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
        assert!(!stdout.contains("Q_RAN"), "stdout: {stdout}");
        assert!(
            stderr.contains(
                "ERROR: Step 'C' failed in its Task enumeration group:\n\
                 Model validation error: 1 validation error for JobTemplate\n\
                 steps[2] -> parameterSpace -> taskParameterDefinitions[0] -> range:\n\
                 \tno frames\n  \
                 range(Steps.P.Output.N) if Steps.P.Output.N > 0 else fail('no frames')\n  \
                 \x20                                                    ^~~~~~~~~~~~~~~~~\n"
            ),
            "stderr: {stderr}"
        );
        assert!(
            stdout.contains("Session ended with errors"),
            "stdout: {stdout}"
        );
    }

    /// A skipped Step runs nothing and gives null; disabled Environments are
    /// not entered; Steps behind a failed Step are blocked.
    #[test]
    fn skipped_steps_disabled_environments_and_blocked_steps() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Skip
jobEnvironments:
- name: JE
  enableIf: false
  script:
    actions:
      onEnter: {command: python, args: [-c, "print('JE_' + 'ENTERED')"]}
steps:
- name: S
  enableIf: false
  outputVariables: [{name: N, type: INT}]
  script:
    actions:
      onRun: {command: python, args: [-c, "print('S_' + 'RAN')"]}
- name: C
  dependencies: [{dependsOn: S}]
  stepEnvironments:
  - name: SE
    enableIf: "{{Steps.S.Output.N != null}}"
    script:
      actions:
        onEnter: {command: python, args: [-c, "print('SE_' + 'ENTERED')"]}
  script:
    actions:
      onRun: {command: python, args: [-c, "print('GOT', {{repr_py(Steps.S.Output.N)}}); raise SystemExit(4)"]}
- name: D
  dependencies: [{dependsOn: C}]
  script:
    actions:
      onRun: {command: python, args: [-c, "print('D_' + 'RAN')"]}
"#,
        );
        let (code, stdout, stderr) = run_cli(&["run", job.path().to_str().unwrap()]);
        assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
        for absent in ["JE_ENTERED", "S_RAN", "SE_ENTERED", "D_RAN"] {
            assert!(!stdout.contains(absent), "{absent} in stdout: {stdout}");
        }
        at(&stdout, "Step 'S' skipped (enableIf is false)");
        at(
            &stdout,
            "Job Environment 'JE' is disabled by its enableIf; not entering it",
        );
        at(
            &stdout,
            "Step Environment 'SE' is disabled by its enableIf; not entering it",
        );
        at(&stdout, "GOT None");
        // The failed Task stops the run before D; D is then marked blocked.
        assert!(!stdout.contains("Running step 'D'"), "stdout: {stdout}");
        at(&stdout, "Step 'D' is blocked: its dependency 'C' failed");
    }

    /// A job output evaluated from a Step's TaskParam stays committed when
    /// the Step then fails; one that reads its outputs is never evaluated.
    #[test]
    fn job_output_stays_committed_when_its_step_fails() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Commit
jobOutputDefinitions:
- {name: Items, type: "LIST[INT]", value: "{{Steps.C.TaskParam.I}}"}
- {name: Sum, type: INT, value: "{{sum(Steps.C.Output.V)}}"}
steps:
- name: C
  parameterSpace:
    taskParameterDefinitions:
    - {name: I, type: INT, range: [4, 5]}
  outputVariables: [{name: V, type: INT}]
  script:
    actions:
      onRun: {command: python, args: [-c, "raise SystemExit(2)"]}
"#,
        );
        let (code, stdout, stderr) =
            run_cli(&["run", job.path().to_str().unwrap(), "--output", "json"]);
        assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
        at(&stdout, "Job output Items = [4, 5]");
        at(
            &stdout,
            "Job output Sum was not evaluated: it waits on Step 'C', which failed",
        );
        let json_start = stdout.find("{\n").expect("JSON result");
        let result: serde_json::Value = serde_json::from_str(&stdout[json_start..]).unwrap();
        assert_eq!(result["job_outputs"], serde_json::json!({"Items": [4, 5]}));
        assert_eq!(
            result["job_outputs_not_evaluated"],
            serde_json::json!([{"name": "Sum", "waits_on": ["Step 'C', which failed"]}])
        );
        assert_eq!(result["steps"][0]["state"], "FAILED");
    }

    /// RFC 0011 §Job outputs item 3: a job output that fails fails the run
    /// at once, so no further Tasks run.
    #[test]
    fn job_output_error_fails_the_run() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Fails
jobOutputDefinitions:
- {name: Count, type: INT, value: "{{Steps.P.Output.N}}", maxValue: 5}
- {name: Other, type: INT, value: "{{Steps.P.Output.N + 1}}"}
steps:
- name: P
  outputVariables: [{name: N, type: INT}]
  script:
    actions:
      onRun: {command: python, args: [-c, "print('openjd_output_var: N=9')"]}
- name: Q
  dependencies: [{dependsOn: P}]
  script:
    actions:
      onRun: {command: python, args: [-c, "print('Q_' + 'RAN')"]}
"#,
        );
        let (code, stdout, stderr) = run_cli(&["run", job.path().to_str().unwrap()]);
        assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
        assert!(!stdout.contains("Q_RAN"), "stdout: {stdout}");
        assert!(
            stderr.contains(
                "ERROR: Model validation error: 1 validation error for JobTemplate\n\
                 jobOutputDefinitions[0] -> value:\n\
                 \tJob output 'Count': value 9 exceeds maximum 5\n  \
                 {{Steps.P.Output.N}}\n"
            ),
            "stderr: {stderr}"
        );
        // The other job output whose inputs are concrete is still evaluated.
        at(&stdout, "Job output Other = 10");
        at(
            &stdout,
            "Job output Count has no value: its evaluation failed",
        );
    }

    /// `--maximum-tasks` leaves a multi-Task producer incomplete: the Step
    /// that reads its outputs is blocked; one that does not still runs.
    #[test]
    fn maximum_tasks_blocks_the_consumer_of_an_incomplete_producer() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Partial
steps:
- name: P
  parameterSpace:
    taskParameterDefinitions:
    - {name: I, type: INT, range: [1, 2]}
  outputVariables: [{name: V, type: INT}]
  script:
    actions:
      onRun: {command: python, args: [-c, "print('openjd_output_var: V={{Task.Param.I}}')"]}
- name: C
  dependencies: [{dependsOn: P}]
  script:
    actions:
      onRun: {command: python, args: [-c, "print('C_GOT', {{repr_py(Steps.P.Output.V)}})"]}
- name: D
  dependencies: [{dependsOn: P}]
  script:
    actions:
      onRun: {command: python, args: [-c, "print('D_' + 'RAN')"]}
- name: E
  dependencies: [{dependsOn: P}]
  script:
    actions:
      onRun: {command: python, args: [-c, "print('E_GOT', {{repr_py(Steps.P.TaskParam.I)}})"]}
"#,
        );
        let (code, stdout, stderr) =
            run_cli(&["run", job.path().to_str().unwrap(), "--maximum-tasks", "1"]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        at(
            &stdout,
            "Step 'C' is blocked: inputs from Step 'P' are incomplete because not all of its Tasks ran",
        );
        assert!(!stdout.contains("C_GOT"), "stdout: {stdout}");
        assert!(stdout.contains("D_RAN"), "stdout: {stdout}");
        // E reads only P's TaskParam, which is concrete without P's outputs.
        assert!(stdout.contains("E_GOT [1, 2]"), "stdout: {stdout}");
    }

    const CONSUMER: &str = r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Alone
steps:
- name: P
  parameterSpace:
    taskParameterDefinitions:
    - {name: I, type: INT, range: [1, 2]}
  outputVariables: [{name: V, type: INT}]
  script:
    actions:
      onRun: {command: python, args: [-c, "print('P_' + 'RAN')"]}
- name: C
  dependencies: [{dependsOn: P}]
  parameterSpace:
    taskParameterDefinitions:
    - {name: J, type: INT, range: "{{Steps.P.Output.V}}"}
  script:
    actions:
      onRun: {command: python, args: [-c, "print('J={{Task.Param.J}}', {{repr_py(Steps.P.TaskParam.I)}})"]}
"#;

    #[test]
    fn step_alone_needs_step_inputs() {
        let job = write(CONSUMER);
        let (code, stdout, stderr) = run_cli(&["run", job.path().to_str().unwrap(), "--step", "C"]);
        assert_ne!(code, 0, "stdout: {stdout}");
        assert!(
            stderr.ends_with(
                "Step 'C' reads Steps.* values of Steps this run does not run:\n  \
                 Steps.P.Output.V (a list with one INT per Task, 2 Tasks)\n\
                 Give them with --step-inputs, or run Step 'C' with --run-dependencies. For example:\n  \
                 --step-inputs '{\"Steps.P.Output.V\": [0, 0]}'\n"
            ),
            "stderr: {stderr}"
        );
    }

    #[test]
    fn step_inputs_supply_a_producers_outputs() {
        let job = write(CONSUMER);
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "C",
            "--step-inputs",
            r#"{"Steps.P.Output.V": [7, "8"]}"#,
        ]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(!stdout.contains("P_RAN"), "stdout: {stdout}");
        at(&stdout, "Step 'C': Tasks known (2 tasks)");
        // C's TaskParam reads of P come from P's parameter space.
        assert!(stdout.contains("J=7 [1, 2]"), "stdout: {stdout}");
        assert!(stdout.contains("J=8 [1, 2]"), "stdout: {stdout}");
    }

    #[test]
    fn step_inputs_are_checked_against_the_declared_types() {
        let job = write(CONSUMER);
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "C",
            "--step-inputs",
            r#"{"Steps.P.Output.V": [7]}"#,
        ]);
        assert_ne!(code, 0);
        assert!(
            stderr.ends_with(
                "--step-inputs: Step 'P': Steps.P.Output.V must be a list with one value per Task (2)\n"
            ),
            "stderr: {stderr}"
        );
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step-inputs",
            r#"{"Steps.P.Output.V": [7, 8]}"#,
        ]);
        assert_ne!(code, 0);
        assert!(
            stderr.ends_with(
                "--step-inputs requires --step (or a Job with one Step or loop) without --run-dependencies.\n"
            ),
            "stderr: {stderr}"
        );
    }

    #[test]
    fn summary_reports_what_is_known_of_pending_steps_and_job_outputs() {
        let job = write(&FAN_OUT.replace(
            "steps:\n",
            "jobEnvironments:\n- {name: JE, enableIf: false, variables: {A: a}}\nsteps:\n- name: S\n  enableIf: false\n  script: {actions: {onRun: {command: x}}}\n",
        ));
        let (code, stdout, stderr) = run_cli(&["summary", job.path().to_str().unwrap()]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(
            stdout.contains("Total tasks: 1 + (determined at run time)\n"),
            "{stdout}"
        );
        assert!(
            stdout.contains("1. 'S' (SKIPPED, 0 total Tasks)\n"),
            "{stdout}"
        );
        assert!(
            stdout.contains(
                "3. 'C' (pending inputs; Tasks determined at run time)\n  Enabled: yes\n  \
                 Tasks: determined at run time from Steps.P.Output.N\n  Host requirements: known\n"
            ),
            "{stdout}"
        );
        assert!(
            stdout.contains("  - JE (from 'root', disabled by its enableIf)\n"),
            "{stdout}"
        );
        assert!(
            stdout.contains(
                "--- Job outputs in 'Fan' ---\n  \
                 - Frames (LIST[INT]): pending; waits for Steps.C.TaskParam.F\n  \
                 - Total (INT, px): pending; waits for Steps.C.Output.Done\n"
            ),
            "{stdout}"
        );

        let (code, stdout, _) =
            run_cli(&["summary", job.path().to_str().unwrap(), "--output", "json"]);
        assert_eq!(code, 0);
        let result: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(result["tasks_determined_at_run_time"], true);
        assert_eq!(result["steps"][0]["state"], "SKIPPED");
        assert_eq!(result["steps"][2]["total_tasks"], serde_json::Value::Null);
        assert_eq!(
            result["steps"][2]["binding"]["tasks"],
            serde_json::json!({"known": false, "waits_on": ["Steps.P.Output.N"], "waits_for_enable_if": false})
        );
        assert_eq!(
            result["job_outputs"][0],
            serde_json::json!({"name": "Frames", "type": "LIST[INT]", "state": "PENDING",
                               "waits_on": ["Steps.C.TaskParam.F"]})
        );
    }

    /// The A11 case: a Step's Tasks are known at creation, but a `let` that
    /// only its script reads waits on an upstream output.
    #[test]
    fn summary_shows_known_tasks_of_a_pending_step() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: A11
steps:
- name: P
  outputVariables: [{name: X, type: INT}]
  script: {actions: {onRun: {command: x}}}
- name: C
  dependencies: [{dependsOn: P}]
  let: [x = Steps.P.Output.X]
  parameterSpace:
    taskParameterDefinitions:
    - {name: I, type: INT, range: "1-240"}
  script: {actions: {onRun: {command: "{{x}}"}}}
"#,
        );
        let (code, stdout, stderr) = run_cli(&["summary", job.path().to_str().unwrap()]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(stdout.contains("Total tasks: 241\n"), "{stdout}");
        assert!(
            stdout.contains(
                "2. 'C' (240 total Tasks; pending inputs)\n  Enabled: yes\n  \
                 Tasks: 240 (known; Step pending inputs for Steps.P.Output.X)\n  \
                 Host requirements: known\n"
            ),
            "{stdout}"
        );
    }
}

mod control_flow_help {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn template_help_lists_job_outputs_and_step_inputs() {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        f.write_all(
            br#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: CfJob
jobOutputDefinitions:
- {name: Count, type: INT, unit: frames, description: How many frames, value: "{{1}}"}
- {name: Dir, type: PATH, objectType: DIRECTORY, value: "/out"}
steps:
- name: S
  script: {actions: {onRun: {command: echo}}}
"#,
        )
        .unwrap();
        let (code, stdout, stderr) = run_cli(&["run", f.path().to_str().unwrap(), "--help"]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(
            stdout.contains(
                "Job Outputs:\n  Count (INT, frames)\n    How many frames\n  Dir (PATH)\n\n"
            ),
            "stdout: {stdout}"
        );
        assert!(
            stdout.contains("  --step-inputs <JSON|file://path>  With --step: the Steps.* values that Step or loop reads\n"),
            "stdout: {stdout}"
        );
        let template = templates_dir().join("basic.yaml");
        let (_, stdout, _) = run_cli(&["run", template.to_str().unwrap(), "--help"]);
        assert!(!stdout.contains("--step-inputs"), "stdout: {stdout}");
        assert!(!stdout.contains("Job Outputs:"), "stdout: {stdout}");
    }
}

mod control_flow_run_more {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_suffix(content: &str, suffix: &str) -> NamedTempFile {
        let mut f = NamedTempFile::with_suffix(suffix).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    fn write(content: &str) -> NamedTempFile {
        write_suffix(content, ".yaml")
    }

    fn lines(stdout: &str) -> Vec<&str> {
        stdout
            .lines()
            .map(|l| l.split_once('\t').map_or(l, |(_, rest)| rest))
            .collect()
    }

    fn at(stdout: &str, line: &str) -> usize {
        lines(stdout)
            .iter()
            .position(|l| *l == line)
            .unwrap_or_else(|| panic!("no line {line:?} in:\n{stdout}"))
    }

    fn json(stdout: &str) -> serde_json::Value {
        let start = stdout.find("{\n").expect("JSON result");
        serde_json::from_str(&stdout[start..]).unwrap()
    }

    /// A producer whose computed range is empty has zero Tasks: its outputs
    /// are empty lists, and a job output over them is evaluated.
    #[test]
    fn zero_task_producer() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Zero
jobOutputDefinitions:
- {name: Sum, type: INT, value: "{{sum(Steps.Q.Output.V)}}"}
steps:
- name: P
  outputVariables: [{name: N, type: INT}]
  script:
    actions:
      onRun: {command: python, args: [-c, "print('openjd_output_var: N=0')"]}
- name: Q
  dependencies: [{dependsOn: P}]
  parameterSpace:
    taskParameterDefinitions:
    - {name: I, type: INT, range: "{{range(Steps.P.Output.N)}}"}
  outputVariables: [{name: V, type: INT}]
  script:
    actions:
      onRun: {command: python, args: [-c, "print('Q_' + 'RAN')"]}
- name: C
  dependencies: [{dependsOn: Q}]
  script:
    actions:
      onRun: {command: python, args: [-c, "print('GOT', {{repr_py(Steps.Q.Output.V)}}, {{repr_py(Steps.Q.TaskParam.I)}})"]}
"#,
        );
        let (code, stdout, stderr) =
            run_cli(&["run", job.path().to_str().unwrap(), "--output", "json"]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        let known = at(&stdout, "Step 'Q': Tasks known (0 tasks)");
        assert!(at(&stdout, "Job output Sum = 0") > known);
        at(&stdout, "Step 'Q' has 0 tasks");
        assert!(!stdout.contains("Q_RAN"), "stdout: {stdout}");
        assert!(stdout.contains("GOT [] []"), "stdout: {stdout}");
        assert_eq!(
            json(&stdout)["steps"][1],
            serde_json::json!({"name": "Q", "state": "SUCCEEDED", "task_count": 0, "outputs": []})
        );
    }

    /// A Step with zero Tasks in a template without CONTROL_FLOW prints
    /// nothing new.
    #[test]
    fn zero_tasks_without_control_flow_prints_nothing_new() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR]
name: Zero
steps:
- name: P
  parameterSpace:
    taskParameterDefinitions:
    - {name: I, type: INT, range: "{{[x for x in [1] if x > 5]}}"}
  script:
    actions:
      onRun: {command: python, args: [-c, "print(1)"]}
"#,
        );
        let (code, stdout, stderr) = run_cli(&["run", job.path().to_str().unwrap()]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(!stdout.contains("has 0 tasks"), "stdout: {stdout}");
    }

    const DYNAMIC: &str = r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Dyn
jobOutputDefinitions:
- {name: CT, type: INT, value: "{{Steps.C.Output.T}}"}
steps:
- name: P
  outputVariables: [{name: Go, type: BOOL}, {name: Os, type: STRING}]
  script:
    actions:
      onRun: {command: python, args: [-c, "print('openjd_output_var: Go=GO'); print('openjd_output_var: Os=\"linux\"')"]}
- name: C
  dependencies: [{dependsOn: P}]
  enableIf: "{{Steps.P.Output.Go}}"
  hostRequirements:
    attributes:
    - {name: attr.worker.os.family, anyOf: ["{{Steps.P.Output.Os}}"]}
  outputVariables: [{name: T, type: INT}]
  script:
    actions:
      onRun: {command: python, args: [-c, "print('openjd_output_var: T=7')"]}
"#;

    #[test]
    fn dynamic_enable_if_true_reports_each_fact() {
        let job = write(&DYNAMIC.replace("Go=GO", "Go=true"));
        let (code, stdout, stderr) = run_cli(&["run", job.path().to_str().unwrap()]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        let pending = at(&stdout, "Step 'C' is pending inputs:");
        assert_eq!(
            lines(&stdout)[pending + 1..pending + 4],
            [
                "  Enabled: determined at run time from Steps.P.Output.Go",
                "  Tasks: determined at run time (waiting for enableIf)",
                "  Host requirements: determined at run time from Steps.P.Output.Os and enableIf",
            ]
        );
        let enabled = at(&stdout, "Step 'C': enabled (enableIf is true)");
        assert_eq!(
            lines(&stdout)[enabled + 1],
            "Step 'C': Tasks known (1 task)"
        );
        assert_eq!(
            lines(&stdout)[enabled + 2],
            "Step 'C': host requirements known"
        );
        assert_eq!(lines(&stdout)[enabled + 3], "Step 'C' is ready");
        at(&stdout, "Job output CT = 7");
    }

    #[test]
    fn dynamic_enable_if_false_skips_and_gives_null() {
        let job = write(&DYNAMIC.replace("Go=GO", "Go=false"));
        let (code, stdout, stderr) =
            run_cli(&["run", job.path().to_str().unwrap(), "--output", "json"]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        let skipped = at(&stdout, "Step 'C' skipped (enableIf is false)");
        assert!(at(&stdout, "Job output CT = null (no value)") > skipped);
        at(&stdout, "Step 'C' is skipped; it runs no Tasks");
        let result = json(&stdout);
        assert_eq!(
            result["steps"][1],
            serde_json::json!({"name": "C", "state": "SKIPPED", "task_count": 0})
        );
        assert_eq!(result["job_outputs"], serde_json::json!({"CT": null}));
    }

    /// A Step behind a failed Step, two deep, is blocked at the end of the
    /// run, and the job output that reads it names it as unavailable.
    #[test]
    fn steps_behind_a_failed_step_are_blocked() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Chain
jobOutputDefinitions:
- {name: DV, type: INT, value: "{{Steps.D.Output.V}}"}
steps:
- name: P
  script:
    actions:
      onRun: {command: python, args: [-c, "raise SystemExit(3)"]}
- name: S
  enableIf: false
  dependencies: [{dependsOn: P}]
  script:
    actions:
      onRun: {command: python, args: [-c, "print(1)"]}
- name: D
  dependencies: [{dependsOn: S}]
  outputVariables: [{name: V, type: INT}]
  script:
    actions:
      onRun: {command: python, args: [-c, "print(1)"]}
"#,
        );
        let (code, stdout, stderr) =
            run_cli(&["run", job.path().to_str().unwrap(), "--output", "json"]);
        assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
        // S stays SKIPPED, which is terminal, but never satisfies dependsOn.
        let s = at(
            &stdout,
            "Step 'S' is skipped and never satisfies dependsOn: its dependency 'P' failed",
        );
        let d = at(
            &stdout,
            "Step 'D' is blocked: its dependency 'S' is skipped but never satisfies dependsOn",
        );
        assert!(s < d);
        assert!(
            at(
                &stdout,
                "Job output DV was not evaluated: it waits on Step 'D', which is blocked"
            ) > d
        );
        let result = json(&stdout);
        assert_eq!(
            result["steps"],
            serde_json::json!([
                {"name": "P", "state": "FAILED", "task_count": 1, "reason": "a Task failed"},
                {"name": "S", "state": "SKIPPED", "task_count": 0,
                 "reason": "its dependency 'P' failed"},
                {"name": "D", "state": "BLOCKED", "task_count": 1,
                 "reason": "its dependency 'S' is skipped but never satisfies dependsOn"},
            ])
        );
    }

    /// A URI PATH output is readable by a consumer's group and a job output.
    #[test]
    fn uri_path_output_reaches_groups_and_job_outputs() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Uri
jobOutputDefinitions:
- {name: J, type: PATH, value: "{{Steps.P.RawOutput.U}}"}
steps:
- name: P
  outputVariables: [{name: U, type: PATH}]
  script:
    actions:
      onRun: {command: python, args: [-c, "print('openjd_output_var: U=\"s3://b/k\"')"]}
- name: C
  dependencies: [{dependsOn: P}]
  parameterSpace:
    taskParameterDefinitions:
    - {name: X, type: STRING, range: "{{[string(Steps.P.RawOutput.U)]}}"}
  script:
    actions:
      onRun: {command: python, args: [-c, "print('X={{Task.Param.X}}')"]}
"#,
        );
        let (code, stdout, stderr) = run_cli(&["run", job.path().to_str().unwrap()]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        at(&stdout, "Job output J = \"s3://b/k\"");
        assert!(stdout.contains("X=s3://b/k"), "stdout: {stdout}");
    }

    /// A chunked Step without output variables may repeat a CHUNK[INT]
    /// value; running all of its Tasks completes it.
    #[test]
    fn repeated_chunk_values_without_outputs_complete() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW, TASK_CHUNKING]
name: Rep
steps:
- name: P
  parameterSpace:
    taskParameterDefinitions:
    - name: F
      type: CHUNK[INT]
      range: [1, 1, 2]
      chunks: {defaultTaskCount: 3, rangeConstraint: NONCONTIGUOUS}
  script:
    actions:
      onRun: {command: python, args: [-c, "print('F={{Task.Param.F}}')"]}
"#,
        );
        let (code, stdout, stderr) =
            run_cli(&["run", job.path().to_str().unwrap(), "--output", "json"]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert_eq!(
            json(&stdout)["steps"][0],
            serde_json::json!({"name": "P", "state": "SUCCEEDED", "task_count": 3})
        );
    }

    const CONSUMER: &str = r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Alone
steps:
- name: P
  outputVariables: [{name: V, type: INT}]
  script:
    actions:
      onRun: {command: python, args: [-c, "print('P_' + 'RAN')"]}
- name: C
  dependencies: [{dependsOn: P}]
  script:
    actions:
      onRun: {command: python, args: [-c, "print('V={{Steps.P.Output.V}}')"]}
"#;

    #[test]
    fn step_inputs_from_files() {
        let job = write(CONSUMER);
        let yaml = write("Steps.P.Output.V: 5\n");
        let json_file = write_suffix(r#"{"Steps.P.Output.V": 6}"#, ".json");
        for (file, want) in [(&yaml, "V=5"), (&json_file, "V=6")] {
            let arg = format!("file://{}", file.path().display());
            let (code, stdout, stderr) = run_cli(&[
                "run",
                job.path().to_str().unwrap(),
                "--step",
                "C",
                "--step-inputs",
                &arg,
            ]);
            assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
            assert!(stdout.contains(want), "stdout: {stdout}");
            assert!(!stdout.contains("P_RAN"), "stdout: {stdout}");
        }
    }

    #[test]
    fn step_inputs_argument_errors() {
        let job = write(CONSUMER);
        let missing = "/nonexistent/audit/inputs.yaml";
        for (arg, want) in [
            (
                "not json".to_string(),
                "--step-inputs argument must be a JSON encoded map or a string with the file:// prefix.\n",
            ),
            (
                "[1, 2]".to_string(),
                "--step-inputs argument must be a map from Steps.<Step>.Output.<Name> to a value when decoded.\n",
            ),
            (
                format!("file://{missing}"),
                "Provided step inputs file '/nonexistent/audit/inputs.yaml' does not exist.\n",
            ),
        ] {
            let (code, _stdout, stderr) = run_cli(&[
                "run",
                job.path().to_str().unwrap(),
                "--step",
                "C",
                "--step-inputs",
                &arg,
            ]);
            assert_ne!(code, 0);
            assert!(stderr.ends_with(want), "{arg}: stderr: {stderr}");
        }
    }
}

mod control_flow_run_round2 {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    fn at(stdout: &str, line: &str) -> usize {
        stdout
            .lines()
            .map(|l| l.split_once('\t').map_or(l, |(_, rest)| rest))
            .position(|l| l == line)
            .unwrap_or_else(|| panic!("no line {line:?} in:\n{stdout}"))
    }

    fn json(stdout: &str) -> serde_json::Value {
        let start = stdout.find("{\n").expect("JSON result");
        serde_json::from_str(&stdout[start..]).unwrap()
    }

    /// A group that fails in the same settle that makes a job output's
    /// inputs concrete still lets the job output be evaluated and committed.
    #[test]
    fn job_output_is_evaluated_when_a_group_fails_in_the_same_settle() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Same
jobOutputDefinitions:
- {name: N, type: INT, value: "{{Steps.P.Output.N}}"}
steps:
- name: P
  outputVariables: [{name: N, type: INT}]
  script:
    actions:
      onRun: {command: python, args: [-c, "print('openjd_output_var: N=-1')"]}
- name: C
  dependencies: [{dependsOn: P}]
  parameterSpace:
    taskParameterDefinitions:
    - {name: F, type: INT, range: "{{fail('neg') if Steps.P.Output.N < 0 else range(1)}}"}
  script:
    actions:
      onRun: {command: python, args: [-c, "print(1)"]}
"#,
        );
        let (code, stdout, stderr) =
            run_cli(&["run", job.path().to_str().unwrap(), "--output", "json"]);
        assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
        at(&stdout, "Job output N = -1");
        let result = json(&stdout);
        assert_eq!(result["job_outputs"], serde_json::json!({"N": -1}));
        assert_eq!(result["job_outputs_not_evaluated"], serde_json::json!([]));
    }

    /// `summary` lists the Task parameters of a pending Step from the
    /// template, and indents job output descriptions under their item.
    #[test]
    fn summary_lists_task_parameters_of_pending_steps() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Params
jobOutputDefinitions:
- {name: Count, type: INT, description: How many, value: "{{Steps.P.Output.X}}"}
steps:
- name: P
  outputVariables: [{name: X, type: INT}]
  script: {actions: {onRun: {command: x}}}
- name: D
  dependencies: [{dependsOn: P}]
  parameterSpace:
    taskParameterDefinitions:
    - {name: B, type: STRING, range: [a]}
    - {name: A, type: INT, range: "{{range(Steps.P.Output.X)}}"}
  script: {actions: {onRun: {command: x}}}
"#,
        );
        let (code, stdout, stderr) =
            run_cli(&["summary", job.path().to_str().unwrap(), "--step", "D"]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(
            stdout.contains("Total task parameters: 2\n"),
            "stdout: {stdout}"
        );
        assert!(
            stdout.contains("Parameters:\n- A (INT)\n- B (STRING)\n"),
            "stdout: {stdout}"
        );
        let (_, stdout, _) = run_cli(&["summary", job.path().to_str().unwrap()]);
        assert!(
            stdout.contains("  - Count (INT): pending; waits for Steps.P.Output.X\n    How many\n"),
            "stdout: {stdout}"
        );
        let (_, stdout, _) =
            run_cli(&["summary", job.path().to_str().unwrap(), "--output", "json"]);
        let result: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(
            result["steps"][1]["parameter_definitions"],
            serde_json::json!([{"name": "A", "type": "INT"}, {"name": "B", "type": "STRING"}])
        );
    }

    /// A bound Step run alone that reads an output in its script names the
    /// missing symbol, as a pending one does.
    #[test]
    fn step_alone_bound_consumer_names_missing_symbols() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Bound
steps:
- name: P
  outputVariables: [{name: D, type: INT}]
  script: {actions: {onRun: {command: x}}}
- name: C
  dependencies: [{dependsOn: P}]
  script:
    actions:
      onRun: {command: python, args: [-c, "print({{Steps.P.Output.D}})"]}
"#,
        );
        let (code, _stdout, stderr) =
            run_cli(&["run", job.path().to_str().unwrap(), "--step", "C"]);
        assert_ne!(code, 0);
        assert!(
            stderr.ends_with(
                "Step 'C' reads Steps.* values of Steps this run does not run:\n  \
                 Steps.P.Output.D (INT)\n\
                 Give them with --step-inputs, or run Step 'C' with --run-dependencies. For example:\n  \
                 --step-inputs '{\"Steps.P.Output.D\": 0}'\n"
            ),
            "stderr: {stderr}"
        );
    }

    /// `--tasks` that lists every Task of a Step without output variables
    /// completes it; one that leaves a Task out does not.
    #[test]
    fn tasks_listing_every_task_completes_the_step() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Two
steps:
- name: P
  parameterSpace:
    taskParameterDefinitions:
    - {name: I, type: INT, range: [1, 2]}
  script:
    actions:
      onRun: {command: python, args: [-c, "print({{Task.Param.I}})"]}
"#,
        );
        for (tasks, incomplete) in [
            (r#"[{"I": "2"}, {"I": "1"}]"#, None),
            (r#"[{"I": "1"}, {"I": "1"}]"#, Some(true)),
        ] {
            let (code, stdout, stderr) = run_cli(&[
                "run",
                job.path().to_str().unwrap(),
                "--step",
                "P",
                "--tasks",
                tasks,
                "--output",
                "json",
            ]);
            assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
            assert_eq!(
                json(&stdout)["steps"][0]
                    .get("incomplete")
                    .and_then(|v| v.as_bool()),
                incomplete,
                "{tasks}"
            );
        }
    }
}

mod control_flow_run_round3 {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    fn at(stdout: &str, line: &str) -> usize {
        stdout
            .lines()
            .map(|l| l.split_once('\t').map_or(l, |(_, rest)| rest))
            .position(|l| l == line)
            .unwrap_or_else(|| panic!("no line {line:?} in:\n{stdout}"))
    }

    fn json(stdout: &str) -> serde_json::Value {
        let start = stdout.find("{\n").expect("JSON result");
        serde_json::from_str(&stdout[start..]).unwrap()
    }

    const SIBLING_A: &str = r#"- name: A
  dependencies: [{dependsOn: Q}]
  parameterSpace:
    taskParameterDefinitions:
    - {name: F, type: INT, range: "{{range(Steps.Q.Output.Num) if Steps.Q.Output.Num > 0 else fail('none')}}"}
  script: {actions: {onRun: {command: python, args: [-c, "print(1)"]}}}
"#;
    const SIBLING_B: &str = r#"- name: B
  dependencies: [{dependsOn: Q}]
  parameterSpace:
    taskParameterDefinitions:
    - {name: F, type: INT, range: "{{[Steps.Q.Output.Num + 5]}}"}
  script: {actions: {onRun: {command: python, args: [-c, "print(1)"]}}}
"#;

    /// A group failure does not keep a sibling's Tasks from becoming known,
    /// whichever Step is declared first, so a job output over them is
    /// evaluated either way.
    #[test]
    fn a_group_failure_does_not_depend_on_step_order() {
        for siblings in [[SIBLING_A, SIBLING_B], [SIBLING_B, SIBLING_A]] {
            let job = write(&format!(
                r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Order
jobOutputDefinitions:
- {{name: BF, type: "LIST[INT]", value: "{{{{Steps.B.TaskParam.F}}}}"}}
steps:
- name: Q
  outputVariables: [{{name: Num, type: INT}}]
  script: {{actions: {{onRun: {{command: python, args: [-c, "print('openjd_output_var: Num=0')"]}}}}}}
{}{}"#,
                siblings[0], siblings[1]
            ));
            let (code, stdout, stderr) =
                run_cli(&["run", job.path().to_str().unwrap(), "--output", "json"]);
            assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
            at(&stdout, "Step 'B': Tasks known (1 task)");
            at(&stdout, "Job output BF = [5]");
            assert_eq!(json(&stdout)["job_outputs"], serde_json::json!({"BF": [5]}));
        }
    }

    const FLOATS: &str = r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Floats
jobOutputDefinitions:
- {name: S, type: INT, value: "{{sum(Steps.C.Output.D)}}"}
steps:
- name: C
  parameterSpace:
    taskParameterDefinitions:
    - {name: A, type: FLOAT, range: ["1.50", "2"]}
  outputVariables: [{name: D, type: INT}]
  script:
    actions:
      onRun: {command: python, args: [-c, "print('openjd_output_var: D=1')"]}
"#;

    /// `--tasks` over every Task of a FLOAT range written as `1.50` and `2`
    /// places each output, matching values by number.
    #[test]
    fn tasks_over_float_ranges_place_outputs() {
        let job = write(FLOATS);
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--tasks",
            r#"[{"A": 2}, {"A": 1.5}]"#,
            "--output",
            "json",
        ]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        let result = json(&stdout);
        assert_eq!(
            result["steps"][0],
            serde_json::json!({"name": "C", "state": "SUCCEEDED", "task_count": 2,
                               "outputs": [{"D": 1}, {"D": 1}]})
        );
        assert_eq!(result["job_outputs"], serde_json::json!({"S": 2}));
    }

    /// Tasks given with `--tasks` that do not cover the Step keep their
    /// outputs in the order they ran.
    #[test]
    fn tasks_not_covering_the_step_keep_run_order() {
        let job = write(FLOATS);
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--tasks",
            r#"[{"A": 2}]"#,
            "--output",
            "json",
        ]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert_eq!(
            json(&stdout)["steps"][0],
            serde_json::json!({"name": "C", "state": "SUCCEEDED", "task_count": 2,
                               "incomplete": true, "outputs_in_run_order": true,
                               "outputs": [{"D": 1}]})
        );
    }

    /// A pending Step run alone lists every missing value at once, and when
    /// none can be given with --step-inputs the hint says so.
    #[test]
    fn step_alone_lists_all_missing_values() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Missing
steps:
- name: P
  outputVariables: [{name: Num, type: INT}]
  script: {actions: {onRun: {command: x}}}
- name: Q
  outputVariables: [{name: Mx, type: INT}]
  script: {actions: {onRun: {command: x}}}
- name: R
  dependencies: [{dependsOn: P}]
  parameterSpace:
    taskParameterDefinitions:
    - {name: I, type: INT, range: "{{range(Steps.P.Output.Num)}}"}
  script: {actions: {onRun: {command: x}}}
- name: C
  dependencies: [{dependsOn: P}, {dependsOn: Q}]
  parameterSpace:
    taskParameterDefinitions:
    - {name: F, type: INT, range: "{{range(Steps.P.Output.Num)}}"}
  script: {actions: {onRun: {command: python, args: [-c, "print({{Steps.Q.Output.Mx}})"]}}}
- name: D
  dependencies: [{dependsOn: R}]
  script: {actions: {onRun: {command: python, args: [-c, "print({{Steps.R.TaskParam.I}})"]}}}
"#,
        );
        let (code, _stdout, stderr) =
            run_cli(&["run", job.path().to_str().unwrap(), "--step", "C"]);
        assert_ne!(code, 0);
        assert!(
            stderr.ends_with(
                "Step 'C' reads Steps.* values of Steps this run does not run:\n  \
                 Steps.P.Output.Num (INT)\n  \
                 Steps.Q.Output.Mx (INT)\n\
                 Give them with --step-inputs, or run Step 'C' with --run-dependencies. For example:\n  \
                 --step-inputs '{\"Steps.P.Output.Num\": 0, \"Steps.Q.Output.Mx\": 0}'\n"
            ),
            "stderr: {stderr}"
        );
        let (code, _stdout, stderr) =
            run_cli(&["run", job.path().to_str().unwrap(), "--step", "D"]);
        assert_ne!(code, 0);
        assert!(
            stderr.ends_with(
                "Step 'D' reads Steps.* values of Steps this run does not run:\n  \
                 Steps.R.TaskParam.I (a Task parameter of Step 'R', whose Tasks are known only once it runs)\n\
                 Run Step 'D' with --run-dependencies.\n"
            ),
            "stderr: {stderr}"
        );
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "C",
            "--step-inputs",
            r#"{"Steps.R.Output.X": 1}"#,
        ]);
        assert_ne!(code, 0);
        assert!(
            stderr.ends_with("--step-inputs: Step 'C' does not read the outputs of Step 'R'.\n"),
            "stderr: {stderr}"
        );
    }

    /// A job output whose evaluation failed is marked in JSON, and a
    /// --step-inputs file that does not parse is named.
    #[test]
    fn failed_job_output_json_and_step_inputs_file_errors() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Fail
jobOutputDefinitions:
- {name: Best, type: INT, value: "{{min(Steps.P.Output.V)}}"}
steps:
- name: P
  parameterSpace:
    taskParameterDefinitions:
    - {name: I, type: INT, range: "{{[x for x in [1] if x > 5]}}"}
  outputVariables: [{name: V, type: INT}]
  script: {actions: {onRun: {command: x}}}
- name: C
  dependencies: [{dependsOn: P}]
  script: {actions: {onRun: {command: python, args: [-c, "print({{Steps.P.Output.V}})"]}}}
"#,
        );
        let (code, stdout, stderr) =
            run_cli(&["run", job.path().to_str().unwrap(), "--output", "json"]);
        assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
        assert_eq!(
            json(&stdout)["job_outputs_not_evaluated"],
            serde_json::json!([{"name": "Best", "failed": true}])
        );
        let mut bad = NamedTempFile::with_suffix(".json").unwrap();
        bad.write_all(b"{1: 2}").unwrap();
        let arg = format!("file://{}", bad.path().display());
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "C",
            "--step-inputs",
            &arg,
        ]);
        assert_ne!(code, 0);
        assert!(
            stderr.ends_with(&format!(
                "Cannot parse step inputs file '{}': key must be a string at line 1 column 2\n",
                bad.path().display()
            )),
            "stderr: {stderr}"
        );
    }
}

mod control_flow_run_round4 {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    fn json(stdout: &str) -> serde_json::Value {
        let start = stdout.find("{\n").expect("JSON result");
        serde_json::from_str(&stdout[start..]).unwrap()
    }

    /// A Step run with `--tasks` that fails keeps the outputs of the given
    /// Tasks that succeeded, in run order.
    #[test]
    fn failed_explicit_tasks_keep_their_outputs() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Fails
steps:
- name: P
  parameterSpace:
    taskParameterDefinitions:
    - {name: A, type: INT, range: "1-3"}
  outputVariables: [{name: D, type: INT}]
  script:
    actions:
      onRun: {command: python, args: [-c, "import sys; print('openjd_output_var: D={{Task.Param.A}}'); sys.exit(1 if {{Task.Param.A}} == 3 else 0)"]}
"#,
        );
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--tasks",
            r#"[{"A": 2}, {"A": 1}, {"A": 3}]"#,
            "--output",
            "json",
        ]);
        assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
        assert_eq!(
            json(&stdout)["steps"][0],
            serde_json::json!({"name": "P", "state": "FAILED", "task_count": 3,
                               "reason": "a Task failed", "outputs_in_run_order": true,
                               "outputs": [{"D": 2}, {"D": 1}]})
        );
    }

    /// The hint offers --step-inputs for a missing RawOutput, which a
    /// Steps.P.Output key gives; and a producer's supplied outputs are
    /// marked in JSON.
    #[test]
    fn raw_output_is_suppliable_and_supplied_inputs_are_marked() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Raw
steps:
- name: Q
  outputVariables: [{name: Mx, type: STRING}]
  script: {actions: {onRun: {command: x}}}
- name: C
  dependencies: [{dependsOn: Q}]
  script: {actions: {onRun: {command: python, args: [-c, "print('GOT {{Steps.Q.RawOutput.Mx}}')"]}}}
"#,
        );
        let (code, _stdout, stderr) =
            run_cli(&["run", job.path().to_str().unwrap(), "--step", "C"]);
        assert_ne!(code, 0);
        assert!(
            stderr.ends_with(
                "Step 'C' reads Steps.* values of Steps this run does not run:\n  \
                 Steps.Q.RawOutput.Mx (STRING)\n\
                 Give them with --step-inputs, or run Step 'C' with --run-dependencies. For example:\n  \
                 --step-inputs '{\"Steps.Q.Output.Mx\": \"\"}'\n"
            ),
            "stderr: {stderr}"
        );
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "C",
            "--step-inputs",
            r#"{"Steps.Q.Output.Mx": "x"}"#,
            "--output",
            "json",
        ]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(stdout.contains("GOT x"), "stdout: {stdout}");
        assert_eq!(
            json(&stdout)["steps"][0],
            serde_json::json!({"name": "Q", "state": "READY", "task_count": 1,
                               "inputs_supplied": true})
        );
    }

    /// `--tasks` naming every chunk of a chunked Step without outputs
    /// completes it.
    #[test]
    fn explicit_chunks_covering_the_step_complete_it() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW, TASK_CHUNKING]
name: Chunks
steps:
- name: P
  parameterSpace:
    taskParameterDefinitions:
    - name: X
      type: CHUNK[INT]
      range: "1-4"
      chunks: {defaultTaskCount: 2, rangeConstraint: CONTIGUOUS}
  script: {actions: {onRun: {command: python, args: [-c, "print(1)"]}}}
"#,
        );
        for (tasks, incomplete) in [
            (r#"[{"X": "3-4"}, {"X": "1-2"}]"#, None),
            (r#"[{"X": "1-2"}]"#, Some(true)),
        ] {
            let (code, stdout, stderr) = run_cli(&[
                "run",
                job.path().to_str().unwrap(),
                "--tasks",
                tasks,
                "--output",
                "json",
            ]);
            assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
            assert_eq!(
                json(&stdout)["steps"][0]
                    .get("incomplete")
                    .and_then(|v| v.as_bool()),
                incomplete,
                "{tasks}"
            );
        }
    }

    const DYNAMIC_SKIP: &str = r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Skip
steps:
- name: R
  outputVariables: [{name: Go, type: BOOL}]
  script: {actions: {onRun: {command: python, args: [-c, "print('openjd_output_var: Go=false')"]}}}
- name: S
  dependencies: [{dependsOn: R}]
  enableIf: "{{Steps.R.Output.Go}}"
  parameterSpace:
    taskParameterDefinitions:
    - {name: I, type: INT, range: [1]}
  script: {actions: {onRun: {command: python, args: [-c, "print('S_' + 'RAN')"]}}}
- name: T
  dependencies: [{dependsOn: R}]
  stepEnvironments:
  - {name: E, enableIf: "{{Steps.R.Output.Go}}", variables: {V: v}}
  script: {actions: {onRun: {command: python, args: [-c, "print('T_' + 'RAN')"]}}}
"#;

    /// Tasks given for a Step that is skipped at run time are refused, as for
    /// a Step skipped at Job creation.
    #[test]
    fn explicit_tasks_for_a_dynamically_skipped_step_are_refused() {
        let job = write(DYNAMIC_SKIP);
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "S",
            "--run-dependencies",
            "--tasks",
            r#"[{"I": 1}]"#,
        ]);
        assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
        assert!(
            stderr.contains(
                "ERROR: Step 'S' is skipped by its enableIf; the Tasks given with --tasks or \
                 --task-param cannot be run.\n"
            ),
            "stderr: {stderr}"
        );
        assert!(!stdout.contains("S_RAN"), "stdout: {stdout}");
    }

    /// `--task-param` for a pending Step without a parameterSpace is refused
    /// before the Session starts.
    #[test]
    fn task_param_for_a_pending_step_without_parameter_space_is_refused_early() {
        let job = write(DYNAMIC_SKIP);
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "T",
            "--run-dependencies",
            "-t",
            "X=1",
        ]);
        assert_ne!(code, 0);
        assert!(
            stderr.ends_with(
                "Step 'T' does not define a parameterSpace; --task-param cannot be used.\n"
            ),
            "stderr: {stderr}"
        );
        assert!(!stdout.contains("Session start"), "stdout: {stdout}");
    }
}

mod control_flow_round5 {
    //! Tests for behaviors that round 5's mutation audit found unprotected.
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    fn lines(stdout: &str) -> Vec<&str> {
        stdout
            .lines()
            .map(|l| l.split_once('\t').map_or(l, |(_, rest)| rest))
            .collect()
    }

    fn at(stdout: &str, line: &str) -> usize {
        lines(stdout)
            .iter()
            .position(|l| *l == line)
            .unwrap_or_else(|| panic!("no line {line:?} in:\n{stdout}"))
    }

    fn json(stdout: &str) -> serde_json::Value {
        let start = stdout.find("{\n").expect("JSON result");
        serde_json::from_str(&stdout[start..]).unwrap()
    }

    const TWO: &str = r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Two
jobOutputDefinitions:
- {name: Sum, type: INT, value: "{{Steps.A.Output.X + Steps.B.Output.Y}}"}
- {name: Fl, type: FLOAT, value: "{{1.5}}"}
steps:
- name: A
  outputVariables: [{name: X, type: INT}]
  script: {actions: {onRun: {command: python, args: [-c, "print('openjd_output_var: X=1')"]}}}
- name: B
  dependencies: [{dependsOn: A}]
  outputVariables: [{name: Y, type: INT}, {name: R, type: RANGE_EXPR}]
  script: {actions: {onRun: {command: python, args: [-c, "print('openjd_output_var: Y=2'); print('openjd_output_var: R=\"1-3\"')"]}}}
"#;

    const HOST: &str = r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW, FEATURE_BUNDLE_1]
name: Host
steps:
- name: P
  outputVariables: [{name: Os, type: STRING}, {name: Num, type: INT}]
  script: {actions: {onRun: {command: python, args: [-c, "print('openjd_output_var: Os=\"linux\"'); print('openjd_output_var: Num=2')"]}}}
- name: K
  parameterSpace:
    taskParameterDefinitions:
    - {name: I, type: INT, range: [1, 2]}
  script: {actions: {onRun: {command: python, args: [-c, "print(1)"]}}}
- name: S
  enableIf: false
  outputVariables: [{name: W, type: INT}]
  script: {actions: {onRun: {command: x}}}
- name: C
  dependencies: [{dependsOn: P}, {dependsOn: K}, {dependsOn: S}]
  parameterSpace:
    taskParameterDefinitions:
    - {name: I, type: INT, range: [1, 2]}
  hostRequirements:
    attributes:
    - {name: attr.worker.os.family, anyOf: ["{{Steps.P.Output.Os}}"]}
  let: [w = Steps.S.Output.W or Steps.P.Output.Num]
  script: {actions: {onRun: {command: python, args: [-c, "print({{w}}, {{Steps.K.TaskParam.I}})"]}}}
- name: D
  dependencies: [{dependsOn: P}]
  enableIf: "{{Steps.P.Output.Num > 1}}"
  script: {actions: {onRun: {command: x}}}
jobOutputDefinitions:
- {name: J, type: INT, value: "{{(Steps.S.Output.W or 0) + len(Steps.K.TaskParam.I) + Steps.P.Output.Num}}"}
- {name: Nul, type: INT, value: "{{Steps.S.Output.W}}"}
"#;

    const CHAIN: &str = r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Chain
jobOutputDefinitions:
- {name: Names, type: "LIST[STRING]", value: "{{[string(p) for p in Steps.C.TaskRawParam.Q]}}"}
steps:
- name: P
  outputVariables: [{name: Num, type: INT}, {name: L, type: "LIST[PATH]"}]
  script: {actions: {onRun: {command: python, args: [-c, "print('openjd_output_var: Num=2'); print('openjd_output_var: L=[\"s3://b/x\", \"s3://b/y\"]')"]}}}
- name: C
  dependencies: [{dependsOn: P}]
  enableIf: "{{len(Steps.P.RawOutput.L) == 2}}"
  parameterSpace:
    taskParameterDefinitions:
    - {name: F, type: INT, range: "{{range(Steps.P.Output.Num)}}"}
    - {name: Q, type: PATH, range: "{{Steps.P.RawOutput.L}}"}
    combination: (F, Q)
  script: {actions: {onRun: {command: python, args: [-c, "print('C_RAN')"]}}}
- name: D
  dependencies: [{dependsOn: C}]
  parameterSpace:
    taskParameterDefinitions:
    - {name: G, type: INT, range: "{{Steps.C.TaskParam.F}}"}
  script: {actions: {onRun: {command: python, args: [-c, "print('D_RAN')"]}}}
"#;

    const SI: &str = r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: SI
steps:
- name: A
  outputVariables: [{name: Num, type: INT}]
  script: {actions: {onRun: {command: x}}}
- name: Q
  dependencies: [{dependsOn: A}]
  parameterSpace:
    taskParameterDefinitions:
    - {name: I, type: INT, range: "{{range(Steps.A.Output.Num)}}"}
  outputVariables: [{name: V, type: INT}]
  script: {actions: {onRun: {command: x}}}
- name: S
  enableIf: false
  outputVariables: [{name: W, type: INT}]
  script: {actions: {onRun: {command: x}}}
- name: P
  outputVariables: [{name: L, type: "LIST[INT]", collect: APPEND}]
  script: {actions: {onRun: {command: x}}}
- name: C
  dependencies: [{dependsOn: Q}, {dependsOn: S}, {dependsOn: P}]
  script: {actions: {onRun: {command: python, args: [-c, "print({{Steps.Q.Output.V}}, {{Steps.S.Output.W}}, {{Steps.P.Output.L}}, {{Steps.Q.TaskParam.I}})"]}}}
"#;

    const NE: &str = r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: NE
jobOutputDefinitions:
- {name: Bad, type: INT, value: "{{10 // (Steps.A.Output.X[0] - 1)}}"}
- {name: Two, type: INT, value: "{{Steps.B.Output.Y + Steps.B.Output.Z}}"}
steps:
- name: A
  parameterSpace:
    taskParameterDefinitions:
    - {name: I, type: INT, range: [1, 2]}
  outputVariables: [{name: X, type: INT}]
  script: {actions: {onRun: {command: python, args: [-c, "print('openjd_output_var: X=1')"]}}}
- name: B
  dependencies: [{dependsOn: A}]
  outputVariables: [{name: Y, type: INT}, {name: Z, type: INT}]
  script: {actions: {onRun: {command: python, args: [-c, "print('openjd_output_var: Y=1'); print('openjd_output_var: Z=1')"]}}}
"#;

    const WRAP: &str = r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW, WRAP_ACTIONS]
name: Wrap
jobEnvironments:
- name: W1
  enableIf: false
  script:
    actions:
      onWrapEnvEnter: {command: python, args: [-c, "print('W1 Enter')"]}
      onWrapTaskRun: {command: python, args: [-c, "print('W1 Task')"]}
      onWrapEnvExit: {command: python, args: [-c, "print('W1 Exit')"]}
steps:
- name: S
  script: {actions: {onRun: {command: python, args: [-c, "print('S_RAN')"]}}}
"#;

    /// A job output reading two Steps waits for both; floats and range
    /// expressions print and serialize as reported.
    #[test]
    fn job_output_over_two_steps_and_value_formats() {
        let job = write(TWO);
        let (code, stdout, stderr) =
            run_cli(&["run", job.path().to_str().unwrap(), "--output", "json"]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        let sum = at(&stdout, "Job output Sum = 3");
        assert!(sum > at(&stdout, "Output R = \"1-3\""));
        at(&stdout, "Job output Fl = 1.5");
        let result = json(&stdout);
        assert_eq!(
            result["job_outputs"],
            serde_json::json!({"Sum": 3, "Fl": 1.5})
        );
        assert_eq!(
            result["steps"][1]["outputs"],
            serde_json::json!([{"Y": 2, "R": "1-3"}])
        );
    }

    /// summary: what a pending Step waits on leaves out what is concrete at
    /// creation (a skipped Step's nulls, Tasks already known), merges the
    /// other groups' inputs sorted and without repeats, and shows a null
    /// job output; `--step` shows the same lines.
    #[test]
    fn summary_waits_and_nulls() {
        let job = write(HOST);
        let (code, stdout, stderr) = run_cli(&["summary", job.path().to_str().unwrap()]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        for want in [
            "Total tasks: 5 + (determined at run time)\n",
            "3. 'S' (SKIPPED, 0 total Tasks)\n",
            "4. 'C' (2 total Tasks; pending inputs)\n  Enabled: yes\n  \
             Tasks: 2 (known; Step pending inputs for Steps.P.Output.Num, Steps.P.Output.Os)\n  \
             Host requirements: determined at run time from Steps.P.Output.Os\n",
            "5. 'D' (pending inputs; Tasks determined at run time)\n  \
             Enabled: determined at run time from Steps.P.Output.Num\n  \
             Tasks: determined at run time (waiting for enableIf)\n  \
             Host requirements: determined at run time (waiting for enableIf)\n",
            "  - J (INT): pending; waits for Steps.P.Output.Num\n  - Nul (INT): null (no value)\n",
        ] {
            assert!(stdout.contains(want), "missing {want:?} in:\n{stdout}");
        }
        let (_, stdout, _) = run_cli(&["summary", job.path().to_str().unwrap(), "--step", "C"]);
        assert!(
            stdout.contains(
                "Total tasks: 2\nEnabled: yes\n\
                 Tasks: 2 (known; Step pending inputs for Steps.P.Output.Num, Steps.P.Output.Os)\n\
                 Host requirements: determined at run time from Steps.P.Output.Os\n"
            ),
            "{stdout}"
        );
        let (_, stdout, _) = run_cli(&["summary", job.path().to_str().unwrap(), "--step", "S"]);
        assert!(stdout.contains("Total tasks: 0 (SKIPPED)\n"), "{stdout}");
        let (_, stdout, _) = run_cli(&["summary", job.path().to_str().unwrap(), "--step", "D"]);
        assert!(
            stdout.contains("Total tasks: determined at run time\n"),
            "{stdout}"
        );
        let (_, stdout, _) =
            run_cli(&["summary", job.path().to_str().unwrap(), "--output", "json"]);
        let result: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(
            result["steps"][4]["binding"]["tasks"],
            serde_json::json!({"known": false, "waits_on": [], "waits_for_enable_if": true})
        );
        assert_eq!(
            result["job_outputs"],
            serde_json::json!([
                {"name": "J", "type": "INT", "state": "PENDING", "waits_on": ["Steps.P.Output.Num"]},
                {"name": "Nul", "type": "INT", "state": "NULL"},
            ])
        );
    }

    /// A pending Step whose host requirements read a producer, run alone,
    /// names that input.
    #[test]
    fn step_alone_names_host_requirement_inputs() {
        let job = write(HOST);
        let (code, _stdout, stderr) =
            run_cli(&["run", job.path().to_str().unwrap(), "--step", "C"]);
        assert_ne!(code, 0);
        assert!(
            stderr.ends_with(
                "Step 'C' reads Steps.* values of Steps this run does not run:\n  \
                 Steps.P.Output.Num (INT)\n  \
                 Steps.P.Output.Os (STRING)\n\
                 Give them with --step-inputs, or run Step 'C' with --run-dependencies. For example:\n  \
                 --step-inputs '{\"Steps.P.Output.Num\": 0, \"Steps.P.Output.Os\": \"\"}'\n"
            ),
            "stderr: {stderr}"
        );
    }

    /// A summary of a template without CONTROL_FLOW has no state fields.
    #[test]
    fn summary_json_without_control_flow_has_no_state() {
        let template = templates_dir().join("job_with_test_steps.yaml");
        let (code, stdout, stderr) =
            run_cli(&["summary", template.to_str().unwrap(), "--output", "json"]);
        assert_eq!(code, 0, "stderr: {stderr}");
        assert!(!stdout.contains("\"state\""), "{stdout}");
        let (_, stdout, _) = run_cli(&[
            "summary",
            template.to_str().unwrap(),
            "--step",
            "BareStep",
            "--output",
            "json",
        ]);
        assert!(!stdout.contains("\"state\""), "{stdout}");
    }

    /// A Step whose Tasks become known makes its consumers' TaskParam (and
    /// TaskRawParam) inputs concrete at once: D binds and the job output is
    /// evaluated before C runs; a LIST[PATH] URI output feeds enableIf.
    #[test]
    fn tasks_known_releases_task_param_readers_at_once() {
        let job = write(CHAIN);
        let (code, stdout, stderr) = run_cli(&["run", job.path().to_str().unwrap()]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        let c_runs = at(&stdout, "Running step 'C'");
        assert!(at(&stdout, "Step 'C': enabled (enableIf is true)") < c_runs);
        assert!(at(&stdout, "Step 'D': Tasks known (2 tasks)") < c_runs);
        assert!(at(&stdout, "Step 'D' is ready") < c_runs);
        assert!(at(&stdout, "Job output Names = [\"s3://b/x\", \"s3://b/y\"]") < c_runs);
    }

    /// --step-inputs refusals, each with its full message.
    #[test]
    fn step_inputs_refusals() {
        let job = write(SI);
        for (inputs, want) in [
            (
                r#"{"Steps.Q.Output.V": [1]}"#,
                "--step-inputs: Step 'Q' is pending inputs itself, which --step-inputs cannot simulate; use --run-dependencies.\n",
            ),
            (
                r#"{"Steps.S.Output.W": 1}"#,
                "--step-inputs: Step 'S' is skipped by its enableIf, so its Steps.* values are null; remove 'Steps.S.Output.W'.\n",
            ),
            (
                r#"{"Steps.Q.TaskParam.I": [1]}"#,
                "--step-inputs: the Tasks of Step 'Q' are determined at run time, which --step-inputs cannot simulate; use --run-dependencies.\n",
            ),
            (
                r#"{"Steps.P.Output.L": 5}"#,
                "--step-inputs: Step 'P': Steps.P.Output.L: an APPEND output must be given as a list\n",
            ),
            (
                r#" {"Steps.P.Output.L": [5]}"#,
                "Step 'C' reads Steps.* values of Steps this run does not run:\n  \
                 Steps.Q.Output.V (INT; Step 'Q' is pending inputs itself, so --step-inputs cannot give it)\n  \
                 Steps.Q.TaskParam.I (a Task parameter of Step 'Q', whose Tasks are known only once it runs)\n\
                 Run Step 'C' with --run-dependencies.\n",
            ),
        ] {
            let (code, _stdout, stderr) = run_cli(&[
                "run",
                job.path().to_str().unwrap(),
                "--step",
                "C",
                "--step-inputs",
                inputs,
            ]);
            assert_ne!(code, 0);
            assert!(stderr.ends_with(want), "{inputs}: stderr: {stderr}");
        }
    }

    /// A group failure in the run's first settle (from --step-inputs values)
    /// fails the run.
    #[test]
    fn group_failure_from_step_inputs_fails_the_run() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: First
steps:
- name: P
  outputVariables: [{name: Num, type: INT}]
  script: {actions: {onRun: {command: x}}}
- name: C
  dependencies: [{dependsOn: P}]
  parameterSpace:
    taskParameterDefinitions:
    - {name: F, type: INT, range: "{{range(Steps.P.Output.Num) if Steps.P.Output.Num > 0 else fail('none')}}"}
  script: {actions: {onRun: {command: python, args: [-c, "print(1)"]}}}
"#,
        );
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "C",
            "--step-inputs",
            r#"{"Steps.P.Output.Num": 0}"#,
        ]);
        assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
        assert!(
            stdout.contains("Session ended with errors"),
            "stdout: {stdout}"
        );
        assert!(
            stderr.contains("ERROR: Step 'C' failed in its Task enumeration group:"),
            "{stderr}"
        );
    }

    /// Why each job output was not evaluated, in the log and the result.
    #[test]
    fn job_outputs_not_evaluated_say_why() {
        let job = write(NE);
        let path = job.path().to_str().unwrap();
        for (args, want) in [
            (
                vec!["--maximum-tasks", "1"],
                vec![
                    "Job output Bad was not evaluated: it waits on Step 'A', which did not run all of its Tasks",
                    "  Two = 2",
                    "  Bad: not evaluated (waits on Step 'A', which did not run all of its Tasks)",
                ],
            ),
            (
                vec!["--step", "A"],
                vec![
                    "Job output Bad has no value: its evaluation failed",
                    "Job output Two was not evaluated: it waits on Step 'B', which this run does not run",
                    "  Bad: has no value (its evaluation failed)",
                    "  Two: not evaluated (waits on Step 'B', which this run does not run)",
                ],
            ),
            (
                vec![],
                vec![
                    "Job output Two was not evaluated: it waits on Step 'B', which the run stopped before",
                    "Job outputs:",
                    "  Bad: has no value (its evaluation failed)",
                ],
            ),
        ] {
            let mut all = vec!["run", path];
            all.extend(args);
            let (_code, stdout, _stderr) = run_cli(&all);
            for line in want {
                at(&stdout, line);
            }
        }
    }

    /// A disabled Job Environment's wrap hooks do not count toward the
    /// single-wrap-layer rule.
    #[test]
    fn disabled_wrap_environment_does_not_count() {
        let job = write(WRAP);
        let env = templates_dir().join("wrap_env_second.yaml");
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--environment",
            env.to_str().unwrap(),
        ]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        at(
            &stdout,
            "Job Environment 'W1' is disabled by its enableIf; not entering it",
        );
        assert!(stdout.contains("WrapB Task"), "stdout: {stdout}");
    }

    /// --tasks for a Step skipped at Job creation is refused in preflight.
    #[test]
    fn tasks_for_a_statically_skipped_step_are_refused() {
        let job = write(HOST);
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "S",
            "--tasks",
            r#"[{"I": 1}]"#,
        ]);
        assert_ne!(code, 0);
        assert!(
            stderr.ends_with("Step 'S' is skipped by its enableIf; --tasks cannot be used.\n"),
            "stderr: {stderr}"
        );
        assert!(!stdout.contains("Session start"), "stdout: {stdout}");
    }

    /// `--maximum-tasks` equal to the Task count runs every Task; a smaller
    /// one lists the outputs that arrived.
    #[test]
    fn maximum_tasks_at_and_below_the_task_count() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Max
steps:
- name: P
  parameterSpace:
    taskParameterDefinitions:
    - {name: I, type: INT, range: [1, 2]}
  script: {actions: {onRun: {command: python, args: [-c, "print(1)"]}}}
- name: Q
  parameterSpace:
    taskParameterDefinitions:
    - {name: I, type: INT, range: [1, 2]}
  outputVariables: [{name: V, type: INT}]
  script: {actions: {onRun: {command: python, args: [-c, "print('openjd_output_var: V={{Task.Param.I}}')"]}}}
"#,
        );
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--maximum-tasks",
            "2",
            "--output",
            "json",
        ]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert_eq!(json(&stdout)["steps"][0].get("incomplete"), None);
        let (_, stdout, _) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--maximum-tasks",
            "1",
            "--output",
            "json",
        ]);
        assert_eq!(
            json(&stdout)["steps"][1],
            serde_json::json!({"name": "Q", "state": "SUCCEEDED", "task_count": 2,
                               "incomplete": true, "outputs": [{"V": 1}]})
        );
    }

    /// A pending consumer of a producer that did not run all of its Tasks
    /// is blocked, not an internal error.
    #[test]
    fn pending_consumer_of_an_incomplete_producer_is_blocked() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Pend
steps:
- name: P
  parameterSpace:
    taskParameterDefinitions:
    - {name: I, type: INT, range: [1, 2]}
  outputVariables: [{name: V, type: INT}]
  script: {actions: {onRun: {command: python, args: [-c, "print('openjd_output_var: V={{Task.Param.I}}')"]}}}
- name: C
  dependencies: [{dependsOn: P}]
  parameterSpace:
    taskParameterDefinitions:
    - {name: J, type: INT, range: "{{Steps.P.Output.V}}"}
  script: {actions: {onRun: {command: python, args: [-c, "print(1)"]}}}
- name: D
  dependencies: [{dependsOn: C}]
  script: {actions: {onRun: {command: python, args: [-c, "print(1)"]}}}
"#,
        );
        let (code, stdout, stderr) =
            run_cli(&["run", job.path().to_str().unwrap(), "--maximum-tasks", "1"]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        let c = at(
            &stdout,
            "Step 'C' is blocked: inputs from Step 'P' are incomplete because not all of its Tasks ran",
        );
        assert!(
            at(
                &stdout,
                "Step 'D' is blocked: its dependency 'C' is blocked"
            ) > c
        );
    }
}

mod control_flow_round5_more {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    /// `--tasks` over every Task of a two-parameter space places each
    /// output by its parameter values, in any given order.
    #[test]
    fn tasks_over_two_parameters_place_outputs() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: TwoParams
jobOutputDefinitions:
- {name: All, type: "LIST[STRING]", value: "{{Steps.P.Output.V}}"}
steps:
- name: P
  parameterSpace:
    taskParameterDefinitions:
    - {name: A, type: INT, range: [1, 2]}
    - {name: B, type: STRING, range: [x, y]}
  outputVariables: [{name: V, type: STRING}]
  script: {actions: {onRun: {command: python, args: [-c, "print('openjd_output_var: V=\"{{Task.Param.A}}{{Task.Param.B}}\"')"]}}}
"#,
        );
        for _ in 0..3 {
            let (code, stdout, stderr) = run_cli(&[
                "run",
                job.path().to_str().unwrap(),
                "--tasks",
                r#"[{"B": "y", "A": "2"}, {"A": "1", "B": "x"}, {"B": "y", "A": "1"}, {"A": "2", "B": "x"}]"#,
            ]);
            assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
            assert!(
                stdout.contains("\tJob output All = [\"1x\", \"1y\", \"2x\", \"2y\"]\n"),
                "stdout: {stdout}"
            );
        }
    }

    /// A disabled Environment is marked in summary JSON.
    #[test]
    fn summary_json_marks_disabled_environments() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Envs
jobEnvironments:
- {name: JE, enableIf: false, variables: {A: a}}
- {name: JF, variables: {A: a}}
steps:
- name: S
  script: {actions: {onRun: {command: x}}}
"#,
        );
        let (code, stdout, stderr) =
            run_cli(&["summary", job.path().to_str().unwrap(), "--output", "json"]);
        assert_eq!(code, 0, "stderr: {stderr}");
        let result: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(
            result["root_environments"],
            serde_json::json!([
                {"name": "JE", "parent": "root", "enabled": false},
                {"name": "JF", "parent": "root"},
            ])
        );
    }
}

// ============================================================
// CONTROL_FLOW: golden-file tests of the RFC 0011, 0012, and 0013 sample templates
// ============================================================

#[cfg(target_os = "linux")]
mod control_flow_golden;

/// `openjd run` and `openjd summary` on templates with loops (RFC 0013, plan
/// L8): the iterations, the `<Loop>[<N>].<Step>` naming, the flag rules, and
/// `--step-inputs` for loops.
mod control_flow_loops {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    /// The log lines, without their timestamps.
    fn lines(stdout: &str) -> Vec<&str> {
        stdout
            .lines()
            .map(|l| l.split_once('\t').map_or(l, |(_, rest)| rest))
            .collect()
    }

    /// The position of the first log line equal to `line`.
    fn at(stdout: &str, line: &str) -> usize {
        lines(stdout)
            .iter()
            .position(|l| *l == line)
            .unwrap_or_else(|| panic!("no line {line:?} in:\n{stdout}"))
    }

    fn has(stdout: &str, line: &str) -> bool {
        lines(stdout).contains(&line)
    }

    /// The JSON result after the log.
    fn json(stdout: &str) -> serde_json::Value {
        let start = stdout.find("{\n").expect("JSON result");
        serde_json::from_str(&stdout[start..]).unwrap()
    }

    /// A Python one-liner Action.
    fn py(code: &str) -> String {
        format!(
            "script:\n        actions:\n          onRun:\n            command: python\n            args: ['-c', {}]",
            serde_json::Value::String(code.to_string())
        )
    }

    /// A loop `L` that runs two iterations. Its body Step `P` has two Tasks
    /// and reports `V`; `Q` reads `P`'s outputs, and `R` depends on `P`
    /// without reading them. `C` after the loop fans out over the APPEND
    /// output `H`.
    fn two_iterations() -> String {
        format!(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Loops
jobOutputDefinitions:
- name: Sum
  type: INT
  value: "{{{{sum(Steps.L.Output.H)}}}}"
steps:
- name: L
  loop:
    variables:
    - name: X
      type: INT
      initial: "{{{{1}}}}"
      next: "{{{{Loop.X * 10}}}}"
    until: "{{{{Loop.Iteration >= 1}}}}"
    maxIterations: 5
    steps:
    - name: P
      parameterSpace:
        taskParameterDefinitions:
        - {{name: I, type: INT, range: [1, 2]}}
      outputVariables:
      - {{name: V, type: INT}}
      {p}
    - name: Q
      dependencies: [{{dependsOn: P}}]
      {q}
    - name: R
      dependencies: [{{dependsOn: P}}]
      {r}
  outputVariables:
  - name: H
    type: LIST[INT]
    collect: APPEND
    value: "{{{{sum(Steps.P.Output.V)}}}}"
- name: C
  dependencies: [{{dependsOn: L}}]
  parameterSpace:
    taskParameterDefinitions:
    - {{name: Y, type: INT, range: "{{{{Steps.L.Output.H}}}}"}}
  {c}
"#,
            p = py("print('openjd_output_var: V=' + str({{Task.Param.I}} * {{Loop.X}}))"),
            q = py("print('Q_' + '{{Loop.Iteration}}' + '_' + str({{Steps.P.Output.V}}))"),
            r = py("print('R_' + '{{Loop.Iteration}}')"),
            c = py("print('C_' + '{{Task.Param.Y}}')").replace("\n      ", "\n  "),
        )
    }

    #[test]
    fn a_loop_runs_its_iterations_and_feeds_its_dependents() {
        let job = write(&two_iterations());
        let path = job.path().to_str().unwrap();
        let (code, stdout, stderr) = run_cli(&["check", path]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        let (code, stdout, stderr) = run_cli(&["run", path]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        let order = [
            "Running loop 'L'",
            "Loop 'L' iteration 0: X = 1",
            "Running step 'L[0].P'",
            "Running step 'L[0].Q'",
            "Q_0_[1, 2]",
            "Running step 'L[0].R'",
            "Loop 'L' iteration 0 outputs: H = 3",
            "Loop 'L' iteration 1: X = 10",
            "Running step 'L[1].P'",
            "Q_1_[10, 20]",
            "Loop 'L' iteration 1 outputs: H = 30",
            "Loop 'L' succeeded after 2 iterations (until is true)",
            "Loop 'L' outputs: H = [3, 30]",
            "Step 'C': Tasks known (2 tasks)",
            "Step 'C' is ready",
            "Job output Sum = 33",
            "Running step 'C'",
            "C_3",
            "C_30",
        ];
        let positions: Vec<usize> = order.iter().map(|l| at(&stdout, l)).collect();
        assert!(
            positions.windows(2).all(|w| w[0] < w[1]),
            "out of order: {positions:?}\n{stdout}"
        );
    }

    #[test]
    fn json_output_lists_the_loop_and_each_body_step_instance() {
        let job = write(&two_iterations());
        let (code, stdout, stderr) =
            run_cli(&["run", job.path().to_str().unwrap(), "--output", "json"]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        let result = json(&stdout);
        let steps = result["steps"].as_array().unwrap();
        let names: Vec<&str> = steps.iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            ["L", "L[0].P", "L[0].Q", "L[0].R", "L[1].P", "L[1].Q", "L[1].R", "C"]
        );
        assert_eq!(
            steps[0],
            serde_json::json!({
                "name": "L", "kind": "loop", "state": "SUCCEEDED", "max_iterations": 5,
                "iterations": 2, "outputs": {"H": [3, 30]},
                "iteration_values": [
                    {"iteration": 0, "variables": {"X": 1}, "outputs": {"H": 3}},
                    {"iteration": 1, "variables": {"X": 10}, "outputs": {"H": 30}},
                ],
            })
        );
        assert_eq!(
            steps[4],
            serde_json::json!({
                "name": "L[1].P", "loop": "L", "iteration": 1, "state": "SUCCEEDED",
                "task_count": 2, "outputs": [{"V": 10}, {"V": 20}],
            })
        );
        assert_eq!(result["job_outputs"], serde_json::json!({"Sum": 33}));
    }

    #[test]
    fn a_failed_body_step_instance_fails_the_loop_and_blocks_its_dependents() {
        let template = two_iterations().replace(
            "print('R_' + '{{Loop.Iteration}}')",
            "import sys; sys.exit({{Loop.Iteration}})",
        );
        let job = write(&template);
        let (code, stdout, stderr) =
            run_cli(&["run", job.path().to_str().unwrap(), "--output", "json"]);
        assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
        assert!(
            at(
                &stdout,
                "Loop 'L' failed: its body Step instance 'L[1].R' failed"
            ) < at(&stdout, "Step 'C' is blocked: its dependency 'L' failed")
        );
        assert!(!has(&stdout, "Running step 'C'"));
        let result = json(&stdout);
        let steps = result["steps"].as_array().unwrap();
        assert_eq!(steps[0]["state"], "FAILED");
        assert_eq!(steps[0]["reason"], "its body Step instance 'L[1].R' failed");
        assert_eq!(steps[0]["iterations"], 2);
        assert!(steps[0].get("outputs").is_none(), "{}", steps[0]);
        assert_eq!(steps[6]["name"], "L[1].R");
        assert_eq!(steps[6]["state"], "FAILED");
        assert_eq!(steps[6]["reason"], "a Task failed");
        assert_eq!(steps[7]["state"], "BLOCKED");
        assert_eq!(
            result["job_outputs_not_evaluated"],
            serde_json::json!([{"name": "Sum", "waits_on": ["Loop 'L', which failed"]}])
        );
    }

    /// RFC 0013 §Implementation Impact: `--maximum-tasks` applies to each
    /// body Step instance. `L[0].P` runs one of its two Tasks, so `L[0].Q`,
    /// which reads its outputs, is blocked, `L[0].R` runs, and the boundary
    /// is never evaluated: the loop and `C` are blocked.
    #[test]
    fn maximum_tasks_applies_to_each_body_step_instance() {
        let job = write(&two_iterations());
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--maximum-tasks",
            "1",
            "--output",
            "json",
        ]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        for line in [
            "Step 'L[0].Q' is blocked: inputs from Step 'L[0].P' are incomplete because not all of its Tasks ran",
            "R_0",
            "Loop 'L' is blocked: its body Step instance 'L[0].P' did not run all of its Tasks, so iteration 0 never completes",
            "Step 'C' is blocked: its dependency 'L' is blocked",
            "Job output Sum was not evaluated: it waits on Loop 'L', which is blocked",
        ] {
            assert!(has(&stdout, line), "no line {line:?} in:\n{stdout}");
        }
        assert!(!has(&stdout, "Loop 'L' iteration 1: X = 10"));
        let steps = json(&stdout)["steps"].as_array().unwrap().clone();
        assert_eq!(steps[0]["state"], "BLOCKED");
        assert_eq!(steps[1]["incomplete"], true);
        assert_eq!(steps[2]["state"], "BLOCKED");
        assert_eq!(steps[3]["state"], "SUCCEEDED");
        assert_eq!(steps[4]["name"], "C");
        assert_eq!(steps[4]["state"], "BLOCKED");
    }

    #[test]
    fn step_naming_a_body_step_is_an_error_that_names_its_loop() {
        let job = write(&two_iterations());
        let (code, stdout, stderr) = run_cli(&["run", job.path().to_str().unwrap(), "--step", "Q"]);
        assert_eq!(code, 1, "{stdout}");
        assert_eq!(
            stderr.trim_end(),
            "ERROR: Step 'Q' is a body Step of loop 'L', so it runs only within the loop's iterations. Use --step L to run the whole loop."
        );
        let (code, _, stderr) = run_cli(&["summary", job.path().to_str().unwrap(), "--step", "Q"]);
        assert_eq!(code, 1);
        assert_eq!(
            stderr.trim_end(),
            "ERROR: Step 'Q' is a body Step of loop 'L'; summarize the loop with --step L."
        );
    }

    /// A Job whose only top-level entry is a loop, so it is selected without
    /// `--step`.
    fn only_a_loop() -> String {
        let t = two_iterations();
        let end = t.find("- name: C\n").unwrap();
        t[..end]
            .replace("jobOutputDefinitions:\n- name: Sum\n  type: INT\n  value: \"{{sum(Steps.L.Output.H)}}\"\n", "")
    }

    #[test]
    fn tasks_and_task_param_are_rejected_for_a_selected_loop() {
        let two = write(&two_iterations());
        let one = write(&only_a_loop());
        for (job, step) in [(&two, Some("L")), (&one, Some("L")), (&one, None)] {
            for (flag, value, option) in [
                ("--tasks", r#"[{"I": "1"}]"#, "--tasks"),
                ("--task-param", "I=1", "--task-param"),
            ] {
                let mut args = vec!["run", job.path().to_str().unwrap(), flag, value];
                if let Some(step) = step {
                    args.extend(["--step", step]);
                }
                let (code, stdout, stderr) = run_cli(&args);
                assert_eq!(code, 1, "{args:?}: {stdout}");
                assert_eq!(
                    stderr.trim_end(),
                    format!(
                        "ERROR: Loop 'L' has no Task parameters or Tasks of its own; {option} cannot be used."
                    ),
                    "{args:?}"
                );
            }
        }
    }

    /// An explicit `--maximum-tasks`, even its default value, is rejected
    /// with `--step` naming a loop; selecting the only loop without `--step`
    /// applies it to each body Step instance.
    #[test]
    fn explicit_maximum_tasks_is_rejected_only_with_step_naming_a_loop() {
        let job = write(&only_a_loop());
        let path = job.path().to_str().unwrap();
        for value in ["1", "-1"] {
            let (code, stdout, stderr) =
                run_cli(&["run", path, "--step", "L", "--maximum-tasks", value]);
            assert_eq!(code, 1, "{stdout}");
            assert_eq!(
                stderr.trim_end(),
                "ERROR: Loop 'L' has no Tasks of its own; --maximum-tasks cannot be used with --step naming a loop. Without --step, --maximum-tasks applies to each body Step instance."
            );
        }
        let (code, stdout, stderr) = run_cli(&["run", path, "--step", "L"]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(has(
            &stdout,
            "Loop 'L' succeeded after 2 iterations (until is true)"
        ));
        let (code, stdout, stderr) = run_cli(&["run", path, "--maximum-tasks", "1"]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(has(
            &stdout,
            "Loop 'L' is blocked: its body Step instance 'L[0].P' did not run all of its Tasks, so iteration 0 never completes"
        ));
    }

    /// `B` reports `N`; loop `L` depends on `B`, its `initial` reads `N`,
    /// and its body reads `B`'s `Text`.
    const READS_DEPENDENCY: &str = r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Dep
steps:
- name: B
  outputVariables:
  - {name: N, type: INT}
  - {name: Text, type: STRING}
  script:
    actions:
      onRun:
        command: python
        args: ['-c', "print('openjd_output_var: N=2'); print('openjd_output_var: Text=\"hi\"')"]
- name: X
  outputVariables:
  - {name: Z, type: INT}
  script:
    actions:
      onRun:
        command: python
        args: ['-c', "print('openjd_output_var: Z=1')"]
- name: L
  dependencies: [{dependsOn: B}]
  loop:
    variables:
    - {name: K, type: INT, initial: "{{Steps.B.Output.N}}", next: "{{Loop.K - 1}}"}
    while: "{{Loop.K > 0}}"
    maxIterations: 5
    steps:
    - name: W
      script:
        actions:
          onRun:
            command: python
            args: ['-c', "print('W_{{Loop.K}}_{{Steps.B.Output.Text}}')"]
"#;

    #[test]
    fn a_pending_loop_is_bound_when_its_dependency_succeeds() {
        let job = write(READS_DEPENDENCY);
        let (code, stdout, stderr) = run_cli(&["run", job.path().to_str().unwrap()]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        let pending = at(&stdout, "Loop 'L' is pending inputs:");
        assert_eq!(
            lines(&stdout)[pending + 1..pending + 4],
            [
                "  Enabled: yes",
                "  maxIterations: 5",
                "  Start (initial values and first while): determined at run time from Steps.B.Output.N",
            ]
        );
        assert!(at(&stdout, "Loop 'L' is ready") > at(&stdout, "Running step 'B'"));
        for line in [
            "W_2_hi",
            "W_1_hi",
            "Loop 'L' succeeded after 2 iterations (while is false)",
        ] {
            assert!(has(&stdout, line), "no line {line:?} in:\n{stdout}");
        }
    }

    #[test]
    fn step_inputs_supply_a_loops_dependencies_for_step_naming_the_loop() {
        let job = write(READS_DEPENDENCY);
        let path = job.path().to_str().unwrap();
        // initial reads N while the loop is pending.
        let (code, _, stderr) = run_cli(&["run", path, "--step", "L"]);
        assert_eq!(code, 1);
        assert_eq!(
            stderr.trim_end(),
            "ERROR: Loop 'L' reads Steps.* values of Steps this run does not run:\n  \
             Steps.B.Output.N (INT)\n\
             Give them with --step-inputs, or run Loop 'L' with --run-dependencies. For example:\n  \
             --step-inputs '{\"Steps.B.Output.N\": 0}'"
        );
        // Only the outputs the loop reads need values: given N, the loop
        // binds, and iteration 0's W reads Text.
        let (code, _, stderr) = run_cli(&[
            "run",
            path,
            "--step",
            "L",
            "--step-inputs",
            r#"{"Steps.B.Output.N": 1}"#,
        ]);
        assert_eq!(code, 1);
        assert_eq!(
            stderr.trim_end(),
            "ERROR: Loop 'L' reads Steps.* values of Steps this run does not run:\n  \
             Steps.B.Output.Text (STRING)\n\
             Give them with --step-inputs, or run Loop 'L' with --run-dependencies. For example:\n  \
             --step-inputs '{\"Steps.B.Output.N\": 1, \"Steps.B.Output.Text\": \"\"}'"
        );
        let (code, stdout, stderr) = run_cli(&[
            "run",
            path,
            "--step",
            "L",
            "--step-inputs",
            r#"{"Steps.B.Output.N": 1, "Steps.B.Output.Text": "yo"}"#,
        ]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(has(&stdout, "W_1_yo"), "{stdout}");
        assert!(!has(&stdout, "Running step 'B'"));
        // X is not a dependency of L, so L cannot read it.
        let (code, _, stderr) = run_cli(&[
            "run",
            path,
            "--step",
            "L",
            "--step-inputs",
            r#"{"Steps.X.Output.Z": 1}"#,
        ]);
        assert_eq!(code, 1);
        assert_eq!(
            stderr.trim_end(),
            "ERROR: --step-inputs: Loop 'L' does not read the outputs of Step 'X'."
        );
        // --run-dependencies runs B first.
        let (code, stdout, stderr) = run_cli(&["run", path, "--step", "L", "--run-dependencies"]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(at(&stdout, "Running step 'B'") < at(&stdout, "Running loop 'L'"));
        assert!(!has(&stdout, "Running step 'X'"));
    }

    /// A body Step reads a loop dependency that only its script reads, which
    /// is found from iteration 0's instances.
    #[test]
    fn step_inputs_find_what_the_body_reads() {
        let template =
            READS_DEPENDENCY.replace("initial: \"{{Steps.B.Output.N}}\"", "initial: \"{{2}}\"");
        let job = write(&template);
        let (code, _, stderr) = run_cli(&["run", job.path().to_str().unwrap(), "--step", "L"]);
        assert_eq!(code, 1);
        assert_eq!(
            stderr.trim_end(),
            "ERROR: Loop 'L' reads Steps.* values of Steps this run does not run:\n  \
             Steps.B.Output.Text (STRING)\n\
             Give them with --step-inputs, or run Loop 'L' with --run-dependencies. For example:\n  \
             --step-inputs '{\"Steps.B.Output.Text\": \"\"}'"
        );
    }

    #[test]
    fn step_inputs_supply_loop_outputs_in_their_declared_type() {
        let job = write(&two_iterations());
        let path = job.path().to_str().unwrap();
        let (code, _, stderr) = run_cli(&["run", path, "--step", "C"]);
        assert_eq!(code, 1);
        assert_eq!(
            stderr.trim_end(),
            "ERROR: Step 'C' reads Steps.* values of Steps this run does not run:\n  \
             Steps.L.Output.H (LIST[INT], the whole list)\n\
             Give them with --step-inputs, or run Step 'C' with --run-dependencies. For example:\n  \
             --step-inputs '{\"Steps.L.Output.H\": []}'"
        );
        let (code, stdout, stderr) = run_cli(&[
            "run",
            path,
            "--step",
            "C",
            "--step-inputs",
            r#"{"Steps.L.Output.H": [7, 8, 9]}"#,
            "--output",
            "json",
        ]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        for line in ["C_7", "C_8", "C_9", "Job output Sum = 24"] {
            assert!(has(&stdout, line), "no line {line:?} in:\n{stdout}");
        }
        let steps = json(&stdout)["steps"].as_array().unwrap().clone();
        assert_eq!(steps[0]["name"], "L");
        assert_eq!(steps[0]["inputs_supplied"], true);
        for (value, want) in [
            (
                r#"{"Steps.L.Output.H": 3}"#,
                "ERROR: --step-inputs: Loop 'L': Steps.L.Output.H: an APPEND output must be given as a list",
            ),
            (
                r#"{"Steps.L.Output.H": ["a"]}"#,
                "ERROR: --step-inputs: Loop 'L': Steps.L.Output.H: Output variable 'H': element (string) 'a' does not convert to int: Cannot convert 'a' to int: invalid digit found in string",
            ),
            (
                r#"{"Steps.L.TaskParam.H": [1]}"#,
                "ERROR: --step-inputs: Loop 'L' has no Task parameters; remove 'Steps.L.TaskParam.H'.",
            ),
            (
                r#"{"Steps.L.Output.G": [1]}"#,
                "ERROR: --step-inputs: Loop 'L' declares no output variable 'G'.",
            ),
            (
                r#"{"Steps.P.Output.V": [1]}"#,
                "ERROR: --step-inputs: Step 'P' is a body Step of loop 'L', and its outputs are not available outside the loop.",
            ),
        ] {
            let (code, _, stderr) =
                run_cli(&["run", path, "--step", "C", "--step-inputs", value]);
            assert_eq!(code, 1, "{value}");
            assert_eq!(stderr.trim_end(), want, "{value}");
        }
    }

    #[test]
    fn a_zero_iteration_loop_succeeds_with_empty_outputs() {
        let template = READS_DEPENDENCY
            .replace("initial: \"{{Steps.B.Output.N}}\"", "initial: \"{{0}}\"")
            + "  outputVariables:\n  - {name: Last, type: INT, collect: LAST, value: \"{{Loop.K}}\"}\n  - {name: All, type: 'LIST[INT]', collect: APPEND, value: \"{{Loop.K}}\"}\n";
        let job = write(&template);
        let (code, stdout, stderr) =
            run_cli(&["run", job.path().to_str().unwrap(), "--output", "json"]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(has(
            &stdout,
            "Loop 'L' succeeded after 0 iterations (the first while is false)"
        ));
        assert!(has(&stdout, "Loop 'L' outputs: Last = null, All = []"));
        assert!(!has(&stdout, "Running loop 'L'"));
        let steps = json(&stdout)["steps"].as_array().unwrap().clone();
        assert_eq!(
            steps[2],
            serde_json::json!({
                "name": "L", "kind": "loop", "state": "SUCCEEDED", "max_iterations": 5,
                "iterations": 0, "outputs": {"Last": null, "All": []},
            })
        );
    }

    #[test]
    fn a_skipped_loop_runs_no_iterations_and_its_outputs_are_null() {
        let template = two_iterations()
            .replace(
                "- name: L\n  loop:",
                "- name: L\n  enableIf: \"{{False}}\"\n  loop:",
            )
            .replace(
                "range: \"{{Steps.L.Output.H}}\"",
                "range: \"{{Steps.L.Output.H or [5]}}\"",
            )
            .replace(
                "{{sum(Steps.L.Output.H)}}",
                "{{len(Steps.L.Output.H or [])}}",
            );
        let job = write(&template);
        let (code, stdout, stderr) =
            run_cli(&["run", job.path().to_str().unwrap(), "--output", "json"]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        for line in [
            "Loop 'L' skipped (enableIf is false)",
            "Job output Sum = 0",
            "Loop 'L' is skipped; it runs no iterations",
            "C_5",
        ] {
            assert!(has(&stdout, line), "no line {line:?} in:\n{stdout}");
        }
        let steps = json(&stdout)["steps"].as_array().unwrap().clone();
        assert_eq!(
            steps[0],
            serde_json::json!({
                "name": "L", "kind": "loop", "state": "SKIPPED", "iterations": 0,
                "outputs": {"H": null},
            })
        );
    }

    /// An error in a body Step instance's group, at the start of an
    /// iteration, fails the loop finally.
    #[test]
    fn an_instance_group_error_fails_the_loop() {
        let template = two_iterations().replace(
            "range: [1, 2]",
            "range: \"{{[1, 2] if Loop.Iteration == 0 else fail('no frames in ' + string(Loop.Iteration))}}\"",
        );
        let job = write(&template);
        let (code, stdout, stderr) =
            run_cli(&["run", job.path().to_str().unwrap(), "--output", "json"]);
        assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
        assert!(has(
            &stdout,
            "Loop 'L' failed: Step 'L[1].P' failed in its Task enumeration group"
        ));
        // Iteration 1 started, though start_iteration returned no instances.
        let steps = json(&stdout)["steps"].as_array().unwrap().clone();
        assert_eq!(steps[0]["iterations"], 2);
        assert_eq!(
            steps[0]["iteration_values"][1]["variables"],
            serde_json::json!({"X": 10})
        );
        let failed = steps.iter().find(|s| s["name"] == "L[1].P").unwrap();
        assert_eq!(failed["state"], "FAILED");
        assert_eq!(failed["iteration"], 1);
        assert!(
            failed["reason"]
                .as_str()
                .unwrap()
                .starts_with("Step 'L[1].P' failed in its Task enumeration group:"),
            "{failed}"
        );
        assert!(
            stderr.starts_with("ERROR: Step 'L[1].P' failed in its Task enumeration group:\n"),
            "{stderr}"
        );
        assert!(stderr.contains("\tno frames in 1\n"), "{stderr}");
        assert!(has(
            &stdout,
            "Step 'C' is blocked: its dependency 'L' failed"
        ));
    }

    #[test]
    fn template_help_lists_loops_and_their_body_steps() {
        let job = write(&two_iterations());
        let (code, stdout, stderr) = run_cli(&["run", job.path().to_str().unwrap(), "--help"]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(
            stdout.contains(
                "Loops (--step <Loop> runs the whole loop; its body Steps run only within it):\n  L: body Steps P, Q, R\n\n"
            ),
            "{stdout}"
        );
    }

    #[test]
    fn summary_counts_a_zero_iteration_loop_as_zero_tasks() {
        let template =
            READS_DEPENDENCY.replace("initial: \"{{Steps.B.Output.N}}\"", "initial: \"{{0}}\"");
        let job = write(&template);
        let (code, stdout, stderr) = run_cli(&["summary", job.path().to_str().unwrap()]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(stdout.contains("Total tasks: 2\n"), "{stdout}");
        assert!(
            stdout.contains("3. 'L' (loop; 0 iterations, 0 total Tasks)\n"),
            "{stdout}"
        );
        let skipped = two_iterations()
            .replace(
                "- name: L\n  loop:",
                "- name: L\n  enableIf: \"{{False}}\"\n  loop:",
            )
            .replace(
                "range: \"{{Steps.L.Output.H}}\"",
                "range: \"{{Steps.L.Output.H or [5]}}\"",
            )
            .replace(
                "{{sum(Steps.L.Output.H)}}",
                "{{len(Steps.L.Output.H or [])}}",
            );
        let job = write(&skipped);
        let (code, stdout, stderr) = run_cli(&["summary", job.path().to_str().unwrap()]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(stdout.contains("1. 'L' (loop; SKIPPED, 0 total Tasks)\n  Condition: until\n  maxIterations: not evaluated (the loop is skipped)\n"), "{stdout}");
    }

    #[test]
    fn summary_step_naming_a_loop_and_a_pending_loop() {
        let job = write(READS_DEPENDENCY);
        let (code, stdout, stderr) =
            run_cli(&["summary", job.path().to_str().unwrap(), "--step", "L"]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert_eq!(
            stdout,
            "\n--- Summary for loop 'L' in Job 'Dep' ---\n\n\
             State: PENDING_INPUTS (loop; pending inputs; Tasks determined at run time)\n\
             Enabled: yes\n\
             maxIterations: 5\n\
             Start (initial values and first while): determined at run time from Steps.B.Output.N\n\
             Condition: while\n\
             Loop variables:\n  - K (INT)\n\
             Body Steps (Tasks determined at run time in each iteration):\n  - 'W'\n\n\
             Dependencies (1):\n- 'B'\n\n"
        );
        let (code, stdout, stderr) = run_cli(&[
            "summary",
            job.path().to_str().unwrap(),
            "--step",
            "L",
            "--output",
            "json",
        ]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        let result: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(
            result["binding"],
            serde_json::json!({
                "enablement": {"known": true},
                "bound": {"known": true},
                "start": {"known": false, "waits_on": ["Steps.B.Output.N"], "waits_for_enable_if": false},
            })
        );
        assert_eq!(
            result["dependencies"],
            serde_json::json!([{"step_name": "B"}])
        );
    }

    /// `A` has two Tasks and reports `V`; loop `L` depends on `A`, and only
    /// its boundary (`until`) reads `A`'s outputs.
    const BOUNDARY_READS_DEPENDENCY: &str = r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Boundary
steps:
- name: A
  parameterSpace:
    taskParameterDefinitions:
    - {name: I, type: INT, range: [1, 2]}
  outputVariables:
  - {name: V, type: INT}
  script:
    actions:
      onRun:
        command: python
        args: ['-c', "print('openjd_output_var: V={{Task.Param.I}}')"]
- name: L
  dependencies: [{dependsOn: A}]
  loop:
    until: "{{Loop.Iteration + 1 >= len(Steps.A.Output.V)}}"
    maxIterations: 5
    steps:
    - name: B
      script:
        actions:
          onRun:
            command: python
            args: ['-c', "print('B_{{Loop.Iteration}}')"]
"#;

    /// A dependency that did not run all of its Tasks blocks a loop whose
    /// boundary reads it, before its first iteration.
    #[test]
    fn an_incomplete_dependency_the_boundary_reads_blocks_the_loop() {
        let job = write(BOUNDARY_READS_DEPENDENCY);
        let path = job.path().to_str().unwrap();
        let (code, stdout, stderr) = run_cli(&["run", path]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(has(
            &stdout,
            "Loop 'L' succeeded after 2 iterations (until is true)"
        ));
        let (code, stdout, stderr) =
            run_cli(&["run", path, "--maximum-tasks", "1", "--output", "json"]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert_eq!(stderr, "");
        assert!(has(
            &stdout,
            "Loop 'L' is blocked: inputs from Step 'A' are incomplete because not all of its Tasks ran"
        ));
        assert!(!has(&stdout, "Running loop 'L'"));
        let steps = json(&stdout)["steps"].as_array().unwrap().clone();
        assert_eq!(steps[1]["state"], "BLOCKED");
    }

    /// `Q` runs only from iteration 1, and reads `A`, which `--step L` does
    /// not run: the preflight sees only iteration 0, so the loop fails when
    /// iteration 1 needs the value.
    #[test]
    fn a_later_iteration_reading_a_dependency_not_run_fails_the_loop() {
        let template = r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Later
steps:
- name: A
  outputVariables:
  - {name: Base, type: INT}
  script:
    actions:
      onRun:
        command: python
        args: ['-c', "print('openjd_output_var: Base=7')"]
- name: L
  dependencies: [{dependsOn: A}]
  loop:
    until: "{{Loop.Iteration >= 1}}"
    maxIterations: 3
    steps:
    - name: Q
      enableIf: "{{Loop.Iteration >= 1}}"
      script:
        actions:
          onRun:
            command: python
            args: ['-c', "print('Q_{{Steps.A.Output.Base}}')"]
"#;
        let job = write(template);
        let path = job.path().to_str().unwrap();
        let (code, stdout, stderr) = run_cli(&["run", path, "--step", "L", "--output", "json"]);
        assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
        assert_eq!(
            stderr.trim_end(),
            "ERROR: Loop 'L' reads, in iteration 1, Steps.* values of 'A', which this run does not run.\nGive them with --step-inputs, or run Loop 'L' with --run-dependencies."
        );
        assert!(has(
            &stdout,
            "Loop 'L' failed: Loop 'L' reads, in iteration 1, Steps.* values of 'A', which this run does not run."
        ));
        assert_eq!(json(&stdout)["steps"][1]["state"], "FAILED");
        let (code, stdout, stderr) = run_cli(&[
            "run",
            path,
            "--step",
            "L",
            "--step-inputs",
            r#"{"Steps.A.Output.Base": 3}"#,
        ]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(has(&stdout, "Q_3"), "{stdout}");
    }

    /// As above, with `A` pending inputs at Job creation (its `enableIf`
    /// reads `Z`), which `--step-inputs` cannot give, so the hint is only to
    /// use `--run-dependencies`, and the instance that needed it is failed.
    #[test]
    fn a_later_iteration_reading_a_pending_dependency_suggests_run_dependencies() {
        let template = r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Later
steps:
- name: Z
  outputVariables:
  - {name: N, type: INT}
  script:
    actions:
      onRun:
        command: python
        args: ['-c', "print('openjd_output_var: N=1')"]
- name: A
  dependencies: [{dependsOn: Z}]
  enableIf: "{{Steps.Z.Output.N > 0}}"
  outputVariables:
  - {name: Base, type: INT}
  script:
    actions:
      onRun:
        command: python
        args: ['-c', "print('openjd_output_var: Base=7')"]
- name: L
  dependencies: [{dependsOn: A}]
  loop:
    until: "{{Loop.Iteration >= 1}}"
    maxIterations: 3
    steps:
    - name: Q
      enableIf: "{{Loop.Iteration >= 1}}"
      script:
        actions:
          onRun:
            command: python
            args: ['-c', "print('Q_{{Steps.A.Output.Base}}')"]
"#;
        let job = write(template);
        let path = job.path().to_str().unwrap();
        let (code, stdout, stderr) = run_cli(&["run", path, "--step", "L", "--output", "json"]);
        assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
        assert_eq!(
            stderr.trim_end(),
            "ERROR: Loop 'L' reads, in iteration 1, Steps.* values of 'A', which this run does not run.\nRun Loop 'L' with --run-dependencies."
        );
        let steps = json(&stdout)["steps"].as_array().unwrap().clone();
        let q = steps.iter().find(|s| s["name"] == "L[1].Q").unwrap();
        assert_eq!(q["state"], "FAILED");
        assert_eq!(
            q["reason"],
            "Loop 'L' reads, in iteration 1, Steps.* values of 'A', which this run does not run.\nRun Loop 'L' with --run-dependencies."
        );
        let (code, stdout, stderr) = run_cli(&["run", path, "--step", "L", "--run-dependencies"]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(has(&stdout, "Q_7"), "{stdout}");
    }

    #[test]
    fn run_dependencies_for_a_step_after_a_loop_runs_the_loop() {
        let template = two_iterations().replace(
            "steps:\n- name: L\n",
            "steps:\n- name: Other\n  script:\n    actions:\n      onRun:\n        command: python\n        args: ['-c', \"print('OTHER')\"]\n- name: L\n",
        );
        let job = write(&template);
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "C",
            "--run-dependencies",
        ]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(at(&stdout, "Running loop 'L'") < at(&stdout, "Running step 'C'"));
        for line in ["C_3", "C_30", "Job output Sum = 33"] {
            assert!(has(&stdout, line), "no line {line:?} in:\n{stdout}");
        }
        assert!(!has(&stdout, "OTHER"));
    }

    #[test]
    fn task_params_without_step_name_every_top_level_entry_in_template_order() {
        let job = write(&two_iterations());
        let (code, _, stderr) = run_cli(&["run", job.path().to_str().unwrap(), "-t", "I=1"]);
        assert_eq!(code, 1);
        assert_eq!(
            stderr.trim_end(),
            "ERROR: Providing task parameters requires a specified step or a job with a single step.\n2 steps: [\"L\", \"C\"]."
        );
    }

    #[test]
    fn a_null_last_output_is_given_only_where_its_type_is_optional() {
        let until = r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Nullable
steps:
- name: M
  loop:
    until: "{{True}}"
    maxIterations: 1
    steps:
    - name: W
      script: {actions: {onRun: {command: python, args: ['-c', 'pass']}}}
  outputVariables:
  - {name: O, type: INT, collect: LAST, value: "{{1}}"}
- name: C
  dependencies: [{dependsOn: M}]
  script:
    actions:
      onRun:
        command: python
        args: ['-c', "print('C_{{Steps.M.Output.O}}')"]
"#;
        let job = write(until);
        let (code, _, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "C",
            "--step-inputs",
            r#"{"Steps.M.Output.O": null}"#,
        ]);
        assert_eq!(code, 1);
        assert_eq!(
            stderr.trim_end(),
            "ERROR: --step-inputs: Loop 'M': Steps.M.Output.O: Output variable 'O': value is null; output values cannot be or contain null"
        );
        // A while loop's LAST output, and one of a loop with an enableIf, is
        // T?.
        for nullable in [
            until.replace("until: \"{{True}}\"", "while: \"{{False}}\""),
            until.replace(
                "- name: M\n  loop:",
                "- name: M\n  enableIf: \"{{True}}\"\n  loop:",
            ),
        ] {
            let job = write(&nullable);
            let (code, stdout, stderr) = run_cli(&[
                "run",
                job.path().to_str().unwrap(),
                "--step",
                "C",
                "--step-inputs",
                r#"{"Steps.M.Output.O": null}"#,
            ]);
            assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
            // A null interpolates as the empty string.
            assert!(has(&stdout, "C_"), "{stdout}");
        }
    }

    #[test]
    fn step_inputs_refuse_keys_for_loops_the_target_cannot_be_given() {
        let job = write(&two_iterations());
        let path = job.path().to_str().unwrap();
        // The target loop's own outputs.
        let (code, _, stderr) = run_cli(&[
            "run",
            path,
            "--step",
            "L",
            "--step-inputs",
            r#"{"Steps.L.Output.H": [1]}"#,
        ]);
        assert_eq!(code, 1);
        assert_eq!(
            stderr.trim_end(),
            "ERROR: --step-inputs: 'Steps.L.Output.H' names Loop 'L' itself; give the outputs of the Steps it reads."
        );
        // A loop the target does not read.
        let two_loops = format!(
            "{}- name: L2\n  loop:\n    until: \"{{{{True}}}}\"\n    maxIterations: 1\n    steps:\n    - name: W2\n      script: {{actions: {{onRun: {{command: python, args: ['-c', 'pass']}}}}}}\n  outputVariables:\n  - {{name: G, type: INT, collect: LAST, value: \"{{{{1}}}}\"}}\n",
            two_iterations()
        );
        let job = write(&two_loops);
        let (code, _, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "C",
            "--step-inputs",
            r#"{"Steps.L2.Output.G": 1}"#,
        ]);
        assert_eq!(code, 1);
        assert_eq!(
            stderr.trim_end(),
            "ERROR: --step-inputs: Step 'C' does not read the outputs of Loop 'L2'."
        );
        // A skipped loop.
        let skipped = two_iterations()
            .replace(
                "- name: L\n  loop:",
                "- name: L\n  enableIf: \"{{False}}\"\n  loop:",
            )
            .replace(
                "range: \"{{Steps.L.Output.H}}\"",
                "range: \"{{Steps.L.Output.H or [5]}}\"",
            )
            .replace(
                "{{sum(Steps.L.Output.H)}}",
                "{{len(Steps.L.Output.H or [])}}",
            );
        let job = write(&skipped);
        let (code, _, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "C",
            "--step-inputs",
            r#"{"Steps.L.Output.H": [1]}"#,
        ]);
        assert_eq!(code, 1);
        assert_eq!(
            stderr.trim_end(),
            "ERROR: --step-inputs: Loop 'L' is skipped by its enableIf, so its Steps.* values are null; remove 'Steps.L.Output.H'."
        );
        // A loop pending inputs at Job creation.
        let pending = format!(
            "{READS_DEPENDENCY}  outputVariables:\n  - {{name: Last, type: INT, collect: LAST, value: \"{{{{Loop.K}}}}\"}}\n- name: C\n  dependencies: [{{dependsOn: L}}]\n  script: {{actions: {{onRun: {{command: python, args: ['-c', \"print({{{{Steps.L.Output.Last}}}})\"]}}}}}}\n"
        );
        let job = write(&pending);
        let (code, _, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "C",
            "--step-inputs",
            r#"{"Steps.L.Output.Last": 1}"#,
        ]);
        assert_eq!(code, 1);
        assert_eq!(
            stderr.trim_end(),
            "ERROR: --step-inputs: Loop 'L' is pending inputs itself, which --step-inputs cannot simulate; use --run-dependencies."
        );
        // Only the loop outputs C reads need values: Extra, which C does
        // not read, is not given.
        let job = write(&format!(
            "{}    - {{name: Extra, type: INT, collect: LAST, value: \"{{{{1}}}}\"}}\n",
            two_iterations().replace(
                "    value: \"{{sum(Steps.P.Output.V)}}\"\n",
                "    value: \"{{sum(Steps.P.Output.V)}}\"\n  - {name: Extra, type: INT, collect: LAST, value: \"{{1}}\"}\n",
            ).trim_end_matches(|_| false)
        ).replace("    - {name: Extra, type: INT, collect: LAST, value: \"{{1}}\"}\n", ""));
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "C",
            "--step-inputs",
            r#"{"Steps.L.Output.H": [1]}"#,
        ]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(!has(&stdout, "Running loop 'L'"), "{stdout}");
    }

    /// RFC 0008: a body Step Environment with wrap hooks, enabled only in
    /// iteration 1, conflicts with a wrap Environment Template there. The
    /// instance fails, which fails the loop, and the run reports its result.
    #[test]
    fn a_wrap_environment_conflict_in_a_body_step_instance_fails_the_loop() {
        let template = r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW, WRAP_ACTIONS]
name: Wrap
steps:
- name: L
  loop:
    until: "{{Loop.Iteration >= 1}}"
    maxIterations: 3
    steps:
    - name: W
      stepEnvironments:
      - name: SW
        enableIf: "{{Loop.Iteration == 1}}"
        script:
          actions:
            onWrapEnvEnter: {command: python, args: ['-c', 'pass']}
            onWrapTaskRun: {command: python, args: ['-c', 'pass']}
            onWrapEnvExit: {command: python, args: ['-c', 'pass']}
      script:
        actions:
          onRun:
            command: python
            args: ['-c', "print('W_{{Loop.Iteration}}')"]
"#;
        let job = write(template);
        let env = templates_dir().join("wrap_env_second.yaml");
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--environment",
            env.to_str().unwrap(),
            "--output",
            "json",
        ]);
        assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
        // WrapB's onWrapTaskRun runs in place of iteration 0's Task.
        assert!(at(&stdout, "WrapB Task") < at(&stdout, "Loop 'L' iteration 1"));
        assert!(!has(&stdout, "Running step 'L[1].W'"));
        assert!(
            stderr.starts_with(
                "ERROR: RFC 0008: a session may have at most one Environment defining wrap hooks"
            ),
            "{stderr}"
        );
        assert!(has(
            &stdout,
            "Loop 'L' failed: its body Step instance 'L[1].W' failed"
        ));
        let result = json(&stdout);
        assert_eq!(result["status"], "error");
        let steps = result["steps"].as_array().unwrap();
        assert_eq!(steps[2]["name"], "L[1].W");
        assert_eq!(steps[2]["state"], "FAILED");
        assert!(
            steps[2]["reason"]
                .as_str()
                .unwrap()
                .starts_with("RFC 0008: "),
            "{}",
            steps[2]
        );
    }

    #[test]
    fn loop_failures_at_binding_at_the_boundary_and_after_zero_iterations() {
        // A loop's own start group fails when its dependency succeeds.
        let template = READS_DEPENDENCY.replace(
            "initial: \"{{Steps.B.Output.N}}\"",
            "initial: \"{{fail('bad start') if Steps.B.Output.N == 2 else 1}}\"",
        ) + "- name: D\n  dependencies: [{dependsOn: L}]\n  script: {actions: {onRun: {command: python, args: ['-c', 'pass']}}}\n";
        let job = write(&template);
        let (code, stdout, stderr) = run_cli(&["run", job.path().to_str().unwrap()]);
        assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
        assert!(
            stderr.starts_with("ERROR: Step 'L' failed in its start group:\n"),
            "{stderr}"
        );
        assert!(
            stderr.contains("\tLoop variable 'K': bad start\n"),
            "{stderr}"
        );
        assert!(
            at(&stdout, "Loop 'L' failed in its start group")
                < at(&stdout, "Step 'D' is blocked: its dependency 'L' failed")
        );
        // next fails at the boundary of iteration 1.
        let template = READS_DEPENDENCY.replace(
            "next: \"{{Loop.K - 1}}\"",
            "next: \"{{fail('no next') if Loop.K == 1 else Loop.K - 1}}\"",
        );
        let job = write(&template);
        let (code, stdout, stderr) = run_cli(&["run", job.path().to_str().unwrap()]);
        assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
        assert!(
            stderr.starts_with("ERROR: Loop 'L' failed at the boundary of iteration 1, in the next of loop variable 'K':\n"),
            "{stderr}"
        );
        assert!(has(
            &stdout,
            "Loop 'L' failed at the boundary of iteration 1, in the next of loop variable 'K'"
        ));
        // A zero-iteration loop whose APPEND output needs an element.
        let template = READS_DEPENDENCY.replace("initial: \"{{Steps.B.Output.N}}\"", "initial: \"{{0}}\"")
            + "  outputVariables:\n  - {name: All, type: 'LIST[INT]', collect: APPEND, value: \"{{Loop.K}}\", minLength: 1}\n";
        let job = write(&template);
        let (code, stdout, stderr) = run_cli(&["run", job.path().to_str().unwrap()]);
        assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
        assert!(
            stderr.starts_with("ERROR: Loop 'L' failed after zero iterations, in the minLength of loop output 'All':\n"),
            "{stderr}"
        );
        assert!(has(
            &stdout,
            "Loop 'L' failed after zero iterations, in the minLength of loop output 'All'"
        ));
    }

    /// A skipped loop behind a failed dependency never satisfies `dependsOn`,
    /// as a skipped Step does not.
    #[test]
    fn a_skipped_loop_behind_a_failure_blocks_its_dependents() {
        let template = format!(
            "{}- name: D\n  dependencies: [{{dependsOn: L}}]\n  script: {{actions: {{onRun: {{command: python, args: ['-c', 'pass']}}}}}}\n",
            READS_DEPENDENCY
                .replace("- name: L\n  dependencies:", "- name: L\n  enableIf: \"{{False}}\"\n  dependencies:")
                .replace("print('openjd_output_var: N=2'); ", "import sys; sys.exit(1); ")
        );
        let job = write(&template);
        let (code, stdout, stderr) = run_cli(&["run", job.path().to_str().unwrap()]);
        assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
        for line in [
            "Loop 'L' skipped (enableIf is false)",
            "Loop 'L' is skipped and never satisfies dependsOn: its dependency 'B' failed",
            "Step 'D' is blocked: its dependency 'L' is skipped but never satisfies dependsOn",
        ] {
            assert!(has(&stdout, line), "no line {line:?} in:\n{stdout}");
        }
    }

    #[test]
    fn summary_lists_loop_totals_and_body_step_environments() {
        let template = two_iterations().replace(
            "    - name: R\n",
            "    - name: R\n      stepEnvironments:\n      - name: REnv\n        script: {actions: {onEnter: {command: python, args: ['-c', 'pass']}}}\n",
        );
        let job = write(&template);
        let (code, stdout, stderr) = run_cli(&["summary", job.path().to_str().unwrap()]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(
            stdout.contains("Total steps: 4\nTotal loops: 1\nTotal tasks: 0 + (determined at run time)\nTotal environments: 1\n"),
            "{stdout}"
        );
        assert!(
            stdout.contains("    - 'R'\n      1 environments\n      1 dependencies\n"),
            "{stdout}"
        );
        assert!(
            stdout.contains("  - REnv (from body Step 'R' of loop 'L')\n"),
            "{stdout}"
        );
        let (code, stdout, _) =
            run_cli(&["summary", job.path().to_str().unwrap(), "--output", "json"]);
        assert_eq!(code, 0);
        let result: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(
            result["step_environments"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0)
                + result["total_environments"].as_u64().unwrap() as usize,
            if result.get("step_environments").is_some() {
                2
            } else {
                1
            }
        );
    }

    /// One `openjd` run of a template in `tests/templates/control_flow_loops/`:
    /// its exit code, the log lines it must print, in order, and lines it
    /// must not print.
    struct LoopCase {
        file: &'static str,
        args: &'static [&'static str],
        code: i32,
        lines: &'static [&'static str],
        absent: &'static [&'static str],
    }

    fn check_loop_case(case: &LoopCase) {
        let path = templates_dir().join("control_flow_loops").join(case.file);
        let mut args = vec!["run", path.to_str().unwrap()];
        args.extend(case.args);
        let (code, stdout, stderr) = run_cli(&args);
        assert_eq!(
            code, case.code,
            "{args:?}\nstdout: {stdout}\nstderr: {stderr}"
        );
        let all = lines(&stdout);
        let mut from = 0;
        for line in case.lines {
            match all[from..].iter().position(|l| l == line) {
                Some(i) => from += i + 1,
                None => panic!(
                    "{}: no line {line:?} after line {from} in:\n{stdout}\nstderr: {stderr}",
                    case.file
                ),
            }
        }
        for line in case.absent {
            assert!(
                !all.contains(line),
                "{}: line {line:?} in:\n{stdout}",
                case.file
            );
        }
    }

    /// The simulator's paths for skipped loops, blocked loops and instances,
    /// instances read only by the boundary or skipped within an iteration, and
    /// the loop binding log lines.
    #[test]
    fn loop_paths() {
        for case in [
            // A loop skipped at Job creation gives its null outputs to a
            // script that reads them.
            LoopCase {
                file: "skipped.yaml", args: &[], code: 0,
                lines: &["Loop 'L' skipped (enableIf is false)", "Loop 'L' is skipped; it runs no iterations", "Running step 'C'", "C got ''"],
                absent: &[],
            },
            // ... and one skipped at run time, to a script and to an enableIf.
            LoopCase {
                file: "dynskip.yaml", args: &[], code: 0,
                lines: &["Output On = false", "Loop 'L' skipped (enableIf is false)", "Loop 'L' is skipped; it runs no iterations", "C got ''"],
                absent: &[],
            },
            LoopCase {
                file: "dynskip2.yaml", args: &[], code: 0,
                lines: &["Loop 'L' skipped (enableIf is false)", "Step 'C': enabled (enableIf is true)", "Step 'C' is ready", "C got ''"],
                absent: &[],
            },
            // The loop binding lines.
            LoopCase {
                file: "dynon.yaml", args: &[], code: 0,
                lines: &[
                    "Loop 'L' is pending inputs:",
                    "  Enabled: determined at run time from Steps.A.Output.On",
                    "  maxIterations: determined at run time (waiting for enableIf)",
                    "  Start (initial values and first while): determined at run time (waiting for enableIf)",
                    "Output On = true", "Loop 'L': enabled (enableIf is true)", "Loop 'L': maxIterations 2",
                    "Loop 'L' is ready", "Running loop 'L'", "C got '5'",
                ],
                absent: &[],
            },
            LoopCase {
                file: "zero.yaml", args: &[], code: 0,
                lines: &["Output N = 0", "Loop 'L': the first while is false, so it has 0 iterations", "Loop 'L' is ready", "Loop 'L' succeeded after 0 iterations (the first while is false)"],
                absent: &["Running loop 'L'"],
            },
            // A loop behind a blocked dependency, and behind an incomplete one
            // that its own group reads.
            LoopCase {
                file: "blk.yaml", args: &["--maximum-tasks", "1"], code: 0,
                lines: &["Step 'B' is blocked: inputs from Step 'A' are incomplete because not all of its Tasks ran", "Loop 'L' is blocked: its dependency 'B' is blocked"],
                absent: &["Running loop 'L'", "P ran"],
            },
            LoopCase {
                file: "pend.yaml", args: &["--maximum-tasks", "1"], code: 0,
                lines: &["Loop 'L' is blocked: inputs from Step 'A' are incomplete because not all of its Tasks ran"],
                absent: &["Running loop 'L'"],
            },
            // An incomplete instance blocks its readers (by script or by
            // enableIf), and their dependents, then the loop.
            LoopCase {
                file: "chain.yaml", args: &["--maximum-tasks", "1"], code: 0,
                lines: &[
                    "Step 'L[0].Q' is blocked: inputs from Step 'L[0].P' are incomplete because not all of its Tasks ran",
                    "Step 'L[0].R' is blocked: its dependency 'L[0].Q' is blocked",
                    "Loop 'L' is blocked: its body Step instance 'L[0].P' did not run all of its Tasks, so iteration 0 never completes",
                ],
                absent: &["R ran"],
            },
            LoopCase {
                file: "chain2.yaml", args: &["--maximum-tasks", "1"], code: 0,
                lines: &[
                    "Step 'L[0].Q' is blocked: inputs from Step 'L[0].P' are incomplete because not all of its Tasks ran",
                    "Step 'L[0].R' is blocked: its dependency 'L[0].Q' is blocked",
                ],
                absent: &["R ran"],
            },
            // An instance whose group reads an incomplete dependency of the
            // loop is blocked, and blocks the loop.
            LoopCase {
                file: "instgrp.yaml", args: &["--maximum-tasks", "1"], code: 0,
                lines: &[
                    "Step 'L[0].P' is blocked: inputs from Step 'A' are incomplete because not all of its Tasks ran",
                    "Loop 'L' is blocked: its body Step instance 'L[0].P' is blocked, so iteration 0 never completes",
                ],
                absent: &["P ran"],
            },
            // A body Step that only the boundary reads.
            LoopCase {
                file: "bout.yaml", args: &[], code: 0,
                lines: &["Loop 'L' iteration 0 outputs: H = 4 frames", "Loop 'L' succeeded after 2 iterations (until is true)", "Loop 'L' outputs: H = 4 frames"],
                absent: &[],
            },
            LoopCase {
                file: "bnext.yaml", args: &[], code: 0,
                lines: &["Loop 'L' iteration 1: X = 4", "Loop 'L' succeeded after 2 iterations (until is true)"],
                absent: &[],
            },
            // An instance skipped as the iteration starts, and at run time,
            // gives its nulls to the instance that reads it.
            LoopCase {
                file: "skipi.yaml", args: &[], code: 0,
                lines: &["Step 'L[0].P' skipped (enableIf is false)", "Step 'L[0].P' is skipped; it runs no Tasks", "Running step 'L[0].Q'", "Q got ''"],
                absent: &[],
            },
            LoopCase {
                file: "skipd.yaml", args: &[], code: 0,
                lines: &["Output B = false", "Step 'L[0].P' skipped (enableIf is false)", "Q got ''"],
                absent: &[],
            },
            // An instance whose Tasks become known makes a reader's range
            // concrete, even when the instance does not run all of its Tasks.
            LoopCase {
                file: "tk.yaml", args: &["--maximum-tasks", "1"], code: 0,
                lines: &["Step 'L[0].P': Tasks known (2 tasks)", "Step 'L[0].Q': Tasks known (2 tasks)", "Step 'L[0].Q' is ready", "Q ran 1"],
                absent: &["Step 'L[0].Q' is blocked: inputs from Step 'L[0].P' are incomplete because not all of its Tasks ran"],
            },
            // A pending instance whose Tasks are known as the iteration
            // starts gives its TaskParam to a reader advanced later.
            LoopCase {
                file: "pending_tasks_known.yaml", args: &["--maximum-tasks", "1"], code: 0,
                lines: &[
                    "Step 'L[0].P' is pending inputs:",
                    "  Tasks: 2 (known; Step pending inputs for Steps.O.Output.N)",
                    "Step 'L[0].Q': Tasks known (2 tasks)", "Step 'L[0].Q' is ready", "Q ran 1",
                ],
                absent: &["Step 'L[0].Q' is blocked: inputs from Step 'L[0].P' are incomplete because not all of its Tasks ran"],
            },
            // An instance reads the TaskParam of a loop dependency that did
            // not run all of its Tasks.
            LoopCase {
                file: "dep_taskparam.yaml", args: &["--maximum-tasks", "1"], code: 0,
                lines: &["Running loop 'L'", "P ran 1"],
                absent: &["Step 'L[0].P' is blocked: inputs from Step 'A' are incomplete because not all of its Tasks ran"],
            },
            // An instance with zero Tasks is named in its log line.
            LoopCase {
                file: "zero_tasks.yaml", args: &[], code: 0,
                lines: &["Running step 'L[0].P'", "Step 'L[0].P' has 0 tasks", "Running step 'L[1].P'", "P ran 7"],
                absent: &[],
            },
            // A pending loop whose bound is known at Job creation reports it
            // then, and not again when it is bound.
            LoopCase {
                file: "known_bound.yaml", args: &[], code: 0,
                lines: &["Loop 'L' is pending inputs:", "  maxIterations: 3", "Loop 'L' is ready", "P ran 1"],
                absent: &["Loop 'L': maxIterations 3"],
            },
            // A reader of a complete and an incomplete instance names the
            // incomplete one.
            LoopCase {
                file: "two_producers.yaml", args: &["--maximum-tasks", "1"], code: 0,
                lines: &[
                    "Step 'L[0].Q' is blocked: inputs from Step 'L[0].P2' are incomplete because not all of its Tasks ran",
                    "Loop 'L' is blocked: its body Step instance 'L[0].P2' did not run all of its Tasks, so iteration 0 never completes",
                ],
                absent: &[],
            },
            // A job output reads a skipped loop's null.
            LoopCase {
                file: "job_output_waits.yaml", args: &[], code: 0,
                lines: &["Loop 'L' skipped (enableIf is false)", "Job output Out = 4"],
                absent: &[],
            },
        ] {
            check_loop_case(&case);
        }
    }

    /// `--step L` checks what the boundary reads (`until`, a loop output's
    /// `value`, a variable's `next`) before the Session starts.
    #[test]
    fn step_naming_a_loop_checks_its_boundary_reads_first() {
        for file in ["bnd.yaml", "bnd_value.yaml", "bnd_next.yaml"] {
            let path = templates_dir().join("control_flow_loops").join(file);
            let (code, stdout, stderr) = run_cli(&["run", path.to_str().unwrap(), "--step", "L"]);
            assert_eq!(code, 1, "{file}");
            assert_eq!(stdout, "", "{file}");
            assert_eq!(
                stderr.trim_end(),
                "ERROR: Loop 'L' reads Steps.* values of Steps this run does not run:\n  \
                 Steps.A.Output.V (INT)\n\
                 Give them with --step-inputs, or run Loop 'L' with --run-dependencies. For example:\n  \
                 --step-inputs '{\"Steps.A.Output.V\": 0}'",
                "{file}"
            );
        }
    }

    /// A body Step instance whose group fails while it advances is FAILED.
    #[test]
    fn an_instance_group_error_while_advancing_fails_the_instance() {
        let path = templates_dir()
            .join("control_flow_loops")
            .join("grpfail.yaml");
        let (code, stdout, stderr) = run_cli(&["run", path.to_str().unwrap(), "--output", "json"]);
        assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
        assert!(
            stderr.starts_with("ERROR: Step 'L[0].P' failed in its Task enumeration group:\n"),
            "{stderr}"
        );
        assert!(has(
            &stdout,
            "Loop 'L' failed: its body Step instance 'L[0].P' failed"
        ));
        let steps = json(&stdout)["steps"].as_array().unwrap().clone();
        let p = steps.iter().find(|s| s["name"] == "L[0].P").unwrap();
        assert_eq!(p["state"], "FAILED");
        assert!(
            p["reason"]
                .as_str()
                .unwrap()
                .starts_with("Step 'L[0].P' failed in its Task enumeration group:"),
            "{p}"
        );
    }

    /// The runtime missing-input message names exactly the dependency the
    /// model's error names (AB, not A), and suggests --step-inputs for a
    /// producer bound at Job creation, Step or loop.
    #[test]
    fn a_missing_input_names_its_dependency() {
        for (file, producer) in [("prefix.yaml", "AB"), ("loopdep.yaml", "K")] {
            let path = templates_dir().join("control_flow_loops").join(file);
            let (code, _, stderr) = run_cli(&["run", path.to_str().unwrap(), "--step", "L"]);
            assert_eq!(code, 1, "{file}");
            assert_eq!(
                stderr.trim_end(),
                format!(
                    "ERROR: Loop 'L' reads, in iteration 1, Steps.* values of '{producer}', which this run does not run.\nGive them with --step-inputs, or run Loop 'L' with --run-dependencies."
                ),
                "{file}"
            );
        }
    }

    #[test]
    fn summary_shows_loop_outputs_units_body_task_parameters_and_waiting_job_outputs() {
        let template = two_iterations()
            .replace("    collect: APPEND\n", "    collect: APPEND\n    unit: px\n")
            .replace(
                "    - name: R\n",
                "    - name: R\n      stepEnvironments:\n      - name: REnv\n        script: {actions: {onEnter: {command: python, args: ['-c', 'pass']}}}\n",
            );
        let job = write(&template);
        let (code, stdout, stderr) = run_cli(&["summary", job.path().to_str().unwrap()]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(
            stdout.contains(
                "1. 'L' (loop; Tasks determined at run time)\n  Condition: until\n  maxIterations: 5\n  Loop variables:\n    - X (INT)\n  Loop outputs:\n    - H (LIST[INT], APPEND, px)\n  Body Steps (Tasks determined at run time in each iteration):\n    - 'P'\n      Task parameters:\n        - I (INT)\n    - 'Q'\n      1 dependencies\n"
            ),
            "{stdout}"
        );
        assert!(
            stdout.contains("  - Sum (INT): pending; waits for Steps.L.Output.H\n"),
            "{stdout}"
        );
        let (code, stdout, _) =
            run_cli(&["summary", job.path().to_str().unwrap(), "--output", "json"]);
        assert_eq!(code, 0);
        let result: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        let l = &result["steps"][0];
        assert_eq!(
            l["outputs"],
            serde_json::json!([{"name": "H", "type": "LIST[INT]", "collect": "APPEND", "unit": "px"}])
        );
        assert_eq!(
            l["body"][2],
            serde_json::json!({"name": "R", "environments": 1, "dependencies": 1})
        );
        // A pending loop with a known bound reports it.
        let path = templates_dir().join("control_flow_loops").join("zero.yaml");
        let (code, stdout, _) = run_cli(&["summary", path.to_str().unwrap(), "--output", "json"]);
        assert_eq!(code, 0);
        let result: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(result["steps"][1]["state"], "PENDING_INPUTS");
        assert_eq!(result["steps"][1]["max_iterations"], 2);
        // --step L names the loop as step_name only.
        let (code, stdout, _) = run_cli(&[
            "summary",
            path.to_str().unwrap(),
            "--step",
            "L",
            "--output",
            "json",
        ]);
        assert_eq!(code, 0);
        let result: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(result["step_name"], "L");
        assert!(result.get("name").is_none(), "{result}");
        assert_eq!(result["kind"], "loop");
        assert_eq!(result["body"], serde_json::json!([{"name": "P"}]));
    }

    #[test]
    fn summary_counts_a_skipped_loop_and_lists_what_job_outputs_wait_on() {
        let path = templates_dir()
            .join("control_flow_loops")
            .join("job_output_waits.yaml");
        let (code, stdout, stderr) = run_cli(&["summary", path.to_str().unwrap()]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(stdout.contains("Total tasks: 1\n"), "{stdout}");
        // A skipped loop's outputs are concrete at Job creation.
        assert!(
            stdout.contains("  - Out (INT): pending; waits for Steps.A.Output.V\n"),
            "{stdout}"
        );
        // The job summary shows a loop's dependency count.
        let job = write(READS_DEPENDENCY);
        let (code, stdout, _) = run_cli(&["summary", job.path().to_str().unwrap()]);
        assert_eq!(code, 0);
        assert!(stdout.contains("    - 'W'\n  1 dependencies\n"), "{stdout}");
        // `--step L` in JSON.
        let (code, stdout, _) = run_cli(&[
            "summary",
            job.path().to_str().unwrap(),
            "--step",
            "L",
            "--output",
            "json",
        ]);
        assert_eq!(code, 0);
        let result: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(result["status"], "success");
        assert_eq!(result["message"], "Summary for loop 'L' in Job 'Dep'");
        assert_eq!(result["job_name"], "Dep");
    }

    /// A blocked loop whose bound was known at Job creation reports it.
    #[test]
    fn a_blocked_loop_reports_its_known_bound() {
        let path = templates_dir()
            .join("control_flow_loops")
            .join("known_bound.yaml");
        let (code, stdout, stderr) = run_cli(&[
            "run",
            path.to_str().unwrap(),
            "-p",
            "Fail=1",
            "--output",
            "json",
        ]);
        assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
        assert_eq!(
            json(&stdout)["steps"][1],
            serde_json::json!({
                "name": "L", "kind": "loop", "state": "BLOCKED", "max_iterations": 3,
                "iterations": 0, "reason": "its dependency 'A' failed",
            })
        );
    }

    /// RFC 0008: a top-level Step's stack is still checked before any
    /// Environment is entered.
    #[test]
    fn a_wrap_environment_conflict_in_a_top_level_step_is_rejected_before_entering() {
        let template = r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW, WRAP_ACTIONS]
name: Wrap
steps:
- name: S
  stepEnvironments:
  - name: SW
    script:
      actions:
        onWrapEnvEnter: {command: python, args: ['-c', 'pass']}
        onWrapTaskRun: {command: python, args: ['-c', 'pass']}
        onWrapEnvExit: {command: python, args: ['-c', 'pass']}
  script:
    actions:
      onRun: {command: python, args: ['-c', "print('S')"]}
"#;
        let job = write(template);
        let env = templates_dir().join("wrap_env_second.yaml");
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--environment",
            env.to_str().unwrap(),
        ]);
        assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
        assert_eq!(stdout, "");
        assert_eq!(
            stderr.trim_end(),
            "ERROR: RFC 0008: a session may have at most one Environment defining wrap hooks (onWrapEnvEnter / onWrapTaskRun / onWrapEnvExit). Found 2: WrapB, SW."
        );
    }

    #[test]
    fn summary_reports_a_loops_bound_and_body() {
        let job = write(&two_iterations());
        let (code, stdout, stderr) =
            run_cli(&["summary", job.path().to_str().unwrap(), "--output", "json"]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        let result: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(result["total_steps"], 4);
        assert_eq!(result["total_loops"], 1);
        assert_eq!(result["tasks_determined_at_run_time"], true);
        assert_eq!(
            result["steps"][0],
            serde_json::json!({
                "name": "L", "kind": "loop", "total_tasks": null, "state": "READY",
                "condition": "until", "max_iterations": 5,
                "variables": [{"name": "X", "type": "INT"}],
                "outputs": [{"name": "H", "type": "LIST[INT]", "collect": "APPEND"}],
                "body": [
                    {"name": "P", "parameter_definitions": [{"name": "I", "type": "INT"}]},
                    {"name": "Q", "dependencies": 1},
                    {"name": "R", "dependencies": 1},
                ],
            })
        );
    }
}

/// CONTROL_FLOW: `--step-inputs` needs only the producer outputs the Step or
/// loop run alone reads, and the missing-inputs error names each value's
/// type and shape with a `--step-inputs` example.
mod control_flow_partial_step_inputs {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::with_suffix(".yaml").unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    /// P declares A, B (ONCE), and L (APPEND); C reads only what `reads`
    /// says.
    fn producer_and_consumer(reads: &str) -> String {
        format!(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Partial
steps:
- name: P
  outputVariables:
  - {{name: A, type: INT}}
  - {{name: B, type: STRING}}
  - {{name: L, type: 'LIST[INT]', collect: APPEND}}
  script: {{actions: {{onRun: {{command: x}}}}}}
- name: C
  dependencies: [{{dependsOn: P}}]
  script: {{actions: {{onRun: {{command: python, args: [-c, "print('GOT {reads}')"]}}}}}}
"#
        )
    }

    #[test]
    fn a_step_producer_needs_only_the_outputs_read() {
        let job = write(&producer_and_consumer("{{Steps.P.Output.A}}"));
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "C",
            "--step-inputs",
            r#"{"Steps.P.Output.A": 7}"#,
        ]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(stdout.contains("GOT 7"), "{stdout}");
    }

    /// A partial set lists only what is still missing; an APPEND output not
    /// given is missing too, not `[]`.
    #[test]
    fn a_partial_set_reports_only_what_is_still_missing() {
        let job = write(&producer_and_consumer(
            "{{Steps.P.Output.A}} {{Steps.P.Output.B}} {{Steps.P.Output.L}}",
        ));
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "C",
            "--step-inputs",
            r#"{"Steps.P.Output.A": 7}"#,
        ]);
        assert_eq!(code, 1);
        assert_eq!(
            stderr.trim_end(),
            "ERROR: Step 'C' reads Steps.* values of Steps this run does not run:\n  \
             Steps.P.Output.B (STRING)\n  \
             Steps.P.Output.L (LIST[INT])\n\
             Give them with --step-inputs, or run Step 'C' with --run-dependencies. For example:\n  \
             --step-inputs '{\"Steps.P.Output.A\": 7, \"Steps.P.Output.B\": \"\", \"Steps.P.Output.L\": []}'"
        );
        // The example holds the values given too, so it runs as it is.
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "C",
            "--step-inputs",
            r#"{"Steps.P.Output.A": 7, "Steps.P.Output.B": "", "Steps.P.Output.L": []}"#,
        ]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(stdout.contains("GOT 7  []"), "{stdout}");
        // Every existing check still applies to what is given.
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "C",
            "--step-inputs",
            r#"{"Steps.P.Output.A": 7, "Steps.P.Output.L": 3}"#,
        ]);
        assert_eq!(code, 1);
        assert_eq!(
            stderr.trim_end(),
            "ERROR: --step-inputs: Step 'P': Steps.P.Output.L: an APPEND output must be given as a list"
        );
    }

    /// The example writes a per-Task list in full up to 16 Tasks, and a
    /// longer one with its first 16 values and `...`, with a note.
    #[test]
    fn a_long_per_task_list_is_shortened_in_the_example() {
        for (tasks, list, note) in [
            ("1-16", format!("[{}]", vec!["0.0"; 16].join(", ")), ""),
            (
                "1-17",
                format!("[{}, ...]", vec!["0.0"; 16].join(", ")),
                "\n(A list ending in ... needs one value per Task; only the first 16 are shown.)",
            ),
        ] {
            let job = write(&format!(
                r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Many
steps:
- name: P
  parameterSpace:
    taskParameterDefinitions: [{{name: I, type: INT, range: '{tasks}'}}]
  outputVariables: [{{name: T, type: FLOAT}}]
  script: {{actions: {{onRun: {{command: x}}}}}}
- name: C
  dependencies: [{{dependsOn: P}}]
  script: {{actions: {{onRun: {{command: python, args: [-c, "print({{{{sum(Steps.P.Output.T)}}}})"]}}}}}}
"#
            ));
            let (code, _stdout, stderr) =
                run_cli(&["run", job.path().to_str().unwrap(), "--step", "C"]);
            assert_eq!(code, 1);
            let count = if tasks == "1-16" { 16 } else { 17 };
            assert_eq!(
                stderr.trim_end(),
                format!(
                    "ERROR: Step 'C' reads Steps.* values of Steps this run does not run:\n  \
                     Steps.P.Output.T (a list with one FLOAT per Task, {count} Tasks)\n\
                     Give them with --step-inputs, or run Step 'C' with --run-dependencies. For example:\n  \
                     --step-inputs '{{\"Steps.P.Output.T\": {list}}}'{note}"
                ),
                "{tasks}"
            );
        }
    }

    /// A while loop's LAST output may be null, which the shape says; and a
    /// value the error lists that --step-inputs cannot give is named with
    /// why, while the example holds only the ones it can.
    #[test]
    fn loop_outputs_and_values_that_cannot_be_given() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Mixed
steps:
- name: S
  outputVariables: [{name: N, type: INT}]
  script: {actions: {onRun: {command: x}}}
- name: Q
  dependencies: [{dependsOn: S}]
  parameterSpace:
    taskParameterDefinitions: [{name: I, type: INT, range: "{{range(Steps.S.Output.N)}}"}]
  outputVariables: [{name: V, type: INT}]
  script: {actions: {onRun: {command: x}}}
- name: L
  loop:
    variables:
    - {name: K, type: INT, initial: "{{2}}", next: "{{Loop.K - 1}}"}
    while: "{{Loop.K > 0}}"
    maxIterations: 5
    steps:
    - name: W
      script: {actions: {onRun: {command: x}}}
  outputVariables:
  - {name: Last, type: INT, collect: LAST, value: "{{Loop.K}}"}
- name: C
  dependencies: [{dependsOn: L}, {dependsOn: Q}]
  script: {actions: {onRun: {command: python, args: [-c, "print({{Steps.L.Output.Last}} {{Steps.Q.Output.V}})"]}}}
"#,
        );
        let (code, _stdout, stderr) =
            run_cli(&["run", job.path().to_str().unwrap(), "--step", "C"]);
        assert_eq!(code, 1);
        assert_eq!(
            stderr.trim_end(),
            "ERROR: Step 'C' reads Steps.* values of Steps this run does not run:\n  \
             Steps.L.Output.Last (INT, or null)\n  \
             Steps.Q.Output.V (INT; Step 'Q' is pending inputs itself, so --step-inputs cannot give it)\n\
             Give them with --step-inputs, or run Step 'C' with --run-dependencies. For example:\n  \
             --step-inputs '{\"Steps.L.Output.Last\": 0}'"
        );
    }

    /// Each type's placeholder, a one-Task list, an `until` loop's LAST
    /// output (not nullable), and one key for a value read as both `Output`
    /// and `RawOutput`.
    #[test]
    fn every_placeholder_and_shape() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Shapes
steps:
- name: P
  outputVariables:
  - {name: B, type: BOOL}
  - {name: R, type: RANGE_EXPR}
  - {name: Pa, type: PATH}
  - {name: LL, type: 'LIST[LIST[INT]]'}
  - {name: S, type: STRING}
  script: {actions: {onRun: {command: x}}}
- name: Q
  parameterSpace:
    taskParameterDefinitions: [{name: I, type: INT, range: '1'}]
  outputVariables: [{name: F, type: BOOL}]
  script: {actions: {onRun: {command: x}}}
- name: L
  loop:
    variables:
    - {name: K, type: INT, initial: "{{0}}", next: "{{Loop.K + 1}}"}
    until: "{{Loop.K >= 1}}"
    maxIterations: 5
    steps:
    - name: W
      script: {actions: {onRun: {command: x}}}
  outputVariables:
  - {name: Last, type: INT, collect: LAST, value: "{{Loop.K}}"}
- name: C
  dependencies: [{dependsOn: P}, {dependsOn: Q}, {dependsOn: L}]
  script:
    actions:
      onRun:
        command: python
        args:
        - -c
        - "print('ok')"
        - "{{Steps.P.Output.B}}"
        - "{{Steps.P.Output.R}}"
        - "{{Steps.P.Output.Pa}}"
        - "{{Steps.P.Output.LL}}"
        - "{{Steps.P.Output.S}}"
        - "{{Steps.P.RawOutput.S}}"
        - "{{Steps.Q.Output.F}}"
        - "{{Steps.L.Output.Last}}"
"#,
        );
        let (code, _stdout, stderr) =
            run_cli(&["run", job.path().to_str().unwrap(), "--step", "C"]);
        assert_eq!(code, 1);
        let path = if cfg!(windows) {
            r#""C:\\path""#
        } else {
            r#""/path""#
        };
        assert_eq!(
            stderr.trim_end(),
            format!(
                "ERROR: Step 'C' reads Steps.* values of Steps this run does not run:\n  \
                 Steps.L.Output.Last (INT)\n  \
                 Steps.P.Output.B (BOOL)\n  \
                 Steps.P.Output.LL (LIST[LIST[INT]])\n  \
                 Steps.P.Output.Pa (PATH)\n  \
                 Steps.P.Output.R (RANGE_EXPR)\n  \
                 Steps.P.Output.S (STRING)\n  \
                 Steps.P.RawOutput.S (STRING)\n  \
                 Steps.Q.Output.F (a list with one BOOL per Task, 1 Task)\n\
                 Give them with --step-inputs, or run Step 'C' with --run-dependencies. For example:\n  \
                 --step-inputs '{{\"Steps.L.Output.Last\": 0, \"Steps.P.Output.B\": false, \
                 \"Steps.P.Output.LL\": [], \"Steps.P.Output.Pa\": {path}, \"Steps.P.Output.R\": \"1\", \
                 \"Steps.P.Output.S\": \"\", \"Steps.Q.Output.F\": [false]}}'"
            )
        );
        // The example, given as it is, is accepted.
        let example = stderr.trim_end().rsplit_once("--step-inputs '").unwrap().1;
        let example = example.strip_suffix('\'').unwrap();
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "C",
            "--step-inputs",
            example,
        ]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(stdout.contains("\tok\n"), "{stdout}");
    }

    /// A later iteration that reads a dependency output --step-inputs did
    /// not give fails the loop, naming the value with an example.
    #[test]
    fn a_later_iteration_reading_an_output_not_given_fails_the_loop() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: Later
steps:
- name: B
  outputVariables:
  - {name: N, type: INT}
  - {name: Text, type: STRING}
  script: {actions: {onRun: {command: x}}}
- name: L
  dependencies: [{dependsOn: B}]
  loop:
    variables:
    - {name: K, type: INT, initial: "{{Steps.B.Output.N}}", next: "{{Loop.K - 1}}"}
    while: "{{Loop.K > 0}}"
    maxIterations: 5
    steps:
    - name: W
      enableIf: "{{Loop.Iteration > 0}}"
      script: {actions: {onRun: {command: python, args: [-c, "print('W {{Steps.B.Output.Text}}')"]}}}
"#,
        );
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "L",
            "--step-inputs",
            r#"{"Steps.B.Output.N": 2}"#,
        ]);
        assert_eq!(code, 1, "stdout: {stdout}");
        assert_eq!(
            stderr.trim_end(),
            "ERROR: Loop 'L' reads, in iteration 1, Steps.* values that --step-inputs does not give (the first ones it found):\n  \
             Steps.B.Output.Text (STRING)\n\
             Give them with --step-inputs too, with any other outputs of these Steps the loop reads. For example:\n  \
             --step-inputs '{\"Steps.B.Output.N\": 2, \"Steps.B.Output.Text\": \"\"}'"
        );
        // The loop's log line and its JSON reason say the same.
        assert!(
            stdout.contains(
                "\tLoop 'L' failed: Loop 'L' reads, in iteration 1, Steps.* values that \
                 --step-inputs does not give (the first ones it found)\n"
            ),
            "{stdout}"
        );
        let (_, stdout, _) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "L",
            "--step-inputs",
            r#"{"Steps.B.Output.N": 2}"#,
            "--output",
            "json",
        ]);
        let json: serde_json::Value =
            serde_json::from_str(&stdout[stdout.find('{').unwrap()..]).unwrap();
        let l = json["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == "L")
            .unwrap();
        assert_eq!(l["state"], "FAILED", "{json}");
        assert_eq!(
            l["reason"].as_str().unwrap(),
            stderr.trim_end().strip_prefix("ERROR: ").unwrap(),
            "{json}"
        );
        // Given Text too, the loop runs both iterations.
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "L",
            "--step-inputs",
            r#"{"Steps.B.Output.N": 2, "Steps.B.Output.Text": "yo"}"#,
        ]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(stdout.contains("W yo"), "{stdout}");
    }

    /// A later iteration that reads an output of a supplied loop dependency
    /// that was not given fails the loop the same way.
    #[test]
    fn a_later_iteration_reading_a_loop_output_not_given_fails_the_loop() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: LaterLoop
steps:
- name: A
  loop:
    variables:
    - {name: K, type: INT, initial: "{{1}}", next: "{{Loop.K - 1}}"}
    until: "{{Loop.K <= 0}}"
    maxIterations: 5
    steps:
    - name: X
      script: {actions: {onRun: {command: x}}}
  outputVariables:
  - {name: N, type: INT, collect: LAST, value: "{{2}}"}
  - {name: T, type: STRING, collect: LAST, value: "{{'t'}}"}
- name: L
  dependencies: [{dependsOn: A}]
  loop:
    variables:
    - {name: K, type: INT, initial: "{{Steps.A.Output.N}}", next: "{{Loop.K - 1}}"}
    while: "{{Loop.K > 0}}"
    maxIterations: 5
    steps:
    - name: V
      enableIf: "{{Loop.Iteration > 0}}"
      script: {actions: {onRun: {command: python, args: [-c, "print('V {{Steps.A.Output.T}}')"]}}}
"#,
        );
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "L",
            "--step-inputs",
            r#"{"Steps.A.Output.N": 2}"#,
        ]);
        assert_eq!(code, 1, "stdout: {stdout}");
        assert_eq!(
            stderr.trim_end(),
            "ERROR: Loop 'L' reads, in iteration 1, Steps.* values that --step-inputs does not give (the first ones it found):\n  \
             Steps.A.Output.T (STRING)\n\
             Give them with --step-inputs too, with any other outputs of these Steps the loop reads. For example:\n  \
             --step-inputs '{\"Steps.A.Output.N\": 2, \"Steps.A.Output.T\": \"\"}'"
        );
    }

    /// An output of a loop pending inputs at Job creation is listed with why
    /// --step-inputs cannot give it, and left out of the example.
    #[test]
    fn a_pending_loops_output_cannot_be_given() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: PendingLoop
steps:
- name: S
  outputVariables: [{name: N, type: INT}]
  script: {actions: {onRun: {command: x}}}
- name: L
  dependencies: [{dependsOn: S}]
  loop:
    variables:
    - {name: K, type: INT, initial: "{{Steps.S.Output.N}}", next: "{{Loop.K - 1}}"}
    while: "{{Loop.K > 0}}"
    maxIterations: 5
    steps:
    - name: W
      script: {actions: {onRun: {command: x}}}
  outputVariables:
  - {name: H, type: 'LIST[INT]', collect: APPEND, value: "{{Loop.K}}"}
- name: C
  dependencies: [{dependsOn: L}]
  script: {actions: {onRun: {command: python, args: [-c, "print({{Steps.L.Output.H}})"]}}}
"#,
        );
        let (code, _stdout, stderr) =
            run_cli(&["run", job.path().to_str().unwrap(), "--step", "C"]);
        assert_eq!(code, 1);
        assert_eq!(
            stderr.trim_end(),
            "ERROR: Step 'C' reads Steps.* values of Steps this run does not run:\n  \
             Steps.L.Output.H (LIST[INT]; Loop 'L' is pending inputs itself, so --step-inputs cannot give it)\n\
             Run Step 'C' with --run-dependencies."
        );
    }

    /// A job output that reads an output --step-inputs did not give says so.
    #[test]
    fn a_job_output_waiting_on_an_output_not_given_says_so() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: JobOut
jobOutputDefinitions:
- {name: GotA, type: INT, value: "{{Steps.P.Output.A}}"}
- {name: GotB, type: STRING, value: "{{Steps.P.Output.B}}"}
steps:
- name: P
  outputVariables:
  - {name: A, type: INT}
  - {name: B, type: STRING}
  script: {actions: {onRun: {command: x}}}
- name: C
  dependencies: [{dependsOn: P}]
  script: {actions: {onRun: {command: python, args: [-c, "print({{Steps.P.Output.A}})"]}}}
"#,
        );
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "C",
            "--step-inputs",
            r#"{"Steps.P.Output.A": 7}"#,
        ]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(stdout.contains("Job output GotA = 7"), "{stdout}");
        assert!(
            stdout.contains(
                "Job output GotB was not evaluated: it waits on Step 'P', which this run does not run, \
                 and --step-inputs does not give the outputs this reads"
            ),
            "{stdout}"
        );
    }

    /// A job output that reads a loop output --step-inputs did not give says
    /// so too.
    #[test]
    fn a_job_output_waiting_on_a_loop_output_not_given_says_so() {
        let job = write(
            r#"specificationVersion: jobtemplate-2023-09
extensions: [EXPR, CONTROL_FLOW]
name: JobOutLoop
jobOutputDefinitions:
- {name: GotT, type: STRING, value: "{{Steps.L.Output.T}}"}
steps:
- name: L
  loop:
    variables:
    - {name: K, type: INT, initial: "{{1}}", next: "{{Loop.K - 1}}"}
    until: "{{Loop.K <= 0}}"
    maxIterations: 5
    steps:
    - name: X
      script: {actions: {onRun: {command: x}}}
  outputVariables:
  - {name: N, type: INT, collect: LAST, value: "{{2}}"}
  - {name: T, type: STRING, collect: LAST, value: "{{'t'}}"}
- name: C
  dependencies: [{dependsOn: L}]
  script: {actions: {onRun: {command: python, args: [-c, "print({{Steps.L.Output.N}})"]}}}
"#,
        );
        let (code, stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "C",
            "--step-inputs",
            r#"{"Steps.L.Output.N": 3}"#,
        ]);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(
            stdout.contains(
                "Job output GotT was not evaluated: it waits on Loop 'L', which this run does not run, \
                 and --step-inputs does not give the outputs this reads"
            ),
            "{stdout}"
        );
    }

    /// A ' in a given value is written for a POSIX shell, so the example
    /// still pastes back.
    #[test]
    fn a_quote_in_a_given_value_is_escaped_in_the_example() {
        let job = write(&producer_and_consumer(
            "{{Steps.P.Output.A}} {{Steps.P.Output.B}}",
        ));
        let (code, _stdout, stderr) = run_cli(&[
            "run",
            job.path().to_str().unwrap(),
            "--step",
            "C",
            "--step-inputs",
            r#"{"Steps.P.Output.B": "it's"}"#,
        ]);
        assert_eq!(code, 1);
        assert_eq!(
            stderr.trim_end(),
            "ERROR: Step 'C' reads Steps.* values of Steps this run does not run:\n  \
             Steps.P.Output.A (INT)\n\
             Give them with --step-inputs, or run Step 'C' with --run-dependencies. For example:\n  \
             --step-inputs '{\"Steps.P.Output.B\": \"it'\\''s\", \"Steps.P.Output.A\": 0}'"
        );
    }

    /// `openjd run --help` says only the outputs read are needed.
    #[test]
    fn help_says_only_the_outputs_read_are_needed() {
        let (code, stdout, _stderr) = run_cli(&["run", "--help"]);
        assert_eq!(code, 0);
        let flat = stdout.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            flat.contains(
                "Only the outputs it reads are needed; when one is missing, the error lists each \
                 with its type and an example"
            ),
            "{stdout}"
        );
    }
}
